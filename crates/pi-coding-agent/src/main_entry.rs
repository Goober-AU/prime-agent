//! Port of packages/coding-agent/src/main.ts
//!
//! The TypeScript entry point is a linear startup sequence that ends in one of
//! seven run modes. Every pure decision helper, the session-manager selection,
//! the runtime-config assembly and the runtime factory are ported directly; the
//! run modes themselves (interactive TUI, print, rpc, acp, daemon, agents view,
//! public command handling) live in other slices, so `main` drives them through
//! the `MainHost` seam below with the same order and the same arguments.
//!
//! blocked_on (other slices, files still empty on disk):
//!   - `cli/public-command.ts` (`handlePublicCommand`)
//!   - `cli/owned-session-worker.ts` (`isOwnedSessionWorkerProcess`,
//!     `installOwnedSessionRecoveryTracking`)
//!   - `modes/print-mode.ts` (`runPrintMode`, `runPrintModeWithConnection`)
//!   - `modes/acp/acp-mode.ts` (`runAcpMode`, `runAcpModeWithConnection`)
//!   - `modes/interactive/components/config-selector.ts` and the interactive TUI
//!     entry points (`InteractiveMode`, `runAgentsViewMode`, `runDaemonMode`,
//!     `runDaemonSupervisorMode`)
//!   - `core/telemetry.ts` (`isTelemetryEnabled`)
//!   - `core/keybindings.ts` (`setKeybindings`)

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pi_ai::types::{ImageContent, Model};
use serde_json::Value;

use crate::cli::args::{parse_args, Args, ListModelsValue, ResumeValue};
use crate::cli::command_registry::format_top_level_help;
use crate::cli::daemon_launch::{
    ensure_interactive_daemon_running, is_daemon_session_summary, list_active_daemon_session_summaries,
    probe_running_daemon_sessions, shutdown_daemon_and_wait, StaleDaemonError,
};
use crate::cli::daemon_stop_confirm::{
    confirm_daemon_session_loss, pluralize_sessions, ConfirmIo, ConfirmOptions, DaemonSessionLossCopy,
};
use crate::cli::file_processor::{process_file_arguments, FileProcessorIo, ProcessFileOptions};
use crate::cli::initial_message::{build_initial_message, InitialMessageInput};
use crate::cli::list_models::{list_models, ListModelsIo};
use crate::cli::session_resolver::{looks_like_session_path, SessionSelectorError};
use crate::config::{expand_tilde_path, get_agent_dir, get_session_dir_env_override, APP_NAME, VERSION};
use crate::core::agent_session_config::{
    merge_agent_session_runtime_config, merge_autonomous_config, AgentAutonomousConfig, AgentSessionRuntimeConfig,
};
use crate::core::agent_session_runtime::{
    create_agent_session_runtime, AgentSessionRuntime, CreateAgentSessionRuntimeFactory,
    CreateAgentSessionRuntimeInput, CreateAgentSessionRuntimeResult, AGENT_SESSION_RUNTIME_KIND_TOP_LEVEL,
};
use crate::core::agent_session_services::{
    create_agent_session_from_services, create_agent_session_services, AgentSessionCreationOptions,
    AgentSessionRuntimeDiagnostic, AgentSessionServices, CreateAgentSessionFromServicesOptions,
    CreateAgentSessionServicesOptions, DIAGNOSTIC_ERROR,
};
use crate::core::auth_guidance::format_no_models_available_message;
use crate::core::auth_storage::AuthStorage;
use crate::core::export_html::{export_from_file, ExportOptions};
use crate::core::model_resolver::{
    find_initial_model, models_are_equal, resolve_cli_model, resolve_model_scope, FindInitialModelOptions,
    ResolveCliModelOptions, ScopedModel,
};
use crate::core::model_registry::ModelRegistry;
use crate::core::output_guard::{restore_stdout, take_over_stdout};
use crate::core::resource_loader::DefaultResourceLoaderOptions;
use crate::core::sdk::{parse_thinking_level, CreateAgentSessionOptions};
use crate::core::session_cwd::{
    format_missing_session_cwd_prompt, get_missing_session_cwd_issue, MissingSessionCwdError, SessionCwdIssue,
};
use crate::core::session_lease::canonical_session_path;
use crate::core::session_manager::{
    find_most_recent_session_for_cwd, get_default_session_dir, load_entries_from_file, SessionManager,
};
use crate::core::settings_manager::{SettingsError, SettingsManager};
use crate::core::timings::{print_timings, reset_timings, time};
use crate::migrations::{run_migrations, show_deprecation_warnings};
use crate::modes::agent_connection::daemon_agent_connection::{
    collect_daemon_client_env, DaemonAgentConnection, DaemonAgentConnectionOptions,
};
use crate::modes::daemon::daemon_client::protocol::{is_unknown_daemon_command_error, DaemonCommand, DaemonResponse};
use crate::modes::daemon::daemon_client::{DaemonClient, DaemonTransportClient};
use crate::modes::daemon::daemon_errors::{
    deserialize_daemon_create_error, deserialize_daemon_error, DaemonError,
};
use crate::modes::daemon::daemon_session_list::SessionSummary;
use crate::modes::daemon::daemon_socket::{default_daemon_socket_path, normalize_socket_path};
use crate::utils::paths::is_local_path;

fn red(message: &str) -> String {
    format!("\u{1b}[31m{message}\u{1b}[39m")
}

fn yellow(message: &str) -> String {
    format!("\u{1b}[33m{message}\u{1b}[39m")
}

fn dim(message: &str) -> String {
    format!("\u{1b}[2m{message}\u{1b}[22m")
}

fn gray(message: &str) -> String {
    format!("\u{1b}[90m{message}\u{1b}[39m")
}

/// Read all content from piped stdin.
/// Returns `None` if stdin is a TTY (interactive terminal).
pub async fn read_piped_stdin() -> Option<String> {
    use std::io::{IsTerminal, Read};
    // If stdin is a TTY, we're running interactively - don't read stdin
    if std::io::stdin().is_terminal() {
        return None;
    }

    let mut data = String::new();
    let _ = std::io::stdin().read_to_string(&mut data);
    let trimmed = data.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// `collectSettingsDiagnostics(settingsManager, context)`.
pub fn collect_settings_diagnostics(
    settings_manager: &mut SettingsManager,
    context: &str,
) -> Vec<AgentSessionRuntimeDiagnostic> {
    settings_manager
        .drain_errors(None)
        .into_iter()
        .map(|SettingsError { scope, error }| AgentSessionRuntimeDiagnostic {
            type_: "warning".to_string(),
            message: format!("({context}, {scope} settings) {}", error.message),
        })
        .collect()
}

/// `reportDiagnostics(diagnostics)`.
pub fn report_diagnostics(diagnostics: &[AgentSessionRuntimeDiagnostic]) {
    for diagnostic in diagnostics {
        let color = |message: &str| match diagnostic.type_.as_str() {
            "error" => red(message),
            "warning" => yellow(message),
            _ => dim(message),
        };
        let prefix = match diagnostic.type_.as_str() {
            "error" => "Error: ",
            "warning" => "Warning: ",
            _ => "",
        };
        eprintln!("{}", color(&format!("{prefix}{}", diagnostic.message)));
    }
}

/// `isTruthyEnvFlag(value)`.
pub fn is_truthy_env_flag(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    if value.is_empty() {
        return false;
    }
    value == "1" || value.to_lowercase() == "true" || value.to_lowercase() == "yes"
}

/// `isTruthyEnvFlag(process.env[PI_OFFLINE])`.
pub fn is_offline_env_flag() -> bool {
    let value = std::env::var("PI_OFFLINE").ok();
    is_truthy_env_flag(value.as_deref())
}

pub type ClientMode = crate::core::agent_session_config::AgentExecutionMode;
/// Compatibility view of the CLI's internal daemon process entrypoint.
pub type AppMode = String;

pub const APP_MODE_DAEMON: &str = "daemon";
pub const APP_MODE_INTERACTIVE: &str = "interactive";
pub const APP_MODE_PRINT: &str = "print";

/// `shouldRejectNonInteractiveAttach(attachAgent, appMode)`.
pub fn should_reject_non_interactive_attach(attach_agent: Option<&str>, app_mode: &str) -> bool {
    attach_agent.is_some() && app_mode != APP_MODE_INTERACTIVE
}

/// `shouldRejectNonInteractiveBareResume(resume, appMode)`.
pub fn should_reject_non_interactive_bare_resume(resume: Option<&ResumeValue>, app_mode: &str) -> bool {
    matches!(resume, Some(ResumeValue::Latest)) && app_mode != APP_MODE_INTERACTIVE
}

/// `resolveAppMode(parsed, stdinIsTTY)`.
pub fn resolve_app_mode(parsed: &Args, stdin_is_tty: bool) -> AppMode {
    if let Some(mode) = &parsed.mode {
        if mode == APP_MODE_DAEMON
            || mode == "rpc"
            || mode == "acp"
            || mode == "json"
        {
            return mode.clone();
        }
    }
    if parsed.print == Some(true) || !stdin_is_tty {
        return APP_MODE_PRINT.to_string();
    }
    APP_MODE_INTERACTIVE.to_string()
}

/// `toPrintOutputMode(appMode)`.
pub fn to_print_output_mode(app_mode: &str) -> &'static str {
    if app_mode == "json" {
        "json"
    } else {
        "text"
    }
}

/// `isClientOwnedDaemonSession(appMode, noSession)`.
pub fn is_client_owned_daemon_session(app_mode: &str, no_session: Option<bool>) -> bool {
    app_mode != "acp" || no_session == Some(true)
}

/// `parseAgentsViewCommand(args)`: `prime-agent agents` opens the agents view directly.
pub struct ParsedAgentsViewCommand {
    pub explicit_agents_view: bool,
    pub args: Vec<String>,
}

impl ParsedAgentsViewCommand {
    pub fn is_empty(&self) -> bool {
        self.args.is_empty()
    }
}

pub fn parse_agents_view_command(args: &[String]) -> ParsedAgentsViewCommand {
    if args.first().map(String::as_str) == Some("agents") {
        return ParsedAgentsViewCommand {
            explicit_agents_view: true,
            args: args[1..].to_vec(),
        };
    }
    ParsedAgentsViewCommand { explicit_agents_view: false, args: args.to_vec() }
}

pub struct DaemonClientStartupDecision {
    pub app_mode: AppMode,
    pub startup_benchmark: bool,
    pub no_session: Option<bool>,
    pub help: Option<bool>,
    pub list_models: Option<ListModelsValue>,
}

pub type InteractiveDaemonStartupDecision = DaemonClientStartupDecision;

/// Retained for callers that only classify persistent interactive startup.
pub fn should_use_daemon_interactive(options: &DaemonClientStartupDecision) -> bool {
    options.app_mode == APP_MODE_INTERACTIVE
        && !options.startup_benchmark
        && options.no_session != Some(true)
        && options.list_models.is_none()
}

/// `shouldUseDaemonClient(options)`.
pub fn should_use_daemon_client(options: &DaemonClientStartupDecision) -> bool {
    options.app_mode != APP_MODE_DAEMON
        && !options.startup_benchmark
        && options.help != Some(true)
        && options.list_models.is_none()
}

pub struct DaemonClientRuntimeDecision {
    pub decision: DaemonClientStartupDecision,
    pub owned_session_worker: bool,
    pub has_process_local_extension_factories: bool,
}

pub fn should_use_daemon_client_runtime(options: &DaemonClientRuntimeDecision) -> bool {
    should_use_daemon_client(&options.decision)
        && !options.owned_session_worker
        && !options.has_process_local_extension_factories
}

pub fn should_ensure_interactive_daemon_for_startup(use_daemon_interactive: bool, attach_agent: Option<&str>) -> bool {
    use_daemon_interactive && attach_agent.is_none()
}

pub struct AgentsViewStartupDecision {
    pub use_daemon_interactive: bool,
    pub needs_onboarding: bool,
    pub explicit_agents_view: Option<bool>,
    pub resume: Option<ResumeValue>,
    pub continue_: Option<bool>,
    pub fork: Option<String>,
}

pub fn should_open_agents_view_for_daemon_interactive(options: &AgentsViewStartupDecision) -> bool {
    let bare_resume = matches!(options.resume, Some(ResumeValue::Latest));
    let requests_agents_view =
        bare_resume || (options.explicit_agents_view == Some(true) && !options.needs_onboarding);
    options.use_daemon_interactive
        // A selector, continuation, or fork must open its target directly rather
        // than the agents view.
        && requests_agents_view
        && !matches!(options.resume, Some(ResumeValue::Selector(_)))
        && options.continue_ != Some(true)
        && options.fork.is_none()
}

pub struct DaemonInteractiveSessionManagerDecision {
    pub resume: Option<ResumeValue>,
    pub continue_: Option<bool>,
    pub fork: Option<String>,
    pub has_active_daemon_session: Option<bool>,
}

pub fn should_use_ephemeral_session_manager_for_daemon_interactive(
    options: &DaemonInteractiveSessionManagerDecision,
) -> bool {
    options.has_active_daemon_session != Some(true)
        && (options.resume.is_none() || matches!(options.resume, Some(ResumeValue::Latest)))
        && options.continue_ != Some(true)
        && options.fork.is_none()
}

pub struct DaemonActiveSessionLookupDecision {
    pub use_daemon_interactive: bool,
    pub resume_selector: Option<String>,
    pub explicit_attach: Option<bool>,
}

pub fn should_ensure_daemon_before_active_session_lookup(options: &DaemonActiveSessionLookupDecision) -> bool {
    options.use_daemon_interactive
        && options.resume_selector.is_some()
        && (options.explicit_attach == Some(true)
            || !looks_like_session_path(options.resume_selector.as_deref().unwrap_or_default()))
}

pub struct ActiveDaemonSessionSummaryLookupOptions {
    pub fallback_on_error: Option<bool>,
}

// ---------------------------------------------------------------------------
// Session manager selection
// ---------------------------------------------------------------------------

async fn find_active_daemon_session_summary_for_interactive_startup(
    socket_path: &str,
    selector: &str,
    options: &ActiveDaemonSessionSummaryLookupOptions,
) -> Result<Option<SessionSummary>, String> {
    match find_active_daemon_session_summary(socket_path, selector).await {
        Ok(summary) => Ok(summary),
        Err(error) => {
            if options.fallback_on_error == Some(false) {
                return Err(error);
            }
            Ok(None)
        }
    }
}

/// `prepareInitialMessage(parsed, autoResizeImages, stdinContent)`.
pub async fn prepare_initial_message(
    parsed: &mut Args,
    auto_resize_images: bool,
    stdin_content: Option<String>,
) -> Result<(Option<String>, Option<Vec<ImageContent>>), String> {
    if parsed.file_args.is_empty() {
        let result = build_initial_message(InitialMessageInput {
            parsed,
            file_text: None,
            file_images: None,
            stdin_content,
        });
        return Ok((result.initial_message, result.initial_images));
    }

    let file_args = parsed.file_args.clone();
    let io = FileProcessorIo {
        error: &|message: &str| eprintln!("{message}"),
        exit: &|code: i32| std::process::exit(code),
    };
    let processed = process_file_arguments(&file_args, Some(ProcessFileOptions { auto_resize_images: Some(auto_resize_images) }), &io).await;
    let result = build_initial_message(InitialMessageInput {
        parsed,
        file_text: Some(processed.text),
        file_images: Some(processed.images),
        stdin_content,
    });
    Ok((result.initial_message, result.initial_images))
}

/// Prompt user for yes/no confirmation.
pub fn prompt_confirm(message: &str) -> bool {
    crate::cli::daemon_stop_confirm::prompt_yes_no(message, &|prompt: &str| {
        use std::io::Write;
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        answer
    })
}

/// Only busy sessions (streaming, compacting, or pending messages) lose work;
/// idle loaded sessions reload from disk on the fresh daemon.
fn startup_session_loss_copy() -> DaemonSessionLossCopy<'static> {
    DaemonSessionLossCopy {
        busy_detail: &|count| {
            let pluralized = pluralize_sessions(count);
            format!(
                "A background service from a different Prime Agent version is running with {count} busy {}. Stopping it will terminate {}.",
                pluralized.noun, pluralized.pronoun
            )
        },
        unlistable_detail:
            "A background service from a different Prime Agent version is running and its sessions could not be listed. Stopping it may terminate active sessions.",
        question: "Stop it and continue?",
        non_tty_hint: "Run \"prime-agent shutdown\" to stop it, then retry.",
    }
}

/// A stale-version daemon couldn't be taken over automatically (busy or stuck).
/// Offer to stop it (default No) and start a fresh daemon, or exit. Returns the
/// fresh ready promise so callers stop re-handling the original rejection.
async fn take_over_stale_daemon_or_exit(socket_path: &str) -> DaemonReadyHandle {
    let probe = probe_running_daemon_sessions(socket_path).await;
    let confirmed = confirm_daemon_session_loss(
        &probe,
        ConfirmOptions { force: false, copy: startup_session_loss_copy() },
        &ConfirmIo {
            stdin_is_tty: stdin_is_tty(),
            error: &|message: &str| eprintln!("{message}"),
            read_line: &|prompt: &str| {
                use std::io::Write;
                print!("{prompt}");
                let _ = std::io::stdout().flush();
                let mut answer = String::new();
                let _ = std::io::stdin().read_line(&mut answer);
                answer
            },
        },
    );
    if !confirmed {
        // Non-TTY already printed the reason; at a TTY the user declined.
        if stdin_is_tty() == Some(true) {
            eprintln!("{}", dim("Cancelled."));
        }
        std::process::exit(1);
    }
    if !shutdown_daemon_and_wait(socket_path, DEFAULT_DAEMON_SHUTDOWN_TIMEOUT_MS).await {
        eprintln!(
            "{}",
            red(&format!(
                "Could not stop the background service on {socket_path}. Run \"prime-agent shutdown\" and retry."
            ))
        );
        std::process::exit(1);
    }
    if let Err(error) = ensure_interactive_daemon_running(socket_path, None).await {
        eprintln!("{}", red(&format!("Could not start the background service: {error}")));
        std::process::exit(1);
    }
    DaemonReadyHandle::ready_immediately(socket_path)
}

/// `shutdownDaemonAndWait(socketPath)`'s default timeout.
pub const DEFAULT_DAEMON_SHUTDOWN_TIMEOUT_MS: f64 = 10_000.0;

fn stdin_is_tty() -> Option<bool> {
    use std::io::IsTerminal;
    Some(std::io::stdin().is_terminal())
}

/// Resolves the daemon-ready promise, returning the promise to keep (the same
/// one on success, or the fresh one from a stale-daemon takeover) so repeat
/// calls don't re-handle the original rejection.
pub async fn await_daemon_ready(daemon_ready: Option<Arc<DaemonReadyHandle>>) -> Option<Arc<DaemonReadyHandle>> {
    let Some(daemon_ready) = daemon_ready else {
        return None;
    };
    match daemon_ready.result().await {
        Ok(()) => Some(daemon_ready),
        Err(error) => {
            // blocked_on: `cli/daemon-launch.ts`'s port reports the stale-daemon
            // failure as its `.message` string, so `instanceof StaleDaemonError`
            // becomes a prefix check on the same message.
            if error.starts_with(STALE_DAEMON_ERROR_PREFIX) {
                return Some(Arc::new(take_over_stale_daemon_or_exit(&daemon_ready.socket_path).await));
            }
            Some(daemon_ready)
        }
    }
}

/// The first line of `StaleDaemonError`'s message.
pub const STALE_DAEMON_ERROR_PREFIX: &str = "An incompatible Prime Agent daemon is running.";

/// The shared `ensureInteractiveDaemonRunning` promise, kept as a handle so
/// callers can await it more than once and keep the same promise on success.
pub struct DaemonReadyHandle {
    socket_path: String,
    shared: Arc<SharedDaemonReady>,
}

struct SharedDaemonReady {
    result: Mutex<Option<Result<(), String>>>,
    notify: tokio::sync::Notify,
}

impl DaemonReadyHandle {
    /// `ensureInteractiveDaemonRunning(socketPath)`, recorded as one shared promise.
    pub fn start(socket_path: &str) -> Arc<Self> {
        let shared = Arc::new(SharedDaemonReady { result: Mutex::new(None), notify: tokio::sync::Notify::new() });
        let socket_path_owned = socket_path.to_string();
        let shared_for_task = Arc::clone(&shared);
        // `daemonReady?.catch(() => {})`: startup only needs to avoid an
        // unhandled rejection; the error is rethrown at the await sites.
        tokio::spawn(async move {
            let result = ensure_interactive_daemon_running(&socket_path_owned, None).await;
            *shared_for_task.result.lock().unwrap() = Some(result);
            shared_for_task.notify.notify_waiters();
        });
        Arc::new(DaemonReadyHandle { socket_path: socket_path.to_string(), shared })
    }

    /// The already-resolved promise returned after a stale-daemon takeover.
    pub fn ready_immediately(socket_path: &str) -> Arc<Self> {
        Arc::new(DaemonReadyHandle {
            socket_path: socket_path.to_string(),
            shared: Arc::new(SharedDaemonReady {
                result: Mutex::new(Some(Ok(()))),
                notify: tokio::sync::Notify::new(),
            }),
        })
    }

    pub async fn result(&self) -> Result<(), String> {
        loop {
            if let Some(result) = self.shared.result.lock().unwrap().clone() {
                return result;
            }
            let notified = self.shared.notify.notified();
            if let Some(result) = self.shared.result.lock().unwrap().clone() {
                return result;
            }
            notified.await;
        }
    }
}

/// `validateForkFlags(parsed)`.
pub fn validate_fork_flags(parsed: &Args) {
    let Some(_fork) = &parsed.fork else {
        return;
    };
    let mut conflicting_flags: Vec<&str> = Vec::new();
    if parsed.continue_ == Some(true) {
        conflicting_flags.push("--continue");
    }
    if parsed.resume.is_some() {
        conflicting_flags.push("--resume");
    }
    if parsed.no_session == Some(true) {
        conflicting_flags.push("--no-session");
    }
    if !conflicting_flags.is_empty() {
        eprintln!(
            "{}",
            red(&format!(
                "Error: --fork cannot be combined with {}",
                conflicting_flags.join(", ")
            ))
        );
        std::process::exit(1);
    }
}

fn fork_session_or_exit(source_path: &str, cwd: &str, session_dir: Option<&str>) -> SessionManager {
    match SessionManager::fork_from(source_path, cwd, session_dir) {
        Ok(manager) => manager,
        Err(message) => {
            eprintln!("{}", red(&format!("Error: {message}")));
            std::process::exit(1);
        }
    }
}

fn get_resume_selector(parsed: &Args) -> Option<String> {
    match &parsed.resume {
        Some(ResumeValue::Selector(selector)) => Some(selector.clone()),
        _ => None,
    }
}

fn read_session_manager(path: &str, session_dir: Option<&str>, cwd_override: Option<&str>) -> SessionManager {
    let entries = load_entries_from_file(path);
    let header = entries
        .iter()
        .find(|entry| entry.get("type").and_then(Value::as_str) == Some("session"));
    let header_cwd = header
        .and_then(|header| header.get("cwd"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let cwd = cwd_override
        .map(str::to_string)
        .or(header_cwd)
        .unwrap_or_else(current_cwd);
    let session_dir = session_dir
        .map(str::to_string)
        .unwrap_or_else(|| parent_dir_string(path));
    let mut manager = SessionManager::in_memory(Some(&cwd), Some(&session_dir)).expect("in-memory session manager");
    manager
        .set_session_file(path, Some(entries), None)
        .expect("session file load");
    manager
}

fn current_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn parent_dir_string(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_else(current_cwd)
}

/// `createSessionManager(parsed, cwd, sessionDir, readOnly = false)`.
pub async fn create_session_manager(
    parsed: &Args,
    cwd: &str,
    session_dir: Option<&str>,
    read_only: bool,
) -> Result<SessionManager, String> {
    let explicit_cwd_override = if parsed.cwd.is_some() { Some(cwd) } else { None };

    if parsed.no_session == Some(true) {
        return SessionManager::in_memory(None, None);
    }

    if let Some(fork) = &parsed.fork {
        let resolved = crate::core::session_resolver::resolve_session_path(fork, cwd, session_dir)
            .await
            .map_err(|error| error.to_string())?;

        match resolved {
            crate::core::session_resolver::ResolvedSession::Path { path }
            | crate::core::session_resolver::ResolvedSession::Local { path }
            | crate::core::session_resolver::ResolvedSession::Global { path, .. } => {
                return Ok(fork_session_or_exit(&path, cwd, session_dir));
            }
        }
    }

    if let Some(resume_selector) = get_resume_selector(parsed) {
        let resolved = crate::core::session_resolver::resolve_session_path(&resume_selector, cwd, session_dir)
            .await
            .map_err(|error| error.to_string())?;

        match resolved {
            crate::core::session_resolver::ResolvedSession::Path { path }
            | crate::core::session_resolver::ResolvedSession::Local { path } => {
                return if read_only {
                    Ok(read_session_manager(&path, session_dir, explicit_cwd_override))
                } else {
                    SessionManager::open(&path, session_dir, explicit_cwd_override)
                };
            }

            crate::core::session_resolver::ResolvedSession::Global { path, cwd: resolved_cwd } => {
                println!("{}", yellow(&format!("Session found in different project: {resolved_cwd}")));
                let should_fork = prompt_confirm("Fork this session into current directory?");
                if !should_fork {
                    println!("{}", dim("Aborted."));
                    std::process::exit(0);
                }
                return Ok(fork_session_or_exit(&path, cwd, session_dir));
            }
        }
    }

    if parsed.continue_ == Some(true) {
        if read_only {
            let dir = session_dir
                .map(str::to_string)
                .unwrap_or_else(|| get_default_session_dir(cwd, None));
            let path = find_most_recent_session_for_cwd(&dir, cwd);
            return match path {
                Some(path) => Ok(read_session_manager(&path, Some(&dir), Some(cwd))),
                None => SessionManager::in_memory(Some(cwd), Some(&dir)),
            };
        }
        return SessionManager::continue_recent(cwd, session_dir);
    }

    if read_only {
        SessionManager::in_memory(Some(cwd), session_dir)
    } else {
        SessionManager::create(cwd, session_dir)
    }
}

// ---------------------------------------------------------------------------
// Runtime services
// ---------------------------------------------------------------------------

/// `buildSessionOptions(config, scopedModels, hasExistingSession, modelRegistry, settingsManager)`.
pub fn build_session_options(
    config: &AgentSessionRuntimeConfig,
    scoped_models: &[ScopedModel],
    has_existing_session: bool,
    model_registry: &crate::core::model_registry::ModelRegistry,
    settings_manager: &SettingsManager,
) -> BuildSessionOptionsResult {
    let mut options = CreateAgentSessionOptions::default();
    let mut diagnostics: Vec<AgentSessionRuntimeDiagnostic> = Vec::new();
    let mut cli_thinking_from_model = false;

    // Model from CLI
    // - supports --provider <name> --model <pattern>
    // - supports --model <provider>/<pattern>
    if let Some(config_model) = &config.model {
        let resolved = resolve_cli_model(
            &ResolveCliModelOptions {
                cli_provider: config.provider.clone(),
                cli_model: Some(config_model.clone()),
            },
            model_registry,
        );
        if let Some(warning) = resolved.warning {
            diagnostics.push(AgentSessionRuntimeDiagnostic { type_: "warning".to_string(), message: warning });
        }
        if let Some(error) = resolved.error {
            diagnostics.push(AgentSessionRuntimeDiagnostic { type_: "error".to_string(), message: error });
        }
        if let Some(model) = resolved.model {
            options.model = Some(model);
            // Allow "--model <pattern>:<thinking>" as a shorthand.
            // Explicit --thinking still takes precedence (applied later).
            if config.thinking.is_none() {
                if let Some(thinking_level) = resolved.thinking_level {
                    options.thinking_level = Some(parse_thinking_level(&thinking_level));
                    cli_thinking_from_model = true;
                }
            }
        }
    }

    if options.model.is_none() && !scoped_models.is_empty() && !has_existing_session {
        // Check if saved default is in scoped models - use it if so, otherwise first scoped model
        let saved_provider = settings_manager.get_default_provider();
        let saved_model_id = settings_manager.get_default_model();
        let saved_model = match (&saved_provider, &saved_model_id) {
            (Some(provider), Some(model_id)) => model_registry.find(provider, model_id),
            _ => None,
        };
        let saved_in_scope = saved_model
            .as_ref()
            .and_then(|saved_model| {
                scoped_models
                    .iter()
                    .find(|scoped| models_are_equal(Some(&scoped.model), Some(saved_model)))
            });

        if let Some(saved_in_scope) = saved_in_scope {
            options.model = Some(saved_in_scope.model.clone());
            // Use thinking level from scoped model config if explicitly set
            if config.thinking.is_none() {
                if let Some(thinking_level) = &saved_in_scope.thinking_level {
                    options.thinking_level = Some(thinking_level.clone());
                }
            }
        } else {
            options.model = Some(scoped_models[0].model.clone());
            // Use thinking level from first scoped model if explicitly set
            if config.thinking.is_none() {
                if let Some(thinking_level) = &scoped_models[0].thinking_level {
                    options.thinking_level = Some(thinking_level.clone());
                }
            }
        }
    }

    // Thinking level from CLI (takes precedence over scoped model thinking levels set above)
    if let Some(thinking) = &config.thinking {
        options.thinking_level = Some(thinking.clone());
    }

    // Scoped models for Ctrl+P cycling
    // Keep thinking level undefined when not explicitly set in the model pattern.
    // Undefined means "inherit current session thinking level" during cycling.
    if !scoped_models.is_empty() {
        options.scoped_models = Some(
            scoped_models
                .iter()
                .map(|scoped| crate::core::agent_session::ScopedModel {
                    model: scoped.model.clone(),
                    thinking_level: scoped
                        .thinking_level
                        .as_ref()
                        .map(|thinking_level| parse_thinking_level(thinking_level)),
                })
                .collect(),
        );
    }

    // API key from CLI - set in authStorage
    // (handled by caller before createAgentSession)

    // Tools
    if config.no_tools == Some(true) {
        options.no_tools = Some("all".to_string());
    } else if config.no_builtin_tools == Some(true) {
        options.no_tools = Some("builtin".to_string());
    }
    if let Some(tools) = &config.tools {
        options.tools = Some(tools.clone());
    }
    if let Some(autonomous) = &config.autonomous {
        options.autonomous = merge_autonomous_config(None, Some(autonomous));
    }

    BuildSessionOptionsResult { options, cli_thinking_from_model, diagnostics }
}

/// `buildSessionOptions`'s return shape.
pub struct BuildSessionOptionsResult {
    pub options: CreateAgentSessionOptions,
    pub cli_thinking_from_model: bool,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

/// `resolveCliPaths(cwd, paths)`.
pub fn resolve_cli_paths(cwd: &str, paths: Option<&Vec<String>>) -> Option<Vec<String>> {
    paths.map(|paths| {
        paths
            .iter()
            .map(|value| {
                if is_local_path(value) {
                    Path::new(cwd).join(value).to_string_lossy().to_string()
                } else {
                    value.clone()
                }
            })
            .collect()
    })
}

/// `runtimeAutonomousConfigFromArgs(parsed)`.
pub fn runtime_autonomous_config_from_args(parsed: &Args) -> Option<AgentAutonomousConfig> {
    let has_autonomous_options = parsed.autonomous == Some(true)
        || parsed.autonomous_gates.is_some()
        || parsed.autonomous_gate_retries.is_some()
        || parsed.autonomous_gate_timeout_ms.is_some()
        || parsed.autonomous_max_continuations.is_some()
        || parsed.autonomous_max_turns.is_some()
        || parsed.autonomous_max_tokens.is_some()
        || parsed.autonomous_timeout_ms.is_some();
    if !has_autonomous_options {
        return None;
    }
    let has_gate_options = parsed.autonomous_gates.is_some()
        || parsed.autonomous_gate_retries.is_some()
        || parsed.autonomous_gate_timeout_ms.is_some();
    Some(AgentAutonomousConfig {
        enabled: Some(true),
        max_continuations: parsed.autonomous_max_continuations.map(|value| value as f64),
        max_turns: parsed.autonomous_max_turns.map(|value| value as f64),
        max_tokens: parsed.autonomous_max_tokens.map(|value| value as f64),
        timeout_ms: parsed.autonomous_timeout_ms.map(|value| value as f64),
        gates: if has_gate_options {
            Some(crate::core::autonomous::AgentAutonomousGateConfig {
                commands: parsed.autonomous_gates.clone(),
                max_retries: parsed.autonomous_gate_retries.map(|value| value as f64),
                timeout_ms: parsed.autonomous_gate_timeout_ms.map(|value| value as f64),
            })
        } else {
            None
        },
        ..Default::default()
    })
}

/// `runtimeConfigFromArgs(parsed, cwd, agentDir, sessionDir, appMode, telemetryDisabled)`.
pub fn runtime_config_from_args(
    parsed: &Args,
    cwd: &str,
    agent_dir: &str,
    session_dir: Option<&str>,
    app_mode: &str,
    telemetry_disabled: Option<bool>,
) -> AgentSessionRuntimeConfig {
    AgentSessionRuntimeConfig {
        cwd: Some(cwd.to_string()),
        agent_dir: Some(agent_dir.to_string()),
        session_dir: session_dir.map(str::to_string),
        provider: parsed.provider.clone(),
        model: parsed.model.clone(),
        api_key: parsed.api_key.clone(),
        system_prompt: parsed.system_prompt.clone(),
        append_system_prompt: parsed.append_system_prompt.clone(),
        thinking: parsed.thinking.clone(),
        models: parsed.models.clone(),
        tools: parsed.tools.clone(),
        no_tools: parsed.no_tools,
        no_builtin_tools: parsed.no_builtin_tools,
        extensions: resolve_cli_paths(cwd, parsed.extensions.as_ref()),
        no_extensions: parsed.no_extensions,
        skills: resolve_cli_paths(cwd, parsed.skills.as_ref()),
        no_skills: parsed.no_skills,
        prompt_templates: resolve_cli_paths(cwd, parsed.prompt_templates.as_ref()),
        no_prompt_templates: parsed.no_prompt_templates,
        themes: resolve_cli_paths(cwd, parsed.themes.as_ref()),
        no_themes: parsed.no_themes,
        no_context_files: parsed.no_context_files,
        autonomous: runtime_autonomous_config_from_args(parsed),
        extension_flag_values: if parsed.unknown_flags.is_empty() {
            None
        } else {
            Some(
                parsed
                    .unknown_flags
                    .iter()
                    .map(|(key, value)| {
                        let value = match value {
                            crate::cli::args::UnknownFlagValue::Flag => Value::Bool(true),
                            crate::cli::args::UnknownFlagValue::Value(value) => Value::String(value.clone()),
                        };
                        (key.clone(), value)
                    })
                    .collect(),
            )
        },
        execution_mode: if app_mode == APP_MODE_DAEMON { None } else { Some(app_mode.to_string()) },
        telemetry_disabled,
        // Serialized refine for print/json/rpc: the client's appMode is NOT
        // "daemon" here - it's "print", "json", or "rpc". The daemon worker
        // receives this flag via AgentSessionRuntimeConfig and uses it
        // instead of its own appMode="daemon".
        serialized_refine: Some(app_mode != APP_MODE_INTERACTIVE && app_mode != APP_MODE_DAEMON),
        initial_goal: parsed.goal.as_ref().map(|objective| crate::core::agent_session_config::InitialGoalConfig {
            objective: objective.clone(),
            token_budget: parsed.goal_token_budget.map(|budget| budget as f64),
        }),
        ..Default::default()
    }
}

/// `PreparedRuntimeServices`.
pub struct PreparedRuntimeServices {
    pub services: Arc<AgentSessionServices>,
    pub scoped_models: Vec<ScopedModel>,
    pub session_options: CreateAgentSessionOptions,
    pub cli_thinking_from_model: bool,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

/// `daemonServerDefaultSessionConfig(config)`.
pub fn daemon_server_default_session_config(config: &AgentSessionRuntimeConfig) -> AgentSessionRuntimeConfig {
    let mut config = config.clone();
    config.initial_goal = None;
    config
}

/// `resolveRuntimeSessionOptions(sessionOptions, runtimeSessionOptions?)`.
pub fn resolve_runtime_session_options(
    session_options: &CreateAgentSessionOptions,
    runtime_session_options: Option<&AgentSessionCreationOptions>,
) -> CreateAgentSessionOptions {
    let base = &session_options.creation;
    let runtime = runtime_session_options;
    let autonomous = match runtime.and_then(|runtime| runtime.rlm_depth.unwrap_or(0)) {
        0 => merge_autonomous_config(base.autonomous.as_ref(), runtime.and_then(|runtime| runtime.autonomous.as_ref())),
        _ => {
            // A subagent runtime never runs its own autonomous loop.
            let disabled = runtime.and_then(|runtime| runtime.autonomous.as_ref()).map(|autonomous| {
                let mut autonomous = autonomous.clone();
                autonomous.enabled = Some(false);
                autonomous
            });
            merge_autonomous_config(base.autonomous.as_ref(), disabled.as_ref())
        }
    };
    CreateAgentSessionOptions {
        model: runtime
            .and_then(|runtime| runtime.model.clone())
            .or_else(|| session_options.model.clone()),
        thinking_level: runtime
            .and_then(|runtime| runtime.thinking_level.clone())
            .or_else(|| session_options.thinking_level.clone()),
        service_tier: runtime
            .and_then(|runtime| runtime.service_tier.clone())
            .or_else(|| session_options.service_tier.clone()),
        scoped_models: runtime
            .and_then(|runtime| runtime.scoped_models.clone())
            .or_else(|| session_options.scoped_models.clone()),
        tools: runtime
            .and_then(|runtime| runtime.tools.clone())
            .or_else(|| session_options.tools.clone()),
        no_tools: runtime
            .and_then(|runtime| runtime.no_tools.clone())
            .or_else(|| session_options.no_tools.clone()),
        custom_tools: runtime
            .and_then(|runtime| runtime.custom_tools.clone())
            .or_else(|| session_options.custom_tools.clone()),
        creation: AgentSessionCreationOptions {
            initial_active_tool_names: runtime.and_then(|runtime| runtime.initial_active_tool_names.clone()),
            allowed_tool_names: runtime.and_then(|runtime| runtime.allowed_tool_names.clone()),
            include_goals: runtime.and_then(|runtime| runtime.include_goals),
            include_compact_skill: runtime.and_then(|runtime| runtime.include_compact_skill),
            rlm_heartbeat_controller: runtime.and_then(|runtime| runtime.rlm_heartbeat_controller.clone()),
            agent_message_controller: runtime.and_then(|runtime| runtime.agent_message_controller.clone()),
            agent_observe_controller: runtime.and_then(|runtime| runtime.agent_observe_controller.clone()),
            autonomous,
            rlm_depth: runtime.and_then(|runtime| runtime.rlm_depth),
            rlm_max_depth: runtime.and_then(|runtime| runtime.rlm_max_depth),
            rlm_session_dir: runtime.and_then(|runtime| runtime.rlm_session_dir.clone()),
            rlm_parent_node_id: runtime.and_then(|runtime| runtime.rlm_parent_node_id.clone()),
            rlm_parent_agent: runtime.and_then(|runtime| runtime.rlm_parent_agent.clone()),
            semantic_parent_session_id: runtime.and_then(|runtime| runtime.semantic_parent_session_id.clone()),
            semantic_spawned_by_request_id: runtime
                .and_then(|runtime| runtime.semantic_spawned_by_request_id.clone()),
            subagent_runtime_host: runtime.and_then(|runtime| runtime.subagent_runtime_host.clone()),
            ..Default::default()
        },
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Runtime services
// ---------------------------------------------------------------------------

/// `createDefaultRuntimeFactory(runtimeDefaultSessionConfig, extensionFactories?)`.
pub fn create_default_runtime_factory(
    runtime_default_session_config: AgentSessionRuntimeConfig,
    extension_factories: Option<Vec<crate::core::extensions::types::ExtensionFactory>>,
) -> CreateAgentSessionRuntimeFactory {
    Arc::new(move |input: CreateAgentSessionRuntimeInput| {
        let runtime_default_session_config = runtime_default_session_config.clone();
        let extension_factories = extension_factories.clone();
        Box::pin(async move {
            let config = merge_agent_session_runtime_config(
                &runtime_default_session_config,
                input.session_config.as_ref(),
            );
            let runtime_session_options = input.session_options.clone();
            let prepared = prepare_runtime_services(PrepareRuntimeServicesOptions {
                config,
                cwd: input.cwd.clone(),
                agent_dir: input.agent_dir.clone(),
                session_manager: Arc::clone(&input.session_manager),
                extension_factories,
                session_options_override: runtime_session_options.clone(),
            })
            .await;
            let PreparedRuntimeServices { services, session_options, diagnostics, .. } = prepared;
            let resolved_session_options =
                resolve_runtime_session_options(&session_options, runtime_session_options.as_ref());

            let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                services: Arc::clone(&services),
                session_manager: Arc::clone(&input.session_manager),
                session_start_event: input.session_start_event.clone(),
                creation: AgentSessionCreationOptions {
                    model: resolved_session_options.model,
                    thinking_level: resolved_session_options.thinking_level,
                    service_tier: resolved_session_options.service_tier,
                    scoped_models: resolved_session_options.scoped_models,
                    tools: resolved_session_options.tools,
                    no_tools: resolved_session_options.no_tools,
                    custom_tools: resolved_session_options.custom_tools,
                    prewarm_ipython_kernel: Some(true),
                    serialized_refine: Some(config.serialized_refine.unwrap_or(false)),
                    execution_mode: config.execution_mode.clone(),
                    telemetry_disabled: config.telemetry_disabled,
                    initial_goal: if resolved_session_options.creation.rlm_depth.unwrap_or(0) == 0 {
                        config.initial_goal.clone()
                    } else {
                        None
                    },
                    ..resolved_session_options.creation
                },
            })
            .await?;

            let cli_thinking_override = config.thinking.is_some() || prepared.cli_thinking_from_model;
            if created.session.model().is_some() && cli_thinking_override {
                created.session.set_thinking_level(created.session.thinking_level());
            }

            Ok(CreateAgentSessionRuntimeResult { result: created, services, diagnostics })
        })
    })
}

/// Options of `prepareRuntimeServices`.
pub struct PrepareRuntimeServicesOptions {
    pub config: AgentSessionRuntimeConfig,
    pub cwd: String,
    pub agent_dir: String,
    pub session_manager: Arc<Mutex<SessionManager>>,
    pub extension_factories: Option<Vec<crate::core::extensions::types::ExtensionFactory>>,
    pub session_options_override: Option<CreateAgentSessionOptions>,
}

/// `prepareRuntimeServices(options)`.
pub async fn prepare_runtime_services(options: PrepareRuntimeServicesOptions) -> PreparedRuntimeServices {
    let config = options.config;
    let effective_agent_dir = config.agent_dir.clone().unwrap_or_else(|| options.agent_dir.clone());
    let auth_storage = AuthStorage::create(
        Some(format!("{}/auth.json", effective_agent_dir.trim_end_matches('/'))),
        Some(crate::core::auth_storage::AuthStorageOptions {
            prime_cli_config_path: None,
            use_prime_cli_config: effective_agent_dir == options.agent_dir,
        }),
    );
    let extension_flag_values = config.extension_flag_values.clone().map(indexmap::IndexMap::from_iter);
    let mut resource_loader_options = DefaultResourceLoaderOptions::new(&options.cwd, &effective_agent_dir);
    resource_loader_options.additional_extension_paths = config.extensions.clone().unwrap_or_default();
    resource_loader_options.additional_skill_paths = config.skills.clone().unwrap_or_default();
    resource_loader_options.additional_prompt_template_paths = config.prompt_templates.clone().unwrap_or_default();
    resource_loader_options.additional_theme_paths = config.themes.clone().unwrap_or_default();
    resource_loader_options.no_extensions = config.no_extensions.unwrap_or(false);
    resource_loader_options.no_skills = config.no_skills.unwrap_or(false);
    resource_loader_options.no_prompt_templates = config.no_prompt_templates.unwrap_or(false);
    resource_loader_options.no_themes = config.no_themes.unwrap_or(false);
    resource_loader_options.no_context_files = config.no_context_files.unwrap_or(false);
    resource_loader_options.system_prompt = config.system_prompt.clone();
    resource_loader_options.append_system_prompt = config.append_system_prompt.clone();
    resource_loader_options.extension_factories = options.extension_factories.clone().unwrap_or_default();
    let services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: options.cwd.clone(),
        agent_dir: Some(effective_agent_dir),
        auth_storage: Some(Arc::new(tokio::sync::Mutex::new(auth_storage))),
        settings_manager: None,
        model_registry: None,
        extension_flag_values,
        // Subagents share the parent's Herdr pane; their own reporter would race
        // the parent's and a subagent quit would release the still-active pane.
        no_builtin_herdr_reporter: Some(
            options
                .session_options_override
                .as_ref()
                .and_then(|options| options.creation.rlm_depth)
                .unwrap_or(0)
                > 0,
        ),
        telemetry_disabled: config.telemetry_disabled,
        resource_loader_options: Some(resource_loader_options),
    })
    .await
    .unwrap_or_else(|error| panic!("createAgentSessionServices failed: {error}"));

    let mut diagnostics: Vec<AgentSessionRuntimeDiagnostic> = services.diagnostics.clone();
    diagnostics.extend(collect_settings_diagnostics(&services.settings_manager, "runtime creation"));
    diagnostics.extend(
        services
            .resource_loader
            .get_extensions()
            .errors
            .iter()
            .map(|error| AgentSessionRuntimeDiagnostic {
                type_: DIAGNOSTIC_ERROR.to_string(),
                message: format!("Failed to load extension \"{}\": {}", error.path, error.error),
            }),
    );

    let model_patterns = config
        .models
        .clone()
        .or_else(|| services.settings_manager.lock().unwrap().get_enabled_models());
    let scoped_models = match model_patterns {
        Some(model_patterns) if !model_patterns.is_empty() => {
            resolve_model_scope(&model_patterns, &mut services.model_registry.lock().unwrap()).await
        }
        _ => Vec::new(),
    };
    let has_existing_session = !options.session_manager.lock().unwrap().build_session_context().messages.is_empty();
    let built = build_session_options(
        &config,
        &scoped_models,
        has_existing_session,
        &services.model_registry.lock().unwrap(),
        &services.settings_manager.lock().unwrap(),
    );
    diagnostics.extend(built.diagnostics.clone());

    let effective_session_model = options
        .session_options_override
        .as_ref()
        .and_then(|options| options.model.clone())
        .or_else(|| built.options.model.clone());
    if let Some(api_key) = &config.api_key {
        match effective_session_model {
            Some(model) => {
                services
                    .auth_storage
                    .lock()
                    .await
                    .set_runtime_api_key(&model.provider, api_key);
            }
            None => diagnostics.push(AgentSessionRuntimeDiagnostic {
                type_: DIAGNOSTIC_ERROR.to_string(),
                message: "--api-key requires a model to be specified via --model, --provider/--model, or --models"
                    .to_string(),
            }),
        }
    }

    PreparedRuntimeServices {
        services: Arc::new(services),
        scoped_models,
        session_options: built.options,
        cli_thinking_from_model: built.cli_thinking_from_model,
        diagnostics,
    }
}

/// `resolvePreparedStartupModel(options)`.
pub async fn resolve_prepared_startup_model(
    prepared: &PreparedRuntimeServices,
    session_manager: &Arc<Mutex<SessionManager>>,
) -> InitialModelSelection {
    let model_registry = Arc::clone(&prepared.services.model_registry);
    let settings_manager = Arc::clone(&prepared.services.settings_manager);
    let existing_session = session_manager.lock().unwrap().build_session_context();
    let has_existing_session = !existing_session.messages.is_empty();

    let mut model = prepared.session_options.model.clone();
    let mut model_fallback_message: Option<String> = None;

    if model.is_none() && has_existing_session {
        if let Some(existing_model) = &existing_session.model {
            let restored = model_registry
                .lock()
                .unwrap()
                .find(&existing_model.provider, &existing_model.id);
            if let Some(restored) = restored {
                if model_registry.lock().unwrap().has_configured_auth(&restored) {
                    model = Some(restored);
                }
            }
            if model.is_none() {
                model_fallback_message = Some(format!(
                    "Could not restore model {}/{}",
                    existing_model.provider, existing_model.id
                ));
            }
        }
    }

    if model.is_none() {
        let settings = settings_manager.lock().unwrap();
        let result = find_initial_model(
            &FindInitialModelOptions {
                cli_provider: None,
                cli_model: None,
                scoped_models: prepared.scoped_models.clone(),
                is_continuing: has_existing_session,
                default_provider: settings.get_default_provider(),
                default_model_id: settings.get_default_model(),
                default_thinking_level: settings.get_default_thinking_level(),
            },
            &mut model_registry.lock().unwrap(),
        )
        .await
        .unwrap_or_default();
        drop(settings);
        model = result.model;
        if model.is_none() {
            model_fallback_message = Some(format_no_models_available_message());
        } else if let Some(existing) = model_fallback_message {
            let selected = model.as_ref().expect("checked above");
            model_fallback_message = Some(format!("{existing}. Using {}/{}", selected.provider, selected.id));
        }
    }

    InitialModelSelection { model, model_fallback_message }
}

/// Return shape of `resolvePreparedStartupModel`.
pub struct InitialModelSelection {
    pub model: Option<Model>,
    pub model_fallback_message: Option<String>,
}
