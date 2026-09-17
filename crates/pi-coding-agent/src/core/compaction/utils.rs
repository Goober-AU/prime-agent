//! Port of packages/coding-agent/src/core/compaction/utils.ts
//!
//! Shared utilities for compaction and branch summarization.

use std::collections::BTreeSet;

use pi_agent_core::types::{AgentMessage, CustomAgentMessage};
use pi_ai::types::Message;
use serde_json::Value;

/// `FileOperations`; `Set<string>` becomes `BTreeSet<String>` (the TypeScript
/// sorts every consumer of these sets before it renders them).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOperations {
    pub read: BTreeSet<String>,
    pub written: BTreeSet<String>,
    pub edited: BTreeSet<String>,
}

pub fn create_file_ops() -> FileOperations {
    FileOperations {
        read: BTreeSet::new(),
        written: BTreeSet::new(),
        edited: BTreeSet::new(),
    }
}

/// Extract file operations from tool calls in an assistant message.
pub fn extract_file_ops_from_message(message: &AgentMessage, file_ops: &mut FileOperations) {
    if let AgentMessage::Message(Message::ToolResult(result)) = message {
        if result.tool_name == "ipython" {
            if let Some(diffs) = result.details.as_ref().and_then(|details| details.get("diffs")).and_then(Value::as_array) {
                for diff in diffs {
                    if let Some(path) = diff.get("path").and_then(Value::as_str).filter(|path| !path.is_empty()) {
                        file_ops.edited.insert(path.to_string());
                    }
                }
            }
        }
        return;
    }
    let AgentMessage::Message(Message::Assistant(assistant)) = message else {
        return;
    };
    for block in &assistant.content {
        let pi_ai::types::ContentBlock::ToolCall(tool_call) = block else {
            continue;
        };
        let args: &serde_json::Map<String, Value> = &tool_call.arguments;
        let path = match args.get("path") {
            Some(Value::String(path)) => path.clone(),
            _ => continue,
        };
        if path.is_empty() {
            continue;
        }
        match tool_call.name.as_str() {
            "edit" => {
                file_ops.edited.insert(path);
            }
            _ => {}
        }
    }
}

/// Compute final file lists from file operations.
/// Returns readFiles (files only read, not modified) and modifiedFiles.
pub fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let mut modified: BTreeSet<String> = file_ops.edited.clone();
    modified.extend(file_ops.written.iter().cloned());
    let read_only: Vec<String> = file_ops
        .read
        .iter()
        .filter(|path| !modified.contains(*path))
        .take(200)
        .cloned()
        .collect();
    let modified_files: Vec<String> = modified.into_iter().take(200).collect();
    (read_only, modified_files)
}

/// Format file operations as XML tags for summary.
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

/// Maximum characters for a tool result in serialized summaries.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// Truncate text to a maximum character length for summarization.
/// Keeps both the beginning and the error-heavy tail inside the same budget.
fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    // `String::chars` counts UTF-16-independent scalar values; the TypeScript
    // `slice` counts UTF-16 code units. Plain ASCII/Unicode text matches.
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let tail_chars = 500.min(max_chars / 4);
    let marker_max = format!("[... {} characters truncated; first {max_chars} and last {tail_chars} kept ...]", chars.len()).len() + 4;
    if max_chars <= marker_max + tail_chars {
        return chars[chars.len() - max_chars..].iter().collect();
    }
    let head_chars = max_chars - tail_chars - marker_max;
    let elided = chars.len() - head_chars - tail_chars;
    let head: String = chars[..head_chars].iter().collect();
    let tail: String = chars[chars.len() - tail_chars..].iter().collect();
    format!("{head}\n\n[... {elided} characters truncated; first {head_chars} and last {tail_chars} kept ...]\n\n{tail}")
}

/// Serialize LLM messages to text for summarization.
/// This prevents the model from treating it as a conversation to continue.
/// Call convertToLlm() first to handle custom message types.
///
/// Tool results are truncated to keep the summarization request within
/// reasonable token budgets. Full content is not needed for summarization.
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for message in messages {
        match message {
            Message::User(user) => {
                let content = user.content.text();
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(assistant) => {
                let mut text_parts: Vec<String> = Vec::new();
                let mut thinking_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<String> = Vec::new();

                for block in &assistant.content {
                    match block {
                        pi_ai::types::ContentBlock::Text(text) => {
                            text_parts.push(text.text.clone())
                        }
                        pi_ai::types::ContentBlock::Thinking(thinking) => {
                            thinking_parts.push(thinking.thinking.clone())
                        }
                        pi_ai::types::ContentBlock::ToolCall(tool_call) => {
                            let args_str = tool_call
                                .arguments
                                .iter()
                                .map(|(key, value)| {
                                    format!(
                                        "{key}={}",
                                        serde_json::to_string(value).unwrap_or_default()
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            tool_calls.push(format!("{}({args_str})", tool_call.name));
                        }
                    }
                }

                if !thinking_parts.is_empty() {
                    parts.push(format!(
                        "[Assistant thinking]: {}",
                        thinking_parts.join("\n")
                    ));
                }
                if !text_parts.is_empty() {
                    parts.push(format!("[Assistant]: {}", text_parts.join("\n")));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Message::ToolResult(tool_result) => {
                let content: String = tool_result
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        pi_ai::types::ImageOrTextContent::Text(text) => Some(text.text.clone()),
                        pi_ai::types::ImageOrTextContent::Image(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
        }
    }

    parts.join("\n\n")
}

pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI coding assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

/// `extractFileOpsFromMessage` accepts the coding-agent custom assistant shape too;
/// this helper mirrors the TypeScript `"content" in message` guard for callers that
/// pass a custom message.
pub fn message_has_content(message: &AgentMessage) -> bool {
    match message {
        AgentMessage::Message(_) => true,
        AgentMessage::Custom(CustomAgentMessage::Custom { .. }) => true,
        AgentMessage::Custom(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{
        AssistantMessage, ContentBlock, ImageOrTextContent, TextContent, ToolCall, UserContent,
        UserMessage,
    };
    use serde_json::json;

    fn assistant_with_tool_call(name: &str, path: Option<&str>) -> AgentMessage {
        let mut arguments = serde_json::Map::new();
        if let Some(path) = path {
            arguments.insert("path".to_string(), json!(path));
        }
        AgentMessage::Message(Message::Assistant(AssistantMessage {
            content: vec![ContentBlock::ToolCall(ToolCall::new(
                "call-1", name, arguments,
            ))],
            ..Default::default()
        }))
    }

    #[test]
    fn edit_tool_calls_record_edited_paths_only() {
        let mut ops = create_file_ops();
        extract_file_ops_from_message(
            &assistant_with_tool_call("edit", Some("src/a.ts")),
            &mut ops,
        );
        extract_file_ops_from_message(
            &assistant_with_tool_call("read", Some("src/b.ts")),
            &mut ops,
        );
        extract_file_ops_from_message(&assistant_with_tool_call("edit", None), &mut ops);
        assert_eq!(
            ops.edited.iter().cloned().collect::<Vec<_>>(),
            vec!["src/a.ts"]
        );
        assert!(ops.read.is_empty());
        assert!(ops.written.is_empty());
    }

    #[test]
    fn compute_file_lists_sorts_and_excludes_modified_reads() {
        let mut ops = create_file_ops();
        ops.read.insert("b.ts".to_string());
        ops.read.insert("a.ts".to_string());
        ops.edited.insert("b.ts".to_string());
        ops.written.insert("c.ts".to_string());
        let (read_files, modified_files) = compute_file_lists(&ops);
        assert_eq!(read_files, vec!["a.ts".to_string()]);
        assert_eq!(modified_files, vec!["b.ts".to_string(), "c.ts".to_string()]);
    }

    #[test]
    fn format_file_operations_matches_typescript_layout() {
        assert_eq!(format_file_operations(&[], &[]), "");
        assert_eq!(
            format_file_operations(&["a.ts".to_string()], &[]),
            "\n\n<read-files>\na.ts\n</read-files>"
        );
        assert_eq!(
            format_file_operations(&["a.ts".to_string()], &["b.ts".to_string()]),
            "\n\n<read-files>\na.ts\n</read-files>\n\n<modified-files>\nb.ts\n</modified-files>"
        );
    }

    #[test]
    fn serialize_conversation_renders_each_role() {
        let messages = vec![
            Message::User(UserMessage::new(
                UserContent::Blocks(vec![ImageOrTextContent::Text(TextContent::new("hello"))]),
                1,
            )),
            Message::Assistant(AssistantMessage {
                content: vec![
                    ContentBlock::Thinking(pi_ai::types::ThinkingContent::new("why")),
                    ContentBlock::Text(TextContent::new("answer")),
                    ContentBlock::ToolCall(ToolCall::new("id", "edit", {
                        let mut map = serde_json::Map::new();
                        map.insert("path".to_string(), json!("a.ts"));
                        map
                    })),
                ],
                ..Default::default()
            }),
            Message::ToolResult(pi_ai::types::ToolResultMessage::new(
                "id",
                "edit",
                vec![ImageOrTextContent::Text(TextContent::new("ok"))],
                false,
                2,
            )),
        ];
        let text = serialize_conversation(&messages);
        assert_eq!(
            text,
            "[User]: hello\n\n[Assistant thinking]: why\n\n[Assistant]: answer\n\n[Assistant tool calls]: edit(path=\"a.ts\")\n\n[Tool result]: ok"
        );
    }

    #[test]
    fn serialize_conversation_truncates_long_tool_results() {
        let long = "x".repeat(TOOL_RESULT_MAX_CHARS + 10);
        let messages = vec![Message::ToolResult(pi_ai::types::ToolResultMessage::new(
            "id",
            "bash",
            vec![ImageOrTextContent::Text(TextContent::new(long))],
            false,
            1,
        ))];
        let text = serialize_conversation(&messages);
        assert!(text.contains("characters truncated; first"));
        assert!(text.ends_with(&"x".repeat(500)));
        assert!(text.chars().count() <= TOOL_RESULT_MAX_CHARS + "[Tool result]: ".len());
    }

    #[test]
    fn summary_keeps_unicode_error_tail_and_structured_python_edits() {
        let output = format!("BEGIN{}\nERROR: failed at final step", "界".repeat(4000));
        let summary = truncate_for_summary(&output, TOOL_RESULT_MAX_CHARS);
        assert!(summary.starts_with("BEGIN"));
        assert!(summary.ends_with("ERROR: failed at final step"));
        assert!(summary.chars().count() <= TOOL_RESULT_MAX_CHARS);
        let mut result = pi_ai::types::ToolResultMessage::new("id", "ipython", vec![], false, 1);
        result.details = Some(json!({"diffs": [{"path": "src/a.rs"}, {"path": ""}, 4, {"path": false}]}));
        let mut ops = create_file_ops();
        extract_file_ops_from_message(&AgentMessage::Message(Message::ToolResult(result)), &mut ops);
        assert_eq!(compute_file_lists(&ops).1, vec!["src/a.rs"]);
        for index in 0..250 { ops.edited.insert(format!("file-{index:03}")); }
        assert_eq!(compute_file_lists(&ops).1.len(), 200);
    }
}
