//! Port of packages/coding-agent/src/core/agent-observe.ts

use pi_agent_core::types::{AgentMessage, CustomAgentMessage, CustomMessageContent};
use serde_json::{Map, Value};

pub const AGENT_OBSERVE_SKILL_NAME: &str = "agent-observe";
pub const AGENT_OBSERVE_IMPORT_NAME: &str = "agent_observe";
pub const ORCHESTRATION_HEARTBEAT_SKILL_NAME: &str = "orchestration-heartbeat";

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentObserveAgentSummary {
    pub active_session_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Compatibility alias for observation clients that predate sessionName.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_kind: Option<String>,
    pub cwd: String,
    pub status: String,
    pub is_current: bool,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub attached_clients: f64,
    /// Active model-context message count.
    pub message_count: f64,
    /// Lifetime JSONL entry count, kept distinct from active model context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_entry_count: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<Option<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_queued: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_state: Option<String>,
    pub queued_count: f64,
    pub is_session_active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_active_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rlm_child_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rlm_parent_node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_message: Option<AgentObserveMessagePreview>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentObserveListResult {
    pub current: AgentObserveAgentSummary,
    pub agents: Vec<AgentObserveAgentSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentObserveAgentSnapshot {
    pub agent: AgentObserveAgentSummary,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentObserveRecentMessagesInput {
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentObserveRecentMessagesResult {
    pub agent: AgentObserveAgentSummary,
    pub messages: Vec<AgentObserveMessagePreview>,
    pub limit: f64,
    pub max_chars: f64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentObserveMessagePreview {
    pub index: f64,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<f64>,
    pub text: String,
    /// Compatibility alias; always byte-identical to text.
    pub content: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_type: Option<String>,
}

/// `AgentObserveController`: async because the TypeScript members may return promises.
pub trait AgentObserveController: Send + Sync {
    fn list_agents(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AgentObserveListResult, String>> + Send>>;
    fn get_agent(
        &self,
        target: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AgentObserveAgentSnapshot, String>> + Send>>;
    fn recent_messages(
        &self,
        input: AgentObserveRecentMessagesInput,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AgentObserveRecentMessagesResult, String>> + Send>,
    >;
}

pub fn create_agent_observe_host_handlers(
    controller: std::sync::Arc<dyn AgentObserveController>,
) -> crate::core::kernel::shared::HostRequestHandlers {
    use crate::core::kernel::shared::{HostRequestHandler, KernelError};
    use std::collections::HashMap;
    use std::sync::Arc;

    let mut handlers: crate::core::kernel::shared::HostRequestHandlers = HashMap::new();

    let list_controller = controller.clone();
    let list: HostRequestHandler = Arc::new(move |_payload: Value| {
        let controller = list_controller.clone();
        Box::pin(async move {
            match controller.list_agents().await {
                Ok(result) => Ok(serde_json::to_value(result).unwrap_or(Value::Null)),
                Err(error) => Err(KernelError::new(error)),
            }
        })
    });
    handlers.insert("agent_observe.list".to_string(), list);

    let get_controller = controller.clone();
    let get: HostRequestHandler = Arc::new(move |payload: Value| {
        let controller = get_controller.clone();
        Box::pin(async move {
            let object = payload.as_object().cloned().unwrap_or_default();
            let Some(Value::String(target)) = object.get("target") else {
                return Err(KernelError::new("agent_observe.get target must be a string"));
            };
            match controller.get_agent(target.clone()).await {
                Ok(result) => Ok(serde_json::to_value(result).unwrap_or(Value::Null)),
                Err(error) => Err(KernelError::new(error)),
            }
        })
    });
    handlers.insert("agent_observe.get".to_string(), get);

    let recent_controller = controller.clone();
    let recent: HostRequestHandler = Arc::new(move |payload: Value| {
        let controller = recent_controller.clone();
        Box::pin(async move {
            let object = payload.as_object().cloned().unwrap_or_default();
            let Some(Value::String(target)) = object.get("target") else {
                return Err(KernelError::new("agent_observe.recent target must be a string"));
            };
            let limit = match normalize_optional_integer(object.get("limit"), "agent_observe.recent limit") {
                Ok(limit) => limit,
                Err(error) => return Err(KernelError::new(error)),
            };
            let max_chars = match normalize_optional_integer(
                object.get("max_chars").or_else(|| object.get("maxChars")),
                "agent_observe.recent max_chars",
            ) {
                Ok(max_chars) => max_chars,
                Err(error) => return Err(KernelError::new(error)),
            };
            match controller
                .recent_messages(AgentObserveRecentMessagesInput {
                    target: target.clone(),
                    limit,
                    max_chars,
                })
                .await
            {
                Ok(result) => Ok(serde_json::to_value(result).unwrap_or(Value::Null)),
                Err(error) => Err(KernelError::new(error)),
            }
        })
    });
    handlers.insert("agent_observe.recent".to_string(), recent);

    handlers
}

pub fn normalize_observe_limit(limit: Option<i64>, default_limit: i64) -> Result<i64, String> {
    clamp_integer(limit.unwrap_or(default_limit), 1, 50, "agent_observe limit")
}

pub fn normalize_observe_max_chars(max_chars: Option<i64>, default_max_chars: i64) -> Result<i64, String> {
    clamp_integer(max_chars.unwrap_or(default_max_chars), 80, 2_000, "agent_observe max_chars")
}

pub fn create_agent_observe_message_preview(
    message: &AgentMessage,
    index: f64,
    max_chars: usize,
) -> AgentObserveMessagePreview {
    let text = message_text(message);
    let (clipped_text, truncated) = truncate(&text, max_chars);
    let tool_calls = match message {
        AgentMessage::Message(pi_ai::types::Message::Assistant(_)) => Some(assistant_tool_calls(message)),
        _ => None,
    };
    let custom_type = match message {
        AgentMessage::Custom(CustomAgentMessage::Custom { custom_type, .. }) => Some(custom_type.clone()),
        _ => None,
    };
    AgentObserveMessagePreview {
        index,
        role: message.role().to_string(),
        timestamp: message_timestamp(message).map(|value| value as f64),
        text: clipped_text.clone(),
        content: clipped_text,
        truncated,
        tool_calls: tool_calls.filter(|calls| !calls.is_empty()),
        custom_type,
    }
}

/// `message.timestamp` for the union; custom messages always carry one.
fn message_timestamp(message: &AgentMessage) -> Option<i64> {
    match message {
        AgentMessage::Message(pi_ai::types::Message::User(user)) => Some(user.timestamp),
        AgentMessage::Message(pi_ai::types::Message::Assistant(assistant)) => Some(assistant.timestamp),
        AgentMessage::Message(pi_ai::types::Message::ToolResult(tool_result)) => Some(tool_result.timestamp),
        AgentMessage::Custom(CustomAgentMessage::BashExecution { timestamp, .. })
        | AgentMessage::Custom(CustomAgentMessage::Custom { timestamp, .. })
        | AgentMessage::Custom(CustomAgentMessage::BranchSummary { timestamp, .. })
        | AgentMessage::Custom(CustomAgentMessage::CompactionSummary { timestamp, .. }) => Some(*timestamp),
    }
}

fn normalize_optional_integer(value: Option<&Value>, label: &str) -> Result<Option<i64>, String> {
    match value {
        None => Ok(None),
        Some(Value::Number(number)) => match number.as_i64() {
            Some(value) => Ok(Some(value)),
            None => Err(format!("{label} must be an integer when provided")),
        },
        Some(_) => Err(format!("{label} must be an integer when provided")),
    }
}

fn clamp_integer(value: i64, min: i64, max: i64, label: &str) -> Result<i64, String> {
    if value < min || value > max {
        return Err(format!("{label} must be between {min} and {max}"));
    }
    Ok(value)
}

fn truncate(text: &str, max_chars: usize) -> (String, bool) {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return (text.to_string(), false);
    }
    (chars[..max_chars].iter().collect(), true)
}

fn message_text(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Message(pi_ai::types::Message::User(user)) => user.content.text(),
        AgentMessage::Message(pi_ai::types::Message::Assistant(assistant)) => {
            content_text_assistant(assistant)
        }
        AgentMessage::Message(pi_ai::types::Message::ToolResult(tool_result)) => {
            content_text_tool_result(tool_result)
        }
        AgentMessage::Custom(CustomAgentMessage::BashExecution {
            command, output, ..
        }) => [command.as_str(), output.as_str()]
            .iter()
            .filter(|part| !part.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        AgentMessage::Custom(CustomAgentMessage::Custom { content, .. }) => match content {
            CustomMessageContent::Text(text) => text.clone(),
            CustomMessageContent::Blocks(blocks) => content_text_blocks(blocks),
        },
        AgentMessage::Custom(CustomAgentMessage::BranchSummary { summary, .. }) => summary.clone(),
        AgentMessage::Custom(CustomAgentMessage::CompactionSummary { summary, .. }) => summary.clone(),
    }
}

fn content_text_blocks(blocks: &[pi_agent_core::types::ContentBlock]) -> String {
    blocks
        .iter()
        .map(|block| match block {
            pi_agent_core::types::ContentBlock::Text(text) => text.text.clone(),
            pi_agent_core::types::ContentBlock::Image(_) => "[image]".to_string(),
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn content_text_assistant(assistant: &pi_ai::types::AssistantMessage) -> String {
    assistant
        .content
        .iter()
        .map(|block| match block {
            pi_ai::types::ContentBlock::Text(text) => text.text.clone(),
            pi_ai::types::ContentBlock::Thinking(thinking) => thinking.thinking.clone(),
            pi_ai::types::ContentBlock::ToolCall(tool_call) => format!("[tool_call:{}]", tool_call.name),
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn content_text_tool_result(tool_result: &pi_ai::types::ToolResultMessage) -> String {
    tool_result
        .content
        .iter()
        .map(|block| match block {
            pi_ai::types::ImageOrTextContent::Text(text) => text.text.clone(),
            pi_ai::types::ImageOrTextContent::Image(_) => "[image]".to_string(),
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn assistant_tool_calls(message: &AgentMessage) -> Vec<String> {
    let AgentMessage::Message(pi_ai::types::Message::Assistant(assistant)) = message else {
        return Vec::new();
    };
    assistant
        .content
        .iter()
        .filter_map(|block| match block {
            pi_ai::types::ContentBlock::ToolCall(tool_call) => Some(tool_call.name.clone()),
            _ => None,
        })
        .collect()
}

/// Re-exported so callers can build the observation envelope without pulling in
/// the kernel types themselves.
pub fn agent_observe_result_to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Unused-import guard for `Map` (kept for JSON object construction parity).
#[allow(dead_code)]
fn empty_object() -> Map<String, Value> {
    Map::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{
        AssistantMessage, ContentBlock, ImageOrTextContent, TextContent, ToolCall, UserContent, UserMessage,
    };
    use serde_json::json;

    fn assistant(name: &str, text: &str) -> AgentMessage {
        AgentMessage::Message(pi_ai::types::Message::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::Text(TextContent::new(text)),
                ContentBlock::ToolCall(ToolCall::new("id", name, serde_json::Map::new())),
            ],
            timestamp: 42,
            ..Default::default()
        }))
    }

    #[test]
    fn preview_includes_tool_calls_for_assistant_messages() {
        let preview = create_agent_observe_message_preview(&assistant("bash", "hello"), 1.0, 100);
        assert_eq!(preview.role, "assistant");
        assert_eq!(preview.text, "hello\n[tool_call:bash]");
        assert_eq!(preview.tool_calls, Some(vec!["bash".to_string()]));
        assert_eq!(preview.timestamp, Some(42.0));
        assert!(!preview.truncated);
        assert!(preview.custom_type.is_none());
    }

    #[test]
    fn preview_truncates_and_marks_the_flag() {
        let preview = create_agent_observe_message_preview(&assistant("bash", "abcdef"), 0.0, 3);
        assert_eq!(preview.text, "abc");
        assert_eq!(preview.content, "abc");
        assert!(preview.truncated);
    }

    #[test]
    fn custom_messages_report_the_custom_type() {
        let message = AgentMessage::Custom(CustomAgentMessage::Custom {
            custom_type: "note".to_string(),
            content: CustomMessageContent::Text("body".to_string()),
            display: true,
            details: None,
            timestamp: 7,
        });
        let preview = create_agent_observe_message_preview(&message, 2.0, 100);
        assert_eq!(preview.role, "custom");
        assert_eq!(preview.custom_type.as_deref(), Some("note"));
        assert_eq!(preview.text, "body");
        assert_eq!(preview.timestamp, Some(7.0));
    }

    #[test]
    fn bash_execution_text_joins_command_and_output() {
        let message = AgentMessage::Custom(CustomAgentMessage::BashExecution {
            command: "ls".to_string(),
            output: "file".to_string(),
            exit_code: Some(0),
            cancelled: false,
            truncated: false,
            full_output_path: None,
            timestamp: 1,
            exclude_from_context: None,
        });
        assert_eq!(message_text(&message), "ls\nfile");
    }

    #[test]
    fn tool_result_text_renders_images_as_markers() {
        let message = AgentMessage::Message(pi_ai::types::Message::ToolResult(
            pi_ai::types::ToolResultMessage::new(
                "id",
                "read",
                vec![
                    ImageOrTextContent::Text(TextContent::new("text")),
                    ImageOrTextContent::Image(pi_ai::types::ImageContent::new("data", "image/png")),
                ],
                false,
                3,
            ),
        ));
        assert_eq!(message_text(&message), "text\n[image]");
    }

    #[test]
    fn limits_clamp_to_the_typescript_ranges() {
        assert_eq!(normalize_observe_limit(None, 8).unwrap(), 8);
        assert_eq!(normalize_observe_limit(Some(1), 8).unwrap(), 1);
        assert_eq!(
            normalize_observe_limit(Some(0), 8).unwrap_err(),
            "agent_observe limit must be between 1 and 50"
        );
        assert_eq!(
            normalize_observe_limit(Some(51), 8).unwrap_err(),
            "agent_observe limit must be between 1 and 50"
        );
        assert_eq!(normalize_observe_max_chars(None, 800).unwrap(), 800);
        assert_eq!(
            normalize_observe_max_chars(Some(79), 800).unwrap_err(),
            "agent_observe max_chars must be between 80 and 2000"
        );
        assert_eq!(
            normalize_observe_max_chars(Some(2_001), 800).unwrap_err(),
            "agent_observe max_chars must be between 80 and 2000"
        );
    }

    #[test]
    fn optional_integer_rejects_non_integers() {
        assert_eq!(normalize_optional_integer(None, "limit").unwrap(), None);
        assert_eq!(
            normalize_optional_integer(Some(&json!(3)), "limit").unwrap(),
            Some(3)
        );
        assert_eq!(
            normalize_optional_integer(Some(&json!(1.5)), "limit").unwrap_err(),
            "limit must be an integer when provided"
        );
        assert_eq!(
            normalize_optional_integer(Some(&json!("3")), "limit").unwrap_err(),
            "limit must be an integer when provided"
        );
    }

    #[test]
    fn user_content_blocks_join_with_newlines() {
        let message = AgentMessage::Message(pi_ai::types::Message::User(UserMessage::new(
            UserContent::Blocks(vec![
                ImageOrTextContent::Text(TextContent::new("a")),
                ImageOrTextContent::Text(TextContent::new("b")),
            ]),
            0,
        )));
        assert_eq!(message_text(&message), "ab");
    }
}
