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

/// The pinned `@mistralai/mistralai` version, used for the SDK user agent.
const MISTRAL_SDK_VERSION: &str = "2.2.1";
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
				for block in output.content.iter_mut() {
					// partialArgs is only a streaming scratch buffer; never persist it.
					if let ContentBlock::ToolCall(tool_call) = block {
						tool_call.partial_args = None;
					}
				}
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

	rename_chat_payload_keys(&mut payload)
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
								let mut chunk = Map::new();
								chunk.insert("type".to_string(), Value::String("image_url".to_string()));
								chunk.insert(
									"imageUrl".to_string(),
									Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
								);
								Value::Object(chunk)
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
					let mut image_chunk = Map::new();
					image_chunk.insert("type".to_string(), Value::String("image_url".to_string()));
					image_chunk.insert(
						"imageUrl".to_string(),
						Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
					);
					tool_content.push(Value::Object(image_chunk));
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
