//! Port of packages/coding-agent/src/core/compaction/branch-summarization.ts
//!
//! Branch summarization for tree navigation.
//!
//! When navigating to a different point in the session tree, this generates
//! a summary of the branch being left so context isn't lost.

use std::collections::BTreeSet;

use pi_agent_core::types::{AgentMessage, CustomAgentMessage, ThinkingLevel};
use pi_ai::models::get_model_input_limit;
use pi_ai::stream::complete_simple;
use pi_ai::types::{
    ContentBlock, Context, Message, Model, SimpleStreamOptions, Usage, STOP_REASON_ABORTED,
    STOP_REASON_ERROR,
};
use serde_json::Value;

use crate::core::compaction::compaction::estimate_tokens;
use crate::core::compaction::compaction::ProviderRetryPolicy;
use crate::core::compaction::utils::{
    compute_file_lists, create_file_ops, extract_file_ops_from_message, format_file_operations,
    serialize_conversation, FileOperations, SUMMARIZATION_SYSTEM_PROMPT,
};
use crate::core::messages::{
    branch_summary_to_agent_message, compaction_summary_to_agent_message, convert_to_llm,
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
    custom_message_to_agent_message, HARNESS_DIGEST_CUSTOM_TYPE,
};

/// `SessionEntry` lives in core/session-manager.ts (another slice).
///
/// blocked_on: needs core::session_manager::{ReadonlySessionManager, SessionEntry}.
pub use crate::core::compaction::compaction::CompactionSessionEntry as SessionEntry;

/// Read-only session manager surface used by
/// `collectEntriesForBranchSummary`.
pub trait ReadonlySessionManager {
    fn get_branch(&self, leaf_id: &str) -> Vec<SessionEntry>;
    fn get_entry(&self, id: &str) -> Option<SessionEntry>;
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BranchSummaryResult {
    pub summary: Option<String>,
    pub read_files: Option<Vec<String>>,
    pub modified_files: Option<Vec<String>>,
    pub aborted: Option<bool>,
    pub error: Option<String>,
    pub usage: Option<Usage>,
}

/// Details stored in BranchSummaryEntry.details for file tracking
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

pub use crate::core::compaction::utils::FileOperations as FileOperationsReExport;

#[derive(Debug, Clone, Default)]
pub struct BranchPreparation {
    /// Messages extracted for summarization, in chronological order
    pub messages: Vec<AgentMessage>,
    /// File operations extracted from tool calls
    pub file_ops: FileOperations,
    /// Total estimated tokens in messages
    pub total_tokens: f64,
}

#[derive(Debug, Clone, Default)]
pub struct CollectEntriesResult {
    /// Entries to summarize, in chronological order
    pub entries: Vec<SessionEntry>,
    /// Common ancestor between old and new position, if any
    pub common_ancestor_id: Option<String>,
}

#[derive(Clone)]
pub struct GenerateBranchSummaryOptions {
    /// Model to use for summarization
    pub model: Model,
    /// API key for the model
    pub api_key: String,
    /// Request headers for the model
    pub headers: Option<serde_json::Map<String, Value>>,
    /// Abort signal for cancellation
    pub signal: Option<tokio_util::sync::CancellationToken>,
    /// Optional custom instructions for summarization
    pub custom_instructions: Option<String>,
    /// If true, customInstructions replaces the default prompt instead of being appended
    pub replace_instructions: Option<bool>,
    pub retry: Option<ProviderRetryPolicy>,
    /// Tokens reserved for prompt + LLM response (default 16384)
    pub reserve_tokens: Option<f64>,
}

/// Collect entries that should be summarized when navigating from one position to another.
///
/// Walks from oldLeafId back to the common ancestor with targetId, collecting entries
/// along the way. Does NOT stop at compaction boundaries - those are included and their
/// summaries become context.
pub fn collect_entries_for_branch_summary(
    session: &dyn ReadonlySessionManager,
    old_leaf_id: Option<&str>,
    target_id: &str,
) -> CollectEntriesResult {
    let Some(old_leaf_id) = old_leaf_id else {
        return CollectEntriesResult::default();
    };
    let old_path: BTreeSet<String> = session
        .get_branch(old_leaf_id)
        .into_iter()
        .map(|entry| entry.id().to_string())
        .collect();
    let target_path = session.get_branch(target_id);
    let mut common_ancestor_id: Option<String> = None;
    for entry in target_path.iter().rev() {
        if old_path.contains(entry.id()) {
            common_ancestor_id = Some(entry.id().to_string());
            break;
        }
    }
    let mut entries: Vec<SessionEntry> = Vec::new();
    let mut current: Option<String> = Some(old_leaf_id.to_string());

    while let Some(id) = current.clone() {
        if Some(id.as_str()) == common_ancestor_id.as_deref() {
            break;
        }
        let Some(entry) = session.get_entry(&id) else {
            break;
        };
        let parent_id = entry.parent_id().map(str::to_string);
        entries.push(entry);
        current = parent_id;
    }
    entries.reverse();

    CollectEntriesResult {
        entries,
        common_ancestor_id,
    }
}

/// Extract AgentMessage from a session entry.
/// Similar to getMessageFromEntry in compaction.ts but also handles compaction entries.
fn get_message_from_entry(entry: &SessionEntry) -> Option<AgentMessage> {
    match entry {
        SessionEntry::Message { message, .. } => {
            // Tool-result context remains attached to its assistant tool call.
            if message.role() == "toolResult" {
                return None;
            }
            Some(message.clone())
        }
        SessionEntry::CustomMessage {
            custom_type,
            content,
            display,
            details,
            timestamp,
            ..
        } => {
            // Harness digests are regenerated at cold boundaries; never summarizer input.
            if custom_type == HARNESS_DIGEST_CUSTOM_TYPE {
                return None;
            }
            Some(custom_message_to_agent_message(create_custom_message(
                custom_type.clone(),
                content.clone(),
                *display,
                details.clone(),
                timestamp,
            )))
        }
        SessionEntry::BranchSummary {
            summary,
            from_id,
            timestamp,
            ..
        } => Some(branch_summary_to_agent_message(
            create_branch_summary_message(summary.clone(), from_id.clone(), timestamp),
        )),
        SessionEntry::Compaction {
            summary,
            tokens_before,
            timestamp,
            custom_instructions,
            ..
        } => Some(compaction_summary_to_agent_message(
            create_compaction_summary_message(
                summary.clone(),
                *tokens_before,
                timestamp,
                custom_instructions.clone(),
                None,
                None,
                None,
            ),
        )),
        SessionEntry::Other { .. } => None,
    }
}

/// Prepare entries for summarization with token budget.
///
/// Walks entries from NEWEST to OLDEST, adding messages until we hit the token budget.
/// This ensures we keep the most recent context when the branch is too long.
///
/// Also collects file operations from:
/// - Tool calls in assistant messages
/// - Existing branch_summary entries' details (for cumulative tracking)
pub fn prepare_branch_entries(entries: &[SessionEntry], token_budget: f64) -> BranchPreparation {
    let mut messages: Vec<AgentMessage> = Vec::new();
    let mut file_ops = create_file_ops();
    let mut total_tokens = 0.0;

    // First pass: collect file ops from ALL entries (even if they don't fit in token budget)
    // This ensures we capture cumulative file tracking from nested branch summaries
    // Only extract from pi-generated summaries (fromHook !== true), not extension-generated ones
    for entry in entries {
        if let SessionEntry::BranchSummary {
            from_hook, details, ..
        } = entry
        {
            if from_hook != &Some(true) {
                if let Some(details) = details {
                    if let Some(read_files) = details.get("readFiles").and_then(Value::as_array) {
                        for path in read_files {
                            if let Some(path) = path.as_str() {
                                file_ops.read.insert(path.to_string());
                            }
                        }
                    }
                    if let Some(modified_files) =
                        details.get("modifiedFiles").and_then(Value::as_array)
                    {
                        for path in modified_files {
                            if let Some(path) = path.as_str() {
                                file_ops.edited.insert(path.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    if entries.is_empty() {
        return BranchPreparation {
            messages,
            file_ops,
            total_tokens,
        };
    }
    for index in (0..entries.len()).rev() {
        let entry = &entries[index];
        let Some(message) = get_message_from_entry(entry) else {
            continue;
        };
        extract_file_ops_from_message(&message, &mut file_ops);

        let tokens = estimate_tokens(&message);
        if token_budget > 0.0 && total_tokens + tokens > token_budget {
            if matches!(
                entry,
                SessionEntry::Compaction { .. } | SessionEntry::BranchSummary { .. }
            ) {
                if total_tokens < token_budget * 0.9 {
                    messages.insert(0, message);
                    total_tokens += tokens;
                }
            }
            break;
        }

        messages.insert(0, message);
        total_tokens += tokens;
    }

    BranchPreparation {
        messages,
        file_ops,
        total_tokens,
    }
}

const BRANCH_SUMMARY_PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";

const BRANCH_SUMMARY_PROMPT: &str = r#"Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

/// Generate a summary of abandoned branch entries.
pub async fn generate_branch_summary(
    entries: &[SessionEntry],
    options: GenerateBranchSummaryOptions,
) -> BranchSummaryResult {
    let GenerateBranchSummaryOptions {
        model,
        api_key,
        headers,
        signal,
        custom_instructions,
        replace_instructions,
        retry,
        reserve_tokens,
    } = options;
    let reserve_tokens = reserve_tokens.unwrap_or(16384.0);
    let context_window = if model.context_window != 0.0 {
        model.context_window
    } else {
        128000.0
    };
    let token_budget = context_window - reserve_tokens;

    let BranchPreparation {
        messages, file_ops, ..
    } = prepare_branch_entries(entries, token_budget);

    // Nothing model-visible remains after filtering.
    if messages.is_empty() {
        return BranchSummaryResult {
            summary: Some("No content to summarize".to_string()),
            ..Default::default()
        };
    }
    // Serialize before the LLM call so it summarizes rather than continues this branch.
    let llm_messages = convert_to_llm(&messages, &Default::default());
    let conversation_text = serialize_conversation(&llm_messages);
    let instructions = if replace_instructions == Some(true) && custom_instructions.is_some() {
        custom_instructions.clone().unwrap_or_default()
    } else if let Some(custom_instructions) = &custom_instructions {
        format!("{BRANCH_SUMMARY_PROMPT}\n\nAdditional focus: {custom_instructions}")
    } else {
        BRANCH_SUMMARY_PROMPT.to_string()
    };
    let prompt_text =
        format!("<conversation>\n{conversation_text}\n</conversation>\n\n{instructions}");

    let model_for_call = model.clone();
    let api_key_for_call = api_key.clone();
    let headers_for_call = headers.clone();
    let signal_for_call = signal.clone();
    let prompt_text_for_call = prompt_text.clone();
    let attempt: crate::core::compaction::compaction::SummaryCallFn = std::sync::Arc::new(
        move |call_headers: Option<serde_json::Map<String, Value>>| {
            let model = model_for_call.clone();
            let api_key = api_key_for_call.clone();
            let headers = headers_for_call.clone();
            let signal = signal_for_call.clone();
            let prompt_text = prompt_text_for_call.clone();
            Box::pin(async move {
                let mut merged_headers = headers.unwrap_or_default();
                if let Some(call_headers) = call_headers {
                    for (key, value) in call_headers {
                        merged_headers.insert(key, value);
                    }
                }
                let mut options = SimpleStreamOptions::default();
                options.stream.max_tokens = Some(2048.0);
                options.stream.api_key = Some(api_key);
                options.stream.signal = signal;
                if !merged_headers.is_empty() {
                    let mut map = indexmap::IndexMap::new();
                    for (key, value) in &merged_headers {
                        if let Some(value) = value.as_str() {
                            map.insert(key.clone(), value.to_string());
                        }
                    }
                    options.stream.headers = Some(map);
                }
                let context = Context {
                    system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
                    messages: vec![Message::User(pi_ai::types::UserMessage::new(
                        pi_ai::types::UserContent::Blocks(vec![
                            pi_ai::types::ImageOrTextContent::Text(pi_ai::types::TextContent::new(
                                prompt_text,
                            )),
                        ]),
                        now_millis(),
                    ))],
                    tools: None,
                };
                Ok(complete_simple(&model, &context, Some(&options)).await)
            })
        },
    );
    let runner = crate::core::compaction::compaction::default_summary_call_runner(None);
    let response = match (runner)(attempt).await {
        Ok(response) => response,
        Err(error) => {
            return BranchSummaryResult {
                error: Some(error),
                ..Default::default()
            }
        }
    };
    let _ = retry;
    if response.stop_reason == STOP_REASON_ABORTED {
        return BranchSummaryResult {
            aborted: Some(true),
            ..Default::default()
        };
    }
    if response.stop_reason == STOP_REASON_ERROR {
        return BranchSummaryResult {
            error: Some(
                response
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "Summarization failed".to_string()),
            ),
            ..Default::default()
        };
    }

    let body = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut summary = BRANCH_SUMMARY_PREAMBLE.to_string() + &body;
    let (read_files, modified_files) = compute_file_lists(&file_ops);
    summary += &format_file_operations(&read_files, &modified_files);

    BranchSummaryResult {
        summary: Some(if summary.is_empty() {
            "No summary generated".to_string()
        } else {
            summary
        }),
        read_files: Some(read_files),
        modified_files: Some(modified_files),
        aborted: None,
        error: None,
        usage: Some(response.usage),
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{AssistantMessage, ContentBlock, TextContent, ToolCall};
    use std::collections::HashMap;

    struct FakeSession {
        entries: HashMap<String, SessionEntry>,
        branches: HashMap<String, Vec<SessionEntry>>,
    }

    impl ReadonlySessionManager for FakeSession {
        fn get_branch(&self, leaf_id: &str) -> Vec<SessionEntry> {
            self.branches.get(leaf_id).cloned().unwrap_or_default()
        }

        fn get_entry(&self, id: &str) -> Option<SessionEntry> {
            self.entries.get(id).cloned()
        }
    }

    fn message_entry(
        id: &str,
        parent_id: Option<&str>,
        role: &str,
        timestamp: i64,
    ) -> SessionEntry {
        let message = if role == "assistant" {
            AgentMessage::Message(Message::Assistant(AssistantMessage {
                content: vec![ContentBlock::Text(TextContent::new("answer"))],
                timestamp,
                ..Default::default()
            }))
        } else if role == "toolResult" {
            AgentMessage::Message(Message::ToolResult(pi_ai::types::ToolResultMessage::new(
                "call",
                "read",
                vec![pi_ai::types::ImageOrTextContent::Text(TextContent::new(
                    "out",
                ))],
                false,
                timestamp,
            )))
        } else {
            AgentMessage::Message(Message::User(pi_ai::types::UserMessage::new(
                pi_ai::types::UserContent::Text("question".to_string()),
                timestamp,
            )))
        };
        SessionEntry::Message {
            id: id.to_string(),
            parent_id: parent_id.map(str::to_string),
            message,
        }
    }

    #[test]
    fn collect_entries_walks_back_to_the_common_ancestor() {
        let root = message_entry("root", None, "user", 1);
        let branch_old = message_entry("old", Some("root"), "assistant", 2);
        let branch_target = message_entry("target", Some("root"), "assistant", 3);
        let mut entries = HashMap::new();
        for entry in [&root, &branch_old, &branch_target] {
            entries.insert(entry.id().to_string(), entry.clone());
        }
        let mut branches = HashMap::new();
        branches.insert("old".to_string(), vec![root.clone(), branch_old.clone()]);
        branches.insert(
            "target".to_string(),
            vec![root.clone(), branch_target.clone()],
        );
        let session = FakeSession { entries, branches };

        let result = collect_entries_for_branch_summary(&session, Some("old"), "target");
        assert_eq!(result.common_ancestor_id.as_deref(), Some("root"));
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].id(), "old");

        let missing = collect_entries_for_branch_summary(&session, None, "target");
        assert!(missing.entries.is_empty());
        assert!(missing.common_ancestor_id.is_none());
    }

    #[test]
    fn prepare_branch_entries_skips_tool_results_and_harness_digests() {
        let entries = vec![
            message_entry("user", None, "user", 1),
            message_entry("assistant", Some("user"), "assistant", 2),
            message_entry("tool", Some("assistant"), "toolResult", 3),
            SessionEntry::CustomMessage {
                id: "digest".to_string(),
                parent_id: Some("tool".to_string()),
                custom_type: HARNESS_DIGEST_CUSTOM_TYPE.to_string(),
                content: pi_agent_core::types::CustomMessageContent::Text("digest".to_string()),
                details: None,
                display: false,
                timestamp: "1970-01-01T00:00:04.000Z".to_string(),
            },
            SessionEntry::CustomMessage {
                id: "note".to_string(),
                parent_id: Some("digest".to_string()),
                custom_type: "note".to_string(),
                content: pi_agent_core::types::CustomMessageContent::Text("note".to_string()),
                details: None,
                display: true,
                timestamp: "1970-01-01T00:00:05.000Z".to_string(),
            },
        ];
        let prepared = prepare_branch_entries(&entries, 0.0);
        assert_eq!(prepared.messages.len(), 3);
        assert_eq!(prepared.messages[0].role(), "user");
        assert_eq!(prepared.messages[2].role(), "custom");
        assert!(prepared.total_tokens > 0.0);
    }

    #[test]
    fn prepare_branch_entries_keeps_the_newest_messages_within_budget() {
        let mut entries = Vec::new();
        let mut parent: Option<String> = None;
        for index in 0..6 {
            let entry = SessionEntry::Message {
                id: format!("m{index}"),
                parent_id: parent.clone(),
                message: AgentMessage::Message(Message::User(pi_ai::types::UserMessage::new(
                    pi_ai::types::UserContent::Text("x".repeat(40)),
                    1,
                ))),
            };
            parent = Some(entry.id().to_string());
            entries.push(entry);
        }
        let prepared = prepare_branch_entries(&entries, 20.0);
        // Each message estimates to 10 tokens; the budget keeps only the newest two.
        assert_eq!(prepared.messages.len(), 2);
        assert_eq!(prepared.total_tokens, 20.0);
    }

    #[test]
    fn prepare_branch_entries_collects_nested_summary_file_ops() {
        let entries = vec![
            SessionEntry::BranchSummary {
                id: "s1".to_string(),
                parent_id: None,
                from_id: "from".to_string(),
                summary: "sum".to_string(),
                details: Some(serde_json::json!({
                    "readFiles": ["b.ts", "a.ts"],
                    "modifiedFiles": ["c.ts"],
                })),
                from_hook: None,
                timestamp: "1970-01-01T00:00:01.000Z".to_string(),
            },
            SessionEntry::BranchSummary {
                id: "s2".to_string(),
                parent_id: Some("s1".to_string()),
                from_id: "from".to_string(),
                summary: "sum".to_string(),
                details: Some(serde_json::json!({
                    "readFiles": ["ignored.ts"],
                    "modifiedFiles": ["ignored.ts"],
                })),
                from_hook: Some(true),
                timestamp: "1970-01-01T00:00:02.000Z".to_string(),
            },
        ];
        let prepared = prepare_branch_entries(&entries, 0.0);
        assert_eq!(
            prepared.file_ops.read.iter().cloned().collect::<Vec<_>>(),
            vec!["a.ts".to_string(), "b.ts".to_string()]
        );
        assert_eq!(
            prepared.file_ops.edited.iter().cloned().collect::<Vec<_>>(),
            vec!["c.ts".to_string()]
        );
    }

    #[test]
    fn empty_entries_return_no_content_to_summarize() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(generate_branch_summary(
            &[],
            GenerateBranchSummaryOptions {
                model: Model::default(),
                api_key: String::new(),
                headers: None,
                signal: None,
                custom_instructions: None,
                replace_instructions: None,
                retry: None,
                reserve_tokens: None,
            },
        ));
        assert_eq!(result.summary.as_deref(), Some("No content to summarize"));
        assert!(result.read_files.is_none());
    }

    #[test]
    fn assistant_entries_contribute_tool_call_file_ops() {
        let entry = SessionEntry::Message {
            id: "a".to_string(),
            parent_id: None,
            message: AgentMessage::Message(Message::Assistant(AssistantMessage {
                content: vec![ContentBlock::ToolCall(ToolCall::new("id", "edit", {
                    let mut args = serde_json::Map::new();
                    args.insert("path".to_string(), serde_json::json!("edited.ts"));
                    args
                }))],
                timestamp: 1,
                ..Default::default()
            })),
        };
        let prepared = prepare_branch_entries(&[entry], 0.0);
        assert_eq!(
            prepared.file_ops.edited.iter().cloned().collect::<Vec<_>>(),
            vec!["edited.ts".to_string()]
        );
    }
}
