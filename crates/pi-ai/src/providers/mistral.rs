//! Port of packages/ai/src/providers/mistral.ts
//!
//! The TypeScript uses `@mistralai/mistralai` 2.2.1 (`mistral.chat.stream`). This port
//! builds the same `POST {serverURL}/v1/chat/completions` request itself with `reqwest`
//! and parses the SSE stream locally, keeping the SDK's outbound JSON (`snake_case`
//! remaps), header merge order and error text.

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::env_api_keys::get_env_api_key;
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::providers::simple_options::build_base_options;
use crate::providers::transform_messages::try_transform_messages;
use crate::types::{
	AssistantMessage, AssistantMessageEvent, ContentBlock, Context, Message, Model, SimpleStreamOptions, StopReason,
	StreamOptions, TextContent, ThinkingContent, ToolCall, Usage,
};
use crate::utils::event_stream::{create_assistant_message_event_stream, AssistantMessageEventStream};
use crate::utils::hash::short_hash;
use crate::utils::headers::header_map_to_record;
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::now_ms;
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::stream_failure::{
	record_stream_failure, stream_failure_from_stop_reason, StreamFailureError, ThrownStreamError,
};

const MISTRAL_TOOL_CALL_ID_LENGTH: usize = 9;
const MAX_MISTRAL_ERROR_BODY_CHARS: usize = 4000;

/// `SDK_METADATA.userAgent` of the pinned `@mistralai/mistralai` 2.2.1 client.
const MISTRAL_USER_AGENT: &str = "speakeasy-sdk/typescript 2.2.1 2.881.4 1.0.0 @mistralai/mistralai";
/// `ServerList[ServerEu]` - the SDK default when `serverURL` is not set.
const MISTRAL_DEFAULT_SERVER_URL: &str = "https://api.mistral.ai";
/// `timeoutMs: options?.timeoutMs || client._options.timeoutMs || 30000`.
const MISTRAL_DEFAULT_TIMEOUT_MS: u64 = 30000;

/// Mistral reasoning-effort values.
pub type MistralReasoningEffort = String;

/// TS: `interface MistralOptions extends StreamOptions`.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MistralOptions {
	#[serde(flatten)]
	pub stream: StreamOptions,
	/// `"auto" | "none" | "any" | "required" | { type: "function"; function: { name: string } }`
	pub tool_choice: Option<Value>,
	pub prompt_mode: Option<String>,
	pub reasoning_effort: Option<MistralReasoningEffort>,
}

impl MistralOptions {
	/// TS: the caller passes `StreamOptions & Record<string, unknown>`; this keeps the
	/// non-serializable fields (signal, on_payload, on_response, on_usage_observation).
	pub fn from_base(base: &StreamOptions) -> Self {
		Self {
			stream: base.clone(),
			tool_choice: None,
			prompt_mode: None,
			reasoning_effort: None,
		}
	}
}

impl std::fmt::Debug for MistralOptions {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("MistralOptions")
			.field("stream", &self.stream)
			.field("tool_choice", &self.tool_choice)
			.field("prompt_mode", &self.prompt_mode)
			.field("reasoning_effort", &self.reasoning_effort)
			.finish()
	}
}

/// The TypeScript throws either a plain `Error`, a `StreamFailureError` or a Mistral
/// SDK error (`MistralError`/`SDKError` with `statusCode` and `body`).
#[derive(Debug, Clone, PartialEq)]
pub enum MistralStreamError {
	Failure(StreamFailureError),
	Message(String),
	/// TS: a Mistral SDK error instance. `message` is the SDK-built message,
	/// `statusCode`/`body` feed `formatMistralError`.
	Api {
		message: String,
		status_code: Option<i64>,
		body: Option<String>,
		value: Value,
	},
}

impl MistralStreamError {
	fn as_thrown(&self) -> ThrownStreamError<'_> {
		match self {
			MistralStreamError::Failure(failure) => ThrownStreamError::Failure(failure),
			MistralStreamError::Message(message) => ThrownStreamError::Message(message),
			MistralStreamError::Api { value, .. } => ThrownStreamError::Value(value),
		}
	}

	/// `error instanceof Error ? error.message : ...`
	fn error_message(&self) -> String {
		match self {
			MistralStreamError::Failure(failure) => failure.message.clone(),
			MistralStreamError::Message(message) => message.clone(),
			MistralStreamError::Api { message, .. } => message.clone(),
		}
	}

	/// `error as Error & { statusCode?, body? }` for `formatMistralError`.
	fn status_code(&self) -> Option<i64> {
		match self {
			MistralStreamError::Api { status_code, .. } => *status_code,
			_ => None,
		}
	}

	fn body(&self) -> Option<String> {
		match self {
			MistralStreamError::Api { body, .. } => body.clone(),
			_ => None,
		}
	}
}

impl std::fmt::Display for MistralStreamError {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(formatter, "{}", self.error_message())
	}
}

impl std::error::Error for MistralStreamError {}

/// Streams Mistral chat completions through `chat.stream`.
///
/// TS: `streamMistral: StreamFunction<"mistral-conversations", MistralOptions>`.
pub fn stream_mistral(
	model: &Model,
	context: &Context,
	options: Option<MistralOptions>,
) -> AssistantMessageEventStream {
	let stream = create_assistant_message_event_stream();
	let out = stream.clone();
	let model = model.clone();
	let context = context.clone();
	let options = options.unwrap_or_default();
	tokio::spawn(async move {
		let mut output = create_output(&model);

		match run_stream_mistral(&model, &context, &options, &mut output, &out).await {
			Ok(()) => {}
			Err(error) => {
				// partialArgs is only a streaming scratch buffer; never persist it.
				// (The Rust port keeps it outside the block, so there is nothing to delete.)
				let aborted = options
					.stream
					.signal
					.as_ref()
					.map(|signal| signal.is_cancelled())
					.unwrap_or(false);
				output.stop_reason = if aborted { "aborted".to_string() } else { "error".to_string() };
				output.error_message = Some(format_mistral_error(&error));
				record_stream_failure(&model, &mut output, &error.as_thrown());
				out.push(AssistantMessageEvent::Error {
					reason: output.stop_reason.clone(),
					error: output.clone(),
				});
				out.end(None);
			}
		}
	});
	stream
}

/// The TypeScript async IIFE body of `streamMistral`.
async fn run_stream_mistral(
	model: &Model,
	context: &Context,
	options: &MistralOptions,
	output: &mut AssistantMessage,
	stream: &AssistantMessageEventStream,
) -> Result<(), MistralStreamError> {
	let api_key = options
		.stream
		.api_key
		.clone()
		.or_else(|| get_env_api_key(&model.provider));
	let Some(api_key) = api_key else {
		return Err(MistralStreamError::Message(format!(
			"No API key for provider: {}",
			model.provider
		)));
	};

	// Intentionally per-request: avoids shared SDK mutable state across concurrent consumers.
	let server_url = if model.base_url.is_empty() {
		MISTRAL_DEFAULT_SERVER_URL.to_string()
	} else {
		model.base_url.clone()
	};

	let normalizer = MistralToolCallIdNormalizer::new();
	let normalize = |id: &str, _model: &Model, _source: &AssistantMessage| normalizer.normalize(id);
	let transformed_messages = try_transform_messages(context.messages.clone(), model, Some(&normalize))
		.map_err(MistralStreamError::Message)?;

	let mut payload = build_chat_payload(model, context, &transformed_messages, Some(options));
	if let Some(on_payload) = options.stream.on_payload.clone() {
		let next_payload = on_payload(Value::Object(payload.clone()), model).await;
		if let Some(next_payload) = next_payload {
			payload = match next_payload {
				Value::Object(map) => map,
				_ => Map::new(),
			};
		}
	}
	let response = send_request(&server_url, &api_key, &payload, model, options).await?;
	if let Some(on_response) = options.stream.on_response.clone() {
		on_response(
			crate::types::ProviderResponse {
				status: response.status().as_u16() as i64,
				headers: header_map_to_record(response.headers()),
			},
			model,
		)
		.await;
	}

	stream.push(AssistantMessageEvent::Start {
		partial: output.clone(),
	});
	let mut chunks = MistralChunkStream::new(response, options.stream.signal.clone());
	consume_chat_stream(model, output, stream, &mut chunks).await?;

	if options
		.stream
		.signal
		.as_ref()
		.map(|signal| signal.is_cancelled())
		.unwrap_or(false)
	{
		return Err(MistralStreamError::Message("Request was aborted".to_string()));
	}

	if output.stop_reason == "aborted" || output.stop_reason == "error" {
		return Err(MistralStreamError::Failure(stream_failure_from_stop_reason(
			output.stop_reason_raw.as_deref(),
			None,
		)));
	}

	stream.push(AssistantMessageEvent::Done {
		reason: output.stop_reason.clone(),
		message: output.clone(),
	});
	stream.end(None);
	Ok(())
}

/// Maps provider-agnostic `SimpleStreamOptions` to Mistral request options.
///
/// TS: `streamSimpleMistral: StreamFunction<"mistral-conversations", SimpleStreamOptions>`.
pub fn stream_simple_mistral(
	model: &Model,
	context: &Context,
	options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
	let api_key = options
		.as_ref()
		.and_then(|options| options.stream.api_key.clone())
		.or_else(|| get_env_api_key(&model.provider));
	let Some(api_key) = api_key else {
		// The TypeScript throws synchronously here.
		panic!("No API key for provider: {}", model.provider);
	};

	let base = build_base_options(model, options.as_ref(), Some(&api_key));
	let clamped_reasoning = options
		.as_ref()
		.and_then(|options| options.reasoning.clone())
		.map(|reasoning| clamp_thinking_level(model, &reasoning));
	let reasoning = clamped_reasoning.filter(|reasoning| reasoning != "off");
	let should_use_reasoning = model.reasoning && reasoning.is_some();

	let mut typed = MistralOptions::from_base(&base);
	typed.prompt_mode = if should_use_reasoning && uses_prompt_mode_reasoning(model) {
		Some("reasoning".to_string())
	} else {
		None
	};
	typed.reasoning_effort = if should_use_reasoning && uses_reasoning_effort(model) {
		Some(map_reasoning_effort(model, reasoning.as_deref().unwrap_or_default()))
	} else {
		None
	};
	stream_mistral(model, context, Some(typed))
}

/// TS: `createOutput(model)`.
fn create_output(model: &Model) -> AssistantMessage {
	AssistantMessage {
		content: Vec::new(),
		api: model.api.clone(),
		provider: model.provider.clone(),
		model: model.id.clone(),
		usage: Usage::zero(),
		stop_reason: "stop".to_string(),
		timestamp: now_ms(),
		..Default::default()
	}
}

/// TS: `createMistralToolCallIdNormalizer()` - the two `Map`s live inside the closure.
///
/// The returned closure is `Fn` (like the TypeScript arrow function), so the maps
/// use interior mutability; it is shared with the streaming tool-call id derivation.
#[derive(Clone, Default)]
struct MistralToolCallIdNormalizer {
	state: Arc<std::sync::Mutex<ToolCallIdState>>,
}

#[derive(Default)]
struct ToolCallIdState {
	id_map: HashMap<String, String>,
	reverse_map: HashMap<String, String>,
}

impl MistralToolCallIdNormalizer {
	fn new() -> Self {
		Self::default()
	}

	fn normalize(&self, id: &str) -> String {
		let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
		if let Some(existing) = state.id_map.get(id) {
			return existing.clone();
		}

		let mut attempt = 0usize;
		loop {
			let candidate = derive_mistral_tool_call_id(id, attempt);
			match state.reverse_map.get(&candidate) {
				Some(owner) if owner != id => {
					attempt += 1;
				}
				_ => {
					state.id_map.insert(id.to_string(), candidate.clone());
					state.reverse_map.insert(candidate.clone(), id.to_string());
					return candidate;
				}
			}
		}
	}
}

/// TS: `deriveMistralToolCallId(id, attempt)`.
fn derive_mistral_tool_call_id(id: &str, attempt: usize) -> String {
	let normalized: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
	if attempt == 0 && normalized.len() == MISTRAL_TOOL_CALL_ID_LENGTH {
		return normalized;
	}
	let seed_base = if normalized.is_empty() { id } else { &normalized };
	let seed = if attempt == 0 {
		seed_base.to_string()
	} else {
		format!("{}:{}", seed_base, attempt)
	};
	short_hash(&seed)
		.chars()
		.filter(|c| c.is_ascii_alphanumeric())
		.take(MISTRAL_TOOL_CALL_ID_LENGTH)
		.collect()
}

/// TS: `formatMistralError(error)`.
fn format_mistral_error(error: &MistralStreamError) -> String {
	match error {
		MistralStreamError::Api { message, .. } => {
			let status_code = error.status_code();
			let body_text = error
				.body()
				.map(|body| body.trim().to_string())
				.filter(|body| !body.is_empty());
			if let (Some(status_code), Some(body_text)) = (status_code, body_text) {
				return format!(
					"Mistral API error ({}): {}",
					status_code,
					truncate_error_text(&body_text, MAX_MISTRAL_ERROR_BODY_CHARS)
				);
			}
			if let Some(status_code) = status_code {
				return format!("Mistral API error ({}): {}", status_code, message);
			}
			message.clone()
		}
		other => other.error_message(),
	}
}

/// TS: `truncateErrorText(text, maxChars)`.
fn truncate_error_text(text: &str, max_chars: usize) -> String {
	if text.len() <= max_chars {
		return text.to_string();
	}
	format!(
		"{}... [truncated {} chars]",
		&text[..max_chars],
		text.len() - max_chars
	)
}

/// TS: `safeJsonStringify(value)`.
fn safe_json_stringify(value: &Value) -> String {
	serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// TS: `buildRequestOptions(model, options?)`.
#[derive(Debug, Clone, Default)]
pub struct MistralRequestOptions {
	pub retries_strategy_none: bool,
	pub headers: IndexMap<String, String>,
}

fn build_request_options(model: &Model, options: &MistralOptions) -> MistralRequestOptions {
	let mut headers: IndexMap<String, String> = IndexMap::new();
	if let Some(model_headers) = &model.headers {
		for (key, value) in model_headers {
			headers.insert(key.clone(), value.clone());
		}
	}
	if let Some(options_headers) = &options.stream.headers {
		for (key, value) in options_headers {
			headers.insert(key.clone(), value.clone());
		}
	}

	// Mistral infrastructure uses `x-affinity` for KV-cache reuse (prefix caching).
	// Respect explicit caller-provided header values.
	if let Some(session_id) = &options.stream.session_id {
		if !headers.keys().any(|key| key.eq_ignore_ascii_case("x-affinity")) {
			headers.insert("x-affinity".to_string(), session_id.clone());
		}
	}

	MistralRequestOptions {
		retries_strategy_none: true,
		headers,
	}
}

/// `mistral.chat.stream(payload, requestOptions)`.
async fn send_request(
	server_url: &str,
	api_key: &str,
	payload: &Map<String, Value>,
	model: &Model,
	options: &MistralOptions,
) -> Result<reqwest::Response, MistralStreamError> {
	let base = server_url.strip_suffix('/').unwrap_or(server_url);
	let url = format!("{}/v1/chat/completions", base);
	let request_options = build_request_options(model, options);

	let mut headers = reqwest::header::HeaderMap::new();
	headers.insert(
		reqwest::header::CONTENT_TYPE,
		reqwest::header::HeaderValue::from_static("application/json"),
	);
	headers.insert(
		reqwest::header::ACCEPT,
		reqwest::header::HeaderValue::from_static("text/event-stream"),
	);
	headers.insert(
		reqwest::header::AUTHORIZATION,
		reqwest::header::HeaderValue::from_str(&format!("Bearer {}", api_key))
			.map_err(|error| MistralStreamError::Message(error.to_string()))?,
	);
	headers.insert(
		reqwest::header::USER_AGENT,
		reqwest::header::HeaderValue::from_static(MISTRAL_USER_AGENT),
	);
	for (key, value) in request_options.headers {
		if let (Ok(name), Ok(header_value)) = (
			reqwest::header::HeaderName::from_bytes(key.as_bytes()),
			reqwest::header::HeaderValue::from_str(&value),
		) {
			headers.insert(name, header_value);
		}
	}

	let mut request = reqwest::Client::new()
		.post(&url)
		.headers(headers)
		.json(&Value::Object(payload.clone()));
	let timeout_ms = options.stream.timeout_ms.map(|value| value.max(0.0) as u64);
	request = request.timeout(std::time::Duration::from_millis(
		timeout_ms.unwrap_or(MISTRAL_DEFAULT_TIMEOUT_MS),
	));

	let send = request.send();
	let response = match options.stream.signal.as_ref() {
		Some(signal) => tokio::select! {
			_ = signal.cancelled() => return Err(MistralStreamError::Message("Request was aborted".to_string())),
			result = send => result,
		},
		None => send.await,
	};
	let response = response.map_err(|error| MistralStreamError::Message(error.to_string()))?;
	check_response(response).await
}

/// TS: `matchStatusCode({ status }, ["4XX", "5XX"])` -> the SDK raises
/// `HTTPClientError`/`SDKError` with `statusCode` and `body` set.
async fn check_response(response: reqwest::Response) -> Result<reqwest::Response, MistralStreamError> {
	if response.status().is_success() {
		return Ok(response);
	}
	let status_code = response.status().as_u16() as i64;
	let status_text = response.status().canonical_reason().unwrap_or("").to_string();
	let body = response.text().await.unwrap_or_default();
	let message = format!("Status {}: Body: {}", status_code, body);
	let mut value = Map::new();
	value.insert("name".to_string(), Value::String("SDKError".to_string()));
	value.insert("message".to_string(), Value::String(message.clone()));
	value.insert("statusCode".to_string(), Value::Number(status_code.into()));
	value.insert("status".to_string(), Value::Number(status_code.into()));
	value.insert("body".to_string(), Value::String(body.clone()));
	value.insert("statusText".to_string(), Value::String(status_text));
	Err(MistralStreamError::Api {
		message,
		status_code: Some(status_code),
		body: Some(body),
		value: Value::Object(value),
	})
}

/// TS: `buildChatPayload(model, context, messages, options?)`.
pub fn build_chat_payload(
	model: &Model,
	context: &Context,
	messages: &[Message],
	options: Option<&MistralOptions>,
) -> Map<String, Value> {
	let supports_images = model.input.iter().any(|m| matches!(m, crate::types::InputModality::Image));
	let mut payload = Map::new();
	payload.insert("model".to_string(), Value::String(model.id.clone()));
	payload.insert("stream".to_string(), Value::Bool(true));
	payload.insert(
		"messages".to_string(),
		Value::Array(to_chat_messages(messages, supports_images)),
	);

	let options = options.cloned().unwrap_or_default();
	if let Some(tools) = context.tools.as_ref().filter(|tools| !tools.is_empty()) {
		payload.insert("tools".to_string(), Value::Array(to_function_tools(tools)));
	}
	if let Some(temperature) = options.stream.temperature {
		payload.insert(
			"temperature".to_string(),
			serde_json::Number::from_f64(temperature)
				.map(Value::Number)
				.unwrap_or(Value::Null),
		);
	}
	if let Some(max_tokens) = options.stream.max_tokens {
		payload.insert(
			"maxTokens".to_string(),
			serde_json::Number::from_f64(max_tokens)
				.map(Value::Number)
				.unwrap_or(Value::Null),
		);
	}
	if let Some(tool_choice) = options.tool_choice.clone() {
		if let Some(mapped) = map_tool_choice(Some(&tool_choice)) {
			payload.insert("toolChoice".to_string(), mapped);
		}
	}
	if let Some(prompt_mode) = options.prompt_mode.clone() {
		payload.insert("promptMode".to_string(), Value::String(prompt_mode));
	}
	if let Some(reasoning_effort) = options.reasoning_effort.clone() {
		payload.insert("reasoningEffort".to_string(), Value::String(reasoning_effort));
	}

	if let Some(system_prompt) = &context.system_prompt {
		let mut system_message = Map::new();
		system_message.insert("role".to_string(), Value::String("system".to_string()));
		system_message.insert(
			"content".to_string(),
			Value::String(sanitize_surrogates(system_prompt)),
		);
		if let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) {
			messages.insert(0, Value::Object(system_message));
		}
	}

	rename_chat_payload_keys(&mut payload);
	payload
}

/// TS: the SDK's outbound `remap$` - camelCase in code, snake_case on the wire.
fn rename_chat_payload_keys(payload: &mut Map<String, Value>) {
	const REMAP: [(&str, &str); 10] = [
		("topP", "top_p"),
		("maxTokens", "max_tokens"),
		("randomSeed", "random_seed"),
		("responseFormat", "response_format"),
		("toolChoice", "tool_choice"),
		("presencePenalty", "presence_penalty"),
		("frequencyPenalty", "frequency_penalty"),
		("parallelToolCalls", "parallel_tool_calls"),
		("reasoningEffort", "reasoning_effort"),
		("promptMode", "prompt_mode"),
	];
	let mut renamed = Map::new();
	for (key, value) in payload.iter() {
		let target = REMAP
			.iter()
			.find(|(from, _)| *from == key)
			.map(|(_, to)| (*to).to_string())
			.unwrap_or_else(|| key.clone());
		renamed.insert(target, value.clone());
	}
	*payload = renamed;
}

/// TS: `toFunctionTools(tools)`.
fn to_function_tools(tools: &[crate::types::Tool]) -> Vec<Value> {
	tools
		.iter()
		.map(|tool| {
			let mut function = Map::new();
			function.insert("name".to_string(), Value::String(tool.name.clone()));
			function.insert("description".to_string(), Value::String(tool.description.clone()));
			function.insert("parameters".to_string(), strip_symbol_keys(&tool.parameters));
			function.insert("strict".to_string(), Value::Bool(false));
			let mut entry = Map::new();
			entry.insert("type".to_string(), Value::String("function".to_string()));
			entry.insert("function".to_string(), Value::Object(function));
			Value::Object(entry)
		})
		.collect()
}

/// TS: `stripSymbolKeys(value)` - a deep copy that drops symbol keys. JSON values
/// cannot carry symbol keys, so the port rebuilds the structure in the same order.
fn strip_symbol_keys(value: &Value) -> Value {
	match value {
		Value::Array(items) => Value::Array(items.iter().map(strip_symbol_keys).collect()),
		Value::Object(object) => {
			let mut result = Map::new();
			for (key, entry) in object {
				result.insert(key.clone(), strip_symbol_keys(entry));
			}
			Value::Object(result)
		}
		other => other.clone(),
	}
}

/// TS: `{ type: "image_url", imageUrl }` - the SDK's `ImageURLChunk` outbound remap
/// (`imageUrl` -> `image_url`) is applied here so the wire JSON matches.
fn image_url_chunk(url: &str) -> Value {
	let mut chunk = Map::new();
	chunk.insert("type".to_string(), Value::String("image_url".to_string()));
	chunk.insert("image_url".to_string(), Value::String(url.to_string()));
	Value::Object(chunk)
}

/// TS: `toChatMessages(messages, supportsImages)`.
pub fn to_chat_messages(messages: &[Message], supports_images: bool) -> Vec<Value> {
	let mut result: Vec<Value> = Vec::new();

	for msg in messages {
		match msg {
			Message::User(user) => match &user.content {
				crate::types::UserContent::Text(text) => {
					let mut message = Map::new();
					message.insert("role".to_string(), Value::String("user".to_string()));
					message.insert("content".to_string(), Value::String(sanitize_surrogates(text)));
					result.push(Value::Object(message));
					continue;
				}
				crate::types::UserContent::Blocks(blocks) => {
					let had_images = blocks
						.iter()
						.any(|item| matches!(item, crate::types::ImageOrTextContent::Image(_)));
					let content: Vec<Value> = blocks
						.iter()
						.filter(|item| {
							matches!(item, crate::types::ImageOrTextContent::Text(_)) || supports_images
						})
						.map(|item| match item {
							crate::types::ImageOrTextContent::Text(text) => {
								let mut chunk = Map::new();
								chunk.insert("type".to_string(), Value::String("text".to_string()));
								chunk.insert(
									"text".to_string(),
									Value::String(sanitize_surrogates(&text.text)),
								);
								Value::Object(chunk)
							}
							crate::types::ImageOrTextContent::Image(image) => {
								image_url_chunk(&format!("data:{};base64,{}", image.mime_type, image.data))
							}
						})
						.collect();
					if !content.is_empty() {
						let mut message = Map::new();
						message.insert("role".to_string(), Value::String("user".to_string()));
						message.insert("content".to_string(), Value::Array(content));
						result.push(Value::Object(message));
						continue;
					}
					if had_images && !supports_images {
						let mut message = Map::new();
						message.insert("role".to_string(), Value::String("user".to_string()));
						message.insert(
							"content".to_string(),
							Value::String("(image omitted: model does not support images)".to_string()),
						);
						result.push(Value::Object(message));
					}
					continue;
				}
			},
			Message::Assistant(assistant) => {
				let mut content_parts: Vec<Value> = Vec::new();
				let mut tool_calls: Vec<Value> = Vec::new();

				for block in assistant.content.iter() {
					match block {
						ContentBlock::Text(text) => {
							if !text.text.trim().is_empty() {
								let mut chunk = Map::new();
								chunk.insert("type".to_string(), Value::String("text".to_string()));
								chunk.insert(
									"text".to_string(),
									Value::String(sanitize_surrogates(&text.text)),
								);
								content_parts.push(Value::Object(chunk));
							}
							continue;
						}
						ContentBlock::Thinking(thinking) => {
							if !thinking.thinking.trim().is_empty() {
								let mut inner = Map::new();
								inner.insert("type".to_string(), Value::String("text".to_string()));
								inner.insert(
									"text".to_string(),
									Value::String(sanitize_surrogates(&thinking.thinking)),
								);
								let mut chunk = Map::new();
								chunk.insert("type".to_string(), Value::String("thinking".to_string()));
								chunk.insert("thinking".to_string(), Value::Array(vec![Value::Object(inner)]));
								content_parts.push(Value::Object(chunk));
							}
							continue;
						}
						ContentBlock::ToolCall(tool_call) => {
							let mut function = Map::new();
							function.insert("name".to_string(), Value::String(tool_call.name.clone()));
							function.insert(
								"arguments".to_string(),
								Value::String(
									serde_json::to_string(&Value::Object(tool_call.arguments.clone()))
										.unwrap_or_else(|_| "{}".to_string()),
								),
							);
							let mut entry = Map::new();
							entry.insert("id".to_string(), Value::String(tool_call.id.clone()));
							entry.insert("type".to_string(), Value::String("function".to_string()));
							entry.insert("function".to_string(), Value::Object(function));
							tool_calls.push(Value::Object(entry));
						}
					}
				}

				let mut message = Map::new();
				message.insert("role".to_string(), Value::String("assistant".to_string()));
				if !content_parts.is_empty() {
					message.insert("content".to_string(), Value::Array(content_parts.clone()));
				}
				if !tool_calls.is_empty() {
					message.insert("toolCalls".to_string(), Value::Array(tool_calls.clone()));
				}
				if !content_parts.is_empty() || !tool_calls.is_empty() {
					result.push(Value::Object(rename_assistant_message(message)));
				}
				continue;
			}
			Message::ToolResult(tool_result) => {
				let mut tool_content: Vec<Value> = Vec::new();
				let text_result = tool_result
					.content
					.iter()
					.filter_map(|part| match part {
						crate::types::ImageOrTextContent::Text(text) => Some(sanitize_surrogates(&text.text)),
						crate::types::ImageOrTextContent::Image(_) => None,
					})
					.collect::<Vec<_>>()
					.join("\n");
				let has_images = tool_result
					.content
					.iter()
					.any(|part| matches!(part, crate::types::ImageOrTextContent::Image(_)));
				let tool_text = build_tool_result_text(&text_result, has_images, supports_images, tool_result.is_error);
				let mut chunk = Map::new();
				chunk.insert("type".to_string(), Value::String("text".to_string()));
				chunk.insert("text".to_string(), Value::String(tool_text));
				tool_content.push(Value::Object(chunk));
				for part in tool_result.content.iter() {
					if !supports_images {
						continue;
					}
					let crate::types::ImageOrTextContent::Image(image) = part else {
						continue;
					};
					tool_content.push(image_url_chunk(&format!("data:{};base64,{}", image.mime_type, image.data)));
				}
				let mut message = Map::new();
				message.insert("role".to_string(), Value::String("tool".to_string()));
				message.insert("toolCallId".to_string(), Value::String(tool_result.tool_call_id.clone()));
				message.insert("name".to_string(), Value::String(tool_result.tool_name.clone()));
				message.insert("content".to_string(), Value::Array(tool_content));
				result.push(Value::Object(rename_tool_message(message)));
			}
		}
	}

	result
}

/// TS: the `AssistantMessage$outboundSchema` remap (`toolCalls` -> `tool_calls`).
fn rename_assistant_message(message: Map<String, Value>) -> Map<String, Value> {
	let mut renamed = Map::new();
	for (key, value) in message {
		let target = if key == "toolCalls" { "tool_calls".to_string() } else { key };
		renamed.insert(target, value);
	}
	renamed
}

/// TS: the `ToolMessage$outboundSchema` remap (`toolCallId` -> `tool_call_id`).
fn rename_tool_message(message: Map<String, Value>) -> Map<String, Value> {
	let mut renamed = Map::new();
	for (key, value) in message {
		let target = if key == "toolCallId" {
			"tool_call_id".to_string()
		} else {
			key
		};
		renamed.insert(target, value);
	}
	renamed
}

/// TS: `buildToolResultText(text, hasImages, supportsImages, isError)`.
fn build_tool_result_text(text: &str, has_images: bool, supports_images: bool, is_error: bool) -> String {
	let trimmed = text.trim();
	let error_prefix = if is_error { "[tool error] " } else { "" };

	if !trimmed.is_empty() {
		let image_suffix = if has_images && !supports_images {
			"\n[tool image omitted: model does not support images]"
		} else {
			""
		};
		return format!("{}{}{}", error_prefix, trimmed, image_suffix);
	}

	if has_images {
		if supports_images {
			return if is_error {
				"[tool error] (see attached image)".to_string()
			} else {
				"(see attached image)".to_string()
			};
		}
		return if is_error {
			"[tool error] (image omitted: model does not support images)".to_string()
		} else {
			"(image omitted: model does not support images)".to_string()
		};
	}

	if is_error {
		"[tool error] (no tool output)".to_string()
	} else {
		"(no tool output)".to_string()
	}
}

/// TS: `usesReasoningEffort(model)`.
fn uses_reasoning_effort(model: &Model) -> bool {
	model.id == "mistral-small-2603" || model.id == "mistral-small-latest" || model.id == "mistral-medium-3.5"
}

/// TS: `usesPromptModeReasoning(model)`.
fn uses_prompt_mode_reasoning(model: &Model) -> bool {
	model.reasoning && !uses_reasoning_effort(model)
}

/// TS: `mapReasoningEffort(model, level)`.
fn map_reasoning_effort(model: &Model, level: &str) -> MistralReasoningEffort {
	model
		.thinking_level_map_get(level)
		.flatten()
		.unwrap_or_else(|| "high".to_string())
}

/// TS: `mapToolChoice(choice)`.
fn map_tool_choice(choice: Option<&Value>) -> Option<Value> {
	let choice = choice?;
	match choice {
		Value::String(choice) if choice == "auto" || choice == "none" || choice == "any" || choice == "required" => {
			Some(Value::String(choice.clone()))
		}
		Value::Object(object) => {
			let name = object
				.get("function")
				.and_then(|function| function.get("name"))
				.and_then(Value::as_str)
				.unwrap_or_default();
			let mut function = Map::new();
			function.insert("name".to_string(), Value::String(name.to_string()));
			let mut mapped = Map::new();
			mapped.insert("type".to_string(), Value::String("function".to_string()));
			mapped.insert("function".to_string(), Value::Object(function));
			Some(Value::Object(mapped))
		}
		_ => None,
	}
}

/// TS: `mapChatStopReason(reason)`.
fn map_chat_stop_reason(reason: Option<&str>) -> StopReason {
	let Some(reason) = reason else {
		return "stop".to_string();
	};
	match reason {
		"stop" => "stop".to_string(),
		"length" | "model_length" => "length".to_string(),
		"tool_calls" => "toolUse".to_string(),
		"error" => "error".to_string(),
		_ => "stop".to_string(),
	}
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// The Mistral SSE transport (`EventStream` in the SDK).
struct MistralChunkStream {
	chunks: std::pin::Pin<Box<dyn futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
	buffer: String,
	pending: Vec<Value>,
	done: bool,
	finished: bool,
	signal: Option<tokio_util::sync::CancellationToken>,
}

impl MistralChunkStream {
	fn new(response: reqwest::Response, signal: Option<tokio_util::sync::CancellationToken>) -> Self {
		Self {
			chunks: Box::pin(response.bytes_stream()),
			buffer: String::new(),
			pending: Vec::new(),
			done: false,
			finished: false,
			signal,
		}
	}

	/// TS: `for await (const event of mistralStream)` - each item is `event.data`.
	async fn next(&mut self) -> Result<Option<Value>, MistralStreamError> {
		loop {
			if !self.pending.is_empty() {
				return Ok(Some(self.pending.remove(0)));
			}
			if self.done {
				return Ok(None);
			}
			if self.finished {
				return Ok(None);
			}
			if let Some(signal) = &self.signal {
				if signal.is_cancelled() {
					return Err(MistralStreamError::Message("Request was aborted".to_string()));
				}
			}
			match self.chunks.next().await {
				None => {
					self.finished = true;
				}
				Some(Err(error)) => return Err(MistralStreamError::Message(error.to_string())),
				Some(Ok(bytes)) => {
					self.buffer.push_str(&String::from_utf8_lossy(&bytes));
					self.drain_events()?;
				}
			}
		}
	}

	/// TS: `findBoundary` + `parseMessage` + the `[DONE]` short circuit.
	fn drain_events(&mut self) -> Result<(), MistralStreamError> {
		const BOUNDARIES: [&str; 8] = [
			"\r\n\r\n", "\r\n\r", "\r\n\n", "\r\r\n", "\n\r\n", "\r\r", "\n\r", "\n\n",
		];
		loop {
			let mut boundary_index: Option<usize> = None;
			let mut boundary_length = 0usize;
			for boundary in BOUNDARIES {
				if let Some(index) = self.buffer.find(boundary) {
					if boundary_index.map_or(true, |current| index < current) {
						boundary_index = Some(index);
						boundary_length = boundary.len();
					}
				}
			}
			let Some(boundary_index) = boundary_index else {
				return Ok(());
			};
			let message = self.buffer[..boundary_index].to_string();
			self.buffer = self.buffer[boundary_index + boundary_length..].to_string();
			let Some(data) = parse_sse_message(&message) else {
				continue;
			};
			if data == "[DONE]" {
				self.done = true;
				return Ok(());
			}
			match serde_json::from_str::<Value>(&data) {
				Ok(value) => self.pending.push(value),
				Err(error) => {
					return Err(MistralStreamError::Message(format!(
						"malformed json: {}",
						error
					)))
				}
			}
		}
	}
}

/// TS: `parseMessage(chunk, parse, state, dataRequired)` - returns the joined
/// `data` field, or `None` for a message with no data lines.
fn parse_sse_message(message: &str) -> Option<String> {
	let mut data_lines: Vec<String> = Vec::new();
	let mut ignore = true;
	for line in message.split(|c| c == '\r' || c == '\n') {
		if line.is_empty() || line.starts_with(':') {
			continue;
		}
		ignore = false;
		let index = line.find(':');
		let (field, value) = match index {
			Some(index) if index > 0 => {
				let value = &line[index + 1..];
				let value = value.strip_prefix(' ').unwrap_or(value);
				(&line[..index], value)
			}
			_ => (line, ""),
		};
		if field == "data" {
			data_lines.push(value.to_string());
		}
	}
	if ignore {
		return None;
	}
	if data_lines.is_empty() {
		return None;
	}
	Some(data_lines.join("\n"))
}

/// TS: `consumeChatStream(model, output, stream, mistralStream)`.
async fn consume_chat_stream(
	model: &Model,
	output: &mut AssistantMessage,
	stream: &AssistantMessageEventStream,
	mistral_stream: &mut MistralChunkStream,
) -> Result<(), MistralStreamError> {
	let mut current_block: Option<CurrentBlock> = None;
	let mut tool_blocks_by_key: IndexMap<String, usize> = IndexMap::new();
	// `partialArgs` is a streaming scratch buffer the TypeScript stores on the block and
	// deletes before the block is persisted; the Rust `ToolCall` has no such field, so the
	// buffer lives beside it, keyed by the block index.
	let mut partial_args_by_index: HashMap<usize, String> = HashMap::new();

	while let Some(chunk) = mistral_stream.next().await? {
		// Mistral's streamed CompletionChunk carries an id field. Keep the first non-empty one,
		// mirroring how OpenAI-style streaming exposes a stable response identifier per stream.
		if output.response_id.is_none() {
			if let Some(id) = chunk.get("id").and_then(Value::as_str) {
				if !id.is_empty() {
					output.response_id = Some(id.to_string());
				}
			}
		}

		if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
			output.usage.input = number_field(usage, "promptTokens");
			output.usage.output = number_field(usage, "completionTokens");
			output.usage.cache_read = 0.0;
			output.usage.cache_write = 0.0;
			let total_tokens = number_field(usage, "totalTokens");
			output.usage.total_tokens = if total_tokens != 0.0 {
				total_tokens
			} else {
				output.usage.input + output.usage.output
			};
			calculate_cost(model, &mut output.usage, None);
		}

		let choice = chunk
			.get("choices")
			.and_then(Value::as_array)
			.and_then(|choices| choices.first());
		let Some(choice) = choice else {
			continue;
		};

		if let Some(finish_reason) = choice.get("finishReason").and_then(Value::as_str) {
			output.stop_reason = map_chat_stop_reason(Some(finish_reason));
			if output.stop_reason == "error" {
				output.stop_reason_raw = Some(finish_reason.to_string());
			}
		}

		let delta = choice.get("delta").cloned().unwrap_or(Value::Object(Map::new()));
		if let Some(content) = delta.get("content").filter(|content| !content.is_null()) {
			let content_items: Vec<Value> = match content {
				Value::String(text) => vec![Value::String(text.clone())],
				Value::Array(items) => items.clone(),
				_ => Vec::new(),
			};
			for item in content_items {
				if let Value::String(text) = &item {
					let text_delta = sanitize_surrogates(text);
					if !matches!(current_block, Some(CurrentBlock::Text(_))) {
						if let Some(block) = current_block.take() {
							finish_current_block(&block, output, stream);
						}
						output.content.push(ContentBlock::Text(TextContent::new(String::new())));
						stream.push(AssistantMessageEvent::TextStart {
							content_index: output.content.len() - 1,
							partial: output.clone(),
						});
						current_block = Some(CurrentBlock::Text(TextContent::new(String::new())));
					}
					if let Some(CurrentBlock::Text(block)) = current_block.as_mut() {
						block.text.push_str(&text_delta);
						if let Some(ContentBlock::Text(target)) = output.content.last_mut() {
							target.text = block.text.clone();
						}
					}
					stream.push(AssistantMessageEvent::TextDelta {
						content_index: output.content.len() - 1,
						delta: text_delta,
						partial: output.clone(),
					});
					continue;
				}

				let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
				if item_type == "thinking" {
					let delta_text = item
						.get("thinking")
						.and_then(Value::as_array)
						.map(|parts| {
							parts
								.iter()
								.map(|part| part.get("text").and_then(Value::as_str).unwrap_or_default())
								.collect::<Vec<_>>()
								.join("")
						})
						.unwrap_or_default();
					let thinking_delta = sanitize_surrogates(&delta_text);
					if thinking_delta.is_empty() {
						continue;
					}
					if !matches!(current_block, Some(CurrentBlock::Thinking(_))) {
						if let Some(block) = current_block.take() {
							finish_current_block(&block, output, stream);
						}
						output
							.content
							.push(ContentBlock::Thinking(ThinkingContent::new(String::new())));
						stream.push(AssistantMessageEvent::ThinkingStart {
							content_index: output.content.len() - 1,
							partial: output.clone(),
						});
						current_block = Some(CurrentBlock::Thinking(ThinkingContent::new(String::new())));
					}
					if let Some(CurrentBlock::Thinking(block)) = current_block.as_mut() {
						block.thinking.push_str(&thinking_delta);
						if let Some(ContentBlock::Thinking(target)) = output.content.last_mut() {
							target.thinking = block.thinking.clone();
						}
					}
					stream.push(AssistantMessageEvent::ThinkingDelta {
						content_index: output.content.len() - 1,
						delta: thinking_delta,
						partial: output.clone(),
					});
					continue;
				}

				if item_type == "text" {
					let text_delta = sanitize_surrogates(item.get("text").and_then(Value::as_str).unwrap_or_default());
					if !matches!(current_block, Some(CurrentBlock::Text(_))) {
						if let Some(block) = current_block.take() {
							finish_current_block(&block, output, stream);
						}
						output.content.push(ContentBlock::Text(TextContent::new(String::new())));
						stream.push(AssistantMessageEvent::TextStart {
							content_index: output.content.len() - 1,
							partial: output.clone(),
						});
						current_block = Some(CurrentBlock::Text(TextContent::new(String::new())));
					}
					if let Some(CurrentBlock::Text(block)) = current_block.as_mut() {
						block.text.push_str(&text_delta);
						if let Some(ContentBlock::Text(target)) = output.content.last_mut() {
							target.text = block.text.clone();
						}
					}
					stream.push(AssistantMessageEvent::TextDelta {
						content_index: output.content.len() - 1,
						delta: text_delta,
						partial: output.clone(),
					});
				}
			}
		}

		let tool_calls = delta
			.get("toolCalls")
			.and_then(Value::as_array)
			.cloned()
			.unwrap_or_default();
		for tool_call in tool_calls {
			if let Some(block) = current_block.take() {
				finish_current_block(&block, output, stream);
			}
			let raw_id = tool_call.get("id").and_then(Value::as_str).unwrap_or_default();
			let call_id = if !raw_id.is_empty() && raw_id != "null" {
				raw_id.to_string()
			} else {
				let index = tool_call.get("index").and_then(Value::as_i64).unwrap_or(0);
				derive_mistral_tool_call_id(&format!("toolcall:{}", index), 0)
			};
			let index = tool_call.get("index").and_then(Value::as_i64).unwrap_or(0);
			let key = format!("{}:{}", call_id, index);
			let mut block_index = tool_blocks_by_key.get(&key).copied();

			if let Some(existing_index) = block_index {
				if !matches!(
					output.content.get(existing_index),
					Some(ContentBlock::ToolCall(_))
				) {
					block_index = None;
				}
			}

			if block_index.is_none() {
				let name = tool_call
					.get("function")
					.and_then(|function| function.get("name"))
					.and_then(Value::as_str)
					.unwrap_or_default()
					.to_string();
				let tool_block = ToolCall::new(call_id.clone(), name, Map::new());
				output.content.push(ContentBlock::ToolCall(tool_block));
				tool_blocks_by_key.insert(key.clone(), output.content.len() - 1);
				partial_args_by_index.insert(output.content.len() - 1, String::new());
				stream.push(AssistantMessageEvent::ToolCallStart {
					content_index: output.content.len() - 1,
					partial: output.clone(),
				});
			}

			let function = tool_call.get("function").cloned().unwrap_or(Value::Object(Map::new()));
			let args_delta = match function.get("arguments") {
				Some(Value::String(arguments)) => arguments.clone(),
				Some(other) => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
				None => serde_json::to_string(&Value::Object(Map::new())).unwrap_or_else(|_| "{}".to_string()),
			};
			let index = tool_blocks_by_key.get(&key).copied().unwrap_or(0);
			let partial_args = format!(
				"{}{}",
				partial_args_by_index.get(&index).cloned().unwrap_or_default(),
				args_delta
			);
			partial_args_by_index.insert(index, partial_args.clone());
			if let Some(ContentBlock::ToolCall(block)) = output.content.get_mut(index) {
				block.arguments = match parse_streaming_json(Some(&partial_args)) {
					Value::Object(map) => map,
					_ => Map::new(),
				};
			}
			stream.push(AssistantMessageEvent::ToolCallDelta {
				content_index: index,
				delta: args_delta,
				partial: output.clone(),
			});
		}
	}

	if let Some(block) = current_block.take() {
		finish_current_block(&block, output, stream);
	}
	for index in tool_blocks_by_key.values().copied().collect::<Vec<usize>>() {
		let Some(ContentBlock::ToolCall(tool_block)) = output.content.get_mut(index) else {
			continue;
		};
		let partial_args = partial_args_by_index.get(&index).cloned().unwrap_or_default();
		tool_block.arguments = match parse_streaming_json(Some(&partial_args)) {
			Value::Object(map) => map,
			_ => Map::new(),
		};
		// Finalize in-place; the scratch buffer is dropped so replay only
		// carries parsed arguments.
		let tool_call = tool_block.clone();
		stream.push(AssistantMessageEvent::ToolCallEnd {
			content_index: index,
			tool_call,
			partial: output.clone(),
		});
	}

	Ok(())
}

/// The block the streaming loop is currently filling.
#[derive(Debug, Clone, PartialEq)]
enum CurrentBlock {
	Text(TextContent),
	Thinking(ThinkingContent),
}

/// TS: `finishCurrentBlock(block?)`.
fn finish_current_block(block: &CurrentBlock, output: &AssistantMessage, stream: &AssistantMessageEventStream) {
	match block {
		CurrentBlock::Text(text) => {
			stream.push(AssistantMessageEvent::TextEnd {
				content_index: output.content.len() - 1,
				content: text.text.clone(),
				partial: output.clone(),
			});
		}
		CurrentBlock::Thinking(thinking) => {
			stream.push(AssistantMessageEvent::ThinkingEnd {
				content_index: output.content.len() - 1,
				content: thinking.thinking.clone(),
				partial: output.clone(),
			});
		}
	}
}

/// TS: `chunk.usage?.promptTokens || 0`.
fn number_field(value: &Value, key: &str) -> f64 {
	value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// Keeps `safeJsonStringify` referenced exactly like the TypeScript module.
pub fn _safe_json_stringify(value: &Value) -> String {
	safe_json_stringify(value)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::types::{
		ImageContent, ImageOrTextContent, InputModality, ModelCost, Tool, ToolResultMessage, UserContent, UserMessage,
	};
	use serde_json::json;

	fn model(id: &str) -> Model {
		let mut model = Model::new(id, id, "mistral-conversations", "mistral", "https://api.mistral.ai");
		model.input = vec![InputModality::Text];
		model.cost = ModelCost::zero();
		model
	}

	fn base_options() -> StreamOptions {
		StreamOptions::default()
	}

	#[test]
	fn options_round_trip_through_serde() {
		let mut options = MistralOptions::from_base(&base_options());
		options.prompt_mode = Some("reasoning".to_string());
		options.reasoning_effort = Some("high".to_string());
		options.tool_choice = Some(json!("required"));
		let value = serde_json::to_value(&options).unwrap();
		assert_eq!(value["promptMode"], json!("reasoning"));
		assert_eq!(value["reasoningEffort"], json!("high"));
		assert_eq!(value["toolChoice"], json!("required"));
		let back: MistralOptions = serde_json::from_value(value).unwrap();
		assert_eq!(back.prompt_mode.as_deref(), Some("reasoning"));
	}

	#[test]
	fn tool_call_id_normalizer_keeps_nine_char_alphanumeric_ids() {
		let normalizer = MistralToolCallIdNormalizer::new();
		assert_eq!(normalizer.normalize("abc123XYZ"), "abc123XYZ");
		// Too short / too long ids are hashed.
		let hashed = normalizer.normalize("short");
		assert_eq!(hashed.len(), MISTRAL_TOOL_CALL_ID_LENGTH);
		assert!(hashed.chars().all(|c| c.is_ascii_alphanumeric()));
		// Repeated calls return the memoised value.
		assert_eq!(normalizer.normalize("short"), hashed);
	}

	#[test]
	fn tool_call_id_normalizer_avoids_collisions_with_an_attempt_suffix() {
		// Both ids normalise to the same 9-char seed; the second must differ.
		let normalizer = MistralToolCallIdNormalizer::new();
		let first = normalizer.normalize("call-1");
		let second = normalizer.normalize("call1");
		assert_ne!(first, second);
		assert_eq!(first.len(), MISTRAL_TOOL_CALL_ID_LENGTH);
		assert_eq!(second.len(), MISTRAL_TOOL_CALL_ID_LENGTH);
	}

	#[test]
	fn derive_tool_call_id_strips_non_alphanumerics() {
		let derived = derive_mistral_tool_call_id("call:1|2", 0);
		assert_eq!(derived.len(), MISTRAL_TOOL_CALL_ID_LENGTH);
		assert!(derived.chars().all(|c| c.is_ascii_alphanumeric()));
		assert_eq!(derive_mistral_tool_call_id("abc123XYZ", 0), "abc123XYZ");
		assert_ne!(derive_mistral_tool_call_id("abcdefghij", 0), derive_mistral_tool_call_id("abcdefghij", 1));
	}

	#[test]
	fn format_mistral_error_matches_typescript_branches() {
		let with_body = MistralStreamError::Api {
			message: "Status 429: Body: slow down".to_string(),
			status_code: Some(429),
			body: Some("  slow down  ".to_string()),
			value: Value::Null,
		};
		assert_eq!(format_mistral_error(&with_body), "Mistral API error (429): slow down");

		let without_body = MistralStreamError::Api {
			message: "Status 500: Body: ".to_string(),
			status_code: Some(500),
			body: Some("   ".to_string()),
			value: Value::Null,
		};
		assert_eq!(
			format_mistral_error(&without_body),
			"Mistral API error (500): Status 500: Body: "
		);

		let plain = MistralStreamError::Message("boom".to_string());
		assert_eq!(format_mistral_error(&plain), "boom");
	}

	#[test]
	fn truncate_error_text_appends_the_truncation_note() {
		assert_eq!(truncate_error_text("abc", 10), "abc");
		assert_eq!(truncate_error_text("abcdef", 3), "abc... [truncated 3 chars]");
	}

	#[test]
	fn map_chat_stop_reason_follows_the_typescript_switch() {
		assert_eq!(map_chat_stop_reason(None), "stop");
		assert_eq!(map_chat_stop_reason(Some("stop")), "stop");
		assert_eq!(map_chat_stop_reason(Some("length")), "length");
		assert_eq!(map_chat_stop_reason(Some("model_length")), "length");
		assert_eq!(map_chat_stop_reason(Some("tool_calls")), "toolUse");
		assert_eq!(map_chat_stop_reason(Some("error")), "error");
		assert_eq!(map_chat_stop_reason(Some("wat")), "stop");
	}

	#[test]
	fn reasoning_routing_helpers_match_typescript() {
		assert!(uses_reasoning_effort(&model("mistral-small-2603")));
		assert!(uses_reasoning_effort(&model("mistral-small-latest")));
		assert!(uses_reasoning_effort(&model("mistral-medium-3.5")));
		assert!(!uses_reasoning_effort(&model("mistral-large-latest")));

		let mut reasoning_model = model("mistral-large-latest");
		reasoning_model.reasoning = true;
		assert!(uses_prompt_mode_reasoning(&reasoning_model));
		let mut effort_model = model("mistral-small-latest");
		effort_model.reasoning = true;
		assert!(!uses_prompt_mode_reasoning(&effort_model));
	}

	#[test]
	fn map_reasoning_effort_prefers_the_thinking_level_map() {
		let mut model = model("mistral-small-latest");
		model.thinking_level_map = Some(
			[("high".to_string(), Some("none".to_string()))]
				.into_iter()
				.collect(),
		);
		assert_eq!(map_reasoning_effort(&model, "high"), "none");
		assert_eq!(map_reasoning_effort(&model, "low"), "high");
	}

	#[test]
	fn map_tool_choice_passes_strings_and_normalises_objects() {
		assert_eq!(map_tool_choice(None), None);
		assert_eq!(map_tool_choice(Some(&json!("auto"))), Some(json!("auto")));
		assert_eq!(map_tool_choice(Some(&json!("required"))), Some(json!("required")));
		assert_eq!(map_tool_choice(Some(&json!("bogus"))), None);
		assert_eq!(
			map_tool_choice(Some(&json!({"type": "function", "function": {"name": "read"}}))),
			Some(json!({"type": "function", "function": {"name": "read"}}))
		);
	}

	#[test]
	fn build_chat_payload_uses_snake_case_wire_keys() {
		let model = model("mistral-large-latest");
		let context = Context::new(
			Some("be nice".to_string()),
			vec![Message::user(UserMessage::new(UserContent::Text("hi".to_string()), 0))],
			Some(vec![Tool {
				name: "read".to_string(),
				description: "Read".to_string(),
				parameters: json!({"type": "object"}),
			}]),
		);
		let mut options = MistralOptions::from_base(&base_options());
		options.stream.temperature = Some(0.3);
		options.stream.max_tokens = Some(128.0);
		options.tool_choice = Some(json!("any"));
		options.prompt_mode = Some("reasoning".to_string());
		options.reasoning_effort = Some("high".to_string());
		let payload = build_chat_payload(&model, &context, &context.messages, Some(&options));
		assert_eq!(
			Value::Object(payload),
			json!({
				"model": "mistral-large-latest",
				"stream": true,
				"messages": [
					{"role": "system", "content": "be nice"},
					{"role": "user", "content": "hi"}
				],
				"tools": [{
					"type": "function",
					"function": {
						"name": "read",
						"description": "Read",
						"parameters": {"type": "object"},
						"strict": false
					}
				}],
				"temperature": 0.3,
				"max_tokens": 128.0,
				"tool_choice": "any",
				"prompt_mode": "reasoning",
				"reasoning_effort": "high"
			})
		);
	}

	#[test]
	fn build_chat_payload_omits_optional_fields() {
		let model = model("mistral-large-latest");
		let context = Context::new(
			None,
			vec![Message::user(UserMessage::new(UserContent::Text("hi".to_string()), 0))],
			None,
		);
		let payload = build_chat_payload(&model, &context, &context.messages, None);
		assert_eq!(
			Value::Object(payload),
			json!({
				"model": "mistral-large-latest",
				"stream": true,
				"messages": [{"role": "user", "content": "hi"}]
			})
		);
	}

	#[test]
	fn to_chat_messages_omits_images_for_text_only_models() {
		let messages = vec![Message::user(UserMessage::new(
			UserContent::Blocks(vec![ImageOrTextContent::Image(ImageContent::new("AAAA", "image/png"))]),
			0,
		))];
		assert_eq!(
			to_chat_messages(&messages, false),
			vec![json!({"role": "user", "content": "(image omitted: model does not support images)"})]
		);

		let messages = vec![Message::user(UserMessage::new(
			UserContent::Blocks(vec![
				ImageOrTextContent::Text(TextContent::new("look")),
				ImageOrTextContent::Image(ImageContent::new("AAAA", "image/png")),
			]),
			0,
		))];
		assert_eq!(
			to_chat_messages(&messages, true),
			vec![json!({
				"role": "user",
				"content": [
					{"type": "text", "text": "look"},
					{"type": "image_url", "image_url": "data:image/png;base64,AAAA"}
				]
			})]
		);
	}

	#[test]
	fn to_chat_messages_maps_assistant_blocks_and_tool_results() {
		let mut assistant = AssistantMessage::default();
		assistant.content = vec![
			ContentBlock::Text(TextContent::new("answer")),
			ContentBlock::Thinking(ThinkingContent::new("hmm")),
			ContentBlock::ToolCall(ToolCall::new("call-1", "read", json!({"path": "a"}).as_object().unwrap().clone())),
		];
		let messages = vec![
			Message::assistant(assistant),
			Message::tool_result(ToolResultMessage::new(
				"call-1",
				"read",
				vec![ImageOrTextContent::Text(TextContent::new("ok"))],
				false,
				0,
			)),
			Message::tool_result(ToolResultMessage::new(
				"call-2",
				"read",
				vec![ImageOrTextContent::Text(TextContent::new("bad"))],
				true,
				0,
			)),
		];
		assert_eq!(
			to_chat_messages(&messages, false),
			vec![
				json!({
					"role": "assistant",
					"content": [
						{"type": "text", "text": "answer"},
						{"type": "thinking", "thinking": [{"type": "text", "text": "hmm"}]}
					],
					"tool_calls": [{
						"id": "call-1",
						"type": "function",
						"function": {"name": "read", "arguments": "{\"path\":\"a\"}"}
					}]
				}),
				json!({"role": "tool", "tool_call_id": "call-1", "name": "read", "content": [{"type": "text", "text": "ok"}]}),
				json!({"role": "tool", "tool_call_id": "call-2", "name": "read", "content": [{"type": "text", "text": "[tool error] bad"}]})
			]
		);
	}

	#[test]
	fn build_tool_result_text_matches_typescript_branches() {
		assert_eq!(build_tool_result_text("out", false, true, false), "out");
		assert_eq!(build_tool_result_text("  out  ", false, true, true), "[tool error] out");
		assert_eq!(
			build_tool_result_text("out", true, false, false),
			"out\n[tool image omitted: model does not support images]"
		);
		assert_eq!(build_tool_result_text("", true, true, false), "(see attached image)");
		assert_eq!(build_tool_result_text("", true, true, true), "[tool error] (see attached image)");
		assert_eq!(
			build_tool_result_text("", true, false, false),
			"(image omitted: model does not support images)"
		);
		assert_eq!(
			build_tool_result_text("", true, false, true),
			"[tool error] (image omitted: model does not support images)"
		);
		assert_eq!(build_tool_result_text("", false, true, false), "(no tool output)");
		assert_eq!(build_tool_result_text("", false, true, true), "[tool error] (no tool output)");
	}

	#[test]
	fn request_options_merge_headers_and_add_affinity() {
		let mut model = model("mistral-large-latest");
		let mut model_headers = IndexMap::new();
		model_headers.insert("x-model".to_string(), "1".to_string());
		model.headers = Some(model_headers);
		let mut options = MistralOptions::from_base(&base_options());
		let mut options_headers = IndexMap::new();
		options_headers.insert("x-option".to_string(), "2".to_string());
		options.stream.headers = Some(options_headers);
		options.stream.session_id = Some("session-1".to_string());
		let request_options = build_request_options(&model, &options);
		assert!(request_options.retries_strategy_none);
		assert_eq!(request_options.headers.get("x-model").map(String::as_str), Some("1"));
		assert_eq!(request_options.headers.get("x-option").map(String::as_str), Some("2"));
		assert_eq!(request_options.headers.get("x-affinity").map(String::as_str), Some("session-1"));

		let mut explicit = MistralOptions::from_base(&base_options());
		let mut explicit_headers = IndexMap::new();
		explicit_headers.insert("x-affinity".to_string(), "caller".to_string());
		explicit.stream.headers = Some(explicit_headers);
		explicit.stream.session_id = Some("session-1".to_string());
		let request_options = build_request_options(&model, &explicit);
		assert_eq!(request_options.headers.get("x-affinity").map(String::as_str), Some("caller"));
	}

	#[test]
	fn sse_parser_joins_data_lines_and_stops_at_done() {
		assert_eq!(parse_sse_message("data: a\ndata: b"), Some("a\nb".to_string()));
		assert_eq!(parse_sse_message(":comment\n\ndata:x"), Some("x".to_string()));
		assert_eq!(parse_sse_message(":only-comment"), None);
		assert_eq!(parse_sse_message("event: ping"), None);
	}

	#[test]
	fn chunk_stream_parses_events_and_honours_done() {
		let mut stream = MistralChunkStream {
			chunks: Box::pin(futures::stream::empty()),
			buffer: "data: {\"id\": \"a\"}\n\ndata: [DONE]\n\ndata: {\"id\": \"b\"}\n\n".to_string(),
			pending: Vec::new(),
			done: false,
			finished: false,
			signal: None,
		};
		stream.drain_events().unwrap();
		assert_eq!(stream.pending, vec![json!({"id": "a"})]);
		assert!(stream.done);
	}

	#[test]
	fn chunk_stream_reports_malformed_json() {
		let mut stream = MistralChunkStream {
			chunks: Box::pin(futures::stream::empty()),
			buffer: "data: {oops}\n\n".to_string(),
			pending: Vec::new(),
			done: false,
			finished: false,
			signal: None,
		};
		let error = stream.drain_events().unwrap_err();
		assert!(error.error_message().starts_with("malformed json: "));
	}

	#[test]
	fn number_field_defaults_to_zero() {
		assert_eq!(number_field(&json!({"promptTokens": 5}), "promptTokens"), 5.0);
		assert_eq!(number_field(&json!({}), "promptTokens"), 0.0);
	}
}
