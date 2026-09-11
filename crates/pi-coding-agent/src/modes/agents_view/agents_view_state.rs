//! Port of packages/coding-agent/src/modes/agents-view/agents-view-state.ts
//!
//! Data model notes (PORT-RULES "absent vs null"):
//! - TypeScript optional fields (`?:`) become `Option<T>`: absent == `None`, and a
//!   JSON `null` also deserializes to `None` because the reference code never
//!   distinguishes the two for these fields (`??` / `=== true` tests only).
//! - `Map<UnifiedSessionRecord, _>` / `Set<UnifiedSessionRecord>` key on the record
//!   value in TypeScript. Rust has no reference identity for owned values, so the
//!   record's stable `identity` string is used as the map key instead. Two records
//!   with the same identity cannot exist: `reconcileUnifiedSessions` folds aliases
//!   onto one record.

use std::collections::{HashMap, HashSet};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Node's `Date#toISOString()` (millisecond precision, `Z` suffix).
pub fn to_iso_string(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Approximation of `Date.parse`: RFC 3339 first, then a few common shapes that
/// the reference code passes in (JSONL headers, `statSync().mtime`).
pub fn parse_js_timestamp(value: &str) -> Option<i64> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.timestamp_millis());
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, format) {
            return Some(Utc.from_utc_datetime(&naive).timestamp_millis());
        }
        if let Ok(date) = chrono::NaiveDate::parse_from_str(value, format) {
            return Some(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?).timestamp_millis());
        }
    }
    None
}

/// Milliseconds since the Unix epoch, `Date.now()`.
pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentsViewSection {
    Running,
    Idle,
    Inactive,
}

/// Durable lifecycle; decides agents-view visibility. Only `Live` is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionLifecycle {
    Draft,
    #[default]
    Live,
    Archived,
}

/// Heuristic activity of a live session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionActivity {
    Working,
    #[default]
    Idle,
}

/// One status formula shared by every agent surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentRosterStatus {
    Running,
    Idle,
    Inactive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeKind {
    TopLevel,
    Subagent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskState {
    NeedsInput,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentRosterStatusLabel {
    Queued,
    Recovering,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkerState {
    Starting,
    Ready,
    Recovering,
    Stopping,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionActionActive {
    pub kind: String,
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionActionSnapshot {
    pub queued_count: i64,
    #[serde(default)]
    pub steering: Vec<String>,
    #[serde(default)]
    pub follow_ups: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<SessionActionActive>,
}

/// Lightweight daemon session shape (SessionSummary in daemon-session-list.ts).
///
/// `#[serde(default)]` mirrors the reference's structural typing: `isSessionSummary`
/// only checks `id` and `sessionId`, so any extra/absent field must not fail.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionSummary {
    pub id: String,
    pub lifecycle: SessionLifecycle,
    pub activity: SessionActivity,
    pub is_session_active: bool,
    pub has_active_heartbeat: Option<bool>,
    pub has_registered_heartbeat: Option<bool>,
    pub has_registered_cron_job: Option<bool>,
    pub last_activity_at: Option<String>,
    pub runtime_kind: Option<RuntimeKind>,
    /// RLM spawn depth (0 for roots); fork edges preserve the source depth.
    pub rlm_depth: Option<i64>,
    pub active_session_id: Option<String>,
    pub session_id: String,
    pub session_file: Option<String>,
    pub session_name: Option<String>,
    pub cwd: String,
    pub model: Option<ModelRef>,
    /// ThinkingLevel union; kept as the wire string until pi-agent-core lands one.
    pub thinking_level: Option<String>,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub is_bash_running: Option<bool>,
    pub has_running_rlm_children: Option<bool>,
    pub usage: Option<SessionUsageSummary>,
    pub is_running_tools: Option<bool>,
    pub attached_clients: i64,
    pub direct_attached_clients: Option<i64>,
    pub message_count: i64,
    pub unfinished_action_count: Option<i64>,
    pub session_actions: SessionActionSnapshot,
    pub streaming_message: Option<Value>,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub first_message: Option<String>,
    pub parent_active_session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub parent_session_path: Option<String>,
    pub rlm_child_id: Option<String>,
    pub replied_since_task: Option<bool>,
    pub rlm_parent_node_id: Option<String>,
    /// Source of the Python cell that spawned this subagent, for display.
    pub spawn_code: Option<String>,
    pub model_fallback_message: Option<String>,
    pub diagnostics: Option<Vec<Value>>,
    pub summary: Option<String>,
    pub task_state: Option<AgentTaskState>,
    pub roster_status: Option<AgentRosterStatus>,
    pub status_label: Option<AgentRosterStatusLabel>,
    /// Set while the owning worker has been silent past the staleness threshold.
    pub last_heard_from_at: Option<String>,
    pub worker_state: Option<WorkerState>,
    pub worker_pid: Option<i64>,
}

impl SessionSummary {
    /// Minimal constructor for the fields the reference always sets.
    pub fn new(id: impl Into<String>, session_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            id: id.clone(),
            session_id,
            cwd: cwd.into(),
            lifecycle: SessionLifecycle::Live,
            activity: SessionActivity::Idle,
            is_session_active: false,
            is_streaming: false,
            is_compacting: false,
            attached_clients: 0,
            message_count: 0,
            session_actions: SessionActionSnapshot::default(),
            ..Self::default()
        }
    }

    pub fn with_session_file(mut self, path: impl Into<String>) -> Self {
        self.session_file = Some(path.into());
        self
    }

    pub fn with_active_session_id(mut self, active: impl Into<String>) -> Self {
        self.active_session_id = Some(active.into());
        self
    }
}


#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UnifiedSessionHeartbeat {
    pub active_count: i64,
    pub paused_count: Option<i64>,
    pub next_run_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentConnectionSavedSessionState {
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentConnectionAgentStatus {
    pub summary: String,
    pub task_state: Option<AgentTaskState>,
    pub based_on_message_count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentConnectionSavedSessionInfo {
    pub path: String,
    pub id: String,
    pub cwd: String,
    pub name: Option<String>,
    pub state: Option<AgentConnectionSavedSessionState>,
    pub parent_session_path: Option<String>,
    pub rlm_depth: Option<i64>,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
    pub message_count: i64,
    pub first_message: String,
    pub all_messages_text: String,
    pub agent_status: Option<AgentConnectionAgentStatus>,
    pub usage: Option<SessionUsageSummary>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentCronSchedule {
    pub kind: String,
    pub expression: String,
    pub interval_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentCronJob {
    pub id: String,
    pub status: String,
    pub source: Option<String>,
    pub runtime_kind: Option<String>,
    pub delivery_mode: Option<String>,
    pub active_session_id: String,
    pub session_id: String,
    pub session_file: String,
    pub cwd: String,
    pub label: Option<String>,
    pub prompt: String,
    pub schedule: AgentCronSchedule,
    pub created_at: String,
    pub updated_at: String,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_skipped_at: Option<String>,
    pub last_error: Option<String>,
    pub run_count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentConnectionHeartbeat {
    pub job: AgentCronJob,
    pub session_name: Option<String>,
    pub first_message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UnifiedSessionRecord {
    pub daemon: Option<SessionSummary>,
    pub saved: Option<AgentConnectionSavedSessionInfo>,
    /// Stable UI key, chosen using canonical path, session id, then active id.
    pub identity: String,
    /// Alternate keys used to restore selection while a session is persisted or reattached.
    pub identity_aliases: Vec<String>,
    pub section: AgentsViewSection,
    pub searchable_text: String,
    pub heartbeat: Option<UnifiedSessionHeartbeat>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentsViewScopeKey {
    pub session_id: String,
    pub active_session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentsViewScopeFrame {
    pub scope: AgentsViewScopeKey,
    /// Chat to revisit before returning to the parent agents view.
    pub return_chat: Option<SessionSummary>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentsViewScopeAction {
    Push {
        scope: AgentsViewScopeKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        return_chat: Option<SessionSummary>,
    },
    Back,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsViewScopeResolution {
    pub frames: Vec<AgentsViewScopeFrame>,
    pub root: Option<UnifiedSessionRecord>,
    pub dropped_frames: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsViewScopeBackResult {
    #[serde(rename = "type")]
    pub kind: String,
    pub selection: SessionSummary,
    pub expanded_ancestor_session_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_chat: Option<SessionSummary>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnattachableChildOpenResult {
    #[serde(rename = "type")]
    pub kind: String,
    pub summary: SessionSummary,
    pub selection: SessionSummary,
    pub expanded_ancestor_session_ids: Vec<String>,
    pub has_children: bool,
    pub status_message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentsViewRowKind {
    Agent,
    SubagentSummary,
    Subagent,
    SubagentCode,
}

/// Hard cap on spawn-code lines shown so a large program never floods the view.
pub const MAX_SPAWN_CODE_LINES: usize = 10;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentsViewRow {
    pub kind: AgentsViewRowKind,
    pub section: AgentsViewSection,
    pub summary: SessionSummary,
    pub title: String,
    pub subtitle: String,
    pub status_label: String,
    pub depth: usize,
    pub selectable: bool,
    pub running_subagent_count: i64,
    pub recursive_cost: f64,
    /// Total descendant sessions (resident + passive) under this row.
    pub descendant_count: i64,
    /// Unique selection identity for this row.
    pub identity: String,
    /// Identity of the agent row this row is nested under.
    pub parent_identity: Option<String>,
    /// True when this row's subagents carry spawn code that can be revealed.
    pub has_spawn_code: Option<bool>,
    /// True when this subagent-summary row's list is expanded.
    pub expanded: Option<bool>,
    /// One source line of the spawn cell, for "subagent-code" rows.
    pub code: Option<String>,
    /// Merged durable/live source data for unified rows.
    pub record: Option<UnifiedSessionRecord>,
    pub heartbeat: Option<UnifiedSessionHeartbeat>,
}

impl Default for AgentsViewRow {
    fn default() -> Self {
        Self {
            kind: AgentsViewRowKind::Agent,
            section: AgentsViewSection::Idle,
            summary: SessionSummary::default(),
            title: String::new(),
            subtitle: String::new(),
            status_label: String::new(),
            depth: 0,
            selectable: true,
            running_subagent_count: 0,
            recursive_cost: 0.0,
            descendant_count: 0,
            identity: String::new(),
            parent_identity: None,
            has_spawn_code: None,
            expanded: None,
            code: None,
            record: None,
            heartbeat: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentsViewRecursiveRollup {
    /// Own cost plus every descendant's cost.
    pub cost: f64,
    /// Total descendant sessions (resident + passive) under this record.
    pub descendant_count: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentsViewSelectionKey {
    pub session_id: String,
    pub active_session_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsViewSelectionResolution {
    pub index: usize,
    pub resolved: bool,
}


/// Key used for record-keyed maps and sets: TypeScript keys on the record value,
/// Rust on its stable `identity` (see module docs).
pub type RecordKey = String;

pub type UnifiedSessionRollups = HashMap<RecordKey, AgentsViewRecursiveRollup>;

#[derive(Clone, Debug, Default)]
pub struct UnifiedSessionIndex {
    pub by_key: HashMap<String, RecordKey>,
    pub children_by_parent: HashMap<RecordKey, Vec<RecordKey>>,
    /// Records in catalog order, keyed by identity.
    pub records_by_key: HashMap<RecordKey, UnifiedSessionRecord>,
}

pub fn classify_agent_status(resident: bool, queued_child: bool, busy: bool) -> AgentRosterStatus {
    if queued_child {
        return AgentRosterStatus::Running;
    }
    if !resident {
        return AgentRosterStatus::Inactive;
    }
    if busy {
        AgentRosterStatus::Running
    } else {
        AgentRosterStatus::Idle
    }
}

pub fn classify_session_roster_status(summary: &SessionSummary, queued_child: bool) -> AgentRosterStatus {
    classify_agent_status(
        summary.active_session_id.is_some(),
        queued_child,
        summary.activity == SessionActivity::Working || summary.is_session_active,
    )
}

pub fn classify_agents_view_session(summary: &SessionSummary) -> AgentsViewSection {
    match summary.roster_status {
        Some(status) => match status {
            AgentRosterStatus::Running => AgentsViewSection::Running,
            AgentRosterStatus::Idle => AgentsViewSection::Idle,
            AgentRosterStatus::Inactive => AgentsViewSection::Inactive,
        },
        None => match classify_session_roster_status(summary, false) {
            AgentRosterStatus::Running => AgentsViewSection::Running,
            AgentRosterStatus::Idle => AgentsViewSection::Idle,
            AgentRosterStatus::Inactive => AgentsViewSection::Inactive,
        },
    }
}

pub fn classify_unified_session(record: &UnifiedSessionRecord) -> AgentsViewSection {
    match &record.daemon {
        None => AgentsViewSection::Inactive,
        Some(summary) => classify_agents_view_session(summary),
    }
}

pub fn should_show_agents_view_session(summary: &SessionSummary, manually_inactive: bool) -> bool {
    if manually_inactive {
        return false;
    }
    summary.lifecycle == SessionLifecycle::Live
}

// TODO(unify: #2055): replace with the shared user-content rule once it lands;
// session summaries only carry message counts today.
pub fn is_empty_agents_view_session(summary: &SessionSummary) -> bool {
    summary.message_count == 0
}

pub fn section_title(section: AgentsViewSection) -> &'static str {
    match section {
        AgentsViewSection::Running => "Running",
        AgentsViewSection::Idle => "Idle",
        AgentsViewSection::Inactive => "Inactive",
    }
}

fn format_age_label(timestamp: &str) -> String {
    let parsed = parse_js_timestamp(timestamp).unwrap_or(0);
    let seconds = (((now_ms() - parsed) as f64) / 1000.0).round().max(0.0) as i64;
    if seconds < 120 {
        return format!("{seconds}s ago");
    }
    let minutes = ((seconds as f64) / 60.0).round() as i64;
    if minutes < 120 {
        format!("{minutes}m ago")
    } else {
        format!("{}h ago", ((minutes as f64) / 60.0).round() as i64)
    }
}

/// `resolve(canonicalizePath(path))`: realpath where possible, lexical resolve after.
fn canonical_session_path(path: &str) -> String {
    crate::utils::paths::resolve_absolute(&crate::utils::paths::canonicalize_path(path))
}

fn file_identity(path: &str) -> String {
    format!("file:{}", canonical_session_path(path))
}

fn summary_identity_aliases(summary: &SessionSummary) -> Vec<String> {
    let roster = if summary.runtime_kind == Some(RuntimeKind::Subagent) && summary.rlm_child_id.is_some() {
        Some(format!("agent:{}", roster_agent_id_for_summary(summary)))
    } else {
        None
    };
    [
        roster,
        summary.session_file.as_deref().map(file_identity),
        Some(format!("session:{}", summary.session_id)),
        summary.active_session_id.as_ref().map(|id| format!("active:{id}")),
        Some(format!("active:{}", summary.id)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn saved_identity_aliases(saved: &AgentConnectionSavedSessionInfo) -> Vec<String> {
    vec![file_identity(&saved.path), format!("session:{}", saved.id)]
}

fn create_unified_searchable_text(
    daemon: Option<&SessionSummary>,
    saved: Option<&AgentConnectionSavedSessionInfo>,
) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if let Some(d) = daemon {
        parts.extend(
            [
                d.session_id.as_str(),
                d.active_session_id.as_deref().unwrap_or(""),
                d.session_name.as_deref().unwrap_or(""),
                d.first_message.as_deref().unwrap_or(""),
                d.cwd.as_str(),
                d.session_file.as_deref().unwrap_or(""),
                d.summary.as_deref().unwrap_or(""),
            ]
            .into_iter(),
        );
    }
    if let Some(s) = saved {
        parts.extend(
            [
                s.id.as_str(),
                s.name.as_deref().unwrap_or(""),
                s.first_message.as_str(),
                s.all_messages_text.as_str(),
                s.agent_status.as_ref().map(|status| status.summary.as_str()).unwrap_or(""),
                s.cwd.as_str(),
                s.path.as_str(),
                s.parent_session_path.as_deref().unwrap_or(""),
            ]
            .into_iter(),
        );
    }
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<&str>>()
        .join(" ")
}

/// Child ids are only unique per parent (32-bit, mkdir-checked); the parent path
/// qualifies them daemon-wide.
pub fn roster_agent_id_for_summary(summary: &SessionSummary) -> String {
    if summary.runtime_kind == Some(RuntimeKind::Subagent) {
        if let Some(child_id) = summary.rlm_child_id.as_deref() {
            // No-session parents have no path (and no ledger edge); their live
            // parent id still disambiguates.
            let parent_key = summary
                .parent_session_path
                .as_deref()
                .map(canonical_session_path)
                .or_else(|| summary.parent_active_session_id.clone());
            return match parent_key {
                Some(key) => format!("{key}#{child_id}"),
                None => child_id.to_string(),
            };
        }
    }
    summary.session_id.clone()
}

/// Convert a merged row to the existing live-row rendering/action shape.
pub fn summary_for_unified_record(record: &UnifiedSessionRecord) -> SessionSummary {
    if let Some(daemon) = &record.daemon {
        let Some(saved) = &record.saved else {
            return daemon.clone();
        };
        let mut merged = daemon.clone();
        if merged.session_name.is_none() {
            merged.session_name = saved.name.clone();
        }
        if merged.first_message.is_none() {
            merged.first_message = Some(saved.first_message.clone());
        }
        if merged.usage.is_none() {
            merged.usage = saved.usage.clone();
        }
        if merged.session_file.is_none() {
            merged.session_file = Some(canonical_session_path(&saved.path));
        }
        if merged.parent_session_path.is_none() {
            merged.parent_session_path = saved.parent_session_path.clone();
        }
        if merged.rlm_depth.is_none() {
            merged.rlm_depth = saved.rlm_depth;
        }
        if merged.created.is_none() {
            merged.created = Some(to_iso_string(&saved.created));
        }
        if merged.modified.is_none() {
            merged.modified = Some(to_iso_string(&saved.modified));
        }
        if merged.last_activity_at.is_none() {
            merged.last_activity_at = Some(to_iso_string(&saved.modified));
        }
        return merged;
    }
    let saved = record
        .saved
        .as_ref()
        .expect("Unified session record has no daemon or saved source");
    let depth = saved.rlm_depth.unwrap_or(if saved.parent_session_path.is_some() { 1 } else { 0 });
    SessionSummary {
        id: saved.id.clone(),
        lifecycle: SessionLifecycle::Archived,
        activity: SessionActivity::Idle,
        is_session_active: false,
        runtime_kind: Some(if depth > 0 { RuntimeKind::Subagent } else { RuntimeKind::TopLevel }),
        rlm_depth: saved.rlm_depth,
        session_id: saved.id.clone(),
        session_file: Some(canonical_session_path(&saved.path)),
        parent_session_path: saved.parent_session_path.clone(),
        session_name: saved.name.clone(),
        cwd: saved.cwd.clone(),
        is_streaming: false,
        is_compacting: false,
        attached_clients: 0,
        message_count: saved.message_count,
        session_actions: SessionActionSnapshot::default(),
        created: Some(to_iso_string(&saved.created)),
        modified: Some(to_iso_string(&saved.modified)),
        last_activity_at: Some(to_iso_string(&saved.modified)),
        first_message: Some(saved.first_message.clone()),
        summary: saved.agent_status.as_ref().map(|status| status.summary.clone()),
        task_state: saved.agent_status.as_ref().and_then(|status| status.task_state),
        usage: saved.usage.clone(),
        ..SessionSummary::default()
    }
}
