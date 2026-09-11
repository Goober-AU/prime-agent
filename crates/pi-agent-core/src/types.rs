//! Port of packages/agent/src/types.ts
//!
//! The TypeScript `CustomAgentMessages` interface is augmented by
//! packages/coding-agent/src/core/messages.ts via declaration merging, so
//! `AgentMessage = Message | CustomAgentMessages[keyof CustomAgentMessages]`.
//! In Rust there is no declaration merging, so [`AgentMessage`] is an enum with
//! the same member set: the three pi-ai message shapes plus the four custom
//! message kinds that the coding agent registers (`bashExecution`, `custom`,
//! `branchSummary`, `compactionSummary`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::types::{
    AssistantMessage, AssistantMessageEvent, ImageContent, Message, Model, ServiceTier,
    SimpleStreamOptions, TextContent, ToolResultMessage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::performance_metrics::AgentLoopPerformanceMetrics;
use self::pi_ai_shim as pi_ai;

/// Type of the streaming entry point used by the agent loop (TypeScript `StreamFn`).
///
/// Contract:
/// - Must not throw or return a rejected promise for request/model/runtime failures.
/// - Must return an `AssistantMessageEventStream`.
/// - Failures must be encoded in the returned stream via protocol events and a
///   final `AssistantMessage` with `stopReason` "error" or "aborted" and `errorMessage`.
pub type StreamFn = Arc<
    dyn Fn(
            Model,
            pi_ai::types::Context,
            SimpleStreamOptions,
        ) -> futures::future::BoxFuture<'static, pi_ai::utils::event_stream::AssistantMessageEventStream>
        + Send
        + Sync,
>;

/// Configuration for how tool calls from a single assistant message are executed.
///
/// - `Sequential`: each tool call is prepared, executed, and finalized before the next one starts.
/// - `Parallel`: tool calls are prepared sequentially, then allowed tools execute concurrently.
///   `tool_execution_end` is emitted in tool completion order after each tool is finalized,
///   while tool-result message artifacts are emitted later in assistant source order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    Sequential,
    Parallel,
}

/// A tool-call content block emitted by an assistant message.
pub type AgentToolCall = pi_ai::types::ToolCall;

/// Result returned from `beforeToolCall`.
///
/// Returning `{ block: true }` prevents the tool from executing. The loop emits an
/// error tool result instead. `reason` becomes the text shown in that error result.
/// If omitted, a default blocked message is used.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BeforeToolCallResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Partial override returned from `afterToolCall`.
///
/// Merge semantics are field-by-field:
/// - `content`: if provided, replaces the tool result content array in full
/// - `details`: if provided, replaces the tool result details value in full
/// - `isError`: if provided, replaces the tool result error flag
/// - `terminate`: if provided, replaces the early-termination hint
///
/// Omitted fields keep the original executed tool result values.
/// There is no deep merge for `content` or `details`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AfterToolCallResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// Hint that the agent should stop after the current tool batch.
    /// Early termination only happens when every finalized tool result in the batch sets this to true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// Context passed to `beforeToolCall` after arguments are validated.
#[derive(Clone)]
pub struct BeforeToolCallContext {
    /// Assistant message that requested the call.
    pub assistant_message: AssistantMessage,
    /// Raw tool-call block from `assistantMessage.content`.
    pub tool_call: AgentToolCall,
    /// Validated arguments for the target tool schema.
    pub args: Value,
    /// Agent context when this call is prepared.
    pub context: AgentContext,
}

/// Context passed to `afterToolCall`.
#[derive(Clone)]
pub struct AfterToolCallContext {
    /// Assistant message that requested the call.
    pub assistant_message: AssistantMessage,
    /// Raw tool-call block from `assistantMessage.content`.
    pub tool_call: AgentToolCall,
    /// Validated arguments for the target tool schema.
    pub args: Value,
    /// Executed result before any `afterToolCall` overrides.
    pub result: AgentToolResult,
    /// Whether the executed result is currently treated as an error.
    pub is_error: bool,
    /// Agent context when this call is finalized.
    pub context: AgentContext,
}

/// Context passed to `shouldStopAfterTurn` and `getContinuationMessages`.
#[derive(Clone)]
pub struct ShouldStopAfterTurnContext {
    /// Assistant message that completed the turn.
    pub message: AssistantMessage,
    /// Tool-result messages included in the preceding `turn_end` event.
    pub tool_results: Vec<ToolResultMessage>,
    /// Context after appending the turn's assistant message and tool results.
    pub context: AgentContext,
    /// Messages returned by this invocation; prompts include initial prompts,
    /// continuations exclude prior context.
    pub new_messages: Vec<AgentMessage>,
}

pub type GetContinuationMessagesContext = ShouldStopAfterTurnContext;

/// Thinking/reasoning level for models that support it.
///
/// Note: "xhigh" and "max" are only supported by selected model families. Use the
/// model thinking-level metadata from pi-ai to detect support for a concrete model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Default for ThinkingLevel {
    fn default() -> Self {
        ThinkingLevel::Off
    }
}

impl ThinkingLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "xhigh",
            ThinkingLevel::Max => "max",
        }
    }
}

/// Final or partial result produced by a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToolResult {
    /// Text or image content returned to the model.
    pub content: Vec<ContentBlock>,
    /// Structured details for logs or UI rendering.
    pub details: Value,
    /// Hint that the agent should stop after the current tool batch.
    /// Early termination only happens when every finalized tool result in the batch sets this to true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

impl AgentToolResult {
    pub fn new(content: Vec<ContentBlock>, details: Value) -> Self {
        Self {
            content,
            details,
            terminate: None,
        }
    }
}

/// Text or image content, the only two block kinds a tool result can carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentBlock {
    Text(TextContent),
    Image(ImageContent),
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text(TextContent {
            r#type: "text".to_string(),
            text: text.into(),
            text_signature: None,
        })
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            ContentBlock::Image(_) => None,
        }
    }
}

/// Callback used by tools to publish partial execution updates.
pub type AgentToolUpdateCallback = Arc<dyn Fn(AgentToolResult) + Send + Sync>;

/// Tool definition used by the agent runtime.
#[derive(Clone)]
pub struct AgentTool {
    pub name: String,
    pub description: String,
    /// TypeBox/JSON schema for the tool parameters.
    pub parameters: Value,
    /// Human-readable label for UI display.
    pub label: String,
    /// Optional compatibility shim for raw tool-call arguments before schema validation.
    /// Must return an object that matches `TParameters`.
    pub prepare_arguments: Option<Arc<dyn Fn(Value) -> Value + Send + Sync>>,
    /// Execute the tool call. Return an error instead of encoding errors in `content`.
    pub execute: Arc<
        dyn Fn(
                String,
                Value,
                Option<tokio_util::sync::CancellationToken>,
                Option<AgentToolUpdateCallback>,
            ) -> futures::future::BoxFuture<'static, Result<AgentToolResult, anyhow::Error>>
            + Send
            + Sync,
    >,
    /// Per-tool execution mode override.
    pub execution_mode: Option<ToolExecutionMode>,
}

impl std::fmt::Debug for AgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("label", &self.label)
            .field("execution_mode", &self.execution_mode)
            .finish_non_exhaustive()
    }
}

/// Context snapshot passed to the low-level agent loop and tool hooks.
#[derive(Debug, Clone, Default)]
pub struct AgentContext {
    /// System prompt included with the request.
    pub system_prompt: String,
    /// Transcript visible to the model.
    pub messages: Vec<AgentMessage>,
    /// Tools available for this run.
    pub tools: Option<Vec<AgentTool>>,
}

/// Extensible custom message kinds registered by the coding agent
/// (TypeScript declaration merging of `CustomAgentMessages`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum CustomAgentMessage {
    /// Message type for bash executions via the ! command.
    #[serde(rename = "bashExecution")]
    BashExecution {
        command: String,
        output: String,
        #[serde(rename = "exitCode")]
        exit_code: Option<i64>,
        cancelled: bool,
        truncated: bool,
        #[serde(rename = "fullOutputPath", default, skip_serializing_if = "Option::is_none")]
        full_output_path: Option<String>,
        timestamp: i64,
        /// If true, this message is excluded from LLM context (!! prefix)
        #[serde(rename = "excludeFromContext", default, skip_serializing_if = "Option::is_none")]
        exclude_from_context: Option<bool>,
    },
    /// Message type for extension-injected messages via sendMessage().
    #[serde(rename = "custom")]
    Custom {
        #[serde(rename = "customType")]
        custom_type: String,
        content: CustomMessageContent,
        display: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        timestamp: i64,
    },
    #[serde(rename = "branchSummary")]
    BranchSummary {
        summary: String,
        #[serde(rename = "fromId")]
        from_id: String,
        timestamp: i64,
    },
    #[serde(rename = "compactionSummary")]
    CompactionSummary {
        summary: String,
        /// Complete opaque provider window, used instead of the display summary.
        #[serde(rename = "providerContext", default, skip_serializing_if = "Option::is_none")]
        provider_context: Option<pi_ai::compaction::ProviderCompactionCheckpoint>,
        #[serde(rename = "tokensBefore")]
        tokens_before: f64,
        /// Number of retained messages that precede this summary in transcript presentation.
        #[serde(rename = "retainedMessageCount", default, skip_serializing_if = "Option::is_none")]
        retained_message_count: Option<f64>,
        /// User instructions that guided the summary (from `/compact <instructions>`)
        #[serde(rename = "customInstructions", default, skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
        /// Harness digest snapshot rendered before the summary in LLM context.
        #[serde(rename = "harnessDigest", default, skip_serializing_if = "Option::is_none")]
        harness_digest: Option<String>,
        timestamp: i64,
    },
}

impl CustomAgentMessage {
    pub fn role(&self) -> &'static str {
        match self {
            CustomAgentMessage::BashExecution { .. } => "bashExecution",
            CustomAgentMessage::Custom { .. } => "custom",
            CustomAgentMessage::BranchSummary { .. } => "branchSummary",
            CustomAgentMessage::CompactionSummary { .. } => "compactionSummary",
        }
    }
}

/// `content: string | (TextContent | ImageContent)[]` for custom messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CustomMessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// Union of the LLM-visible pi-ai messages and the coding agent custom messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentMessage {
    Message(Message),
    Custom(CustomAgentMessage),
}

impl AgentMessage {
    /// Mirrors `message.role` in TypeScript. `ToolResult` reads as "toolResult"
    /// and `Custom` reads as "custom", matching the declared discriminant.
    pub fn role(&self) -> &str {
        match self {
            AgentMessage::Message(message) => message.role(),
            AgentMessage::Custom(custom) => custom.role(),
        }
    }

    pub fn as_message(&self) -> Option<&Message> {
        match self {
            AgentMessage::Message(message) => Some(message),
            AgentMessage::Custom(_) => None,
        }
    }
}

impl From<Message> for AgentMessage {
    fn from(value: Message) -> Self {
        AgentMessage::Message(value)
    }
}

impl From<AssistantMessage> for AgentMessage {
    fn from(value: AssistantMessage) -> Self {
        AgentMessage::Message(Message::Assistant(value))
    }
}

impl From<pi_ai::types::UserMessage> for AgentMessage {
    fn from(value: pi_ai::types::UserMessage) -> Self {
        AgentMessage::Message(Message::User(value))
    }
}

impl From<ToolResultMessage> for AgentMessage {
    fn from(value: ToolResultMessage) -> Self {
        AgentMessage::Message(Message::ToolResult(value))
    }
}

/// Public agent state.
///
/// `tools` and `messages` use accessor methods so implementations can copy
/// assigned arrays before storing them (`set_tools` / `set_messages`).
#[derive(Clone)]
pub struct AgentState {
    /// System prompt sent with each model request.
    pub system_prompt: String,
    /// Model used for future turns.
    pub model: Model,
    /// Requested reasoning level for future turns.
    pub thinking_level: ThinkingLevel,
    /// Requested provider service tier for future turns.
    pub service_tier: ServiceTier,
    /// True while processing a prompt or continuation, including awaited `agent_end` listeners.
    pub is_streaming: bool,
    /// Partial assistant message for the active streamed response, if any.
    pub streaming_message: Option<AgentMessage>,
    /// Tool-call IDs currently executing.
    pub pending_tool_calls: BTreeSet<String>,
    /// Error from the most recent failed or aborted assistant turn, if any.
    pub error_message: Option<String>,
}

/// Events emitted by the Agent for UI updates.
///
/// `agent_end` is the last event emitted for a run, but awaited `Agent.subscribe()`
/// listeners for that event are still part of run settlement. The agent becomes
/// idle only after those listeners finish.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Starts and ends one agent run; `agent_end` carries all messages produced by that run.
    AgentStart,
    AgentEnd {
        messages: Vec<AgentMessage>,
    },
    /// One assistant response and its resulting tool calls.
    TurnStart,
    TurnEnd {
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    /// Lifecycle events for user, assistant, and tool-result messages.
    MessageStart {
        message: AgentMessage,
    },
    /// Only emitted for assistant messages during streaming.
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: AssistantMessageEvent,
    },
    MessageEnd {
        message: AgentMessage,
    },
    /// Tool execution events; parallel calls may end in completion rather than source order.
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        partial_result: AgentToolResult,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: AgentToolResult,
        is_error: bool,
    },
}

impl AgentEvent {
    /// `event.type` in the TypeScript union.
    pub fn type_name(&self) -> &'static str {
        match self {
            AgentEvent::AgentStart => "agent_start",
            AgentEvent::AgentEnd { .. } => "agent_end",
            AgentEvent::TurnStart => "turn_start",
            AgentEvent::TurnEnd { .. } => "turn_end",
            AgentEvent::MessageStart { .. } => "message_start",
            AgentEvent::MessageUpdate { .. } => "message_update",
            AgentEvent::MessageEnd { .. } => "message_end",
            AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
            AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
            AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
        }
    }

    /// JSON projection that uses the TypeScript event names for `type` and the
    /// original camelCase field names. `assistant_message_event` and the tool
    /// result payloads are carried through as their own serialized shapes.
    pub fn to_json(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("type".to_string(), Value::String(self.type_name().to_string()));
        match self {
            AgentEvent::AgentStart | AgentEvent::TurnStart => {}
            AgentEvent::AgentEnd { messages } => {
                object.insert("messages".to_string(), serde_json::to_value(messages).unwrap_or(Value::Null));
            }
            AgentEvent::TurnEnd { message, tool_results } => {
                object.insert("message".to_string(), serde_json::to_value(message).unwrap_or(Value::Null));
                object.insert(
                    "toolResults".to_string(),
                    serde_json::to_value(tool_results).unwrap_or(Value::Null),
                );
            }
            AgentEvent::MessageStart { message } | AgentEvent::MessageEnd { message } => {
                object.insert("message".to_string(), serde_json::to_value(message).unwrap_or(Value::Null));
            }
            AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } => {
                object.insert("message".to_string(), serde_json::to_value(message).unwrap_or(Value::Null));
                object.insert(
                    "assistantMessageEvent".to_string(),
                    serde_json::to_value(assistant_message_event).unwrap_or(Value::Null),
                );
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                object.insert("toolCallId".to_string(), Value::String(tool_call_id.clone()));
                object.insert("toolName".to_string(), Value::String(tool_name.clone()));
                object.insert("args".to_string(), args.clone());
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
            } => {
                object.insert("toolCallId".to_string(), Value::String(tool_call_id.clone()));
                object.insert("toolName".to_string(), Value::String(tool_name.clone()));
                object.insert("args".to_string(), args.clone());
                object.insert(
                    "partialResult".to_string(),
                    serde_json::to_value(partial_result).unwrap_or(Value::Null),
                );
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => {
                object.insert("toolCallId".to_string(), Value::String(tool_call_id.clone()));
                object.insert("toolName".to_string(), Value::String(tool_name.clone()));
                object.insert("result".to_string(), serde_json::to_value(result).unwrap_or(Value::Null));
                object.insert("isError".to_string(), Value::Bool(*is_error));
            }
        }
        Value::Object(object)
    }
}

/// Hook callbacks used by the low-level agent loop. All callbacks are
/// `Send + Sync` because the loop is driven from async tasks.
#[derive(Clone, Default)]
pub struct AgentLoopConfig {
    /// Base simple stream options (temperature, maxTokens, headers, metadata, ...).
    pub stream_options: SimpleStreamOptions,
    pub model: Model,
    /// Optional disposable local measurements. `None` preserves the zero-overhead default.
    pub performance_metrics: Option<AgentLoopPerformanceMetrics>,
    /// Converts `AgentMessage[]` to LLM-compatible `Message[]` before each LLM call.
    ///
    /// Contract: must not throw or reject. Return a safe fallback value instead.
    pub convert_to_llm: Option<Arc<dyn Fn(Vec<AgentMessage>) -> Vec<Message> + Send + Sync>>,
    /// Optional transform applied to the context before `convertToLlm`.
    pub transform_context: Option<
        Arc<
            dyn Fn(Vec<AgentMessage>, Option<tokio_util::sync::CancellationToken>) -> Vec<AgentMessage>
                + Send
                + Sync,
        >,
    >,
    /// Resolves the system prompt immediately before each LLM call.
    pub get_system_prompt: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    /// Resolves an API key dynamically for each LLM call.
    ///
    /// Contract: must not throw or reject. Return `None` when no key is available.
    pub get_api_key: Option<Arc<dyn Fn(String) -> Option<String> + Send + Sync>>,
    /// Called after each turn fully completes and `turn_end` has been emitted.
    pub should_stop_after_turn:
        Option<Arc<dyn Fn(ShouldStopAfterTurnContext) -> bool + Send + Sync>>,
    /// Called synchronously after a completed turn and before polling work for another turn.
    pub should_stop_before_turn: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// Returns steering messages to inject into the conversation mid-run.
    pub get_steering_messages: Option<Arc<dyn Fn() -> Vec<AgentMessage> + Send + Sync>>,
    /// Returns follow-up messages to process after the agent would otherwise stop.
    pub get_follow_up_messages: Option<Arc<dyn Fn() -> Vec<AgentMessage> + Send + Sync>>,
    /// Returns continuation messages when the agent would otherwise stop.
    pub get_continuation_messages: Option<
        Arc<
            dyn Fn(
                    GetContinuationMessagesContext,
                    Option<tokio_util::sync::CancellationToken>,
                ) -> Vec<AgentMessage>
                + Send
                + Sync,
        >,
    >,
    /// Tool execution mode. Defaults to `Parallel`.
    pub tool_execution: Option<ToolExecutionMode>,
    /// Called before a tool is executed, after arguments have been validated.
    pub before_tool_call: Option<
        Arc<
            dyn Fn(
                    BeforeToolCallContext,
                    Option<tokio_util::sync::CancellationToken>,
                ) -> Option<BeforeToolCallResult>
                + Send
                + Sync,
        >,
    >,
    /// Called after a tool finishes executing, before `tool_execution_end` and
    /// tool-result message events are emitted.
    pub after_tool_call: Option<
        Arc<
            dyn Fn(
                    AfterToolCallContext,
                    Option<tokio_util::sync::CancellationToken>,
                ) -> Option<AfterToolCallResult>
                + Send
                + Sync,
        >,
    >,
}

impl std::fmt::Debug for AgentLoopConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoopConfig")
            .field("model", &self.model.id)
            .field("tool_execution", &self.tool_execution)
            .finish_non_exhaustive()
    }
}

impl AgentLoopConfig {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            ..Default::default()
        }
    }

    /// Provider options handed to the stream function: the base simple stream
    /// options plus `apiKey`, `signal`, and the observed callbacks.
    pub fn provider_options(&self) -> SimpleStreamOptions {
        self.stream_options.clone()
    }

    pub fn resolved_tool_execution(&self) -> ToolExecutionMode {
        self.tool_execution.unwrap_or(ToolExecutionMode::Parallel)
    }
}

/// `Record<string, string>` helper used by provider options.
pub type StringRecord = BTreeMap<String, String>;

/// `Record<string, unknown>` helper used by provider options.
pub type ValueRecord = IndexMap<String, Value>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_message_role_matches_typescript_discriminants() {
        let user = AgentMessage::from(pi_ai::types::UserMessage {
            role: "user".to_string(),
            content: pi_ai::types::UserContent::Text("hi".to_string()),
            provider_context: None,
            timestamp: 1,
        });
        assert_eq!(user.role(), "user");

        let tool_result = AgentMessage::from(ToolResultMessage {
            role: "toolResult".to_string(),
            tool_call_id: "call-1".to_string(),
            tool_name: "bash".to_string(),
            content: Vec::new(),
            details: None,
            is_error: false,
            timestamp: 2,
        });
        assert_eq!(tool_result.role(), "toolResult");

        let custom = AgentMessage::Custom(CustomAgentMessage::Custom {
            custom_type: "notification".to_string(),
            content: CustomMessageContent::Text("body".to_string()),
            display: true,
            details: None,
            timestamp: 3,
        });
        assert_eq!(custom.role(), "custom");
    }

    #[test]
    fn thinking_level_serializes_lowercase() {
        assert_eq!(serde_json::to_value(ThinkingLevel::Xhigh).unwrap(), Value::String("xhigh".into()));
        assert_eq!(serde_json::from_value::<ThinkingLevel>(Value::String("off".into())).unwrap(), ThinkingLevel::Off);
    }

    #[test]
    fn tool_execution_mode_defaults_to_parallel() {
        let config = AgentLoopConfig::new(Model::unknown());
        assert_eq!(config.resolved_tool_execution(), ToolExecutionMode::Parallel);
        assert_eq!(serde_json::to_value(ToolExecutionMode::Sequential).unwrap(), Value::String("sequential".into()));
    }

    #[test]
    fn agent_event_type_names_match_typescript() {
        assert_eq!(AgentEvent::AgentStart.type_name(), "agent_start");
        assert_eq!(AgentEvent::TurnStart.type_name(), "turn_start");
        assert_eq!(
            AgentEvent::ToolExecutionEnd {
                tool_call_id: "c".into(),
                tool_name: "t".into(),
                result: AgentToolResult::new(vec![], Value::Object(Default::default())),
                is_error: true,
            }
            .type_name(),
            "tool_execution_end"
        );
    }
}


// ---------------------------------------------------------------------------
// TEMPORARY in-crate stand-in for the pi-ai types this slice needs.
//
// packages/agent imports these from @earendil-works/pi-ai. The `pi-ai` crate in
// this workspace is still empty, so the module layout of the real crate is
// mirrored here and aliased as `pi_ai` at the top of each file in this crate:
//
//     use self::pi_ai_shim as pi_ai;
//
// When pi-ai lands, delete this module and the alias lines. Nothing else in the
// port has to change, because every path below matches the mapped pi-ai module
// (`pi-ai/src/types.rs`, `pi-ai/src/utils/event_stream.rs`, ...).
// ---------------------------------------------------------------------------
pub mod pi_ai_shim {
    pub mod types {
        use std::collections::BTreeMap;
        use std::sync::Arc;

        use serde::{Deserialize, Serialize};
        use serde_json::Value;

        pub type Api = String;
        pub type Provider = String;

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "lowercase")]
        pub enum StopReason {
            Stop,
            Length,
            #[serde(rename = "toolUse")]
            ToolUse,
            Error,
            Aborted,
        }

        impl StopReason {
            pub fn as_str(self) -> &'static str {
                match self {
                    StopReason::Stop => "stop",
                    StopReason::Length => "length",
                    StopReason::ToolUse => "toolUse",
                    StopReason::Error => "error",
                    StopReason::Aborted => "aborted",
                }
            }
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "lowercase")]
        pub enum Transport {
            Sse,
            Websocket,
            #[serde(rename = "websocket-cached")]
            WebsocketCached,
            Auto,
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "lowercase")]
        pub enum CacheRetention {
            None,
            Short,
            Long,
        }

        /// `ServiceTier = "auto" | "default" | "flex" | "scale" | "priority" | null`
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "lowercase")]
        pub enum ServiceTier {
            Auto,
            Default,
            Flex,
            Scale,
            Priority,
            Null,
        }

        impl Default for ServiceTier {
            fn default() -> Self {
                ServiceTier::Null
            }
        }

        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct ThinkingBudgets {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub minimal: Option<f64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub low: Option<f64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub medium: Option<f64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub high: Option<f64>,
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct TextContent {
            pub r#type: String,
            pub text: String,
            #[serde(rename = "textSignature", default, skip_serializing_if = "Option::is_none")]
            pub text_signature: Option<String>,
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct ThinkingContent {
            pub r#type: String,
            pub thinking: String,
            #[serde(rename = "thinkingSignature", default, skip_serializing_if = "Option::is_none")]
            pub thinking_signature: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub redacted: Option<bool>,
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct ImageContent {
            pub r#type: String,
            pub data: String,
            #[serde(rename = "mimeType")]
            pub mime_type: String,
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct ToolCall {
            pub r#type: String,
            pub id: String,
            pub name: String,
            pub arguments: Value,
            #[serde(rename = "thoughtSignature", default, skip_serializing_if = "Option::is_none")]
            pub thought_signature: Option<String>,
        }

        impl ToolCall {
            pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
                Self {
                    r#type: "toolCall".to_string(),
                    id: id.into(),
                    name: name.into(),
                    arguments,
                    thought_signature: None,
                }
            }
        }

        /// `AssistantMessage["content"][number]`
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "type")]
        pub enum AssistantContentPart {
            #[serde(rename = "text")]
            Text {
                text: String,
                #[serde(rename = "textSignature", default, skip_serializing_if = "Option::is_none")]
                text_signature: Option<String>,
            },
            #[serde(rename = "thinking")]
            Thinking {
                thinking: String,
                #[serde(rename = "thinkingSignature", default, skip_serializing_if = "Option::is_none")]
                thinking_signature: Option<String>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                redacted: Option<bool>,
            },
            #[serde(rename = "toolCall")]
            ToolCall {
                id: String,
                name: String,
                arguments: Value,
                #[serde(rename = "thoughtSignature", default, skip_serializing_if = "Option::is_none")]
                thought_signature: Option<String>,
            },
        }

        impl AssistantContentPart {
            pub fn as_tool_call(&self) -> Option<ToolCall> {
                match self {
                    AssistantContentPart::ToolCall {
                        id,
                        name,
                        arguments,
                        thought_signature,
                    } => Some(ToolCall {
                        r#type: "toolCall".to_string(),
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                        thought_signature: thought_signature.clone(),
                    }),
                    _ => None,
                }
            }
        }

        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct UsageCost {
            pub input: f64,
            pub output: f64,
            #[serde(rename = "cacheRead")]
            pub cache_read: f64,
            #[serde(rename = "cacheWrite")]
            pub cache_write: f64,
            pub total: f64,
        }

        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct Usage {
            pub input: f64,
            pub output: f64,
            #[serde(rename = "cacheRead")]
            pub cache_read: f64,
            #[serde(rename = "cacheWrite")]
            pub cache_write: f64,
            #[serde(rename = "totalTokens")]
            pub total_tokens: f64,
            pub cost: UsageCost,
        }

        impl Usage {
            pub fn empty() -> Self {
                Self::default()
            }
        }

        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct DiagnosticErrorInfo {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub name: Option<String>,
            pub message: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub stack: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub code: Option<Value>,
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct AssistantMessageDiagnostic {
            pub r#type: String,
            pub timestamp: i64,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub error: Option<DiagnosticErrorInfo>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub details: Option<BTreeMap<String, Value>>,
        }

        /// `formatThrownValue`
        pub fn format_thrown_value(value: &Value) -> String {
            match value {
                Value::String(text) => text.clone(),
                Value::Null => "null".to_string(),
                other => other.to_string(),
            }
        }

        /// `extractDiagnosticError`
        pub fn extract_diagnostic_error(error: &anyhow::Error) -> DiagnosticErrorInfo {
            DiagnosticErrorInfo {
                name: None,
                message: format!("{error}"),
                stack: None,
                code: None,
            }
        }

        /// `createAssistantMessageDiagnostic`
        pub fn create_assistant_message_diagnostic(
            r#type: impl Into<String>,
            error: &anyhow::Error,
            details: Option<BTreeMap<String, Value>>,
            timestamp: i64,
        ) -> AssistantMessageDiagnostic {
            AssistantMessageDiagnostic {
                r#type: r#type.into(),
                timestamp,
                error: Some(extract_diagnostic_error(error)),
                details,
            }
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct AssistantMessage {
            pub role: String,
            pub content: Vec<AssistantContentPart>,
            pub api: Api,
            pub provider: Provider,
            pub model: String,
            #[serde(rename = "responseModel", default, skip_serializing_if = "Option::is_none")]
            pub response_model: Option<String>,
            #[serde(rename = "responseId", default, skip_serializing_if = "Option::is_none")]
            pub response_id: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
            pub usage: Usage,
            #[serde(rename = "stopReason")]
            pub stop_reason: StopReason,
            #[serde(rename = "stopReasonRaw", default, skip_serializing_if = "Option::is_none")]
            pub stop_reason_raw: Option<String>,
            #[serde(rename = "errorMessage", default, skip_serializing_if = "Option::is_none")]
            pub error_message: Option<String>,
            pub timestamp: i64,
        }

        impl AssistantMessage {
            pub fn empty(api: impl Into<String>, provider: impl Into<String>, model: impl Into<String>) -> Self {
                Self {
                    role: "assistant".to_string(),
                    content: Vec::new(),
                    api: api.into(),
                    provider: provider.into(),
                    model: model.into(),
                    response_model: None,
                    response_id: None,
                    diagnostics: None,
                    usage: Usage::empty(),
                    stop_reason: StopReason::Stop,
                    stop_reason_raw: None,
                    error_message: None,
                    timestamp: 0,
                }
            }

            /// `content.filter((c) => c.type === "toolCall")`
            pub fn tool_calls(&self) -> Vec<ToolCall> {
                self.content.iter().filter_map(|part| part.as_tool_call()).collect()
            }
        }

        /// `AssistantMessageEvent`
        #[derive(Debug, Clone)]
        pub enum AssistantMessageEvent {
            Start {
                partial: AssistantMessage,
            },
            TextStart {
                content_index: usize,
                partial: AssistantMessage,
            },
            TextDelta {
                content_index: usize,
                delta: String,
                partial: AssistantMessage,
            },
            TextEnd {
                content_index: usize,
                content: String,
                partial: AssistantMessage,
            },
            ThinkingStart {
                content_index: usize,
                partial: AssistantMessage,
            },
            ThinkingDelta {
                content_index: usize,
                delta: String,
                partial: AssistantMessage,
            },
            ThinkingEnd {
                content_index: usize,
                content: String,
                partial: AssistantMessage,
            },
            ToolCallStart {
                content_index: usize,
                partial: AssistantMessage,
            },
            ToolCallDelta {
                content_index: usize,
                delta: String,
                partial: AssistantMessage,
            },
            ToolCallEnd {
                content_index: usize,
                tool_call: ToolCall,
                partial: AssistantMessage,
            },
            Done {
                reason: StopReason,
                message: AssistantMessage,
            },
            Error {
                reason: StopReason,
                error: AssistantMessage,
            },
        }

        impl AssistantMessageEvent {
            pub fn type_name(&self) -> &'static str {
                match self {
                    AssistantMessageEvent::Start { .. } => "start",
                    AssistantMessageEvent::TextStart { .. } => "text_start",
                    AssistantMessageEvent::TextDelta { .. } => "text_delta",
                    AssistantMessageEvent::TextEnd { .. } => "text_end",
                    AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
                    AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
                    AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
                    AssistantMessageEvent::ToolCallStart { .. } => "toolcall_start",
                    AssistantMessageEvent::ToolCallDelta { .. } => "toolcall_delta",
                    AssistantMessageEvent::ToolCallEnd { .. } => "toolcall_end",
                    AssistantMessageEvent::Done { .. } => "done",
                    AssistantMessageEvent::Error { .. } => "error",
                }
            }

            pub fn partial(&self) -> Option<&AssistantMessage> {
                match self {
                    AssistantMessageEvent::Start { partial }
                    | AssistantMessageEvent::TextStart { partial, .. }
                    | AssistantMessageEvent::TextDelta { partial, .. }
                    | AssistantMessageEvent::TextEnd { partial, .. }
                    | AssistantMessageEvent::ThinkingStart { partial, .. }
                    | AssistantMessageEvent::ThinkingDelta { partial, .. }
                    | AssistantMessageEvent::ThinkingEnd { partial, .. }
                    | AssistantMessageEvent::ToolCallStart { partial, .. }
                    | AssistantMessageEvent::ToolCallDelta { partial, .. }
                    | AssistantMessageEvent::ToolCallEnd { partial, .. } => Some(partial),
                    AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => None,
                }
            }
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(untagged)]
        pub enum UserContent {
            Text(String),
            Blocks(Vec<Value>),
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct UserMessage {
            pub role: String,
            pub content: UserContent,
            #[serde(rename = "providerContext", default, skip_serializing_if = "Option::is_none")]
            pub provider_context: Option<crate::types::pi_ai_shim::compaction::ProviderCompactionCheckpoint>,
            pub timestamp: i64,
        }

        impl UserMessage {
            pub fn text(text: impl Into<String>, timestamp: i64) -> Self {
                Self {
                    role: "user".to_string(),
                    content: UserContent::Text(text.into()),
                    provider_context: None,
                    timestamp,
                }
            }
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct ToolResultMessage {
            pub role: String,
            #[serde(rename = "toolCallId")]
            pub tool_call_id: String,
            #[serde(rename = "toolName")]
            pub tool_name: String,
            pub content: Vec<crate::types::ContentBlock>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub details: Option<Value>,
            #[serde(rename = "isError")]
            pub is_error: bool,
            pub timestamp: i64,
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(untagged)]
        pub enum Message {
            User(UserMessage),
            Assistant(AssistantMessage),
            ToolResult(ToolResultMessage),
        }

        impl Message {
            pub fn role(&self) -> &str {
                match self {
                    Message::User(_) => "user",
                    Message::Assistant(_) => "assistant",
                    Message::ToolResult(_) => "toolResult",
                }
            }
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct Tool {
            pub name: String,
            pub description: String,
            pub parameters: Value,
        }

        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct Context {
            #[serde(rename = "systemPrompt", default, skip_serializing_if = "Option::is_none")]
            pub system_prompt: Option<String>,
            pub messages: Vec<Message>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub tools: Option<Vec<Tool>>,
        }

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct ProviderResponse {
            pub status: i64,
            pub headers: BTreeMap<String, String>,
        }

        /// Content-free raw token observation for process-local performance metrics.
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct ProviderUsageObservation {
            #[serde(rename = "inputTokens")]
            pub input_tokens: Option<f64>,
            #[serde(rename = "cachedInputTokens")]
            pub cached_input_tokens: Option<f64>,
            #[serde(rename = "outputTokens")]
            pub output_tokens: Option<f64>,
            #[serde(rename = "reasoningTokens")]
            pub reasoning_tokens: Option<f64>,
            #[serde(rename = "totalTokens")]
            pub total_tokens: Option<f64>,
            #[serde(rename = "cachedInputIncludedInInput")]
            pub cached_input_included_in_input: Option<bool>,
            #[serde(rename = "reasoningIncludedInOutput")]
            pub reasoning_included_in_output: Option<bool>,
        }

        #[derive(Clone, Default)]
        pub struct StreamOptions {
            pub temperature: Option<f64>,
            pub max_tokens: Option<f64>,
            pub api_key: Option<String>,
            pub transport: Option<Transport>,
            pub service_tier: Option<ServiceTier>,
            pub cache_retention: Option<CacheRetention>,
            pub session_id: Option<String>,
            pub headers: Option<BTreeMap<String, String>>,
            pub timeout_ms: Option<f64>,
            pub metadata: Option<BTreeMap<String, Value>>,
            pub on_payload: Option<Arc<dyn Fn(Value, Model) -> Option<Value> + Send + Sync>>,
            pub on_response: Option<Arc<dyn Fn(ProviderResponse, Model) + Send + Sync>>,
            pub on_usage_observation:
                Option<Arc<dyn Fn(ProviderUsageObservation, Model) + Send + Sync>>,
        }

        impl std::fmt::Debug for StreamOptions {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("StreamOptions")
                    .field("temperature", &self.temperature)
                    .field("max_tokens", &self.max_tokens)
                    .field("session_id", &self.session_id)
                    .finish_non_exhaustive()
            }
        }

        #[derive(Clone, Default)]
        pub struct SimpleStreamOptions {
            pub temperature: Option<f64>,
            pub max_tokens: Option<f64>,
            pub api_key: Option<String>,
            pub transport: Option<Transport>,
            pub service_tier: Option<ServiceTier>,
            pub cache_retention: Option<CacheRetention>,
            pub session_id: Option<String>,
            pub headers: Option<BTreeMap<String, String>>,
            pub timeout_ms: Option<f64>,
            pub metadata: Option<BTreeMap<String, Value>>,
            pub reasoning: Option<String>,
            pub thinking_budgets: Option<ThinkingBudgets>,
            pub on_payload: Option<Arc<dyn Fn(Value, Model) -> Option<Value> + Send + Sync>>,
            pub on_response: Option<Arc<dyn Fn(ProviderResponse, Model) + Send + Sync>>,
            pub on_usage_observation:
                Option<Arc<dyn Fn(ProviderUsageObservation, Model) + Send + Sync>>,
        }

        impl std::fmt::Debug for SimpleStreamOptions {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("SimpleStreamOptions")
                    .field("temperature", &self.temperature)
                    .field("max_tokens", &self.max_tokens)
                    .field("reasoning", &self.reasoning)
                    .finish_non_exhaustive()
            }
        }

        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct Model {
            pub id: String,
            pub name: String,
            pub api: Api,
            pub provider: Provider,
            #[serde(rename = "baseUrl")]
            pub base_url: String,
            pub reasoning: bool,
            #[serde(rename = "thinkingLevelMap", default, skip_serializing_if = "Option::is_none")]
            pub thinking_level_map: Option<BTreeMap<String, Option<String>>>,
            pub input: Vec<String>,
            pub cost: ModelCost,
            #[serde(rename = "contextWindow")]
            pub context_window: f64,
            #[serde(rename = "maxInputTokens", default, skip_serializing_if = "Option::is_none")]
            pub max_input_tokens: Option<f64>,
            #[serde(rename = "maxTokens")]
            pub max_tokens: f64,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub featured: Option<bool>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub headers: Option<BTreeMap<String, String>>,
        }

        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct ModelCost {
            pub input: f64,
            pub output: f64,
            #[serde(rename = "cacheRead")]
            pub cache_read: f64,
            #[serde(rename = "cacheWrite")]
            pub cache_write: f64,
        }

        impl Model {
            /// `DEFAULT_MODEL` in packages/agent/src/agent.ts.
            pub fn unknown() -> Self {
                Self {
                    id: "unknown".to_string(),
                    name: "unknown".to_string(),
                    api: "unknown".to_string(),
                    provider: "unknown".to_string(),
                    base_url: String::new(),
                    reasoning: false,
                    thinking_level_map: None,
                    input: Vec::new(),
                    cost: ModelCost::default(),
                    context_window: 0.0,
                    max_input_tokens: None,
                    max_tokens: 0.0,
                    featured: None,
                    headers: None,
                }
            }
        }
    }

    pub mod compaction {
        use serde::{Deserialize, Serialize};
        use serde_json::Value;

        /// An opaque provider checkpoint; replay the entire window without rewriting its items.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct ProviderCompactionCheckpoint {
            pub version: i64,
            pub provider: String,
            pub api: String,
            pub model: String,
            #[serde(rename = "baseUrl")]
            pub base_url: String,
            /// Exact request endpoint when recorded by a newer adapter. Legacy checkpoints omit it.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub endpoint: Option<String>,
            pub items: Vec<Value>,
            #[serde(rename = "estimatedTokens")]
            pub estimated_tokens: f64,
        }
    }

    pub mod utils {
        pub mod event_stream {
            use std::collections::VecDeque;
            use std::sync::{Arc, Mutex};

            use futures::future::BoxFuture;
            use tokio::sync::oneshot;

            use crate::types::pi_ai_shim::types::{AssistantMessage, AssistantMessageEvent};

            type Waiter<T> = oneshot::Sender<Option<T>>;

            /// Port of packages/ai/src/utils/event-stream.ts `EventStream`.
            pub struct EventStream<T, R> {
                queue: Mutex<VecDeque<T>>,
                waiting: Mutex<VecDeque<Waiter<T>>>,
                done: Mutex<bool>,
                final_result: Mutex<Option<R>>,
                result_waiters: Mutex<Vec<oneshot::Sender<R>>>,
                is_complete: Arc<dyn Fn(&T) -> bool + Send + Sync>,
                extract_result: Arc<dyn Fn(&T) -> R + Send + Sync>,
            }

            impl<T: Send + 'static, R: Send + Clone + 'static> EventStream<T, R> {
                pub fn new(
                    is_complete: impl Fn(&T) -> bool + Send + Sync + 'static,
                    extract_result: impl Fn(&T) -> R + Send + Sync + 'static,
                ) -> Self {
                    Self {
                        queue: Mutex::new(VecDeque::new()),
                        waiting: Mutex::new(VecDeque::new()),
                        done: Mutex::new(false),
                        final_result: Mutex::new(None),
                        result_waiters: Mutex::new(Vec::new()),
                        is_complete: Arc::new(is_complete),
                        extract_result: Arc::new(extract_result),
                    }
                }

                pub fn push(&self, event: T) {
                    if *self.done.lock().unwrap() {
                        return;
                    }

                    if (self.is_complete)(&event) {
                        *self.done.lock().unwrap() = true;
                        let result = (self.extract_result)(&event);
                        self.resolve_final_result(result);
                    }

                    let waiter = self.waiting.lock().unwrap().pop_front();
                    match waiter {
                        Some(waiter) => {
                            let _ = waiter.send(Some(event));
                        }
                        None => self.queue.lock().unwrap().push_back(event),
                    }
                }

                pub fn end(&self, result: Option<R>) {
                    *self.done.lock().unwrap() = true;
                    if let Some(result) = result {
                        self.resolve_final_result(result);
                    }
                    while let Some(waiter) = self.waiting.lock().unwrap().pop_front() {
                        let _ = waiter.send(None);
                    }
                }

                fn resolve_final_result(&self, result: R) {
                    let mut slot = self.final_result.lock().unwrap();
                    if slot.is_none() {
                        *slot = Some(result.clone());
                    }
                    drop(slot);
                    let mut waiters = self.result_waiters.lock().unwrap();
                    while let Some(waiter) = waiters.pop() {
                        let _ = waiter.send(result.clone());
                    }
                }

                pub fn result(&self) -> BoxFuture<'static, R> {
                    let existing = self.final_result.lock().unwrap().clone();
                    if let Some(result) = existing {
                        return Box::pin(async move { result });
                    }
                    let (sender, receiver) = oneshot::channel();
                    self.result_waiters.lock().unwrap().push(sender);
                    Box::pin(async move {
                        match receiver.await {
                            Ok(result) => result,
                            // The sender is only dropped when the stream itself is dropped.
                            Err(_) => loop {
                                futures::future::pending::<()>().await;
                            },
                        }
                    })
                }

                /// `[Symbol.asyncIterator]()` - `None` ends the iteration.
                pub fn next_event(&self) -> BoxFuture<'static, Option<T>> {
                    let queued = self.queue.lock().unwrap().pop_front();
                    if let Some(event) = queued {
                        return Box::pin(async move { Some(event) });
                    }
                    if *self.done.lock().unwrap() {
                        return Box::pin(async move { None });
                    }
                    let (sender, receiver) = oneshot::channel();
                    self.waiting.lock().unwrap().push_back(sender);
                    Box::pin(async move { receiver.await.unwrap_or(None) })
                }

                pub fn is_done(&self) -> bool {
                    *self.done.lock().unwrap()
                }
            }

            /// Port of `AssistantMessageEventStream`.
            ///
            /// The TypeScript class is used both as an async iterator and via
            /// `result()`. In Rust both views need shared ownership, so the
            /// stream is handed around as an `Arc`.
            pub type AssistantMessageEventStream =
                Arc<EventStream<AssistantMessageEvent, AssistantMessage>>;

            pub fn create_assistant_message_event_stream() -> AssistantMessageEventStream {
                Arc::new(EventStream::new(
                    |event| {
                        matches!(
                            event,
                            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
                        )
                    },
                    |event| match event {
                        AssistantMessageEvent::Done { message, .. } => message.clone(),
                        AssistantMessageEvent::Error { error, .. } => error.clone(),
                        other => panic!(
                            "Unexpected event type for final result: {}",
                            other.type_name()
                        ),
                    },
                ))
            }
        }

        pub mod json_parse {
            use serde_json::{Map, Value};

            const VALID_JSON_ESCAPES: [char; 9] = ['"', '\\', '/', 'b', 'f', 'n', 'r', 't', 'u'];

            fn is_control_character(ch: char) -> bool {
                (ch as u32) <= 0x1f
            }

            fn escape_control_character(ch: char) -> String {
                match ch {
                    '\u{8}' => "\\b".to_string(),
                    '\u{c}' => "\\f".to_string(),
                    '\n' => "\\n".to_string(),
                    '\r' => "\\r".to_string(),
                    '\t' => "\\t".to_string(),
                    other => format!("\\u{:04x}", other as u32),
                }
            }

            /// Repairs malformed JSON string literals by escaping raw control
            /// characters inside strings and doubling backslashes before invalid
            /// escape characters.
            pub fn repair_json(json: &str) -> String {
                let chars: Vec<char> = json.chars().collect();
                let mut repaired = String::with_capacity(json.len());
                let mut in_string = false;
                let mut index = 0usize;

                while index < chars.len() {
                    let ch = chars[index];

                    if !in_string {
                        repaired.push(ch);
                        if ch == '"' {
                            in_string = true;
                        }
                        index += 1;
                        continue;
                    }

                    if ch == '"' {
                        repaired.push(ch);
                        in_string = false;
                        index += 1;
                        continue;
                    }

                    if ch == '\\' {
                        let next = chars.get(index + 1).copied();
                        let Some(next) = next else {
                            repaired.push_str("\\\\");
                            index += 1;
                            continue;
                        };

                        if next == 'u' {
                            let unicode_digits: String = chars.iter().skip(index + 2).take(4).collect();
                            if unicode_digits.len() == 4
                                && unicode_digits.chars().all(|digit| digit.is_ascii_hexdigit())
                            {
                                repaired.push_str(&format!("\\u{unicode_digits}"));
                                index += 6;
                                continue;
                            }
                        }

                        if VALID_JSON_ESCAPES.contains(&next) {
                            repaired.push('\\');
                            repaired.push(next);
                            index += 2;
                            continue;
                        }

                        repaired.push_str("\\\\");
                        index += 1;
                        continue;
                    }

                    if is_control_character(ch) {
                        repaired.push_str(&escape_control_character(ch));
                    } else {
                        repaired.push(ch);
                    }
                    index += 1;
                }

                repaired
            }

            pub fn parse_json_with_repair(json: &str) -> Result<Value, serde_json::Error> {
                match serde_json::from_str::<Value>(json) {
                    Ok(value) => Ok(value),
                    Err(error) => {
                        let repaired = repair_json(json);
                        if repaired != json {
                            return serde_json::from_str::<Value>(&repaired);
                        }
                        Err(error)
                    }
                }
            }

            /// Attempts to parse potentially incomplete JSON during streaming.
            /// Always returns a valid object, even if the JSON is incomplete.
            pub fn parse_streaming_json(partial_json: Option<&str>) -> Value {
                let Some(partial_json) = partial_json else {
                    return Value::Object(Map::new());
                };
                if partial_json.trim().is_empty() {
                    return Value::Object(Map::new());
                }
                if let Ok(value) = parse_json_with_repair(partial_json) {
                    return value;
                }
                // partial-json equivalent: close open strings/containers and retry.
                if let Some(value) = try_close_incomplete_json(partial_json) {
                    return value;
                }
                if let Some(value) = try_close_incomplete_json(&repair_json(partial_json)) {
                    return value;
                }
                Value::Object(Map::new())
            }

            fn try_close_incomplete_json(input: &str) -> Option<Value> {
                let chars: Vec<char> = input.chars().collect();
                let mut stack: Vec<char> = Vec::new();
                let mut in_string = false;
                let mut escaped = false;

                for (index, ch) in chars.iter().enumerate() {
                    if in_string {
                        if escaped {
                            escaped = false;
                        } else if *ch == '\\' {
                            escaped = true;
                        } else if *ch == '"' {
                            in_string = false;
                        }
                        continue;
                    }
                    match ch {
                        '"' => in_string = true,
                        '{' | '[' => stack.push(*ch),
                        '}' | ']' => {
                            stack.pop();
                        }
                        _ => {}
                    }
                    let _ = index;
                }

                let mut repaired = input.to_string();
                if escaped {
                    repaired.push('\\');
                }
                if in_string {
                    repaired.push('"');
                }
                // A trailing key without a value cannot be closed safely.
                while let Some(open) = stack.pop() {
                    repaired.push(if open == '{' { '}' } else { ']' });
                }
                serde_json::from_str::<Value>(&repaired).ok()
            }
        }

        pub mod validation {
            use serde_json::{Map, Value};

            use crate::types::pi_ai_shim::types::Tool;

            fn is_record(value: &Value) -> bool {
                value.is_object()
            }

            fn matches_json_type(value: &Value, json_type: &str) -> bool {
                match json_type {
                    "number" => value.is_number(),
                    "integer" => value.is_i64() || value.is_u64() || value.as_f64().map(f64::fract) == Some(0.0),
                    "boolean" => value.is_boolean(),
                    "string" => value.is_string(),
                    "null" => value.is_null(),
                    "array" => value.is_array(),
                    "object" => is_record(value),
                    _ => false,
                }
            }

            fn format_validation_path(instance_path: &str, required_property: Option<&str>) -> String {
                if let Some(required_property) = required_property {
                    let base_path = instance_path
                        .trim_start_matches('/')
                        .replace('/', ".");
                    return if base_path.is_empty() {
                        required_property.to_string()
                    } else {
                        format!("{base_path}.{required_property}")
                    };
                }
                let path = instance_path.trim_start_matches('/').replace('/', ".");
                if path.is_empty() {
                    "root".to_string()
                } else {
                    path
                }
            }

            fn collect_errors(schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
                if let Some(required) = schema.get("required").and_then(Value::as_array) {
                    for property in required.iter().filter_map(Value::as_str) {
                        let present = value.get(property).map(|entry| !entry.is_null()).unwrap_or(false);
                        if !present {
                            errors.push(format!(
                                "  - {}: must have required property '{}'",
                                format_validation_path(path, Some(property)),
                                property
                            ));
                        }
                    }
                }

                if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                    for (key, property_schema) in properties {
                        let Some(property_value) = value.get(key) else { continue };
                        if property_value.is_null() {
                            continue;
                        }
                        if let Some(expected) = property_schema.get("type").and_then(Value::as_str) {
                            if !matches_json_type(property_value, expected) {
                                errors.push(format!(
                                    "  - {}: must be {}",
                                    format_validation_path(&format!("{path}/{key}"), None),
                                    expected
                                ));
                            }
                        }
                        if let Some(enum_values) = property_schema.get("enum").and_then(Value::as_array) {
                            if !enum_values.contains(property_value) {
                                errors.push(format!(
                                    "  - {}: must be equal to one of the allowed values",
                                    format_validation_path(&format!("{path}/{key}"), None)
                                ));
                            }
                        }
                    }
                }
            }

            /// Validates tool call arguments against the tool's schema.
            ///
            /// TypeBox compilation is not available in Rust; this checks required
            /// properties, declared types and enums, and keeps the TypeScript error
            /// message shape. Recorded as needs
            /// pi-ai::utils::validation::validate_tool_arguments.
            pub fn validate_tool_arguments(tool: &Tool, tool_call: &crate::types::pi_ai_shim::types::ToolCall) -> Result<Value, anyhow::Error> {
                let mut args = match tool_call.arguments.clone() {
                    Value::Object(map) => map,
                    other => {
                        let mut map = Map::new();
                        map.insert("value".to_string(), other);
                        map
                    }
                };

                let mut errors = Vec::new();
                collect_errors(&tool.parameters, &Value::Object(args.clone()), "", &mut errors);

                if errors.is_empty() {
                    return Ok(Value::Object(args));
                }

                let joined = if errors.is_empty() {
                    "Unknown validation error".to_string()
                } else {
                    errors.join("\n")
                };
                let error_message = format!(
                    "Validation failed for tool \"{}\":\n{}\n\nReceived arguments:\n{}",
                    tool_call.name,
                    joined,
                    serde_json::to_string_pretty(&tool_call.arguments).unwrap_or_default()
                );
                // Keep the mutable binding observable for parity with Value.Convert.
                args.clear();
                Err(anyhow::anyhow!(error_message))
            }
        }
    }

    pub mod stream {
        use std::sync::Arc;

        use crate::types::pi_ai_shim::types::{
            AssistantMessage, Context, Model, SimpleStreamOptions,
        };
        use crate::types::pi_ai_shim::utils::event_stream::{
            create_assistant_message_event_stream, AssistantMessageEventStream,
        };

        /// Minimal stand-in for `streamSimple`.
        ///
        /// The real implementation resolves a registered API provider; without the
        /// pi-ai provider registry this returns a stream whose terminal message
        /// carries `stopReason: "error"` and the same "No API provider registered
        /// for api" message that `resolveApiProvider` throws.
        pub fn stream_simple(
            model: Model,
            _context: Context,
            _options: Option<SimpleStreamOptions>,
        ) -> AssistantMessageEventStream {
            let stream = create_assistant_message_event_stream();
            let mut message = AssistantMessage::empty(model.api.clone(), model.provider.clone(), model.id.clone());
            message.stop_reason = crate::types::pi_ai_shim::types::StopReason::Error;
            message.error_message = Some(format!("No API provider registered for api: {}", model.api));
            let partial = message.clone();
            stream.push(crate::types::pi_ai_shim::types::AssistantMessageEvent::Start {
                partial: partial.clone(),
            });
            stream.push(crate::types::pi_ai_shim::types::AssistantMessageEvent::Error {
                reason: crate::types::pi_ai_shim::types::StopReason::Error,
                error: message,
            });
            stream
        }

        pub type StreamSimpleFn = Arc<
            dyn Fn(Model, Context, Option<SimpleStreamOptions>) -> AssistantMessageEventStream
                + Send
                + Sync,
        >;
    }
}
