//! Port of packages/coding-agent/src/core/side-question.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pi_agent_core::types::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentState, AgentTool, StreamFn,
};
use pi_ai::types::{AssistantMessage, Model, ServiceTier, Usage, UserMessage, STOP_REASON_STOP};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::core::provider_retry::{
    complete_with_provider_retry, default_provider_retry_policy, ProviderRetryExecutionOptions,
    ProviderRetryPolicy,
};
use crate::core::semantic_edges::unwrap_semantic_edge_stream_fn;

pub type BoxFuture<T> = pi_ai::types::BoxFuture<T>;

/// `type SideQuestionStatus`.
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

/// `interface SideQuestionRun`.
#[derive(Clone)]
pub struct SideQuestionRun {
    pub done: BoxFuture<()>,
    pub abort: Arc<dyn Fn() + Send + Sync>,
}

const SIDE_QUESTION_INSTRUCTION: &str = "Answer this side question using only the conversation context above. Do not use tools. The user may send follow-up side questions; none of this side conversation is added to the main session.";

/// `sideQuestionPrompt(question, isFirstTurn)`.
pub fn side_question_prompt(question: &str, is_first_turn: bool) -> String {
    let body = if is_first_turn {
        format!("{}\n\n{}", SIDE_QUESTION_INSTRUCTION, question)
    } else {
        question.to_string()
    };
    format!("<side_question>\n{}\n</side_question>", body)
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

/// The `parent` argument of `startSideQuestion`.
///
/// `Agent` belongs to the pi-agent-core slice; the side question only reads this
/// surface from the parent, so the port keeps a minimal seam that mirrors the
/// exact TypeScript members it calls.
pub trait SideQuestionParent: Send + Sync {
    fn state(&self) -> AgentState;
    fn convert_to_llm(&self) -> Option<Arc<dyn Fn(Vec<AgentMessage>) -> Vec<pi_ai::types::Message> + Send + Sync>>;
    fn transform_context(
        &self,
    ) -> Option<
        Arc<
            dyn Fn(Vec<AgentMessage>, Option<CancellationToken>) -> Vec<AgentMessage> + Send + Sync,
        >,
    >;
    fn stream_fn(&self) -> StreamFn;
    fn session_id(&self) -> Option<String>;
    fn thinking_budgets(&self) -> Option<pi_ai::types::ThinkingBudgets>;
}

/// The child `Agent` the side question drives (pi-agent-core `Agent`).
pub trait SideQuestionAgent: Send + Sync {
    fn state(&self) -> AgentState;
    fn set_state(&self, state: AgentState);
    fn subscribe(
        &self,
        listener: Arc<dyn Fn(AgentEvent) -> BoxFuture<()> + Send + Sync>,
    ) -> Box<dyn Fn() + Send + Sync>;
    fn prompt(&self, messages: Vec<AgentMessage>) -> BoxFuture<Result<(), String>>;
    fn continue_(&self) -> BoxFuture<Result<(), String>>;
    fn abort(&self);
}

/// Options used to build the side agent (the `new Agent({...})` call).
#[derive(Clone)]
pub struct SideQuestionAgentOptions {
    pub initial_state: AgentState,
    pub convert_to_llm: Option<Arc<dyn Fn(Vec<AgentMessage>) -> Vec<pi_ai::types::Message> + Send + Sync>>,
    pub transform_context:
        Option<Arc<dyn Fn(Vec<AgentMessage>, Option<CancellationToken>) -> Vec<AgentMessage> + Send + Sync>>,
    pub stream_fn: StreamFn,
    pub on_payload: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    pub on_response: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    pub session_id: Option<String>,
    pub thinking_budgets: Option<pi_ai::types::ThinkingBudgets>,
}

/// `new Agent(options)`.
pub type SideQuestionAgentFactory =
    Arc<dyn Fn(SideQuestionAgentOptions) -> Arc<dyn SideQuestionAgent> + Send + Sync>;

/// `startSideQuestion(parent, id, question, onEvent, previousTurns, retry)`.
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
    let model = Some(parent_state.model.clone());
    let model = match model {
        Some(model) => model,
        None => return Err("Select a model before asking a side question".to_string()),
    };

    let previous_turns = previous_turns.unwrap_or_default();
    let previous_turn_messages: Vec<AgentMessage> = previous_turns
        .iter()
        .enumerate()
        .flat_map(|(index, turn)| {
            let now = now_millis();
            let user: AgentMessage = UserMessage::new(
                pi_ai::types::UserContent::Blocks(vec![pi_ai::types::ImageOrTextContent::Text(
                    pi_ai::types::TextContent::new(side_question_prompt(
                        &turn.question,
                        index == 0,
                    )),
                )]),
                now,
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
                timestamp: now,
                ..Default::default()
            }
            .into();
            [user, assistant]
        })
        .collect();

    let mut initial_messages = parent_state.messages.clone();
    initial_messages.extend(previous_turn_messages);
    let side_agent = agent_factory(SideQuestionAgentOptions {
        initial_state: AgentState {
            model: model.clone(),
            system_prompt: parent_state.system_prompt.clone(),
            messages: initial_messages,
            thinking_level: "off".to_string(),
            service_tier: parent_state.service_tier.clone(),
            tools: Vec::new(),
            ..parent_state.clone()
        },
        convert_to_llm: parent.convert_to_llm(),
        transform_context: parent.transform_context(),
        stream_fn: unwrap_semantic_edge_stream_fn(&parent.stream_fn()),
        on_payload: None,
        on_response: None,
        session_id: parent.session_id(),
        thinking_budgets: parent.thinking_budgets(),
    });

    let answer = Arc::new(Mutex::new(String::new()));
    let abort_requested = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicBool::new(false));
    let retry_abort_controller = CancellationToken::new();

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

    let unsubscribe = side_agent.subscribe({
        let answer = Arc::clone(&answer);
        let emit = Arc::clone(&emit);
        Arc::new(move |event: AgentEvent| {
            let message = match &event {
                AgentEvent::MessageUpdate { message, .. } => message.clone(),
                AgentEvent::MessageEnd { message } => message.clone(),
                _ => return Box::pin(async {}) as BoxFuture<()>,
            };
            let next_answer = read_assistant_text(&message);
            {
                let mut current = answer.lock().expect("side question answer poisoned");
                if *current == next_answer {
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
        Box::pin(async move {
            emit(SIDE_QUESTION_STATUS_RUNNING, None).await;
            if abort_requested.load(Ordering::SeqCst) {
                emit(SIDE_QUESTION_STATUS_CANCELLED, None).await;
                return;
            }
            started.store(true, Ordering::SeqCst);
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
                        if prompted_once.swap(true, Ordering::SeqCst) {
                            let mut state = side_agent.state();
                            state.messages.pop();
                            side_agent.set_state(state);
                            let _ = side_agent.continue_().await;
                        } else {
                            let _ = side_agent
                                .prompt(vec![UserMessage::new(
                                    pi_ai::types::UserContent::Blocks(vec![
                                        pi_ai::types::ImageOrTextContent::Text(
                                            pi_ai::types::TextContent::new(prompt),
                                        ),
                                    ]),
                                    now_millis(),
                                )
                                .into()])
                                .await;
                        }
                        let state = side_agent.state();
                        match state.messages.last() {
                            Some(AgentMessage::Message(pi_ai::types::Message::Assistant(assistant))) => {
                                assistant.clone()
                            }
                            _ => AssistantMessage {
                                stop_reason: pi_ai::types::STOP_REASON_ERROR.to_string(),
                                error_message: Some(
                                    state
                                        .error_message
                                        .clone()
                                        .unwrap_or_else(|| "Side question produced no assistant message".to_string()),
                                ),
                                ..Default::default()
                            },
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
            if let Some(error_message) = message.error_message.clone() {
                if message.stop_reason == pi_ai::types::STOP_REASON_ERROR
                    || message.stop_reason == pi_ai::types::STOP_REASON_ABORTED
                    || side_agent.state().error_message.is_some()
                {
                    emit(SIDE_QUESTION_STATUS_ERROR, Some(error_message)).await;
                    return;
                }
            }
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
    let _ = unsubscribe;

    Ok(SideQuestionRun { done, abort })
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

/// Re-exported for callers that build side agents from a tool list.
#[allow(dead_code)]
fn empty_tools() -> Vec<AgentTool> {
    Vec::new()
}

/// Unused `AgentContext` import keeper for the ported call sites.
#[allow(dead_code)]
fn agent_context_marker(context: AgentContext, config: AgentLoopConfig) -> (AgentContext, AgentLoopConfig) {
    (context, config)
}

/// Unused `ServiceTier`/`Model` marker.
#[allow(dead_code)]
fn model_marker(_model: &Model, _tier: &ServiceTier) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_turn_includes_the_instruction() {
        let prompt = side_question_prompt("why?", true);
        assert_eq!(
            prompt,
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
                pi_ai::types::ContentBlock::Text(pi_ai::types::TextContent::new("a")),
                pi_ai::types::ContentBlock::Thinking(pi_ai::types::ThinkingContent::default()),
                pi_ai::types::ContentBlock::Text(pi_ai::types::TextContent::new("b")),
            ],
            ..Default::default()
        }
        .into();
        assert_eq!(read_assistant_text(&message), "ab");
    }

    #[test]
    fn read_assistant_text_ignores_non_assistant_messages() {
        let message: AgentMessage = UserMessage::new(pi_ai::types::UserContent::Text("hi".to_string()), 0).into();
        assert_eq!(read_assistant_text(&message), "");
    }
}
