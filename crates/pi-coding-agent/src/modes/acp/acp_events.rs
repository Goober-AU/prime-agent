//! Port of packages/coding-agent/src/modes/acp/acp-events.ts
//!
//! Translate prime-agent session events into ACP `session/update` payloads.
//!
//! Kept as a pure function so the mapping is testable without a live ACP client
//! or a running agent. Returning a vector lets one prime-agent event fan out to
//! several ACP updates (or none, for events ACP has no place for).

use serde_json::{Map, Value};

use crate::modes::acp::acp_meta::{
    prime_agent_meta, PrimeAgentIpythonAttachmentMeta, PrimeAgentIpythonMeta, PrimeAgentSessionMeta,
};
use crate::modes::agent_connection::types::AgentConnectionSessionEvent;

/// `sessionUpdate` string plus arbitrary extra keys.
pub type AcpSessionUpdate = Map<String, Value>;

pub const ACP_TOOL_KIND_READ: &str = "read";
pub const ACP_TOOL_KIND_EDIT: &str = "edit";
pub const ACP_TOOL_KIND_DELETE: &str = "delete";
pub const ACP_TOOL_KIND_MOVE: &str = "move";
pub const ACP_TOOL_KIND_SEARCH: &str = "search";
pub const ACP_TOOL_KIND_EXECUTE: &str = "execute";
pub const ACP_TOOL_KIND_THINK: &str = "think";
pub const ACP_TOOL_KIND_FETCH: &str = "fetch";
pub const ACP_TOOL_KIND_OTHER: &str = "other";

pub type AcpToolKind = &'static str;

pub const ACP_TOOL_STATUS_PENDING: &str = "pending";
pub const ACP_TOOL_STATUS_IN_PROGRESS: &str = "in_progress";
pub const ACP_TOOL_STATUS_COMPLETED: &str = "completed";
pub const ACP_TOOL_STATUS_FAILED: &str = "failed";

pub type AcpToolStatus = &'static str;

/// prime-agent's model-facing tool is the Python REPL; bash is the secondary escape hatch.
pub const IPYTHON_TOOL_NAME: &str = "ipython";

pub fn acp_tool_kind(tool_name: &str) -> AcpToolKind {
    match tool_name {
        IPYTHON_TOOL_NAME => ACP_TOOL_KIND_EXECUTE,
        "bash" => ACP_TOOL_KIND_EXECUTE,
        "read" => ACP_TOOL_KIND_READ,
        "edit" => ACP_TOOL_KIND_EDIT,
        "write" => ACP_TOOL_KIND_EDIT,
        _ => ACP_TOOL_KIND_OTHER,
    }
}

/// Decoded byte length of a base64 payload, without materializing it.
fn base64_byte_length(data: &str) -> u64 {
    let padding = if data.ends_with("==") {
        2
    } else if data.ends_with('=') {
        1
    } else {
        0
    };
    let raw = (data.len() as i64 * 3) / 4 - padding;
    raw.max(0) as u64
}

fn text_content(text: &str) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("text".to_string()));
    object.insert("text".to_string(), Value::String(text.to_string()));
    Value::Object(object)
}

fn update_with(session_update: &str, extra: Vec<(&str, Value)>) -> AcpSessionUpdate {
    let mut update = Map::new();
    update.insert("sessionUpdate".to_string(), Value::String(session_update.to_string()));
    for (key, value) in extra {
        update.insert(key.to_string(), value);
    }
    update
}

/// Map one streaming assistant event to an ACP chunk.
///
/// The delta discriminator lives on the event itself (`text_delta` /
/// `thinking_delta`) and carries a plain string, so reasoning and visible answer
/// text are distinct ACP update kinds a client can render or hide separately.
fn assistant_delta_updates(event: &pi_ai::types::AssistantMessageEvent, message_id: &str) -> Vec<AcpSessionUpdate> {
    match event {
        pi_ai::types::AssistantMessageEvent::ThinkingDelta { delta, .. } if !delta.is_empty() => vec![update_with(
            "agent_thought_chunk",
            vec![
                ("messageId", Value::String(message_id.to_string())),
                ("content", text_content(delta)),
            ],
        )],
        pi_ai::types::AssistantMessageEvent::TextDelta { delta, .. } if !delta.is_empty() => vec![update_with(
            "agent_message_chunk",
            vec![
                ("messageId", Value::String(message_id.to_string())),
                ("content", text_content(delta)),
            ],
        )],
        _ => Vec::new(),
    }
}

/// Extract the Python cell source so a client can show what is executing.
fn ipython_cell_source(args: &Value) -> Option<String> {
    let code = args.as_object()?.get("code")?;
    code.as_str().map(|text| text.to_string())
}

fn tool_result_text(result: &Value) -> Option<String> {
    if let Some(text) = result.as_str() {
        return Some(text.to_string());
    }
    let object = result.as_object()?;
    if let Some(output) = object.get("output") {
        if let Some(text) = output.as_str() {
            return Some(text.to_string());
        }
    }
    if let Some(content) = object.get("content") {
        if let Some(items) = content.as_array() {
            let parts: Vec<String> = items
                .iter()
                .map(|block| match block.as_object() {
                    Some(block) if block.get("type").and_then(Value::as_str) == Some("text") => block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    _ => String::new(),
                })
                .filter(|part| !part.is_empty())
                .collect();
            if !parts.is_empty() {
                return Some(parts.join("\n"));
            }
        }
    }
    None
}

/// Rich kernel output that ACP has no content type for.
///
/// The ipython tool reports media and diffs under `details` (images additionally
/// ride along as ACP image content blocks); mirror those exact fields rather than
/// inventing a MIME bundle the tool never produces.
fn ipython_rich_output(result: &Value) -> Option<PrimeAgentIpythonMeta> {
    let details = result.as_object()?.get("details")?.as_object()?;
    let mut meta = PrimeAgentIpythonMeta::default();
    if let Some(attachments) = details.get("attachments").and_then(Value::as_array) {
        if !attachments.is_empty() {
            meta.attachments = Some(
                attachments
                    .iter()
                    .map(|attachment| {
                        // KernelAttachment exposes mimeType, base64 `data`, and an optional path.
                        // Report the decoded size rather than a `bytes` field the kernel never
                        // sends, and never inline the payload.
                        let typed = attachment.as_object();
                        let mut entry = PrimeAgentIpythonAttachmentMeta::default();
                        if let Some(mime_type) = typed.and_then(|value| value.get("mimeType")).and_then(Value::as_str)
                        {
                            entry.mime_type = Some(mime_type.to_string());
                        }
                        if let Some(path) = typed.and_then(|value| value.get("path")).and_then(Value::as_str) {
                            entry.path = Some(path.to_string());
                        }
                        if let Some(data) = typed.and_then(|value| value.get("data")).and_then(Value::as_str) {
                            entry.bytes = Some(base64_byte_length(data));
                        }
                        entry
                    })
                    .collect(),
            );
        }
    }
    if let Some(diffs) = details.get("diffs").and_then(Value::as_array) {
        if !diffs.is_empty() {
            meta.diff_count = Some(diffs.len());
        }
    }
    if meta.attachments.is_some() || meta.diff_count.is_some() {
        Some(meta)
    } else {
        None
    }
}

/// Correlates streamed bash output and assistant chunks with their owning run or message.
#[derive(Debug, Clone, Default)]
pub struct AcpEventMappingState {
    pub active_bash_run_id: Option<String>,
    pub active_assistant_message_id: Option<String>,
    pub next_assistant_message_sequence: Option<i64>,
}

fn start_assistant_message(state: &mut AcpEventMappingState) -> String {
    let sequence = state.next_assistant_message_sequence.unwrap_or(0) + 1;
    state.next_assistant_message_sequence = Some(sequence);
    let message_id = format!("prime-agent-assistant-{sequence}");
    state.active_assistant_message_id = Some(message_id.clone());
    message_id
}

pub fn acp_updates_for_session_event(
    event: &AgentConnectionSessionEvent,
    state: &mut AcpEventMappingState,
) -> Vec<AcpSessionUpdate> {
    match event {
        AgentConnectionSessionEvent::MessageStart { message } => {
            if message.role() == "assistant" {
                start_assistant_message(state);
            }
            Vec::new()
        }

        AgentConnectionSessionEvent::MessageUpdate {
            message,
            assistant_message_event,
        } => {
            if message.role() != "assistant" {
                return Vec::new();
            }
            let message_id = match state.active_assistant_message_id.clone() {
                Some(message_id) => message_id,
                None => start_assistant_message(state),
            };
            assistant_delta_updates(assistant_message_event, &message_id)
        }

        AgentConnectionSessionEvent::MessageEnd { message } => {
            if message.role() == "assistant" {
                state.active_assistant_message_id = None;
            }
            Vec::new()
        }

        AgentConnectionSessionEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => {
            let cell = if tool_name == IPYTHON_TOOL_NAME {
                ipython_cell_source(args)
            } else {
                None
            };
            let raw_input = match cell {
                Some(code) => {
                    let mut object = Map::new();
                    object.insert("code".to_string(), Value::String(code));
                    Value::Object(object)
                }
                None => args.clone(),
            };
            vec![update_with(
                "tool_call",
                vec![
                    ("toolCallId", Value::String(tool_call_id.clone())),
                    (
                        "title",
                        Value::String(if tool_name == IPYTHON_TOOL_NAME {
                            "Python cell".to_string()
                        } else {
                            tool_name.clone()
                        }),
                    ),
                    ("kind", Value::String(acp_tool_kind(tool_name).to_string())),
                    ("status", Value::String(ACP_TOOL_STATUS_IN_PROGRESS.to_string())),
                    ("rawInput", raw_input),
                ],
            )]
        }

        AgentConnectionSessionEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
            ..
        } => {
            let text = tool_result_text(result);
            let rich = if tool_name == IPYTHON_TOOL_NAME {
                ipython_rich_output(result)
            } else {
                None
            };
            let mut extra = vec![
                ("toolCallId", Value::String(tool_call_id.clone())),
                (
                    "status",
                    Value::String(
                        if *is_error {
                            ACP_TOOL_STATUS_FAILED
                        } else {
                            ACP_TOOL_STATUS_COMPLETED
                        }
                        .to_string(),
                    ),
                ),
            ];
            if let Some(text) = text {
                if !text.is_empty() {
                    let mut content_entry = Map::new();
                    content_entry.insert("type".to_string(), Value::String("content".to_string()));
                    content_entry.insert("content".to_string(), text_content(&text));
                    extra.push(("content", Value::Array(vec![Value::Object(content_entry)])));
                }
            }
            if let Some(rich) = rich {
                let mut meta = PrimeAgentSessionMeta::new();
                meta.ipython = Some(rich);
                extra.push(("_meta", Value::Object(prime_agent_meta(meta))));
            }
            vec![update_with("tool_call_update", extra)]
        }

        // Bash runs outside the tool-call lifecycle, so it gets a synthetic tool
        // call keyed by run id to keep incremental output addressable.
        AgentConnectionSessionEvent::BashStart { command, run_id, .. } => {
            state.active_bash_run_id = run_id.clone();
            vec![update_with(
                "tool_call",
                vec![
                    ("toolCallId", Value::String(bash_tool_call_id(run_id.as_deref()))),
                    ("title", Value::String(command.clone())),
                    ("kind", Value::String(ACP_TOOL_KIND_EXECUTE.to_string())),
                    ("status", Value::String(ACP_TOOL_STATUS_IN_PROGRESS.to_string())),
                    (
                        "rawInput",
                        Value::Object({
                            let mut object = Map::new();
                            object.insert("command".to_string(), Value::String(command.clone()));
                            object
                        }),
                    ),
                ],
            )]
        }

        AgentConnectionSessionEvent::BashOutput { chunk } => vec![update_with(
            "tool_call_update",
            vec![
                (
                    "toolCallId",
                    Value::String(bash_tool_call_id(state.active_bash_run_id.as_deref())),
                ),
                ("status", Value::String(ACP_TOOL_STATUS_IN_PROGRESS.to_string())),
                (
                    "content",
                    Value::Array(vec![Value::Object({
                        let mut object = Map::new();
                        object.insert("type".to_string(), Value::String("content".to_string()));
                        object.insert("content".to_string(), text_content(chunk));
                        object
                    })]),
                ),
            ],
        )],

        AgentConnectionSessionEvent::BashEnd {
            exit_code,
            cancelled,
            run_id,
            ..
        } => {
            if state.active_bash_run_id == *run_id {
                state.active_bash_run_id = None;
            }
            let completed = exit_code == &Some(0) && !*cancelled;
            vec![update_with(
                "tool_call_update",
                vec![
                    ("toolCallId", Value::String(bash_tool_call_id(run_id.as_deref()))),
                    (
                        "status",
                        Value::String(
                            if completed {
                                ACP_TOOL_STATUS_COMPLETED
                            } else {
                                ACP_TOOL_STATUS_FAILED
                            }
                            .to_string(),
                        ),
                    ),
                ],
            )]
        }

        // Compaction, subagents, goals and recaps have no ACP equivalent: surface
        // them as namespaced metadata rather than distorting a standard update.
        AgentConnectionSessionEvent::CompactionEnd { result, .. } => {
            let mut meta = PrimeAgentSessionMeta::new();
            meta.compaction = Some(crate::modes::acp::acp_meta::PrimeAgentCompactionMeta {
                tokens_before: result.as_ref().and_then(|result| result.tokens_before),
                summary: result.as_ref().and_then(|result| result.summary.clone()),
            });
            vec![update_with(
                "session_info_update",
                vec![("_meta", Value::Object(prime_agent_meta(meta)))],
            )]
        }

        AgentConnectionSessionEvent::RlmChildUpdate { child } => {
            let mut meta = PrimeAgentSessionMeta::new();
            meta.subagents = Some(vec![crate::modes::acp::acp_meta::PrimeAgentSubagentMeta {
                id: child.id.clone(),
                session_name: child.session_name.clone(),
                status: child.status.clone(),
                model: child.model.clone(),
                depth: None,
                token_count: child.token_count,
                error: child.error.clone(),
            }]);
            vec![update_with(
                "session_info_update",
                vec![("_meta", Value::Object(prime_agent_meta(meta)))],
            )]
        }

        // Goals, continual-harness refinement, and agent-to-agent messaging are
        // prime-agent concepts with no ACP counterpart. They are still part of a
        // turn's observable behavior, so they surface as namespaced metadata
        // instead of being dropped.
        AgentConnectionSessionEvent::GoalUpdate { goal } => {
            let mut meta = PrimeAgentSessionMeta::new();
            meta.goal = Some(crate::modes::acp::acp_meta::PrimeAgentGoalMeta {
                status: goal.status.clone(),
                objective: goal.objective.clone(),
                token_budget: goal.token_budget,
                tokens_used: goal.tokens_used,
            });
            vec![update_with(
                "session_info_update",
                vec![("_meta", Value::Object(prime_agent_meta(meta)))],
            )]
        }

        AgentConnectionSessionEvent::RefineComplete { result } => {
            let mut meta = PrimeAgentSessionMeta::new();
            meta.refinement = Some(crate::modes::acp::acp_meta::PrimeAgentRefinementMeta {
                status: "complete".to_string(),
                summary: result.summary.clone(),
                changes: result.applied_edits.as_ref().map(|edits| {
                    edits
                        .iter()
                        .filter(|edit| edit.applied)
                        .map(|edit| format!("{} {}:{}", edit.action, edit.kind, edit.id))
                        .collect()
                }),
                error: None,
            });
            vec![update_with(
                "session_info_update",
                vec![("_meta", Value::Object(prime_agent_meta(meta)))],
            )]
        }

        AgentConnectionSessionEvent::RefineFailed { error } => {
            let mut meta = PrimeAgentSessionMeta::new();
            meta.refinement = Some(crate::modes::acp::acp_meta::PrimeAgentRefinementMeta {
                status: "failed".to_string(),
                summary: None,
                changes: None,
                error: Some(error.clone()),
            });
            vec![update_with(
                "session_info_update",
                vec![("_meta", Value::Object(prime_agent_meta(meta)))],
            )]
        }

        AgentConnectionSessionEvent::IpythonSentAgentMessage { tool_call_id, message } => {
            let mut meta = PrimeAgentSessionMeta::new();
            meta.agent_message = Some(crate::modes::acp::acp_meta::PrimeAgentAgentMessageMeta {
                tool_call_id: tool_call_id.clone(),
                target: Some(
                    message
                        .target
                        .session_name
                        .clone()
                        .unwrap_or_else(|| message.target.session_id.clone()),
                ),
                delivery_status: Some(message.delivery_status.clone()),
            });
            vec![update_with(
                "session_info_update",
                vec![("_meta", Value::Object(prime_agent_meta(meta)))],
            )]
        }

        _ => Vec::new(),
    }
}

const BASH_TOOL_CALL_PREFIX: &str = "prime-agent-bash";

pub fn bash_tool_call_id(run_id: Option<&str>) -> String {
    match run_id {
        Some(run_id) => format!("{BASH_TOOL_CALL_PREFIX}-{run_id}"),
        None => BASH_TOOL_CALL_PREFIX.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_kinds_match_the_typescript_switch() {
        assert_eq!(acp_tool_kind("ipython"), "execute");
        assert_eq!(acp_tool_kind("bash"), "execute");
        assert_eq!(acp_tool_kind("read"), "read");
        assert_eq!(acp_tool_kind("edit"), "edit");
        assert_eq!(acp_tool_kind("write"), "edit");
        assert_eq!(acp_tool_kind("grep"), "other");
    }

    #[test]
    fn bash_tool_call_ids_use_the_prefix() {
        assert_eq!(bash_tool_call_id(None), "prime-agent-bash");
        assert_eq!(bash_tool_call_id(Some("r1")), "prime-agent-bash-r1");
    }

    #[test]
    fn decoded_base64_length_matches_the_formula() {
        assert_eq!(base64_byte_length(""), 0);
        assert_eq!(base64_byte_length("QQ=="), 1);
        assert_eq!(base64_byte_length("QUI="), 2);
        assert_eq!(base64_byte_length("QUJD"), 3);
    }

    #[test]
    fn assistant_deltas_map_to_chunk_updates() {
        let mut state = AcpEventMappingState::default();
        let partial = pi_ai::types::AssistantMessage::default();
        let event = AgentConnectionSessionEvent::MessageUpdate {
            message: pi_agent_core::types::AgentMessage::Message(pi_ai::types::Message::Assistant(partial.clone())),
            assistant_message_event: pi_ai::types::AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "hi".to_string(),
                partial,
            },
        };
        let updates = acp_updates_for_session_event(&event, &mut state);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].get("sessionUpdate").unwrap(), "agent_message_chunk");
        assert_eq!(
            updates[0].get("messageId").unwrap(),
            &Value::String("prime-agent-assistant-1".to_string())
        );
    }

    #[test]
    fn unhandled_events_produce_no_updates() {
        let mut state = AcpEventMappingState::default();
        let updates = acp_updates_for_session_event(&AgentConnectionSessionEvent::AutoRetryEnd {
            success: true,
            attempt: 1,
            final_error: None,
        }, &mut state);
        assert!(updates.is_empty());
    }
}
