//! Port of packages/coding-agent/src/modes/agent-connection/types.ts
//!
//! Client-side interaction boundary consumed by InteractiveMode. Types are kept
//! as plain data (serde structs/enums) so local and daemon adapters share one
//! contract. Fields that the TypeScript marks optional are `Option` and are
//! skipped when serializing, preserving absent-vs-null.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use pi_agent_core::types::{AgentEvent, AgentMessage, ThinkingLevel};
use pi_ai::types::{ImageContent, Model, ServiceTier, Transport};

/// `AgentConnectionModel = Model<Api>`.
pub type AgentConnectionModel = Model;

pub const QUEUE_MODE_ALL: &str = "all";
pub const QUEUE_MODE_ONE_AT_A_TIME: &str = "one-at-a-time";

pub type AgentConnectionQueueMode = &'static str;

pub fn queue_mode_from_str(value: &str) -> Option<AgentConnectionQueueMode> {
    match value {
        QUEUE_MODE_ALL => Some(QUEUE_MODE_ALL),
        QUEUE_MODE_ONE_AT_A_TIME => Some(QUEUE_MODE_ONE_AT_A_TIME),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionModelCatalog {
    pub models: Vec<AgentConnectionModel>,
    pub configured_providers: Vec<String>,
}

pub const SAVED_SESSION_SCOPE_CURRENT: &str = "current";
pub const SAVED_SESSION_SCOPE_ALL: &str = "all";

pub type AgentConnectionSavedSessionScope = &'static str;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSessionGit {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSessionHeader {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<f64>,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rlm_depth: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<AgentConnectionSessionGit>,
}

pub const SAVED_SESSION_STATE_ACTIVE: &str = "active";
pub const SAVED_SESSION_STATE_ARCHIVED: &str = "archived";
pub const SAVED_SESSION_STATE_CRASH: &str = "crash";

pub type AgentConnectionSavedSessionStateStatus = &'static str;

pub const SOURCE_SCOPE_USER: &str = "user";
pub const SOURCE_SCOPE_PROJECT: &str = "project";
pub const SOURCE_SCOPE_TEMPORARY: &str = "temporary";

pub type AgentConnectionSourceScope = &'static str;

pub const SOURCE_ORIGIN_PACKAGE: &str = "package";
pub const SOURCE_ORIGIN_TOP_LEVEL: &str = "top-level";

pub type AgentConnectionSourceOrigin = &'static str;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSourceInfo {
    pub path: String,
    pub source: String,
    pub scope: String,
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionResourceCollision {
    pub resource_type: String,
    pub name: String,
    pub winner_path: String,
    pub loser_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loser_source: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionResourceDiagnostic {
    #[serde(rename = "type")]
    pub type_: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collision: Option<AgentConnectionResourceCollision>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSavedSessionState {
    pub status: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionAgentStatus {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_state: Option<String>,
    pub based_on_message_count: f64,
}

/// Saved-session registry row for the current local TUI migration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSavedSessionInfo {
    pub path: String,
    pub id: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<AgentConnectionSavedSessionState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rlm_depth: Option<f64>,
    /// TypeScript `Date`; the port carries the epoch milliseconds.
    pub created: f64,
    pub modified: f64,
    pub message_count: f64,
    pub first_message: String,
    pub all_messages_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<AgentConnectionAgentStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
}

pub type AgentConnectionSessionListProgress = std::sync::Arc<dyn Fn(f64, f64) + Send + Sync>;

#[derive(Clone, Default)]
pub struct AgentConnectionSessionListCallbacks {
    pub on_progress: Option<AgentConnectionSessionListProgress>,
    pub on_session: Option<std::sync::Arc<dyn Fn(&AgentConnectionSavedSessionInfo) + Send + Sync>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSessionEntryBase {
    #[serde(rename = "type")]
    pub type_: String,
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
}

/// `AgentConnectionSessionEntry` union, discriminated on `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentConnectionSessionEntry {
    #[serde(rename = "message")]
    Message {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        message: AgentMessage,
    },
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        thinking_level: String,
    },
    #[serde(rename = "service_tier_change")]
    ServiceTierChange {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        service_tier: ServiceTier,
    },
    #[serde(rename = "model_change")]
    ModelChange {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        provider: String,
        model_id: String,
    },
    #[serde(rename = "compaction")]
    Compaction {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        summary: String,
        first_kept_entry_id: String,
        tokens_before: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
    },
    #[serde(rename = "branch_summary")]
    BranchSummary {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        from_id: String,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
    },
    #[serde(rename = "custom")]
    Custom {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        custom_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    #[serde(rename = "child_usage_attributed")]
    ChildUsageAttribution {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        target_id: String,
        child_usage: Value,
        aggregate_usage: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
    },
    #[serde(rename = "custom_message")]
    CustomMessage {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        custom_type: String,
        content: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        display: bool,
    },
    #[serde(rename = "label")]
    Label {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        target_id: String,
        label: Option<String>,
    },
    #[serde(rename = "session_info")]
    SessionInfo {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    #[serde(rename = "session_state")]
    SessionState {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        state: AgentConnectionSavedSessionState,
    },
    #[serde(rename = "agent_status")]
    AgentStatus {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        status: AgentConnectionAgentStatus,
    },
    #[serde(rename = "git_state")]
    GitState {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        git: AgentConnectionSessionGit,
    },
}

impl AgentConnectionSessionEntry {
    pub fn type_name(&self) -> &'static str {
        match self {
            AgentConnectionSessionEntry::Message { .. } => "message",
            AgentConnectionSessionEntry::ThinkingLevelChange { .. } => "thinking_level_change",
            AgentConnectionSessionEntry::ServiceTierChange { .. } => "service_tier_change",
            AgentConnectionSessionEntry::ModelChange { .. } => "model_change",
            AgentConnectionSessionEntry::Compaction { .. } => "compaction",
            AgentConnectionSessionEntry::BranchSummary { .. } => "branch_summary",
            AgentConnectionSessionEntry::Custom { .. } => "custom",
            AgentConnectionSessionEntry::ChildUsageAttribution { .. } => "child_usage_attributed",
            AgentConnectionSessionEntry::CustomMessage { .. } => "custom_message",
            AgentConnectionSessionEntry::Label { .. } => "label",
            AgentConnectionSessionEntry::SessionInfo { .. } => "session_info",
            AgentConnectionSessionEntry::SessionState { .. } => "session_state",
            AgentConnectionSessionEntry::AgentStatus { .. } => "agent_status",
            AgentConnectionSessionEntry::GitState { .. } => "git_state",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            AgentConnectionSessionEntry::Message { id, .. }
            | AgentConnectionSessionEntry::ThinkingLevelChange { id, .. }
            | AgentConnectionSessionEntry::ServiceTierChange { id, .. }
            | AgentConnectionSessionEntry::ModelChange { id, .. }
            | AgentConnectionSessionEntry::Compaction { id, .. }
            | AgentConnectionSessionEntry::BranchSummary { id, .. }
            | AgentConnectionSessionEntry::Custom { id, .. }
            | AgentConnectionSessionEntry::ChildUsageAttribution { id, .. }
            | AgentConnectionSessionEntry::CustomMessage { id, .. }
            | AgentConnectionSessionEntry::Label { id, .. }
            | AgentConnectionSessionEntry::SessionInfo { id, .. }
            | AgentConnectionSessionEntry::SessionState { id, .. }
            | AgentConnectionSessionEntry::AgentStatus { id, .. }
            | AgentConnectionSessionEntry::GitState { id, .. } => id,
        }
    }

    pub fn parent_id(&self) -> Option<&str> {
        match self {
            AgentConnectionSessionEntry::Message { parent_id, .. }
            | AgentConnectionSessionEntry::ThinkingLevelChange { parent_id, .. }
            | AgentConnectionSessionEntry::ServiceTierChange { parent_id, .. }
            | AgentConnectionSessionEntry::ModelChange { parent_id, .. }
            | AgentConnectionSessionEntry::Compaction { parent_id, .. }
            | AgentConnectionSessionEntry::BranchSummary { parent_id, .. }
            | AgentConnectionSessionEntry::Custom { parent_id, .. }
            | AgentConnectionSessionEntry::ChildUsageAttribution { parent_id, .. }
            | AgentConnectionSessionEntry::CustomMessage { parent_id, .. }
            | AgentConnectionSessionEntry::Label { parent_id, .. }
            | AgentConnectionSessionEntry::SessionInfo { parent_id, .. }
            | AgentConnectionSessionEntry::SessionState { parent_id, .. }
            | AgentConnectionSessionEntry::AgentStatus { parent_id, .. }
            | AgentConnectionSessionEntry::GitState { parent_id, .. } => parent_id.as_deref(),
        }
    }

    pub fn timestamp(&self) -> &str {
        match self {
            AgentConnectionSessionEntry::Message { timestamp, .. }
            | AgentConnectionSessionEntry::ThinkingLevelChange { timestamp, .. }
            | AgentConnectionSessionEntry::ServiceTierChange { timestamp, .. }
            | AgentConnectionSessionEntry::ModelChange { timestamp, .. }
            | AgentConnectionSessionEntry::Compaction { timestamp, .. }
            | AgentConnectionSessionEntry::BranchSummary { timestamp, .. }
            | AgentConnectionSessionEntry::Custom { timestamp, .. }
            | AgentConnectionSessionEntry::ChildUsageAttribution { timestamp, .. }
            | AgentConnectionSessionEntry::CustomMessage { timestamp, .. }
            | AgentConnectionSessionEntry::Label { timestamp, .. }
            | AgentConnectionSessionEntry::SessionInfo { timestamp, .. }
            | AgentConnectionSessionEntry::SessionState { timestamp, .. }
            | AgentConnectionSessionEntry::AgentStatus { timestamp, .. }
            | AgentConnectionSessionEntry::GitState { timestamp, .. } => timestamp,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSessionTreeFlatNode {
    pub entry: AgentConnectionSessionEntry,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSessionTreeNode {
    pub entry: AgentConnectionSessionEntry,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_timestamp: Option<String>,
    pub children: Vec<AgentConnectionSessionTreeNode>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSessionContextModel {
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub service_tier: ServiceTier,
    pub model: Option<AgentConnectionSessionContextModel>,
}

pub const REPLAY_STATUS_COMPLETE: &str = "complete";
pub const REPLAY_STATUS_PARTIAL: &str = "partial";
pub const REPLAY_STATUS_UNAVAILABLE: &str = "unavailable";

pub type AgentConnectionReplayStatus = &'static str;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionEventCursor {
    pub generation: String,
    pub sequence: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionReplayInfo {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_sequence: Option<f64>,
    pub to_sequence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_cursor: Option<AgentConnectionEventCursor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_cursor: Option<AgentConnectionEventCursor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionParentMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionHistoryWindow {
    pub version: f64,
    pub generation: String,
    /// Opaque identity of the target-model representation used to build this window.
    pub representation: String,
    pub tip_entry_id: Option<String>,
    pub total_message_count: f64,
    pub start_index: f64,
    /// Stable entry ids aligned by index with AgentConnectionSnapshot.messages.
    pub entry_ids: Vec<String>,
    pub has_older: bool,
    pub order: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionHistoryRange {
    #[serde(flatten)]
    pub window: AgentConnectionHistoryWindow,
    pub messages: Vec<AgentMessage>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionHistoryRangeRequest {
    pub generation: String,
    pub representation: String,
    pub tip_entry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_entry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSessionTree {
    pub tree: Vec<AgentConnectionSessionTreeNode>,
    pub leaf_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSnapshot {
    pub state: AgentConnectionState,
    pub messages: Vec<AgentMessage>,
    /// Present only when recent-first history was capability-negotiated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<AgentConnectionHistoryWindow>,
    /// In-flight assistant message, separate from finalized transcript messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming_message: Option<AgentMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_context: Option<AgentConnectionSessionContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_tree: Option<AgentConnectionSessionTree>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<AgentConnectionParentMetadata>,
    /// Live RLM children, including descendants, known to the host at snapshot time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<AgentConnectionRlmChildAgentSnapshot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_sequence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_cursor: Option<AgentConnectionEventCursor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay: Option<AgentConnectionReplayInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionScopedModel {
    pub model: AgentConnectionModel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionModelCycleResult {
    pub model: AgentConnectionModel,
    pub thinking_level: ThinkingLevel,
    pub service_tier: ServiceTier,
    pub is_scoped: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_session_id: Option<String>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<AgentConnectionModel>,
    pub thinking_level: ThinkingLevel,
    pub service_tier: ServiceTier,
    pub available_thinking_levels: Vec<ThinkingLevel>,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub is_bash_running: bool,
    pub retry_attempt: f64,
    pub steering_mode: String,
    pub follow_up_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_dir: Option<String>,
    pub leaf_id: Option<String>,
    pub auto_compaction_enabled: bool,
    pub message_count: f64,
    pub session_actions: Value,
    pub compaction_count: f64,
    pub goal: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat: Option<Option<Value>>,
    pub scoped_models: Vec<AgentConnectionScopedModel>,
    pub active_tool_names: Vec<String>,
    pub context_usage: Value,
    /// One-line recent-work recap for the prompt UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recap: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSlashCommand {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registered_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    pub source: String,
    pub source_info: AgentConnectionSourceInfo,
}

pub const ARTIFACT_TYPE_CONTEXT_FILE: &str = "context_file";
pub const ARTIFACT_TYPE_EXTENSION: &str = "extension";
pub const ARTIFACT_TYPE_PROMPT: &str = "prompt";
pub const ARTIFACT_TYPE_SKILL: &str = "skill";
pub const ARTIFACT_TYPE_THEME: &str = "theme";

pub type AgentConnectionArtifactType = &'static str;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionArtifactReference {
    pub id: String,
    pub session_id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub logical_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionResourceContextFile {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<AgentConnectionArtifactReference>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionResourceSkill {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub file_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_info: Option<AgentConnectionSourceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<AgentConnectionArtifactReference>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionResourcePrompt {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    pub file_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_info: Option<AgentConnectionSourceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<AgentConnectionArtifactReference>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionResourceExtension {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_info: Option<AgentConnectionSourceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<AgentConnectionArtifactReference>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionResourceTheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_info: Option<AgentConnectionSourceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<AgentConnectionArtifactReference>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionResourceDiagnostics {
    pub skills: Vec<AgentConnectionResourceDiagnostic>,
    pub prompts: Vec<AgentConnectionResourceDiagnostic>,
    pub extensions: Vec<AgentConnectionResourceDiagnostic>,
    pub themes: Vec<AgentConnectionResourceDiagnostic>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionResourceSnapshot {
    pub context_files: Vec<AgentConnectionResourceContextFile>,
    pub skills: Vec<AgentConnectionResourceSkill>,
    pub prompts: Vec<AgentConnectionResourcePrompt>,
    pub extensions: Vec<AgentConnectionResourceExtension>,
    pub themes: Vec<AgentConnectionResourceTheme>,
    pub diagnostics: AgentConnectionResourceDiagnostics,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionToolDefinition {
    pub name: String,
    pub label: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_guidelines: Option<Vec<String>>,
    pub parameters: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_shell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_built_in_tool_name: Option<String>,
}

/// `AgentConnectionPromptAdmissionError`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct AgentConnectionPromptAdmissionError {
    pub message: String,
    pub status: String,
    pub cancelled: bool,
}

impl AgentConnectionPromptAdmissionError {
    pub fn new(message: impl Into<String>, status: impl Into<String>) -> Self {
        let status = status.into();
        let cancelled = status == "cancelled";
        Self {
            message: message.into(),
            status,
            cancelled,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionPromptOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageContent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming_behavior: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_if_busy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSideQuestionEvent {
    pub id: String,
    pub question: String,
    pub answer: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSideQuestionTurn {
    pub question: String,
    pub answer: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionExecuteBashOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_from_context: Option<bool>,
    /// Side-conversation bash is not recorded into the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transient: Option<bool>,
    /// Caller-generated id echoed on the run's bash_start/bash_end events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionNewSessionOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionForkOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionSwitchSessionOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd_override: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionNavigateTreeOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summarize: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_instructions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionNavigateTreeResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_text: Option<String>,
    pub cancelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aborted: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionUserMessage {
    pub entry_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentConnectionQueueState {
    pub steering: Vec<String>,
    pub follow_up: Vec<String>,
}

pub type AgentConnectionQueuedMessageLane = String;
pub type AgentConnectionQueuedMessageMutation = Value;
/// `unsupported` means an older remote daemon lacks queued-message mutation.
pub type AgentConnectionQueuedMessageMutationStatus = String;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionHeartbeat {
    pub job: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_message: Option<String>,
}

/// `{ value: string } | { confirmed: boolean } | { cancelled: true }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentConnectionExtensionUiResponse {
    Value { value: String },
    Confirmed { confirmed: bool },
    Cancelled { cancelled: bool },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionExtensionUiRequest {
    pub id: String,
    pub method: String,
    pub payload: Value,
}

pub const CHILD_STATUS_QUEUED: &str = "queued";
pub const CHILD_STATUS_RUNNING: &str = "running";
pub const CHILD_STATUS_DONE: &str = "done";
pub const CHILD_STATUS_ERROR: &str = "error";
pub const CHILD_STATUS_CANCELLED: &str = "cancelled";

pub type AgentConnectionRlmChildAgentStatus = &'static str;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionRlmChildAgentActivity {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionRlmChildAgentSnapshot {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Child daemon active-session id, for direct attachment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_session_id: Option<String>,
    /// Stable daemon-visible child name for addressing and display.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Exact provider/model selector used by the child.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub label: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replied_since_task: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_count: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recap: Option<String>,
    pub session_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<AgentConnectionRlmChildAgentActivity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The prime-agent-specific session events the connection layer carries.
///
/// The full TypeScript union also includes every `AgentEvent` member; those are
/// carried by `AgentConnectionSessionEvent::Agent(AgentEvent)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentConnectionSessionEvent {
    #[serde(rename = "ipython_sent_agent_message")]
    IpythonSentAgentMessage {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        message: KernelSentAgentMessage,
    },
    #[serde(rename = "session_action_update")]
    SessionActionUpdate { actions: Value },
    #[serde(rename = "compaction_start")]
    CompactionStart {
        reason: String,
        #[serde(rename = "customInstructions", skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
    #[serde(rename = "session_info_changed")]
    SessionInfoChanged { name: Option<String> },
    #[serde(rename = "thinking_level_changed")]
    ThinkingLevelChanged { level: ThinkingLevel },
    #[serde(rename = "service_tier_changed")]
    ServiceTierChanged {
        #[serde(rename = "serviceTier")]
        service_tier: ServiceTier,
    },
    #[serde(rename = "compaction_end")]
    CompactionEnd {
        reason: String,
        result: Option<CompactionResultSummary>,
        aborted: bool,
        #[serde(rename = "willRetry")]
        will_retry: bool,
        #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        #[serde(rename = "errorSeverity", skip_serializing_if = "Option::is_none")]
        error_severity: Option<String>,
        #[serde(rename = "customInstructions", skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
    #[serde(rename = "auto_retry_start")]
    AutoRetryStart {
        attempt: f64,
        #[serde(rename = "maxAttempts")]
        max_attempts: f64,
        #[serde(rename = "delayMs")]
        delay_ms: f64,
        #[serde(rename = "errorMessage")]
        error_message: String,
    },
    #[serde(rename = "auto_retry_end")]
    AutoRetryEnd {
        success: bool,
        attempt: f64,
        #[serde(rename = "finalError", skip_serializing_if = "Option::is_none")]
        final_error: Option<String>,
    },
    #[serde(rename = "auth_stale")]
    AuthStale {
        provider: String,
        #[serde(rename = "sourceTokens", skip_serializing_if = "Option::is_none")]
        source_tokens: Option<Vec<Value>>,
    },
    #[serde(rename = "rlm_child_update")]
    RlmChildUpdate {
        child: AgentConnectionRlmChildAgentSnapshot,
    },
    #[serde(rename = "recap_update")]
    RecapUpdate { recap: Option<String> },
    #[serde(rename = "goal_update")]
    GoalUpdate { goal: GoalState },
    #[serde(rename = "bash_start")]
    BashStart {
        command: String,
        #[serde(rename = "excludeFromContext")]
        exclude_from_context: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        transient: Option<bool>,
        #[serde(rename = "runId", skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
    },
    #[serde(rename = "bash_output")]
    BashOutput { chunk: String },
    #[serde(rename = "bash_end")]
    BashEnd {
        #[serde(rename = "exitCode")]
        exit_code: Option<i64>,
        cancelled: bool,
        truncated: bool,
        #[serde(rename = "fullOutputPath", skip_serializing_if = "Option::is_none")]
        full_output_path: Option<String>,
        #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        transient: Option<bool>,
        #[serde(rename = "runId", skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
    },
    #[serde(rename = "refine_complete")]
    RefineComplete { result: RefinementResultSummary },
    #[serde(rename = "refine_failed")]
    RefineFailed { error: String },
    /// Any plain `AgentEvent` member of the union.
    #[serde(rename = "agent_event")]
    Agent(AgentEvent),
    /// `message_start` / `message_update` / `message_end` as emitted by the session.
    #[serde(rename = "message_start")]
    MessageStart { message: AgentMessage },
    #[serde(rename = "message_update")]
    MessageUpdate {
        message: AgentMessage,
        #[serde(rename = "assistantMessageEvent")]
        assistant_message_event: pi_ai::types::AssistantMessageEvent,
    },
    #[serde(rename = "message_end")]
    MessageEnd { message: AgentMessage },
    #[serde(rename = "tool_execution_start")]
    ToolExecutionStart {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        args: Value,
    },
    #[serde(rename = "tool_execution_end")]
    ToolExecutionEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        result: Value,
        #[serde(rename = "isError")]
        is_error: bool,
    },
}

impl AgentConnectionSessionEvent {
    pub fn type_name(&self) -> &'static str {
        match self {
            AgentConnectionSessionEvent::IpythonSentAgentMessage { .. } => "ipython_sent_agent_message",
            AgentConnectionSessionEvent::SessionActionUpdate { .. } => "session_action_update",
            AgentConnectionSessionEvent::CompactionStart { .. } => "compaction_start",
            AgentConnectionSessionEvent::SessionInfoChanged { .. } => "session_info_changed",
            AgentConnectionSessionEvent::ThinkingLevelChanged { .. } => "thinking_level_changed",
            AgentConnectionSessionEvent::ServiceTierChanged { .. } => "service_tier_changed",
            AgentConnectionSessionEvent::CompactionEnd { .. } => "compaction_end",
            AgentConnectionSessionEvent::AutoRetryStart { .. } => "auto_retry_start",
            AgentConnectionSessionEvent::AutoRetryEnd { .. } => "auto_retry_end",
            AgentConnectionSessionEvent::AuthStale { .. } => "auth_stale",
            AgentConnectionSessionEvent::RlmChildUpdate { .. } => "rlm_child_update",
            AgentConnectionSessionEvent::RecapUpdate { .. } => "recap_update",
            AgentConnectionSessionEvent::GoalUpdate { .. } => "goal_update",
            AgentConnectionSessionEvent::BashStart { .. } => "bash_start",
            AgentConnectionSessionEvent::BashOutput { .. } => "bash_output",
            AgentConnectionSessionEvent::BashEnd { .. } => "bash_end",
            AgentConnectionSessionEvent::RefineComplete { .. } => "refine_complete",
            AgentConnectionSessionEvent::RefineFailed { .. } => "refine_failed",
            AgentConnectionSessionEvent::Agent(event) => event.type_name(),
            AgentConnectionSessionEvent::MessageStart { .. } => "message_start",
            AgentConnectionSessionEvent::MessageUpdate { .. } => "message_update",
            AgentConnectionSessionEvent::MessageEnd { .. } => "message_end",
            AgentConnectionSessionEvent::ToolExecutionStart { .. } => "tool_execution_start",
            AgentConnectionSessionEvent::ToolExecutionEnd { .. } => "tool_execution_end",
        }
    }

    /// Role of the message this event carries, when it carries one.
    pub fn message_role(&self) -> Option<&str> {
        match self {
            AgentConnectionSessionEvent::MessageStart { message }
            | AgentConnectionSessionEvent::MessageUpdate { message, .. }
            | AgentConnectionSessionEvent::MessageEnd { message } => Some(message.role()),
            AgentConnectionSessionEvent::Agent(event) => match event {
                AgentEvent::MessageStart { message }
                | AgentEvent::MessageUpdate { message, .. }
                | AgentEvent::MessageEnd { message } => Some(message.role()),
                _ => None,
            },
            _ => None,
        }
    }
}

/// `CompactionResult` fields the connection layer reads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResultSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_before: Option<f64>,
}

/// `RefinementResult` fields the connection layer reads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefinementResultSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_edits: Option<Vec<RefinementAppliedEditSummary>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefinementAppliedEditSummary {
    pub action: String,
    pub kind: String,
    pub id: String,
    pub applied: bool,
}

/// `GoalState` fields the connection layer reads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalState {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<f64>,
}

/// `KernelSentAgentMessage` fields the connection layer reads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KernelSentAgentMessage {
    pub delivery_status: String,
    pub target: KernelSentAgentMessageTarget,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KernelSentAgentMessageTarget {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
}

/// `AgentConnectionEvent` union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentConnectionEvent {
    #[serde(rename = "session_event")]
    SessionEvent { event: AgentConnectionSessionEvent },
    #[serde(rename = "side_question_event")]
    SideQuestionEvent { event: AgentConnectionSideQuestionEvent },
    #[serde(rename = "session_replaced")]
    SessionReplaced {
        state: AgentConnectionState,
        messages: Vec<AgentMessage>,
    },
    #[serde(rename = "session_resynced")]
    SessionResynced { snapshot: AgentConnectionSnapshot },
    #[serde(rename = "session_status")]
    SessionStatus {
        #[serde(skip_serializing_if = "Option::is_none")]
        recap: Option<String>,
    },
    #[serde(rename = "extension_ui_request")]
    ExtensionUiRequest { request: AgentConnectionExtensionUiRequest },
    #[serde(rename = "extension_error")]
    ExtensionError {
        #[serde(rename = "extensionPath")]
        extension_path: String,
        event: String,
        error: String,
    },
    #[serde(rename = "connection_status")]
    ConnectionStatus {
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "heartbeats_changed")]
    HeartbeatsChanged,
    #[serde(rename = "closed")]
    Closed {
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl AgentConnectionEvent {
    pub fn type_name(&self) -> &'static str {
        match self {
            AgentConnectionEvent::SessionEvent { .. } => "session_event",
            AgentConnectionEvent::SideQuestionEvent { .. } => "side_question_event",
            AgentConnectionEvent::SessionReplaced { .. } => "session_replaced",
            AgentConnectionEvent::SessionResynced { .. } => "session_resynced",
            AgentConnectionEvent::SessionStatus { .. } => "session_status",
            AgentConnectionEvent::ExtensionUiRequest { .. } => "extension_ui_request",
            AgentConnectionEvent::ExtensionError { .. } => "extension_error",
            AgentConnectionEvent::ConnectionStatus { .. } => "connection_status",
            AgentConnectionEvent::HeartbeatsChanged => "heartbeats_changed",
            AgentConnectionEvent::Closed { .. } => "closed",
        }
    }
}

pub type AgentConnectionEventListener =
    std::sync::Arc<dyn Fn(AgentConnectionEvent) -> pi_ai::types::BoxFuture<()> + Send + Sync>;

pub type AgentConnectionBeforeSessionInvalidateListener = std::sync::Arc<dyn Fn() + Send + Sync>;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionHeadlessCompletionOptions {
    /// Wait for descendant terminal publication and the parent turns it triggers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_for_rlm_quiescence: Option<bool>,
}

/// `AgentConnectionSessionInputPause.release()`.
pub type AgentConnectionSessionInputPause = std::sync::Arc<dyn AgentConnectionInputPause + Send + Sync>;

pub trait AgentConnectionInputPause {
    fn release(&self) -> pi_ai::types::BoxFuture<Result<(), String>>;
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnectionWatchSessionTree {
    pub tree: Vec<AgentConnectionSessionTreeNode>,
    pub leaf_id: Option<String>,
}

/// `AgentConnectionSessionWatcher`; a trait because each adapter builds one.
pub trait AgentConnectionSessionWatcher: Send + Sync {
    fn get_messages(&self) -> pi_ai::types::BoxFuture<Vec<AgentMessage>>;
    fn get_commands(&self) -> pi_ai::types::BoxFuture<Vec<AgentConnectionSlashCommand>>;
    fn subscribe(&self, listener: AgentConnectionEventListener) -> Box<dyn Fn() + Send + Sync>;
    fn get_tool_definition(
        &self,
        name: &str,
    ) -> pi_ai::types::BoxFuture<Option<AgentConnectionToolDefinition>>;
    fn close(&self) -> pi_ai::types::BoxFuture<()>;
}

/// `AgentConnection` interface. Every method that may fail returns `Result`.
///
/// The trait keeps the TypeScript method names in snake_case and the same
/// ordering. Optional members of the TypeScript interface (`getHistoryRange`,
/// `subscribeAgentRoster`, the ACP MCP hooks) are default-implemented to mirror
/// "method may be absent".
pub trait AgentConnection: Send + Sync {
    fn subscribe(&self, listener: AgentConnectionEventListener) -> Box<dyn Fn() + Send + Sync>;
    fn on_before_session_invalidate(
        &self,
        listener: AgentConnectionBeforeSessionInvalidateListener,
    ) -> Box<dyn Fn() + Send + Sync>;

    fn get_state(&self) -> pi_ai::types::BoxFuture<Result<AgentConnectionState, String>>;
    fn get_initial_snapshot(&self) -> pi_ai::types::BoxFuture<Result<AgentConnectionSnapshot, String>>;
    fn get_rlm_child_snapshots(&self) -> pi_ai::types::BoxFuture<Result<Vec<AgentConnectionRlmChildAgentSnapshot>, String>>;
    fn get_messages(&self) -> pi_ai::types::BoxFuture<Result<Vec<AgentMessage>, String>>;
    fn get_history_range(
        &self,
        request: AgentConnectionHistoryRangeRequest,
    ) -> pi_ai::types::BoxFuture<Result<AgentConnectionHistoryRange, String>> {
        let _ = request;
        Box::pin(async { Err("getHistoryRange is not supported by this connection".to_string()) })
    }
    fn get_session_header(&self) -> pi_ai::types::BoxFuture<Result<Option<AgentConnectionSessionHeader>, String>>;
    fn get_commands(&self) -> pi_ai::types::BoxFuture<Result<Vec<AgentConnectionSlashCommand>, String>>;
    fn get_resource_snapshot(&self) -> pi_ai::types::BoxFuture<Result<AgentConnectionResourceSnapshot, String>>;
    fn get_model_catalog(&self) -> pi_ai::types::BoxFuture<Result<AgentConnectionModelCatalog, String>>;
    fn get_available_models(&self) -> pi_ai::types::BoxFuture<Result<Vec<AgentConnectionModel>, String>>;
    fn get_session_stats(&self) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn get_context_tree(&self) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn get_session_context(&self) -> pi_ai::types::BoxFuture<Result<AgentConnectionSessionContext, String>>;
    fn get_session_tree(&self) -> pi_ai::types::BoxFuture<Result<AgentConnectionWatchSessionTree, String>>;
    fn list_saved_sessions(
        &self,
        scope: &str,
    ) -> pi_ai::types::BoxFuture<Result<Vec<AgentConnectionSavedSessionInfo>, String>>;
    fn get_queue(&self) -> pi_ai::types::BoxFuture<Result<AgentConnectionQueueState, String>>;
    fn mutate_queued_message(
        &self,
        lane: &str,
        index: i64,
        expected_text: &str,
        mutation: Value,
    ) -> pi_ai::types::BoxFuture<Result<String, String>>;
    fn clear_queue(&self) -> pi_ai::types::BoxFuture<Result<AgentConnectionQueueState, String>>;
    fn abort_and_clear_queue(&self) -> pi_ai::types::BoxFuture<Result<AgentConnectionQueueState, String>>;
    fn acquire_session_input_pause(
        &self,
        lease_key: &str,
    ) -> pi_ai::types::BoxFuture<Result<AgentConnectionSessionInputPause, String>>;
    fn list_cron_jobs(&self, include_inactive: bool) -> pi_ai::types::BoxFuture<Result<Vec<Value>, String>>;
    fn list_heartbeats(&self) -> pi_ai::types::BoxFuture<Result<Vec<AgentConnectionHeartbeat>, String>>;
    fn manage_heartbeat(
        &self,
        active_session_id: &str,
        job_id: &str,
        action: Value,
    ) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn add_cron_job(&self, schedule: &str, prompt: &str) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn cancel_cron_job(&self, job_id: &str) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn get_heartbeat(&self) -> pi_ai::types::BoxFuture<Result<Option<Value>, String>>;
    fn set_heartbeat(
        &self,
        schedule: &str,
        instruction: &str,
        delivery_mode: Option<&str>,
    ) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn update_heartbeat(&self, action: Value) -> pi_ai::types::BoxFuture<Result<Option<Value>, String>>;
    fn send_agent_message(
        &self,
        target_active_session_id: &str,
        message: &str,
    ) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn get_agent_message_status(&self) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn pause_agent_messages(&self) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn resume_agent_messages(&self) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn clear_agent_messages(&self) -> pi_ai::types::BoxFuture<Result<f64, String>>;
    fn get_user_messages_for_forking(&self) -> pi_ai::types::BoxFuture<Result<Vec<AgentConnectionUserMessage>, String>>;
    fn get_last_assistant_text(&self) -> pi_ai::types::BoxFuture<Result<Option<String>, String>>;
    fn get_system_prompt(&self) -> pi_ai::types::BoxFuture<Result<String, String>>;
    fn get_tool_definition(
        &self,
        name: &str,
    ) -> pi_ai::types::BoxFuture<Result<Option<AgentConnectionToolDefinition>, String>>;
    fn set_session_entry_label(&self, entry_id: &str, label: Option<&str>) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn respond_to_extension_ui_request(
        &self,
        request_id: &str,
        response: AgentConnectionExtensionUiResponse,
    ) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn subscribe_agent_roster(
        &self,
        listener: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> pi_ai::types::BoxFuture<Result<(), String>> {
        let _ = listener;
        Box::pin(async { Err("subscribeAgentRoster is not supported by this connection".to_string()) })
    }
    fn supports_acp_mcp_servers(&self) -> bool {
        false
    }
    fn replace_acp_mcp_servers(
        &self,
        servers: Vec<Value>,
        owner_id: &str,
    ) -> pi_ai::types::BoxFuture<Result<(), String>> {
        let _ = (servers, owner_id);
        Box::pin(async { Err("replaceAcpMcpServers is not supported by this connection".to_string()) })
    }
    fn release_acp_mcp_servers(
        &self,
        owner_id: &str,
        server_names: Vec<String>,
    ) -> pi_ai::types::BoxFuture<Result<(), String>> {
        let _ = (owner_id, server_names);
        Box::pin(async { Err("releaseAcpMcpServers is not supported by this connection".to_string()) })
    }

    fn prompt(&self, message: &str, options: Option<AgentConnectionPromptOptions>) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn prompt_and_wait(
        &self,
        message: &str,
        options: Option<AgentConnectionPromptOptions>,
    ) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn start_side_question(
        &self,
        id: &str,
        question: &str,
        previous_turns: Option<Vec<AgentConnectionSideQuestionTurn>>,
    ) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn abort_side_question(&self, id: &str) -> pi_ai::types::BoxFuture<Result<bool, String>>;
    fn steer(&self, message: &str, images: Option<Vec<ImageContent>>) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn follow_up(&self, message: &str, images: Option<Vec<ImageContent>>) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn abort(&self) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn cancel_rlm_child(&self, child_id: &str) -> pi_ai::types::BoxFuture<Result<bool, String>>;
    fn wait_for_idle(&self) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn wait_for_headless_completion(
        &self,
        options: Option<AgentConnectionHeadlessCompletionOptions>,
    ) -> pi_ai::types::BoxFuture<Result<AgentAutonomousStatus, String>>;

    fn execute_bash(
        &self,
        command: &str,
        options: Option<AgentConnectionExecuteBashOptions>,
    ) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn execute_bash_and_wait(&self, command: &str) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn abort_bash(&self) -> pi_ai::types::BoxFuture<Result<(), String>>;

    fn set_model(&self, provider: &str, model_id: &str) -> pi_ai::types::BoxFuture<Result<AgentConnectionModel, String>>;
    fn cycle_model(
        &self,
        direction: Option<&str>,
    ) -> pi_ai::types::BoxFuture<Result<Option<AgentConnectionModelCycleResult>, String>>;
    fn set_scoped_models(&self, scoped_models: Vec<AgentConnectionScopedModel>) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn set_thinking_level(&self, level: ThinkingLevel) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn set_service_tier(&self, service_tier: ServiceTier) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn cycle_thinking_level(&self) -> pi_ai::types::BoxFuture<Result<Option<ThinkingLevel>, String>>;
    fn set_transport(&self, transport: Transport) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn set_steering_mode(&self, mode: &str) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn set_follow_up_mode(&self, mode: &str) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn set_auto_compaction_enabled(&self, enabled: bool) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn set_auto_retry_enabled(&self, enabled: bool) -> pi_ai::types::BoxFuture<Result<(), String>>;

    fn compact(&self, custom_instructions: Option<&str>) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn refine(&self, options: Value) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn abort_compaction(&self) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn abort_branch_summary(&self) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn abort_retry(&self) -> pi_ai::types::BoxFuture<Result<(), String>>;

    fn reload(&self) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn new_session(
        &self,
        options: Option<AgentConnectionNewSessionOptions>,
    ) -> pi_ai::types::BoxFuture<Result<bool, String>>;
    fn switch_session(
        &self,
        session_path: &str,
        options: Option<AgentConnectionSwitchSessionOptions>,
    ) -> pi_ai::types::BoxFuture<Result<bool, String>>;
    fn fork(
        &self,
        entry_id: &str,
        options: Option<AgentConnectionForkOptions>,
    ) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn navigate_tree(
        &self,
        target_id: &str,
        options: Option<AgentConnectionNavigateTreeOptions>,
    ) -> pi_ai::types::BoxFuture<Result<AgentConnectionNavigateTreeResult, String>>;
    fn import_from_jsonl(
        &self,
        input_path: &str,
        cwd_override: Option<&str>,
    ) -> pi_ai::types::BoxFuture<Result<bool, String>>;
    fn export_to_html(&self, output_path: Option<&str>) -> pi_ai::types::BoxFuture<Result<String, String>>;
    fn export_to_jsonl(&self, output_path: Option<&str>) -> pi_ai::types::BoxFuture<Result<String, String>>;
    fn set_session_name(&self, name: &str) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn get_rlm_max_depth_status(&self) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn set_rlm_max_depth(&self, max_depth: f64, options: Option<Value>) -> pi_ai::types::BoxFuture<Result<Value, String>>;
    fn rename_saved_session(&self, session_path: &str, name: &str) -> pi_ai::types::BoxFuture<Result<(), String>>;
    fn delete_saved_session(&self, session_path: &str) -> pi_ai::types::BoxFuture<Result<Value, String>>;

    /// Read-only live-session watcher; unavailable transports return `None`.
    fn watch_session(
        &self,
        active_session_id: &str,
    ) -> pi_ai::types::BoxFuture<Result<Option<Box<dyn AgentConnectionSessionWatcher>>, String>>;

    fn dispose(&self) -> pi_ai::types::BoxFuture<Result<(), String>>;
}

/// Autonomous status the connection returns from `waitForHeadlessCompletion`.
///
/// The owning slice (core/autonomous.ts) has not landed; this minimal shape
/// carries the fields this slice reads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAutonomousStatus {
    pub enabled: bool,
    pub continuations_used: i64,
    pub turns_used: i64,
    pub tokens_used: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    pub limits: AgentAutonomousLimits,
    pub gates: AgentAutonomousGateStatus,
    #[serde(default)]
    pub gate_attempts: BTreeMap<String, i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_gate_failure: Option<AgentAutonomousGateFailure>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAutonomousLimits {
    pub max_continuations: i64,
    pub max_turns: i64,
    pub max_tokens: i64,
    pub timeout_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAutonomousGateStatus {
    pub commands: Vec<String>,
    pub max_retries: i64,
    pub timeout_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAutonomousGateFailure {
    pub command: String,
    pub attempt: i64,
    pub exit_text: String,
    pub output: String,
}

pub const LIMIT_MAX_CONTINUATIONS: &str = "maxContinuations";
pub const LIMIT_MAX_TURNS: &str = "maxTurns";
pub const LIMIT_MAX_TOKENS: &str = "maxTokens";
pub const LIMIT_TIMEOUT_MS: &str = "timeoutMs";

pub type AutonomousLimitReason = &'static str;

/// Port of `autonomousLimitReason(state, now)` from core/autonomous.ts.
pub fn autonomous_limit_reason(state: &AgentAutonomousStatus) -> Option<AutonomousLimitReason> {
    autonomous_limit_reason_at(state, now_ms())
}

pub fn autonomous_limit_reason_at(state: &AgentAutonomousStatus, now: i64) -> Option<AutonomousLimitReason> {
    if state.continuations_used >= state.limits.max_continuations {
        return Some(LIMIT_MAX_CONTINUATIONS);
    }
    if state.turns_used >= state.limits.max_turns {
        return Some(LIMIT_MAX_TURNS);
    }
    if state.tokens_used >= state.limits.max_tokens {
        return Some(LIMIT_MAX_TOKENS);
    }
    if let Some(started_at) = state.started_at {
        if now - started_at >= state.limits.timeout_ms {
            return Some(LIMIT_TIMEOUT_MS);
        }
    }
    None
}

/// `Date.now()`.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Ordered JSON object used for absent-vs-null-preserving payloads.
pub type JsonObject = IndexMap<String, Value>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn queue_modes_parse() {
        assert_eq!(queue_mode_from_str("all"), Some("all"));
        assert_eq!(queue_mode_from_str("one-at-a-time"), Some("one-at-a-time"));
        assert_eq!(queue_mode_from_str("bogus"), None);
    }

    #[test]
    fn prompt_admission_error_marks_cancelled() {
        let error = AgentConnectionPromptAdmissionError::new("Prompt admission was cancelled.", "cancelled");
        assert!(error.cancelled);
        assert_eq!(error.status, "cancelled");
        assert!(!AgentConnectionPromptAdmissionError::new("x", "owned").cancelled);
    }

    #[test]
    fn optional_fields_stay_absent() {
        let state = AgentConnectionState {
            session_id: "s1".to_string(),
            ..Default::default()
        };
        let value = serde_json::to_value(&state).unwrap();
        assert!(value.get("recap").is_none());
        assert!(value.get("sessionFile").is_none());
        assert_eq!(value.get("sessionId").unwrap(), &json!("s1"));
    }

    #[test]
    fn heartbeat_preserves_absent_vs_null() {
        let mut state = AgentConnectionState::default();
        assert!(serde_json::to_value(&state).unwrap().get("heartbeat").is_none());
        state.heartbeat = Some(None);
        assert!(serde_json::to_value(&state).unwrap().get("heartbeat").unwrap().is_null());
        state.heartbeat = Some(Some(json!({"jobId": "j"})));
        assert_eq!(
            serde_json::to_value(&state).unwrap().get("heartbeat").unwrap(),
            &json!({"jobId": "j"})
        );
    }

    #[test]
    fn autonomous_limit_reason_order_matches_typescript() {
        let mut status = AgentAutonomousStatus {
            limits: AgentAutonomousLimits {
                max_continuations: 3,
                max_turns: 12,
                max_tokens: 80_000,
                timeout_ms: 1000,
            },
            started_at: Some(0),
            ..Default::default()
        };
        assert_eq!(autonomous_limit_reason_at(&status, 0), None);
        status.continuations_used = 3;
        assert_eq!(autonomous_limit_reason_at(&status, 0), Some("maxContinuations"));
        status.continuations_used = 0;
        status.turns_used = 12;
        assert_eq!(autonomous_limit_reason_at(&status, 0), Some("maxTurns"));
        status.turns_used = 0;
        status.tokens_used = 80_000;
        assert_eq!(autonomous_limit_reason_at(&status, 0), Some("maxTokens"));
        status.tokens_used = 0;
        assert_eq!(autonomous_limit_reason_at(&status, 1000), Some("timeoutMs"));
    }

    #[test]
    fn session_entry_accessors_read_the_union() {
        let entry = AgentConnectionSessionEntry::Message {
            id: "e1".to_string(),
            parent_id: None,
            timestamp: "2026-01-01T00:00:00.000Z".to_string(),
            message: pi_agent_core::types::AgentMessage::Message(pi_ai::types::Message::Assistant(
                pi_ai::types::AssistantMessage::default(),
            )),
        };
        assert_eq!(entry.type_name(), "message");
        assert_eq!(entry.id(), "e1");
        assert_eq!(entry.parent_id(), None);
        assert_eq!(entry.timestamp(), "2026-01-01T00:00:00.000Z");
    }

    #[test]
    fn extension_ui_response_serialises_untagged() {
        assert_eq!(
            serde_json::to_value(AgentConnectionExtensionUiResponse::Value {
                value: "a".to_string()
            })
            .unwrap(),
            json!({"value": "a"})
        );
        assert_eq!(
            serde_json::to_value(AgentConnectionExtensionUiResponse::Cancelled { cancelled: true }).unwrap(),
            json!({"cancelled": true})
        );
    }

    #[test]
    fn connection_event_type_names_match() {
        assert_eq!(AgentConnectionEvent::HeartbeatsChanged.type_name(), "heartbeats_changed");
        assert_eq!(
            AgentConnectionEvent::Closed { error: None }.type_name(),
            "closed"
        );
    }
}
