//! Port of packages/coding-agent/src/modes/rpc/rpc-types.ts
//!
//! RPC protocol types for headless operation. Commands are sent as JSON lines on
//! stdin; responses and events are emitted as JSON lines on stdout.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::modes::agent_connection::types::{
    AgentConnectionHeartbeat, AgentConnectionModel, AgentConnectionSourceInfo,
};
use pi_agent_core::types::{AgentEvent, AgentMessage, ThinkingLevel};

// ============================================================================
// RPC Commands (stdin)
// ============================================================================

/// `RpcCommand` union, discriminated on `type`.
///
/// The TypeScript declares an inline union with an optional `id` on every arm;
/// serde uses the same `type` tag and the same field names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcCommand {
    // Prompting
    Prompt {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<Value>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        streaming_behavior: Option<String>,
    },
    Steer {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<Value>>,
    },
    FollowUp {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<Value>>,
    },
    Abort {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    NewSession {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_session: Option<String>,
    },

    // State
    GetState {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Model
    SetModel {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        provider: String,
        model_id: String,
    },
    CycleModel {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetAvailableModels {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Thinking
    SetThinkingLevel {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        level: ThinkingLevel,
    },
    CycleThinkingLevel {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Queue modes
    SetSteeringMode {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        mode: String,
    },
    SetFollowUpMode {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        mode: String,
    },

    // Compaction
    Compact {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
    Refine {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        rollback_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        global: Option<bool>,
    },
    SetAutoCompaction {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        enabled: bool,
    },

    // Retry
    SetAutoRetry {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        enabled: bool,
    },
    AbortRetry {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Bash
    Bash {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        command: String,
    },
    AbortBash {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Session
    GetSessionStats {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    ExportHtml {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_path: Option<String>,
    },
    SwitchSession {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        session_path: String,
    },
    Fork {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        entry_id: String,
    },
    Clone {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetForkMessages {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetLastAssistantText {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    SetSessionName {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
    },

    // Messages
    GetMessages {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    SendMessage {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        target_active_session_id: String,
        message: String,
    },
    AgentMessagesStatus {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    AgentMessagesPause {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    AgentMessagesResume {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    AgentMessagesClear {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    // Scheduling
    ListSchedules {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        include_inactive: Option<bool>,
    },
    AddSchedule {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        schedule: String,
        prompt: String,
    },
    CancelSchedule {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        job_id: String,
    },
    ListHeartbeats {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetHeartbeat {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    SetHeartbeat {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        schedule: String,
        prompt: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        delivery_mode: Option<String>,
    },
    UpdateHeartbeat {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        action: Value,
    },
    ManageHeartbeat {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        active_session_id: String,
        job_id: String,
        action: Value,
    },

    // Active session and subagent observation
    Observe {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        active_session_id: String,
    },
    Unobserve {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        active_session_id: String,
    },

    // Commands (available for invocation via prompt)
    GetCommands {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
}

impl RpcCommand {
    /// The TypeScript discriminant (`command.type`).
    pub fn type_name(&self) -> &'static str {
        match self {
            RpcCommand::Prompt { .. } => "prompt",
            RpcCommand::Steer { .. } => "steer",
            RpcCommand::FollowUp { .. } => "follow_up",
            RpcCommand::Abort { .. } => "abort",
            RpcCommand::NewSession { .. } => "new_session",
            RpcCommand::GetState { .. } => "get_state",
            RpcCommand::SetModel { .. } => "set_model",
            RpcCommand::CycleModel { .. } => "cycle_model",
            RpcCommand::GetAvailableModels { .. } => "get_available_models",
            RpcCommand::SetThinkingLevel { .. } => "set_thinking_level",
            RpcCommand::CycleThinkingLevel { .. } => "cycle_thinking_level",
            RpcCommand::SetSteeringMode { .. } => "set_steering_mode",
            RpcCommand::SetFollowUpMode { .. } => "set_follow_up_mode",
            RpcCommand::Compact { .. } => "compact",
            RpcCommand::Refine { .. } => "refine",
            RpcCommand::SetAutoCompaction { .. } => "set_auto_compaction",
            RpcCommand::SetAutoRetry { .. } => "set_auto_retry",
            RpcCommand::AbortRetry { .. } => "abort_retry",
            RpcCommand::Bash { .. } => "bash",
            RpcCommand::AbortBash { .. } => "abort_bash",
            RpcCommand::GetSessionStats { .. } => "get_session_stats",
            RpcCommand::ExportHtml { .. } => "export_html",
            RpcCommand::SwitchSession { .. } => "switch_session",
            RpcCommand::Fork { .. } => "fork",
            RpcCommand::Clone { .. } => "clone",
            RpcCommand::GetForkMessages { .. } => "get_fork_messages",
            RpcCommand::GetLastAssistantText { .. } => "get_last_assistant_text",
            RpcCommand::SetSessionName { .. } => "set_session_name",
            RpcCommand::GetMessages { .. } => "get_messages",
            RpcCommand::SendMessage { .. } => "send_message",
            RpcCommand::AgentMessagesStatus { .. } => "agent_messages_status",
            RpcCommand::AgentMessagesPause { .. } => "agent_messages_pause",
            RpcCommand::AgentMessagesResume { .. } => "agent_messages_resume",
            RpcCommand::AgentMessagesClear { .. } => "agent_messages_clear",
            RpcCommand::ListSchedules { .. } => "list_schedules",
            RpcCommand::AddSchedule { .. } => "add_schedule",
            RpcCommand::CancelSchedule { .. } => "cancel_schedule",
            RpcCommand::ListHeartbeats { .. } => "list_heartbeats",
            RpcCommand::GetHeartbeat { .. } => "get_heartbeat",
            RpcCommand::SetHeartbeat { .. } => "set_heartbeat",
            RpcCommand::UpdateHeartbeat { .. } => "update_heartbeat",
            RpcCommand::ManageHeartbeat { .. } => "manage_heartbeat",
            RpcCommand::Observe { .. } => "observe",
            RpcCommand::Unobserve { .. } => "unobserve",
            RpcCommand::GetCommands { .. } => "get_commands",
        }
    }

    pub fn id(&self) -> Option<&str> {
        let id = match self {
            RpcCommand::Prompt { id, .. }
            | RpcCommand::Steer { id, .. }
            | RpcCommand::FollowUp { id, .. }
            | RpcCommand::Abort { id }
            | RpcCommand::NewSession { id, .. }
            | RpcCommand::GetState { id }
            | RpcCommand::SetModel { id, .. }
            | RpcCommand::CycleModel { id }
            | RpcCommand::GetAvailableModels { id }
            | RpcCommand::SetThinkingLevel { id, .. }
            | RpcCommand::CycleThinkingLevel { id }
            | RpcCommand::SetSteeringMode { id, .. }
            | RpcCommand::SetFollowUpMode { id, .. }
            | RpcCommand::Compact { id, .. }
            | RpcCommand::Refine { id, .. }
            | RpcCommand::SetAutoCompaction { id, .. }
            | RpcCommand::SetAutoRetry { id, .. }
            | RpcCommand::AbortRetry { id }
            | RpcCommand::Bash { id, .. }
            | RpcCommand::AbortBash { id }
            | RpcCommand::GetSessionStats { id }
            | RpcCommand::ExportHtml { id, .. }
            | RpcCommand::SwitchSession { id, .. }
            | RpcCommand::Fork { id, .. }
            | RpcCommand::Clone { id }
            | RpcCommand::GetForkMessages { id }
            | RpcCommand::GetLastAssistantText { id }
            | RpcCommand::SetSessionName { id, .. }
            | RpcCommand::GetMessages { id }
            | RpcCommand::SendMessage { id, .. }
            | RpcCommand::AgentMessagesStatus { id }
            | RpcCommand::AgentMessagesPause { id }
            | RpcCommand::AgentMessagesResume { id }
            | RpcCommand::AgentMessagesClear { id }
            | RpcCommand::ListSchedules { id, .. }
            | RpcCommand::AddSchedule { id, .. }
            | RpcCommand::CancelSchedule { id, .. }
            | RpcCommand::ListHeartbeats { id }
            | RpcCommand::GetHeartbeat { id }
            | RpcCommand::SetHeartbeat { id, .. }
            | RpcCommand::UpdateHeartbeat { id, .. }
            | RpcCommand::ManageHeartbeat { id, .. }
            | RpcCommand::Observe { id, .. }
            | RpcCommand::Unobserve { id, .. }
            | RpcCommand::GetCommands { id } => id,
        };
        id.as_deref()
    }
}

// ============================================================================
// RPC Slash Command (for get_commands response)
// ============================================================================

/// A command available for invocation via prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSlashCommand {
    /// Command name (without leading slash).
    pub name: String,
    /// Human-readable description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What kind of command this is.
    pub source: String,
    /// Source metadata for the owning resource.
    pub source_info: AgentConnectionSourceInfo,
}

// ============================================================================
// RPC State
// ============================================================================

/// `RpcSessionState`.
///
/// `sessionActions` and `goal` keep the JSON shape of `SessionActionSnapshot`
/// and `GoalState` (see blocked_on: the owning slices do not expose serializers).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSessionState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<AgentConnectionModel>,
    pub thinking_level: ThinkingLevel,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub steering_mode: String,
    pub follow_up_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    pub auto_compaction_enabled: bool,
    pub message_count: f64,
    pub session_actions: Value,
    pub goal: Value,
}

// ============================================================================
// RPC Responses (stdout)
// ============================================================================

/// `RpcResponse`: `{ id?, type: "response", command, success, data? , error? }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub type_: String,
    pub command: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RpcResponse {
    /// `success(id, command, data?)`.
    pub fn success(id: Option<&str>, command: &str, data: Option<Value>) -> Self {
        Self {
            id: id.map(str::to_string),
            type_: "response".to_string(),
            command: command.to_string(),
            success: true,
            data,
            error: None,
        }
    }

    /// `error(id, command, message)`.
    pub fn error(id: Option<&str>, command: &str, message: &str) -> Self {
        Self {
            id: id.map(str::to_string),
            type_: "response".to_string(),
            command: command.to_string(),
            success: false,
            data: None,
            error: Some(message.to_string()),
        }
    }
}

// ============================================================================
// Extension UI Events (stdout)
// ============================================================================

/// `RpcExtensionUIRequest` union, discriminated on `method`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcExtensionUiRequest {
    #[serde(rename = "type")]
    pub type_: String,
    pub id: String,
    pub method: String,
    #[serde(flatten)]
    pub payload: Value,
}

// ============================================================================
// Extension UI Commands (stdin)
// ============================================================================

/// `RpcExtensionUIResponse` union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcExtensionUiResponse {
    Cancelled {
        #[serde(rename = "type")]
        type_: String,
        id: String,
        cancelled: bool,
    },
    Confirmed {
        #[serde(rename = "type")]
        type_: String,
        id: String,
        confirmed: bool,
    },
    Value {
        #[serde(rename = "type")]
        type_: String,
        id: String,
        value: String,
    },
}

impl RpcExtensionUiResponse {
    pub fn id(&self) -> &str {
        match self {
            RpcExtensionUiResponse::Cancelled { id, .. }
            | RpcExtensionUiResponse::Confirmed { id, .. }
            | RpcExtensionUiResponse::Value { id, .. } => id,
        }
    }

    pub fn type_name(&self) -> &str {
        match self {
            RpcExtensionUiResponse::Cancelled { type_, .. }
            | RpcExtensionUiResponse::Confirmed { type_, .. }
            | RpcExtensionUiResponse::Value { type_, .. } => type_,
        }
    }
}

// ============================================================================
// Helper type for extracting command types
// ============================================================================

/// `RpcCommandType`.
pub type RpcCommandType = String;

/// `RpcObservedSessionEvent` union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcObservedSessionEvent {
    ObservedSessionEvent {
        active_session_id: String,
        event: AgentEvent,
    },
    ObservedSessionClosed {
        active_session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// `{ messages: AgentMessage[] }` payload used by several responses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcMessagesPayload {
    pub messages: Vec<AgentMessage>,
}

/// `{ heartbeats: AgentConnectionHeartbeat[] }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcHeartbeatsPayload {
    pub heartbeats: Vec<AgentConnectionHeartbeat>,
}

/// `{ models: Model[] }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcModelsPayload {
    pub models: Vec<AgentConnectionModel>,
}

/// `{ commands: RpcSlashCommand[] }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcCommandsPayload {
    pub commands: Vec<RpcSlashCommand>,
}

/// `{ text: string | null }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcTextPayload {
    pub text: Option<String>,
}

/// `{ cancelled: boolean }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcCancelledPayload {
    pub cancelled: bool,
}

/// `{ text: string; cancelled: boolean }` for `fork`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcForkPayload {
    pub text: String,
    pub cancelled: bool,
}

/// `{ entryId: string; text: string }` for `get_fork_messages`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcForkMessage {
    pub entry_id: String,
    pub text: String,
}

/// `{ messages: Array<{ entryId, text }> }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcForkMessagesPayload {
    pub messages: Vec<RpcForkMessage>,
}

/// `{ cleared: number }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcClearedPayload {
    pub cleared: f64,
}

/// `{ path: string }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcPathPayload {
    pub path: String,
}

/// `{ job: AgentCronJob }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcJobPayload {
    pub job: Value,
}

/// `{ jobs: AgentCronJob[] }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcJobsPayload {
    pub jobs: Vec<Value>,
}

/// `{ heartbeat: AgentCronJob | null }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcHeartbeatPayload {
    pub heartbeat: Option<Value>,
}

/// `{ level: ThinkingLevel }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcThinkingLevelPayload {
    pub level: ThinkingLevel,
}

/// `{ model: Model; thinkingLevel: ThinkingLevel; isScoped: boolean } | null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcModelCyclePayload {
    pub model: AgentConnectionModel,
    pub thinking_level: ThinkingLevel,
    pub is_scoped: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip_with_the_type_tag() {
        let value = serde_json::json!({"id": "1", "type": "get_state"});
        let command: RpcCommand = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(command.type_name(), "get_state");
        assert_eq!(command.id(), Some("1"));
        assert_eq!(serde_json::to_value(&command).unwrap(), value);
    }

    #[test]
    fn snake_case_command_names_match_the_wire() {
        for (json, expected) in [
            (serde_json::json!({"type": "follow_up", "message": "hi"}), "follow_up"),
            (
                serde_json::json!({"type": "set_auto_compaction", "enabled": true}),
                "set_auto_compaction",
            ),
            (
                serde_json::json!({"type": "agent_messages_clear"}),
                "agent_messages_clear",
            ),
        ] {
            let command: RpcCommand = serde_json::from_value(json).unwrap();
            assert_eq!(command.type_name(), expected);
        }
    }

    #[test]
    fn optional_fields_are_omitted_from_the_wire() {
        let response = RpcResponse::success(None, "abort", None);
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"type": "response", "command": "abort", "success": true})
        );
        assert_eq!(response.type_, "response");
    }

    #[test]
    fn error_responses_carry_the_message() {
        let response = RpcResponse::error(Some("7"), "prompt", "boom");
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"id": "7", "type": "response", "command": "prompt", "success": false, "error": "boom"})
        );
    }

    #[test]
    fn extension_ui_responses_discriminate_on_their_keys() {
        let cancelled: RpcExtensionUiResponse =
            serde_json::from_value(serde_json::json!({"type": "extension_ui_response", "id": "a", "cancelled": true}))
                .unwrap();
        assert_eq!(cancelled.id(), "a");
        assert_eq!(cancelled.type_name(), "extension_ui_response");

        let value: RpcExtensionUiResponse = serde_json::from_value(
            serde_json::json!({"type": "extension_ui_response", "id": "b", "value": "x"}),
        )
        .unwrap();
        match value {
            RpcExtensionUiResponse::Value { value, .. } => assert_eq!(value, "x"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn observed_session_events_keep_their_names() {
        let event = RpcObservedSessionEvent::ObservedSessionClosed {
            active_session_id: "s1".to_string(),
            error: None,
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({"type": "observed_session_closed", "activeSessionId": "s1"})
        );
    }
}
