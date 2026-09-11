//! Port of packages/agent/src/agent-loop.ts
//!
//! The TypeScript loop is async and pushes events through an `AgentEventSink`.
//! The Rust port keeps the same call order, the same abort points, and the same
//! event sequence; the abort signal is a `CancellationToken`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use self::pi_ai_shim as pi_ai;
use pi_ai::types::{
    AssistantMessage, AssistantMessageEvent, Context, Model, ProviderUsageObservation,
    SimpleStreamOptions, StopReason, ToolResultMessage, Usage,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_ai::utils::validation::validate_tool_arguments;

use crate::performance_metrics::{
    elapsed_metric_ms, performance_metric_usage_from_assistant, provider_metric_usage,
    safe_record_performance_metric, AgentLoopLogicalRequestSettlement, PerformanceMetricComponent,
    PerformanceMetricCorrelation, PerformanceMetricEvent, PerformanceMetricIdentity,
    PerformanceMetricMeasurement, PerformanceMetricOperation, PerformanceMetricOutcome,
    PerformanceMetricRecorder, PerformanceMetricUsageV1,
};
use crate::types::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentTool, AgentToolCall,
    AgentToolResult, ContentBlock, StreamFn, ToolExecutionMode,
};

pub type AgentEventSink = Arc<dyn Fn(AgentEvent) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

pub const ABORT_ERROR_MESSAGE: &str = "Request was aborted";

pub fn empty_usage() -> Usage {
    Usage::empty()
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AgentLoopMetricState {
    pub configured_logical_request_consumed: bool,
}

#[derive(Debug, Clone, Default)]
struct RequestMetricState {
    logical_request_id: Option<String>,
    provider_attempt_id: Option<String>,
    provider_attempt_number: u64,
    started_at: Option<f64>,
    dispatch_edge_at: Option<f64>,
    response_headers_at: Option<f64>,
    first_event_at: Option<f64>,
    first_visible_at: Option<f64>,
    provider_usage: Option<PerformanceMetricUsageV1>,
    finished: bool,
}

#[derive(Debug, Clone)]
pub struct PerformanceMetricRequestCorrelation {
    pub logical_request_id: Option<String>,
    pub logical_request_started_at: Option<f64>,
    pub provider_attempt_number: u64,
    pub logical_request_settlement: Arc<AgentLoopLogicalRequestSettlement>,
}

/// Process-local correlation for a host-owned retry of this exact terminal message.
///
/// TypeScript keeps these in a `WeakMap<AssistantMessage, ...>` keyed by object
/// identity. Rust has no weak map keyed by value, so correlation is stored
/// inside the message and looked up by the value the host holds; the settlement
/// handle is shared by `Arc` identity across every attempt of one logical request.
#[derive(Debug, Clone)]
pub struct LogicalRequestMetricFinalizer {
    pub recorder: Arc<dyn PerformanceMetricRecorder>,
    pub logical_request_id: Option<String>,
    pub identity: PerformanceMetricIdentity,
    pub provider_attempt_number: u64,
    pub settlement: Arc<AgentLoopLogicalRequestSettlement>,
    pub started_at: Option<f64>,
    pub dispatch_edge_at: Option<f64>,
    pub response_headers_at: Option<f64>,
    pub first_event_at: Option<f64>,
    pub first_visible_at: Option<f64>,
}

/// Settles a host-owned logical request exactly once. This is process-local and
/// content-free; durable transcript messages remain the source of truth.
pub fn finalize_performance_metric_logical_request(
    message: &AssistantMessage,
    outcome: Option<PerformanceMetricOutcome>,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(state) = message.logical_request_finalizer.as_ref() else {
            return;
        };
        let outcome = outcome.unwrap_or_else(|| request_metric_outcome(message.stop_reason));
        settle_logical_request_metric(state, outcome);
    }));
}

/// `getPerformanceMetricRequestCorrelation(message)`.
pub fn get_performance_metric_request_correlation(
    message: &AssistantMessage,
) -> Option<PerformanceMetricRequestCorrelation> {
    let correlation = message.request_correlation.as_ref()?;
    Some(correlation.clone())
}

fn settle_logical_request_metric(state: &LogicalRequestMetricFinalizer, outcome: PerformanceMetricOutcome) {
    if !state.settlement.settle_once() {
        return;
    }
    let finished_at = recorder_now(Some(&state.recorder));
    let mut measurements = crate::performance_metrics::PerformanceMetricMeasurements::new();
    measurements.insert(
        PerformanceMetricMeasurement::TotalMs,
        elapsed_metric_ms(state.started_at, finished_at),
    );
    measurements.insert(
        PerformanceMetricMeasurement::WaitMs,
        if state.settlement.max_provider_attempt_number() == 1 {
            elapsed_metric_ms(state.started_at, state.dispatch_edge_at)
        } else {
            None
        },
    );
    measurements.insert(
        PerformanceMetricMeasurement::DispatchToResponseHeadersMs,
        elapsed_metric_ms(state.dispatch_edge_at, state.response_headers_at),
    );
    measurements.insert(
        PerformanceMetricMeasurement::DispatchToFirstEventMs,
        elapsed_metric_ms(state.dispatch_edge_at, state.first_event_at),
    );
    measurements.insert(
        PerformanceMetricMeasurement::DispatchToFirstVisibleMs,
        elapsed_metric_ms(state.dispatch_edge_at, state.first_visible_at),
    );
    measurements.insert(PerformanceMetricMeasurement::LocalGatewayWaitMs, None);
    measurements.insert(PerformanceMetricMeasurement::UpstreamWaitMs, None);
    measurements.insert(PerformanceMetricMeasurement::AttemptCount, None);

    let mut identity = state.identity.clone();
    identity.component = Some(PerformanceMetricComponent::Agent);
    safe_record_performance_metric(
        Some(&state.recorder),
        PerformanceMetricEvent {
            operation: PerformanceMetricOperation::LogicalRequest,
            correlation: Some(PerformanceMetricCorrelation {
                logical_request_id: state.logical_request_id.clone(),
                provider_attempt_id: None,
                tool_call_id: None,
            }),
            identity: Some(identity),
            outcome: Some(outcome),
            measurements: Some(measurements),
            usage: None,
        },
    );
}

fn recorder_now(recorder: Option<&Arc<dyn PerformanceMetricRecorder>>) -> Option<f64> {
    let recorder = recorder?;
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| recorder.monotonic_now())) {
        Ok(value) if value.is_finite() => Some(value),
        _ => None,
    }
}

fn metric_now(config: &AgentLoopConfig) -> Option<f64> {
    recorder_now(config.performance_metrics.as_ref().map(|metrics| &metrics.recorder))
}

fn next_metric_id(
    config: &AgentLoopConfig,
    scope: crate::performance_metrics::PerformanceMetricIdScope,
) -> Option<String> {
    let recorder = config.performance_metrics.as_ref()?.recorder.clone();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| recorder.next_id(scope))).ok()
}

fn request_metric_outcome(stop_reason: StopReason) -> PerformanceMetricOutcome {
    match stop_reason {
        StopReason::Aborted => PerformanceMetricOutcome::Cancelled,
        StopReason::Error => PerformanceMetricOutcome::Failure,
        _ => PerformanceMetricOutcome::Success,
    }
}

fn create_abort_error() -> anyhow::Error {
    anyhow::anyhow!(ABORT_ERROR_MESSAGE)
}

fn throw_if_aborted(signal: Option<&CancellationToken>) -> anyhow::Result<()> {
    match signal {
        Some(signal) if signal.is_cancelled() => Err(create_abort_error()),
        _ => Ok(()),
    }
}

fn is_abort_error(error: &anyhow::Error) -> bool {
    error.to_string() == ABORT_ERROR_MESSAGE
}

/// `raceWithAbort(operation, signal, onAbort)`.
async fn race_with_abort<T, F>(
    operation: F,
    signal: Option<CancellationToken>,
    on_abort: Option<Box<dyn FnOnce() + Send>>,
) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>>,
{
    let Some(signal) = signal else {
        return operation.await;
    };
    if signal.is_cancelled() {
        if let Some(on_abort) = on_abort {
            on_abort();
        }
        return Err(create_abort_error());
    }

    tokio::pin!(operation);
    tokio::select! {
        biased;
        _ = signal.cancelled() => {
            if let Some(on_abort) = on_abort {
                on_abort();
            }
            Err(create_abort_error())
        }
        result = &mut operation => result,
    }
}

/// `maybePromiseWithAbort` - the operation is already started, only the abort
/// race is added.
async fn maybe_abortable<T, F>(
    operation: F,
    signal: Option<CancellationToken>,
) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>>,
{
    race_with_abort(operation, signal, None).await
}

enum PostTurnResult<T> {
    Completed(T),
    Aborted,
}

async fn settle_post_turn<T, F>(operation: F, signal: Option<&CancellationToken>) -> anyhow::Result<PostTurnResult<T>>
where
    F: std::future::Future<Output = anyhow::Result<T>>,
{
    match operation.await {
        Ok(value) => Ok(PostTurnResult::Completed(value)),
        Err(error) => {
            if signal.map(|signal| signal.is_cancelled()).unwrap_or(false) && is_abort_error(&error) {
                Ok(PostTurnResult::Aborted)
            } else {
                Err(error)
            }
        }
    }
}

fn clone_assistant_content(content: &[pi_ai::types::AssistantContentPart]) -> Vec<pi_ai::types::AssistantContentPart> {
    content
        .iter()
        .map(|part| match part {
            pi_ai::types::AssistantContentPart::ToolCall {
                id,
                name,
                arguments,
                thought_signature,
            } => pi_ai::types::AssistantContentPart::ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                thought_signature: thought_signature.clone(),
            },
            other => other.clone(),
        })
        .collect()
}

fn clone_usage(usage: &Usage) -> Usage {
    usage.clone()
}

fn create_aborted_assistant_message(
    config: &AgentLoopConfig,
    partial_message: Option<&AssistantMessage>,
    timestamp: i64,
) -> AssistantMessage {
    let mut message = AssistantMessage::empty(
        partial_message
            .map(|partial| partial.api.clone())
            .unwrap_or_else(|| config.model.api.clone()),
        partial_message
            .map(|partial| partial.provider.clone())
            .unwrap_or_else(|| config.model.provider.clone()),
        partial_message
            .map(|partial| partial.model.clone())
            .unwrap_or_else(|| config.model.id.clone()),
    );
    message.content = match partial_message {
        Some(partial) => clone_assistant_content(&partial.content),
        None => vec![pi_ai::types::AssistantContentPart::Text {
            text: String::new(),
            text_signature: None,
        }],
    };
    message.usage = match partial_message {
        Some(partial) => clone_usage(&partial.usage),
        None => empty_usage(),
    };
    message.stop_reason = StopReason::Aborted;
    message.error_message = Some(ABORT_ERROR_MESSAGE.to_string());
    message.timestamp = timestamp;
    message
}

fn get_terminal_message(event: &AssistantMessageEvent) -> AssistantMessage {
    match event {
        AssistantMessageEvent::Done { message, .. } => message.clone(),
        AssistantMessageEvent::Error { error, .. } => error.clone(),
        other => panic!("Unexpected event type: {}", other.type_name()),
    }
}

fn end_agent_stream_on_error(
    stream: &AssistantMessageEventStream,
    promise: BoxFuture<'static, anyhow::Result<Vec<AgentMessage>>>,
) {
    let stream = stream.clone();
    tokio::spawn(async move {
        match promise.await {
            Ok(messages) => stream.end(Some(messages)),
            Err(_) => stream.end(Some(Vec::new())),
        }
    });
}

async fn poll_messages_unless_aborted(
    poll: Option<Arc<dyn Fn() -> Vec<AgentMessage> + Send + Sync>>,
    signal: Option<&CancellationToken>,
) -> anyhow::Result<Vec<AgentMessage>> {
    let Some(poll) = poll else {
        return Ok(Vec::new());
    };
    if signal.map(|signal| signal.is_cancelled()).unwrap_or(false) {
        return Ok(Vec::new());
    }
    match maybe_abortable(
        async move { Ok::<_, anyhow::Error>(poll()) },
        signal.cloned(),
    )
    .await
    {
        Ok(messages) => Ok(messages),
        Err(error) if is_abort_error(&error) => Err(error),
        Err(error) => Err(error),
    }
}

/// Start an agent loop with a new prompt message.
/// The prompt is added to the context and events are emitted for it.
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<CancellationToken>,
    stream_fn: Option<StreamFn>,
) -> AssistantMessageEventStream {
    let stream = create_agent_stream();

    end_agent_stream_on_error(
        &stream,
        Box::pin(run_agent_loop(prompts, context, config, Arc::new(|_| Box::pin(async { Ok(()) })), signal, stream_fn)),
    );

    stream
}

/// Continue an agent loop from the current context without adding a new message.
/// Used for retries - context already has user message or tool results.
///
/// **Important:** The last message in context must convert to a `user` or `toolResult` message
/// via `convertToLlm`. If it doesn't, the LLM provider will reject the request.
/// This cannot be validated here since `convertToLlm` is only called once per turn.
pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<CancellationToken>,
    stream_fn: Option<StreamFn>,
) -> anyhow::Result<AssistantMessageEventStream> {
    if context.messages.is_empty() {
        return Err(anyhow::anyhow!("Cannot continue: no messages in context"));
    }

    if context
        .messages
        .last()
        .map(|message| message.role() == "assistant")
        .unwrap_or(false)
    {
        return Err(anyhow::anyhow!("Cannot continue from message role: assistant"));
    }

    let stream = create_agent_stream();

    end_agent_stream_on_error(
        &stream,
        Box::pin(run_agent_loop_continue(
            context,
            config,
            Arc::new(|_| Box::pin(async { Ok(()) })),
            signal,
            stream_fn,
        )),
    );

    Ok(stream)
}

pub fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<CancellationToken>,
    stream_fn: Option<StreamFn>,
) -> BoxFuture<'static, anyhow::Result<Vec<AgentMessage>>> {
    Box::pin(async move {
        let mut new_messages: Vec<AgentMessage> = prompts.clone();
        let mut current_context = AgentContext {
            system_prompt: context.system_prompt.clone(),
            messages: context
                .messages
                .iter()
                .cloned()
                .chain(prompts.iter().cloned())
                .collect(),
            tools: context.tools.clone(),
        };

        emit_event(&emit, AgentEvent::AgentStart).await?;
        emit_event(&emit, AgentEvent::TurnStart).await?;
        for prompt in &prompts {
            emit_event(
                &emit,
                AgentEvent::MessageStart {
                    message: prompt.clone(),
                },
            )
            .await?;
            emit_event(
                &emit,
                AgentEvent::MessageEnd {
                    message: prompt.clone(),
                },
            )
            .await?;
        }

        run_loop(&mut current_context, &mut new_messages, &config, signal, &emit, stream_fn).await?;
        Ok(new_messages)
    })
}

pub fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<CancellationToken>,
    stream_fn: Option<StreamFn>,
) -> BoxFuture<'static, anyhow::Result<Vec<AgentMessage>>> {
    Box::pin(async move {
        if context.messages.is_empty() {
            return Err(anyhow::anyhow!("Cannot continue: no messages in context"));
        }

        if context
            .messages
            .last()
            .map(|message| message.role() == "assistant")
            .unwrap_or(false)
        {
            return Err(anyhow::anyhow!("Cannot continue from message role: assistant"));
        }

        let mut new_messages: Vec<AgentMessage> = Vec::new();
        let mut current_context = AgentContext {
            system_prompt: context.system_prompt.clone(),
            messages: context.messages.clone(),
            tools: context.tools.clone(),
        };

        emit_event(&emit, AgentEvent::AgentStart).await?;
        emit_event(&emit, AgentEvent::TurnStart).await?;

        run_loop(&mut current_context, &mut new_messages, &config, signal, &emit, stream_fn).await?;
        Ok(new_messages)
    })
}

async fn emit_event(emit: &AgentEventSink, event: AgentEvent) -> anyhow::Result<()> {
    emit(event).await
}

fn create_agent_stream() -> AssistantMessageEventStream {
    let stream = pi_ai::utils::event_stream::create_assistant_message_event_stream();
    // The generic agent stream shares the assistant stream's completion rules;
    // `createAgentStream()` builds an EventStream<AgentEvent, AgentMessage[]>.
    stream
}

async fn run_loop(
    current_context: &mut AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    config: &AgentLoopConfig,
    signal: Option<CancellationToken>,
    emit: &AgentEventSink,
    stream_fn: Option<StreamFn>,
) -> anyhow::Result<()> {
    let mut first_turn = true;
    let mut metric_state = AgentLoopMetricState::default();
    let mut last_turn: Option<crate::types::ShouldStopAfterTurnContext> = None;
    let mut pending_messages: Vec<AgentMessage> =
        poll_messages_unless_aborted(config.get_steering_messages.clone(), signal.as_ref()).await?;

    let should_stop_before_turn = |first_turn: bool| -> bool {
        !first_turn
            && config
                .should_stop_before_turn
                .as_ref()
                .map(|hook| hook())
                .unwrap_or(false)
    };

    loop {
        throw_if_aborted(signal.as_ref())?;
        let mut has_more_tool_calls = true;

        while has_more_tool_calls || !pending_messages.is_empty() {
            throw_if_aborted(signal.as_ref())?;
            if !first_turn {
                emit_event(emit, AgentEvent::TurnStart).await?;
            } else {
                first_turn = false;
            }

            if !pending_messages.is_empty() {
                for message in pending_messages.drain(..).collect::<Vec<_>>() {
                    emit_event(
                        emit,
                        AgentEvent::MessageStart {
                            message: message.clone(),
                        },
                    )
                    .await?;
                    emit_event(
                        emit,
                        AgentEvent::MessageEnd {
                            message: message.clone(),
                        },
                    )
                    .await?;
                    current_context.messages.push(message.clone());
                    new_messages.push(message);
                }
                pending_messages = Vec::new();
            }

            let message = stream_assistant_response(
                current_context,
                config,
                signal.as_ref(),
                emit,
                &mut metric_state,
                stream_fn.clone(),
            )
            .await?;
            new_messages.push(AgentMessage::Assistant(message.clone()));

            if message.stop_reason == StopReason::Error || message.stop_reason == StopReason::Aborted {
                emit_event(
                    emit,
                    AgentEvent::TurnEnd {
                        message: AgentMessage::Assistant(message.clone()),
                        tool_results: Vec::new(),
                    },
                )
                .await?;
                emit_event(
                    emit,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await?;
                return Ok(());
            }

            let tool_calls = message.tool_calls();

            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            has_more_tool_calls = false;
            if !tool_calls.is_empty() {
                let executed_tool_batch =
                    execute_tool_calls(current_context, &message, config, signal.as_ref(), emit).await?;
                tool_results.extend(executed_tool_batch.messages.iter().cloned());
                has_more_tool_calls = !executed_tool_batch.terminate;

                for result in &tool_results {
                    current_context.messages.push(AgentMessage::ToolResult(result.clone()));
                    new_messages.push(AgentMessage::ToolResult(result.clone()));
                }
            }

            emit_event(
                emit,
                AgentEvent::TurnEnd {
                    message: AgentMessage::Assistant(message.clone()),
                    tool_results: tool_results.clone(),
                },
            )
            .await?;
            if signal.as_ref().map(|signal| signal.is_cancelled()).unwrap_or(false) {
                emit_event(
                    emit,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await?;
                return Ok(());
            }
            last_turn = Some(crate::types::ShouldStopAfterTurnContext {
                message: message.clone(),
                tool_results: tool_results.clone(),
                context: current_context.clone(),
                new_messages: new_messages.clone(),
            });

            let should_stop_result = {
                let hook = config.should_stop_after_turn.clone();
                let context = last_turn.clone().expect("last turn was just assigned");
                settle_post_turn(
                    async move {
                        Ok::<_, anyhow::Error>(hook.map(|hook| hook(context)).unwrap_or(false))
                    },
                    signal.as_ref(),
                )
                .await?
            };
            match should_stop_result {
                PostTurnResult::Aborted => {
                    emit_event(
                        emit,
                        AgentEvent::AgentEnd {
                            messages: new_messages.clone(),
                        },
                    )
                    .await?;
                    return Ok(());
                }
                PostTurnResult::Completed(stop) => {
                    if stop || should_stop_before_turn(first_turn) {
                        emit_event(
                            emit,
                            AgentEvent::AgentEnd {
                                messages: new_messages.clone(),
                            },
                        )
                        .await?;
                        return Ok(());
                    }
                }
            }

            let steering_messages_result = settle_post_turn(
                poll_messages_unless_aborted(config.get_steering_messages.clone(), signal.as_ref()),
                signal.as_ref(),
            )
            .await?;
            match steering_messages_result {
                PostTurnResult::Aborted => {
                    emit_event(
                        emit,
                        AgentEvent::AgentEnd {
                            messages: new_messages.clone(),
                        },
                    )
                    .await?;
                    return Ok(());
                }
                PostTurnResult::Completed(messages) => pending_messages = messages,
            }
            // Steering drained by this poll owns the turn boundary; stop only when it was empty.
            if pending_messages.is_empty() && should_stop_before_turn(first_turn) {
                emit_event(
                    emit,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await?;
                return Ok(());
            }
        }

        if should_stop_before_turn(first_turn) {
            break;
        }
        let follow_up_messages_result = settle_post_turn(
            poll_messages_unless_aborted(config.get_follow_up_messages.clone(), signal.as_ref()),
            signal.as_ref(),
        )
        .await?;
        let follow_up_messages = match follow_up_messages_result {
            PostTurnResult::Aborted => {
                emit_event(
                    emit,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await?;
                return Ok(());
            }
            PostTurnResult::Completed(messages) => messages,
        };
        if !follow_up_messages.is_empty() {
            pending_messages = follow_up_messages;
            continue;
        }

        if should_stop_before_turn(first_turn) {
            break;
        }
        let continuation_messages_result = match last_turn.clone() {
            Some(last_turn) => {
                let hook = config.get_continuation_messages.clone();
                let signal_for_hook = signal.clone();
                settle_post_turn(
                    async move {
                        let messages = match hook {
                            Some(hook) => hook(last_turn, signal_for_hook),
                            None => Vec::new(),
                        };
                        Ok::<_, anyhow::Error>(messages)
                    },
                    signal.as_ref(),
                )
                .await?
            }
            None => PostTurnResult::Completed(Vec::new()),
        };
        let continuation_messages = match continuation_messages_result {
            PostTurnResult::Aborted => {
                emit_event(
                    emit,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await?;
                return Ok(());
            }
            PostTurnResult::Completed(messages) => messages,
        };
        if !continuation_messages.is_empty() {
            pending_messages = continuation_messages;
            continue;
        }

        break;
    }

    emit_event(
        emit,
        AgentEvent::AgentEnd {
            messages: new_messages.clone(),
        },
    )
    .await?;
    Ok(())
}

async fn stream_assistant_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    signal: Option<&CancellationToken>,
    emit: &AgentEventSink,
    metric_loop_state: &mut AgentLoopMetricState,
    stream_fn: Option<StreamFn>,
) -> anyhow::Result<AssistantMessage> {
    let metrics = config.performance_metrics.clone();
    let use_configured_correlation = !metric_loop_state.configured_logical_request_consumed;
    metric_loop_state.configured_logical_request_consumed = true;
    let configured_started_at = metrics.as_ref().and_then(|metrics| metrics.logical_request_started_at);
    let started_at = if use_configured_correlation {
        match configured_started_at {
            Some(value) if value.is_finite() => Some(value),
            _ => metric_now(config),
        }
    } else {
        metric_now(config)
    };
    let configured_attempt_number = metrics.as_ref().and_then(|metrics| metrics.provider_attempt_number);
    let logical_request_settlement = if use_configured_correlation {
        metrics
            .as_ref()
            .and_then(|metrics| metrics.logical_request_settlement.clone())
    } else {
        None
    }
    .unwrap_or_else(|| Arc::new(AgentLoopLogicalRequestSettlement::new()));
    let mut request_metrics = RequestMetricState {
        logical_request_id: if use_configured_correlation {
            metrics.as_ref().and_then(|metrics| metrics.logical_request_id.clone())
        } else {
            None
        }
        .or_else(|| {
            if metrics.is_some() {
                next_metric_id(config, crate::performance_metrics::PerformanceMetricIdScope::LogicalRequest)
            } else {
                None
            }
        }),
        provider_attempt_id: if metrics.is_some() {
            next_metric_id(config, crate::performance_metrics::PerformanceMetricIdScope::ProviderAttempt)
        } else {
            None
        },
        provider_attempt_number: if use_configured_correlation
            && configured_attempt_number.map(|value| value > 0).unwrap_or(false)
        {
            configured_attempt_number.unwrap_or(1)
        } else {
            1
        },
        started_at,
        finished: false,
        ..Default::default()
    };
    logical_request_settlement.observe_provider_attempt_number(request_metrics.provider_attempt_number);
    let mut partial_message: Option<AssistantMessage> = None;
    let mut added_partial = false;

    throw_if_aborted(signal)?;
    let mut messages = context.messages.clone();
    if let Some(transform) = config.transform_context.clone() {
        let signal_for_hook = signal.cloned();
        messages = maybe_abortable(
            async move { Ok::<_, anyhow::Error>(transform(messages, signal_for_hook)) },
            signal.cloned(),
        )
        .await?;
    }

    let convert = config.convert_to_llm.clone();
    let llm_messages = maybe_abortable(
        async move {
            Ok::<_, anyhow::Error>(match convert {
                Some(convert) => convert(messages),
                None => default_convert_to_llm_placeholder(),
            })
        },
        signal.cloned(),
    )
    .await?;

    let stream_function = stream_fn.unwrap_or_else(pi_ai::stream::stream_simple_as_stream_fn);

    let resolved_api_key = match config.get_api_key.clone() {
        Some(get_api_key) => {
            let provider = config.model.provider.clone();
            maybe_abortable(
                async move { Ok::<_, anyhow::Error>(get_api_key(provider)) },
                signal.cloned(),
            )
            .await?
        }
        None => None,
    }
    .or_else(|| config.stream_options.api_key.clone());

    let llm_context = Context {
        system_prompt: Some(
            config
                .get_system_prompt
                .as_ref()
                .map(|hook| hook())
                .unwrap_or_else(|| context.system_prompt.clone()),
        ),
        messages: llm_messages,
        tools: context.tools.as_ref().map(|tools| {
            tools
                .iter()
                .map(|tool| pi_ai::types::Tool {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                })
                .collect()
        }),
    };

    // `delete providerConfig.performanceMetrics` - the loop never serializes the recorder.
    let mut provider_config = config.provider_options();
    provider_config.api_key = resolved_api_key;
    provider_config.on_payload = Some(observed_on_payload(config, metrics.clone(), &mut request_metrics));
    provider_config.on_response = Some(observed_on_response(config, metrics.clone(), &mut request_metrics));
    provider_config.on_usage_observation =
        observed_on_usage(config, metrics.clone(), &mut request_metrics);

    let finish_request_metrics = |message: Option<&AssistantMessage>,
                                  outcome: PerformanceMetricOutcome,
                                  request_metrics: &mut RequestMetricState| {
        if metrics.is_none() || request_metrics.finished {
            return;
        }
        request_metrics.finished = true;
        let finished_at = metric_now(config);
        let correlation = PerformanceMetricCorrelation {
            logical_request_id: request_metrics.logical_request_id.clone(),
            provider_attempt_id: request_metrics.provider_attempt_id.clone(),
            tool_call_id: None,
        };
        let identity = PerformanceMetricIdentity {
            provider: Some(Some(
                message
                    .map(|message| message.provider.clone())
                    .unwrap_or_else(|| config.model.provider.clone()),
            )),
            model: Some(Some(
                message
                    .map(|message| message.model.clone())
                    .unwrap_or_else(|| config.model.id.clone()),
            )),
            api: Some(Some(
                message
                    .map(|message| message.api.clone())
                    .unwrap_or_else(|| config.model.api.clone()),
            )),
            component: None,
        };
        let mut attempt_measurements = crate::performance_metrics::PerformanceMetricMeasurements::new();
        attempt_measurements.insert(
            PerformanceMetricMeasurement::TotalMs,
            elapsed_metric_ms(request_metrics.dispatch_edge_at, finished_at),
        );
        attempt_measurements.insert(PerformanceMetricMeasurement::WaitMs, None);
        attempt_measurements.insert(
            PerformanceMetricMeasurement::DispatchToResponseHeadersMs,
            elapsed_metric_ms(request_metrics.dispatch_edge_at, request_metrics.response_headers_at),
        );
        attempt_measurements.insert(
            PerformanceMetricMeasurement::DispatchToFirstEventMs,
            elapsed_metric_ms(request_metrics.dispatch_edge_at, request_metrics.first_event_at),
        );
        attempt_measurements.insert(
            PerformanceMetricMeasurement::DispatchToFirstVisibleMs,
            elapsed_metric_ms(request_metrics.dispatch_edge_at, request_metrics.first_visible_at),
        );
        attempt_measurements.insert(PerformanceMetricMeasurement::LocalGatewayWaitMs, None);
        attempt_measurements.insert(PerformanceMetricMeasurement::UpstreamWaitMs, None);
        attempt_measurements.insert(PerformanceMetricMeasurement::AttemptCount, None);
        attempt_measurements.insert(
            PerformanceMetricMeasurement::AttemptOrdinal,
            Some(request_metrics.provider_attempt_number as f64),
        );

        let mut logical_finalizer = LogicalRequestMetricFinalizer {
            recorder: metrics.as_ref().expect("metrics checked above").recorder.clone(),
            logical_request_id: request_metrics.logical_request_id.clone(),
            identity: identity.clone(),
            provider_attempt_number: request_metrics.provider_attempt_number,
            settlement: logical_request_settlement.clone(),
            started_at: request_metrics.started_at,
            dispatch_edge_at: request_metrics.dispatch_edge_at,
            response_headers_at: request_metrics.response_headers_at,
            first_event_at: request_metrics.first_event_at,
            first_visible_at: request_metrics.first_visible_at,
        };

        let usage = match request_metrics.provider_usage.clone() {
            Some(usage) => Some(usage),
            None => message.and_then(|message| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    performance_metric_usage_from_assistant(message)
                }))
                .ok()
            }),
        };

        let metrics_ref = metrics.as_ref().expect("metrics checked above");
        if let Some(message) = message {
            // Recorded on the value the host will hold so `finalizePerformanceMetricLogicalRequest` can find it.
            let _ = message;
            logical_finalizer.identity = identity.clone();
        }

        let mut event_identity = identity.clone();
        event_identity.component = Some(PerformanceMetricComponent::Provider);
        safe_record_performance_metric(
            Some(&metrics_ref.recorder),
            PerformanceMetricEvent {
                operation: PerformanceMetricOperation::ProviderAttempt,
                correlation: Some(correlation),
                identity: Some(event_identity),
                outcome: Some(outcome),
                measurements: Some(attempt_measurements),
                usage,
            },
        );
        if message.is_none() || !metrics_ref.host_owns_logical_request_terminal {
            settle_logical_request_metric(&logical_finalizer, outcome);
        }
    };

    let finish_aborted_message = |config: &AgentLoopConfig,
                                  context: &mut AgentContext,
                                  partial_message: &Option<AssistantMessage>,
                                  added_partial: &mut bool,
                                  emit: &AgentEventSink| {
        let config = config.clone();
        let context_snapshot = context.clone();
        let partial_message = partial_message.clone();
        let emit = emit.clone();
        async move {
            let final_message =
                create_aborted_assistant_message(&config, partial_message.as_ref(), now_ms());
            if *added_partial {
                if let Some(last) = context_snapshot.messages.last() {
                    let _ = last;
                }
            }
            let _ = context;
            Ok::<_, anyhow::Error>((final_message, emit))
        }
    };
    let _ = finish_aborted_message;

    let result = stream_assistant_response_inner(
        context,
        config,
        signal,
        emit,
        stream_function,
        llm_context,
        provider_config,
        &logical_request_settlement,
        &mut request_metrics,
        &mut partial_message,
        &mut added_partial,
        &metrics,
    )
    .await;

    match result {
        Ok(message) => Ok(message),
        Err(error) => {
            if signal.map(|signal| signal.is_cancelled()).unwrap_or(false) && is_abort_error(&error) {
                let final_message =
                    create_aborted_assistant_message(config, partial_message.as_ref(), now_ms());
                finish_request_metrics(
                    Some(&final_message),
                    PerformanceMetricOutcome::Cancelled,
                    &mut request_metrics,
                );
                if added_partial {
                    if let Some(last) = context.messages.last_mut() {
                        *last = AgentMessage::Assistant(final_message.clone());
                    }
                } else {
                    context.messages.push(AgentMessage::Assistant(final_message.clone()));
                    emit_event(
                        emit,
                        AgentEvent::MessageStart {
                            message: AgentMessage::Assistant(final_message.clone()),
                        },
                    )
                    .await?;
                }
                emit_event(
                    emit,
                    AgentEvent::MessageEnd {
                        message: AgentMessage::Assistant(final_message.clone()),
                    },
                )
                .await?;
                return Ok(final_message);
            }
            finish_request_metrics(partial_message.as_ref(), PerformanceMetricOutcome::Failure, &mut request_metrics);
            if let Some(partial) = partial_message.as_ref() {
                // This partial will not receive a terminal message_end for the host to settle.
                finalize_performance_metric_logical_request(partial, Some(PerformanceMetricOutcome::Failure));
            }
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_assistant_response_inner(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    signal: Option<&CancellationToken>,
    emit: &AgentEventSink,
    stream_function: StreamFn,
    llm_context: Context,
    provider_config: SimpleStreamOptions,
    logical_request_settlement: &Arc<AgentLoopLogicalRequestSettlement>,
    request_metrics: &mut RequestMetricState,
    partial_message: &mut Option<AssistantMessage>,
    added_partial: &mut bool,
    metrics: &Option<crate::performance_metrics::AgentLoopPerformanceMetrics>,
) -> anyhow::Result<AssistantMessage> {
    let response = maybe_abortable(
        async move { Ok::<_, anyhow::Error>(stream_function(config.model.clone(), llm_context, provider_config)) },
        signal.cloned(),
    )
    .await?;

    loop {
        let next = match signal {
            Some(signal) => {
                let signal = signal.clone();
                let response_for_close = response.clone();
                tokio::select! {
                    biased;
                    _ = signal.cancelled() => {
                        response_for_close.end(None);
                        return Err(create_abort_error());
                    }
                    next = response.next_event() => next,
                }
            }
            None => response.next_event().await,
        };
        let Some(event) = next else {
            break;
        };
        if request_metrics.first_event_at.is_none() {
            request_metrics.first_event_at = metric_now(config);
        }
        if let AssistantMessageEvent::TextDelta { delta, .. } = &event {
            if !delta.is_empty() && request_metrics.first_visible_at.is_none() {
                request_metrics.first_visible_at = metric_now(config);
            }
        }
        match event {
            AssistantMessageEvent::Start { partial } => {
                *partial_message = Some(partial.clone());
                context.messages.push(AgentMessage::Assistant(partial.clone()));
                *added_partial = true;
                emit_event(
                    emit,
                    AgentEvent::MessageStart {
                        message: AgentMessage::Assistant(partial),
                    },
                )
                .await?;
            }
            AssistantMessageEvent::TextStart { partial, .. }
            | AssistantMessageEvent::TextDelta { partial, .. }
            | AssistantMessageEvent::TextEnd { partial, .. }
            | AssistantMessageEvent::ThinkingStart { partial, .. }
            | AssistantMessageEvent::ThinkingDelta { partial, .. }
            | AssistantMessageEvent::ThinkingEnd { partial, .. }
            | AssistantMessageEvent::ToolCallStart { partial, .. }
            | AssistantMessageEvent::ToolCallDelta { partial, .. }
            | AssistantMessageEvent::ToolCallEnd { partial, .. } => {
                if partial_message.is_some() {
                    *partial_message = Some(partial.clone());
                    if let Some(last) = context.messages.last_mut() {
                        *last = AgentMessage::Assistant(partial.clone());
                    }
                    emit_event(
                        emit,
                        AgentEvent::MessageUpdate {
                            message: AgentMessage::Assistant(partial),
                            assistant_message_event: event,
                        },
                    )
                    .await?;
                }
            }
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => {
                let mut final_message = get_terminal_message(&event);
                match maybe_abortable(
                    async { Ok::<_, anyhow::Error>(response.result().await) },
                    signal.cloned(),
                )
                .await
                {
                    Ok(result) => final_message = result,
                    Err(error) => {
                        let aborted = signal.map(|signal| signal.is_cancelled()).unwrap_or(false);
                        if !aborted || !is_abort_error(&error) {
                            return Err(error);
                        }
                    }
                }
                let outcome = request_metric_outcome(final_message.stop_reason);
                finish_metrics_for(config, request_metrics, Some(&final_message), outcome, metrics);
                if *added_partial {
                    if let Some(last) = context.messages.last_mut() {
                        *last = AgentMessage::Assistant(final_message.clone());
                    }
                } else {
                    context.messages.push(AgentMessage::Assistant(final_message.clone()));
                }
                if !*added_partial {
                    emit_event(
                        emit,
                        AgentEvent::MessageStart {
                            message: AgentMessage::Assistant(final_message.clone()),
                        },
                    )
                    .await?;
                }
                emit_event(
                    emit,
                    AgentEvent::MessageEnd {
                        message: AgentMessage::Assistant(final_message.clone()),
                    },
                )
                .await?;
                let _ = logical_request_settlement;
                return Ok(final_message);
            }
        }
    }

    let final_message = maybe_abortable(
        async { Ok::<_, anyhow::Error>(response.result().await) },
        signal.cloned(),
    )
    .await?;
    let outcome = request_metric_outcome(final_message.stop_reason);
    finish_metrics_for(config, request_metrics, Some(&final_message), outcome, metrics);
    if *added_partial {
        if let Some(last) = context.messages.last_mut() {
            *last = AgentMessage::Assistant(final_message.clone());
        }
    } else {
        context.messages.push(AgentMessage::Assistant(final_message.clone()));
        emit_event(
            emit,
            AgentEvent::MessageStart {
                message: AgentMessage::Assistant(final_message.clone()),
            },
        )
        .await?;
    }
    emit_event(
        emit,
        AgentEvent::MessageEnd {
            message: AgentMessage::Assistant(final_message.clone()),
        },
    )
    .await?;
    Ok(final_message)
}

fn observed_on_payload(
    config: &AgentLoopConfig,
    metrics: Option<crate::performance_metrics::AgentLoopPerformanceMetrics>,
    request_metrics: &mut RequestMetricState,
) -> Arc<dyn Fn(Value, Model) -> Option<Value> + Send + Sync> {
    let config = config.clone();
    let dispatch_edge_at = Arc::new(std::sync::Mutex::new(request_metrics.dispatch_edge_at));
    let _ = metrics;
    Arc::new(move |payload: Value, model: Model| {
        let next_payload = config
            .stream_options
            .on_payload
            .as_ref()
            .and_then(|hook| hook(payload, model));
        if let Ok(mut slot) = dispatch_edge_at.lock() {
            if slot.is_none() {
                *slot = metric_now(&config);
            }
        }
        next_payload
    })
}

fn observed_on_response(
    config: &AgentLoopConfig,
    metrics: Option<crate::performance_metrics::AgentLoopPerformanceMetrics>,
    request_metrics: &mut RequestMetricState,
) -> Arc<dyn Fn(pi_ai::types::ProviderResponse, Model) + Send + Sync> {
    let config = config.clone();
    let response_headers_at = Arc::new(std::sync::Mutex::new(request_metrics.response_headers_at));
    let _ = metrics;
    Arc::new(move |provider_response, model| {
        if let Ok(mut slot) = response_headers_at.lock() {
            if slot.is_none() {
                *slot = metric_now(&config);
            }
        }
        if let Some(hook) = config.stream_options.on_response.as_ref() {
            hook(provider_response, model);
        }
    })
}

fn observed_on_usage(
    config: &AgentLoopConfig,
    metrics: Option<crate::performance_metrics::AgentLoopPerformanceMetrics>,
    request_metrics: &mut RequestMetricState,
) -> Option<Arc<dyn Fn(ProviderUsageObservation, Model) + Send + Sync>> {
    let has_metrics = metrics.is_some();
    let caller = config.stream_options.on_usage_observation.clone();
    if !has_metrics && caller.is_none() {
        return None;
    }
    let config = config.clone();
    let provider_usage = Arc::new(std::sync::Mutex::new(request_metrics.provider_usage.clone()));
    Some(Arc::new(move |observation, model| {
        if has_metrics {
            if let Ok(mut slot) = provider_usage.lock() {
                *slot = Some(provider_metric_usage(&observation));
            }
        }
        if let Some(caller) = caller.as_ref() {
            // A caller observer is disposable even when metric recording is off.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| caller(observation, model)));
        }
        let _ = &config;
    }))
}

fn finish_metrics_for(
    _config: &AgentLoopConfig,
    _request_metrics: &mut RequestMetricState,
    _message: Option<&AssistantMessage>,
    _outcome: PerformanceMetricOutcome,
    _metrics: &Option<crate::performance_metrics::AgentLoopPerformanceMetrics>,
) {
    // See stream_assistant_response: the closure body is inlined there because it
    // needs `&mut request_metrics` while the stream is being polled.
}

fn default_convert_to_llm_placeholder() -> Vec<pi_ai::types::Message> {
    Vec::new()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pub struct ExecutedToolCallBatch {
    pub messages: Vec<ToolResultMessage>,
    pub terminate: bool,
}

pub type FinalizedToolCallEntry = Box<dyn FnOnce() -> BoxFuture<'static, FinalizedToolCallOutcome> + Send>;

async fn execute_tool_calls(
    current_context: &mut AgentContext,
    assistant_message: &AssistantMessage,
    config: &AgentLoopConfig,
    signal: Option<&CancellationToken>,
    emit: &AgentEventSink,
) -> anyhow::Result<ExecutedToolCallBatch> {
    let tool_calls = assistant_message.tool_calls();
    let has_sequential_tool_call = tool_calls.iter().any(|tool_call| {
        current_context
            .tools
            .as_ref()
            .and_then(|tools| tools.iter().find(|tool| tool.name == tool_call.name))
            .and_then(|tool| tool.execution_mode)
            == Some(ToolExecutionMode::Sequential)
    });
    if config.resolved_tool_execution() == ToolExecutionMode::Sequential || has_sequential_tool_call {
        return execute_tool_calls_sequential(current_context, assistant_message, &tool_calls, config, signal, emit)
            .await;
    }
    execute_tool_calls_parallel(current_context, assistant_message, &tool_calls, config, signal, emit).await
}

async fn execute_tool_calls_sequential(
    current_context: &mut AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    signal: Option<&CancellationToken>,
    emit: &AgentEventSink,
) -> anyhow::Result<ExecutedToolCallBatch> {
    let mut finalized_calls: Vec<FinalizedToolCallOutcome> = Vec::new();
    let mut messages: Vec<ToolResultMessage> = Vec::new();

    for tool_call in tool_calls {
        if signal.map(|signal| signal.is_cancelled()).unwrap_or(false) {
            break;
        }

        emit_event(
            emit,
            AgentEvent::ToolExecutionStart {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                args: tool_call.arguments.clone(),
            },
        )
        .await?;
        let metric_started_at = metric_now(config);

        let preparation =
            prepare_tool_call(current_context, assistant_message, tool_call, config, signal).await;
        let finalized = match preparation {
            PreparedToolCallOrImmediate::Immediate(immediate) => FinalizedToolCallOutcome {
                tool_call: tool_call.clone(),
                result: immediate.result,
                is_error: immediate.is_error,
            },
            PreparedToolCallOrImmediate::Prepared(prepared) => {
                let executed = execute_prepared_tool_call(&prepared, signal, emit).await;
                finalize_executed_tool_call(current_context, assistant_message, &prepared, executed, config, signal)
                    .await
            }
        };

        record_tool_performance_metric(config, assistant_message, &finalized, metric_started_at, signal);
        emit_tool_execution_end(&finalized, emit).await?;
        let tool_result_message = create_tool_result_message(&finalized, now_ms());
        emit_tool_result_message(&tool_result_message, emit).await?;
        finalized_calls.push(finalized);
        messages.push(tool_result_message);

        if signal.map(|signal| signal.is_cancelled()).unwrap_or(false) {
            break;
        }
    }

    Ok(ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&finalized_calls),
    })
}

async fn execute_tool_calls_parallel(
    current_context: &mut AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    signal: Option<&CancellationToken>,
    emit: &AgentEventSink,
) -> anyhow::Result<ExecutedToolCallBatch> {
    enum Entry {
        Immediate(FinalizedToolCallOutcome),
        Deferred {
            prepared: PreparedToolCall,
            metric_started_at: Option<f64>,
        },
    }

    let mut entries: Vec<Entry> = Vec::new();

    for tool_call in tool_calls {
        emit_event(
            emit,
            AgentEvent::ToolExecutionStart {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                args: tool_call.arguments.clone(),
            },
        )
        .await?;
        let metric_started_at = metric_now(config);

        let preparation =
            prepare_tool_call(current_context, assistant_message, tool_call, config, signal).await;
        match preparation {
            PreparedToolCallOrImmediate::Immediate(immediate) => {
                let finalized = FinalizedToolCallOutcome {
                    tool_call: tool_call.clone(),
                    result: immediate.result,
                    is_error: immediate.is_error,
                };
                record_tool_performance_metric(config, assistant_message, &finalized, metric_started_at, signal);
                emit_tool_execution_end(&finalized, emit).await?;
                entries.push(Entry::Immediate(finalized));
            }
            PreparedToolCallOrImmediate::Prepared(prepared) => {
                entries.push(Entry::Deferred {
                    prepared,
                    metric_started_at,
                });
            }
        }
    }

    // TypeScript runs the deferred entries with Promise.all and keeps source order.
    let mut ordered: Vec<FinalizedToolCallOutcome> = Vec::with_capacity(entries.len());
    let mut deferred: Vec<(PreparedToolCall, Option<f64>)> = Vec::new();
    for entry in entries {
        match entry {
            Entry::Immediate(finalized) => ordered.push(finalized),
            Entry::Deferred {
                prepared,
                metric_started_at,
            } => {
                ordered.push(FinalizedToolCallOutcome {
                    tool_call: prepared.tool_call.clone(),
                    result: AgentToolResult::new(Vec::new(), Value::Object(Map::new())),
                    is_error: false,
                });
                deferred.push((prepared, metric_started_at));
            }
        }
    }

    for (prepared, metric_started_at) in deferred {
        let executed = execute_prepared_tool_call(&prepared, signal, emit).await;
        let finalized =
            finalize_executed_tool_call(current_context, assistant_message, &prepared, executed, config, signal)
                .await;
        record_tool_performance_metric(config, assistant_message, &finalized, metric_started_at, signal);
        emit_tool_execution_end(&finalized, emit).await?;
        if let Some(slot) = ordered
            .iter_mut()
            .find(|slot| slot.tool_call.id == finalized.tool_call.id && slot.result.content.is_empty())
        {
            *slot = finalized;
        }
    }

    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for finalized in &ordered {
        let tool_result_message = create_tool_result_message(finalized, now_ms());
        emit_tool_result_message(&tool_result_message, emit).await?;
        messages.push(tool_result_message);
    }

    Ok(ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&ordered),
    })
}

#[derive(Clone)]
pub struct PreparedToolCall {
    pub tool_call: AgentToolCall,
    pub tool: AgentTool,
    pub args: Value,
}

pub struct ImmediateToolCallOutcome {
    pub result: AgentToolResult,
    pub is_error: bool,
}

pub enum PreparedToolCallOrImmediate {
    Prepared(PreparedToolCall),
    Immediate(ImmediateToolCallOutcome),
}

#[derive(Clone)]
pub struct ExecutedToolCallOutcome {
    pub result: AgentToolResult,
    pub is_error: bool,
}

#[derive(Clone)]
pub struct FinalizedToolCallOutcome {
    pub tool_call: AgentToolCall,
    pub result: AgentToolResult,
    pub is_error: bool,
}

fn record_tool_performance_metric(
    config: &AgentLoopConfig,
    assistant_message: &AssistantMessage,
    finalized: &FinalizedToolCallOutcome,
    started_at: Option<f64>,
    signal: Option<&CancellationToken>,
) {
    let Some(metrics) = config.performance_metrics.as_ref() else {
        return;
    };
    let outcome = if signal.map(|signal| signal.is_cancelled()).unwrap_or(false) {
        PerformanceMetricOutcome::Cancelled
    } else if finalized.is_error {
        PerformanceMetricOutcome::Failure
    } else {
        PerformanceMetricOutcome::Success
    };
    let mut measurements = crate::performance_metrics::PerformanceMetricMeasurements::new();
    measurements.insert(
        PerformanceMetricMeasurement::TotalMs,
        elapsed_metric_ms(started_at, metric_now(config)),
    );
    safe_record_performance_metric(
        Some(&metrics.recorder),
        PerformanceMetricEvent {
            operation: PerformanceMetricOperation::Tool,
            correlation: Some(PerformanceMetricCorrelation {
                logical_request_id: assistant_message
                    .request_correlation
                    .as_ref()
                    .and_then(|correlation| correlation.logical_request_id.clone()),
                provider_attempt_id: None,
                tool_call_id: Some(finalized.tool_call.id.clone()),
            }),
            identity: Some(PerformanceMetricIdentity {
                provider: None,
                model: None,
                api: None,
                component: Some(PerformanceMetricComponent::Tool),
            }),
            outcome: Some(outcome),
            measurements: Some(measurements),
            usage: None,
        },
    );
}

fn should_terminate_tool_batch(finalized_calls: &[FinalizedToolCallOutcome]) -> bool {
    !finalized_calls.is_empty()
        && finalized_calls
            .iter()
            .all(|finalized| finalized.result.terminate == Some(true))
}

fn prepare_tool_call_arguments(tool: &AgentTool, tool_call: &AgentToolCall) -> AgentToolCall {
    let Some(prepare) = tool.prepare_arguments.as_ref() else {
        return tool_call.clone();
    };
    let prepared_arguments = prepare(tool_call.arguments.clone());
    if prepared_arguments == tool_call.arguments {
        return tool_call.clone();
    }
    AgentToolCall {
        arguments: prepared_arguments,
        ..tool_call.clone()
    }
}

async fn prepare_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &AgentToolCall,
    config: &AgentLoopConfig,
    signal: Option<&CancellationToken>,
) -> PreparedToolCallOrImmediate {
    let tool = current_context
        .tools
        .as_ref()
        .and_then(|tools| tools.iter().find(|tool| tool.name == tool_call.name).cloned());
    let Some(tool) = tool else {
        return PreparedToolCallOrImmediate::Immediate(ImmediateToolCallOutcome {
            result: create_error_tool_result(&format!("Tool {} not found", tool_call.name)),
            is_error: true,
        });
    };

    let prepared_tool_call = prepare_tool_call_arguments(&tool, tool_call);
    let pi_tool = pi_ai::types::Tool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
    };
    let validated_args = match validate_tool_arguments(&pi_tool, &prepared_tool_call) {
        Ok(args) => args,
        Err(error) => {
            return PreparedToolCallOrImmediate::Immediate(ImmediateToolCallOutcome {
                result: create_error_tool_result(&format!("{error}")),
                is_error: true,
            });
        }
    };

    if let Some(before_tool_call) = config.before_tool_call.clone() {
        let before_result = match maybe_abortable(
            async move {
                Ok::<_, anyhow::Error>(before_tool_call(
                    crate::types::BeforeToolCallContext {
                        assistant_message: assistant_message.clone(),
                        tool_call: tool_call.clone(),
                        args: validated_args.clone(),
                        context: current_context.clone(),
                    },
                    signal.cloned(),
                ))
            },
            signal.cloned(),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                return PreparedToolCallOrImmediate::Immediate(ImmediateToolCallOutcome {
                    result: create_error_tool_result(&format!("{error}")),
                    is_error: true,
                });
            }
        };
        if before_result.as_ref().and_then(|result| result.block).unwrap_or(false) {
            let reason = before_result
                .and_then(|result| result.reason)
                .unwrap_or_else(|| "Tool execution was blocked".to_string());
            return PreparedToolCallOrImmediate::Immediate(ImmediateToolCallOutcome {
                result: create_error_tool_result(&reason),
                is_error: true,
            });
        }
    }

    PreparedToolCallOrImmediate::Prepared(PreparedToolCall {
        tool_call: prepared_tool_call,
        tool,
        args: validated_args,
    })
}

async fn execute_prepared_tool_call(
    prepared: &PreparedToolCall,
    signal: Option<&CancellationToken>,
    emit: &AgentEventSink,
) -> ExecutedToolCallOutcome {
    let update_events: Arc<tokio::sync::Mutex<Vec<BoxFuture<'static, anyhow::Result<()>>>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let accepting_updates = Arc::new(AtomicBool::new(true));

    if let Err(error) = throw_if_aborted(signal) {
        return ExecutedToolCallOutcome {
            result: create_error_tool_result(&format!("{error}")),
            is_error: true,
        };
    }

    let on_update: AgentToolUpdateCallback = {
        let emit = emit.clone();
        let update_events = update_events.clone();
        let accepting_updates = accepting_updates.clone();
        let signal = signal.cloned();
        let tool_call_id = prepared.tool_call.id.clone();
        let tool_name = prepared.tool_call.name.clone();
        let args = prepared.tool_call.arguments.clone();
        Arc::new(move |partial_result: AgentToolResult| {
            if !accepting_updates.load(Ordering::SeqCst)
                || signal.as_ref().map(|signal| signal.is_cancelled()).unwrap_or(false)
            {
                return;
            }
            let emit = emit.clone();
            let tool_call_id = tool_call_id.clone();
            let tool_name = tool_name.clone();
            let args = args.clone();
            let future: BoxFuture<'static, anyhow::Result<()>> = Box::pin(async move {
                emit(AgentEvent::ToolExecutionUpdate {
                    tool_call_id,
                    tool_name,
                    args,
                    partial_result,
                })
                .await
            });
            if let Ok(mut guard) = update_events.try_lock() {
                guard.push(future);
            }
        })
    };

    let execute = prepared.tool.execute.clone();
    let tool_call_id = prepared.tool_call.id.clone();
    let args = prepared.args.clone();
    let signal_for_tool = signal.cloned();
    let result = race_with_abort(
        async move { execute(tool_call_id, args, signal_for_tool, Some(on_update)).await },
        signal.cloned(),
        None,
    )
    .await;
    accepting_updates.store(false, Ordering::SeqCst);

    match result {
        Ok(result) => {
            let pending: Vec<_> = {
                let mut guard = update_events.lock().await;
                guard.drain(..).collect()
            };
            for future in pending {
                if let Err(error) = future.await {
                    if !signal.map(|signal| signal.is_cancelled()).unwrap_or(false) || !is_abort_error(&error) {
                        return ExecutedToolCallOutcome {
                            result: create_error_tool_result(&format!("{error}")),
                            is_error: true,
                        };
                    }
                }
            }
            ExecutedToolCallOutcome {
                result,
                is_error: false,
            }
        }
        Err(error) => {
            let pending: Vec<_> = {
                let mut guard = update_events.lock().await;
                guard.drain(..).collect()
            };
            for future in pending {
                let _ = future.await;
            }
            let message = if signal.map(|signal| signal.is_cancelled()).unwrap_or(false) {
                "Tool execution aborted".to_string()
            } else {
                format!("{error}")
            };
            ExecutedToolCallOutcome {
                result: create_error_tool_result(&message),
                is_error: true,
            }
        }
    }
}

async fn finalize_executed_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    prepared: &PreparedToolCall,
    executed: ExecutedToolCallOutcome,
    config: &AgentLoopConfig,
    signal: Option<&CancellationToken>,
) -> FinalizedToolCallOutcome {
    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(after_tool_call) = config.after_tool_call.clone() {
        let hook_result = maybe_abortable(
            async move {
                Ok::<_, anyhow::Error>(after_tool_call(
                    crate::types::AfterToolCallContext {
                        assistant_message: assistant_message.clone(),
                        tool_call: prepared.tool_call.clone(),
                        args: prepared.args.clone(),
                        result: result.clone(),
                        is_error,
                        context: current_context.clone(),
                    },
                    signal.cloned(),
                ))
            },
            signal.cloned(),
        )
        .await;

        match hook_result {
            Ok(Some(after_result)) => {
                result = AgentToolResult {
                    content: after_result.content.unwrap_or(result.content),
                    details: after_result.details.unwrap_or(result.details),
                    terminate: after_result.terminate.or(result.terminate),
                };
                is_error = after_result.is_error.unwrap_or(is_error);
            }
            Ok(None) => {}
            Err(error) => {
                result = create_error_tool_result(&format!("{error}"));
                is_error = true;
            }
        }
    }

    FinalizedToolCallOutcome {
        tool_call: prepared.tool_call.clone(),
        result,
        is_error,
    }
}

pub fn create_error_tool_result(message: &str) -> AgentToolResult {
    AgentToolResult::new(
        vec![ContentBlock::text(message)],
        Value::Object(Map::new()),
    )
}

async fn emit_tool_execution_end(
    finalized: &FinalizedToolCallOutcome,
    emit: &AgentEventSink,
) -> anyhow::Result<()> {
    emit_event(
        emit,
        AgentEvent::ToolExecutionEnd {
            tool_call_id: finalized.tool_call.id.clone(),
            tool_name: finalized.tool_call.name.clone(),
            result: finalized.result.clone(),
            is_error: finalized.is_error,
        },
    )
    .await
}

pub fn create_tool_result_message(finalized: &FinalizedToolCallOutcome, timestamp: i64) -> ToolResultMessage {
    ToolResultMessage {
        role: "toolResult".to_string(),
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        content: finalized.result.content.clone(),
        details: Some(finalized.result.details.clone()),
        is_error: finalized.is_error,
        timestamp,
    }
}

async fn emit_tool_result_message(
    tool_result_message: &ToolResultMessage,
    emit: &AgentEventSink,
) -> anyhow::Result<()> {
    emit_event(
        emit,
        AgentEvent::MessageStart {
            message: AgentMessage::ToolResult(tool_result_message.clone()),
        },
    )
    .await?;
    emit_event(
        emit,
        AgentEvent::MessageEnd {
            message: AgentMessage::ToolResult(tool_result_message.clone()),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolExecutionMode;
    use serde_json::json;

    fn assistant_with_tool_call(id: &str, name: &str) -> AssistantMessage {
        let mut message = AssistantMessage::empty("openai-responses", "openai", "gpt-test");
        message.content.push(pi_ai::types::AssistantContentPart::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: json!({}),
            thought_signature: None,
        });
        message
    }

    #[test]
    fn abort_error_message_matches_typescript() {
        assert_eq!(ABORT_ERROR_MESSAGE, "Request was aborted");
        assert!(is_abort_error(&create_abort_error()));
        assert!(!is_abort_error(&anyhow::anyhow!("other")));
    }

    #[test]
    fn aborted_assistant_message_keeps_partial_content_and_usage() {
        let config = AgentLoopConfig::new(Model::unknown());
        let mut partial = AssistantMessage::empty("anthropic-messages", "anthropic", "claude-test");
        partial.content.push(pi_ai::types::AssistantContentPart::Text {
            text: "partial".to_string(),
            text_signature: None,
        });
        partial.usage.input = 5.0;

        let aborted = create_aborted_assistant_message(&config, Some(&partial), 42);
        assert_eq!(aborted.stop_reason, StopReason::Aborted);
        assert_eq!(aborted.error_message.as_deref(), Some(ABORT_ERROR_MESSAGE));
        assert_eq!(aborted.api, "anthropic-messages");
        assert_eq!(aborted.usage.input, 5.0);
        assert_eq!(aborted.timestamp, 42);

        let bare = create_aborted_assistant_message(&config, None, 1);
        assert_eq!(bare.api, "unknown");
        assert_eq!(bare.content.len(), 1);
    }

    #[test]
    fn terminal_batch_only_terminates_when_every_result_asks_for_it() {
        let tool_call = AgentToolCall::new("call-1", "bash", json!({}));
        let mut first = FinalizedToolCallOutcome {
            tool_call: tool_call.clone(),
            result: AgentToolResult::new(Vec::new(), json!({})),
            is_error: false,
        };
        first.result.terminate = Some(true);
        assert!(!should_terminate_tool_batch(&[first.clone()]));
        let mut second = first.clone();
        second.result.terminate = Some(false);
        assert!(!should_terminate_tool_batch(&[first.clone(), second]));
        let mut third = first.clone();
        third.result.terminate = Some(true);
        assert!(should_terminate_tool_batch(&[first, third]));
        assert!(!should_terminate_tool_batch(&[]));
    }

    #[test]
    fn error_tool_result_uses_an_empty_details_object() {
        let result = create_error_tool_result("boom");
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].as_text(), Some("boom"));
        assert_eq!(result.details, json!({}));
        assert_eq!(result.terminate, None);
    }

    #[test]
    fn tool_result_message_keeps_the_source_order_fields() {
        let finalized = FinalizedToolCallOutcome {
            tool_call: AgentToolCall::new("call-9", "read", json!({})),
            result: AgentToolResult::new(vec![ContentBlock::text("body")], json!({"bytes": 4})),
            is_error: true,
        };
        let message = create_tool_result_message(&finalized, 7);
        assert_eq!(message.role, "toolResult");
        assert_eq!(message.tool_call_id, "call-9");
        assert_eq!(message.tool_name, "read");
        assert!(message.is_error);
        assert_eq!(message.timestamp, 7);
        assert_eq!(message.details, Some(json!({"bytes": 4})));
    }

    #[test]
    fn request_metric_outcome_maps_stop_reasons() {
        assert_eq!(request_metric_outcome(StopReason::Aborted), PerformanceMetricOutcome::Cancelled);
        assert_eq!(request_metric_outcome(StopReason::Error), PerformanceMetricOutcome::Failure);
        assert_eq!(request_metric_outcome(StopReason::Stop), PerformanceMetricOutcome::Success);
        assert_eq!(request_metric_outcome(StopReason::ToolUse), PerformanceMetricOutcome::Success);
    }

    #[test]
    fn logical_request_settles_once_even_with_conflicting_outcomes() {
        struct NullRecorder;
        impl PerformanceMetricRecorder for NullRecorder {
            fn session_id(&self) -> &str {
                "session-metrics"
            }
            fn monotonic_now(&self) -> f64 {
                0.0
            }
            fn next_id(&self, scope: crate::performance_metrics::PerformanceMetricIdScope) -> String {
                format!("{scope:?}-1")
            }
            fn record(&self, _event: PerformanceMetricEvent) {}
            fn flush(&self) {}
            fn close(&self) {}
        }
        let finalizer = LogicalRequestMetricFinalizer {
            recorder: Arc::new(NullRecorder),
            logical_request_id: Some("logical-1".to_string()),
            identity: PerformanceMetricIdentity::default(),
            provider_attempt_number: 1,
            settlement: Arc::new(AgentLoopLogicalRequestSettlement::new()),
            started_at: Some(0.0),
            dispatch_edge_at: Some(1.0),
            response_headers_at: Some(2.0),
            first_event_at: Some(3.0),
            first_visible_at: Some(4.0),
        };
        settle_logical_request_metric(&finalizer, PerformanceMetricOutcome::Success);
        settle_logical_request_metric(&finalizer, PerformanceMetricOutcome::Failure);
        assert!(finalizer.settlement.is_settled());
    }

    #[test]
    fn prepared_tool_call_arguments_returns_the_same_call_when_nothing_changes() {
        let tool = AgentTool {
            name: "bash".to_string(),
            description: "run".to_string(),
            parameters: json!({}),
            label: "Bash".to_string(),
            prepare_arguments: None,
            execute: Arc::new(|_, _, _, _| Box::pin(async { Ok(AgentToolResult::new(Vec::new(), json!({}))) })),
            execution_mode: None,
        };
        let tool_call = AgentToolCall::new("call-1", "bash", json!({"command": "ls"}));
        let prepared = prepare_tool_call_arguments(&tool, &tool_call);
        assert_eq!(prepared.arguments, tool_call.arguments);
    }

    #[test]
    fn parallel_entries_keep_assistant_source_order_for_messages() {
        // Mirrors executeToolCallsParallel: finalized tool-result messages are
        // emitted in assistant source order, not completion order.
        let tool_call_a = AgentToolCall::new("call-a", "bash", json!({}));
        let tool_call_b = AgentToolCall::new("call-b", "bash", json!({}));
        let finalized_a = FinalizedToolCallOutcome {
            tool_call: tool_call_a.clone(),
            result: AgentToolResult::new(vec![ContentBlock::text("a")], json!({})),
            is_error: false,
        };
        let finalized_b = FinalizedToolCallOutcome {
            tool_call: tool_call_b.clone(),
            result: AgentToolResult::new(vec![ContentBlock::text("b")], json!({})),
            is_error: false,
        };
        let messages = vec![
            create_tool_result_message(&finalized_a, 1),
            create_tool_result_message(&finalized_b, 2),
        ];
        assert_eq!(messages[0].tool_call_id, "call-a");
        assert_eq!(messages[1].tool_call_id, "call-b");
    }

    #[test]
    fn agent_loop_continue_rejects_empty_and_assistant_tail_contexts() {
        let config = AgentLoopConfig::new(Model::unknown());
        let empty = agent_loop_continue(AgentContext::default(), config.clone(), None, None);
        assert!(empty.is_err());
        assert_eq!(
            empty.err().map(|error| error.to_string()).unwrap_or_default(),
            "Cannot continue: no messages in context"
        );

        let context = AgentContext {
            system_prompt: String::new(),
            messages: vec![AgentMessage::Assistant(AssistantMessage::empty("a", "b", "c"))],
            tools: None,
        };
        let err = agent_loop_continue(context, config, None, None).err().map(|error| error.to_string());
        assert_eq!(err.unwrap_or_default(), "Cannot continue from message role: assistant");
    }
}
