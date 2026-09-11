//! Port of packages/coding-agent/src/modes/agents-view/agents-view-mode.ts
//!
//! Integration seams (see blocked_on in evidence/status/ca-agents-view.json):
//! the TUI (`TUI`, `ProcessTerminal`, `CustomEditor`, `BrandSplashHeader`), the
//! interactive chat mode and the daemon socket client belong to other slices that
//! are still empty in this workspace. They are modelled here as small traits with
//! the exact operations this mode calls, so the port keeps its structure and its
//! behaviour is testable without a terminal.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex;

use super::agents_view_state::{
    build_unified_session_index, collapse_whitespace, compute_recursive_rollups,
    create_unattachable_child_open_result, filter_unified_sessions, format_heartbeat_badge,
    get_agents_view_selection_key, get_agents_view_session_title, get_agents_view_summary_identity,
    get_unified_session_ancestor_session_ids, has_unified_session_children, migrate_agents_view_identity_set,
    parse_js_timestamp, reconcile_unified_sessions, resolve_agents_view_left_result,
    resolve_agents_view_scope_frames, resolve_agents_view_selection_state, scope_to_session_subtree,
    section_title, should_apply_scope_resolution, should_show_agents_view_session, summary_for_unified_record,
    to_iso_string, transition_agents_view_scope, AgentsViewRecursiveRollup, AgentsViewRow, AgentsViewRowInput,
    AgentsViewRowKind, AgentsViewScopeBackResult, AgentsViewScopeFrame, AgentsViewScopeKey,
    AgentsViewSection, AgentsViewSelectionKey, AgentConnectionHeartbeat, AgentConnectionSavedSessionInfo,
    SessionLifecycle, SessionSummary, UnifiedSessionIndex, UnifiedSessionRecord,
    build_agents_view_rows,
};
use super::roster_store::{
    AgentsViewRosterStore, DaemonClientRequestOptions, DaemonResponse, DaemonTransportClient,
    STALE_ROSTER_DAEMON_MESSAGE,
};
use super::session_view_search::matches_search_text;

pub const HEARTBEAT_POLL_INTERVAL_MS: u64 = 15000;
pub const RECONNECT_TIMEOUT_MS: u64 = 120000;
pub const RECONNECT_RETRY_MS: u64 = 1000;
pub const EXIT_HINT_DURATION_MS: u64 = 2000;
pub const DELETE_CONFIRM_DURATION_MS: u64 = 2000;
pub const STATUS_MESSAGE_DURATION_MS: u64 = 4500;
pub const SEARCH_PROMPT_PLACEHOLDER: &str = "Search sessions";
pub const REPLY_PROMPT_FALLBACK_PLACEHOLDER: &str = "Write a reply to this agent";
pub const RESUME_PROMPT_PLACEHOLDER: &str = "Write a prompt to resume this session";
pub const COMPLETED_ROW_ICON: &str = "✓";
pub const NEEDS_INPUT_ROW_ICON: &str = "●";
pub const SELECTED_ROW_MARKER: &str = "\u{0}agents-view-selected-row\u{0}";
pub const CODE_ROW_MARKER: &str = "\u{0}agents-view-code-row\u{0}";

/// `WORKING_ICON_INTERVAL_MS` from modes/interactive/theme/working-icon.ts.
pub const WORKING_ICON_INTERVAL_MS: u64 = 250;
pub const WORKING_ICON_FRAMES: [&str; 4] = ["◇", "◈", "◆", "◈"];

pub fn working_icon_frame(frame: i64) -> &'static str {
    let len = WORKING_ICON_FRAMES.len() as i64;
    let index = ((frame % len) + len) % len;
    WORKING_ICON_FRAMES[index as usize]
}

/// Settings/theme/cwd surface of `InteractiveModeUiServices`.
pub trait AgentsViewUiServices: Send + Sync {
    fn get_initial_cwd(&self) -> String;
    fn get_theme(&self) -> String;
    fn get_themes(&self) -> Vec<String>;
    fn get_show_hardware_cursor(&self) -> bool;
    fn get_clear_on_shrink(&self) -> bool;
    fn get_editor_padding_x(&self) -> usize;
    fn get_autocomplete_max_visible(&self) -> usize;
}

/// `AgentSessionRuntimeConfig` fields this mode reads or copies.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentsViewRuntimeConfig {
    pub cwd: Option<String>,
    pub session_dir: Option<String>,
    pub telemetry_disabled: Option<bool>,
}

#[derive(Clone)]
pub struct AgentsViewModeOptions {
    pub socket_path: Option<String>,
    pub config: AgentsViewRuntimeConfig,
    pub ui_services: Arc<dyn AgentsViewUiServices>,
    pub migrated_providers: Option<Vec<String>>,
    pub model_fallback_message: Option<String>,
    pub startup_model_id: Option<String>,
    pub verbose: Option<bool>,
    pub reconnect_timeout_ms: Option<u64>,
    pub initial_session: Option<SessionSummary>,
    /// When set, the first view is rooted at this session's direct children.
    pub initial_scope_key: Option<AgentsViewScopeKey>,
}

/// `StartupNotices` (modes/shared/startup-notices.ts).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StartupNotices {
    pub new_version: Option<String>,
    pub package_updates: Vec<String>,
    pub tmux_warning: Option<String>,
}

pub const PACKAGE_UPDATE_NOTICE_PREFIX: &str = "Package updates available: ";
pub const UPDATE_AVAILABLE_NOTICE_PREFIX: &str = "Update available: ";
pub const TMUX_WARNING_NOTICE_PREFIX: &str = "tmux: ";

pub fn format_update_available_notice(version: &str) -> String {
    format!("{UPDATE_AVAILABLE_NOTICE_PREFIX}{version}")
}

pub fn format_package_update_notice(packages: &[String]) -> String {
    format!("{PACKAGE_UPDATE_NOTICE_PREFIX}{}", packages.join(", "))
}

pub fn format_tmux_warning_notice(warning: &str) -> String {
    format!("{TMUX_WARNING_NOTICE_PREFIX}{warning}")
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentsViewRunResult {
    Exit,
    ScopeBack {
        selection: SessionSummary,
        expanded_ancestor_session_ids: Vec<String>,
        return_chat: Option<SessionSummary>,
        has_children: bool,
    },
    Open {
        summary: SessionSummary,
        /// Row restored after chat closes; differs from summary only for an
        /// unattachable-child fallback.
        selection: Option<SessionSummary>,
        expanded_ancestor_session_ids: Option<Vec<String>>,
        has_children: Option<bool>,
        status_message: Option<String>,
    },
}

/// `AgentsViewPersistentState`.
#[derive(Clone, Debug, Default)]
pub struct AgentsViewPersistentState {
    pub selected_row_identity: Option<String>,
    pub back_session: Option<SessionSummary>,
    pub scope_frames: Option<Vec<AgentsViewScopeFrame>>,
    pub scope_root_summary: Option<SessionSummary>,
    pub selected_session_key: Option<AgentsViewSelectionKey>,
    /// Ancestor chain to re-expand on return to a nested agent. Kept by sessionId,
    /// not row identity, so it survives an active→persisted identity flip.
    pub pending_expanded_ancestor_session_ids: Option<Vec<String>>,
    pub expanded_subagent_parents: Option<HashSet<String>>,
    pub program_shown_parents: Option<HashSet<String>>,
    pub inactive_expanded: Option<bool>,
    /// Distinguish a deliberate collapse from the legacy collapsed-by-default state.
    pub inactive_visibility_explicit: Option<bool>,
    pub status_message: Option<String>,
    pub startup_notices: Option<StartupNotices>,
    pub query: Option<String>,
    /// Reused across agents-view instances (`persistentState.rosterClient`).
    pub roster_client: Option<DaemonTransportClient>,
    pub saved_sessions: Option<Vec<AgentConnectionSavedSessionInfo>>,
    pub last_successful_saved_sessions: Option<Vec<AgentConnectionSavedSessionInfo>>,
    pub saved_catalog_loaded: Option<bool>,
    pub last_successful_live_summaries: Option<Vec<SessionSummary>>,
    pub saved_catalog_generation: Option<i64>,
    pub heartbeats: Option<Vec<AgentConnectionHeartbeat>>,
}

/// `PromptCommand` = `Extract<DaemonCommand, { type: "prompt" }>`.
pub fn create_prompt_command(
    active_session_id: &str,
    message: &str,
    streaming_behavior: Option<&str>,
) -> Value {
    let mut command = serde_json::json!({
        "type": "prompt",
        "activeSessionId": active_session_id,
        "message": message,
    });
    if let Some(behavior) = streaming_behavior {
        command["streamingBehavior"] = Value::String(behavior.to_string());
    }
    command
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingDeleteAgent {
    pub identity: String,
    pub active_session_id: Option<String>,
    pub session_file: Option<String>,
    pub summary: SessionSummary,
    pub stopped: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingKillSubagent {
    pub identity: String,
    pub root_active_session_id: String,
    pub child_id: String,
}

pub const AGENTS_VIEW_COMMAND_NAMES: [&str; 2] = ["name", "kill"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentsViewCommandName {
    Name,
    Kill,
}

impl AgentsViewCommandName {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentsViewCommandName::Name => "name",
            AgentsViewCommandName::Kill => "kill",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentsViewCommand {
    pub name: AgentsViewCommandName,
    pub args: String,
}

/// Built-in slash commands this mode maps onto existing RPCs.
pub const BUILTIN_SLASH_COMMAND_NAMES: [&str; 37] = [
    "settings", "model", "effort", "fast", "scoped-models", "export", "import", "share", "copy", "btw",
    "name", "session", "system-prompt", "logs", "traces", "context", "changelog", "update", "hotkeys",
    "fork", "clone", "tree", "login", "logout", "mcp", "new", "compact", "refine", "goal", "autonomous",
    "rlm-max-depth", "heartbeat", "heartbeats", "resume", "reload", "fullscreen", "quit",
];

pub const BUILTIN_SLASH_COMMAND_ALIASES: [(&str, &str); 5] = [
    ("clear", "new"),
    ("usage", "context"),
    ("thinking", "effort"),
    ("rename", "name"),
    ("side", "btw"),
];

pub const SESSION_SLASH_COMMAND_NAMES: [&str; 4] = ["compact", "refine", "goal", "autonomous"];

pub fn is_session_slash_command_name(value: &str) -> bool {
    SESSION_SLASH_COMMAND_NAMES.contains(&value)
}

pub fn resolve_builtin_slash_command_name(name: &str) -> String {
    BUILTIN_SLASH_COMMAND_ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map(|(_, target)| (*target).to_string())
        .unwrap_or_else(|| name.to_string())
}

pub fn is_builtin_slash_command_name(name: &str) -> bool {
    BUILTIN_SLASH_COMMAND_NAMES.contains(&name)
        || BUILTIN_SLASH_COMMAND_ALIASES.iter().any(|(alias, _)| *alias == name)
}

/// `parseSlashCommand` (core/slash-commands.ts): `^\/(\S+)(?:\s+([\s\S]*))?$`.
pub fn parse_slash_command(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    let mut name = String::new();
    let mut args_start = None;
    for (index, ch) in rest.char_indices() {
        if ch.is_whitespace() {
            args_start = Some(index);
            break;
        }
        name.push(ch);
    }
    if name.is_empty() {
        return None;
    }
    let args = match args_start {
        None => String::new(),
        Some(index) => rest[index..].trim().to_string(),
    };
    Some((name, args))
}


/// `parseAgentsViewCommand`.
pub fn parse_agents_view_command(text: &str) -> Option<AgentsViewCommand> {
    let (name, args) = parse_slash_command(text)?;
    let resolved = resolve_builtin_slash_command_name(&name);
    match resolved.as_str() {
        "name" => Some(AgentsViewCommand { name: AgentsViewCommandName::Name, args }),
        "kill" => Some(AgentsViewCommand { name: AgentsViewCommandName::Kill, args }),
        _ => None,
    }
}

/// Reject recognized built-ins that are neither session-owned nor view
/// commands, so they are never sent to the model as plain prompt text.
pub fn get_reply_composer_command_rejection(text: &str) -> Option<String> {
    let (name, _args) = parse_slash_command(text)?;
    let resolved = resolve_builtin_slash_command_name(&name);
    if is_session_slash_command_name(&resolved) {
        return None;
    }
    if AGENTS_VIEW_COMMAND_NAMES.contains(&resolved.as_str()) {
        return None;
    }
    if !is_builtin_slash_command_name(&name) {
        return None;
    }
    Some(format!("/{name} is not available here; open the session to run it"))
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentsViewSlashCommand {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub argument_hint: Option<String>,
    pub takes_argument: Option<bool>,
}

pub const AGENTS_VIEW_COMMAND_DESCRIPTIONS: [(&str, &str, Option<&str>); 2] = [
    ("name", "Set session display name", Some("<name>")),
    ("kill", "Stop this agent's runtime (session stays resumable)", None),
];

/// `agentsViewSlashCommands`.
pub fn agents_view_slash_commands() -> Vec<AgentsViewSlashCommand> {
    AGENTS_VIEW_COMMAND_NAMES
        .iter()
        .map(|name| {
            let display = AGENTS_VIEW_COMMAND_DESCRIPTIONS
                .iter()
                .find(|(command, _, _)| command == name)
                .expect("description table covers every view command");
            AgentsViewSlashCommand {
                name: (*name).to_string(),
                aliases: Vec::new(),
                description: display.1.to_string(),
                argument_hint: display.2.map(|hint| hint.to_string()),
                takes_argument: if *name == "name" { Some(true) } else { None },
            }
        })
        .collect()
}

/// Session-owned built-ins offered by the reply composer autocomplete.
pub fn session_slash_commands() -> Vec<AgentsViewSlashCommand> {
    SESSION_SLASH_COMMAND_NAMES
        .iter()
        .map(|name| AgentsViewSlashCommand {
            name: (*name).to_string(),
            aliases: Vec::new(),
            description: String::new(),
            argument_hint: None,
            takes_argument: None,
        })
        .collect()
}

/// Autocomplete for the reply composer: session-owned plus view commands.
pub fn create_reply_composer_commands() -> Vec<AgentsViewSlashCommand> {
    let mut commands = session_slash_commands();
    commands.extend(agents_view_slash_commands());
    commands
}

pub fn resolve_current_reply_target_summary(
    records: &[UnifiedSessionRecord],
    target: &(String, SessionSummary),
    find_live: &dyn Fn(&str) -> Option<SessionSummary>,
) -> SessionSummary {
    let identity = get_agents_view_summary_identity(&target.1);
    let current = records
        .iter()
        .find(|record| record.identity == identity || record.identity_aliases.iter().any(|alias| alias == &identity));
    if let Some(record) = current {
        return summary_for_unified_record(record);
    }
    if let Some(active_session_id) = target.1.active_session_id.as_deref() {
        if let Some(live) = find_live(active_session_id) {
            return live;
        }
    }
    // A persisted target missing from the current live catalog can still be
    // resumed from its captured file, but its captured runtime id is stale.
    if target.1.session_file.is_some() && target.1.active_session_id.is_some() {
        let mut summary = target.1.clone();
        summary.active_session_id = None;
        summary.lifecycle = SessionLifecycle::Archived;
        summary.activity = super::agents_view_state::SessionActivity::Idle;
        return summary;
    }
    target.1.clone()
}

pub async fn resolve_agents_view_session_ui_services(
    options: &AgentsViewModeOptions,
    create_ui_services_for_session: Option<&(dyn Fn(&SessionSummary) -> super::agents_view_state::SessionSummary)>,
    summary: &SessionSummary,
) -> Arc<dyn AgentsViewUiServices> {
    match create_ui_services_for_session {
        Some(_) => options.ui_services.clone(),
        None => {
            let _ = summary;
            options.ui_services.clone()
        }
    }
}

/// Stripping cwd opens the session in its own stored directory; overrideCwd is
/// sent when that directory no longer exists so the daemon doesn't reject it.
pub fn create_agents_view_resume_config(
    config: &AgentsViewRuntimeConfig,
    override_cwd: Option<&str>,
) -> AgentsViewRuntimeConfig {
    let mut resume_config = config.clone();
    match override_cwd {
        Some(cwd) => resume_config.cwd = Some(cwd.to_string()),
        None => resume_config.cwd = None,
    }
    resume_config
}

pub fn create_agents_view_list_command() -> Value {
    // Omitting `all` returns daemon-resident sessions only; on-disk ones come back
    // through the view's saved-session catalog.
    serde_json::json!({ "type": "list" })
}

pub fn resolve_agents_view_active_summary_for_path(
    session_path: &str,
    summaries: &[SessionSummary],
) -> Option<SessionSummary> {
    let selected_path = resolve_absolute(&crate::utils::paths::canonicalize_path(session_path));
    summaries
        .iter()
        .find(|summary| {
            summary.active_session_id.is_some()
                && summary
                    .session_file
                    .as_deref()
                    .map(|file| {
                        resolve_absolute(&crate::utils::paths::canonicalize_path(file))
                            == selected_path
                    })
                    .unwrap_or(false)
        })
        .cloned()
}

// Status messages render in a single-row hint slot below the editor; embedded
// newlines would make that row taller than the layout accounts for and overlap
// the input, so flatten all whitespace runs to single spaces.
pub fn format_agents_view_status_line(text: &str) -> String {
    collapse_whitespace(text)
}

pub fn combine_agents_view_startup_notices(notices: &[Option<&str>]) -> Option<String> {
    let formatted: Vec<String> = notices
        .iter()
        .flatten()
        .map(|notice| format_agents_view_status_line(notice))
        .filter(|notice| !notice.is_empty())
        .collect();
    if formatted.is_empty() {
        None
    } else {
        Some(formatted.join(" · "))
    }
}

pub fn should_reconnect_agents_view_daemon(reason: Option<&str>) -> bool {
    reason != Some("shutdown")
}

pub fn create_agents_view_reply_headline(text: Option<&str>) -> Option<String> {
    text?.split('\n')
        .map(collapse_whitespace)
        .find(|line| !line.is_empty())
}

pub fn get_agents_view_depth(scope_root: Option<&SessionSummary>) -> i64 {
    match scope_root {
        Some(root) => root.rlm_depth.unwrap_or(0) + 1,
        None => 0,
    }
}

pub fn create_initial_agents_view_scope_frames(
    initial_scope_key: Option<&AgentsViewScopeKey>,
    return_chat: Option<&SessionSummary>,
) -> Vec<AgentsViewScopeFrame> {
    let Some(initial_scope_key) = initial_scope_key else {
        return Vec::new();
    };
    vec![AgentsViewScopeFrame {
        scope: initial_scope_key.clone(),
        return_chat: match return_chat {
            Some(chat) if chat.session_id == initial_scope_key.session_id => Some(chat.clone()),
            _ => None,
        },
    }]
}

pub fn create_initial_agents_view_persistent_state(
    initial_scope_key: Option<&AgentsViewScopeKey>,
    initial_session: Option<&SessionSummary>,
) -> AgentsViewPersistentState {
    // A scoped view excludes its root from its own rows, so anchoring the
    // selection on the entered-from chat could never resolve there and would
    // only arm the pending-anchor state for the whole catalog scan.
    let seed_selection = initial_session.is_some() && initial_scope_key.is_none();
    let mut state = AgentsViewPersistentState {
        back_session: initial_session.cloned(),
        ..AgentsViewPersistentState::default()
    };
    if seed_selection {
        let session = initial_session.expect("seedSelection implies initialSession");
        state.selected_row_identity = Some(get_agents_view_summary_identity(session));
        state.selected_session_key = Some(get_agents_view_selection_key(session));
    }
    if let Some(scope_key) = initial_scope_key {
        state.scope_frames = Some(create_initial_agents_view_scope_frames(Some(scope_key), initial_session));
        if let Some(session) = initial_session {
            state.last_successful_live_summaries = Some(vec![session.clone()]);
        }
    }
    state
}

pub fn create_scope_back_return_chat_open_result(
    result: &AgentsViewRunResult,
) -> Option<AgentsViewRunResult> {
    let AgentsViewRunResult::ScopeBack {
        return_chat,
        expanded_ancestor_session_ids,
        has_children,
        ..
    } = result
    else {
        return None;
    };
    let return_chat = return_chat.clone()?;
    Some(AgentsViewRunResult::Open {
        summary: return_chat,
        selection: None,
        expanded_ancestor_session_ids: Some(expanded_ancestor_session_ids.clone()),
        has_children: Some(*has_children),
        status_message: None,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpenedAgentsViewSession {
    pub summary: SessionSummary,
    pub cwd_fallback_notice: Option<String>,
}

pub fn resolve_agents_view_open_cwd(
    summary: &SessionSummary,
    fallback_cwd: Option<&str>,
) -> (Option<String>, Option<String>) {
    if summary.cwd.is_empty() || std::path::Path::new(&summary.cwd).exists() || fallback_cwd.is_none() {
        return (None, None);
    }
    let fallback = fallback_cwd.unwrap();
    (
        Some(fallback.to_string()),
        Some(format!(
            "Original directory is missing ({}); opened in {} instead.",
            summary.cwd, fallback
        )),
    )
}

/// `expectSessionSummary` / `isSessionSummary`.
pub fn expect_session_summary(value: &Value) -> Result<SessionSummary, String> {
    if !is_session_summary(value) {
        return Err("Daemon returned an invalid session summary".to_string());
    }
    serde_json::from_value(value.clone()).map_err(|_| "Daemon returned an invalid session summary".to_string())
}

pub fn is_session_summary(value: &Value) -> bool {
    is_record(value)
        && value.get("id").map(|id| id.is_string()).unwrap_or(false)
        && value.get("sessionId").map(|id| id.is_string()).unwrap_or(false)
}

pub fn expect_session_list(value: &Value) -> Result<Vec<SessionSummary>, String> {
    let Some(sessions) = value.get("sessions").filter(|sessions| sessions.is_array()) else {
        return Err("Daemon returned an invalid session list response".to_string());
    };
    if !is_record(value) {
        return Err("Daemon returned an invalid session list response".to_string());
    }
    let mut result = Vec::new();
    for session in sessions.as_array().unwrap() {
        if !is_session_summary(session) {
            return Err("Daemon returned an invalid session summary".to_string());
        }
        result.push(serde_json::from_value(session.clone()).map_err(|_| {
            "Daemon returned an invalid session summary".to_string()
        })?);
    }
    Ok(result)
}

pub fn is_record(value: &Value) -> bool {
    value.is_object()
}

pub fn require_daemon_data(response: &DaemonResponse) -> Result<Value, String> {
    if !response.success {
        return Err(response.error.clone().unwrap_or_else(|| "unknown error".to_string()));
    }
    Ok(response.data.clone().unwrap_or(Value::Null))
}

pub fn format_error(prefix: &str, message: &str) -> String {
    format_agents_view_status_line(&format!("{prefix}: {message}"))
}

pub fn is_unknown_active_session_error(message: &str) -> bool {
    message.starts_with("Unknown active session:")
}

/// `isUnknownDaemonCommandError(error, command)` (modes/daemon/daemon-protocol.ts).
pub fn is_unknown_daemon_command_error(message: &str, command: &str) -> bool {
    message.contains("Unknown daemon command") && message.contains(command)
}


/// Editor surface used by the agents view (`CustomEditor` in the reference).
/// The TUI slice owns the real component; this trait captures the calls the mode
/// makes so the behaviour stays testable. TODO(port): bind pi-tui's editor.
pub trait AgentsViewEditor: Send {
    fn set_text(&mut self, text: &str);
    fn get_text(&self) -> String;
    fn get_expanded_text(&self) -> String;
    fn set_placeholder(&mut self, text: &str);
    fn render(&mut self, width: usize) -> Vec<String>;
    fn invalidate(&mut self);
    fn get_lines(&self) -> Vec<String>;
    fn get_cursor(&self) -> (usize, usize);
    /// `handleInput(data)`; returns true when the editor consumed the key.
    fn handle_input(&mut self, data: &str) -> bool;
    fn focus(&mut self);
    fn is_focused(&self) -> bool;
}

/// `Component` from pi-tui.
pub trait AgentsViewComponent: Send {
    fn render(&mut self, width: usize) -> Vec<String>;
    fn invalidate(&mut self) {}
}

/// Terminal surface (`TUI` + `ProcessTerminal`).
pub trait AgentsViewTerminal: Send + Sync {
    fn rows(&self) -> usize;
    fn request_render(&self, force: bool);
    fn set_title(&self, title: &str);
}

/// `clippedFullscreenDockHeight(renderedRows, terminalRows)` from pi-tui.
pub fn clipped_fullscreen_dock_height(rendered_rows: usize, terminal_rows: usize) -> usize {
    // The dock never takes the whole screen: keep at least one content row.
    rendered_rows.min(terminal_rows.saturating_sub(1))
}

/// `truncateToWidth` from pi-tui; ANSI-aware in the reference. This port keeps the
/// plain-text path and leaves escape-aware truncation to the TUI slice.
pub fn truncate_to_width(value: &str, width: usize) -> String {
    if visible_width(value) <= width {
        return value.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in value.chars() {
        let ch_width = unicode_width_of(ch);
        if used + ch_width > width {
            break;
        }
        used += ch_width;
        out.push(ch);
    }
    out
}

/// `visibleWidth` from pi-tui (ANSI sequences count as zero width).
pub fn visible_width(value: &str) -> usize {
    let mut width = 0usize;
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            // Skip CSI/OSC sequences.
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        width += unicode_width_of(ch);
    }
    width
}

fn unicode_width_of(ch: char) -> usize {
    match ch {
        '\u{200b}' | '\u{feff}' => 0,
        _ => {
            if is_wide(ch) {
                2
            } else {
                1
            }
        }
    }
}

fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF |
        0x4E00..=0x9FFF | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF |
        0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F |
        0x1F900..=0x1F9FF | 0x20000..=0x2FFFD | 0x30000..=0x3FFFD)
}

/// `wrapTextWithAnsi(line, width)` reduced to plain text wrapping.
pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for word in text.split(' ') {
        let word_width = visible_width(word);
        if current_width > 0 && current_width + 1 + word_width > width {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        if current_width > 0 {
            current.push(' ');
            current_width += 1;
        }
        while word_width > width && current.is_empty() {
            // Long word: hard-split at the width boundary.
            let head = truncate_to_width(word, width);
            lines.push(head);
            break;
        }
        current.push_str(word);
        current_width += word_width;
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// `padLine(line, width)`.
pub fn pad_line(line: &str, width: usize) -> String {
    let padding = width.saturating_sub(visible_width(line));
    format!("{line}{}", " ".repeat(padding))
}

/// `formatTableCell(value, width)`: truncate, then right-pad to the column.
pub fn format_table_cell(value: &str, width: usize) -> String {
    let truncated = truncate_to_width(value, width);
    let padding = width.saturating_sub(visible_width(&truncated));
    format!("{truncated}{}", " ".repeat(padding))
}

pub fn pad_cell_start(value: &str, width: usize) -> String {
    let padding = width.saturating_sub(visible_width(value));
    format!("{}{value}", " ".repeat(padding))
}

/// `theme.fg(color, text)` / `theme.bold` / `theme.italic` / `theme.bg`.
/// The interactive theme slice owns the real ANSI palette; the agents view only
/// needs the composition points, so colour is applied through a small trait.
pub trait AgentsViewTheme: Send + Sync {
    fn fg(&self, color: &str, text: &str) -> String;
    fn bg(&self, color: &str, text: &str) -> String;
    fn bold(&self, text: &str) -> String;
    fn italic(&self, text: &str) -> String;
    fn selection_background_color(&self) -> Box<dyn Fn(&str) -> String + Send + Sync>;
}

/// Passthrough theme used when the interactive slice has not landed: it returns
/// the text unchanged so row geometry and ordering stay testable.
#[derive(Debug, Default)]
pub struct PlainAgentsViewTheme;

impl AgentsViewTheme for PlainAgentsViewTheme {
    fn fg(&self, _color: &str, text: &str) -> String {
        text.to_string()
    }
    fn bg(&self, _color: &str, text: &str) -> String {
        text.to_string()
    }
    fn bold(&self, text: &str) -> String {
        text.to_string()
    }
    fn italic(&self, text: &str) -> String {
        text.to_string()
    }
    fn selection_background_color(&self) -> Box<dyn Fn(&str) -> String + Send + Sync> {
        Box::new(|text: &str| text.to_string())
    }
}

/// `keyText(keybinding)` from modes/interactive/components/keybinding-hints.ts.
pub fn key_text(keybinding: &str) -> String {
    keybinding
        .split('/')
        .map(|binding| {
            binding
                .split('+')
                .map(|part| {
                    let normalized = if part == "escape" { "esc" } else { part };
                    match normalized {
                        "up" => "↑".to_string(),
                        "down" => "↓".to_string(),
                        "left" => "←".to_string(),
                        "right" => "→".to_string(),
                        "alt" => {
                            if cfg!(target_os = "macos") {
                                "Option".to_string()
                            } else {
                                "Alt".to_string()
                            }
                        }
                        other => {
                            let mut chars = other.chars();
                            match chars.next() {
                                None => String::new(),
                                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                            }
                        }
                    }
                })
                .collect::<Vec<String>>()
                .join("+")
        })
        .collect::<Vec<String>>()
        .join("/")
}

/// Default keybindings used by the view (`KeybindingsManager.create()`).
pub fn default_keybinding(keybinding: &str) -> &'static str {
    match keybinding {
        "app.clear" => "ctrl+c",
        "app.shortcuts" => "ctrl+o",
        "app.agents.rename" => "ctrl+r",
        "app.agents.delete" => "ctrl+x",
        "app.agents.reply" => "ctrl+p",
        "app.agents.new" => "ctrl+n",
        "app.agents.program" => "ctrl+g",
        "app.agents.inactiveCollapse" => "ctrl+h",
        "app.agents.expand" => "ctrl+e",
        "app.agents.open" => "ctrl+a",
        "app.message.followUp" => "alt+enter",
        "tui.select.up" => "up",
        "tui.select.down" => "down",
        "tui.select.pageUp" => "pageup",
        "tui.select.pageDown" => "pagedown",
        "tui.select.confirm" => "enter",
        "tui.select.cancel" => "escape",
        _ => "",
    }
}

pub fn matches_key(data: &str, keybinding: &str) -> bool {
    default_keybinding(keybinding) == data
}

#[derive(Clone, Debug, PartialEq)]
pub enum DisplayItem {
    Spacer,
    Heading(AgentsViewSection),
    RunningSubagents(usize),
    Row(usize),
}


#[derive(Clone, Debug, PartialEq)]
pub struct AgentsViewUsageLayout {
    pub legend: String,
    pub details: HashMap<String, String>,
    pub name_width: usize,
    pub model_width: usize,
    pub activity_width: usize,
}

pub fn build_compact_agents_view_layout(
    rows: &[AgentsViewRow],
    width: usize,
    theme: &dyn AgentsViewTheme,
) -> AgentsViewUsageLayout {
    let _ = theme;
    let sessions: Vec<&AgentsViewRow> = rows
        .iter()
        .filter(|row| row.kind == AgentsViewRowKind::Agent || row.kind == AgentsViewRowKind::Subagent)
        .collect();
    let entries: Vec<(String, String, String)> = sessions
        .iter()
        .map(|row| {
            (
                row.identity.clone(),
                format!("${:.2}", row.recursive_cost),
                format_session_duration(&row.summary),
            )
        })
        .collect();
    let cost_width = entries.iter().fold(4usize, |size, entry| size.max(visible_width(&entry.1)));
    let age_width = entries.iter().fold(3usize, |size, entry| size.max(visible_width(&entry.2)));
    let details_width = cost_width + 2 + age_width;
    let available = width.saturating_sub(details_width + 4);
    let desired_model_width = sessions
        .iter()
        .fold(12usize, |size, row| size.max(visible_width(&format_session_model(&row.summary))));
    let model_width = desired_model_width.min(32).min(available.saturating_sub(12));
    let name_width = 28usize.min(available.saturating_sub(model_width));
    let activity_width = available.saturating_sub(model_width + name_width + 2);
    let detail_line = |cost: &str, age: &str| format!("{}  {}", pad_cell_start(cost, cost_width), pad_cell_start(age, age_width));
    let mut headings = vec![
        format_table_cell("Session", name_width),
        format_table_cell("Model", model_width),
    ];
    if activity_width > 0 {
        headings.push(format_table_cell("Activity", activity_width));
    }
    headings.push(detail_line("Cost", "Age"));
    AgentsViewUsageLayout {
        legend: format_table_cell(&headings.join("  "), width),
        details: entries
            .into_iter()
            .map(|(identity, cost, age)| (identity, detail_line(&cost, &age)))
            .collect(),
        name_width,
        model_width,
        activity_width,
    }
}

pub fn format_session_model(summary: &SessionSummary) -> String {
    summary.model.as_ref().map(|model| model.id.clone()).unwrap_or_else(|| "-".to_string())
}

pub fn format_session_duration(summary: &SessionSummary) -> String {
    let value = if summary.active_session_id.is_some() {
        summary.created.as_deref().or(summary.modified.as_deref())
    } else {
        summary.modified.as_deref().or(summary.created.as_deref())
    };
    format_agents_view_relative_time(value, super::agents_view_state::now_ms())
}

pub fn format_agents_view_relative_time(value: Option<&str>, now: i64) -> String {
    let Some(timestamp) = parse_session_timestamp(value) else {
        return String::new();
    };
    let seconds = ((now - timestamp) as f64 / 1000.0).floor().max(0.0) as i64;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = (seconds / 60) as i64;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    format!("{}d", hours / 24)
}

pub fn parse_session_timestamp(value: Option<&str>) -> Option<i64> {
    let value = value?;
    if value.is_empty() {
        return None;
    }
    parse_js_timestamp(value)
}

// Explicit session names read bold so they stand out from fallback titles
// (first prompt, cwd, ids); the "(no messages)" placeholder reads italic.
pub fn style_row_title(row: &AgentsViewRow, theme: &dyn AgentsViewTheme) -> String {
    if row
        .summary
        .session_name
        .as_deref()
        .map(|name| !collapse_whitespace(name).is_empty())
        .unwrap_or(false)
    {
        return theme.bold(&row.title);
    }
    if row.title == "(no messages)" {
        return theme.italic(&row.title);
    }
    row.title.clone()
}

/// `isInactiveExpanded(state)`.
pub fn is_inactive_expanded(state: &AgentsViewPersistentState) -> bool {
    // Older clients persisted false even when the user never chose to hide saved chats.
    state.inactive_visibility_explicit != Some(true) || state.inactive_expanded != Some(false)
}

/// `compactSessionRows(rows, showInactive)`.
pub fn compact_session_rows(rows: &[AgentsViewRow], show_inactive: bool) -> Vec<AgentsViewRow> {
    let mut visible = true;
    rows.iter()
        .filter(|row| {
            if row.depth == 0 {
                visible = show_inactive || row.section != AgentsViewSection::Inactive;
            }
            visible && row.kind != AgentsViewRowKind::SubagentSummary
        })
        .cloned()
        .collect()
}

// Nested rows (subagent summaries and expanded subagents) always render in
// their top-level agent's section block, regardless of their own section.
pub fn get_display_rows_for_section(rows: &[AgentsViewRow], section: AgentsViewSection) -> Vec<AgentsViewRow> {
    let mut result: Vec<AgentsViewRow> = Vec::new();
    let mut include = false;
    for row in rows {
        if row.depth == 0 {
            include = row.section == section;
        }
        if include {
            result.push(row.clone());
        }
    }
    result
}

pub fn count_rows_by_section(rows: &[AgentsViewRow]) -> HashMap<AgentsViewSection, i64> {
    let agents: Vec<&AgentsViewRow> = rows.iter().filter(|row| row.kind == AgentsViewRowKind::Agent).collect();
    let mut counts = HashMap::new();
    for section in [AgentsViewSection::Running, AgentsViewSection::Idle, AgentsViewSection::Inactive] {
        counts.insert(
            section,
            agents.iter().filter(|row| row.section == section).count() as i64,
        );
    }
    counts
}

pub fn get_selected_row_identity(row: Option<&AgentsViewRow>) -> Option<String> {
    row.map(|row| row.identity.clone())
}

pub fn row_has_spawn_code(row: &AgentsViewRow) -> bool {
    row.summary.spawn_code.as_deref().map(|code| !code.trim().is_empty()).unwrap_or(false)
}

// Destructive actions gate on live work anywhere in the subtree, never on the display section.
pub fn has_live_work(row: &AgentsViewRow) -> bool {
    row.section == AgentsViewSection::Running
        || row.running_subagent_count > 0
        || row.summary.has_running_rlm_children == Some(true)
}

/// `formatAgentsViewRelativeTime` for the reply header.
pub fn format_agents_view_relative_time_now(value: Option<&str>) -> String {
    format_agents_view_relative_time(value, super::agents_view_state::now_ms())
}

/// `new Date().toISOString()` used by `logClientError`.
pub fn now_iso_string() -> String {
    to_iso_string(&chrono::Utc::now())
}


/// `listDaemonHeartbeats` (modes/daemon/heartbeat-catalog.ts).
pub async fn list_daemon_heartbeats(
    client: &DaemonTransportClient,
    active_session_id: Option<&str>,
) -> Result<Vec<AgentConnectionHeartbeat>, String> {
    if client.hello().is_none() {
        let _ = client.wait_for_hello(3000).await;
    }
    if !client.supports_server_capability("heartbeat_catalog") {
        return Ok(Vec::new());
    }
    let mut command = serde_json::json!({ "type": "heartbeats_list" });
    if let Some(active_session_id) = active_session_id {
        command["activeSessionId"] = Value::String(active_session_id.to_string());
    }
    match client.request(command, 30000).await {
        Ok(response) => {
            if !response.success {
                return Err(deserialize_daemon_error(&response));
            }
            let heartbeats = response
                .data
                .as_ref()
                .and_then(|data| data.get("heartbeats"))
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| serde_json::from_value(value.clone()).ok())
                        .collect()
                })
                .unwrap_or_default();
            Ok(heartbeats)
        }
        Err(error) => {
            if is_unknown_daemon_command_error(&error, "heartbeats_list") {
                return Ok(Vec::new());
            }
            Err(error)
        }
    }
}

/// `deserializeDaemonError`: the specialized error classes are declared by the
/// daemon slice, so this returns the same message the reference surfaces.
pub fn deserialize_daemon_error(response: &DaemonResponse) -> String {
    response.error.clone().unwrap_or_else(|| "unknown error".to_string())
}

pub const SAVED_SESSION_LIST_TIMEOUT_MS: u64 = 30000;

/// `DaemonSavedSessionCatalogContext`.
#[derive(Clone, Debug, PartialEq)]
pub enum DaemonSavedSessionCatalogContext {
    ActiveSessionId(String),
    Cwd { cwd: String, session_dir: Option<String> },
}

/// `deserializeSavedSessionInfo` (modes/daemon/saved-session-info.ts).
pub fn deserialize_saved_session_info(value: &Value) -> Result<AgentConnectionSavedSessionInfo, String> {
    let mut session: AgentConnectionSavedSessionInfo =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    // The wire carries ISO strings; `Date` conversion is already done by serde.
    let _ = &mut session;
    Ok(session)
}

/// `listDaemonSavedSessions` (modes/daemon/saved-session-catalog.ts).
pub async fn list_daemon_saved_sessions(
    client: &DaemonTransportClient,
    context: &DaemonSavedSessionCatalogContext,
    scope: &str,
    on_session: Option<&(dyn Fn() + Send + Sync)>,
    on_progress: Option<&(dyn Fn(i64, i64) + Send + Sync)>,
) -> Result<Vec<AgentConnectionSavedSessionInfo>, String> {
    let command = match context {
        DaemonSavedSessionCatalogContext::ActiveSessionId(active_session_id) => serde_json::json!({
            "type": "list_saved_sessions",
            "activeSessionId": active_session_id,
            "scope": scope,
        }),
        DaemonSavedSessionCatalogContext::Cwd { cwd, session_dir } => {
            let mut command = serde_json::json!({
                "type": "list_saved_sessions",
                "cwd": cwd,
                "scope": scope,
            });
            if let Some(session_dir) = session_dir {
                command["sessionDir"] = Value::String(session_dir.clone());
            }
            command
        }
    };
    let response = client
        .request_with_options(
            command,
            SAVED_SESSION_LIST_TIMEOUT_MS,
            DaemonClientRequestOptions::new()
                .recoverable(true)
                .with_progress(Box::new(move |progress: &Value| {
                if progress.get("type").and_then(|value| value.as_str()) == Some("session_list_progress") {
                    if let Some(callback) = on_progress {
                        callback(
                            progress.get("loaded").and_then(|value| value.as_i64()).unwrap_or(0),
                            progress.get("total").and_then(|value| value.as_i64()).unwrap_or(0),
                        );
                    }
                } else if let Some(callback) = on_session {
                    callback();
                }
            })),
        )
        .await?;
    if !response.success {
        return Err(deserialize_daemon_error(&response));
    }
    let sessions = response
        .data
        .as_ref()
        .and_then(|data| data.get("sessions"))
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| deserialize_saved_session_info(value).ok())
                .collect()
        })
        .unwrap_or_default();
    Ok(sessions)
}

/// `renameDaemonSavedSession`.
pub async fn rename_daemon_saved_session(
    client: &DaemonTransportClient,
    context: &DaemonSavedSessionCatalogContext,
    session_path: &str,
    name: &str,
) -> Result<(), String> {
    let command = match context {
        DaemonSavedSessionCatalogContext::ActiveSessionId(active_session_id) => serde_json::json!({
            "type": "rename_saved_session",
            "activeSessionId": active_session_id,
            "sessionPath": session_path,
            "name": name,
        }),
        DaemonSavedSessionCatalogContext::Cwd { .. } => serde_json::json!({
            "type": "rename_saved_session",
            "sessionPath": session_path,
            "name": name,
        }),
    };
    let response = client.request(command, 30000).await?;
    if !response.success {
        return Err(deserialize_daemon_error(&response));
    }
    Ok(())
}

/// `DeleteSessionFileResult` (core/session-file-actions.ts).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeleteSessionFileResult {
    pub ok: bool,
    pub method: Option<String>,
    pub error: Option<String>,
}

/// `deleteDaemonSavedSession`.
pub async fn delete_daemon_saved_session(
    client: &DaemonTransportClient,
    context: &DaemonSavedSessionCatalogContext,
    session_path: &str,
) -> Result<DeleteSessionFileResult, String> {
    let command = match context {
        DaemonSavedSessionCatalogContext::ActiveSessionId(active_session_id) => serde_json::json!({
            "type": "delete_saved_session",
            "activeSessionId": active_session_id,
            "sessionPath": session_path,
        }),
        DaemonSavedSessionCatalogContext::Cwd { .. } => serde_json::json!({
            "type": "delete_saved_session",
            "sessionPath": session_path,
        }),
    };
    let response = client.request(command, 30000).await?;
    if !response.success {
        return Err(deserialize_daemon_error(&response));
    }
    let data = response.data.clone().unwrap_or(Value::Null);
    Ok(DeleteSessionFileResult {
        ok: data.get("ok").and_then(|value| value.as_bool()).unwrap_or(false),
        method: data.get("method").and_then(|value| value.as_str()).map(|value| value.to_string()),
        error: data.get("error").and_then(|value| value.as_str()).map(|value| value.to_string()),
    })
}

/// `connectAgentsViewDaemonClient(socketPath)`.
pub async fn connect_agents_view_daemon_client(
    socket_path: &str,
    transport: Arc<dyn super::roster_store::DaemonTransport>,
) -> Result<DaemonTransportClient, String> {
    let client = DaemonTransportClient::new(transport);
    match client.connect(3000).await {
        Ok(()) => Ok(client),
        Err(error) => {
            client.close();
            Err(error)
        }
    }
}


/// `DaemonAgentConnection` surface used by the agents view.
/// TODO(port): modes/agent-connection/daemon-agent-connection.ts owns the real
/// type; this trait keeps the attach/prompt/dispose sequence in place.
pub trait DaemonAgentConnectionHandle: Send + Sync {
    fn prompt(&self, message: &str, streaming_behavior: Option<&str>) -> TransportFuture<Result<(), String>>;
    fn dispose(&self) -> TransportFuture<Result<(), String>>;
}

pub type TransportFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

/// `InteractiveMode` surface used by the run loop.
/// TODO(port): modes/interactive/interactive-mode.ts owns the real type.
pub trait InteractiveModeHandle: Send {
    fn run(&mut self) -> TransportFuture<Result<InteractiveRunResult, String>>;
    fn teardown_session_ui(&mut self, preserve_alt_screen: bool) -> TransportFuture<Result<(), String>>;
}

/// The subset of `InteractiveModeRunResult` the run loop reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InteractiveRunResult {
    pub kind: String,
    pub source: SessionSummary,
}

pub type InteractiveModeFactory = Box<
    dyn Fn(InteractiveModeOptions) -> Box<dyn InteractiveModeHandle> + Send + Sync,
>;

#[derive(Clone)]
pub struct InteractiveModeOptions {
    pub daemon_socket_path: Option<String>,
    pub ui_services: Arc<dyn AgentsViewUiServices>,
    pub prompt_stash_session_id: String,
    pub bind_local_session_extensions: bool,
    pub migrated_providers: Option<Vec<String>>,
    pub model_fallback_message: Option<String>,
    pub startup_notice: Option<String>,
    pub verbose: Option<bool>,
    pub return_to_agents_view: bool,
    pub force_fullscreen: bool,
    /// The agents view renders the global notices itself, so suppress them in-session.
    pub agents_view_owns_startup_notices: bool,
    pub session_depth: Option<i64>,
    pub session_has_children: Option<bool>,
}

pub trait DaemonAgentConnectionFactory: Send + Sync {
    /// `DaemonAgentConnection.attach(client, activeSessionId, options)`.
    fn attach(
        &self,
        client: DaemonTransportClient,
        active_session_id: &str,
        options: AttachOptions,
    ) -> TransportFuture<Result<Arc<dyn DaemonAgentConnectionHandle>, String>>;
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttachOptions {
    pub close_client_on_dispose: Option<bool>,
    pub supports_extension_ui: Option<bool>,
    pub reconnect_timeout_ms: Option<u64>,
    pub telemetry_disabled: Option<bool>,
}

/// `openAgentsViewSession(options, summary)`.
pub async fn open_agents_view_session(
    options: &AgentsViewModeOptions,
    summary: &SessionSummary,
    transport: Arc<dyn super::roster_store::DaemonTransport>,
    factory: &dyn DaemonAgentConnectionFactory,
) -> Result<(Arc<dyn DaemonAgentConnectionHandle>, SessionSummary, Option<String>), String> {
    let socket_path = options
        .socket_path
        .clone()
        .ok_or_else(|| "Agents view daemon socket is not configured".to_string())?;
    let mut client = connect_agents_view_daemon_client(&socket_path, transport.clone()).await?;
    if let Some(active_session_id) = summary.active_session_id.clone() {
        let attached = factory
            .attach(
                client.clone(),
                &active_session_id,
                AttachOptions {
                    close_client_on_dispose: Some(true),
                    supports_extension_ui: None,
                    reconnect_timeout_ms: options.reconnect_timeout_ms,
                    telemetry_disabled: options.config.telemetry_disabled,
                },
            )
            .await;
        match attached {
            Ok(connection) => return Ok((connection, summary.clone(), None)),
            Err(error) => {
                client.close();
                // Recovering takes the saved-session path too; its create/open route
                // retries the recovery.
                if summary.session_file.is_none()
                    || !(is_unknown_active_session_error(&error)
                        || error.starts_with("Session is recovering"))
                {
                    return Err(error);
                }
                client = connect_agents_view_daemon_client(&socket_path, transport.clone()).await?;
            }
        }
    }

    if summary.session_file.is_none() {
        client.close();
        return Err("Cannot open agent without an active runtime or saved session file".to_string());
    }

    match resume_saved_agents_view_session(&client, &options.config, summary).await {
        Ok((resumed, active_session_id, cwd_fallback_notice)) => {
            let attached = factory
                .attach(
                    client.clone(),
                    &active_session_id,
                    AttachOptions {
                        close_client_on_dispose: Some(true),
                        supports_extension_ui: None,
                        reconnect_timeout_ms: options.reconnect_timeout_ms,
                        telemetry_disabled: options.config.telemetry_disabled,
                    },
                )
                .await;
            match attached {
                Ok(connection) => Ok((connection, resumed, cwd_fallback_notice)),
                Err(error) => {
                    client.close();
                    Err(error)
                }
            }
        }
        Err(error) => {
            client.close();
            Err(error)
        }
    }
}

/// Resume a saved session file into the daemon and return the live summary.
/// The daemon's create-with-sessionPath is idempotent for this client: an
/// already-resident session is reused instead of resumed twice.
pub async fn resume_saved_agents_view_session(
    client: &DaemonTransportClient,
    config: &AgentsViewRuntimeConfig,
    summary: &SessionSummary,
) -> Result<(SessionSummary, String, Option<String>), String> {
    let Some(session_file) = summary.session_file.clone() else {
        return Err("Cannot resume a session without a saved session file".to_string());
    };
    let (override_cwd, notice) = resolve_agents_view_open_cwd(summary, config.cwd.as_deref());
    let resume_config = create_agents_view_resume_config(config, override_cwd.as_deref());
    let mut command = serde_json::json!({
        "type": "create",
        "config": {
            "cwd": resume_config.cwd,
            "sessionDir": resume_config.session_dir,
            "telemetryDisabled": resume_config.telemetry_disabled,
        },
        "sessionPath": session_file,
    });
    if resume_config.cwd.is_none() {
        command["config"].as_object_mut().unwrap().remove("cwd");
    }
    if resume_config.session_dir.is_none() {
        command["config"].as_object_mut().unwrap().remove("sessionDir");
    }
    if resume_config.telemetry_disabled.is_none() {
        command["config"].as_object_mut().unwrap().remove("telemetryDisabled");
    }
    let response = client.request(command, 30000).await?;
    let data = require_daemon_data(&response)?;
    let created_summary = expect_session_summary(&data)?;
    let active_session_id = get_required_active_session_id(&created_summary)?;
    Ok((created_summary, active_session_id, notice))
}

pub fn get_required_active_session_id(summary: &SessionSummary) -> Result<String, String> {
    summary
        .active_session_id
        .clone()
        .ok_or_else(|| "Daemon returned a session without an active session id".to_string())
}


/// `runAgentsViewMode(options)`.
pub async fn run_agents_view_mode(
    options: AgentsViewModeOptions,
    terminal: Arc<dyn AgentsViewTerminal>,
    editor: Box<dyn AgentsViewEditor>,
    theme: Arc<dyn AgentsViewTheme>,
    transport: Arc<dyn super::roster_store::DaemonTransport>,
    factory: &dyn DaemonAgentConnectionFactory,
    interactive_factory: InteractiveModeFactory,
    recover_daemon: Option<Arc<dyn Fn() -> TransportFuture<Result<(), String>> + Send + Sync>>,
    prompt_stash_store: Option<Arc<()>>,
) -> Result<(), String> {
    let persistent_state =
        create_initial_agents_view_persistent_state(options.initial_scope_key.as_ref(), options.initial_session.as_ref());
    let mut runner = AgentsViewRunner {
        options,
        persistent_state,
        terminal,
        editor,
        theme,
        transport,
        factory,
        interactive_factory,
        recover_daemon,
        prompt_stash_store,
        roster_store: None,
        roster_client: None,
    };
    runner.run_loop().await
}

struct AgentsViewRunner<'a> {
    options: AgentsViewModeOptions,
    persistent_state: AgentsViewPersistentState,
    terminal: Arc<dyn AgentsViewTerminal>,
    editor: Box<dyn AgentsViewEditor>,
    theme: Arc<dyn AgentsViewTheme>,
    transport: Arc<dyn super::roster_store::DaemonTransport>,
    factory: &'a dyn DaemonAgentConnectionFactory,
    interactive_factory: InteractiveModeFactory,
    recover_daemon: Option<Arc<dyn Fn() -> TransportFuture<Result<(), String>> + Send + Sync>>,
    prompt_stash_store: Option<Arc<()>>,
    roster_store: Option<AgentsViewRosterStore>,
    roster_client: Option<DaemonTransportClient>,
}

impl AgentsViewRunner<'_> {
    async fn run_loop(&mut self) -> Result<(), String> {
        let result = self.run_loop_inner().await;
        // Close first: the supervisor drops the subscription with the socket.
        if let Some(client) = self.roster_client.take() {
            client.close();
        }
        if let Some(store) = self.roster_store.take() {
            store.dispose().await;
        }
        result
    }

    async fn run_loop_inner(&mut self) -> Result<(), String> {
        loop {
            let mut view = AgentsViewMode::new(
                self.options.clone(),
                &mut self.persistent_state,
                self.terminal.clone(),
                &mut *self.editor,
                self.theme.clone(),
                self.transport.clone(),
                self.factory,
                &mut self.interactive_factory,
                self.recover_daemon.clone(),
            );
            let view_result = view.run().await?;
            if view_result == AgentsViewRunResult::Exit {
                return Ok(());
            }
            let result = match view_result {
                AgentsViewRunResult::ScopeBack {
                    ref selection,
                    ref expanded_ancestor_session_ids,
                    ..
                } => {
                    let frames = self.persistent_state.scope_frames.clone().unwrap_or_default();
                    self.persistent_state.scope_frames =
                        Some(transition_agents_view_scope(&frames, &super::agents_view_state::AgentsViewScopeAction::Back));
                    self.persistent_state.scope_root_summary = None;
                    self.persistent_state.selected_row_identity = Some(get_agents_view_summary_identity(selection));
                    self.persistent_state.selected_session_key = Some(get_agents_view_selection_key(selection));
                    self.persistent_state.pending_expanded_ancestor_session_ids =
                        Some(expanded_ancestor_session_ids.clone());
                    self.persistent_state.query = Some(String::new());
                    match create_scope_back_return_chat_open_result(&view_result) {
                        Some(result) => result,
                        None => continue,
                    }
                }
                other => other,
            };

            let (summary, selection, expanded_ancestors, status_message) = match &result {
                AgentsViewRunResult::Open {
                    summary,
                    selection,
                    expanded_ancestor_session_ids,
                    status_message,
                    ..
                } => (
                    summary.clone(),
                    selection.clone().unwrap_or_else(|| summary.clone()),
                    expanded_ancestor_session_ids.clone(),
                    status_message.clone(),
                ),
                _ => continue,
            };
            self.persistent_state.selected_row_identity = Some(get_agents_view_summary_identity(&selection));
            self.persistent_state.selected_session_key = Some(get_agents_view_selection_key(&selection));
            self.persistent_state.pending_expanded_ancestor_session_ids = expanded_ancestors;
            if let Some(status_message) = status_message {
                self.persistent_state.status_message = Some(status_message);
            }

            let opened = open_agents_view_session(&self.options, &summary, self.transport.clone(), self.factory).await;
            match opened {
                Ok((connection, opened_summary, cwd_fallback_notice)) => {
                    self.persistent_state.back_session = Some(opened_summary.clone());
                    if let Some(notice) = &cwd_fallback_notice {
                        self.persistent_state.status_message = combine_agents_view_startup_notices(&[
                            result_status_message(&result).as_deref(),
                            Some(notice.as_str()),
                        ]);
                    }
                    let mut interactive = (self.interactive_factory)(InteractiveModeOptions {
                        daemon_socket_path: self.options.socket_path.clone(),
                        ui_services: self.options.ui_services.clone(),
                        prompt_stash_session_id: opened_summary.session_id.clone(),
                        bind_local_session_extensions: false,
                        migrated_providers: self.options.migrated_providers.clone(),
                        model_fallback_message: resolve_attach_model_fallback_message(
                            &opened_summary,
                            self.options.model_fallback_message.as_deref(),
                        ),
                        startup_notice: combine_agents_view_startup_notices(&[
                            result_status_message(&result).as_deref(),
                            cwd_fallback_notice.as_deref(),
                        ]),
                        verbose: self.options.verbose,
                        return_to_agents_view: true,
                        force_fullscreen: true,
                        agents_view_owns_startup_notices: true,
                        session_depth: opened_summary.rlm_depth,
                        session_has_children: result_has_children(&result),
                    });
                    match interactive.run().await {
                        Ok(interactive_result) => {
                            let mut returned_session = opened_summary.clone();
                            let source = interactive_result.source;
                            returned_session.active_session_id = source.active_session_id.clone();
                            if let Some(active) = &source.active_session_id {
                                returned_session.id = active.clone();
                            }
                            returned_session.session_id = source.session_id.clone();
                            returned_session.modified = source.modified.clone();
                            returned_session.created = source.created.clone();
                            returned_session.model = source.model.clone();
                            returned_session.cwd = source.cwd.clone();
                            returned_session.is_streaming = source.is_streaming;
                            returned_session.is_compacting = source.is_compacting;
                            returned_session.is_bash_running = source.is_bash_running;
                            returned_session.is_running_tools = source.is_running_tools;
                            returned_session.message_count = source.message_count;
                            returned_session.session_file = source.session_file.clone();
                            returned_session.rlm_depth = source.rlm_depth;
                            // Preserve an unattachable child's selection while its parent chat was open.
                            if selection.session_id == summary.session_id {
                                self.persistent_state.selected_row_identity =
                                    Some(get_agents_view_summary_identity(&returned_session));
                                self.persistent_state.selected_session_key =
                                    Some(get_agents_view_selection_key(&returned_session));
                            }
                            if interactive_result.kind == "scoped_agents_view" {
                                let next_scope = AgentsViewScopeKey {
                                    session_id: source.session_id.clone(),
                                    active_session_id: source.active_session_id.clone(),
                                };
                                let frames = self.persistent_state.scope_frames.clone().unwrap_or_default();
                                self.persistent_state.scope_frames = Some(transition_agents_view_scope(
                                    &frames,
                                    &super::agents_view_state::AgentsViewScopeAction::Push {
                                        scope: next_scope,
                                        return_chat: Some(returned_session.clone()),
                                    },
                                ));
                                let cached = self
                                    .persistent_state
                                    .last_successful_live_summaries
                                    .clone()
                                    .unwrap_or_default();
                                let cached_index =
                                    cached.iter().position(|entry| entry.session_id == returned_session.session_id);
                                self.persistent_state.last_successful_live_summaries = Some(match cached_index {
                                    None => {
                                        let mut next = cached.clone();
                                        next.push(returned_session.clone());
                                        next
                                    }
                                    Some(index) => cached
                                        .iter()
                                        .enumerate()
                                        .map(|(position, entry)| {
                                            if position == index { returned_session.clone() } else { entry.clone() }
                                        })
                                        .collect(),
                                });
                                self.persistent_state.scope_root_summary = None;
                                self.persistent_state.query = Some(String::new());
                            }
                            self.persistent_state.back_session = Some(returned_session);
                        }
                        Err(error) => {
                            // The session opened fine and then threw while running; label it as a
                            // runtime crash so it isn't mixed in with true open failures.
                            log_client_error("Agent session crashed", &error);
                            self.persistent_state.status_message = Some(format_error("Agent session crashed", &error));
                            // Tear down the session TUI exactly as a normal back-navigation would
                            // (drain input, stop renderer + theme watcher) so it doesn't fight the
                            // agents-view UI for the terminal, then drop the daemon connection.
                            let _ = interactive.teardown_session_ui(true).await;
                            let _ = connection.dispose().await;
                        }
                    }
                }
                Err(error) => {
                    log_client_error("Failed to open agent", &error);
                    self.persistent_state.status_message = Some(format_error("Failed to open agent", &error));
                }
            }
        }
    }
}

fn result_status_message(result: &AgentsViewRunResult) -> Option<String> {
    match result {
        AgentsViewRunResult::Open { status_message, .. } => status_message.clone(),
        _ => None,
    }
}

fn result_has_children(result: &AgentsViewRunResult) -> Option<bool> {
    match result {
        AgentsViewRunResult::Open { has_children, .. } => *has_children,
        _ => None,
    }
}

/// `resolveAttachModelFallbackMessage` (modes/daemon/daemon-session-list.ts).
pub fn resolve_attach_model_fallback_message(
    summary: &SessionSummary,
    startup_model_fallback_message: Option<&str>,
) -> Option<String> {
    if let Some(message) = &summary.model_fallback_message {
        return Some(message.clone());
    }
    if summary.model.is_some() {
        return None;
    }
    startup_model_fallback_message.map(|message| message.to_string())
}

/// `logClientError`: the TUI owns stdout/stderr, so a log file is the only safe sink.
pub fn log_client_error(prefix: &str, error: &str) {
    let line = format!("[{}] {prefix}: {error}", now_iso_string());
    append_rotating_log(&client_error_log_path(), &line);
}

pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// `appendRotatingLog` (config.ts). Private plumbing for `logClientError`.
pub fn append_rotating_log(log_path: &str, message: &str) {
    let path = std::path::Path::new(log_path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(metadata) = std::fs::metadata(path) {
        if metadata.len() > MAX_LOG_BYTES {
            // Drop any prior .old first: rename fails on Windows if it exists.
            let old = format!("{log_path}.old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(path, &old);
        }
    }
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}

/// `getClientErrorLogPath()` = `<agentDir>/logs/client-errors.log`.
pub fn client_error_log_path() -> String {
    format!("{}{}logs{}client-errors.log", agent_dir(), std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR)
}

/// `getAgentDir()`: `$PI_CONFIG_DIR_AGENT_DIR`-style override, else `~/.prime`.
pub const ENV_AGENT_DIR: &str = "PRIME_AGENT_DIR";

pub fn agent_dir() -> String {
    if let Ok(value) = std::env::var(ENV_AGENT_DIR) {
        if !value.is_empty() {
            return value;
        }
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    format!("{home}{}.prime", std::path::MAIN_SEPARATOR)
}


/// `class AgentsViewMode implements Component, Focusable`.
pub struct AgentsViewMode<'a> {
    options: AgentsViewModeOptions,
    persistent_state: AgentsViewPersistentState,
    terminal: Arc<dyn AgentsViewTerminal>,
    editor: Box<dyn AgentsViewEditor>,
    theme: Arc<dyn AgentsViewTheme>,
    transport: Arc<dyn super::roster_store::DaemonTransport>,
    factory: &'a dyn DaemonAgentConnectionFactory,
    interactive_factory: &'a mut InteractiveModeFactory,
    recover_daemon: Option<Arc<dyn Fn() -> TransportFuture<Result<(), String>> + Send + Sync>>,
    roster_store: Arc<AgentsViewRosterStore>,
    roster_listener: Option<usize>,
    client: Option<DaemonTransportClient>,
    unsubscribe_client_close: Option<Box<dyn Fn() + Send + Sync>>,
    unsubscribe_client_message: Option<Box<dyn Fn() + Send + Sync>>,
    /// Last close reason reported by the transport (`getDaemonSocketCloseReason`).
    last_close_reason: Arc<Mutex<Option<String>>>,
    reconnect_last_error: Option<String>,
    reconnect_started: bool,
    reconnect_timed_out: bool,
    daemon_shutdown_received: bool,
    resolve_run: Option<tokio::sync::oneshot::Sender<AgentsViewRunResult>>,
    ctrl_c_exit_hint_expires_at: i64,
    delete_confirm_expires_at: i64,
    working_icon_frame: i64,
    rows: Vec<AgentsViewRow>,
    all_rows: Vec<AgentsViewRow>,
    show_actions: bool,
    last_listed_summaries: Vec<SessionSummary>,
    last_visible_summaries: Vec<SessionSummary>,
    saved_sessions: Vec<AgentConnectionSavedSessionInfo>,
    last_successful_saved_sessions: Vec<AgentConnectionSavedSessionInfo>,
    heartbeats: Vec<AgentConnectionHeartbeat>,
    unified_records: Vec<UnifiedSessionRecord>,
    unified_index: UnifiedSessionIndex,
    scoped_records: Vec<UnifiedSessionRecord>,
    scope_key: Option<AgentsViewScopeKey>,
    scope_root_summary: Option<SessionSummary>,
    saved_catalog_ready: bool,
    saved_catalog_generation: i64,
    heartbeat_catalog_generation: i64,
    saved_catalog_refresh_pending: bool,
    saved_catalog_progress: i64,
    expanded_subagent_parents: HashSet<String>,
    /// Agent row identities whose full spawn program is currently shown.
    program_shown_parents: HashSet<String>,
    selected_index: usize,
    selected_row_identity: Option<String>,
    selected_active_session_id: Option<String>,
    selected_session_key: Option<AgentsViewSelectionKey>,
    selection_anchor_pending: bool,
    /// Armed reply composer target: a live agent or a saved session to resume on send.
    reply_target: Option<(String, SessionSummary)>,
    fd_path: Option<String>,
    creating_new_session: bool,
    reply_last_assistant_text: Option<String>,
    reply_last_assistant_text_loading: bool,
    reply_header_time: String,
    pending_delete_agent: Option<PendingDeleteAgent>,
    pending_kill_subagent: Option<PendingKillSubagent>,
    rename_target: Option<(Option<String>, Option<String>, SessionSummary)>,
    action_mode_search_query: Option<String>,
    /// Session the view was entered from; exempt from the empty-session sort demotion.
    anchor_session_id: Option<String>,
    inactive_agent_identities: HashSet<String>,
    saved_search_fetch_started: bool,
    status_message: Option<String>,
    status_message_tone: StatusTone,
    status_message_sticky: bool,
    stopped: bool,
    focused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusTone {
    Muted,
    Error,
    Warning,
}

impl StatusTone {
    fn as_str(self) -> &'static str {
        match self {
            StatusTone::Muted => "muted",
            StatusTone::Error => "error",
            StatusTone::Warning => "warning",
        }
    }
}

impl<'a> AgentsViewMode<'a> {
    pub fn new(
        options: AgentsViewModeOptions,
        persistent_state: AgentsViewPersistentState,
        terminal: Arc<dyn AgentsViewTerminal>,
        editor: Box<dyn AgentsViewEditor>,
        theme: Arc<dyn AgentsViewTheme>,
        transport: Arc<dyn super::roster_store::DaemonTransport>,
        factory: &'a dyn DaemonAgentConnectionFactory,
        interactive_factory: &'a mut InteractiveModeFactory,
        recover_daemon: Option<Arc<dyn Fn() -> TransportFuture<Result<(), String>> + Send + Sync>>,
    ) -> Self {
        let initial_frames = persistent_state.scope_frames.clone().unwrap_or_else(|| {
            create_initial_agents_view_scope_frames(
                options.initial_scope_key.as_ref(),
                persistent_state
                    .back_session
                    .as_ref()
                    .or(options.initial_session.as_ref()),
            )
        });
        let anchor_session_id = persistent_state
            .back_session
            .as_ref()
            .or(options.initial_session.as_ref())
            .map(|session| session.session_id.clone());
        let scope_key = initial_frames.last().map(|frame| frame.scope.clone());
        let mut persistent_state = persistent_state;
        persistent_state.scope_frames = Some(initial_frames);
        let expanded_subagent_parents = persistent_state.expanded_subagent_parents.take().unwrap_or_default();
        let program_shown_parents = persistent_state.program_shown_parents.take().unwrap_or_default();
        let last_listed_summaries = persistent_state.last_successful_live_summaries.clone().unwrap_or_default();
        let saved_sessions = persistent_state.saved_sessions.clone().unwrap_or_default();
        let last_successful_saved_sessions = persistent_state
            .last_successful_saved_sessions
            .clone()
            .unwrap_or_else(|| saved_sessions.clone());
        let saved_catalog_ready = persistent_state.saved_catalog_loaded == Some(true);
        let heartbeats = persistent_state.heartbeats.clone().unwrap_or_default();
        let saved_catalog_generation = persistent_state.saved_catalog_generation.unwrap_or(0);
        let mut editor = editor;
        editor.set_text(persistent_state.query.clone().unwrap_or_default().as_str());
        editor.set_placeholder(SEARCH_PROMPT_PLACEHOLDER);
        terminal.set_title("π - Agents");
        let mut mode = Self {
            selected_row_identity: persistent_state.selected_row_identity.clone(),
            selected_session_key: persistent_state.selected_session_key.clone(),
            selected_active_session_id: persistent_state
                .selected_session_key
                .as_ref()
                .and_then(|key| key.active_session_id.clone()),
            options,
            persistent_state,
            terminal,
            editor,
            theme,
            transport,
            factory,
            interactive_factory,
            recover_daemon,
            roster_store: Arc::new(AgentsViewRosterStore::new()),
            roster_listener: None,
            client: None,
            unsubscribe_client_close: None,
            unsubscribe_client_message: None,
            last_close_reason: Arc::new(Mutex::new(None)),
            reconnect_last_error: None,
            reconnect_started: false,
            reconnect_timed_out: false,
            daemon_shutdown_received: false,
            resolve_run: None,
            ctrl_c_exit_hint_expires_at: 0,
            delete_confirm_expires_at: 0,
            working_icon_frame: 0,
            rows: Vec::new(),
            all_rows: Vec::new(),
            show_actions: false,
            last_listed_summaries,
            last_visible_summaries: Vec::new(),
            saved_sessions,
            last_successful_saved_sessions,
            heartbeats,
            unified_records: Vec::new(),
            unified_index: super::agents_view_state::build_unified_session_index(&[]),
            scoped_records: Vec::new(),
            scope_key,
            scope_root_summary: None,
            saved_catalog_ready,
            saved_catalog_generation,
            heartbeat_catalog_generation: 0,
            saved_catalog_refresh_pending: false,
            saved_catalog_progress: 0,
            expanded_subagent_parents,
            program_shown_parents,
            selected_index: 0,
            selection_anchor_pending: false,
            reply_target: None,
            fd_path: None,
            creating_new_session: false,
            reply_last_assistant_text: None,
            reply_last_assistant_text_loading: false,
            reply_header_time: String::new(),
            pending_delete_agent: None,
            pending_kill_subagent: None,
            rename_target: None,
            action_mode_search_query: None,
            anchor_session_id,
            inactive_agent_identities: HashSet::new(),
            saved_search_fetch_started: false,
            status_message: None,
            status_message_tone: StatusTone::Muted,
            status_message_sticky: false,
            stopped: false,
            focused: false,
        };
        mode.scope_root_summary = mode.persistent_state.scope_root_summary.clone();
        mode
    }

    pub fn take_persistent_state(&mut self) -> AgentsViewPersistentState {
        self.persistent_state.expanded_subagent_parents = Some(self.expanded_subagent_parents.clone());
        self.persistent_state.program_shown_parents = Some(self.program_shown_parents.clone());
        self.persistent_state.saved_sessions = Some(self.saved_sessions.clone());
        self.persistent_state.last_successful_saved_sessions = Some(self.last_successful_saved_sessions.clone());
        self.persistent_state.last_successful_live_summaries = Some(self.last_listed_summaries.clone());
        self.persistent_state.heartbeats = Some(self.heartbeats.clone());
        self.persistent_state.saved_catalog_generation = Some(self.saved_catalog_generation);
        self.persistent_state.selected_row_identity = self.selected_row_identity.clone();
        self.persistent_state.selected_session_key = self.selected_session_key.clone();
        self.persistent_state.query = Some(self.editor.get_text());
        self.persistent_state.back_session = self.persistent_state.back_session.clone();
        self.persistent_state.status_message = self.status_message.clone();
        self.persistent_state.scope_root_summary = self.scope_root_summary.clone();
        std::mem::take(&mut self.persistent_state)
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// `run(): Promise<AgentsViewRunResult>`.
    ///
    /// The reference arms two `setInterval` timers (`refreshHeartbeats` every
    /// HEARTBEAT_POLL_INTERVAL_MS, the working-icon/animation tick every
    /// WORKING_ICON_INTERVAL_MS) and resolves a promise from `finish`. Rust cannot
    /// spawn a task that borrows `&mut self`, so the same cadence is driven by a
    /// `select!` loop over the completion channel and the two tickers: identical
    /// ordering, no background mutation.
    pub async fn run(&mut self) -> Result<AgentsViewRunResult, String> {
        let socket_path = self.require_socket_path()?;
        let client = match self.persistent_state.roster_client.clone() {
            Some(client) => client,
            None => {
                let client = connect_agents_view_daemon_client(&socket_path, self.transport.clone()).await?;
                self.persistent_state.roster_client = Some(client.clone());
                client
            }
        };
        if !client.is_connected() {
            let _ = client.reconnect(1000).await;
        }
        let heartbeats_changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let heartbeats_changed_listener = heartbeats_changed.clone();
        let unsubscribe_message = client.on_message(Box::new(move |message| {
            if matches!(message, super::roster_store::DaemonOutbound::HeartbeatsChanged) {
                heartbeats_changed_listener.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }));
        self.unsubscribe_client_message = Some(unsubscribe_message);
        self.client = Some(client.clone());

        if !self.roster_store.attach(client.clone()).await? {
            return Err(STALE_ROSTER_DAEMON_MESSAGE.to_string());
        }
        self.subscribe_to_client_close(client.clone());

        let (tx, mut rx) = tokio::sync::oneshot::channel();
        self.resolve_run = Some(tx);
        let listener_index = self.roster_store.on_update(Arc::new(|| {})).await;
        self.roster_listener = Some(listener_index);
        let summaries = self.roster_store.summaries().await;
        self.apply_session_list(summaries, true);
        self.arm_saved_search_fetch(false);
        self.resolve_missing_selection_anchor();
        let _ = self.refresh_heartbeats(false).await;
        self.load_startup_notices();
        self.terminal.request_render(true);

        let mut heartbeat_ticker =
            tokio::time::interval(std::time::Duration::from_millis(HEARTBEAT_POLL_INTERVAL_MS));
        heartbeat_ticker.tick().await;
        let mut animation_ticker =
            tokio::time::interval(std::time::Duration::from_millis(WORKING_ICON_INTERVAL_MS));
        animation_ticker.tick().await;

        loop {
            tokio::select! {
                result = &mut rx => {
                    return result.map_err(|_| "agents view run loop ended unexpectedly".to_string());
                }
                _ = heartbeat_ticker.tick() => {
                    let _ = heartbeats_changed.swap(false, std::sync::atomic::Ordering::SeqCst);
                    let _ = self.refresh_heartbeats(false).await;
                }
                _ = animation_ticker.tick() => {
                    self.tick_animation();
                }
            }
        }
    }

    /// The animation interval body: age labels are baked into rows at build time,
    /// so ticking them needs a rebuild.
    pub fn tick_animation(&mut self) {
        let has_running = self.rows.iter().any(|row| row.section == AgentsViewSection::Running);
        let has_stale_age = self.rows.iter().any(|row| row.summary.last_heard_from_at.is_some());
        if !has_running && !has_stale_age {
            return;
        }
        if has_stale_age {
            self.rebuild_rows();
        }
        if has_running {
            self.working_icon_frame += 1;
        }
        self.terminal.request_render(false);
    }

    fn persistent_state_client(&self) -> Option<DaemonTransportClient> {
        None
    }

    /// `finish(result)`.
    pub fn finish(&mut self, result: AgentsViewRunResult) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.saved_catalog_generation += 1;
        self.heartbeat_catalog_generation += 1;
        // The `select!` loop in run() owns the timers; finish() only stops the view.
        self.clear_ctrl_c_exit_hint(false);
        self.clear_delete_confirmation(false);
        self.set_status_message(None, false, None, false);
        if let Some(unsubscribe) = self.unsubscribe_client_close.take() {
            unsubscribe();
        }
        if let Some(unsubscribe) = self.unsubscribe_client_message.take() {
            unsubscribe();
        }
        self.client = None;
        if let Some(resolve) = self.resolve_run.take() {
            let _ = resolve.send(result);
        }
    }

    fn require_socket_path(&self) -> Result<String, String> {
        self.options
            .socket_path
            .clone()
            .ok_or_else(|| "Session view daemon socket is not configured".to_string())
    }

    fn require_client(&self) -> Result<DaemonTransportClient, String> {
        self.client
            .clone()
            .ok_or_else(|| "Agents view daemon client is not connected".to_string())
    }
}

    /// `subscribeToClientClose(client)`.
    fn subscribe_to_client_close(&mut self, client: DaemonTransportClient) {
        if let Some(unsubscribe) = self.unsubscribe_client_close.take() {
            unsubscribe();
        }
        let reason_holder = Arc::new(Mutex::new(None::<String>));
        let holder = reason_holder.clone();
        let unsubscribe = client.on_close(Box::new(move |reason| {
            *holder.lock().unwrap() = Some(reason.to_string());
        }));
        self.unsubscribe_client_close = Some(unsubscribe);
        // The reference dispatches on `getDaemonSocketCloseReason(error)`; the
        // close listener here records the reason so the run loop can read it.
        self.last_close_reason = reason_holder;
    }

    /// `handleDaemonShutdown(client, error)`.
    pub fn handle_daemon_shutdown(&mut self, message: &str) {
        if self.stopped {
            return;
        }
        self.daemon_shutdown_received = true;
        self.reconnect_timed_out = false;
        self.set_status_message(
            Some(&format!(
                "Prime Agent daemon shut down. Restart Prime Agent to reconnect. {message}"
            )),
            false,
            Some(StatusTone::Error),
            true,
        );
        self.apply_session_list(Vec::new(), false);
    }

    /// `startClientReconnect(client, error)`.
    pub fn start_client_reconnect(&mut self, error: &str) {
        if self.stopped || self.reconnect_started || self.daemon_shutdown_received {
            return;
        }
        if !self.reconnect_timed_out {
            self.set_status_message(Some("Daemon connection lost; reconnecting…"), false, Some(StatusTone::Warning), true);
        }
        self.reconnect_started = true;
        self.reconnect_last_error = Some(error.to_string());
    }

    /// `reconnectClient(client, initialError)`: bounded retry loop.
    pub async fn reconnect_client(&mut self, initial_error: &str) -> Result<(), String> {
        let deadline = super::agents_view_state::now_ms()
            + (self.options.reconnect_timeout_ms.unwrap_or(RECONNECT_TIMEOUT_MS) as i64);
        let client = self.require_client()?;
        let mut last_error = initial_error.to_string();
        while !self.stopped
            && !self.daemon_shutdown_received
            && super::agents_view_state::now_ms() < deadline
        {
            let recovered = match &self.recover_daemon {
                Some(recover) => recover().await,
                None => Ok(()),
            };
            let attempt = match recovered {
                Ok(()) => match client.reconnect(1000).await {
                    Ok(()) => self.finish_reconnect_attempt(&client).await,
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            match attempt {
                Ok(()) => return Ok(()),
                Err(error) => last_error = error,
            }
            tokio::time::sleep(std::time::Duration::from_millis(RECONNECT_RETRY_MS)).await;
        }
        if !self.stopped && !self.daemon_shutdown_received {
            self.reconnect_timed_out = true;
            self.set_status_message(
                Some(&format_error("Daemon unavailable; retrying", &last_error)),
                false,
                Some(StatusTone::Error),
                true,
            );
        }
        self.reconnect_started = false;
        Ok(())
    }

    async fn finish_reconnect_attempt(&mut self, client: &DaemonTransportClient) -> Result<(), String> {
        if !self.roster_store.attach(client.clone()).await? {
            return Err("Daemon lost the agent_roster capability during reconnect".to_string());
        }
        if !self.refresh_heartbeats(true).await {
            return Err("Heartbeat catalog did not refresh during reconnect".to_string());
        }
        let sessions = self.roster_store.summaries().await;
        self.daemon_shutdown_received = false;
        self.reconnect_timed_out = false;
        self.reconnect_started = false;
        self.set_status_message(Some("Daemon reconnected"), false, None, false);
        self.apply_session_list(sessions, true);
        self.arm_saved_search_fetch(true);
        Ok(())
    }

    /// `applySessionList(sessions, successful)`.
    pub fn apply_session_list(&mut self, sessions: Vec<SessionSummary>, successful: bool) {
        self.last_listed_summaries = sessions.clone();
        if successful {
            self.persistent_state.last_successful_live_summaries = Some(sessions);
        }
        self.reconcile_catalogs();
    }

    /// `reconcileCatalogs()`.
    pub fn reconcile_catalogs(&mut self) {
        let manually_inactive = self.inactive_agent_identities.clone();
        let visible_sessions: Vec<SessionSummary> = self
            .last_listed_summaries
            .iter()
            .filter(|summary| {
                should_show_agents_view_session(
                    summary,
                    manually_inactive.contains(&get_agents_view_summary_identity(summary)),
                )
            })
            .cloned()
            .collect();
        self.last_visible_summaries = self.with_pending_delete_session(&visible_sessions);
        self.unified_records = reconcile_unified_sessions(
            &self.last_visible_summaries,
            &self.saved_sessions,
            &self.heartbeats,
        );
        self.unified_index = super::agents_view_state::build_unified_session_index(&self.unified_records);
        migrate_agents_view_identity_set(&mut self.expanded_subagent_parents, &self.unified_index.by_key);
        migrate_agents_view_identity_set(&mut self.program_shown_parents, &self.unified_index.by_key);

        let frames = self.persistent_state.scope_frames.clone().unwrap_or_default();
        let resolution = resolve_agents_view_scope_frames(&self.unified_records, &frames, Some(&self.unified_index));
        if should_apply_scope_resolution(resolution.dropped_frames, self.saved_catalog_ready) {
            self.persistent_state.scope_frames = Some(resolution.frames.clone());
            self.scope_key = resolution.frames.last().map(|frame| frame.scope.clone());
            self.scope_root_summary = resolution.root.as_ref().map(summary_for_unified_record);
            self.persistent_state.scope_root_summary = self.scope_root_summary.clone();
            if resolution.dropped_frames > 0 {
                let destination = if resolution.root.is_some() {
                    "the nearest available parent"
                } else {
                    "the global view"
                };
                self.set_status_message(
                    Some(&format!("Scope is no longer available; returned to {destination}")),
                    false,
                    None,
                    false,
                );
            }
        }
        self.scoped_records =
            scope_to_session_subtree(&self.unified_records, self.scope_key.as_ref(), Some(&self.unified_index));
        self.rebuild_rows();
        self.apply_pending_ancestor_expansion();
        self.restore_selection();
        self.terminal.request_render(false);
    }

    /// `rebuildRows()`: rebuild from the last fetched summaries, keeping
    /// selection on the same row.
    pub fn rebuild_rows(&mut self) {
        let selected_identity = self.rows.get(self.selected_index).map(|row| row.identity.clone());
        let filtered = self.get_filtered_records();
        let rollups = compute_recursive_rollups(&self.unified_records, Some(&self.unified_index));
        let inputs: Vec<AgentsViewRowInput> = filtered.into_iter().map(AgentsViewRowInput::Record).collect();
        self.all_rows = build_agents_view_rows(
            &inputs,
            &self.expanded_subagent_parents,
            &self.program_shown_parents,
            self.scope_key.as_ref(),
            Some(&rollups),
            self.anchor_session_id.as_deref(),
        );
        let show_inactive = is_inactive_expanded(&self.persistent_state) || self.action_search_text().trim().len() > 0;
        self.rows = compact_session_rows(&self.all_rows, show_inactive);
        match selected_identity {
            Some(identity) => {
                let index = self.rows.iter().position(|row| row.identity == identity);
                match index {
                    Some(index) => self.selected_index = index,
                    None => self.restore_selection(),
                }
            }
            None => self.restore_selection(),
        }
    }

    /// The text the list filter uses: the search query, or the preserved query
    /// while the reply composer or rename mode owns the editor.
    fn action_search_text(&self) -> String {
        if self.reply_target.is_some() || self.rename_target.is_some() {
            self.action_mode_search_query.clone().unwrap_or_default()
        } else {
            self.editor.get_text()
        }
    }

    /// `getFilteredRecords()`.
    pub fn get_filtered_records(&self) -> Vec<UnifiedSessionRecord> {
        let query = self.action_search_text();
        filter_unified_sessions(&self.scoped_records, &|text| matches_search_text(text, &query))
    }
