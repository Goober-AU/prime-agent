//! Port of packages/coding-agent/src/modes/interactive/components/conversation-components.ts
//!
//! PARTIAL: `buildConversationComponents` constructs per-message components
//! (`AssistantMessageComponent`, `ToolExecutionComponent`, `AgentMessageComponent`,
//! `SlashCommandMessageComponent`, ...). Those classes belong to other slices of
//! this crate and are still empty, so this port keeps every classification
//! decision of the TypeScript and emits the component *kinds* in the same order.
//! When the component classes land, each kind becomes the corresponding
//! constructor call without changing the branch order below.
//!
//! Messages are read through their serialized shape (`serde_json::Value`), which
//! is the same field set the TypeScript reads (`role`, `customType`, `display`,
//! `content`, `stopReason`, `errorMessage`, `toolCallId`, `details`).

use pi_agent_core::types::AgentMessage;
use pi_tui::components::markdown::MarkdownTheme;
use pi_tui::tui::TUI;
use serde_json::Value;
use std::rc::Rc;

use crate::core::messages::{
    is_compaction_outcome_message, is_refinement_outcome_message, is_session_slash_command_message,
    is_session_slash_command_result_message, COMPACTION_OUTCOME_CUSTOM_TYPE,
    REFINEMENT_OUTCOME_CUSTOM_TYPE, SESSION_SLASH_COMMAND_CUSTOM_TYPE,
    SESSION_SLASH_COMMAND_RESULT_CUSTOM_TYPE,
};

/// `ASYNC_BASH_COMPLETION_CUSTOM_TYPE`, `HEARTBEAT_PROMPT_CUSTOM_TYPE`,
/// `GOAL_CONTEXT_CUSTOM_TYPE`, `IPYTHON_STATE_RESTORED_CUSTOM_TYPE`,
/// `RLM_CHILD_FAILURE_CUSTOM_TYPE`, `RLM_CHILD_TERMINAL_NOTICE_CUSTOM_TYPE`.
///
/// Private copy of `isInjectedPromptMessage` (injected-prompt-message.ts, another
/// slice) so the branch order below is identical.
fn is_injected_prompt_message(message: &Value) -> bool {
    let Some(custom_type) = message.get("customType").and_then(Value::as_str) else {
        return false;
    };
    const INJECTED: [&str; 6] = [
        crate::core::messages::ASYNC_BASH_COMPLETION_CUSTOM_TYPE,
        crate::core::messages::HEARTBEAT_PROMPT_CUSTOM_TYPE,
        crate::core::goals::GOAL_CONTEXT_CUSTOM_TYPE,
        crate::core::messages::IPYTHON_STATE_RESTORED_CUSTOM_TYPE,
        crate::core::messages::RLM_CHILD_FAILURE_CUSTOM_TYPE,
        crate::core::messages::RLM_CHILD_TERMINAL_NOTICE_CUSTOM_TYPE,
    ];
    INJECTED.contains(&custom_type)
}

/// `isAgentSessionMessage` (core/agent-messages.ts) over the serialized shape.
fn is_agent_session_message_value(message: &Value) -> bool {
    if message.get("customType").and_then(Value::as_str)
        != Some(crate::core::agent_messages::AGENT_MESSAGE_CUSTOM_TYPE)
    {
        return false;
    }
    let Some(details) = message.get("details") else {
        return false;
    };
    details.get("id").map(Value::is_string).unwrap_or(false)
        && details
            .get("message")
            .map(Value::is_string)
            .unwrap_or(false)
}

/// Port of `getToolDefinition` from `ConversationComponentsOptions`.
pub type GetToolDefinition<'a> = &'a dyn Fn(&str) -> Option<ToolExecutionDefinition>;

/// Port of `ToolExecutionDefinition` (components/tool-execution.ts).
#[derive(Debug, Clone, Default)]
pub struct ToolExecutionDefinition {
    pub label: Option<String>,
    pub render_shell: Option<String>,
    pub replay_built_in_tool_name: Option<String>,
    pub has_render_call: bool,
    pub has_render_result: bool,
}

/// Port of `ToolExecutionOptions` (components/tool-execution.ts).
#[derive(Debug, Clone, Copy, Default)]
pub struct ToolExecutionOptions {
    pub show_images: Option<bool>,
    /// Whether image metadata may parse dimensions from base64 data.
    pub include_image_dimensions: Option<bool>,
}

/// Port of `ConversationComponentsOptions`.
pub struct ConversationComponentsOptions<'a> {
    pub ui: Rc<std::cell::RefCell<TUI>>,
    pub cwd: String,
    pub tool_options: ToolExecutionOptions,
    pub get_tool_definition: GetToolDefinition<'a>,
    pub markdown_theme: Option<MarkdownTheme>,
    pub hide_thinking_block: Option<bool>,
    pub hidden_thinking_label: Option<String>,
    pub tools_expanded: Option<bool>,
    pub agent_messages_expanded: Option<bool>,
    pub edit_diffs_expanded: Option<bool>,
    pub is_recognized_slash_command: Option<Box<dyn Fn(&str) -> bool>>,
}

/// The component kinds `buildConversationComponents` emits, in emission order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationComponentKind {
    /// `AssistantMessageComponent`
    AssistantMessage,
    /// `ToolExecutionComponent`
    ToolExecution {
        tool_call_id: String,
        tool_name: String,
    },
    /// `ToolExecutionComponent.updateResult(...)` on an existing row.
    ToolResultUpdate { tool_call_id: String },
    /// `SlashCommandMessageComponent`
    SlashCommandMessage,
    /// `SlashCommandResultMessageComponent`
    SlashCommandResultMessage,
    /// `UserMessageComponent("[Malformed session command message]")`
    MalformedSessionCommand,
    /// `CompactionOutcomeMessageComponent`
    CompactionOutcome,
    /// `MalformedCompactionOutcomeMessageComponent`
    MalformedCompactionOutcome,
    /// `RefinementOutcomeMessageComponent`
    RefinementOutcome,
    /// `MalformedRefinementOutcomeMessageComponent`
    MalformedRefinementOutcome,
    /// `AgentMessageComponent`
    AgentMessage,
    /// `InjectedPromptMessageComponent`
    InjectedPrompt,
    /// `UserMessageComponent`
    UserMessage { text: String },
}

/// Port of `isCompactAgentMessageNeighbor`.
pub fn is_compact_agent_message_neighbor(component: Option<&ConversationComponentKind>) -> bool {
    matches!(
        component,
        Some(ConversationComponentKind::AgentMessage)
            | Some(ConversationComponentKind::ToolExecution { .. })
    )
}

/// Port of `readUserText`.
pub fn read_user_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| {
                    block.get("type").and_then(Value::as_str) == Some("text")
                        && block.get("text").map(Value::is_string).unwrap_or(false)
                })
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<&str>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Build conversation components from a message list, matching tool results to their calls.
pub fn build_conversation_components(
    messages: &[AgentMessage],
    options: &ConversationComponentsOptions<'_>,
) -> Vec<ConversationComponentKind> {
    let mut components: Vec<ConversationComponentKind> = Vec::new();
    let expanded = options.tools_expanded.unwrap_or(false);
    let agent_messages_expanded = options.agent_messages_expanded.unwrap_or(false);
    let edit_diffs_expanded = options.edit_diffs_expanded.unwrap_or(false);
    let _ = &options.tool_options;

    for message in messages {
        let Ok(value) = serde_json::to_value(message) else {
            continue;
        };
        let role = value.get("role").and_then(Value::as_str).unwrap_or("");

        if role == "assistant" {
            let _ = options.hide_thinking_block.unwrap_or(false);
            let _ = options
                .hidden_thinking_label
                .clone()
                .unwrap_or_else(|| "Thinking...".to_string());
            let _ = &options.cwd;
            components.push(ConversationComponentKind::AssistantMessage);

            let stop_reason = value
                .get("stopReason")
                .and_then(Value::as_str)
                .unwrap_or("");
            let error_message = value
                .get("errorMessage")
                .and_then(Value::as_str)
                .unwrap_or("Operation aborted")
                .to_string();
            let content = value
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for block in content {
                if block.get("type").and_then(Value::as_str) != Some("toolCall") {
                    continue;
                }
                let call_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let call_name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let _ = (options.get_tool_definition)(&call_name);
                components.push(ConversationComponentKind::ToolExecution {
                    tool_call_id: call_id.clone(),
                    tool_name: call_name,
                });
                let _ = (expanded, agent_messages_expanded, edit_diffs_expanded);
                if stop_reason == "aborted" || stop_reason == "error" {
                    let _ = error_message;
                    components.push(ConversationComponentKind::ToolResultUpdate {
                        tool_call_id: call_id,
                    });
                }
            }
        } else if role == "toolResult" {
            let tool_call_id = value
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            components.push(ConversationComponentKind::ToolResultUpdate { tool_call_id });
        } else if role == "custom" {
            let custom_type = value
                .get("customType")
                .and_then(Value::as_str)
                .unwrap_or("");
            let display = value
                .get("display")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            if custom_type == SESSION_SLASH_COMMAND_CUSTOM_TYPE
                || custom_type == SESSION_SLASH_COMMAND_RESULT_CUSTOM_TYPE
            {
                if !display {
                    continue;
                }
                if is_session_slash_command_message(&value) {
                    components.push(ConversationComponentKind::SlashCommandMessage);
                } else if is_session_slash_command_result_message(&value) {
                    components.push(ConversationComponentKind::SlashCommandResultMessage);
                } else {
                    components.push(ConversationComponentKind::MalformedSessionCommand);
                }
            } else if custom_type == COMPACTION_OUTCOME_CUSTOM_TYPE {
                if !display {
                    continue;
                }
                components.push(if is_compaction_outcome_message(&value) {
                    ConversationComponentKind::CompactionOutcome
                } else {
                    ConversationComponentKind::MalformedCompactionOutcome
                });
            } else if custom_type == REFINEMENT_OUTCOME_CUSTOM_TYPE {
                if !display {
                    continue;
                }
                components.push(if is_refinement_outcome_message(&value) {
                    ConversationComponentKind::RefinementOutcome
                } else {
                    ConversationComponentKind::MalformedRefinementOutcome
                });
            } else if is_agent_session_message_value(&value) && display {
                let _ = is_compact_agent_message_neighbor(components.last());
                let _ = agent_messages_expanded;
                components.push(ConversationComponentKind::AgentMessage);
            } else if is_injected_prompt_message(&value) && display {
                let _ = expanded;
                components.push(ConversationComponentKind::InjectedPrompt);
            }
        } else if role == "user" {
            let content = value.get("content").cloned().unwrap_or(Value::Null);
            let text = read_user_text(&content);
            let has_content = if let Some(text) = content.as_str() {
                !text.is_empty()
            } else {
                content
                    .as_array()
                    .map(|blocks| !blocks.is_empty())
                    .unwrap_or(false)
            };
            // An image-only prompt has no text; show a placeholder rather than dropping it.
            let display = if text.is_empty() && has_content {
                "[image]".to_string()
            } else {
                text
            };
            if !display.is_empty() {
                components.push(ConversationComponentKind::UserMessage { text: display });
            }
        }
        // Non-conversational messages (bash/branch-summary/compaction/other custom) aren't shown.
    }
    components
}

/// Marker so the option struct keeps the same `TUI` dependency the TypeScript has.
pub fn ui_request_render(ui: &Rc<std::cell::RefCell<TUI>>) {
    ui.borrow_mut().request_render();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::messages::{
        create_session_slash_command_message, create_session_slash_command_result_message,
    };
    use crate::core::slash_commands::SessionSlashCommand;

    fn options<'a>(
        get_tool_definition: &'a dyn Fn(&str) -> Option<ToolExecutionDefinition>,
    ) -> ConversationComponentsOptions<'a> {
        ConversationComponentsOptions {
            ui: Rc::new(std::cell::RefCell::new(TUI::new(
                Box::new(pi_tui::terminal::ProcessTerminal::new()),
                None,
            ))),
            cwd: "/cwd".to_string(),
            tool_options: ToolExecutionOptions::default(),
            get_tool_definition,
            markdown_theme: None,
            hide_thinking_block: None,
            hidden_thinking_label: None,
            tools_expanded: None,
            agent_messages_expanded: None,
            edit_diffs_expanded: None,
            is_recognized_slash_command: None,
        }
    }

    fn no_definition(_name: &str) -> Option<ToolExecutionDefinition> {
        None
    }

    #[test]
    fn user_messages_are_emitted_and_image_only_prompts_get_a_placeholder() {
        let message =
            AgentMessage::Message(pi_ai::types::Message::User(pi_ai::types::UserMessage {
                role: "user".to_string(),
                content: pi_ai::types::UserContent::Blocks(vec![
                    pi_ai::types::ImageOrTextContent::Image(pi_ai::types::ImageContent::new(
                        "x",
                        "image/png",
                    )),
                ]),
                provider_context: None,
                timestamp: 0,
            }));
        let kinds = build_conversation_components(&[message], &options(&no_definition));
        assert_eq!(
            kinds,
            vec![ConversationComponentKind::UserMessage {
                text: "[image]".to_string()
            }]
        );
    }

    #[test]
    fn hidden_session_commands_are_skipped() {
        let message = crate::core::messages::custom_message_to_agent_message(
            create_session_slash_command_message(
                SessionSlashCommand {
                    name: "compact".to_string(),
                    args: String::new(),
                    text: "/compact".to_string(),
                },
                crate::core::messages::SessionSlashCommandDetails {
                    command: SessionSlashCommand {
                        name: "compact".to_string(),
                        args: String::new(),
                        text: "/compact".to_string(),
                    },
                    command_entry_id: None,
                },
                false,
                0,
            ),
        );
        let kinds = build_conversation_components(&[message], &options(&no_definition));
        assert!(kinds.is_empty());
    }

    #[test]
    fn displayed_session_command_results_produce_their_component() {
        let command = SessionSlashCommand {
            name: "compact".to_string(),
            args: String::new(),
            text: "/compact".to_string(),
        };
        let message = crate::core::messages::custom_message_to_agent_message(
            create_session_slash_command_result_message(
                "done".to_string(),
                crate::core::messages::SessionSlashCommandResultDetails {
                    command: command.clone(),
                    success: true,
                    severity: "info".to_string(),
                    error: None,
                    command_entry_id: None,
                },
                true,
                0,
            ),
        );
        let kinds = build_conversation_components(&[message], &options(&no_definition));
        assert_eq!(
            kinds,
            vec![ConversationComponentKind::SlashCommandResultMessage]
        );
        let _ = command;
    }

    #[test]
    fn compact_neighbor_only_matches_agent_message_and_tool_execution() {
        assert!(is_compact_agent_message_neighbor(Some(
            &ConversationComponentKind::AgentMessage
        )));
        assert!(!is_compact_agent_message_neighbor(Some(
            &ConversationComponentKind::UserMessage {
                text: "x".to_string()
            }
        )));
        assert!(!is_compact_agent_message_neighbor(None));
    }

    #[test]
    fn read_user_text_joins_text_blocks() {
        let content = serde_json::json!([
            {"type": "text", "text": "a"},
            {"type": "image", "data": "x"},
            {"type": "text", "text": "b"}
        ]);
        assert_eq!(read_user_text(&content), "ab");
        assert_eq!(read_user_text(&serde_json::json!("plain")), "plain");
    }
}
