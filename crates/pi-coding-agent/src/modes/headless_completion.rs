//! Port of packages/coding-agent/src/modes/headless-completion.ts

use std::sync::Arc;

use pi_agent_core::types::AgentMessage;
use pi_ai::types::{AssistantMessage, BoxFuture, Message, STOP_REASON_ABORTED, STOP_REASON_ERROR};
use serde_json::Value;

use crate::core::messages::{
    is_compaction_outcome_message, is_session_slash_command_result_message, COMPACTION_OUTCOME_CUSTOM_TYPE,
    HARNESS_DIGEST_CUSTOM_TYPE, REFINEMENT_NOTICE_CUSTOM_TYPE, REFINEMENT_OUTCOME_CUSTOM_TYPE,
};
use crate::modes::agent_connection::types::{
    AgentAutonomousGateFailure, AgentAutonomousStatus,
};

/// `latestAutonomousGateAttempt(status)`.
pub fn latest_autonomous_gate_attempt(status: &AgentAutonomousStatus) -> f64 {
    let mut attempt = status.last_gate_failure.as_ref().map(|failure| failure.attempt).unwrap_or(0.0);
    attempt = attempt.max(0.0);
    for value in status.gate_attempts.values() {
        attempt = attempt.max(*value);
    }
    attempt
}

/// `HeadlessTerminalResultMessage`.
pub enum HeadlessTerminalResultMessage {
    Assistant(Box<AssistantMessage>),
    /// `SessionSlashCommandResultMessage`, carried as its wire object.
    SessionSlashCommandResult(Value),
}

/// `CompactionOutcomeMessage`, carried as its wire object.
pub type CompactionOutcomeMessage = Value;

/// `HeadlessTerminalResult`.
#[derive(Default)]
pub struct HeadlessTerminalResult {
    pub primary: Option<HeadlessTerminalResultMessage>,
    pub compaction_outcomes: Vec<CompactionOutcomeMessage>,
}

/// `selectHeadlessTerminalResult(messages)`.
pub fn select_headless_terminal_result(messages: &[AgentMessage]) -> HeadlessTerminalResult {
    let mut index = messages.len() as i64 - 1;
    let mut compaction_outcomes: Vec<CompactionOutcomeMessage> = Vec::new();
    while index >= 0 {
        let message = &messages[index as usize];
        let value = message_to_value(message);
        if is_compaction_outcome_message(&value) {
            compaction_outcomes.insert(0, value);
            index -= 1;
            continue;
        }
        // A corrupt outcome is still part of the terminal outcome suffix. Skip it
        // without letting it hide earlier valid outcomes or their failure status.
        if value.get("role").and_then(Value::as_str) == Some("custom") {
            let custom_type = value.get("customType").and_then(Value::as_str).unwrap_or_default();
            if custom_type == COMPACTION_OUTCOME_CUSTOM_TYPE
                || custom_type == REFINEMENT_OUTCOME_CUSTOM_TYPE
                || custom_type == REFINEMENT_NOTICE_CUSTOM_TYPE
                || custom_type == HARNESS_DIGEST_CUSTOM_TYPE
            {
                index -= 1;
                continue;
            }
        }
        break;
    }
    let preceding = if index >= 0 {
        Some(&messages[index as usize])
    } else {
        None
    };
    let primary = preceding.and_then(|message| match message {
        AgentMessage::Message(Message::Assistant(assistant)) => {
            Some(HeadlessTerminalResultMessage::Assistant(Box::new(assistant.clone())))
        }
        other => {
            let value = message_to_value(other);
            if is_session_slash_command_result_message(&value) {
                Some(HeadlessTerminalResultMessage::SessionSlashCommandResult(value))
            } else {
                None
            }
        }
    });
    HeadlessTerminalResult {
        primary,
        compaction_outcomes,
    }
}

fn message_to_value(message: &AgentMessage) -> Value {
    serde_json::to_value(message).unwrap_or(Value::Null)
}

fn should_continue_autonomous_gates(status: &AgentAutonomousStatus) -> bool {
    status.enabled
        && !status.gates.commands.is_empty()
        && status.last_gate_failure.is_some()
        && latest_autonomous_gate_attempt(status) <= status.gates.max_retries
        && crate::modes::agent_connection::types::autonomous_limit_reason(status).is_none()
}

fn autonomous_progress_key(status: &AgentAutonomousStatus) -> String {
    let exit_text = status
        .last_gate_failure
        .as_ref()
        .map(|failure| failure.exit_text.clone())
        .unwrap_or_default();
    format!(
        "{}:{}:{}:{}:{}",
        latest_autonomous_gate_attempt(status),
        status.continuations_used,
        status.turns_used,
        status.tokens_used,
        exit_text
    )
}

/// `HeadlessCompletionOptions`.
#[derive(Debug, Clone, Default)]
pub struct HeadlessCompletionOptions {
    /// Include descendant settlement and the parent turns caused by their results.
    pub wait_for_rlm_quiescence: Option<bool>,
}

/// The `AgentSession` surface `waitForHeadlessCompletion` uses.
///
/// blocked_on: `core/agent-session.ts` belongs to another slice, so the port
/// drives the exact same members through an explicit seam.
pub trait HeadlessCompletionSession: Send + Sync {
    /// `session.waitForRlmQuiescence()`.
    fn wait_for_rlm_quiescence(&self) -> BoxFuture<()>;
    /// `session.waitForHeadlessIdle()`.
    fn wait_for_headless_idle(&self) -> BoxFuture<()>;
    /// `session.getAutonomousStatus()`.
    fn get_autonomous_status(&self) -> AgentAutonomousStatus;
    /// `session.recordHostAutonomousContinuation()`.
    fn record_host_autonomous_continuation(&self);
    /// `session.prompt(text, { streamingBehavior, internalPrompt, suppressAutonomousContinuation })`.
    fn prompt_headless_continuation(&self, text: String) -> BoxFuture<Result<(), String>>;
    /// `session.waitForIdle()`.
    fn wait_for_idle(&self) -> BoxFuture<()>;
    /// `session.refreshAutonomousGates()`.
    fn refresh_autonomous_gates(&self) -> BoxFuture<()>;
    /// `session.state.messages`.
    fn state_messages(&self) -> Vec<AgentMessage>;
}

/// `waitForHeadlessCompletion(session, options)`.
pub async fn wait_for_headless_completion(
    session: Arc<dyn HeadlessCompletionSession>,
    options: HeadlessCompletionOptions,
) -> AgentAutonomousStatus {
    let mut last_prompted_progress_key: Option<String> = None;
    let mut repeated_progress_prompts: i64 = 0;
    loop {
        if options.wait_for_rlm_quiescence.unwrap_or(false) {
            session.wait_for_rlm_quiescence().await;
        } else {
            session.wait_for_headless_idle().await;
        }
        let status = session.get_autonomous_status();
        if !should_continue_autonomous_gates(&status) {
            return status;
        }
        let Some(last_gate_failure) = status.last_gate_failure.clone() else {
            return status;
        };
        let progress_key = autonomous_progress_key(&status);
        if Some(&progress_key) == last_prompted_progress_key.as_ref() {
            repeated_progress_prompts += 1;
        } else {
            repeated_progress_prompts = 0;
            last_prompted_progress_key = Some(progress_key);
        }
        if repeated_progress_prompts > 0 {
            let delay_ms = (repeated_progress_prompts * 50).min(1000);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms as u64)).await;
        }
        session.record_host_autonomous_continuation();
        let attempt = latest_autonomous_gate_attempt(&status);
        session
            .prompt_headless_continuation(build_autonomous_gate_failure_continuation(
                &AgentAutonomousGateFailure {
                    attempt,
                    ..last_gate_failure
                },
                status.gates.max_retries,
                now_ms(),
            ))
            .await
            .ok();
        session.wait_for_idle().await;
        session.refresh_autonomous_gates().await;
        let result = select_headless_terminal_result(&session.state_messages());
        if let Some(HeadlessTerminalResultMessage::Assistant(primary)) = &result.primary {
            if primary.stop_reason == STOP_REASON_ERROR || primary.stop_reason == STOP_REASON_ABORTED {
                let post_error_status = session.get_autonomous_status();
                if should_continue_autonomous_gates(&post_error_status) && post_error_status.last_gate_failure.is_some()
                {
                    continue;
                }
                if options.wait_for_rlm_quiescence.unwrap_or(false) {
                    continue;
                }
                return post_error_status;
            }
        }
    }
}

/// `buildAutonomousGateFailureContinuation(failure, maxRetries, timestamp)`.
pub fn build_autonomous_gate_failure_continuation(
    failure: &AgentAutonomousGateFailure,
    max_retries: f64,
    timestamp: i64,
) -> String {
    let output = if failure.output.is_empty() {
        "\n".to_string()
    } else {
        format!("\nOutput:\n{}\n", failure.output)
    };
    format!(
        "Autonomous quality gate failed (attempt {}/{}): `{}` {}.\n{}\nContinue working. Fix the failure, then produce terminal evidence. Timestamp: {}.",
        failure.attempt,
        max_retries,
        failure.command,
        failure.exit_text,
        output,
        iso_from_millis(timestamp)
    )
}

/// `new Date(timestamp).toISOString()`.
fn iso_from_millis(timestamp: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(timestamp) {
        Some(datetime) => datetime.to_utc().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        None => String::new(),
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Re-export so callers of this module can read a `status` limit.
pub use crate::modes::agent_connection::types::autonomous_limit_reason;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use pi_ai::types::{ContentBlock, Usage, STOP_REASON_STOP};

    fn assistant(stop_reason: &str, text: &str) -> AgentMessage {
        AgentMessage::Message(Message::Assistant(AssistantMessage {
            role: "assistant".to_string(),
            content: if text.is_empty() {
                Vec::new()
            } else {
                vec![ContentBlock::Text(pi_ai::types::TextContent {
                    type_: "text".to_string(),
                    text: text.to_string(),
                    text_signature: None,
                })]
            },
            api: String::new(),
            provider: String::new(),
            model: "m".to_string(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: stop_reason.to_string(),
            stop_reason_raw: None,
            error_message: None,
            timestamp: 1,
        }))
    }

    fn status_with_gate() -> AgentAutonomousStatus {
        AgentAutonomousStatus {
            enabled: true,
            continuations_used: 0.0,
            turns_used: 0.0,
            tokens_used: 0.0,
            started_at: None,
            limits: crate::modes::agent_connection::types::AgentAutonomousLimits {
                max_continuations: 10.0,
                max_turns: 10.0,
                max_tokens: 10.0,
                timeout_ms: 10.0,
            },
            gates: crate::modes::agent_connection::types::AgentAutonomousGateStatus {
                commands: vec!["npm test".to_string()],
                max_retries: 2.0,
                timeout_ms: 1000.0,
            },
            gate_attempts: BTreeMap::new(),
            last_gate_failure: Some(AgentAutonomousGateFailure {
                command: "npm test".to_string(),
                attempt: 1.0,
                exit_text: "exit 1".to_string(),
                output: "boom".to_string(),
            }),
        }
    }

    #[test]
    fn latest_attempt_uses_the_gate_attempts_map() {
        let mut status = status_with_gate();
        status.gate_attempts.insert("npm test".to_string(), 4.0);
        assert_eq!(latest_autonomous_gate_attempt(&status), 4.0);
        status.gate_attempts.clear();
        assert_eq!(latest_autonomous_gate_attempt(&status), 1.0);
        status.last_gate_failure = None;
        assert_eq!(latest_autonomous_gate_attempt(&status), 0.0);
    }

    #[test]
    fn terminal_result_prefers_the_assistant_message() {
        let messages = vec![assistant(STOP_REASON_STOP, "done")];
        let result = select_headless_terminal_result(&messages);
        match result.primary {
            Some(HeadlessTerminalResultMessage::Assistant(message)) => {
                assert_eq!(message.stop_reason, STOP_REASON_STOP);
                assert_eq!(message.content.len(), 1);
            }
            _ => panic!("expected an assistant message"),
        }
        assert!(result.compaction_outcomes.is_empty());
    }

    #[test]
    fn terminal_result_skips_trailing_outcome_messages() {
        let outcome = serde_json::json!({
            "role": "custom",
            "customType": crate::core::messages::COMPACTION_OUTCOME_CUSTOM_TYPE,
            "content": "compacted",
            "display": false,
            "details": {"outcome": "failed", "reason": "threshold"},
            "timestamp": 1,
        });
        let messages = vec![
            assistant(STOP_REASON_STOP, "done"),
            AgentMessage::Custom(serde_json::from_value(outcome).unwrap()),
        ];
        let result = select_headless_terminal_result(&messages);
        assert_eq!(result.compaction_outcomes.len(), 1);
        assert!(matches!(
            result.primary,
            Some(HeadlessTerminalResultMessage::Assistant(_))
        ));
    }

    #[test]
    fn terminal_result_ignores_a_corrupt_outcome() {
        let corrupt = serde_json::json!({
            "role": "custom",
            "customType": crate::core::messages::COMPACTION_OUTCOME_CUSTOM_TYPE,
            "content": "compacted",
            "display": false,
            "timestamp": 1,
        });
        let messages = vec![
            assistant(STOP_REASON_STOP, "done"),
            AgentMessage::Custom(serde_json::from_value(corrupt).unwrap()),
        ];
        let result = select_headless_terminal_result(&messages);
        assert!(result.compaction_outcomes.is_empty());
        assert!(matches!(
            result.primary,
            Some(HeadlessTerminalResultMessage::Assistant(_))
        ));
    }

    #[test]
    fn gate_failure_continuation_matches_the_typescript() {
        let text = build_autonomous_gate_failure_continuation(
            &AgentAutonomousGateFailure {
                command: "npm test".to_string(),
                attempt: 2.0,
                exit_text: "exit 1".to_string(),
                output: "boom".to_string(),
            },
            3.0,
            0,
        );
        assert!(text.starts_with("Autonomous quality gate failed (attempt 2/3): `npm test` exit 1.\n\nOutput:\nboom\n\n"));
        assert!(text.ends_with("Timestamp: 1970-01-01T00:00:00.000Z."));
    }

    #[test]
    fn gate_failure_continuation_omits_empty_output() {
        let text = build_autonomous_gate_failure_continuation(
            &AgentAutonomousGateFailure {
                command: "npm test".to_string(),
                attempt: 1.0,
                exit_text: "exit 1".to_string(),
                output: String::new(),
            },
            1.0,
            0,
        );
        assert!(text.contains("exit 1.\n\nContinue working."));
    }
}
