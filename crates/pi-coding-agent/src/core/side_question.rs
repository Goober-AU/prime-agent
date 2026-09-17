//! Port of packages/coding-agent/src/core/side-question.ts

pub(crate) mod native;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_agent_core::types::{AgentEvent, AgentMessage, AgentState, StreamFn};
use pi_ai::types::{AssistantMessage, Usage, UserMessage, STOP_REASON_STOP};
use pi_ai::utils::diagnostics::{create_assistant_message_diagnostic, ThrownValue};
use tokio_util::sync::CancellationToken;

use crate::core::provider_retry::{
    complete_with_provider_retry, default_provider_retry_policy, ProviderRetryExecutionOptions,
    ProviderRetryPolicy,
};
use crate::core::semantic_edges::unwrap_semantic_edge_stream_fn;

pub type BoxFuture<T> = pi_ai::types::BoxFuture<T>;

/// `type SideQuestionStatus = "running" | "complete" | "cancelled" | "error"`.
pub const SIDE_QUESTION_STATUS_RUNNING: &str = "running";
pub const SIDE_QUESTION_STATUS_COMPLETE: &str = "complete";
pub const SIDE_QUESTION_STATUS_CANCELLED: &str = "cancelled";
pub const SIDE_QUESTION_STATUS_ERROR: &str = "error";

/// `interface SideQuestionEvent`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideQuestionEvent {
    pub id: String,
    pub question: String,
    pub answer: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// `interface SideQuestionTurn`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideQuestionTurn {
    pub question: String,
    pub answer: String,
}

/// `interface SideQuestionRun { done: Promise<void>; abort(): void }`.
///
/// Not `Clone`: `done` is a boxed future, which cannot be duplicated.
pub struct SideQuestionRun {
    pub done: BoxFuture<()>,
    pub abort: Arc<dyn Fn() + Send + Sync>,
}

/// `const SIDE_QUESTION_INSTRUCTION`.
const SIDE_QUESTION_INSTRUCTION: &str = "The user asked this via `/btw` — a temporary side thread cloned from the main conversation to answer a question without interrupting the main work. Tools (including `ipython`) are deactivated in this side thread and return an error if called; answer using only the conversation context above. The user may send follow-up side questions. Nothing here is added to the main session, so don't start or plan main-session work from this thread.";
pub(super) const SIDE_QUESTION_TOOL_BLOCKED: &str = "Tools are deactivated in this side thread. Answer from the conversation context.";

/// `sideQuestionPrompt(question, isFirstTurn)`.
pub fn side_question_prompt(question: &str, is_first_turn: bool) -> String {
    let body = if is_first_turn {
        format!("{SIDE_QUESTION_INSTRUCTION}\n\n{question}")
    } else {
        question.to_string()
    };
    format!("<side_question>\n{body}\n</side_question>")
}

/// `readAssistantText(message)`.
pub fn read_assistant_text(message: &AgentMessage) -> String {
    let AgentMessage::Message(pi_ai::types::Message::Assistant(assistant)) = message else {
        return String::new();
    };
    assistant
        .content
        .iter()
        .filter_map(|block| block.as_text())
        .map(|text| text.text.clone())
        .collect::<Vec<String>>()
        .join("")
}

/// The `parent: Agent` argument of `startSideQuestion`.
///
/// `Agent` belongs to the pi-agent-core slice; the side question reads only these
/// members from the parent, so the port keeps a minimal seam that mirrors the
/// exact TypeScript members it calls.
pub trait SideQuestionParent: Send + Sync {
    fn state(&self) -> AgentState;
    fn convert_to_llm(
        &self,
    ) -> Option<Arc<dyn Fn(Vec<AgentMessage>) -> Vec<pi_ai::types::Message> + Send + Sync>>;
    fn transform_context(
        &self,
    ) -> Option<
        Arc<dyn Fn(Vec<AgentMessage>, Option<CancellationToken>) -> Vec<AgentMessage> + Send + Sync>,
    >;
    fn stream_fn(&self) -> StreamFn;
    fn get_api_key(&self) -> Option<Arc<dyn Fn(String) -> Option<String> + Send + Sync>>;
    fn on_payload(&self) -> Option<pi_ai::types::OnPayload>;
    fn on_response(&self) -> Option<pi_ai::types::OnResponse>;
    fn tool_execution(&self) -> Option<String>;
    fn session_id(&self) -> Option<String>;
    fn thinking_budgets(&self) -> Option<pi_ai::types::ThinkingBudgets>;
}

/// The side `Agent` the side question drives (`new Agent({...})` in the TypeScript).
pub trait SideQuestionAgent: Send + Sync {
    fn state(&self) -> AgentState;
    fn set_state(&self, state: AgentState);
    fn subscribe(
        &self,
        listener: Arc<dyn Fn(AgentEvent, Option<CancellationToken>) -> BoxFuture<()> + Send + Sync>,
    ) -> Arc<dyn Fn() + Send + Sync>;
    fn prompt(&self, messages: Vec<AgentMessage>) -> BoxFuture<anyhow::Result<()>>;
    fn continue_(&self) -> BoxFuture<Result<(), String>>;
    fn abort(&self);
}

/// `new Agent({ initialState, convertToLlm, transformContext, streamFn, getApiKey,
/// onPayload, onResponse, shouldStopAfterTurn, sessionId, thinkingBudgets,
/// transport, toolExecution })`.
/// Not `Default`: the required `shouldStopAfterTurn` callback has no default
/// value, and every construction site supplies the full option set.
#[derive(Clone)]
pub struct SideQuestionAgentOptions {
    pub initial_state: AgentState,
    pub convert_to_llm: Option<Arc<dyn Fn(Vec<AgentMessage>) -> Vec<pi_ai::types::Message> + Send + Sync>>,
    pub transform_context:
        Option<Arc<dyn Fn(Vec<AgentMessage>, Option<CancellationToken>) -> Vec<AgentMessage> + Send + Sync>>,
    pub stream_fn: Option<StreamFn>,
    pub get_api_key: Option<Arc<dyn Fn(String) -> Option<String> + Send + Sync>>,
    pub on_payload: Option<pi_ai::types::OnPayload>,
    pub on_response: Option<pi_ai::types::OnResponse>,
    /// `shouldStopAfterTurn: () => true`.
    pub should_stop_after_turn: Arc<dyn Fn(pi_agent_core::types::ShouldStopAfterTurnContext) -> bool + Send + Sync>,
    pub session_id: Option<String>,
    pub thinking_budgets: Option<pi_ai::types::ThinkingBudgets>,
    /// `transport: "sse"`.
    pub transport: String,
    pub tool_execution: Option<String>,
}

/// `new Agent(options)`.
pub type SideQuestionAgentFactory =
    Arc<dyn Fn(SideQuestionAgentOptions) -> Arc<dyn SideQuestionAgent> + Send + Sync>;

/// `startSideQuestion(parent, id, question, onEvent, previousTurns = [], retry = DEFAULT_PROVIDER_RETRY_POLICY)`.
///
/// Returns `Err` where the TypeScript throws
/// (`Select a model before asking a side question`).
pub fn start_side_question(
    parent: Arc<dyn SideQuestionParent>,
    agent_factory: SideQuestionAgentFactory,
    id: String,
    question: String,
    on_event: Arc<dyn Fn(SideQuestionEvent) -> BoxFuture<()> + Send + Sync>,
    previous_turns: Option<Vec<SideQuestionTurn>>,
    retry: Option<ProviderRetryPolicy>,
) -> Result<SideQuestionRun, String> {
    let parent_state = parent.state();
    let model = parent_state.model.clone();
    if model.id.is_empty() {
        return Err("Select a model before asking a side question".to_string());
    }

    let previous_turns = previous_turns.unwrap_or_default();

    // Each turn re-clones the live main conversation, so follow-ups always see
    // the newest main-thread context; earlier side turns are replayed after it.
    let previous_turn_messages: Vec<AgentMessage> = previous_turns
        .iter()
        .enumerate()
        .flat_map(|(index, turn)| {
            let user: AgentMessage = UserMessage::new(
                pi_ai::types::UserContent::Blocks(vec![pi_ai::types::ImageOrTextContent::Text(
                    pi_ai::types::TextContent::new(side_question_prompt(
                        &turn.question,
                        index == 0,
                    )),
                )]),
                now_millis(),
            )
            .into();
            let assistant: AgentMessage = AssistantMessage {
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                content: vec![pi_ai::types::ContentBlock::Text(pi_ai::types::TextContent::new(
                    turn.answer.clone(),
                ))],
                usage: Usage::zero(),
                stop_reason: STOP_REASON_STOP.to_string(),
                timestamp: now_millis(),
                ..Default::default()
            }
            .into();
            [user, assistant]
        })
        .collect();

    let mut initial_messages = parent_state.messages.clone();
    initial_messages.extend(previous_turn_messages);
    let cloned_message_count = initial_messages.len();
    let turn_count = AtomicUsize::new(0);
    let preferred_effort = match parent_state.thinking_level {
        pi_agent_core::types::ThinkingLevel::Off => "off",
        pi_agent_core::types::ThinkingLevel::Minimal => "minimal",
        _ => "low",
    };
    let auxiliary_effort = serde_json::from_value(serde_json::Value::String(
        pi_ai::models::clamp_thinking_level(&model, preferred_effort),
    )).unwrap_or(pi_agent_core::types::ThinkingLevel::Off);
    let side_agent = agent_factory(SideQuestionAgentOptions {
        initial_state: AgentState {
            model: model.clone(),
            system_prompt: parent_state.system_prompt.clone(),
            messages: initial_messages,
            thinking_level: auxiliary_effort,
            service_tier: parent_state.service_tier.clone(),
            tools: parent_state.tools.clone(),
            ..Default::default()
        },
        convert_to_llm: parent.convert_to_llm(),
        transform_context: parent.transform_context(),
        // Side questions are excluded from session history; their calls carry no provenance.
        stream_fn: Some(unwrap_semantic_edge_stream_fn(&parent.stream_fn())),
        get_api_key: parent.get_api_key(),
        on_payload: parent.on_payload(),
        on_response: parent.on_response(),
        should_stop_after_turn: Arc::new(move |context| {
            turn_count.fetch_add(1, Ordering::SeqCst) + 1 >= 3
                || !context.message.content.iter().any(|block| matches!(block, pi_ai::types::ContentBlock::ToolCall(_)))
        }),
        session_id: parent.session_id(),
        thinking_budgets: parent.thinking_budgets(),
        transport: "sse".to_string(),
        tool_execution: parent.tool_execution(),
    });

    let answer = Arc::new(Mutex::new(String::new()));
    let abort_requested = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicBool::new(false));
    let retry_abort_controller = CancellationToken::new();

    // `const emit = (status, errorMessage?) => onEvent({ id, question, answer, status, ...(errorMessage ? { errorMessage } : {}) })`.
    let emit = {
        let on_event = Arc::clone(&on_event);
        let id = id.clone();
        let question = question.clone();
        let answer = Arc::clone(&answer);
        Arc::new(move |status: &str, error_message: Option<String>| {
            let event = SideQuestionEvent {
                id: id.clone(),
                question: question.clone(),
                answer: answer.lock().expect("side question answer poisoned").clone(),
                status: status.to_string(),
                error_message,
            };
            on_event(event)
        })
    };

    // `sideAgent.subscribe(async (event) => { if (event.type !== "message_update" && event.type !== "message_end") return; ... })`.
    let unsubscribe = side_agent.subscribe({
        let answer = Arc::clone(&answer);
        let emit = Arc::clone(&emit);
        Arc::new(move |event: AgentEvent, _signal: Option<CancellationToken>| {
            let message = match &event {
                AgentEvent::MessageUpdate { message, .. } => message.clone(),
                AgentEvent::MessageEnd { message } => message.clone(),
                _ => return Box::pin(async {}) as BoxFuture<()>,
            };
            let next_answer = read_assistant_text(&message);
            {
                let mut current = answer.lock().expect("side question answer poisoned");
                if next_answer.is_empty() || *current == next_answer {
                    return Box::pin(async {}) as BoxFuture<()>;
                }
                *current = next_answer;
            }
            let emit = Arc::clone(&emit);
            Box::pin(async move {
                emit(SIDE_QUESTION_STATUS_RUNNING, None).await;
            }) as BoxFuture<()>
        })
    });


    let prompt = side_question_prompt(&question, previous_turns.is_empty());
    let done = {
        let side_agent = Arc::clone(&side_agent);
        let emit = Arc::clone(&emit);
        let abort_requested = Arc::clone(&abort_requested);
        let started = Arc::clone(&started);
        let retry_abort_controller = retry_abort_controller.clone();
        let retry = retry.unwrap_or_else(default_provider_retry_policy);
        let answer = Arc::clone(&answer);
        Box::pin(async move {
            emit(SIDE_QUESTION_STATUS_RUNNING, None).await;
            if abort_requested.load(Ordering::SeqCst) {
                emit(SIDE_QUESTION_STATUS_CANCELLED, None).await;
                return;
            }
            started.store(true, Ordering::SeqCst);
            // Standalone side agents bypass the session auto-retry loop; retry here instead.
            let prompted_once = Arc::new(AtomicBool::new(false));
            let attempt_agent = Arc::clone(&side_agent);
            let attempt_prompted = Arc::clone(&prompted_once);
            let attempt_prompt = prompt.clone();
            let message = complete_with_provider_retry(
                move || {
                    let side_agent = Arc::clone(&attempt_agent);
                    let prompted_once = Arc::clone(&attempt_prompted);
                    let prompt = attempt_prompt.clone();
                    async move {
                        // The TypeScript closure lets a rejected `continue()`/`prompt()`
                        // escape `completeWithProviderRetry`, so the error is captured here
                        // (never discarded) and turned into a fail-fast failure below.
                        let failure: Option<String> = if prompted_once.swap(true, Ordering::SeqCst) {
                            // Session-loop recovery: drop the failed assistant turn and re-run.
                            let mut state = side_agent.state();
                            state.messages.pop();
                            side_agent.set_state(state);
                            side_agent.continue_().await.err().map(|error| error.to_string())
                        } else {
                            side_agent
                                .prompt(vec![UserMessage::new(
                                    pi_ai::types::UserContent::Blocks(vec![
                                        pi_ai::types::ImageOrTextContent::Text(
                                            pi_ai::types::TextContent::new(prompt),
                                        ),
                                    ]),
                                    now_millis(),
                                )
                                .into()])
                                .await
                                .err()
                                .map(|error| error.to_string())
                        };
                        let state = side_agent.state();
                        match state.messages.iter().skip(cloned_message_count).rev().find(|message| matches!(message, AgentMessage::Message(pi_ai::types::Message::Assistant(_)))) {
                            Some(AgentMessage::Message(pi_ai::types::Message::Assistant(assistant)))
                                if failure.is_none() =>
                            {
                                assistant.clone()
                            }
                            _ => {
                                // `throw new Error(sideAgent.state.errorMessage || "Side question
                                // produced no assistant message")` rejects
                                // `completeWithProviderRetry` with no retry at all. Marking the
                                // message as an agent lifecycle failure keeps that fail-fast
                                // behavior: the shared retry loop returns it immediately instead
                                // of re-entering the closure, where the recovery branch would pop
                                // the user prompt that is now the last message.
                                // `sideAgent.state.errorMessage || "Side question produced no
                                // assistant message"`.
                                let error_message = failure
                                    .or_else(|| state.error_message.clone())
                                    .unwrap_or_else(|| {
                                        "Side question produced no assistant message".to_string()
                                    });
                                AssistantMessage {
                                    diagnostics: Some(vec![create_assistant_message_diagnostic(
                                        "agent_lifecycle_failure",
                                        &ThrownValue::Text(&error_message),
                                        Some(lifecycle_failure_details()),
                                    )]),
                                    stop_reason: pi_ai::types::STOP_REASON_ERROR.to_string(),
                                    error_message: Some(error_message),
                                    ..Default::default()
                                }
                            }
                        }
                    }
                },
                ProviderRetryExecutionOptions {
                    policy: Some(retry),
                    signal: Some(retry_abort_controller),
                    ..Default::default()
                },
            )
            .await;
            if abort_requested.load(Ordering::SeqCst) {
                emit(SIDE_QUESTION_STATUS_CANCELLED, None).await;
                return;
            }
            if message.stop_reason == pi_ai::types::STOP_REASON_ERROR {
                emit(
                    SIDE_QUESTION_STATUS_ERROR,
                    Some(message.error_message.clone().unwrap_or_default()),
                )
                .await;
                return;
            }
            if let Some(error_message) = side_agent.state().error_message.clone() {
                emit(SIDE_QUESTION_STATUS_ERROR, Some(error_message)).await;
                return;
            }
            let final_answer = if message.content.iter().any(|block| matches!(block, pi_ai::types::ContentBlock::ToolCall(_))) {
                side_agent.state().messages.iter().skip(cloned_message_count).rev()
                    .map(read_assistant_text).find(|text| !text.is_empty()).unwrap_or_default()
            } else {
                read_assistant_text(&AgentMessage::Message(pi_ai::types::Message::Assistant(message)))
            };
            *answer.lock().expect("side question answer poisoned") = final_answer;
            emit(SIDE_QUESTION_STATUS_COMPLETE, None).await;
        })
    };

    let abort = {
        let side_agent = Arc::clone(&side_agent);
        let abort_requested = Arc::clone(&abort_requested);
        let started = Arc::clone(&started);
        let retry_abort_controller = retry_abort_controller.clone();
        Arc::new(move || {
            abort_requested.store(true, Ordering::SeqCst);
            retry_abort_controller.cancel();
            if started.load(Ordering::SeqCst) {
                side_agent.abort();
            }
        })
    };

    // `.finally(unsubscribe)`: the listener stays attached for the run's lifetime.
    let done = {
        let unsubscribe = Arc::clone(&unsubscribe);
        let done: BoxFuture<()> = done;
        Box::pin(async move {
            done.await;
            unsubscribe();
        })
    };

    Ok(SideQuestionRun { done, abort })
}

/// `{ source: "run_with_lifecycle" }` - the diagnostic detail
/// `Agent.handleRunFailure` attaches to its non-retryable failure message
/// (packages/agent/src/agent.ts:533).
fn lifecycle_failure_details() -> serde_json::Map<String, serde_json::Value> {
    let mut details = serde_json::Map::new();
    details.insert(
        "source".to_string(),
        serde_json::Value::String("run_with_lifecycle".to_string()),
    );
    details
}

/// `Date.now()`.
fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{ContentBlock, TextContent};
    use serde_json::Value;

    #[test]
    fn first_turn_includes_the_instruction() {
        assert_eq!(
            side_question_prompt("why?", true),
            format!(
                "<side_question>\n{}\n\nwhy?\n</side_question>",
                SIDE_QUESTION_INSTRUCTION
            )
        );
    }

    #[test]
    fn follow_up_turn_is_the_question_only() {
        assert_eq!(
            side_question_prompt("again?", false),
            "<side_question>\nagain?\n</side_question>"
        );
    }

    #[test]
    fn read_assistant_text_joins_text_blocks() {
        let message: AgentMessage = AssistantMessage {
            content: vec![
                ContentBlock::Text(TextContent::new("a")),
                ContentBlock::Thinking(pi_ai::types::ThinkingContent::default()),
                ContentBlock::Text(TextContent::new("b")),
            ],
            ..Default::default()
        }
        .into();
        assert_eq!(read_assistant_text(&message), "ab");
    }

    #[test]
    fn read_assistant_text_ignores_non_assistant_messages() {
        let message: AgentMessage =
            UserMessage::new(pi_ai::types::UserContent::Text("hi".to_string()), 0).into();
        assert_eq!(read_assistant_text(&message), "");
    }

    #[test]
    fn statuses_match_the_typescript_literals() {
        assert_eq!(SIDE_QUESTION_STATUS_RUNNING, "running");
        assert_eq!(SIDE_QUESTION_STATUS_COMPLETE, "complete");
        assert_eq!(SIDE_QUESTION_STATUS_CANCELLED, "cancelled");
        assert_eq!(SIDE_QUESTION_STATUS_ERROR, "error");
    }

    /// A side agent that records the prompt but never appends an assistant
    /// message: exactly the failure the TypeScript closure throws on.
    struct NoAssistantAgent {
        state: Mutex<AgentState>,
        prompt_calls: std::sync::atomic::AtomicUsize,
        continue_calls: std::sync::atomic::AtomicUsize,
    }

    impl SideQuestionAgent for NoAssistantAgent {
        fn state(&self) -> AgentState {
            self.state.lock().expect("fake state poisoned").clone()
        }

        fn set_state(&self, state: AgentState) {
            *self.state.lock().expect("fake state poisoned") = state;
        }

        fn subscribe(
            &self,
            _listener: Arc<dyn Fn(AgentEvent, Option<CancellationToken>) -> BoxFuture<()> + Send + Sync>,
        ) -> Arc<dyn Fn() + Send + Sync> {
            Arc::new(|| {})
        }

        fn prompt(&self, messages: Vec<AgentMessage>) -> BoxFuture<anyhow::Result<()>> {
            self.prompt_calls.fetch_add(1, Ordering::SeqCst);
            let mut state = self.state.lock().expect("fake state poisoned");
            state.messages.extend(messages);
            Box::pin(async { Ok(()) })
        }

        fn continue_(&self) -> BoxFuture<Result<(), String>> {
            self.continue_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }

        fn abort(&self) {}
    }

    struct StaticParent {
        state: AgentState,
        stream_fn: StreamFn,
    }

    impl StaticParent {
        fn new(messages: Vec<AgentMessage>) -> Self {
            let mut state = AgentState {
                model: pi_ai::types::Model::new("test-model", "test-model", "test-api", "test-provider", ""),
                ..Default::default()
            };
            state.messages = messages;
            Self {
                state,
                stream_fn: Arc::new(|_model, _context, _options| {
                    Box::pin(async { pi_ai::utils::event_stream::create_assistant_message_event_stream() })
                }),
            }
        }
    }

    impl SideQuestionParent for StaticParent {
        fn state(&self) -> AgentState {
            self.state.clone()
        }

        fn convert_to_llm(&self) -> Option<Arc<dyn Fn(Vec<AgentMessage>) -> Vec<pi_ai::types::Message> + Send + Sync>> {
            None
        }

        fn transform_context(
            &self,
        ) -> Option<Arc<dyn Fn(Vec<AgentMessage>, Option<CancellationToken>) -> Vec<AgentMessage> + Send + Sync>>
        {
            None
        }

        fn stream_fn(&self) -> StreamFn {
            Arc::clone(&self.stream_fn)
        }

        fn get_api_key(&self) -> Option<Arc<dyn Fn(String) -> Option<String> + Send + Sync>> {
            None
        }

        fn on_payload(&self) -> Option<pi_ai::types::OnPayload> {
            None
        }

        fn on_response(&self) -> Option<pi_ai::types::OnResponse> {
            None
        }

        fn tool_execution(&self) -> Option<String> {
            None
        }

        fn session_id(&self) -> Option<String> {
            None
        }

        fn thinking_budgets(&self) -> Option<pi_ai::types::ThinkingBudgets> {
            None
        }
    }

    /// `side-question.ts:152-154`: a missing assistant message rejects
    /// `completeWithProviderRetry` immediately, so the recovery branch never runs
    /// and the user prompt is never popped. The default retry policy would sleep
    /// 2s/4s/8s and pop the prompt on each retry.
    #[tokio::test]
    async fn missing_assistant_message_fails_fast_without_popping_the_prompt() {
        let prompt: AgentMessage =
            UserMessage::new(pi_ai::types::UserContent::Text("question".to_string()), 0).into();
        let parent: Arc<dyn SideQuestionParent> = Arc::new(StaticParent::new(vec![prompt.clone()]));
        let fake = Arc::new(NoAssistantAgent {
            state: Mutex::new(AgentState {
                model: pi_ai::types::Model::new("test-model", "test-model", "test-api", "test-provider", ""),
                messages: vec![prompt.clone()],
                ..Default::default()
            }),
            prompt_calls: std::sync::atomic::AtomicUsize::new(0),
            continue_calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let factory: SideQuestionAgentFactory = {
            let fake = Arc::clone(&fake);
            Arc::new(move |_options| Arc::clone(&fake) as Arc<dyn SideQuestionAgent>)
        };
        let events: Arc<Mutex<Vec<(String, Option<String>)>>> = Arc::new(Mutex::new(Vec::new()));
        let on_event = {
            let events = Arc::clone(&events);
            Arc::new(move |event: SideQuestionEvent| {
                events
                    .lock()
                    .expect("events poisoned")
                    .push((event.status.clone(), event.error_message.clone()));
                Box::pin(async {}) as BoxFuture<()>
            })
        };
        let run = start_side_question(
            parent,
            factory,
            "id".to_string(),
            "question".to_string(),
            on_event,
            None,
            Some(ProviderRetryPolicy {
                enabled: true,
                max_retries: 3.0,
                base_delay_ms: 2_000.0,
                max_retry_delay_ms: 60_000.0,
            }),
        )
        .expect("side question starts");
        tokio::time::timeout(std::time::Duration::from_secs(1), run.done)
            .await
            .expect("a missing assistant message must not retry with 2s/4s/8s backoff");

        assert_eq!(
            fake.prompt_calls.load(Ordering::SeqCst),
            1,
            "the prompt must be sent exactly once"
        );
        assert_eq!(
            fake.continue_calls.load(Ordering::SeqCst),
            0,
            "the recovery branch must not run: it would pop the user prompt"
        );
        assert_eq!(
            fake.state().messages.len(),
            2,
            "the user prompt must still be in the side conversation"
        );
        assert_eq!(
            fake.state().messages.last().map(|message| message.role()),
            Some("user"),
            "the user prompt must be the last message"
        );
        assert_eq!(
            events.lock().expect("events poisoned").clone(),
            vec![
                (SIDE_QUESTION_STATUS_RUNNING.to_string(), None),
                (
                    SIDE_QUESTION_STATUS_ERROR.to_string(),
                    Some("Side question produced no assistant message".to_string())
                ),
            ]
        );
    }

    #[test]
    fn error_message_is_omitted_when_absent() {
        let event = SideQuestionEvent {
            id: "1".to_string(),
            question: "q".to_string(),
            answer: "a".to_string(),
            status: SIDE_QUESTION_STATUS_RUNNING.to_string(),
            error_message: None,
        };
        let value = serde_json::to_value(&event).expect("event serializes");
        assert!(value.get("errorMessage").is_none());
        assert_eq!(value.get("id").and_then(Value::as_str), Some("1"));
    }
}
