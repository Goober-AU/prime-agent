//! Port of packages/coding-agent/src/package-manager-cli.ts
//!
//! `handleConfigCommand`, `handlePackageCommand` and the daemon update-restart
//! coordinator they can spawn.

use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::cli::config_selector::{select_config, ConfigSelectorOptions};
use crate::cli::daemon_launch::{
    ensure_interactive_daemon_running, is_daemon_session_summary, is_session_busy, probe_running_daemon_sessions,
    shutdown_connected_daemon_and_wait, RunningDaemonProbe,
};
use crate::cli::daemon_stop_confirm::{
    confirm_daemon_session_loss, pluralize_sessions, ConfirmIo, ConfirmOptions, DaemonSessionLossCopy,
};
use crate::cli::daemon_update_restart::{
    acquire_daemon_update_restart_coordinator, build_daemon_update_restart_report,
    launch_daemon_update_restart_coordinator, wait_for_active_daemon_update_restart_coordinator,
    DaemonUpdateRestartCounts, DaemonUpdateRestartFailure, DaemonUpdateRestartProcessIdentity,
    DaemonUpdateRestartStatus, DaemonUpdateRestartStatusWriter, DaemonUpdateRestartUpdate,
    DAEMON_UPDATE_RESTART_COORDINATOR_FLAG, DAEMON_UPDATE_RESTART_ORIGIN_FLAG, DAEMON_UPDATE_RESTART_STATUS_FLAG,
};
use crate::config::{
    get_agent_dir, get_daemon_update_restart_manifest_path, get_legacy_daemon_update_restart_manifest_path,
    get_self_update_command, get_self_update_unavailable_instruction, APP_NAME, CONFIG_DIR_NAME, PACKAGE_NAME,
    SELF_UPDATE_INTERACTIVE_CHILD_ENV, SELF_UPDATE_NOT_ATTEMPTED_EXIT_CODE, VERSION,
};
use crate::core::messages::CustomMessage;
use crate::core::package_manager::{DefaultPackageManager, PackageManagerOptions, ProgressEvent};
use crate::core::settings_manager::{SettingsError, SettingsManager};
use crate::modes::daemon::daemon_client::{DaemonClient, DaemonCommandBody, DaemonClientRequestOptions};
use crate::modes::daemon::daemon_protocol::{
    is_unknown_daemon_command_error, DaemonResponse, DaemonUpdateRestartManifest,
    DaemonUpdateRestartSession, DAEMON_PROTOCOL_VERSION, DAEMON_SCHEMA_ID,
    DAEMON_UPDATE_RESTART_FORMAT_VERSION,
};
use crate::cli::daemon_update_restart::{
    DaemonUpdateRestartCoordinatorLease, DaemonUpdateRestartCoordinatorRecord, DaemonUpdateRestartPhase,
    AcquireDaemonUpdateRestartCoordinatorOptions, DEFAULT_COORDINATOR_PROGRESS_TIMEOUT_MS,
};
use crate::cli::daemon_stop_confirm::Pluralized;
use crate::core::agent_session::{
    SessionActionRecoveryPayload, SessionActionRecoverySnapshot, SESSION_ACTION_RECOVERY_FORMAT_VERSION,
};
use crate::modes::daemon::daemon_protocol::AgentSessionRuntimeMetadata;
use crate::modes::daemon::daemon_supervisor_ownership::{DaemonShutdownAdmission, DaemonSupervisorHelloIdentity};
use crate::modes::daemon::daemon_socket::{default_daemon_socket_dir, default_daemon_socket_path};
use crate::utils::daemon_socket_path::normalize_socket_path;
use crate::modes::daemon::daemon_supervisor_ownership::{
    persist_daemon_startup_fence_from_owner, wait_for_daemon_startup_fence,
};
use crate::modes::daemon::daemon_worker_protocol::{
    DAEMON_WORKER_ACTIVE_SESSION_ID_ENV, DAEMON_WORKER_SUPERVISOR_SOCKET_ENV,
};
use crate::utils::child_process::should_use_windows_shell;
use crate::utils::update_source::PRIME_AGENT_UPDATE_REPOSITORY_URL;
use crate::utils::version_check::{get_latest_pi_release, is_main_build_update_available};

pub type PackageCommand = &'static str;

pub const PACKAGE_COMMANDS: [PackageCommand; 4] = ["install", "remove", "update", "list"];

const UPDATE_RESTART_PREDECESSOR_FENCE_TIMEOUT_MS: u64 = 60_000;

/// `UpdateTarget`.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateTarget {
    All,
    Self_,
    Extensions { source: Option<String> },
}

/// `isSelfUpdateSource(source)`.
pub fn is_self_update_source(source: &str) -> bool {
    source == "self" || source == "pi" || source == APP_NAME
}

/// `PackageCommandOptions`.
#[derive(Debug, Clone, Default)]
pub struct PackageCommandOptions {
    pub command: String,
    pub source: Option<String>,
    pub update_target: Option<UpdateTarget>,
    pub local: bool,
    pub force: bool,
    pub help: bool,
    pub daemon_socket_path: Option<String>,
    pub restart_coordinator: bool,
    pub restart_status_path: Option<String>,
    pub restart_origin_active_session_id: Option<String>,
    pub invalid_option: Option<String>,
    pub invalid_argument: Option<String>,
    pub missing_option_value: Option<String>,
    pub conflicting_options: Option<String>,
}

fn yellow(message: &str) -> String {
    format!("\u{1b}[33m{message}\u{1b}[39m")
}

fn red(message: &str) -> String {
    format!("\u{1b}[31m{message}\u{1b}[39m")
}

fn green(message: &str) -> String {
    format!("\u{1b}[32m{message}\u{1b}[39m")
}

fn dim(message: &str) -> String {
    format!("\u{1b}[2m{message}\u{1b}[22m")
}

fn bold(message: &str) -> String {
    format!("\u{1b}[1m{message}\u{1b}[22m")
}

fn gray(message: &str) -> String {
    format!("\u{1b}[90m{message}\u{1b}[39m")
}

/// `reportSettingsErrors(settingsManager, context)`.
fn report_settings_errors(settings_manager: &mut SettingsManager, context: &str) {
    let errors: Vec<SettingsError> = settings_manager.drain_errors(None);
    for settings_error in errors {
        eprintln!(
            "{}",
            yellow(&format!(
                "Warning ({}, {} settings): {}",
                context, settings_error.scope, settings_error.error.message
            ))
        );
        // REPAIR CURSOR: TS prints `error.stack`; SettingsErrorValue has only `message` (core/settings_manager.rs:370).
    }
}

fn get_package_command_usage(command: &str) -> String {
    match command {
        "install" => format!("{APP_NAME} package install <source> [--local]"),
        "remove" => format!("{APP_NAME} package remove <source> [--local]"),
        "update" => format!("{APP_NAME} update [--force] or {APP_NAME} package update [source]"),
        _ => format!("{APP_NAME} package list"),
    }
}

fn print_package_command_help(command: &str) {
    match command {
        "install" => println!(
            "{}
  {}

Install a package and add it to settings.

Options:
  --local    Install project-locally ({CONFIG_DIR_NAME}/settings.json)

Examples:
  {APP_NAME} package install npm:@foo/bar
  {APP_NAME} package install git:github.com/user/repo
  {APP_NAME} package install git:git@github.com:user/repo
  {APP_NAME} package install https://github.com/user/repo
  {APP_NAME} package install ssh://git@github.com/user/repo
  {APP_NAME} package install ./local/path
",
            bold("Usage:"),
            get_package_command_usage("install")
        ),
        "remove" => println!(
            "{}
  {}

Remove a package and its source from settings.

Options:
  --local    Remove from project settings ({CONFIG_DIR_NAME}/settings.json)

Examples:
  {APP_NAME} package remove npm:@foo/bar
",
            bold("Usage:"),
            get_package_command_usage("remove")
        ),
        "update" => println!(
            "{}
  {}

Update {APP_NAME} or installed packages.

Options:
  --self                  Update {APP_NAME} only
  --extensions            Update installed packages only
  --extension <source>    Update one package only
  --force                 Reinstall {APP_NAME} even if the current version is latest
  --daemon-socket <path>  Restart the daemon listening on this exact socket

Commands:
  {APP_NAME} update                Update {APP_NAME}
  {APP_NAME} package update        Update installed packages
  {APP_NAME} package update <source> Update one package
",
            bold("Usage:"),
            get_package_command_usage("update")
        ),
        _ => println!(
            "{}
  {}

List installed packages from user and project settings.
",
            bold("Usage:"),
            get_package_command_usage("list")
        ),
    }
}

/// `parsePackageCommand(args)`.
pub fn parse_package_command(args: &[String]) -> Option<PackageCommandOptions> {
    let raw_command = args.first().map(String::as_str)?;
    let rest = &args[1..];
    let command: &'static str = if raw_command == "uninstall" {
        "remove"
    } else {
        match raw_command {
            "install" => "install",
            "remove" => "remove",
            "update" => "update",
            "list" => "list",
            _ => return None,
        }
    };

    let mut local = false;
    let mut force = false;
    let mut help = false;
    let mut invalid_option: Option<String> = None;
    let mut invalid_argument: Option<String> = None;
    let mut missing_option_value: Option<String> = None;
    let mut conflicting_options: Option<String> = None;
    let mut source: Option<String> = None;
    let mut self_flag = false;
    let mut extensions_flag = false;
    let mut extension_flag_source: Option<String> = None;
    let mut daemon_socket_path: Option<String> = None;
    let mut restart_coordinator = false;
    let mut restart_status_path: Option<String> = None;
    let mut restart_origin_active_session_id: Option<String> = None;

    let mut index = 0usize;
    while index < rest.len() {
        let arg = rest[index].clone();
        if arg == "-h" || arg == "--help" {
            help = true;
            index += 1;
            continue;
        }

        if arg == "--local" {
            if command == "install" || command == "remove" {
                local = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg);
            }
            index += 1;
            continue;
        }

        if arg == "--self" {
            if command == "update" {
                self_flag = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg);
            }
            index += 1;
            continue;
        }

        if arg == "--extensions" {
            if command == "update" {
                extensions_flag = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg);
            }
            index += 1;
            continue;
        }

        if arg == "--force" {
            if command == "update" {
                force = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg);
            }
            index += 1;
            continue;
        }

        if arg == "--daemon-socket" {
            if command != "update" {
                if invalid_option.is_none() {
                    invalid_option = Some(arg);
                }
                index += 1;
                continue;
            }
            let value = rest.get(index + 1);
            match value {
                None => {
                    if missing_option_value.is_none() {
                        missing_option_value = Some(arg);
                    }
                    index += 1;
                }
                Some(value) if value.starts_with('-') => {
                    if missing_option_value.is_none() {
                        missing_option_value = Some(arg);
                    }
                    index += 1;
                }
                Some(value) if daemon_socket_path.is_some() => {
                    if conflicting_options.is_none() {
                        conflicting_options = Some("--daemon-socket can only be provided once".to_string());
                    }
                    index += 2;
                }
                Some(value) => {
                    daemon_socket_path = Some(normalize_socket_path(value, None));
                    index += 2;
                }
            }
            continue;
        }

        if arg == DAEMON_UPDATE_RESTART_COORDINATOR_FLAG {
            if command == "update" {
                restart_coordinator = true;
            } else if invalid_option.is_none() {
                invalid_option = Some(arg);
            }
            index += 1;
            continue;
        }

        if arg == DAEMON_UPDATE_RESTART_STATUS_FLAG || arg == DAEMON_UPDATE_RESTART_ORIGIN_FLAG {
            if command != "update" {
                if invalid_option.is_none() {
                    invalid_option = Some(arg);
                }
                index += 1;
                continue;
            }
            let value = rest.get(index + 1);
            match value {
                None => {
                    if missing_option_value.is_none() {
                        missing_option_value = Some(arg);
                    }
                    index += 1;
                }
                Some(value) if value.starts_with('-') => {
                    if missing_option_value.is_none() {
                        missing_option_value = Some(arg);
                    }
                    index += 1;
                }
                Some(value) => {
                    if arg == DAEMON_UPDATE_RESTART_STATUS_FLAG {
                        restart_status_path = Some(value.clone());
                    } else {
                        restart_origin_active_session_id = Some(value.clone());
                    }
                    index += 2;
                }
            }
            continue;
        }

        if arg == "--extension" {
            if command != "update" {
                if invalid_option.is_none() {
                    invalid_option = Some(arg);
                }
                index += 1;
                continue;
            }

            let value = rest.get(index + 1);
            match value {
                None => {
                    if missing_option_value.is_none() {
                        missing_option_value = Some(arg);
                    }
                    index += 1;
                }
                Some(value) if value.starts_with('-') => {
                    if missing_option_value.is_none() {
                        missing_option_value = Some(arg);
                    }
                    index += 1;
                }
                Some(value) if extension_flag_source.is_some() => {
                    if conflicting_options.is_none() {
                        conflicting_options = Some("--extension can only be provided once".to_string());
                    }
                    index += 2;
                }
                Some(value) => {
                    extension_flag_source = Some(value.clone());
                    index += 2;
                }
            }
            continue;
        }

        if arg.starts_with('-') {
            if invalid_option.is_none() {
                invalid_option = Some(arg);
            }
            index += 1;
            continue;
        }

        if source.is_none() {
            source = Some(arg);
        } else if invalid_argument.is_none() {
            invalid_argument = Some(arg);
        }
        index += 1;
    }

    let mut update_target: Option<UpdateTarget> = None;
    if command == "update" {
        if let Some(extension_flag_source) = extension_flag_source.clone() {
            if self_flag || extensions_flag {
                if conflicting_options.is_none() {
                    conflicting_options =
                        Some("--extension cannot be combined with --self or --extensions".to_string());
                }
            }
            if source.is_some() {
                if conflicting_options.is_none() {
                    conflicting_options =
                        Some("--extension cannot be combined with a positional source".to_string());
                }
            }
            update_target = Some(UpdateTarget::Extensions {
                source: Some(extension_flag_source),
            });
        } else if let Some(source) = source.clone() {
            if is_self_update_source(&source) {
                update_target = Some(if extensions_flag { UpdateTarget::All } else { UpdateTarget::Self_ });
            } else {
                if extensions_flag || self_flag {
                    if conflicting_options.is_none() {
                        conflicting_options = Some(
                            "positional update targets cannot be combined with --self or --extensions".to_string(),
                        );
                    }
                }
                update_target = Some(UpdateTarget::Extensions { source: Some(source) });
            }
        } else if self_flag && extensions_flag {
            update_target = Some(UpdateTarget::All);
        } else if self_flag {
            update_target = Some(UpdateTarget::Self_);
        } else if extensions_flag {
            update_target = Some(UpdateTarget::Extensions { source: None });
        } else {
            update_target = Some(UpdateTarget::All);
        }
    }

    Some(PackageCommandOptions {
        command: command.to_string(),
        source,
        update_target,
        local,
        force,
        help,
        daemon_socket_path,
        restart_coordinator,
        restart_status_path,
        restart_origin_active_session_id,
        invalid_option,
        invalid_argument,
        missing_option_value,
        conflicting_options,
    })
}

fn update_target_includes_self(target: &UpdateTarget) -> bool {
    matches!(target, UpdateTarget::All | UpdateTarget::Self_)
}

fn update_target_includes_extensions(target: &UpdateTarget) -> bool {
    matches!(target, UpdateTarget::All | UpdateTarget::Extensions { .. })
}

/// `resolveUpdateDaemonSocketPath(explicitSocketPath?)`.
pub fn resolve_update_daemon_socket_path(explicit_socket_path: Option<&str>) -> String {
    let socket_path = explicit_socket_path
        .map(str::to_string)
        .or_else(|| std::env::var(DAEMON_WORKER_SUPERVISOR_SOCKET_ENV).ok())
        .unwrap_or_else(default_daemon_socket_path);
    normalize_socket_path(&socket_path, None)
}

fn report_daemon_update_restart_status(status: &DaemonUpdateRestartStatus) {
    let report = build_daemon_update_restart_report(status);
    for message in report.info {
        println!("{}", green(&message));
    }
    for warning in report.warnings {
        eprintln!("{}", yellow(&format!("Warning: {warning}")));
    }
}

fn print_self_update_unavailable(npm_command: Option<&[String]>, update_spec: &str, update_package_name: &str) {
    eprintln!("error: {APP_NAME} cannot self-update this installation.");
    eprintln!(
        "{}",
        get_self_update_unavailable_instruction(
            PACKAGE_NAME,
            npm_command,
            update_spec,
            update_package_name
        )
    );

    if let Some(entrypoint) = std::env::args().nth(1) {
        eprintln!();
        eprintln!("Location of pi executable: {entrypoint}");
    }
}

fn print_self_update_fallback(command: &crate::config::SelfUpdateCommand) {
    eprintln!(
        "{}",
        dim(&format!(
            "If this keeps failing, run this command yourself: {}",
            command.display
        ))
    );
}

/// `SelfUpdatePlan`.
#[derive(Debug, Clone, PartialEq)]
pub struct SelfUpdatePlan {
    pub install_spec: String,
    pub package_name: String,
    pub should_run: bool,
    pub target_version: Option<String>,
}

fn set_self_update_no_change_exit_code() {
    if std::env::var(SELF_UPDATE_INTERACTIVE_CHILD_ENV).as_deref() == Ok("1") {
        std::env::set_var("PI_EXIT_CODE", SELF_UPDATE_NOT_ATTEMPTED_EXIT_CODE.to_string());
    }
}

/// `getSelfUpdatePlan(force)`.
pub async fn get_self_update_plan(force: bool) -> Result<SelfUpdatePlan, String> {
    let latest_release = get_latest_pi_release(VERSION, None).await.ok_or_else(|| {
        format!(
            "No installable main build is available from {PRIME_AGENT_UPDATE_REPOSITORY_URL}. Check the connection and the main release build, then retry."
        )
    })?;
    let should_run = force || is_main_build_update_available(&latest_release.version, VERSION);
    if !should_run {
        println!("{}", green(&format!("{APP_NAME} is already up to date (v{VERSION})")));
    }
    Ok(SelfUpdatePlan {
        install_spec: latest_release.install_spec,
        package_name: latest_release.package_name,
        should_run,
        target_version: Some(latest_release.version),
    })
}

async fn run_self_update(command: &crate::config::SelfUpdateCommand) -> Result<(), String> {
    println!("{}", dim(&format!("Updating {APP_NAME} with {}...", command.display)));
    let steps: Vec<crate::config::SelfUpdateCommandStep> = match &command.steps {
        Some(steps) => steps.clone(),
        None => vec![crate::config::SelfUpdateCommandStep {
            command: command.command.clone(),
            args: command.args.clone(),
            display: command.display.clone(),
        }],
    };
    for step in steps {
        // Windows package managers are commonly .cmd shims. Use the shell so the
        // child can execute them.
        let mut builder = if should_use_windows_shell(&step.command) && cfg!(windows) {
            let mut builder = tokio::process::Command::new("cmd");
            builder.arg("/c").arg(&step.command);
            builder
        } else {
            tokio::process::Command::new(&step.command)
        };
        builder.args(&step.args);
        // `stdio: "inherit"` - the package manager writes to this terminal.
        builder.stdin(std::process::Stdio::inherit());
        builder.stdout(std::process::Stdio::inherit());
        builder.stderr(std::process::Stdio::inherit());
        let mut child = builder.spawn().map_err(|error| error.to_string())?;
        let status = child.wait().await.map_err(|error| error.to_string())?;
        match (status.code(), exit_signal_name(&status)) {
            (Some(0), _) => {}
            (Some(code), _) => return Err(format!("{} exited with code {code}", step.display)),
            (None, Some(signal)) => return Err(format!("{} terminated by signal {signal}", step.display)),
            (None, None) => return Err(format!("{} exited with code unknown", step.display)),
        }
    }
    Ok(())
}

/// `close(code, signal)`'s signal arm.
fn exit_signal_name(status: &std::process::ExitStatus) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        return status.signal().map(|signal| signal.to_string());
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

const UPDATE_RESTART_CONTINUATION_PROMPT: &str =
    "Prime Agent restarted after an update. Continue the interrupted task from the saved transcript and restored tool/kernel state. Inspect current state before retrying commands when needed.";

fn update_session_loss_copy() -> DaemonSessionLossCopy<'static> {
    DaemonSessionLossCopy {
        busy_detail: &|count| {
            let Pluralized { noun, pronoun } = pluralize_sessions(count);
            format!(
                "Prime Agent has {count} busy {noun}. After the update installs, it will stop {pronoun}, restart its background service, and resume interrupted work."
            )
        },
        unlistable_detail:
            "Running agents could not be listed. After the update installs, Prime Agent will stop resident agents, restart its background service, and resume interrupted work where possible.",
        question: "Continue?",
        non_tty_hint: "Re-run with --force to proceed.",
    }
}

/// Returns false when the update should be aborted to avoid terminating live sessions.
fn confirm_daemon_session_loss_before_update(
    probe: &RunningDaemonProbe,
    force: bool,
    io: &ConfirmIo<'_>,
) -> bool {
    confirm_daemon_session_loss(
        probe,
        ConfirmOptions { force, copy: update_session_loss_copy() },
        io,
    )
}

fn daemon_probe_may_have_busy_sessions(probe: &RunningDaemonProbe) -> bool {
    probe.reachable
        && (probe.active_sessions.is_none()
            || probe.busy_client_owned_session_count.unwrap_or(0) > 0
            || probe
                .active_sessions
                .as_ref()
                .is_some_and(|sessions| sessions.iter().any(is_session_busy)))
}

fn is_record(value: &Value) -> bool {
    value.is_object()
}

fn read_string(value: Option<&Value>, field_name: &str) -> Result<String, String> {
    match value.and_then(Value::as_str) {
        Some(value) => Ok(value.to_string()),
        None => Err(format!("Daemon update restart response is missing {field_name}")),
    }
}

fn read_optional_string(value: Option<&Value>, field_name: &str) -> Result<Option<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_str() {
            Some(value) => Ok(Some(value.to_string())),
            None => Err(format!("Daemon update restart response is missing {field_name}")),
        },
    }
}

fn read_boolean(value: Option<&Value>, field_name: &str) -> Result<bool, String> {
    match value.and_then(Value::as_bool) {
        Some(value) => Ok(value),
        None => Err(format!("Daemon update restart response is missing {field_name}")),
    }
}

fn read_optional_string_record(
    value: Option<&Value>,
    field_name: &str,
) -> Result<Option<indexmap::IndexMap<String, String>>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(object) = value.as_object() else {
        return Err(format!("Daemon update restart response is missing {field_name}"));
    };
    let mut record = indexmap::IndexMap::new();
    for (key, entry) in object {
        match entry.as_str() {
            Some(entry) => {
                record.insert(key.clone(), entry.to_string());
            }
            None => return Err(format!("Daemon update restart response is missing {field_name}")),
        }
    }
    Ok(Some(record))
}

fn read_custom_messages(value: Option<&Value>, field_name: &str) -> Result<Vec<CustomMessage>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let error = || format!("Daemon update restart response is missing {field_name}");
    let entries = value.as_array().ok_or_else(error)?;
    let mut messages = Vec::with_capacity(entries.len());
    for entry in entries {
        messages.push(serde_json::from_value::<CustomMessage>(entry.clone()).map_err(|_| error())?);
    }
    Ok(messages)
}

/// `parseSessionActionRecoverySnapshot(value)`: the deep field checks of the
/// TypeScript are the serde shape of `SessionActionRecoverySnapshot`.
fn parse_session_action_recovery_snapshot(value: Option<&Value>) -> Result<SessionActionRecoverySnapshot, String> {
    let Some(value) = value else {
        return Err("Daemon update restart response contains invalid session actions".to_string());
    };
    if !is_record(value) {
        return Err("Daemon update restart response contains invalid session actions".to_string());
    }
    let format_version = value.get("formatVersion").and_then(Value::as_i64);
    if format_version != Some(SESSION_ACTION_RECOVERY_FORMAT_VERSION) {
        return Err(format!(
            "Unsupported session action recovery format version: {}",
            value.get("formatVersion").map(display_value).unwrap_or_else(|| "undefined".to_string())
        ));
    }
    serde_json::from_value::<SessionActionRecoverySnapshot>(value.clone())
        .map_err(|_| "Daemon update restart response is missing session actions".to_string())
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn parse_daemon_update_restart_runtime_metadata(
    value: Option<&Value>,
) -> Result<Option<AgentSessionRuntimeMetadata>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !is_record(value) {
        return Err("Daemon update restart response contains invalid runtime metadata".to_string());
    }
    let kind = read_string(value.get("kind"), "runtimeMetadata.kind")?;
    if kind != "top-level" && kind != "subagent" {
        return Err("Daemon update restart response contains invalid runtime metadata kind".to_string());
    }
    Ok(Some(AgentSessionRuntimeMetadata {
        kind,
        created_at: value
            .get("createdAt")
            .and_then(Value::as_f64)
            .ok_or_else(|| "Daemon update restart response is missing runtimeMetadata.createdAt".to_string())?,
        parent_active_session_id: read_optional_string(
            value.get("parentActiveSessionId"),
            "runtimeMetadata.parentActiveSessionId",
        )?,
        parent_session_id: read_optional_string(value.get("parentSessionId"), "runtimeMetadata.parentSessionId")?,
        parent_session_file: read_optional_string(
            value.get("parentSessionFile"),
            "runtimeMetadata.parentSessionFile",
        )?,
        rlm_child_id: read_optional_string(value.get("rlmChildId"), "runtimeMetadata.rlmChildId")?,
        rlm_parent_node_id: read_optional_string(value.get("rlmParentNodeId"), "runtimeMetadata.rlmParentNodeId")?,
        rehydrated_completed: value.get("rehydratedCompleted").and_then(Value::as_bool),
        prompt: read_optional_string(value.get("prompt"), "runtimeMetadata.prompt")?,
        spawn_code: read_optional_string(value.get("spawnCode"), "runtimeMetadata.spawnCode")?,
        session_dir: read_optional_string(value.get("sessionDir"), "runtimeMetadata.sessionDir")?,
    }))
}

fn parse_daemon_update_restart_session(value: &Value) -> Result<DaemonUpdateRestartSession, String> {
    if !is_record(value) {
        return Err("Daemon update restart response contains an invalid session".to_string());
    }
    let Some(queue) = value.get("queue") else {
        return Err("Daemon update restart response contains an invalid queue".to_string());
    };
    if !is_record(queue) {
        return Err("Daemon update restart response contains an invalid queue".to_string());
    }
    let Some(config) = value.get("config") else {
        return Err("Daemon update restart response contains an invalid session config".to_string());
    };
    if !is_record(config) {
        return Err("Daemon update restart response contains an invalid session config".to_string());
    }
    let client_env = read_optional_string_record(value.get("clientEnv"), "clientEnv")?;
    let runtime_metadata = parse_daemon_update_restart_runtime_metadata(value.get("runtimeMetadata"))?;
    Ok(DaemonUpdateRestartSession {
        active_session_id: read_string(value.get("activeSessionId"), "activeSessionId")?,
        session_id: read_string(value.get("sessionId"), "sessionId")?,
        session_file: read_string(value.get("sessionFile"), "sessionFile")?,
        cwd: read_string(value.get("cwd"), "cwd")?,
        config: serde_json::from_value(config.clone())
            .map_err(|_| "Daemon update restart response contains an invalid session config".to_string())?,
        runtime_metadata,
        client_env,
        queue: crate::modes::daemon::daemon_protocol::DaemonUpdateRestartQueue {
            actions: parse_session_action_recovery_snapshot(queue.get("actions"))?,
            next_turn: read_custom_messages(queue.get("nextTurn"), "queue.nextTurn")?,
        },
        should_resume: read_boolean(value.get("shouldResume"), "shouldResume")?,
        was_streaming: read_boolean(value.get("wasStreaming"), "wasStreaming")?,
        was_compacting: read_boolean(value.get("wasCompacting"), "wasCompacting")?,
        was_bash_running: read_boolean(value.get("wasBashRunning"), "wasBashRunning")?,
        had_running_rlm_children: read_boolean(value.get("hadRunningRlmChildren"), "hadRunningRlmChildren")?,
        was_retrying: read_boolean(value.get("wasRetrying"), "wasRetrying")?,
        had_accepted_prompt_in_flight: read_boolean(
            value.get("hadAcceptedPromptInFlight"),
            "hadAcceptedPromptInFlight",
        )?,
    })
}

fn parse_daemon_update_restart_manifest(value: &Value) -> Result<DaemonUpdateRestartManifest, String> {
    if !is_record(value) {
        return Err("Daemon update restart response is invalid".to_string());
    }
    let format_version = value.get("formatVersion").and_then(Value::as_f64);
    if format_version != Some(DAEMON_UPDATE_RESTART_FORMAT_VERSION) {
        return Err(format!(
            "Unsupported daemon update restart format version: {}",
            value.get("formatVersion").map(display_value).unwrap_or_else(|| "undefined".to_string())
        ));
    }
    let Some(sessions) = value.get("sessions") else {
        return Err("Daemon update restart response is missing sessions".to_string());
    };
    let Some(sessions) = sessions.as_array() else {
        return Err("Daemon update restart response is missing sessions".to_string());
    };
    let mut parsed = Vec::with_capacity(sessions.len());
    for session in sessions {
        parsed.push(parse_daemon_update_restart_session(session)?);
    }
    Ok(DaemonUpdateRestartManifest {
        format_version: DAEMON_UPDATE_RESTART_FORMAT_VERSION,
        created_at: read_string(value.get("createdAt"), "createdAt")?,
        sessions: parsed,
        discarded_active_session_ids: value
            .get("discardedActiveSessionIds")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(Value::as_str).map(str::to_string).collect()),
    })
}

fn manifest_paths(socket_path: &str, agent_dir: &str) -> Vec<String> {
    vec![
        get_daemon_update_restart_manifest_path(socket_path, Some(agent_dir)),
        get_legacy_daemon_update_restart_manifest_path(Some(agent_dir)),
    ]
}

fn clear_prepared_daemon_update_restart_manifest(socket_path: &str, agent_dir: &str) {
    for manifest_path in manifest_paths(socket_path, agent_dir) {
        // Best effort only; the mtime guard below prevents stale fallback use.
        let _ = std::fs::remove_file(manifest_path);
    }
}

fn read_prepared_daemon_update_restart_manifest(
    socket_path: &str,
    agent_dir: &str,
    not_before_ms: Option<f64>,
) -> Result<Option<DaemonUpdateRestartManifest>, String> {
    for manifest_path in manifest_paths(socket_path, agent_dir) {
        let modified_at_ms = match std::fs::metadata(&manifest_path).and_then(|metadata| metadata.modified()) {
            Ok(modified) => match modified.duration_since(std::time::UNIX_EPOCH) {
                Ok(duration) => duration.as_secs_f64() * 1000.0,
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        if let Some(not_before_ms) = not_before_ms {
            if modified_at_ms < not_before_ms - 1000.0 {
                continue;
            }
        }
        let contents = std::fs::read_to_string(&manifest_path).map_err(|error| error.to_string())?;
        let parsed: Value = serde_json::from_str(&contents).map_err(|error| error.to_string())?;
        return parse_daemon_update_restart_manifest(&parsed).map(Some);
    }
    Ok(None)
}

fn try_read_prepared_daemon_update_restart_manifest(
    socket_path: &str,
    agent_dir: &str,
) -> Option<DaemonUpdateRestartManifest> {
    match read_prepared_daemon_update_restart_manifest(socket_path, agent_dir, None) {
        Ok(manifest) => manifest,
        Err(_) => {
            clear_prepared_daemon_update_restart_manifest(socket_path, agent_dir);
            None
        }
    }
}

fn has_restorable_daemon_update_restart(manifest: Option<&DaemonUpdateRestartManifest>) -> bool {
    manifest.is_some_and(|manifest| !manifest.sessions.is_empty())
}

fn response_has_active_daemon_sessions(data: Option<&Value>) -> bool {
    match data.and_then(Value::as_object).and_then(|object| object.get("sessions")) {
        Some(Value::Array(sessions)) => !sessions.is_empty(),
        _ => true,
    }
}

fn daemon_hello_owner_identity(hello: Option<&Value>) -> DaemonSupervisorHelloIdentity {
    match hello {
        Some(hello) => DaemonSupervisorHelloIdentity::from_value(hello),
        None => DaemonSupervisorHelloIdentity::from_value(&Value::Null),
    }
}

fn has_fixed_daemon_supervisor_owner_identity(identity: &DaemonSupervisorHelloIdentity) -> bool {
    identity.supervisor_generation.is_some()
        && identity.supervisor_owner_token.is_some()
        && identity.supervisor_pid.is_some_and(|pid| pid > 0)
        && identity.supervisor_process_start_id.is_some()
        && identity.supervisor_socket_path.is_some()
}

async fn prepare_connected_daemon_update_restart(
    client: &Arc<DaemonClient>,
    socket_path: &str,
    agent_dir: &str,
    hello: Option<&Value>,
) -> Result<DaemonUpdateRestartManifest, String> {
    let pending_manifest = try_read_prepared_daemon_update_restart_manifest(socket_path, agent_dir);
    let mut started_at: Option<f64> = None;
    let mut fixed_owner_identity: Option<DaemonSupervisorHelloIdentity> = None;
    let mut fence_persistence_started = false;

    if let Some(hello) = hello {
        let identity = DaemonSupervisorHelloIdentity::from_value(hello);
        if has_fixed_daemon_supervisor_owner_identity(&identity) {
            fixed_owner_identity = Some(identity);
        }
    }

    let persist_prepared_restart_fence = |fixed_owner_identity: &mut Option<DaemonSupervisorHelloIdentity>,
                                          fence_persistence_started: &mut bool| {
        let current_hello = client.hello().map(|hello| hello.raw);
        if let Some(current_hello) = &current_hello {
            let identity = DaemonSupervisorHelloIdentity::from_value(current_hello);
            if has_fixed_daemon_supervisor_owner_identity(&identity) {
                *fixed_owner_identity = Some(identity);
            }
        }
        let Some(identity) = fixed_owner_identity.clone() else {
            return None;
        };
        *fence_persistence_started = true;
        Some(identity)
    };

    let preparation = async {
        if let Some(manifest) = &pending_manifest {
            if !manifest.sessions.is_empty() {
                let list_response = client
                    .request(
                        Map::from_iter([("type".to_string(), Value::String("list".to_string()))]),
                        Some(30_000),
                        DaemonClientRequestOptions::default(),
                    )
                    .await
                    .map_err(|error| error.message())?;
                if list_response.success && !response_has_active_daemon_sessions(list_response.data.as_ref()) {
                    return Ok::<Option<DaemonUpdateRestartManifest>, String>(Some(manifest.clone()));
                }
            }
        }
        clear_prepared_daemon_update_restart_manifest(socket_path, agent_dir);
        started_at = Some(now_ms());
        let response = client
            .request(
                Map::from_iter([(
                    "type".to_string(),
                    Value::String("prepare_update_restart".to_string()),
                )]),
                Some(120_000),
                DaemonClientRequestOptions::default(),
            )
            .await
            .map_err(|error| error.message())?;
        if !response.success {
            return Err(response.error.unwrap_or_else(|| "Daemon request failed".to_string()));
        }
        let manifest = parse_daemon_update_restart_manifest(
            response.data.as_ref().unwrap_or(&Value::Null),
        )?;
        Ok(Some(manifest))
    }
    .await;

    match preparation {
        Ok(Some(manifest)) => {
            if let Some(identity) = persist_prepared_restart_fence(&mut fixed_owner_identity, &mut fence_persistence_started)
            {
                persist_daemon_startup_fence_from_owner(socket_path, &identity, None, None)
                    .await
                    .map_err(|error| error)?;
            }
            Ok(manifest)
        }
        Ok(None) => Err("Daemon update restart preparation failed".to_string()),
        Err(error) => {
            if fence_persistence_started {
                return Err(error);
            }
            if let Some(started_at) = started_at {
                if let Ok(Some(fallback)) =
                    read_prepared_daemon_update_restart_manifest(socket_path, agent_dir, Some(started_at))
                {
                    if let Some(identity) =
                        persist_prepared_restart_fence(&mut fixed_owner_identity, &mut fence_persistence_started)
                    {
                        persist_daemon_startup_fence_from_owner(socket_path, &identity, None, None)
                            .await
                            .map_err(|error| error)?;
                    }
                    return Ok(fallback);
                }
            }
            Err(error)
        }
    }
}

pub async fn prepare_daemon_update_restart(
    socket_path: &str,
    agent_dir: &str,
) -> Result<DaemonUpdateRestartManifest, String> {
    let pending_manifest = try_read_prepared_daemon_update_restart_manifest(socket_path, agent_dir);
    let client = DaemonClient::create(socket_path);
    let mut connected = false;
    let hello = {
        match client.connect(1000).await {
            Ok(()) => {
                connected = true;
                client.wait_for_hello(2000).await.ok().map(|hello| hello.raw)
            }
            Err(error) => {
                if !connected && pending_manifest.as_ref().is_some_and(|manifest| !manifest.sessions.is_empty()) {
                    return pending_manifest.ok_or_else(|| error.message());
                }
                return Err(error.message());
            }
        }
    };
    let result = prepare_connected_daemon_update_restart(&client, socket_path, agent_dir, hello.as_ref()).await;
    client.close().await;
    result
}

fn read_created_active_session_id(value: Option<&Value>) -> Result<String, String> {
    let Some(value) = value else {
        return Err("Daemon returned an invalid session create response".to_string());
    };
    if !is_daemon_session_summary(value) {
        return Err("Daemon returned an invalid session create response".to_string());
    }
    match value.get("activeSessionId").and_then(Value::as_str) {
        Some(active_session_id) => Ok(active_session_id.to_string()),
        None => Err("Daemon returned an invalid session create response".to_string()),
    }
}

async fn restore_next_turn_messages(
    client: &Arc<DaemonClient>,
    active_session_id: &str,
    session_file: &str,
    messages: &[CustomMessage],
) -> bool {
    if messages.is_empty() {
        return true;
    }
    let mut command: DaemonCommandBody = Map::from_iter([(
        "type".to_string(),
        Value::String("restore_next_turn".to_string()),
    )]);
    command.insert(
        "activeSessionId".to_string(),
        Value::String(active_session_id.to_string()),
    );
    command.insert(
        "messages".to_string(),
        Value::Array(
            messages
                .iter()
                .filter_map(|message| serde_json::to_value(message).ok())
                .collect(),
        ),
    );
    let response = client
        .request(command, Some(30_000), DaemonClientRequestOptions::default())
        .await;
    match response {
        Ok(response) if response.success => true,
        Ok(response) => {
            eprintln!(
                "{}",
                yellow(&format!(
                    "Warning: could not restore pending context for {session_file}: {}",
                    response.error.unwrap_or_else(|| "Daemon request failed".to_string())
                ))
            );
            false
        }
        Err(error) => {
            eprintln!(
                "{}",
                yellow(&format!(
                    "Warning: could not restore pending context for {session_file}: {}",
                    error.message()
                ))
            );
            false
        }
    }
}

struct RestoreDaemonUpdateRestartSessionResult {
    restored: bool,
    resumed: bool,
    failure_message: Option<String>,
}

struct RestoreDaemonUpdateRestartResult {
    total: i64,
    restored: i64,
    resumed: i64,
    failed: i64,
    failures: Vec<DaemonUpdateRestartFailure>,
}

fn remap_daemon_update_restart_runtime_metadata(
    session: &DaemonUpdateRestartSession,
    restored_active_session_ids: &indexmap::IndexMap<String, String>,
) -> Option<AgentSessionRuntimeMetadata> {
    let metadata = session.runtime_metadata.as_ref()?;
    if metadata.kind != "subagent" {
        return Some(metadata.clone());
    }
    let mut runtime_metadata = metadata.clone();
    let parent_active_session_id = metadata
        .parent_active_session_id
        .as_ref()
        .and_then(|old_parent| restored_active_session_ids.get(old_parent))
        .cloned();
    runtime_metadata.parent_active_session_id = parent_active_session_id;
    Some(runtime_metadata)
}

async fn request_command(
    client: &Arc<DaemonClient>,
    command: DaemonCommandBody,
    timeout_ms: u64,
) -> Result<DaemonResponse, String> {
    client
        .request(command, Some(timeout_ms), DaemonClientRequestOptions::default())
        .await
        .map_err(|error| error.message())
}

async fn restore_daemon_update_restart_session(
    client: &Arc<DaemonClient>,
    session: &DaemonUpdateRestartSession,
    restored_active_session_ids: &mut indexmap::IndexMap<String, String>,
    restart_origin_active_session_id: Option<&str>,
) -> Result<RestoreDaemonUpdateRestartSessionResult, String> {
    let runtime_metadata = remap_daemon_update_restart_runtime_metadata(session, restored_active_session_ids);
    // DaemonCommandBody is the JSON object body the daemon wire protocol takes.
    let mut create: DaemonCommandBody = Map::from_iter([(
        "type".to_string(),
        Value::String("create".to_string()),
    )]);
    create.insert(
        "sessionPath".to_string(),
        Value::String(session.session_file.clone()),
    );
    create.insert(
        "config".to_string(),
        serde_json::to_value(&session.config).unwrap_or(Value::Null),
    );
    if let Some(runtime_metadata) = &runtime_metadata {
        create.insert(
            "runtimeMetadata".to_string(),
            serde_json::to_value(runtime_metadata).unwrap_or(Value::Null),
        );
    }
    if let Some(client_env) = &session.client_env {
        create.insert(
            "env".to_string(),
            serde_json::to_value(client_env).unwrap_or(Value::Null),
        );
    }
    let create_response = request_command(client, create, 120_000).await?;
    if !create_response.success {
        let error = create_response
            .error
            .unwrap_or_else(|| "Daemon request failed".to_string());
        eprintln!(
            "{}",
            yellow(&format!("Warning: could not restore {}: {error}", session.session_file))
        );
        return Ok(RestoreDaemonUpdateRestartSessionResult {
            restored: false,
            resumed: false,
            failure_message: Some(error),
        });
    }
    let active_session_id = read_created_active_session_id(create_response.data.as_ref())?;
    restored_active_session_ids.insert(session.active_session_id.clone(), active_session_id.clone());
    if Some(session.active_session_id.as_str()) == restart_origin_active_session_id {
        let mut notice: DaemonCommandBody = Map::from_iter([(
            "type".to_string(),
            Value::String("append_custom_message".to_string()),
        )]);
        notice.insert(
            "activeSessionId".to_string(),
            Value::String(active_session_id.clone()),
        );
        notice.insert(
            "message".to_string(),
            serde_json::json!({
                "customType": "prime-agent.update_complete",
                "content": format!("Prime Agent updated to v{VERSION}. This daemon session was restored after the update."),
                "display": true,
                "details": { "version": VERSION },
            }),
        );
        match request_command(client, notice, 30_000).await {
            Ok(response) if response.success => {}
            Ok(response) => {
                eprintln!(
                    "{}",
                    yellow(&format!(
                        "Warning: could not record update completion in {}: {}",
                        session.session_file,
                        response.error.unwrap_or_else(|| "Daemon request failed".to_string())
                    ))
                );
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    yellow(&format!(
                        "Warning: could not record update completion in {}: {error}",
                        session.session_file
                    ))
                );
            }
        }
    }
    restore_next_turn_messages(
        client,
        &active_session_id,
        &session.session_file,
        &session.queue.next_turn,
    )
    .await;
    if !session.should_resume {
        return Ok(RestoreDaemonUpdateRestartSessionResult {
            restored: true,
            resumed: false,
            failure_message: None,
        });
    }

    let needs_continuation_prompt = session.was_streaming
        || session.was_compacting
        || session.was_bash_running
        || session.had_running_rlm_children
        || session.was_retrying
        || session.had_accepted_prompt_in_flight;
    let mut resumed_session = false;
    let mut restored_queued_work = false;
    if !session.queue.actions.actions.is_empty() {
        let mut restore: DaemonCommandBody = Map::from_iter([(
            "type".to_string(),
            Value::String("restore_actions".to_string()),
        )]);
        restore.insert(
            "activeSessionId".to_string(),
            Value::String(active_session_id.clone()),
        );
        restore.insert(
            "snapshot".to_string(),
            serde_json::to_value(&session.queue.actions).unwrap_or(Value::Null),
        );
        match request_command(client, restore, 30_000).await {
            Ok(response) if response.success => restored_queued_work = true,
            Ok(response) => {
                eprintln!(
                    "{}",
                    yellow(&format!(
                        "Warning: could not restore queued actions for {}: {}",
                        session.session_file,
                        response.error.unwrap_or_else(|| "Daemon request failed".to_string())
                    ))
                );
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    yellow(&format!(
                        "Warning: could not restore queued actions for {}: {error}",
                        session.session_file
                    ))
                );
            }
        }
    }
    let restored_accepted_turn = restored_queued_work
        && session.queue.actions.actions.iter().any(|action| match &action.payload {
            SessionActionRecoveryPayload::Turn {
                queue_visible,
                accepted_before_completion,
                ..
            } => !queue_visible && *accepted_before_completion,
            SessionActionRecoveryPayload::SessionCommand { .. } => false,
        });
    if needs_continuation_prompt && !restored_accepted_turn {
        let mut prompt: DaemonCommandBody = Map::from_iter([(
            "type".to_string(),
            Value::String("prompt".to_string()),
        )]);
        prompt.insert(
            "activeSessionId".to_string(),
            Value::String(active_session_id.clone()),
        );
        prompt.insert(
            "message".to_string(),
            Value::String(UPDATE_RESTART_CONTINUATION_PROMPT.to_string()),
        );
        prompt.insert("expandPromptTemplates".to_string(), Value::Bool(false));
        match request_command(client, prompt, 120_000).await {
            Ok(response) if response.success => resumed_session = true,
            Ok(response) => {
                eprintln!(
                    "{}",
                    yellow(&format!(
                        "Warning: could not resume {}: {}",
                        session.session_file,
                        response.error.unwrap_or_else(|| "Daemon request failed".to_string())
                    ))
                );
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    yellow(&format!("Warning: could not resume {}: {error}", session.session_file))
                );
            }
        }
    }
    if !resumed_session && restored_queued_work {
        let mut resume_queue: DaemonCommandBody = Map::from_iter([(
            "type".to_string(),
            Value::String("resume_queue".to_string()),
        )]);
        resume_queue.insert(
            "activeSessionId".to_string(),
            Value::String(active_session_id.clone()),
        );
        match request_command(client, resume_queue, 30_000).await {
            Ok(response) if response.success => resumed_session = true,
            Ok(response) => {
                eprintln!(
                    "{}",
                    yellow(&format!(
                        "Warning: could not resume queued work for {}: {}",
                        session.session_file,
                        response.error.unwrap_or_else(|| "Daemon request failed".to_string())
                    ))
                );
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    yellow(&format!(
                        "Warning: could not resume queued work for {}: {error}",
                        session.session_file
                    ))
                );
            }
        }
    }
    Ok(RestoreDaemonUpdateRestartSessionResult {
        restored: true,
        resumed: resumed_session,
        failure_message: None,
    })
}

async fn restore_daemon_update_restart(
    socket_path: &str,
    manifest: &DaemonUpdateRestartManifest,
    restart_origin_active_session_id: Option<&str>,
    on_progress: &dyn Fn(&RestoreDaemonUpdateRestartResult),
) -> Result<RestoreDaemonUpdateRestartResult, String> {
    let mut restored_active_session_ids: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
    if manifest.sessions.is_empty() {
        return Ok(RestoreDaemonUpdateRestartResult {
            total: 0,
            restored: 0,
            resumed: 0,
            failed: 0,
            failures: Vec::new(),
        });
    }
    let client = DaemonClient::create(socket_path);
    client.connect(10_000).await.map_err(|error| error.message())?;
    let mut restored = 0i64;
    let mut resumed = 0i64;
    let mut failures: Vec<DaemonUpdateRestartFailure> = Vec::new();
    for session in &manifest.sessions {
        match restore_daemon_update_restart_session(
            &client,
            session,
            &mut restored_active_session_ids,
            restart_origin_active_session_id,
        )
        .await
        {
            Ok(result) => {
                if result.restored {
                    restored += 1;
                }
                if result.resumed {
                    resumed += 1;
                }
                if !result.restored {
                    failures.push(DaemonUpdateRestartFailure {
                        session_file: session.session_file.clone(),
                        message: result
                            .failure_message
                            .unwrap_or_else(|| "unknown restore error".to_string()),
                    });
                }
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    yellow(&format!("Warning: could not restore {}: {error}", session.session_file))
                );
                failures.push(DaemonUpdateRestartFailure {
                    session_file: session.session_file.clone(),
                    message: error,
                });
            }
        }
        on_progress(&RestoreDaemonUpdateRestartResult {
            total: manifest.sessions.len() as i64,
            restored,
            resumed,
            failed: failures.len() as i64,
            failures: failures.clone(),
        });
    }
    client.close().await;
    println!(
        "{}",
        green(&format!(
            "Restored {restored} agent session{}",
            if restored == 1 { "" } else { "s" }
        ))
    );
    if resumed > 0 {
        println!(
            "{}",
            green(&format!(
                "Resumed {resumed} interrupted session{}",
                if resumed == 1 { "" } else { "s" }
            ))
        );
    }
    Ok(RestoreDaemonUpdateRestartResult {
        total: manifest.sessions.len() as i64,
        restored,
        resumed,
        failed: manifest.sessions.len() as i64 - restored,
        failures,
    })
}

fn format_unknown_error(error: String) -> String {
    error
}

fn process_identity_from_daemon_hello(hello: Option<&Value>) -> Option<DaemonUpdateRestartProcessIdentity> {
    let pid = hello
        .and_then(|hello| hello.get("supervisorPid"))
        .and_then(Value::as_i64)?;
    if pid <= 0 {
        return None;
    }
    let field = |key: &str| hello.and_then(|hello| hello.get(key)).and_then(Value::as_str).map(str::to_string);
    Some(DaemonUpdateRestartProcessIdentity {
        pid,
        process_start_id: field("supervisorProcessStartId"),
        supervisor_generation: field("supervisorGeneration"),
        supervisor_owner_token: field("supervisorOwnerToken"),
    })
}

fn validate_replacement_daemon(
    socket_path: &str,
    hello: &Value,
    predecessor: Option<&DaemonUpdateRestartProcessIdentity>,
) -> Result<DaemonUpdateRestartProcessIdentity, String> {
    let protocol_version = hello
        .get("protocol")
        .and_then(|protocol| protocol.get("version"))
        .and_then(Value::as_u64);
    let schema_id = hello.get("schemaId").and_then(Value::as_str);
    let app_version = hello.get("appVersion").and_then(Value::as_str);
    if protocol_version != Some(DAEMON_PROTOCOL_VERSION as u64)
        || schema_id != Some(DAEMON_SCHEMA_ID)
        || app_version != Some(VERSION)
    {
        return Err(format!(
            "Replacement daemon is v{}/proto{}/schema {}, expected v{VERSION}/proto{DAEMON_PROTOCOL_VERSION}/schema {DAEMON_SCHEMA_ID}",
            app_version.unwrap_or("unknown"),
            protocol_version.map(|version| version.to_string()).unwrap_or_else(|| "unknown".to_string()),
            schema_id.unwrap_or("legacy"),
        ));
    }
    let supervisor_socket_path = hello.get("supervisorSocketPath").and_then(Value::as_str);
    if supervisor_socket_path.is_none()
        || normalize_socket_path(supervisor_socket_path.unwrap_or_default(), None)
            != normalize_socket_path(socket_path, None)
    {
        return Err(format!("Replacement daemon identity does not match {socket_path}"));
    }
    let successor = process_identity_from_daemon_hello(Some(hello))
        .ok_or_else(|| format!("Replacement daemon on {socket_path} did not provide an identity fence"))?;
    if successor.supervisor_generation.is_none() || successor.supervisor_owner_token.is_none() {
        return Err(format!("Replacement daemon on {socket_path} did not provide an identity fence"));
    }
    if let Some(predecessor) = predecessor {
        let same_generation = predecessor.supervisor_generation.is_some()
            && successor.supervisor_generation == predecessor.supervisor_generation;
        let same_token = predecessor.supervisor_owner_token.is_some()
            && successor.supervisor_owner_token == predecessor.supervisor_owner_token;
        let same_process = successor.pid == predecessor.pid
            && predecessor.process_start_id.is_some()
            && successor.process_start_id == predecessor.process_start_id;
        if same_generation || same_token || same_process {
            return Err(format!("Replacement daemon on {socket_path} still has the predecessor identity"));
        }
    }
    Ok(successor)
}

/// `runDaemonUpdateRestartCoordinator(options)`.
pub async fn run_daemon_update_restart_coordinator(
    options: &RunDaemonUpdateRestartCoordinatorOptions,
) -> DaemonUpdateRestartStatus {
    let status_writer = Arc::new(DaemonUpdateRestartStatusWriter::new(
        &options.status_path,
        &format!("{}-{}", std::process::id(), now_ms() as i64),
        &options.socket_path,
    ));
    let stop_status_heartbeat = status_writer.start_heartbeat();
    let mut lease: Option<DaemonUpdateRestartCoordinatorLease> = None;
    let mut shutdown_admission: Option<Arc<DaemonShutdownAdmission>> = None;
    let mut connected_client: Option<Arc<DaemonClient>> = None;
    let mut manifest: Option<DaemonUpdateRestartManifest> = None;

    let run = async {
        match acquire_daemon_update_restart_coordinator(AcquireDaemonUpdateRestartCoordinatorOptions {
            request_id: status_writer.current().request_id,
            socket_path: options.socket_path.clone(),
            status_path: options.status_path.clone(),
            registry_dir: None,
        })
        .await
        {
            Ok(acquired) => lease = Some(acquired),
            Err(error) => {
                if !error.starts_with("Another daemon update restart is already running") {
                    return Err(error);
                }
                // blocked_on: the ported `acquireDaemonUpdateRestartCoordinator`
                // reports `DaemonUpdateRestartCoordinatorAlreadyRunningError` as a
                // message string, so `error.record` is not reachable. The record is
                // re-read from the coordinator registry instead, which yields the
                // same object the TypeScript's error carries.
                let record = read_running_coordinator_record(&options.socket_path).ok_or(error)?;
                let active_status = wait_for_active_daemon_update_restart_coordinator(
                    &record,
                    DEFAULT_COORDINATOR_PROGRESS_TIMEOUT_MS,
                )
                .await?;
                status_writer.update(DaemonUpdateRestartUpdate {
                    phase: Some(active_status.phase),
                    counts: Some(active_status.counts.clone()),
                    predecessor: Some(active_status.predecessor.clone()),
                    successor: Some(active_status.successor.clone()),
                    failures: Some(active_status.failures.clone()),
                    message: Some(active_status.message.clone()),
                    ..DaemonUpdateRestartUpdate::default()
                });
                return Err(ALREADY_RUNNING.to_string());
            }
        }

        shutdown_admission = Some(
            crate::modes::daemon::daemon_supervisor_ownership::acquire_daemon_shutdown_admission()
                .await?,
        );
        let daemon_probe = probe_running_daemon_sessions(&options.socket_path).await;
        let report_restore_progress = |progress: &RestoreDaemonUpdateRestartResult| {
            status_writer.update(DaemonUpdateRestartUpdate {
                counts: Some(DaemonUpdateRestartCounts {
                    total: progress.total,
                    restored: progress.restored,
                    resumed: progress.resumed,
                    failed: progress.failed,
                }),
                failures: Some(Some(progress.failures.clone())),
                ..DaemonUpdateRestartUpdate::default()
            });
        };
        let mut predecessor: Option<DaemonUpdateRestartProcessIdentity> = None;
        if daemon_probe.reachable {
            let client = DaemonClient::create(&options.socket_path);
            client.connect(1000).await.map_err(|error| error.message())?;
            let hello = client.wait_for_hello(2000).await.map_err(|error| error.message())?;
            let hello_value = hello.raw.clone();
            connected_client = Some(Arc::clone(&client));
            predecessor = process_identity_from_daemon_hello(Some(&hello_value));
            status_writer.update(DaemonUpdateRestartUpdate {
                phase: Some(DaemonUpdateRestartPhase::Preparing),
                predecessor: Some(predecessor.clone()),
                ..DaemonUpdateRestartUpdate::default()
            });
            match prepare_connected_daemon_update_restart(
                &client,
                &options.socket_path,
                &options.agent_dir,
                Some(&hello_value),
            )
            .await
            {
                Ok(prepared) => manifest = Some(prepared),
                Err(error) => {
                    let daemon_lacks_prepare_command = is_unknown_daemon_command_error(
                        &error,
                        "prepare_update_restart",
                    );
                    if daemon_probe_may_have_busy_sessions(&daemon_probe) || !daemon_lacks_prepare_command {
                        return Err(format!(
                            "Could not prepare daemon sessions for automatic resume; the previous daemon is still running ({error})"
                        ));
                    }
                }
            }
            status_writer.update(DaemonUpdateRestartUpdate {
                phase: Some(DaemonUpdateRestartPhase::Stopping),
                counts: Some(DaemonUpdateRestartCounts {
                    total: manifest.as_ref().map(|manifest| manifest.sessions.len() as i64).unwrap_or(0),
                    restored: 0,
                    resumed: 0,
                    failed: 0,
                }),
                ..DaemonUpdateRestartUpdate::default()
            });
            shutdown_admission
                .as_ref()
                .expect("shutdown admission")
                .assert_or_renew()
                .await
                .map_err(|error| error.message)?;
            let launch_hello = crate::cli::daemon_launch::DaemonHello::from_json(&hello_value);
            let stopped = shutdown_connected_daemon_and_wait(
                &options.socket_path,
                10_000.0,
                launch_hello.as_ref(),
            )
            .await;
            connected_client = None;
            if !stopped {
                let remaining_daemon = probe_running_daemon_sessions(&options.socket_path).await;
                if remaining_daemon.reachable {
                    if let Some(manifest) = &manifest {
                        if let Ok(restore_result) = restore_daemon_update_restart(
                            &options.socket_path,
                            manifest,
                            options.origin_active_session_id.as_deref(),
                            &report_restore_progress,
                        )
                        .await
                        {
                            clear_prepared_daemon_update_restart_manifest(
                                &options.socket_path,
                                &options.agent_dir,
                            );
                            status_writer.update(DaemonUpdateRestartUpdate {
                                counts: Some(DaemonUpdateRestartCounts {
                                    total: restore_result.total,
                                    restored: restore_result.restored,
                                    resumed: restore_result.resumed,
                                    failed: restore_result.failed,
                                }),
                                failures: if restore_result.failures.is_empty() {
                                    None
                                } else {
                                    Some(Some(restore_result.failures.clone()))
                                },
                                ..DaemonUpdateRestartUpdate::default()
                            });
                        }
                    }
                    return Err(format!(
                        "Could not stop the predecessor daemon on {}",
                        options.socket_path
                    ));
                }
            }
        } else {
            manifest = try_read_prepared_daemon_update_restart_manifest(&options.socket_path, &options.agent_dir);
            if !has_restorable_daemon_update_restart(manifest.as_ref()) {
                status_writer.update(DaemonUpdateRestartUpdate {
                    phase: Some(DaemonUpdateRestartPhase::Skipped),
                    message: Some(Some("No running daemon needed to be restarted".to_string())),
                    ..DaemonUpdateRestartUpdate::default()
                });
                return Ok(());
            }
        }

        status_writer.update(DaemonUpdateRestartUpdate {
            phase: Some(DaemonUpdateRestartPhase::StartingDaemon),
            ..DaemonUpdateRestartUpdate::default()
        });
        wait_for_daemon_startup_fence(
            &options.socket_path,
            UPDATE_RESTART_PREDECESSOR_FENCE_TIMEOUT_MS,
            None,
        )
        .await?;
        shutdown_admission
            .as_ref()
            .expect("shutdown admission")
            .assert_or_renew()
            .await
            .map_err(|error| error.message)?;
        let admission = shutdown_admission.take().expect("shutdown admission");
        admissions_release(&admission).await;
        ensure_interactive_daemon_running(&options.socket_path, None)
            .await
            .map_err(|error| error)?;
        let successor_client = DaemonClient::create(&options.socket_path);
        let successor = {
            successor_client
                .connect(1000)
                .await
                .map_err(|error| error.message())?;
            let successor_hello = successor_client
                .wait_for_hello(60_000)
                .await
                .map_err(|error| error.message())?;
            let validated = validate_replacement_daemon(
                &options.socket_path,
                &successor_hello.raw,
                predecessor.as_ref(),
            );
            successor_client.close().await;
            validated?
        };
        status_writer.update(DaemonUpdateRestartUpdate {
            phase: Some(DaemonUpdateRestartPhase::Restoring),
            successor: Some(Some(successor)),
            ..DaemonUpdateRestartUpdate::default()
        });

        let mut counts = DaemonUpdateRestartCounts::default();
        let mut failures: Vec<DaemonUpdateRestartFailure> = Vec::new();
        if let Some(manifest) = &manifest {
            let restore_result = restore_daemon_update_restart(
                &options.socket_path,
                manifest,
                options.origin_active_session_id.as_deref(),
                &report_restore_progress,
            )
            .await?;
            counts = DaemonUpdateRestartCounts {
                total: restore_result.total,
                restored: restore_result.restored,
                resumed: restore_result.resumed,
                failed: restore_result.failed,
            };
            failures = restore_result.failures;
            clear_prepared_daemon_update_restart_manifest(&options.socket_path, &options.agent_dir);
        }
        let message = if counts.failed > 0 {
            format!(
                "Restarted the daemon with {} session restore failure{}",
                counts.failed,
                if counts.failed == 1 { "" } else { "s" }
            )
        } else {
            "Restarted the daemon after the update".to_string()
        };
        status_writer.update(DaemonUpdateRestartUpdate {
            phase: Some(DaemonUpdateRestartPhase::Complete),
            counts: Some(counts),
            failures: if failures.is_empty() { None } else { Some(Some(failures)) },
            message: Some(Some(message)),
            ..DaemonUpdateRestartUpdate::default()
        });
        Ok(())
    }
    .await;

    if let Err(error) = run {
        if error != ALREADY_RUNNING {
            status_writer.update(DaemonUpdateRestartUpdate {
                phase: Some(DaemonUpdateRestartPhase::Failed),
                message: Some(Some(format_unknown_error(error))),
                ..DaemonUpdateRestartUpdate::default()
            });
        }
    }

    stop_status_heartbeat.abort();
    if let Some(client) = connected_client {
        client.close().await;
    }
    if let Some(admission) = shutdown_admission {
        admissions_release(&admission).await;
    }
    if let Some(lease) = &lease {
        let _ = lease.release().await;
    }
    status_writer.current()
}

async fn admissions_release(admission: &Arc<DaemonShutdownAdmission>) {
    let _ = admission.release().await;
}

/// `RunDaemonUpdateRestartCoordinatorOptions`.
#[derive(Debug, Clone)]
pub struct RunDaemonUpdateRestartCoordinatorOptions {
    pub socket_path: String,
    pub agent_dir: String,
    pub status_path: String,
    pub origin_active_session_id: Option<String>,
}

const ALREADY_RUNNING: &str = "another coordinator is already running";

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// `error.record` for `DaemonUpdateRestartCoordinatorAlreadyRunningError`.
///
/// blocked_on: `coordinatorRecordPath` and `defaultCoordinatorRegistryDir` are
/// private to `cli/daemon-update-restart.ts`'s port (another slice), so this
/// module rebuilds the same registry path that file publishes and reads the
/// record back.
fn read_running_coordinator_record(socket_path: &str) -> Option<DaemonUpdateRestartCoordinatorRecord> {
    use sha2::Digest;
    let registry_dir = Path::new(&default_daemon_socket_dir()).join("update-restart-coordinators");
    let normalized = normalize_socket_path(socket_path, None);
    let mut hasher = sha2::Sha256::new();
    hasher.update(normalized.as_bytes());
    let path = registry_dir.join(format!("{:x}.json", hasher.finalize()));
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// `handleConfigCommand(args)`.
pub async fn handle_config_command(args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("config") {
        return false;
    }

    let cwd = current_cwd();
    let agent_dir = get_agent_dir();
    let mut settings_manager = SettingsManager::create(&cwd, Some(&agent_dir));
    report_settings_errors(&mut settings_manager, "config command");
    let settings_manager = Arc::new(std::sync::Mutex::new(settings_manager));
    let package_manager = DefaultPackageManager::new(PackageManagerOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        settings_manager,
        bundled_skills_dir: None,
        extra_builtin_skill_overrides: None,
    });
    let resolved_paths = match package_manager.resolve(None).await {
        Ok(resolved_paths) => resolved_paths,
        Err(error) => {
            eprintln!("{}", red(&format!("Error: {error}")));
            std::process::exit(1);
        }
    };

    // `selectConfig` receives the same `SettingsManager` instance in the
    // TypeScript. `PackageManagerOptions` consumes its manager inside an
    // `Arc<Mutex<..>>`, so the port builds a second manager over the same
    // settings files (post-`resolve()`, so migration writes are already on disk).
    let settings_manager = SettingsManager::create(&cwd, Some(&agent_dir));

    if let Err(error) = select_config(ConfigSelectorOptions {
        resolved_paths,
        settings_manager,
        cwd,
        agent_dir,
    })
    .await
    {
        eprintln!("{}", red(&format!("Error: {error}")));
    }

    std::process::exit(0);
}

fn current_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

/// `handlePackageCommand(args)`.
pub async fn handle_package_command(args: &[String]) -> bool {
    let options = match parse_package_command(args) {
        Some(options) => options,
        None => return false,
    };

    if options.help {
        print_package_command_help(&options.command);
        return true;
    }

    if let Some(invalid_option) = &options.invalid_option {
        if invalid_option == "-l" && (options.command == "install" || options.command == "remove") {
            eprintln!("{}", red("Option -l was removed. Use \"--local\"."));
            set_exit_code(1);
            return true;
        }
        eprintln!(
            "{}",
            red(&format!(
                "Unknown option {invalid_option} for \"{}\".",
                options.command
            ))
        );
        eprintln!(
            "{}",
            dim(&format!(
                "Use \"{APP_NAME} --help\" or \"{}\".",
                get_package_command_usage(&options.command)
            ))
        );
        set_exit_code(1);
        return true;
    }

    if let Some(missing_option_value) = &options.missing_option_value {
        eprintln!("{}", red(&format!("Missing value for {missing_option_value}.")));
        eprintln!(
            "{}",
            dim(&format!("Usage: {}", get_package_command_usage(&options.command)))
        );
        set_exit_code(1);
        return true;
    }

    if let Some(invalid_argument) = &options.invalid_argument {
        eprintln!("{}", red(&format!("Unexpected argument {invalid_argument}.")));
        eprintln!(
            "{}",
            dim(&format!("Usage: {}", get_package_command_usage(&options.command)))
        );
        set_exit_code(1);
        return true;
    }

    if let Some(conflicting_options) = &options.conflicting_options {
        eprintln!("{}", red(conflicting_options));
        eprintln!(
            "{}",
            dim(&format!("Usage: {}", get_package_command_usage(&options.command)))
        );
        set_exit_code(1);
        return true;
    }

    if options.restart_coordinator {
        let agent_dir = get_agent_dir();
        let status_path = options.restart_status_path.clone();
        let daemon_socket_path = options.daemon_socket_path.clone();
        let restart_directory = resolve_path(&Path::new(&agent_dir).join("update-restarts"));
        let status_path_valid = status_path.as_ref().is_some_and(|status_path| {
            resolve_path(Path::new(status_path)).starts_with(&format!(
                "{restart_directory}{}",
                std::path::MAIN_SEPARATOR
            ))
        });
        if !status_path_valid || daemon_socket_path.is_none() {
            eprintln!("{}", red("Invalid daemon update restart coordinator invocation."));
            set_exit_code(1);
            return true;
        }
        let status = run_daemon_update_restart_coordinator(&RunDaemonUpdateRestartCoordinatorOptions {
            socket_path: daemon_socket_path.unwrap_or_default(),
            agent_dir,
            status_path: status_path.unwrap_or_default(),
            origin_active_session_id: options.restart_origin_active_session_id.clone(),
        })
        .await;
        if status.phase == DaemonUpdateRestartPhase::Failed {
            set_exit_code(1);
        }
        return true;
    }

    if options.restart_status_path.is_some() || options.restart_origin_active_session_id.is_some() {
        eprintln!("{}", red("Invalid daemon update restart coordinator invocation."));
        set_exit_code(1);
        return true;
    }

    let source = options.source.clone();
    if (options.command == "install" || options.command == "remove") && source.is_none() {
        eprintln!("{}", red(&format!("Missing {} source.", options.command)));
        eprintln!(
            "{}",
            dim(&format!("Usage: {}", get_package_command_usage(&options.command)))
        );
        set_exit_code(1);
        return true;
    }

    let cwd = current_cwd();
    let agent_dir = get_agent_dir();
    let mut settings_manager = SettingsManager::create(&cwd, Some(&agent_dir));
    report_settings_errors(&mut settings_manager, "package command");
    let self_update_npm_command = settings_manager
        .get_global_settings()
        .get("npmCommand")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<String>>()
        });

    let settings_manager = Arc::new(std::sync::Mutex::new(settings_manager));
    let mut package_manager = DefaultPackageManager::new(PackageManagerOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        settings_manager,
        bundled_skills_dir: None,
        extra_builtin_skill_overrides: None,
    });

    package_manager.set_progress_callback(Some(Arc::new(|event: ProgressEvent| {
        if event.event_type == "start" {
            if let Some(message) = &event.message {
                println!("{}", dim(&format!("{message}\n")));
            }
        }
    })));

    let result = run_package_command(
        &package_manager,
        &options,
        source.as_deref(),
        self_update_npm_command.as_deref(),
        &cwd,
        &agent_dir,
    )
    .await;
    if let Err(error) = result {
        eprintln!("{}", red(&format!("Error: {error}")));
        set_exit_code(1);
    }
    true
}

async fn run_package_command(
    package_manager: &DefaultPackageManager,
    options: &PackageCommandOptions,
    source: Option<&str>,
    self_update_npm_command: Option<&[String]>,
    cwd: &str,
    agent_dir: &str,
) -> Result<(), String> {
    match options.command.as_str() {
        "install" => {
            package_manager
                .install_and_persist(source.expect("install source"), options.local)
                .await?;
            println!("{}", green(&format!("Installed {}", source.expect("install source"))));
            Ok(())
        }
        "remove" => {
            let removed = package_manager
                .remove_and_persist(source.expect("remove source"), options.local)
                .await?;
            if !removed {
                eprintln!("{}", red(&format!("No matching package found for {}", source.unwrap_or_default())));
                set_exit_code(1);
                return Ok(());
            }
            println!("{}", green(&format!("Removed {}", source.expect("remove source"))));
            Ok(())
        }
        "list" => {
            let configured_packages = package_manager.list_configured_packages();
            let user_packages: Vec<_> = configured_packages
                .iter()
                .filter(|pkg| pkg.scope == "user")
                .collect();
            let project_packages: Vec<_> = configured_packages
                .iter()
                .filter(|pkg| pkg.scope == "project")
                .collect();

            if configured_packages.is_empty() {
                println!("{}", dim("No packages installed."));
                return Ok(());
            }

            let format_package = |pkg: &crate::core::package_manager::ConfiguredPackage| {
                let display = if pkg.filtered {
                    format!("{} (filtered)", pkg.source)
                } else {
                    pkg.source.clone()
                };
                println!("  {display}");
                if let Some(installed_path) = &pkg.installed_path {
                    println!("{}", dim(&format!("    {installed_path}")));
                }
            };

            if !user_packages.is_empty() {
                println!("{}", bold("User packages:"));
                for pkg in &user_packages {
                    format_package(pkg);
                }
            }

            if !project_packages.is_empty() {
                if !user_packages.is_empty() {
                    println!();
                }
                println!("{}", bold("Project packages:"));
                for pkg in &project_packages {
                    format_package(pkg);
                }
            }

            Ok(())
        }
        "update" => {
            let target = options
                .update_target
                .clone()
                .unwrap_or(UpdateTarget::All);
            if update_target_includes_extensions(&target) {
                let update_source = match &target {
                    UpdateTarget::Extensions { source } => source.as_deref(),
                    _ => None,
                };
                package_manager.update(update_source).await?;
                match update_source {
                    Some(update_source) => println!("{}", green(&format!("Updated {update_source}"))),
                    None => println!("{}", green("Updated packages")),
                }
            }
            if update_target_includes_self(&target) {
                let self_update_plan = match get_self_update_plan(options.force).await {
                    Ok(plan) => plan,
                    Err(error) => {
                        eprintln!("{}", red(&format!("Error: {error}")));
                        set_exit_code(1);
                        return Ok(());
                    }
                };
                if !self_update_plan.should_run {
                    set_self_update_no_change_exit_code();
                    return Ok(());
                }
                let self_update_command = get_self_update_command(
                    PACKAGE_NAME,
                    self_update_npm_command,
                    &self_update_plan.install_spec,
                    &self_update_plan.package_name,
                );
                let Some(self_update_command) = self_update_command else {
                    print_self_update_unavailable(
                        self_update_npm_command,
                        &self_update_plan.install_spec,
                        &self_update_plan.package_name,
                    );
                    set_exit_code(1);
                    return Ok(());
                };
                // Confirm before the install, since upgrading the daemon afterward stops and resumes busy work.
                let daemon_socket_path = resolve_update_daemon_socket_path(options.daemon_socket_path.as_deref());
                let daemon_probe = probe_running_daemon_sessions(&daemon_socket_path).await;
                if !confirm_daemon_session_loss_before_update(
                    &daemon_probe,
                    options.force,
                    &ConfirmIo {
                        stdin_is_tty: stdin_is_tty(),
                        error: &|message: &str| eprintln!("{message}"),
                        read_line: &read_prompt_line,
                    },
                ) {
                    if stdin_is_tty() == Some(true) {
                        println!("{}", dim("Update cancelled."));
                    }
                    set_exit_code(1);
                    return Ok(());
                }
                if let Err(error) = run_self_update(&self_update_command).await {
                    eprintln!("{}", red(&format!("Error: {error}")));
                    print_self_update_fallback(&self_update_command);
                    set_exit_code(1);
                    return Ok(());
                }
                let version_change = self_update_plan
                    .target_version
                    .as_ref()
                    .map(|target_version| format!(" from v{VERSION} to v{target_version}"))
                    .unwrap_or_default();
                println!("{}", green(&format!("Updated {APP_NAME}{version_change}")));
                if std::env::var(SELF_UPDATE_INTERACTIVE_CHILD_ENV).as_deref() == Ok("1") {
                    return Ok(());
                }
                match launch_daemon_update_restart_coordinator(
                    crate::cli::daemon_update_restart::LaunchDaemonUpdateRestartCoordinatorOptions {
                        socket_path: daemon_socket_path,
                        agent_dir: agent_dir.to_string(),
                        cwd: Some(cwd.to_string()),
                        origin_active_session_id: std::env::var(DAEMON_WORKER_ACTIVE_SESSION_ID_ENV).ok(),
                        timeout_ms: None,
                    },
                )
                .await
                {
                    Ok(status) => report_daemon_update_restart_status(&status),
                    Err(error) => {
                        eprintln!(
                            "{}",
                            yellow(&format!(
                                "Warning: updated, but could not coordinate the daemon restart ({error})."
                            ))
                        );
                    }
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn resolve_path(path: &Path) -> String {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(path)
    };
    joined.to_string_lossy().to_string()
}

fn set_exit_code(code: i32) {
    std::env::set_var("PI_EXIT_CODE", code.to_string());
}

fn stdin_is_tty() -> Option<bool> {
    use std::io::IsTerminal;
    Some(std::io::stdin().is_terminal())
}

fn read_prompt_line(prompt: &str) -> String {
    use std::io::Write;
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(argv: &[&str]) -> Vec<String> {
        argv.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn self_update_sources_match_the_typescript_set() {
        for source in ["self", "pi", "prime-agent"] {
            assert!(is_self_update_source(source), "{source}");
        }
        for source in ["", "extensions", "github:owner/repo"] {
            assert!(!is_self_update_source(source), "{source}");
        }
    }

    #[test]
    fn install_parses_the_local_and_source_flags() {
        let options = parse_package_command(&args(&["install", "github:owner/repo"])).expect("install");
        assert_eq!(options.command, "install");
        assert_eq!(options.source.as_deref(), Some("github:owner/repo"));
        assert_eq!(options.update_target, None);

        let options = parse_package_command(&args(&["install", "--local"])).expect("install");
        assert!(options.local);
        assert_eq!(options.source, None);

        // `--extension` is an update-only flag. On install it is recorded as an
        // invalid option and "my-ext" is read as the positional source, so "src"
        // becomes the extra positional argument - exactly the TypeScript loop.
        let options = parse_package_command(&args(&["install", "--extension", "my-ext", "src"])).expect("install");
        assert_eq!(options.invalid_option.as_deref(), Some("--extension"));
        assert_eq!(options.update_target, None);
        assert_eq!(options.source.as_deref(), Some("my-ext"));
        assert_eq!(options.invalid_argument.as_deref(), Some("src"));

        // The update-only target the flag really builds.
        let options = parse_package_command(&args(&["update", "--extension", "my-ext"])).expect("update");
        assert_eq!(
            options.update_target,
            Some(UpdateTarget::Extensions { source: Some("my-ext".to_string()) })
        );
    }

    #[test]
    fn update_parses_each_target() {
        assert_eq!(
            parse_package_command(&args(&["update"])).expect("update").update_target,
            Some(UpdateTarget::All)
        );
        assert_eq!(
            parse_package_command(&args(&["update", "--self"]))
                .expect("update")
                .update_target,
            Some(UpdateTarget::Self_)
        );
        assert_eq!(
            parse_package_command(&args(&["update", "--extensions", "github:owner/repo"]))
                .expect("update")
                .update_target,
            Some(UpdateTarget::Extensions {
                source: Some("github:owner/repo".to_string())
            })
        );
        assert_eq!(
            parse_package_command(&args(&["update", "source-name"]))
                .expect("update")
                .update_target,
            Some(UpdateTarget::Extensions {
                source: Some("source-name".to_string())
            })
        );
    }

    #[test]
    fn update_target_membership_matches_its_variants() {
        assert!(update_target_includes_self(&UpdateTarget::All));
        assert!(!update_target_includes_self(&UpdateTarget::Extensions { source: None }));
        assert!(update_target_includes_extensions(&UpdateTarget::All));
        assert!(update_target_includes_extensions(&UpdateTarget::Extensions { source: None }));
        assert!(!update_target_includes_extensions(&UpdateTarget::Self_));
    }

    #[test]
    fn remove_and_list_require_only_the_optional_source() {
        let options = parse_package_command(&args(&["remove", "github:owner/repo"])).expect("remove");
        assert_eq!(options.command, "remove");
        assert_eq!(options.source.as_deref(), Some("github:owner/repo"));

        let options = parse_package_command(&args(&["list"])).expect("list");
        assert_eq!(options.command, "list");
        assert_eq!(options.source, None);
    }

    #[test]
    fn unknown_commands_and_removed_flags_are_rejected() {
        assert!(parse_package_command(&args(&["frobnicate"])).is_none());
        assert!(parse_package_command(&args(&[])).is_none());
        // `--daemon-socket` is only accepted for update and the coordinator flag.
        assert!(parse_package_command(&args(&["install", "--daemon-socket", "/tmp/d.sock"])).is_none());
        assert!(parse_package_command(&args(&["update", "--daemon-socket", "/tmp/d.sock"])).is_some());
        // `--self` conflicts with an explicit source.
        assert!(parse_package_command(&args(&["update", "--self", "src"])).is_none());
        assert!(parse_package_command(&args(&["update", "--local", "--self"])).is_none());
    }

    #[test]
    fn usage_lists_every_package_command() {
        // `getPackageCommandUsage(command)` renders one command at a time.
        for command in PACKAGE_COMMANDS {
            let usage = get_package_command_usage(command);
            assert!(usage.contains(command), "{command}");
        }
    }

    #[test]
    fn update_restart_manifest_round_trips_through_the_parser() {
        let manifest = serde_json::json!({
            "formatVersion": DAEMON_UPDATE_RESTART_FORMAT_VERSION,
            "createdAt": "2026-01-01T00:00:00.000Z",
            "sessions": [],
        });
        let parsed = parse_daemon_update_restart_manifest(&manifest).expect("manifest");
        assert_eq!(parsed.created_at, "2026-01-01T00:00:00.000Z");
        assert!(parsed.sessions.is_empty());
        assert!(!has_restorable_daemon_update_restart(Some(&parsed)));

        let unsupported = serde_json::json!({
            "formatVersion": DAEMON_UPDATE_RESTART_FORMAT_VERSION + 1.0,
            "createdAt": "2026-01-01T00:00:00.000Z",
            "sessions": [],
        });
        let error = parse_daemon_update_restart_manifest(&unsupported).expect_err("unsupported version");
        assert!(error.starts_with("Unsupported daemon update restart format version"));

        assert!(parse_daemon_update_restart_manifest(&serde_json::json!([])).is_err());
        assert!(parse_daemon_update_restart_manifest(&serde_json::json!({
            "formatVersion": DAEMON_UPDATE_RESTART_FORMAT_VERSION,
            "createdAt": "2026-01-01T00:00:00.000Z",
        }))
        .is_err());
    }

    #[test]
    fn a_manifest_with_a_session_is_restorable() {
        let mut session = serde_json::Map::new();
        session.insert("activeSessionId".to_string(), Value::String("a1".to_string()));
        session.insert("sessionId".to_string(), Value::String("s1".to_string()));
        session.insert("sessionFile".to_string(), Value::String("/sessions/s1.jsonl".to_string()));
        session.insert("cwd".to_string(), Value::String("/work".to_string()));
        session.insert(
            "config".to_string(),
            serde_json::json!({ "cwd": "/work", "agentDir": "/agent" }),
        );
        session.insert("queue".to_string(), serde_json::json!({
            "actions": { "formatVersion": 1, "actions": [] },
            "nextTurn": [],
        }));
        for field in [
            "shouldResume",
            "wasStreaming",
            "wasCompacting",
            "wasBashRunning",
            "hadRunningRlmChildren",
            "wasRetrying",
            "hadAcceptedPromptInFlight",
        ] {
            session.insert(field.to_string(), Value::Bool(false));
        }
        let manifest = serde_json::json!({
            "formatVersion": DAEMON_UPDATE_RESTART_FORMAT_VERSION,
            "createdAt": "2026-01-01T00:00:00.000Z",
            "sessions": [Value::Object(session)],
        });
        let parsed = parse_daemon_update_restart_manifest(&manifest).expect("manifest");
        assert_eq!(parsed.sessions.len(), 1);
        assert_eq!(parsed.sessions[0].active_session_id, "a1");
        assert!(!parsed.sessions[0].should_resume);
        assert!(has_restorable_daemon_update_restart(Some(&parsed)));
    }
}
