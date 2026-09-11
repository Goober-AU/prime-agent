# ai-providers slice: interface contract (draft)

This file is written by the ai-providers slice lead. It is NOT a Rust module and must
not be compiled. It pins the Rust names/shapes that every file under
`crates/pi-ai/src/providers/**` must use.

`crates/pi-ai/src/types.rs`, `utils/event_stream.rs`, `utils/stream_failure.rs`,
`utils/json_parse.rs`, `api_registry.rs`, `compaction.rs` are owned by the `ai-core`
slice. Use them, never edit them. If a name here does not exist in the real file, adapt
mechanically (TS kebab/camel -> snake_case) and report the mismatch.

## Non-negotiable port rules (from docs/PORT-RULES.md)

- 1:1 port of `packages/ai/src/providers/<x>.ts`. Keep function/const names and every
  string literal, number, default, JSON field name and error message identical.
- Only serde, serde_json, thiserror, anyhow, tokio, futures, indexmap, base64, sha2,
  uuid, chrono, regex, url, bytes, async-trait, tokio-util, reqwest are available
  (added by the ai-core slice to crates/pi-ai/Cargo.toml). Do not add dependencies.
- Do not create new Rust modules. Every helper you need lives inside your own file.
- Do not edit files outside your assigned paths. Do not edit packages/**.
- Never call a real provider/network in tests.

## Types (crate::types::*)

TS `Api`/`Provider` are open string unions -> Rust `String`:

```rust
pub type Api = String;        // "anthropic-messages", "openai-completions", ...
pub type Provider = String;   // "anthropic", "openai", ...
pub type KnownApi = String;   pub type KnownProvider = String;
pub type ThinkingLevel = String;  pub type ModelThinkingLevel = String;
pub type CacheRetention = String; pub type Transport = String;
pub type ServiceTier = Option<String>;              // includes null
pub type StopReason = String;                       // "stop"|"length"|"toolUse"|"error"|"aborted"
pub type InputModality = enum { Text, Image }       // "text"/"image"; Model.input: Vec<InputModality>
```
StopReason is a STRING, not an enum: write `"toolUse".to_string()`, `msg.stop_reason == "error"`, etc.

Model (one struct; TS `Model<TApi>` is not generic in Rust):

```rust
pub struct Model {
    pub id: String,
    pub name: String,
    pub api: Api,
    pub provider: Provider,
    pub base_url: String,
    pub reasoning: bool,
    pub thinking_level_map: Option<IndexMap<String, Option<String>>>, // ModelThinkingLevel -> Option<String>
    pub input: Vec<String>,          // "text" | "image"
    pub cost: ModelCost,             // { input, output, cache_read, cache_write: f64 }
    pub context_window: f64,
    pub max_input_tokens: Option<f64>,
    pub max_tokens: f64,
    pub featured: Option<bool>,
    pub native_compaction: Option<NativeCompactionCapability>,
    pub headers: Option<IndexMap<String, String>>,
    pub compat: Option<Compat>,
}
impl Model {
    pub fn compat_completions(&self) -> Option<&OpenAICompletionsCompat>;
    pub fn compat_responses(&self) -> Option<&OpenAIResponsesCompat>;
    pub fn compat_anthropic(&self) -> Option<&AnthropicMessagesCompat>;
}
```

Messages. TS discriminated unions on `role` -> Rust enum with serde tag:

```rust
#[serde(tag = "role", rename_all = "camelCase")]   // "user" | "assistant" | "toolResult"
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

pub struct UserMessage {
    pub content: UserContent,                          // string | (Text|Image)[]
    pub provider_context: Option<ProviderCompactionCheckpoint>,
    pub timestamp: f64,
}
pub enum UserContent { Text(String), Blocks(Vec<ContentBlock>) }

pub struct AssistantMessage {
    pub content: Vec<ContentBlock>,                    // Text | Thinking | ToolCall
    pub api: Api, pub provider: Provider, pub model: String,
    pub response_model: Option<String>, pub response_id: Option<String>,
    pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    pub stop_reason_raw: Option<String>,
    pub error_message: Option<String>,
    pub timestamp: f64,
}

pub struct ToolResultMessage {
    pub tool_call_id: String, pub tool_name: String,
    pub content: Vec<ContentBlock>,                    // Text | Image
    pub details: Option<serde_json::Value>,
    pub is_error: bool, pub timestamp: f64,
}
```

Content blocks (TS TextContent | ThinkingContent | ImageContent | ToolCall). If
types.rs instead exposes four separate structs plus a wrapper enum, use those names;
the variants below are the expected shape:

```rust
#[serde(tag = "type")]      // "text" | "thinking" | "image" | "toolCall"
pub enum ContentBlock {
    #[serde(rename = "text")]     Text(TextContent),
    #[serde(rename = "thinking")] Thinking(ThinkingContent),
    #[serde(rename = "image")]    Image(ImageContent),
    #[serde(rename = "toolCall")] ToolCall(ToolCall),
}
pub struct TextContent { pub text: String, pub text_signature: Option<String> }
pub struct ThinkingContent { pub thinking: String, pub thinking_signature: Option<String>, pub redacted: Option<bool> }
pub struct ImageContent { pub data: String, pub mime_type: String }
pub struct ToolCall { pub id: String, pub name: String, pub arguments: serde_json::Map<String, serde_json::Value>, pub thought_signature: Option<String> }
```

Context / tools / usage / stop reason:

```rust
pub struct Context { pub system_prompt: Option<String>, pub messages: Vec<Message>, pub tools: Option<Vec<Tool>> }
pub struct Tool { pub name: String, pub description: String, pub parameters: serde_json::Value }
pub struct Usage { pub input: f64, pub output: f64, pub cache_read: f64, pub cache_write: f64, pub total_tokens: f64, pub cost: UsageCost }
pub struct UsageCost { pub input: f64, pub output: f64, pub cache_read: f64, pub cache_write: f64, pub total: f64 }
pub enum StopReason { Stop, Length, ToolUse, Error, Aborted }   // serde rename "toolUse"
```

Options:

```rust
pub struct StreamOptions {
    pub temperature: Option<f64>, pub max_tokens: Option<f64>,
    pub signal: Option<CancellationToken>,            // AbortSignal
    pub api_key: Option<String>, pub transport: Option<String>,
    pub service_tier: Option<Option<String>>,         // ServiceTier includes null
    pub cache_retention: Option<String>, pub session_id: Option<String>,
    pub on_payload: Option<Arc<dyn Fn(serde_json::Value, &Model) -> Option<serde_json::Value> + Send + Sync>>,
    pub on_response: Option<Arc<dyn Fn(ProviderResponse, &Model) + Send + Sync>>,
    pub on_usage_observation: Option<...>,
    pub headers: Option<IndexMap<String, String>>,
    pub timeout_ms: Option<f64>,
    pub metadata: Option<IndexMap<String, serde_json::Value>>,
}
pub struct SimpleStreamOptions { /* StreamOptions flattened */ pub reasoning: Option<String>, pub thinking_budgets: Option<ThinkingBudgets> }
pub struct ProviderResponse { pub status: u16, pub headers: IndexMap<String, String> }
```

Event stream (crate::utils::event_stream, owner ai-core, signatures agreed):

```rust
pub struct AssistantMessageEventStream;                 // Clone (Arc inside), Send + Sync
impl AssistantMessageEventStream {
    pub fn new() -> Self;
    pub fn push(&self, event: AssistantMessageEvent);
    pub fn end(&self, result: Option<AssistantMessage>);
    pub async fn next(&self) -> Option<AssistantMessageEvent>;
    pub async fn result(&self) -> AssistantMessage;
    pub fn spawn<F: Future<Output = ()> + Send + 'static>(&self, future: F);
}
pub fn create_assistant_message_event_stream() -> AssistantMessageEventStream;
```

`AssistantMessageEvent` mirrors the TS union (serde tag `type`), variants:
`Start{partial}`, `TextStart{content_index, partial}`, `TextDelta{content_index, delta, partial}`,
`TextEnd{content_index, content, partial}`, `ThinkingStart/Delta/End{...}`,
`ToolcallStart{content_index, partial}`, `ToolcallDelta{content_index, delta, partial}`,
`ToolcallEnd{content_index, tool_call, partial}`, `Done{reason, message}`, `Error{reason, error}`.
Field names are snake_case in Rust, renamed to camelCase on the wire.

## Required provider pattern (TS -> Rust)

TS:

```ts
export const streamAnthropic: StreamFunction<"anthropic-messages", AnthropicOptions> = (model, context, options) => {
    const stream = new AssistantMessageEventStream();
    (async () => { try { ... } catch (error) { ... } })();
    return stream;
};
```

Rust (same observable behaviour: sync return, work in the background):

```rust
pub fn stream_anthropic(model: Model, context: Context, options: Option<AnthropicOptions>) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let out = stream.clone();
    tokio::spawn(async move {
        // the whole TS async IIFE body, in order
        // push events to `out`; finish with out.end(Some(final_message))
    });
    stream
}
```

- Never panic out of the spawned task: the TS IIFE catches everything and turns it into
  `AssistantMessageEvent::Error` + `end`. Keep the same try/catch boundary and the same
  error message text.
- Abort: `options.signal` is `Option<CancellationToken>`; check `is_cancelled()` at the
  same points the TS checks `signal?.aborted`, and produce the same aborted message
  (`stopReason: "aborted"`, errorMessage from the TS).
- HTTP: use `reqwest`. Build the request body exactly as the TS code/SDK does (same JSON
  keys, same defaults) and parse the SSE/streaming response in a local loop inside your
  file (no shared module). `reqwest` is in the workspace.
- `on_payload` / `on_response` callbacks must be called at the same point, with the same
  payload shape, as the TS.
- Usage/`calculateCost` etc. come from `crate::models` (ai-core slice).

## Status reporting

Write your report to your parent (`agent_message.send(..., receiver_role="parent")`):
file(s) written, functions mapped count, anything you could not port, exact blockers.

## Authoritative surface confirmed by the ai-core slice owner (2026-09-11)

Differences from the draft above - these win:

- `StopReason = String`, `Api = String`, `Provider = String`, `ThinkingLevel = String`,
  `ModelThinkingLevel = String`, `CacheRetention = String`, `Transport = String`,
  `ServiceTier = Option<String>`, `KnownApi = String`, `KnownProvider = String`.
  There are per-literal consts (`API_OPENAI_COMPLETIONS`, ...) if you want them.
- `Model.input: Vec<InputModality>` where `enum InputModality { Text, Image }`.
  Check images with `model.input.iter().any(|m| matches!(m, InputModality::Image))`.
- `ContentBlock` = `Text(TextContent) | Thinking(ThinkingContent) | ToolCall(ToolCall)` only.
  User/tool-result content uses `ImageOrTextContent` (`Text(TextContent) | Image(ImageContent)`).
  `UserMessage.content: UserContent` (`Text(String) | Blocks(Vec<ImageOrTextContent>)`).
- `ToolResultMessage::new(tool_call_id, tool_name, content, is_error, timestamp)` exists.
- `ThinkingBudgets { minimal, low, medium, high: Option<f64> }`.
- `StreamOptions` fields: temperature, max_tokens, signal: Option<CancellationToken>, api_key,
  transport, service_tier: ServiceTier, cache_retention, session_id, on_payload, on_response,
  on_usage_observation, headers: Option<IndexMap<String,String>>, timeout_ms, metadata,
  `extra` (flattened `Map<String,Value>` so `ProviderStreamOptions` can carry unknown keys).
- `SimpleStreamOptions { #[serde(flatten)] stream: StreamOptions, reasoning: Option<String>,
  thinking_budgets: Option<ThinkingBudgets> }`.
- `AssistantMessageEvent` variants: `Start`, `TextStart`, `TextDelta`, `TextEnd`,
  `ThinkingStart`, `ThinkingDelta`, `ThinkingEnd`, `ToolCallStart`, `ToolCallDelta`,
  `ToolCallEnd`, `Done`, `Error`; serde tag `type`, explicit renames (`text_start`, ...),
  camelCase fields (`contentIndex`, `toolCall`).
- `AssistantMessage`/`UserMessage`/`ToolResultMessage` `timestamp: i64` (milliseconds).
- `Compat` is an untagged enum: `Completions(OpenAICompletionsCompat)`,
  `Responses(OpenAIResponsesCompat)`, `Anthropic(AnthropicMessagesCompat)`;
  access with `model.compat_completions()` / `compat_responses()` / `compat_anthropic()`.
- `StreamFunction = Arc<dyn Fn(&Model, &Context, Option<&StreamOptions>) -> AssistantMessageEventStream + Send + Sync>`.
- Registry (api_registry.rs): `register_api_provider(ApiProvider)`,
  `get_api_provider(api, provider) -> Option<ApiProvider>`, `get_api_providers()`,
  `unregister_api_providers(source_id)`, `clear_api_providers()`.
- `crate::compaction`: `ProviderCompactionCheckpoint`, `ProviderCompactionResult`,
  `CompactionOptions { #[serde(flatten)] simple: SimpleStreamOptions, custom_instructions }`,
  `is_compaction_checkpoint(&Value) -> bool`, `compaction_matches_model(&ProviderCompactionCheckpoint, &Model) -> bool`.
- `crate::utils::json_parse`: `repair_json(&str) -> String`, `parse_json_with_repair(&str) -> Result<Value,String>`,
  `parse_streaming_json(Option<&str>) -> Value`.
- `crate::utils::sanitize_unicode`: `sanitize_surrogates(&str) -> String`.
- `crate::utils::stream_failure`: port of utils/stream-failure.ts, snake_case names
  (`classify_stream_failure`, `is_retryable`, ...). Read the file before using it.
- `crate::utils::overflow`: port of utils/overflow.ts, snake_case names
  (`is_context_overflow_error`, ...). Read the file before using it.
- `crate::models`: `get_model`, `get_models`, `calculate_cost(&Model, &mut Usage, Option<CostOverrides>)`,
  `get_supported_thinking_levels`, `clamp_thinking_level`, `supports_fast_mode`,
  `get_model_input_limit`, `models_are_equal`.

Every provider file must expose BOTH:

1. the typed entry point mirroring the TS export, e.g.
   `pub fn stream_anthropic(model: &Model, context: &Context, options: Option<AnthropicOptions>) -> AssistantMessageEventStream`
   (provider options struct = `#[serde(flatten)] pub stream: StreamOptions` plus the extra TS fields),
2. nothing else is needed for registration: `register_builtins.rs` (written by the slice lead)
   converts `Option<&StreamOptions>` into the typed options struct.

## transform-messages (written by the slice lead, use this exact signature)

```rust
pub fn transform_messages(
    messages: Vec<Message>,
    model: &Model,
    normalize_tool_call_id: Option<&dyn Fn(&str, &Model, &AssistantMessage) -> String>,
) -> Vec<Message>;
```
