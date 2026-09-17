//! Port of packages/coding-agent/src/modes/daemon/agent-roster.ts

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One status formula shared by every agent surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentRosterStatus {
    Running,
    Idle,
    Inactive,
}

impl AgentRosterStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentRosterStatus::Running => "running",
            AgentRosterStatus::Idle => "idle",
            AgentRosterStatus::Inactive => "inactive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentStatusInput {
    /// A live runtime exists for the agent.
    pub resident: bool,
    /// Admitted child run whose session has not materialized yet.
    pub queued_child: bool,
    /// Actively working: streaming or running tools/bash.
    pub busy: bool,
}

pub fn classify_agent_status(input: AgentStatusInput) -> AgentRosterStatus {
    if input.queued_child {
        return AgentRosterStatus::Running;
    }
    if !input.resident {
        return AgentRosterStatus::Inactive;
    }
    if input.busy {
        AgentRosterStatus::Running
    } else {
        AgentRosterStatus::Idle
    }
}

/// Residency/shutdown-safety busy: delegated child work counts.
pub fn is_session_summary_busy(is_session_active: bool, has_running_rlm_children: Option<bool>) -> bool {
    is_session_active || has_running_rlm_children == Some(true)
}

/// The `SessionSummary` fields the roster classifier reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RosterSummaryView {
    pub active_session_id: Option<String>,
    pub activity: Option<String>,
    pub is_session_active: Option<bool>,
}

pub fn classify_session_roster_status(summary: &RosterSummaryView, queued_child: bool) -> AgentRosterStatus {
    classify_agent_status(AgentStatusInput {
        resident: summary.active_session_id.is_some(),
        queued_child,
        busy: match summary.activity.as_deref() {
            Some("working") => true,
            Some("idle") => false,
            _ => summary.is_session_active == Some(true),
        },
    })
}

/// The slim session summary carried by roster entries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RosterSessionSummary {
    pub id: String,
    pub lifecycle: String,
    pub activity: String,
    #[serde(rename = "isSessionActive")]
    pub is_session_active: bool,
    #[serde(rename = "hasActiveHeartbeat", skip_serializing_if = "Option::is_none", default)]
    pub has_active_heartbeat: Option<bool>,
    #[serde(rename = "hasRegisteredHeartbeat", skip_serializing_if = "Option::is_none", default)]
    pub has_registered_heartbeat: Option<bool>,
    #[serde(rename = "hasRegisteredCronJob", skip_serializing_if = "Option::is_none", default)]
    pub has_registered_cron_job: Option<bool>,
    #[serde(rename = "lastActivityAt", skip_serializing_if = "Option::is_none", default)]
    pub last_activity_at: Option<String>,
    #[serde(rename = "runtimeKind", skip_serializing_if = "Option::is_none", default)]
    pub runtime_kind: Option<String>,
    #[serde(rename = "rlmDepth", skip_serializing_if = "Option::is_none", default)]
    pub rlm_depth: Option<i64>,
    #[serde(rename = "activeSessionId", skip_serializing_if = "Option::is_none", default)]
    pub active_session_id: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "sessionFile", skip_serializing_if = "Option::is_none", default)]
    pub session_file: Option<String>,
    #[serde(rename = "sessionName", skip_serializing_if = "Option::is_none", default)]
    pub session_name: Option<String>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model: Option<Value>,
    #[serde(rename = "thinkingLevel", skip_serializing_if = "Option::is_none", default)]
    pub thinking_level: Option<String>,
    #[serde(rename = "isStreaming")]
    pub is_streaming: bool,
    #[serde(rename = "isCompacting")]
    pub is_compacting: bool,
    #[serde(rename = "isBashRunning", skip_serializing_if = "Option::is_none", default)]
    pub is_bash_running: Option<bool>,
    #[serde(rename = "hasRunningRlmChildren", skip_serializing_if = "Option::is_none", default)]
    pub has_running_rlm_children: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub usage: Option<Value>,
    #[serde(rename = "isRunningTools", skip_serializing_if = "Option::is_none", default)]
    pub is_running_tools: Option<bool>,
    #[serde(rename = "attachedClients")]
    pub attached_clients: i64,
    #[serde(rename = "directAttachedClients", skip_serializing_if = "Option::is_none", default)]
    pub direct_attached_clients: Option<i64>,
    #[serde(rename = "messageCount")]
    pub message_count: i64,
    #[serde(rename = "unfinishedActionCount", skip_serializing_if = "Option::is_none", default)]
    pub unfinished_action_count: Option<i64>,
    #[serde(rename = "sessionActions", skip_serializing_if = "Option::is_none", default)]
    pub session_actions: Option<Value>,
    #[serde(rename = "streamingMessage", skip_serializing_if = "Option::is_none", default)]
    pub streaming_message: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub modified: Option<String>,
    #[serde(rename = "firstMessage", skip_serializing_if = "Option::is_none", default)]
    pub first_message: Option<String>,
    #[serde(rename = "parentActiveSessionId", skip_serializing_if = "Option::is_none", default)]
    pub parent_active_session_id: Option<String>,
    #[serde(rename = "parentSessionId", skip_serializing_if = "Option::is_none", default)]
    pub parent_session_id: Option<String>,
    #[serde(rename = "parentSessionPath", skip_serializing_if = "Option::is_none", default)]
    pub parent_session_path: Option<String>,
    #[serde(rename = "rlmChildId", skip_serializing_if = "Option::is_none", default)]
    pub rlm_child_id: Option<String>,
    #[serde(rename = "repliedSinceTask", skip_serializing_if = "Option::is_none", default)]
    pub replied_since_task: Option<bool>,
    #[serde(rename = "rlmParentNodeId", skip_serializing_if = "Option::is_none", default)]
    pub rlm_parent_node_id: Option<String>,
    #[serde(rename = "spawnCode", skip_serializing_if = "Option::is_none", default)]
    pub spawn_code: Option<String>,
    #[serde(rename = "modelFallbackMessage", skip_serializing_if = "Option::is_none", default)]
    pub model_fallback_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub diagnostics: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub summary: Option<String>,
    #[serde(rename = "taskState", skip_serializing_if = "Option::is_none", default)]
    pub task_state: Option<String>,
    #[serde(rename = "rosterStatus", skip_serializing_if = "Option::is_none", default)]
    pub roster_status: Option<AgentRosterStatus>,
    #[serde(rename = "statusLabel", skip_serializing_if = "Option::is_none", default)]
    pub status_label: Option<String>,
    #[serde(rename = "lastHeardFromAt", skip_serializing_if = "Option::is_none", default)]
    pub last_heard_from_at: Option<String>,
    #[serde(rename = "workerState", skip_serializing_if = "Option::is_none", default)]
    pub worker_state: Option<String>,
    #[serde(rename = "workerPid", skip_serializing_if = "Option::is_none", default)]
    pub worker_pid: Option<i64>,
    /// Fields this slice does not model yet, kept so a round trip loses nothing.
    #[serde(flatten, default)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerRosterEntry {
    #[serde(rename = "agentId")]
    pub agent_id: String,
    #[serde(rename = "queuedChild", skip_serializing_if = "Option::is_none", default)]
    pub queued_child: Option<bool>,
    #[serde(rename = "seededCwd", skip_serializing_if = "Option::is_none", default)]
    pub seeded_cwd: Option<bool>,
    pub summary: RosterSessionSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRosterEntry {
    #[serde(rename = "agentId")]
    pub agent_id: String,
    #[serde(rename = "queuedChild", skip_serializing_if = "Option::is_none", default)]
    pub queued_child: Option<bool>,
    #[serde(rename = "seededCwd", skip_serializing_if = "Option::is_none", default)]
    pub seeded_cwd: Option<bool>,
    pub summary: RosterSessionSummary,
    pub status: AgentRosterStatus,
    #[serde(rename = "statusLabel", skip_serializing_if = "Option::is_none", default)]
    pub status_label: Option<String>,
    #[serde(rename = "lastHeardFromAt", skip_serializing_if = "Option::is_none", default)]
    pub last_heard_from_at: Option<String>,
    #[serde(rename = "workerId", skip_serializing_if = "Option::is_none", default)]
    pub worker_id: Option<String>,
}

/// Child ids are only unique per parent; the parent path qualifies them daemon-wide.
pub fn roster_agent_id_for_summary(
    runtime_kind: Option<&str>,
    rlm_child_id: Option<&str>,
    session_id: &str,
    parent_session_path: Option<&str>,
    parent_active_session_id: Option<&str>,
) -> String {
    if runtime_kind == Some("subagent") {
        if let Some(rlm_child_id) = rlm_child_id {
            let parent_key = parent_session_path
                .map(canonical_session_path)
                .or_else(|| parent_active_session_id.map(str::to_string));
            return match parent_key {
                Some(parent_key) => format!("{parent_key}#{rlm_child_id}"),
                None => rlm_child_id.to_string(),
            };
        }
    }
    session_id.to_string()
}

pub fn roster_agent_id_for_entry(summary: &RosterSessionSummary) -> String {
    roster_agent_id_for_summary(
        summary.runtime_kind.as_deref(),
        summary.rlm_child_id.as_deref(),
        &summary.session_id,
        summary.parent_session_path.as_deref(),
        summary.parent_active_session_id.as_deref(),
    )
}

pub fn worker_roster_entry_from_summary(summary: &RosterSessionSummary) -> WorkerRosterEntry {
    let mut slim = summary.clone();
    slim.streaming_message = None;
    slim.session_actions = None;
    slim.diagnostics = None;
    WorkerRosterEntry {
        agent_id: roster_agent_id_for_entry(summary),
        queued_child: None,
        seeded_cwd: None,
        summary: slim,
    }
}

fn classify_worker_roster_entry(entry: &WorkerRosterEntry) -> AgentRosterStatus {
    classify_session_roster_status(
        &RosterSummaryView {
            active_session_id: entry.summary.active_session_id.clone(),
            activity: Some(entry.summary.activity.clone()),
            is_session_active: Some(entry.summary.is_session_active),
        },
        entry.queued_child == Some(true),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RegisteredHeartbeatFlags {
    pub has_registered_heartbeat: bool,
    pub has_registered_cron_job: bool,
}

pub fn passivated_worker_roster_entry(
    entry: &WorkerRosterEntry,
    registrations: Option<RegisteredHeartbeatFlags>,
) -> WorkerRosterEntry {
    let mut summary = entry.summary.clone();
    summary.active_session_id = None;
    summary.direct_attached_clients = None;
    summary.has_active_heartbeat = None;
    summary.has_registered_heartbeat = None;
    summary.has_registered_cron_job = None;
    summary.has_running_rlm_children = None;
    summary.is_bash_running = None;
    summary.is_running_tools = None;
    summary.worker_state = None;
    summary.worker_pid = None;
    summary.id = summary.session_id.clone();
    summary.activity = "idle".to_string();
    summary.is_session_active = false;
    summary.is_streaming = false;
    summary.is_compacting = false;
    summary.attached_clients = 0;
    if registrations.is_some_and(|flags| flags.has_registered_heartbeat) {
        summary.has_registered_heartbeat = Some(true);
    }
    if registrations.is_some_and(|flags| flags.has_registered_cron_job) {
        summary.has_registered_cron_job = Some(true);
    }
    WorkerRosterEntry {
        agent_id: entry.agent_id.clone(),
        queued_child: entry.queued_child,
        seeded_cwd: entry.seeded_cwd,
        summary,
    }
}

/// The full `SessionSummary` shape (roster entries carry a slim version).
pub fn session_summary_from_roster_entry(
    summary: &RosterSessionSummary,
    roster_status: Option<AgentRosterStatus>,
    status_label: Option<&str>,
    last_heard_from_at: Option<&str>,
) -> RosterSessionSummary {
    let mut full = summary.clone();
    full.session_actions = Some(serde_json::json!({
        "queuedCount": 0,
        "steering": [],
        "followUps": [],
    }));
    if let Some(roster_status) = roster_status {
        full.roster_status = Some(roster_status);
    }
    if let Some(status_label) = status_label {
        full.status_label = Some(status_label.to_string());
    }
    if let Some(last_heard_from_at) = last_heard_from_at {
        full.last_heard_from_at = Some(last_heard_from_at.to_string());
    }
    full
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRosterMutation {
    Write { agent_id: String },
    Delete { agent_id: String },
}

/// Canonical session path helper (core/session-lease.ts owns the real one).
pub fn canonical_session_path(session_path: &str) -> String {
    let resolved = lexical_resolve(session_path);
    std::fs::canonicalize(&resolved)
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or(resolved)
}

fn lexical_resolve(path: &str) -> String {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        normalize_lexically(candidate)
    } else {
        let base = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
        normalize_lexically(&base.join(candidate))
    }
}

fn normalize_lexically(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut prefix = String::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::Prefix(prefix_component) => {
                prefix.push_str(&prefix_component.as_os_str().to_string_lossy());
            }
            Component::RootDir => {
                prefix.push(std::path::MAIN_SEPARATOR);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
        }
    }
    let joined = parts.join(&std::path::MAIN_SEPARATOR.to_string());
    if prefix.is_empty() {
        joined
    } else if prefix.ends_with(std::path::MAIN_SEPARATOR) {
        format!("{prefix}{joined}")
    } else {
        format!("{prefix}{}{joined}", std::path::MAIN_SEPARATOR)
    }
}

/// Supervisor-owned roster store; write() classifies once and its file index
/// converges seed and worker keys.
pub struct AgentRoster {
    entries: HashMap<String, AgentRosterEntry>,
    agent_id_by_active_session_id: HashMap<String, String>,
    agent_id_by_session_file: HashMap<String, String>,
    canonical_path: Box<dyn Fn(&str) -> String + Send + Sync>,
    on_mutation: Box<dyn Fn(AgentRosterMutation) + Send + Sync>,
}

impl AgentRoster {
    pub fn new(
        canonical_path: Box<dyn Fn(&str) -> String + Send + Sync>,
        on_mutation: Box<dyn Fn(AgentRosterMutation) + Send + Sync>,
    ) -> Self {
        Self {
            entries: HashMap::new(),
            agent_id_by_active_session_id: HashMap::new(),
            agent_id_by_session_file: HashMap::new(),
            canonical_path,
            on_mutation,
        }
    }

    pub fn values(&self) -> Vec<AgentRosterEntry> {
        self.entries.values().cloned().collect()
    }

    pub fn get(&self, agent_id: &str) -> Option<AgentRosterEntry> {
        self.entries.get(agent_id).cloned()
    }

    pub fn has(&self, agent_id: &str) -> bool {
        self.entries.contains_key(agent_id)
    }

    pub fn by_active_session_id(&self, active_session_id: &str) -> Option<AgentRosterEntry> {
        let agent_id = self.agent_id_by_active_session_id.get(active_session_id)?;
        self.entries.get(agent_id).cloned()
    }

    pub fn by_session_file(&self, canonical_path: &str) -> Option<AgentRosterEntry> {
        let agent_id = self.agent_id_by_session_file.get(canonical_path)?;
        self.entries.get(agent_id).cloned()
    }

    pub fn has_session_file(&self, canonical_path: &str) -> bool {
        self.agent_id_by_session_file.contains_key(canonical_path)
    }

    pub fn entries_for_worker(&self, worker_id: &str) -> Vec<AgentRosterEntry> {
        self.entries
            .values()
            .filter(|entry| entry.worker_id.as_deref() == Some(worker_id))
            .cloned()
            .collect()
    }

    pub fn write(
        &mut self,
        entry: WorkerRosterEntry,
        worker_id: Option<&str>,
        status_label: Option<&str>,
    ) -> AgentRosterEntry {
        let status = classify_worker_roster_entry(&entry);
        let stored = AgentRosterEntry {
            agent_id: entry.agent_id.clone(),
            queued_child: entry.queued_child,
            seeded_cwd: entry.seeded_cwd,
            summary: entry.summary,
            status,
            status_label: if entry.queued_child == Some(true) {
                Some("queued".to_string())
            } else {
                status_label.map(str::to_string)
            },
            last_heard_from_at: None,
            worker_id: worker_id.map(str::to_string),
        };
        if let Some(previous) = self.entries.get(&entry.agent_id).cloned() {
            self.drop_indexes(&previous);
        }
        if let Some(session_file) = &stored.summary.session_file {
            let file = (self.canonical_path)(session_file);
            if let Some(existing_agent_id) = self.agent_id_by_session_file.get(&file).cloned() {
                if existing_agent_id != entry.agent_id {
                    self.delete(&existing_agent_id);
                }
            }
            self.agent_id_by_session_file.insert(file, entry.agent_id.clone());
        }
        if let Some(active_session_id) = &stored.summary.active_session_id {
            self.agent_id_by_active_session_id
                .insert(active_session_id.clone(), entry.agent_id.clone());
        }
        self.entries.insert(entry.agent_id.clone(), stored.clone());
        (self.on_mutation)(AgentRosterMutation::Write {
            agent_id: entry.agent_id,
        });
        stored
    }

    pub fn delete(&mut self, agent_id: &str) {
        let Some(entry) = self.entries.get(agent_id).cloned() else {
            return;
        };
        self.drop_indexes(&entry);
        self.entries.remove(agent_id);
        (self.on_mutation)(AgentRosterMutation::Delete {
            agent_id: agent_id.to_string(),
        });
    }

    pub fn amend(&mut self, agent_id: &str, marks: RosterEntryMarks) {
        let Some(entry) = self.entries.get_mut(agent_id) else {
            return;
        };
        if let Some(status_label) = marks.status_label {
            match status_label {
                Some(label) => entry.status_label = Some(label),
                None => entry.status_label = None,
            }
        }
        if let Some(last_heard_from_at) = marks.last_heard_from_at {
            match last_heard_from_at {
                Some(value) => entry.last_heard_from_at = Some(value),
                None => entry.last_heard_from_at = None,
            }
        }
        (self.on_mutation)(AgentRosterMutation::Write {
            agent_id: agent_id.to_string(),
        });
    }

    fn drop_indexes(&mut self, entry: &AgentRosterEntry) {
        if let Some(active_session_id) = &entry.summary.active_session_id {
            if self.agent_id_by_active_session_id.get(active_session_id) == Some(&entry.agent_id) {
                self.agent_id_by_active_session_id.remove(active_session_id);
            }
        }
        if let Some(session_file) = &entry.summary.session_file {
            let file = (self.canonical_path)(session_file);
            if self.agent_id_by_session_file.get(&file) == Some(&entry.agent_id) {
                self.agent_id_by_session_file.remove(&file);
            }
        }
    }
}

/// The roster wire value for one entry: `AgentRosterEntry` serialized as the
/// TypeScript emits it, with `status` written through `as_str`.
pub fn agent_roster_entry_to_value(entry: &AgentRosterEntry) -> Value {
    let mut value = serde_json::to_value(entry).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.insert("status".to_string(), Value::String(entry.status.as_str().to_string()));
    }
    value
}

pub fn agent_roster_entry_from_value(value: &Value) -> Option<AgentRosterEntry> {
    serde_json::from_value(value.clone()).ok()
}

/// `amend` marks: `None` means "not provided", `Some(None)` clears the field.
#[derive(Debug, Clone, Default)]
pub struct RosterEntryMarks {
    pub status_label: Option<Option<String>>,
    pub last_heard_from_at: Option<Option<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(active_session_id: Option<&str>, activity: &str) -> RosterSessionSummary {
        RosterSessionSummary {
            id: "id".to_string(),
            lifecycle: "live".to_string(),
            activity: activity.to_string(),
            is_session_active: activity == "working",
            active_session_id: active_session_id.map(str::to_string),
            session_id: "session".to_string(),
            cwd: "/tmp".to_string(),
            is_streaming: false,
            is_compacting: false,
            attached_clients: 0,
            message_count: 0,
            ..Default::default()
        }
    }

    #[test]
    fn status_formula_matches_the_typescript() {
        assert_eq!(
            classify_agent_status(AgentStatusInput {
                resident: false,
                queued_child: true,
                busy: false
            }),
            AgentRosterStatus::Running
        );
        assert_eq!(
            classify_agent_status(AgentStatusInput {
                resident: false,
                queued_child: false,
                busy: true
            }),
            AgentRosterStatus::Inactive
        );
        assert_eq!(
            classify_agent_status(AgentStatusInput {
                resident: true,
                queued_child: false,
                busy: true
            }),
            AgentRosterStatus::Running
        );
        assert_eq!(
            classify_agent_status(AgentStatusInput {
                resident: true,
                queued_child: false,
                busy: false
            }),
            AgentRosterStatus::Idle
        );
        assert!(is_session_summary_busy(false, Some(true)));
        assert!(!is_session_summary_busy(false, None));
        assert_eq!(
            classify_session_roster_status(
                &RosterSummaryView {
                    active_session_id: Some("a".to_string()),
                    activity: Some("idle".to_string()),
                    is_session_active: Some(false)
                },
                false
            ),
            AgentRosterStatus::Idle
        );
    }

    #[test]
    fn background_residency_does_not_override_foreground_idle_but_queued_work_does() {
        let mut summary = RosterSummaryView {
            active_session_id: Some("a".into()),
            activity: Some("idle".into()),
            is_session_active: Some(true),
        };
        assert_eq!(classify_session_roster_status(&summary, false), AgentRosterStatus::Idle);
        assert!(is_session_summary_busy(true, None), "display classification must not weaken shutdown safety");
        assert_eq!(classify_session_roster_status(&summary, true), AgentRosterStatus::Running);
        summary.activity = Some("working".into());
        assert_eq!(classify_session_roster_status(&summary, false), AgentRosterStatus::Running);
        summary.activity = None;
        assert_eq!(classify_session_roster_status(&summary, false), AgentRosterStatus::Running);
    }

    #[test]
    fn roster_agent_id_qualifies_child_ids_with_the_parent() {
        assert_eq!(
            roster_agent_id_for_summary(Some("subagent"), Some("c1"), "s", Some("/p/session.jsonl"), None),
            format!("{}#c1", canonical_session_path("/p/session.jsonl"))
        );
        assert_eq!(
            roster_agent_id_for_summary(Some("subagent"), Some("c1"), "s", None, Some("parent-active")),
            "parent-active#c1"
        );
        assert_eq!(
            roster_agent_id_for_summary(Some("subagent"), Some("c1"), "s", None, None),
            "c1"
        );
        assert_eq!(roster_agent_id_for_summary(Some("top-level"), Some("c1"), "s", None, None), "s");
    }

    #[test]
    fn passivation_clears_residency_fields() {
        let mut base = summary(Some("active"), "working");
        base.has_active_heartbeat = Some(true);
        base.has_registered_heartbeat = Some(true);
        base.has_registered_cron_job = Some(true);
        base.is_bash_running = Some(true);
        base.has_running_rlm_children = Some(true);
        base.worker_state = Some("ready".to_string());
        let entry = WorkerRosterEntry {
            agent_id: "a".to_string(),
            queued_child: None,
            seeded_cwd: None,
            summary: base,
        };
        let passivated = passivated_worker_roster_entry(&entry, None);
        assert!(passivated.summary.active_session_id.is_none());
        assert_eq!(passivated.summary.activity, "idle");
        assert_eq!(passivated.summary.attached_clients, 0);
        assert!(passivated.summary.has_active_heartbeat.is_none());
        assert!(passivated.summary.worker_state.is_none());
        assert_eq!(passivated.summary.id, "session");

        let with_registrations = passivated_worker_roster_entry(
            &entry,
            Some(RegisteredHeartbeatFlags {
                has_registered_heartbeat: true,
                has_registered_cron_job: false,
            }),
        );
        assert_eq!(with_registrations.summary.has_registered_heartbeat, Some(true));
        assert!(with_registrations.summary.has_registered_cron_job.is_none());
    }

    #[test]
    fn roster_store_indexes_and_mutations() {
        let mutations = Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&mutations);
        let mut roster = AgentRoster::new(
            Box::new(canonical_session_path),
            Box::new(move |mutation| {
                sink.lock().expect("mutations poisoned").push(mutation);
            }),
        );
        let mut first = summary(Some("active-1"), "working");
        first.session_file = Some("/tmp/session.jsonl".to_string());
        let entry = WorkerRosterEntry {
            agent_id: "agent-1".to_string(),
            queued_child: None,
            seeded_cwd: None,
            summary: first,
        };
        let stored = roster.write(entry.clone(), Some("worker-1"), None);
        assert_eq!(stored.status, AgentRosterStatus::Running);
        assert_eq!(stored.worker_id.as_deref(), Some("worker-1"));
        assert!(roster.has("agent-1"));
        assert!(roster.by_active_session_id("active-1").is_some());
        assert!(roster
            .by_session_file(&canonical_session_path("/tmp/session.jsonl"))
            .is_some());
        assert!(roster.has_session_file(&canonical_session_path("/tmp/session.jsonl")));
        assert_eq!(roster.entries_for_worker("worker-1").len(), 1);

        // A second agent claiming the same file evicts the first.
        let mut second = summary(None, "idle");
        second.session_file = Some("/tmp/session.jsonl".to_string());
        roster.write(
            WorkerRosterEntry {
                agent_id: "agent-2".to_string(),
                queued_child: None,
                seeded_cwd: None,
                summary: second,
            },
            None,
            None,
        );
        assert!(!roster.has("agent-1"));
        assert!(roster.has("agent-2"));

        roster.amend(
            "agent-2",
            RosterEntryMarks {
                status_label: Some(Some("failed".to_string())),
                last_heard_from_at: None,
            },
        );
        assert_eq!(
            roster.get("agent-2").expect("entry").status_label.as_deref(),
            Some("failed")
        );
        roster.delete("agent-2");
        assert!(!roster.has("agent-2"));
        let recorded = mutations.lock().expect("mutations poisoned").clone();
        assert!(matches!(recorded.last(), Some(AgentRosterMutation::Delete { .. })));
    }

    #[test]
    fn queued_children_are_labelled_queued() {
        let mut roster = AgentRoster::new(
            Box::new(canonical_session_path),
            Box::new(|_| {}),
        );
        let mut entry = summary(None, "idle");
        entry.runtime_kind = Some("subagent".to_string());
        entry.rlm_child_id = Some("child".to_string());
        let stored = roster.write(
            WorkerRosterEntry {
                agent_id: "child".to_string(),
                queued_child: Some(true),
                seeded_cwd: None,
                summary: entry,
            },
            None,
            Some("failed"),
        );
        assert_eq!(stored.status_label.as_deref(), Some("queued"));
        assert_eq!(stored.status, AgentRosterStatus::Running);
    }
}
