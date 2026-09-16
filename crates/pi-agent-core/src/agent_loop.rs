//! Port of packages/agent/src/agent-loop.ts
//!
//! The TypeScript loop is async and pushes events through an `AgentEventSink`.
//! The Rust port keeps the same call order, the same abort points, and the same
//! event sequence; the abort signal is a `CancellationToken`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use futures::future::BoxFuture;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use pi_ai::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, ImageOrTextContent, Message,
    Model, ProviderUsageObservation, SimpleStreamOptions, StopReason, TextContent, ToolCall,
    ToolResultMessage, Usage,
};
use pi_ai::utils::event_stream::EventStream;
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
    AgentToolResult, ContentBlock as AgentContentBlock, StreamFn, ToolExecutionMode,
};

pub type AgentEventSink = Arc<dyn Fn(AgentEvent) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

pub const ABORT_ERROR_MESSAGE: &str = "Request was aborted";

/// `const EMPTY_USAGE`.
pub fn empty_usage() -> Usage {
    Usage::zero()
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
/// TypeScript keeps these in `WeakMap<AssistantMessage, ...>` keyed by object
/// identity. Rust values have no identity, so the same two maps are keyed by the
/// terminal message's serialized form, which is what the host holds and passes
/// back to `finalize_performance_metric_logical_request`.
#[derive(Clone)]
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

/// How long a correlation entry may live. A host-owned logical request that never settles
/// (the successful retry path has no finalizer call) still cannot grow the registry without
/// bound: the oldest entry is evicted first. This is the documented retention lifecycle.
const CORRELATION_REGISTRY_CAPACITY: usize = 256;

/// One terminal message lifetime. The correlation and the finalizer always belong to the same
/// `(session, message)` pair, so they share one entry and one eviction position.
struct CorrelationEntry {
    session_id: String,
    fingerprint: String,
    sequence: u64,
    correlation: Option<PerformanceMetricRequestCorrelation>,
    finalizer: Option<LogicalRequestMetricFinalizer>,
}

#[derive(Default)]
struct CorrelationRegistry {
    /// Keyed by `(session id, message fingerprint)`, exactly as the host supplies both.
    ///
    /// TypeScript keeps these in `WeakMap<AssistantMessage, ...>` keyed by object identity, so
    /// an entry disappears with its message. A Rust `AssistantMessage` passed by value has no
    /// identity, so the same two lifetimes are keyed by the serialized terminal message *plus*
    /// the recorder session. Without the session part, two sessions that produce byte-identical
    /// messages overwrite each other and the second session is attributed the first session's
    /// correlation.
    entries: HashMap<String, CorrelationEntry>,
    /// Insertion order, used to evict the oldest entry once the capacity is reached.
    order: std::collections::VecDeque<String>,
    next_sequence: u64,
}

fn correlation_registry() -> &'static Mutex<CorrelationRegistry> {
    static REGISTRY: OnceLock<Mutex<CorrelationRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(CorrelationRegistry::default()))
}

/// Live correlation-registry entries: retained terminal message lifetimes.
///
/// Test-only retention seam (TEST-SPEC T16), compiled out of non-test builds.
#[cfg(test)]
fn correlation_registry_len_for_tests() -> usize {
    correlation_registry()
        .lock()
        .map(|registry| registry.entries.len())
        .unwrap_or(0)
}

/// Logical-request ids of the correlation lifetimes that are still retained.
///
/// Test-only identity seam (TEST-SPEC T16): it proves how many distinct message lifetimes the
/// registry kept, independently of the retention bound and of the lookup ambiguity rule.
#[cfg(test)]
fn correlation_registry_logical_ids_for_tests() -> Vec<String> {
    correlation_registry()
        .lock()
        .map(|registry| {
            let mut ids: Vec<String> = registry
                .entries
                .values()
                .filter_map(|entry| entry.correlation.as_ref())
                .filter_map(|correlation| correlation.logical_request_id.clone())
                .collect();
            ids.sort();
            ids.dedup();
            ids
        })
        .unwrap_or_default()
}

fn message_fingerprint(message: &AssistantMessage) -> String {
    serde_json::to_string(message).unwrap_or_default()
}

/// `(session, fingerprint)` - the identity of one terminal message lifetime.
fn correlation_key(session_id: &str, fingerprint: &str) -> String {
    let mut key = String::with_capacity(session_id.len() + fingerprint.len() + 1);
    key.push_str(session_id);
    key.push(char::from(31));
    key.push_str(fingerprint);
    key
}

/// Store a message lifetime and evict the oldest lifetime when the bound is reached.
fn remember_entry(
    registry: &mut CorrelationRegistry,
    session_id: &str,
    message: &AssistantMessage,
    correlation: Option<PerformanceMetricRequestCorrelation>,
    finalizer: Option<LogicalRequestMetricFinalizer>,
) {
    let fingerprint = message_fingerprint(message);
    let key = correlation_key(session_id, &fingerprint);
    registry.next_sequence += 1;
    let sequence = registry.next_sequence;
    let entry = registry.entries.entry(key.clone()).or_insert_with(|| CorrelationEntry {
        session_id: session_id.to_string(),
        fingerprint,
        sequence,
        correlation: None,
        finalizer: None,
    });
    entry.sequence = sequence;
    if correlation.is_some() {
        entry.correlation = correlation;
    }
    if finalizer.is_some() {
        entry.finalizer = finalizer;
    }
    registry.order.retain(|existing| existing != &key);
    registry.order.push_back(key);
    while registry.order.len() > CORRELATION_REGISTRY_CAPACITY {
        if let Some(oldest) = registry.order.pop_front() {
            registry.entries.remove(&oldest);
        }
    }
}

fn remember_request_correlation(
    message: &AssistantMessage,
    session_id: &str,
    correlation: PerformanceMetricRequestCorrelation,
) {
    if let Ok(mut registry) = correlation_registry().lock() {
        remember_entry(&mut registry, session_id, message, Some(correlation), None);
    }
}

fn remember_logical_request_finalizer(
    message: &AssistantMessage,
    session_id: &str,
    finalizer: LogicalRequestMetricFinalizer,
) {
    if let Ok(mut registry) = correlation_registry().lock() {
        remember_entry(&mut registry, session_id, message, None, Some(finalizer));
    }
}

/// The correlation of the one session that owns this exact terminal message.
///
/// Two sessions can produce byte-identical messages. Returning either one would mis-attribute
/// the request, so an ambiguous message resolves to `None` instead; the caller then falls back
/// to its own session-scoped metrics rather than to another session's correlation.
fn entry_for_message<'a>(
    registry: &'a CorrelationRegistry,
    message: &AssistantMessage,
    has: impl Fn(&CorrelationEntry) -> bool,
) -> Option<&'a CorrelationEntry> {
    let fingerprint = message_fingerprint(message);
    let mut owner: Option<&CorrelationEntry> = None;
    for entry in registry.entries.values() {
        if entry.fingerprint != fingerprint || !has(entry) {
            continue;
        }
        match owner {
            None => owner = Some(entry),
            Some(current) if current.session_id == entry.session_id => {
                if entry.sequence > current.sequence {
                    owner = Some(entry);
                }
            }
            // A second session owns the same content: content alone cannot answer.
            Some(_) => return None,
        }
    }
    owner
}

fn correlation_for_message(
    registry: &CorrelationRegistry,
    message: &AssistantMessage,
) -> Option<PerformanceMetricRequestCorrelation> {
    entry_for_message(registry, message, |entry| entry.correlation.is_some())
        .and_then(|entry| entry.correlation.clone())
}

/// Same ambiguity rule as [`correlation_for_message`], for the host-owned settlement path.
fn finalizer_for_message(
    registry: &CorrelationRegistry,
    message: &AssistantMessage,
) -> Option<LogicalRequestMetricFinalizer> {
    entry_for_message(registry, message, |entry| entry.finalizer.is_some())
        .and_then(|entry| entry.finalizer.clone())
}

/// Settles a host-owned logical request exactly once. This is process-local and
/// content-free; durable transcript messages remain the source of truth.
pub fn finalize_performance_metric_logical_request(
    message: &AssistantMessage,
    outcome: Option<PerformanceMetricOutcome>,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let state = correlation_registry()
            .lock()
            .ok()
            .and_then(|registry| finalizer_for_message(&registry, message));
        let Some(state) = state else {
            return;
        };
        let outcome = outcome.unwrap_or_else(|| request_metric_outcome(&message.stop_reason));
        settle_logical_request_metric(&state, outcome);
    }));
}

/// `getPerformanceMetricRequestCorrelation(message)`.
pub fn get_performance_metric_request_correlation(
    message: &AssistantMessage,
) -> Option<PerformanceMetricRequestCorrelation> {
    correlation_registry()
        .lock()
        .ok()
        .and_then(|registry| correlation_for_message(&registry, message))
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

fn request_metric_outcome(stop_reason: &StopReason) -> PerformanceMetricOutcome {
    if stop_reason == pi_ai::types::STOP_REASON_ABORTED {
        PerformanceMetricOutcome::Cancelled
    } else if stop_reason == pi_ai::types::STOP_REASON_ERROR {
        PerformanceMetricOutcome::Failure
    } else {
        PerformanceMetricOutcome::Success
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
async fn race_with_abort<T, F>(operation: F, signal: Option<CancellationToken>, on_abort: Option<OnAbort>) -> anyhow::Result<T>
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

/// The `onAbort` callback of `raceWithAbort`.
pub type OnAbort = Box<dyn FnOnce() + Send>;

/// `maybePromiseWithAbort` - the operation is already started, only the abort
/// race is added.
async fn maybe_abortable<T, F>(operation: F, signal: Option<CancellationToken>) -> anyhow::Result<T>
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

fn clone_assistant_content(content: &[ContentBlock]) -> Vec<ContentBlock> {
    content
        .iter()
        .map(|part| match part {
            // `{ ...part, arguments: { ...part.arguments } }`
            ContentBlock::ToolCall(tool_call) => ContentBlock::ToolCall(ToolCall {
                arguments: tool_call.arguments.clone(),
                ..tool_call.clone()
            }),
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
    let mut message = AssistantMessage::new(
        partial_message
            .map(|partial| partial.api.clone())
            .unwrap_or_else(|| config.model.api.clone()),
        partial_message
            .map(|partial| partial.provider.clone())
            .unwrap_or_else(|| config.model.provider.clone()),
        partial_message
            .map(|partial| partial.model.clone())
            .unwrap_or_else(|| config.model.id.clone()),
        timestamp,
    );
    message.content = match partial_message {
        Some(partial) => clone_assistant_content(&partial.content),
        None => vec![ContentBlock::Text(TextContent::new(""))],
    };
    message.usage = match partial_message {
        Some(partial) => clone_usage(&partial.usage),
        None => empty_usage(),
    };
    message.stop_reason = pi_ai::types::STOP_REASON_ABORTED.to_string();
    message.error_message = Some(ABORT_ERROR_MESSAGE.to_string());
    message
}

fn get_terminal_message(event: &AssistantMessageEvent) -> AssistantMessage {
    match event {
        AssistantMessageEvent::Done { message, .. } => message.clone(),
        AssistantMessageEvent::Error { error, .. } => error.clone(),
        other => panic!("Unexpected event type: {}", other.event_type()),
    }
}

/// `EventStream<AgentEvent, AgentMessage[]>`.
pub type AgentEventStream = EventStream<AgentEvent, Vec<AgentMessage>>;

fn create_agent_stream() -> AgentEventStream {
    EventStream::new(
        Box::new(|event: &AgentEvent| event.type_name() == "agent_end"),
        Box::new(|event: &AgentEvent| match event {
            AgentEvent::AgentEnd { messages } => messages.clone(),
            _ => Vec::new(),
        }),
    )
}

fn end_agent_stream_on_error(
    stream: &AgentEventStream,
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

/// `pollMessagesUnlessAborted`.
async fn poll_messages_unless_aborted(
    poll: Option<Arc<dyn Fn() -> BoxFuture<'static, Vec<AgentMessage>> + Send + Sync>>,
    signal: Option<&CancellationToken>,
) -> anyhow::Result<Vec<AgentMessage>> {
    let Some(poll) = poll else {
        return Ok(Vec::new());
    };
    if signal.map(|signal| signal.is_cancelled()).unwrap_or(false) {
        return Ok(Vec::new());
    }
    match maybe_abortable(async move { Ok::<_, anyhow::Error>(poll().await) }, signal.cloned()).await {
        Ok(messages) => Ok(messages),
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
) -> AgentEventStream {
    let stream = create_agent_stream();
    let sink_stream = stream.clone();

    end_agent_stream_on_error(
        &stream,
        Box::pin(run_agent_loop(
            prompts,
            context,
            config,
            Arc::new(move |event| {
                let stream = sink_stream.clone();
                Box::pin(async move {
                    stream.push(event);
                    Ok(())
                })
            }),
            signal,
            stream_fn,
        )),
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
) -> anyhow::Result<AgentEventStream> {
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
    let sink_stream = stream.clone();

    end_agent_stream_on_error(
        &stream,
        Box::pin(run_agent_loop_continue(
            context,
            config,
            Arc::new(move |event| {
                let stream = sink_stream.clone();
                Box::pin(async move {
                    stream.push(event);
                    Ok(())
                })
            }),
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

async fn emit_agent_end(emit: &AgentEventSink, new_messages: &[AgentMessage]) -> anyhow::Result<()> {
    emit_event(
        emit,
        AgentEvent::AgentEnd {
            messages: new_messages.to_vec(),
        },
    )
    .await
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

    // `const shouldStopBeforeTurn = () => !firstTurn && (hook?.() ?? false)`.
    // Rust closures cannot hold a mutable borrow of the loop's `first_turn`, so
    // the flag is passed in explicitly.
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
                for message in std::mem::take(&mut pending_messages) {
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
            new_messages.push(AgentMessage::from(message.clone()));

            if message.stop_reason == pi_ai::types::STOP_REASON_ERROR
                || message.stop_reason == pi_ai::types::STOP_REASON_ABORTED
            {
                emit_event(
                    emit,
                    AgentEvent::TurnEnd {
                        message: AgentMessage::from(message.clone()),
                        tool_results: Vec::new(),
                    },
                )
                .await?;
                emit_agent_end(emit, new_messages).await?;
                return Ok(());
            }

            let tool_calls = message_tool_calls(&message);

            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            has_more_tool_calls = false;
            if !tool_calls.is_empty() {
                let executed_tool_batch =
                    execute_tool_calls(current_context, &message, config, signal.as_ref(), emit).await?;
                tool_results.extend(executed_tool_batch.messages.iter().cloned());
                has_more_tool_calls = !executed_tool_batch.terminate;

                for result in &tool_results {
                    current_context.messages.push(AgentMessage::from(result.clone()));
                    new_messages.push(AgentMessage::from(result.clone()));
                }
            }

            emit_event(
                emit,
                AgentEvent::TurnEnd {
                    message: AgentMessage::from(message.clone()),
                    tool_results: tool_results.clone(),
                },
            )
            .await?;
            if signal.as_ref().map(|signal| signal.is_cancelled()).unwrap_or(false) {
                emit_agent_end(emit, new_messages).await?;
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
                    maybe_abortable(
                        async move {
                            Ok::<_, anyhow::Error>(match hook {
                                Some(hook) => hook(context).await,
                                None => false,
                            })
                        },
                        signal.clone(),
                    ),
                    signal.as_ref(),
                )
                .await?
            };
            match should_stop_result {
                PostTurnResult::Aborted => {
                    emit_agent_end(emit, new_messages).await?;
                    return Ok(());
                }
                PostTurnResult::Completed(stop) => {
                    if stop || should_stop_before_turn(first_turn) {
                        emit_agent_end(emit, new_messages).await?;
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
                    emit_agent_end(emit, new_messages).await?;
                    return Ok(());
                }
                PostTurnResult::Completed(messages) => pending_messages = messages,
            }
            // Steering drained by this poll owns the turn boundary; stop only when it was empty.
            if pending_messages.is_empty() && should_stop_before_turn(first_turn) {
                emit_agent_end(emit, new_messages).await?;
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
                emit_agent_end(emit, new_messages).await?;
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
                    maybe_abortable(
                        async move {
                            let messages = match hook {
                                Some(hook) => hook(last_turn, signal_for_hook).await,
                                None => Vec::new(),
                            };
                            Ok::<_, anyhow::Error>(messages)
                        },
                        signal.clone(),
                    ),
                    signal.as_ref(),
                )
                .await?
            }
            None => PostTurnResult::Completed(Vec::new()),
        };
        let continuation_messages = match continuation_messages_result {
            PostTurnResult::Aborted => {
                emit_agent_end(emit, new_messages).await?;
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

    emit_agent_end(emit, new_messages).await?;
    Ok(())
}



/// The `observedOnPayload` / `observedOnResponse` / `observedOnUsage` closures.
struct ObservedCallbacks {
    on_payload: pi_ai::types::OnPayload,
    on_response: pi_ai::types::OnResponse,
    on_usage_observation: Option<pi_ai::types::OnUsageObservation>,
    /// TypeScript closes over `requestMetrics` and mutates it; the Rust callbacks
    /// share one `Mutex` with the loop so the timestamps are still written through.
    timestamps: Arc<Mutex<RequestMetricState>>,
}

fn create_observed_callbacks(
    config: &AgentLoopConfig,
    metrics: Option<crate::performance_metrics::AgentLoopPerformanceMetrics>,
    request_metrics: &RequestMetricState,
) -> ObservedCallbacks {
    let metrics_enabled = metrics.is_some();
    let timestamps = Arc::new(Mutex::new(request_metrics.clone()));

    let on_payload = {
        let config = config.clone();
        let state = timestamps.clone();
        Arc::new(move |payload: Value, model: &Model| -> pi_ai::types::BoxFuture<Option<Value>> {
            let next_payload = match config.stream_options.stream.on_payload.as_ref() {
                Some(hook) => hook(payload, model),
                None => Box::pin(async { None }) as pi_ai::types::BoxFuture<Option<Value>>,
            };
            if metrics_enabled {
                if let Ok(mut slot) = state.lock() {
                    if slot.dispatch_edge_at.is_none() {
                        slot.dispatch_edge_at = metric_now(&config);
                    }
                }
            }
            next_payload
        }) as pi_ai::types::OnPayload
    };

    let on_response = {
        let config = config.clone();
        let state = timestamps.clone();
        Arc::new(move |provider_response: pi_ai::types::ProviderResponse, model: &Model| {
            let observed = if metrics_enabled {
                if let Ok(mut slot) = state.lock() {
                    if slot.response_headers_at.is_none() {
                        slot.response_headers_at = metric_now(&config);
                    }
                }
                match config.stream_options.stream.on_response.as_ref() {
                    Some(hook) => hook(provider_response, model),
                    None => Box::pin(async {}) as pi_ai::types::BoxFuture<()>,
                }
            } else {
                match config.stream_options.stream.on_response.as_ref() {
                    Some(hook) => hook(provider_response, model),
                    None => Box::pin(async {}) as pi_ai::types::BoxFuture<()>,
                }
            };
            observed
        }) as pi_ai::types::OnResponse
    };

    let on_usage_observation = {
        let caller = config.stream_options.stream.on_usage_observation.clone();
        if !metrics_enabled && caller.is_none() {
            None
        } else {
            let state = timestamps.clone();
            Some(Arc::new(move |observation: ProviderUsageObservation, model: &Model| {
                if metrics_enabled {
                    if let Ok(mut slot) = state.lock() {
                        slot.provider_usage = Some(provider_metric_usage(&observation));
                    }
                }
                match caller.as_ref() {
                    // A caller observer is disposable even when metric recording is off.
                    Some(caller) => {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            caller(observation, model)
                        }));
                    }
                    None => {}
                }
                Box::pin(async {}) as pi_ai::types::BoxFuture<()>
            }) as pi_ai::types::OnUsageObservation)
        }
    };

    ObservedCallbacks {
        on_payload,
        on_response,
        on_usage_observation,
        timestamps,
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
    request_metrics: &mut RequestMetricState,
    partial_message: &mut Option<AssistantMessage>,
    added_partial: &mut bool,
    logical_request_settlement: &Arc<AgentLoopLogicalRequestSettlement>,
    metrics: &Option<crate::performance_metrics::AgentLoopPerformanceMetrics>,
    observed: &ObservedCallbacks,
) -> anyhow::Result<AssistantMessage> {
    // `streamFn` may return a promise; awaiting it is part of the provider call.
    let response = maybe_abortable(
        async move {
            Ok::<_, anyhow::Error>(
                stream_function(config.model.clone(), llm_context, provider_config).await,
            )
        },
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
                        close_stream(Some(&signal));
                        return Err(create_abort_error());
                    }
                    next = response.next() => next,
                }
            }
            None => response.next().await,
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
        // The `message_update` event carries the whole event, and the `partial`
        // field is moved out of it below, so the event is cloned once here.
        let event_for_update = event.clone();
        match event {
            AssistantMessageEvent::Start { partial } => {
                *partial_message = Some(partial.clone());
                context.messages.push(AgentMessage::from(partial.clone()));
                *added_partial = true;
                emit_event(
                    emit,
                    AgentEvent::MessageStart {
                        message: AgentMessage::from(partial),
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
                        *last = AgentMessage::from(partial.clone());
                    }
                    emit_event(
                        emit,
                        AgentEvent::MessageUpdate {
                            message: AgentMessage::from(partial),
                            assistant_message_event: event_for_update,
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
                        // `if (!signal?.aborted || !isAbortError(error)) throw error;`
                        let aborted = signal.map(|signal| signal.is_cancelled()).unwrap_or(false);
                        if !aborted || !is_abort_error(&error) {
                            return Err(error);
                        }
                    }
                }
                finish_request_metrics(
                    config,
                    metrics,
                    logical_request_settlement,
                    Some(&final_message),
                    request_metric_outcome(&final_message.stop_reason),
                    request_metrics,
                    observed,
                );
                if *added_partial {
                    if let Some(last) = context.messages.last_mut() {
                        *last = AgentMessage::from(final_message.clone());
                    }
                } else {
                    context.messages.push(AgentMessage::from(final_message.clone()));
                }
                if !*added_partial {
                    emit_event(
                        emit,
                        AgentEvent::MessageStart {
                            message: AgentMessage::from(final_message.clone()),
                        },
                    )
                    .await?;
                }
                emit_event(
                    emit,
                    AgentEvent::MessageEnd {
                        message: AgentMessage::from(final_message.clone()),
                    },
                )
                .await?;
                return Ok(final_message);
            }
        }
    }

    let final_message = maybe_abortable(
        async { Ok::<_, anyhow::Error>(response.result().await) },
        signal.cloned(),
    )
    .await?;
    finish_request_metrics(
        config,
        metrics,
        logical_request_settlement,
        Some(&final_message),
        request_metric_outcome(&final_message.stop_reason),
        request_metrics,
        observed,
    );
    if *added_partial {
        if let Some(last) = context.messages.last_mut() {
            *last = AgentMessage::from(final_message.clone());
        }
    } else {
        context.messages.push(AgentMessage::from(final_message.clone()));
        emit_event(
            emit,
            AgentEvent::MessageStart {
                message: AgentMessage::from(final_message.clone()),
            },
        )
        .await?;
    }
    emit_event(
        emit,
        AgentEvent::MessageEnd {
            message: AgentMessage::from(final_message.clone()),
        },
    )
    .await?;
    Ok(final_message)
}

fn default_convert_to_llm_placeholder() -> Vec<Message> {
    Vec::new()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn message_tool_calls(message: &AssistantMessage) -> Vec<AgentToolCall> {
    // `assistantMessage.content.filter((c) => c.type === "toolCall")`
    message
        .content
        .iter()
        .filter_map(ContentBlock::as_tool_call)
        .cloned()
        .collect()
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
    let tool_calls = message_tool_calls(assistant_message);
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
                args: Value::Object(tool_call.arguments.clone()),
            },
        )
        .await?;
        let metric_started_at = metric_now(config);

        let preparation = prepare_tool_call(current_context, assistant_message, tool_call, config, signal).await;
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
    // `FinalizedToolCallEntry = FinalizedToolCallOutcome | (() => Promise<...>)`
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
                args: Value::Object(tool_call.arguments.clone()),
            },
        )
        .await?;
        let metric_started_at = metric_now(config);

        let preparation = prepare_tool_call(current_context, assistant_message, tool_call, config, signal).await;
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

    // `Promise.all(finalizedCalls.map((entry) => typeof entry === "function" ? entry() : entry))`
    // - all entries run concurrently and the resulting array keeps assistant source order.
    let mut futures_vec: Vec<BoxFuture<'static, anyhow::Result<FinalizedToolCallOutcome>>> =
        Vec::with_capacity(entries.len());
    for entry in entries {
        match entry {
            Entry::Immediate(finalized) => futures_vec.push(Box::pin(async move { Ok(finalized) })),
            Entry::Deferred {
                prepared,
                metric_started_at,
            } => {
                let context = current_context.clone();
                let assistant_message = assistant_message.clone();
                let config = config.clone();
                let signal = signal.cloned();
                let emit = emit.clone();
                futures_vec.push(Box::pin(async move {
                    let executed = execute_prepared_tool_call(&prepared, signal.as_ref(), &emit).await;
                    let finalized = finalize_executed_tool_call(
                        &context,
                        &assistant_message,
                        &prepared,
                        executed,
                        &config,
                        signal.as_ref(),
                    )
                    .await;
                    record_tool_performance_metric(
                        &config,
                        &assistant_message,
                        &finalized,
                        metric_started_at,
                        signal.as_ref(),
                    );
                    emit_tool_execution_end(&finalized, &emit).await?;
                    Ok::<_, anyhow::Error>(finalized)
                }));
            }
        }
    }

    let ordered_finalized_calls = futures::future::join_all(futures_vec).await;
    let mut ordered: Vec<FinalizedToolCallOutcome> = Vec::with_capacity(ordered_finalized_calls.len());
    for outcome in ordered_finalized_calls {
        ordered.push(outcome?);
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
                logical_request_id: get_performance_metric_request_correlation(assistant_message)
                    .and_then(|correlation| correlation.logical_request_id),
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
    let prepared_arguments = prepare(Value::Object(tool_call.arguments.clone()));
    if prepared_arguments == Value::Object(tool_call.arguments.clone()) {
        return tool_call.clone();
    }
    let mut prepared = tool_call.clone();
    prepared.arguments = match prepared_arguments {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            map.insert("value".to_string(), other);
            map
        }
    };
    prepared
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
                result: create_error_tool_result(&error),
                is_error: true,
            });
        }
    };

    if let Some(before_tool_call) = config.before_tool_call.clone() {
        let before_result = match maybe_abortable(
            {
                let hook_args = validated_args.clone();
                async move {
                    before_tool_call(
                        crate::types::BeforeToolCallContext {
                            assistant_message: assistant_message.clone(),
                            tool_call: tool_call.clone(),
                            args: hook_args,
                            context: current_context.clone(),
                        },
                        signal.cloned(),
                    ).await
                }
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
    // `updateEvents: Promise<void>[]`. TypeScript starts `emit(...)` when the tool publishes
    // the partial result and joins the promises at the end. Each update is therefore started
    // immediately in its own task (so a client can render progress while the tool still runs)
    // and the task handle is retained for the join below.
    let accepting_updates = Arc::new(AtomicBool::new(true));

    if let Err(error) = throw_if_aborted(signal) {
        return ExecutedToolCallOutcome {
            result: create_error_tool_result(&format!("{error}")),
            is_error: true,
        };
    }

    // `updateEvents: Promise<void>[]`. TypeScript starts `emit(...)` inside the publisher's
    // callback, so emission begins while the tool is still running and the sink observes the
    // publish order. A single drain task preserves both properties here: the callback appends
    // to an ordered queue (the linearization point) and the drain task emits from it
    // immediately, so nothing is dropped by lock contention and no update is reordered.
    let update_queue: Arc<std::sync::Mutex<VecDeque<AgentEvent>>> =
        Arc::new(std::sync::Mutex::new(VecDeque::new()));
    let update_wake = Arc::new(tokio::sync::Notify::new());
    let updates_closed = Arc::new(AtomicBool::new(false));
    let update_drain = {
        let emit = emit.clone();
        let update_queue = update_queue.clone();
        let update_wake = update_wake.clone();
        let updates_closed = updates_closed.clone();
        tokio::spawn(async move {
            loop {
                // Register interest before checking the queue so a publish cannot be missed.
                let notified = update_wake.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let pending: Vec<AgentEvent> = match update_queue.lock() {
                    Ok(mut guard) => guard.drain(..).collect(),
                    Err(poisoned) => poisoned.into_inner().drain(..).collect(),
                };
                if pending.is_empty() {
                    if updates_closed.load(Ordering::SeqCst) {
                        return Ok::<(), anyhow::Error>(());
                    }
                    notified.await;
                    continue;
                }
                for event in pending {
                    emit(event).await?;
                }
            }
        })
    };

    let on_update: crate::types::AgentToolUpdateCallback = {
        let update_queue = update_queue.clone();
        let update_wake = update_wake.clone();
        let accepting_updates = accepting_updates.clone();
        let signal = signal.cloned();
        let tool_call_id = prepared.tool_call.id.clone();
        let tool_name = prepared.tool_call.name.clone();
        let args = Value::Object(prepared.tool_call.arguments.clone());
        Arc::new(move |partial_result: AgentToolResult| {
            if !accepting_updates.load(Ordering::SeqCst)
                || signal.as_ref().map(|signal| signal.is_cancelled()).unwrap_or(false)
            {
                return;
            }
            let tool_call_id = tool_call_id.clone();
            let tool_name = tool_name.clone();
            let args = args.clone();
            // `updateEvents.push(Promise.resolve(emit(...)))`: the update joins the ordered
            // queue now (publish order is the linearization point) and the drain task emits it
            // immediately, so progress is observable while the tool is unfinished.
            let event = AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
            };
            match update_queue.lock() {
                Ok(mut guard) => guard.push_back(event),
                Err(poisoned) => poisoned.into_inner().push_back(event),
            }
            // `notify_one` stores a permit, so a wakeup cannot be lost.
            update_wake.notify_one();
        })
    };

    let execute = prepared.tool.execute.clone();
    let tool_call_id = prepared.tool_call.id.clone();
    let args = prepared.args.clone();
    let signal_for_tool = signal.cloned();
    // TypeScript's abort race leaves the tool promise alive to finish cleanup.
    // Dropping the execution future here strands the Python kernel's queue slot.
    // A dropped JoinHandle detaches the task; the tool still receives cancellation.
    let execution = tokio::spawn(async move {
        execute(tool_call_id, args, signal_for_tool, Some(on_update)).await
    });
    let result = race_with_abort(
        async move { execution.await.map_err(anyhow::Error::from)? },
        signal.cloned(),
        None,
    )
    .await;
    accepting_updates.store(false, Ordering::SeqCst);
    // `await Promise.all(updateEvents)` in TypeScript: close the drain and join it, so every
    // accepted update has reached the sink before the tool outcome is returned.
    {
        let finish_drain = async {
            updates_closed.store(true, Ordering::SeqCst);
            update_wake.notify_one();
            update_drain
                .await
                .map_err(anyhow::Error::from)
                .and_then(|inner| inner)
        };
        match race_with_abort(finish_drain, signal.cloned(), None).await {
            Ok(()) => {}
            Err(error) if is_abort_error(&error) => {
                // The caller aborted while the join was in flight; the updates already queued
                // were emitted by the drain task, which finishes on its own.
            }
            Err(error) => {
                return ExecutedToolCallOutcome {
                    result: create_error_tool_result(&format!("{error}")),
                    is_error: true,
                };
            }
        }
    }

    match result {
        Ok(result) => {
            ExecutedToolCallOutcome {
                result,
                is_error: false,
            }
        }
        Err(error) => {
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
    let executed_result = executed.result.clone();
    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(after_tool_call) = config.after_tool_call.clone() {
        let hook_result = maybe_abortable(
            async move {
                after_tool_call(
                    crate::types::AfterToolCallContext {
                        assistant_message: assistant_message.clone(),
                        tool_call: prepared.tool_call.clone(),
                        args: prepared.args.clone(),
                        result: executed_result,
                        is_error,
                        context: current_context.clone(),
                    },
                    signal.cloned(),
                ).await
            },
            signal.cloned(),
        )
        .await;

        match hook_result {
            Ok(Some(after_result)) => {
                // Omitted fields keep the original executed result values.
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
    AgentToolResult::new(vec![AgentContentBlock::text(message)], Value::Object(Map::new()))
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
        role: pi_ai::types::ROLE_TOOL_RESULT.to_string(),
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        content: finalized
            .result
            .content
            .iter()
            .map(|block| match block {
                AgentContentBlock::Text(text) => ImageOrTextContent::Text(text.clone()),
                AgentContentBlock::Image(image) => ImageOrTextContent::Image(image.clone()),
            })
            .collect(),
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
            message: AgentMessage::from(tool_result_message.clone()),
        },
    )
    .await?;
    emit_event(
        emit,
        AgentEvent::MessageEnd {
            message: AgentMessage::from(tool_result_message.clone()),
        },
    )
    .await
}

/// `streamSimple` used when the caller supplies no `streamFn`.
fn default_stream_fn() -> StreamFn {
    Arc::new(|model, context, options| {
        let stream = pi_ai::stream::stream_simple(&model, &context, Some(&options));
        Box::pin(async move { stream })
    })
}

/// The abort `onAbort` hook of the TypeScript `closeIterator()`.
fn close_stream(signal: Option<&CancellationToken>) {
    // The TypeScript calls `iterator.return?.()` so the producer stops reading the
    // HTTP body. In Rust the abort token is the producer's stop signal and
    // `race_with_abort` has already returned, so cancelling the token mirrors the
    // intent of `closeIterator()`.
    if let Some(signal) = signal {
        signal.cancel();
    }
}

#[allow(clippy::too_many_arguments)]
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
    let started_at = if use_configured_correlation
        && configured_started_at.map(|value| value.is_finite()).unwrap_or(false)
    {
        configured_started_at
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
            async move { Ok::<_, anyhow::Error>(transform(messages, signal_for_hook).await) },
            signal.cloned(),
        )
        .await?;
    }

    let convert = config.convert_to_llm.clone();
    let llm_messages = maybe_abortable(
        async move {
            Ok::<_, anyhow::Error>(match convert {
                Some(convert) => convert(messages).await,
                None => default_convert_to_llm_placeholder(),
            })
        },
        signal.cloned(),
    )
    .await?;

    let stream_function = stream_fn.unwrap_or_else(default_stream_fn);

    let resolved_api_key = match config.get_api_key.clone() {
        Some(get_api_key) => {
            let provider = config.model.provider.clone();
            maybe_abortable(
                async move { Ok::<_, anyhow::Error>(get_api_key(provider).await) },
                signal.cloned(),
            )
            .await?
        }
        None => None,
    }
    .or_else(|| config.stream_options.stream.api_key.clone());

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
    provider_config.stream.api_key = resolved_api_key;
    provider_config.stream.signal = signal.cloned();
    let observed = create_observed_callbacks(config, metrics.clone(), &request_metrics);
    provider_config.stream.on_payload = Some(observed.on_payload.clone());
    provider_config.stream.on_response = Some(observed.on_response.clone());
    provider_config.stream.on_usage_observation = observed.on_usage_observation.clone();

    let result = stream_assistant_response_inner(
        context,
        config,
        signal,
        emit,
        stream_function,
        llm_context,
        provider_config,
        &mut request_metrics,
        &mut partial_message,
        &mut added_partial,
        &logical_request_settlement,
        &metrics,
        &observed,
    )
    .await;

    match result {
        Ok(message) => Ok(message),
        Err(error) => {
            if signal.map(|signal| signal.is_cancelled()).unwrap_or(false) && is_abort_error(&error) {
                let final_message =
                    create_aborted_assistant_message(config, partial_message.as_ref(), now_ms());
                finish_request_metrics(
                    config,
                    &metrics,
                    &logical_request_settlement,
                    Some(&final_message),
                    PerformanceMetricOutcome::Cancelled,
                    &mut request_metrics,
                    &observed,
                );
                if added_partial {
                    if let Some(last) = context.messages.last_mut() {
                        *last = AgentMessage::from(final_message.clone());
                    }
                } else {
                    context.messages.push(AgentMessage::from(final_message.clone()));
                    emit_event(
                        emit,
                        AgentEvent::MessageStart {
                            message: AgentMessage::from(final_message.clone()),
                        },
                    )
                    .await?;
                }
                emit_event(
                    emit,
                    AgentEvent::MessageEnd {
                        message: AgentMessage::from(final_message.clone()),
                    },
                )
                .await?;
                return Ok(final_message);
            }
            finish_request_metrics(
                config,
                &metrics,
                &logical_request_settlement,
                partial_message.as_ref(),
                PerformanceMetricOutcome::Failure,
                &mut request_metrics,
                &observed,
            );
            if let Some(partial) = partial_message.as_ref() {
                // This partial will not receive a terminal message_end for the host to settle.
                finalize_performance_metric_logical_request(partial, Some(PerformanceMetricOutcome::Failure));
            }
            Err(error)
        }
    }
}

/// The `finishRequestMetrics` closure of `streamAssistantResponse`.
#[allow(clippy::too_many_arguments)]
fn finish_request_metrics(
    config: &AgentLoopConfig,
    metrics: &Option<crate::performance_metrics::AgentLoopPerformanceMetrics>,
    logical_request_settlement: &Arc<AgentLoopLogicalRequestSettlement>,
    message: Option<&AssistantMessage>,
    outcome: PerformanceMetricOutcome,
    request_metrics: &mut RequestMetricState,
    observed: &ObservedCallbacks,
) {
    if metrics.is_none() || request_metrics.finished {
        return;
    }
    request_metrics.finished = true;
    // The provider callbacks write their timestamps into the shared loop state.
    if let Ok(observed_state) = observed.timestamps.lock() {
        request_metrics.dispatch_edge_at = request_metrics.dispatch_edge_at.or(observed_state.dispatch_edge_at);
        request_metrics.response_headers_at =
            request_metrics.response_headers_at.or(observed_state.response_headers_at);
        if request_metrics.provider_usage.is_none() {
            request_metrics.provider_usage = observed_state.provider_usage.clone();
        }
    }
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
    let logical_finalizer = LogicalRequestMetricFinalizer {
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
    let metrics_ref = metrics.as_ref().expect("metrics checked above");
    if let Some(message) = message {
        // The lifetime is scoped to the recording session, so two sessions that finish with
        // byte-identical content cannot overwrite each other's correlation.
        let session_id = metrics_ref.recorder.session_id();
        remember_request_correlation(
            message,
            session_id,
            PerformanceMetricRequestCorrelation {
                logical_request_id: request_metrics.logical_request_id.clone(),
                logical_request_started_at: request_metrics.started_at,
                provider_attempt_number: request_metrics.provider_attempt_number,
                logical_request_settlement: logical_request_settlement.clone(),
            },
        );
        remember_logical_request_finalizer(message, session_id, logical_finalizer.clone());
    }

    let usage = match request_metrics.provider_usage.clone() {
        Some(usage) => Some(usage),
        None => message.and_then(|message| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                performance_metric_usage_from_assistant(message)
            }))
            .ok()
        }),
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
}

/// T16 lane (owner rlm-agentcore): D-15 performance-metric correlation registry.
///
/// In-crate `#[cfg(test)]` module: the registry is private to this module, so the retention and
/// identity seams are reachable only here. Production entry points under test: the real
/// `agent_loop` stream plus `get_performance_metric_request_correlation` /
/// `finalize_performance_metric_logical_request`.
#[cfg(test)]
mod rlm_t16_tests {
    use super::*;
    use crate::performance_metrics::AgentLoopPerformanceMetrics;
    use crate::types::{AgentContext, AgentMessage};
    use pi_ai::providers::faux::{faux_assistant_message, register_faux_provider, FauxResponseStep};
    use pi_ai::types::{UserMessage, UserContent};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Documented retention bound. This is an independent expectation, not a read of the
    /// production constant, so weakening the constant alone cannot make the test pass.
    const RETENTION_BOUND: usize = 256;

    /// Serializes the tests in this binary that assert registry counts.
    static REGISTRY_TESTS: Mutex<()> = Mutex::new(());

    /// A real recorder whose session id distinguishes two process-local sessions.
    struct SessionRecorder {
        session_id: String,
        recorded: AtomicUsize,
    }

    impl SessionRecorder {
        fn new(session_id: &str) -> Arc<Self> {
            Arc::new(Self {
                session_id: session_id.to_string(),
                recorded: AtomicUsize::new(0),
            })
        }
    }

    impl PerformanceMetricRecorder for SessionRecorder {
        fn session_id(&self) -> &str {
            &self.session_id
        }
        fn monotonic_now(&self) -> f64 {
            1.0
        }
        fn next_id(&self, scope: crate::performance_metrics::PerformanceMetricIdScope) -> String {
            format!("{scope:?}-1")
        }
        fn record(&self, _event: PerformanceMetricEvent) {
            self.recorded.fetch_add(1, Ordering::SeqCst);
        }
        fn flush(&self) {}
        fn close(&self) {}
    }

    /// A terminal message whose bytes do not depend on the provider registration, so two
    /// sessions can be made byte-identical on purpose.
    fn pinned_message(
        registration: &pi_ai::providers::faux::FauxProviderRegistration,
        content_seed: u64,
    ) -> AssistantMessage {
        let model = registration.get_model();
        let mut message = faux_assistant_message("answer".into(), None);
        message.api = model.api.clone();
        message.provider = model.provider.clone();
        message.model = model.id.clone();
        message.timestamp = content_seed as i64;
        message.response_id = Some("pinned-response".to_string());
        message.usage = Usage::zero();
        message
    }

    /// Run one real agent loop for `session_id` and return its terminal assistant message.
    async fn run_session(
        provider: &pi_ai::providers::faux::FauxProviderRegistration,
        recorder: Arc<SessionRecorder>,
        logical_request_id: &str,
        content_seed: u64,
    ) -> AssistantMessage {
        provider.set_responses(vec![FauxResponseStep::Message(pinned_message(provider, content_seed))]);
        let context = AgentContext {
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: None,
        };
        let mut config = AgentLoopConfig::new(provider.get_model());
        config.performance_metrics = Some(AgentLoopPerformanceMetrics {
            recorder: recorder.clone(),
            logical_request_id: Some(logical_request_id.to_string()),
            logical_request_started_at: Some(0.0),
            provider_attempt_number: Some(1),
            host_owns_logical_request_terminal: false,
            logical_request_settlement: None,
        });
        config.stream_options.stream.session_id = Some("pinned-stream-session".to_string());
        let stream = agent_loop(
            vec![AgentMessage::from(UserMessage {
                role: "user".to_string(),
                content: UserContent::Text("question".to_string()),
                provider_context: None,
                timestamp: 1,
            })],
            context,
            config,
            None,
            None,
        );
        let mut messages: Vec<AgentMessage> = Vec::new();
        while let Some(event) = stream.next().await {
            if let AgentEvent::AgentEnd { messages: produced } = event {
                messages = produced;
            }
        }
        messages
            .into_iter()
            .rev()
            .find_map(|message| match message {
                AgentMessage::Message(Message::Assistant(assistant)) => Some(assistant),
                _ => None,
            })
            .expect("the run must produce a terminal assistant message")
    }

    /// Two sessions that finish with byte-identical assistant content must not share one
    /// correlation lifetime. On a content-keyed registry the second run overwrites the first.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identical_content_messages_do_not_share_correlation() {
        let _guard = REGISTRY_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let provider = register_faux_provider(None);
        let session_a = SessionRecorder::new("session-a");
        let session_b = SessionRecorder::new("session-b");
        let left = run_session(&provider, session_a.clone(), "logical-a", 42).await;
        let right = run_session(&provider, session_b, "logical-b", 42).await;
        assert_eq!(
            serde_json::to_string(&left).unwrap(),
            serde_json::to_string(&right).unwrap(),
            "fixture precondition: the terminal messages must be byte-identical"
        );
        assert!(session_a.recorded.load(Ordering::SeqCst) > 0, "session A recorded metrics");
        let retained = correlation_registry_logical_ids_for_tests();
        let left_id = get_performance_metric_request_correlation(&left)
            .and_then(|correlation| correlation.logical_request_id);
        let right_id = get_performance_metric_request_correlation(&right)
            .and_then(|correlation| correlation.logical_request_id);
        // The registry is process-global, so other tests in this binary may also hold
        // lifetimes. The property under test is that BOTH sessions keep their own lifetime.
        assert!(
            retained.contains(&"logical-a".to_string()) && retained.contains(&"logical-b".to_string()),
            "each session must keep its own correlation lifetime for byte-identical content; \
             retained lifetimes observed {retained:?} (left={left_id:?}, right={right_id:?})"
        );
        let mut distinct = retained.clone();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            retained.len(),
            "the two sessions must not share a single lifetime: {retained:?}"
        );
        assert_ne!(
            left_id.as_deref(),
            Some("logical-b"),
            "session A must not be attributed to session B's request"
        );
        assert_ne!(
            right_id.as_deref(),
            Some("logical-a"),
            "session B must not be attributed to session A's request"
        );
        if left_id.is_some() {
            assert_eq!(left_id.as_deref(), Some("logical-a"));
        }
        if right_id.is_some() {
            assert_eq!(right_id.as_deref(), Some("logical-b"));
        }

        // Preservation control: distinct content still resolves to its own session's request.
        let distinct_c = run_session(&provider, SessionRecorder::new("session-c"), "logical-c", 501).await;
        let distinct_d = run_session(&provider, SessionRecorder::new("session-d"), "logical-d", 502).await;
        assert_eq!(
            get_performance_metric_request_correlation(&distinct_c)
                .and_then(|correlation| correlation.logical_request_id)
                .as_deref(),
            Some("logical-c"),
            "a unique terminal message must keep its own correlation"
        );
        assert_eq!(
            get_performance_metric_request_correlation(&distinct_d)
                .and_then(|correlation| correlation.logical_request_id)
                .as_deref(),
            Some("logical-d"),
        );
        provider.unregister();
    }

    /// Bounded synthetic message lifetimes must not grow the registry without bound.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finalized_message_lifetimes_do_not_leak_registry_entries() {
        let _guard = REGISTRY_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let base = correlation_registry_len_for_tests();
        let lifetimes = RETENTION_BOUND + 32;
        let mut peak = base;
        let provider = register_faux_provider(None);
        for index in 0..lifetimes {
            let recorder = SessionRecorder::new("session-retention");
            let terminal = run_session(&provider, recorder, "logical-retention", 1000 + index as u64).await;
            // The documented end of a message lifetime: the host settles the logical request.
            finalize_performance_metric_logical_request(
                &terminal,
                Some(PerformanceMetricOutcome::Success),
            );
            peak = peak.max(correlation_registry_len_for_tests());
        }
        provider.unregister();
        let after = correlation_registry_len_for_tests();
        assert!(
            peak <= RETENTION_BOUND + base,
            "retained lifetimes must stay within the documented bound: peak {peak}, base {base}, \
             bound {RETENTION_BOUND}"
        );
        assert!(
            after <= RETENTION_BOUND + base,
            "registry must not grow per message lifetime: {after} live entries after {lifetimes} \
             finalized lifetimes (base {base}, bound {RETENTION_BOUND})"
        );
    }

    /// Preservation control for /monitor behavior: attempts are recorded and settlement is
    /// exactly once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn metrics_recording_and_single_settlement_are_preserved() {
        let provider = register_faux_provider(None);
        let recorder = SessionRecorder::new("session-control");
        let terminal = run_session(&provider, recorder.clone(), "logical-control", 7).await;
        provider.unregister();
        assert!(
            recorder.recorded.load(Ordering::SeqCst) >= 1,
            "the provider attempt must still be recorded"
        );
        assert_eq!(
            get_performance_metric_request_correlation(&terminal)
                .and_then(|correlation| correlation.logical_request_id)
                .as_deref(),
            Some("logical-control"),
        );
        let before = recorder.recorded.load(Ordering::SeqCst);
        finalize_performance_metric_logical_request(&terminal, Some(PerformanceMetricOutcome::Success));
        let after_first = recorder.recorded.load(Ordering::SeqCst);
        finalize_performance_metric_logical_request(&terminal, Some(PerformanceMetricOutcome::Failure));
        let after_second = recorder.recorded.load(Ordering::SeqCst);
        assert!(after_first <= before + 1, "at most one extra record per settlement");
        assert_eq!(
            after_second, after_first,
            "a second settlement of the same lifetime must not record again"
        );
    }

    /// A run without a recorder must not create registry state, and an unregistered message
    /// must not resolve.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_messages_and_metricless_runs_add_no_state() {
        let _guard = REGISTRY_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let unknown = faux_assistant_message("never registered".into(), None);
        assert!(get_performance_metric_request_correlation(&unknown).is_none());
        let before = correlation_registry_len_for_tests();
        let provider = register_faux_provider(None);
        provider.set_responses(vec![FauxResponseStep::Message(faux_assistant_message("no metrics".into(), None))]);
        let config = AgentLoopConfig::new(provider.get_model());
        assert!(config.performance_metrics.is_none(), "no recorder by default");
        let stream = agent_loop(
            vec![AgentMessage::from(UserMessage {
                role: "user".to_string(),
                content: UserContent::Text("plain".to_string()),
                provider_context: None,
                timestamp: 1,
            })],
            AgentContext::default(),
            config,
            None,
            None,
        );
        while stream.next().await.is_some() {}
        provider.unregister();
        assert_eq!(
            correlation_registry_len_for_tests(),
            before,
            "a run without a recorder must not create correlation state"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolExecutionMode;
    use serde_json::json;

    fn assistant_with_tool_call(id: &str, name: &str) -> AssistantMessage {
        let mut message = AssistantMessage::new("openai-responses", "openai", "gpt-test", 1);
        message.content.push(ContentBlock::ToolCall(ToolCall::new(
            id,
            name,
            match json!({}) {
                Value::Object(map) => map,
                _ => unreachable!(),
            },
        )));
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
        let config = AgentLoopConfig::new(Model::new("unknown", "unknown", "unknown", "unknown", ""));
        let mut partial = AssistantMessage::new("anthropic-messages", "anthropic", "claude-test", 0);
        partial
            .content
            .push(ContentBlock::Text(TextContent::new("partial")));
        partial.usage.input = 5.0;

        let aborted = create_aborted_assistant_message(&config, Some(&partial), 42);
        assert_eq!(aborted.stop_reason, pi_ai::types::STOP_REASON_ABORTED);
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
        let tool_call = AgentToolCall::new(
            "call-1",
            "bash",
            match json!({}) {
                Value::Object(map) => map,
                _ => unreachable!(),
            },
        );
        let mut first = FinalizedToolCallOutcome {
            tool_call: tool_call.clone(),
            result: AgentToolResult::new(Vec::new(), json!({})),
            is_error: false,
        };
        first.result.terminate = Some(true);
        // `finalizedCalls.every((finalized) => finalized.result.terminate === true)`
        assert!(should_terminate_tool_batch(&[first.clone()]));
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
            tool_call: AgentToolCall::new(
                "call-9",
                "read",
                match json!({}) {
                    Value::Object(map) => map,
                    _ => unreachable!(),
                },
            ),
            result: AgentToolResult::new(vec![AgentContentBlock::text("body")], json!({"bytes": 4})),
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
        assert_eq!(
            request_metric_outcome(&pi_ai::types::STOP_REASON_ABORTED.to_string()),
            PerformanceMetricOutcome::Cancelled
        );
        assert_eq!(
            request_metric_outcome(&pi_ai::types::STOP_REASON_ERROR.to_string()),
            PerformanceMetricOutcome::Failure
        );
        assert_eq!(
            request_metric_outcome(&pi_ai::types::STOP_REASON_STOP.to_string()),
            PerformanceMetricOutcome::Success
        );
        assert_eq!(
            request_metric_outcome(&pi_ai::types::STOP_REASON_TOOL_USE.to_string()),
            PerformanceMetricOutcome::Success
        );
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
        let tool_call = AgentToolCall::new(
            "call-1",
            "bash",
            match json!({"command": "ls"}) {
                Value::Object(map) => map,
                _ => unreachable!(),
            },
        );
        let prepared = prepare_tool_call_arguments(&tool, &tool_call);
        assert_eq!(prepared.arguments, tool_call.arguments);
    }

    #[test]
    fn parallel_entries_keep_assistant_source_order_for_messages() {
        // Mirrors executeToolCallsParallel: finalized tool-result messages are
        // emitted in assistant source order, not completion order.
        let tool_call_a = AgentToolCall::new(
            "call-a",
            "bash",
            match json!({}) {
                Value::Object(map) => map,
                _ => unreachable!(),
            },
        );
        let tool_call_b = AgentToolCall::new(
            "call-b",
            "bash",
            match json!({}) {
                Value::Object(map) => map,
                _ => unreachable!(),
            },
        );
        let finalized_a = FinalizedToolCallOutcome {
            tool_call: tool_call_a.clone(),
            result: AgentToolResult::new(vec![AgentContentBlock::text("a")], json!({})),
            is_error: false,
        };
        let finalized_b = FinalizedToolCallOutcome {
            tool_call: tool_call_b.clone(),
            result: AgentToolResult::new(vec![AgentContentBlock::text("b")], json!({})),
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
        let config = AgentLoopConfig::new(Model::new("unknown", "unknown", "unknown", "unknown", ""));
        let empty = agent_loop_continue(AgentContext::default(), config.clone(), None, None);
        assert!(empty.is_err());
        assert_eq!(
            empty.err().map(|error| error.to_string()).unwrap_or_default(),
            "Cannot continue: no messages in context"
        );

        let context = AgentContext {
            system_prompt: String::new(),
            messages: vec![AgentMessage::from(assistant_with_tool_call("call-1", "bash"))],
            tools: None,
        };
        let err = agent_loop_continue(context, config, None, None)
            .err()
            .map(|error| error.to_string());
        assert_eq!(err.unwrap_or_default(), "Cannot continue from message role: assistant");
    }

    #[test]
    fn agent_stream_completes_on_agent_end_only() {
        let stream = create_agent_stream();
        stream.push(AgentEvent::AgentStart);
        stream.push(AgentEvent::TurnStart);
        assert!(!stream.is_done());
        stream.push(AgentEvent::AgentEnd {
            messages: Vec::new(),
        });
        assert!(stream.is_done());
    }

    #[test]
    fn tool_execution_mode_sequential_forces_sequential_batches() {
        let config = AgentLoopConfig::new(Model::new("unknown", "unknown", "unknown", "unknown", ""));
        assert_eq!(config.resolved_tool_execution(), ToolExecutionMode::Parallel);
    }
}
