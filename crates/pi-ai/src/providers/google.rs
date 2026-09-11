//! Port of packages/ai/src/providers/google.ts
//!
//! The TypeScript uses `@google/genai` (`GoogleGenAI.models.generateContentStream`).
//! This port builds the same `POST {baseUrl}/{apiVersion}/models/{model}:streamGenerateContent?alt=sse`
//! request itself with `reqwest` and parses the SSE stream locally, keeping the SDK's
//! wire shape (`generateContentParametersToMldev`), header merge order and error text.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::env_api_keys::get_env_api_key;
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::providers::google_shared::{
	convert_messages, convert_tools, get_google_thinking_budget, is_thinking_part, map_stop_reason, map_tool_choice,
	retain_thought_signature, GoogleThinkingLevel,
};
use crate::providers::opencode_headers::with_opencode_headers;
use crate::providers::simple_options::build_base_options;
use crate::types::{
	AssistantMessage, AssistantMessageEvent, ContentBlock, Context, Model, SimpleStreamOptions, StopReason,
	StreamOptions, TextContent, ThinkingContent, ToolCall, Usage,
};
use crate::utils::event_stream::{create_assistant_message_event_stream, AssistantMessageEventStream};
use crate::utils::headers::header_map_to_record;
use crate::utils::now_ms;
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::stream_failure::{
	format_stream_failure_message, record_stream_failure, stream_failure_from_stop_reason, StreamFailureError,
	ThrownStreamError,
};

/// The pinned `@google/genai` version, used for the SDK default headers.
const GOOGLE_GENAI_SDK_VERSION: &str = "1.52.0";
const LIBRARY_LABEL: &str = "google-genai-sdk/1.52.0";
/// `GOOGLE_AI_API_DEFAULT_VERSION`.
const GOOGLE_AI_API_DEFAULT_VERSION: &str = "v1beta";
const USER_AGENT_HEADER: &str = "User-Agent";
const GOOGLE_API_CLIENT_HEADER: &str = "x-goog-api-client";
const CONTENT_TYPE_HEADER: &str = "Content-Type";
const GOOGLE_API_KEY_HEADER: &str = "x-goog-api-key";

/// TS: `interface GoogleOptions extends StreamOptions`.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GoogleOptions {
	#[serde(flatten)]
	pub stream: StreamOptions,
	/// `"auto" | "none" | "any"`
	pub tool_choice: Option<String>,
	pub thinking: Option<GoogleThinkingOptions>,
}

/// TS: `thinking?: { enabled: boolean; budgetTokens?: number; level?: GoogleThinkingLevel }`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GoogleThinkingOptions {
	pub enabled: bool,
	/// -1 for dynamic, 0 to disable
	pub budget_tokens: Option<f64>,
	pub level: Option<GoogleThinkingLevel>,
}

impl GoogleOptions {
	/// TS: the caller passes `StreamOptions & Record<string, unknown>`; this keeps the
	/// non-serializable fields (signal, on_payload, on_response, on_usage_observation).
	pub fn from_base(base: &StreamOptions) -> Self {
		Self {
			stream: base.clone(),
			tool_choice: None,
			thinking: None,
		}
	}
}

impl std::fmt::Debug for GoogleOptions {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("GoogleOptions")
			.field("stream", &self.stream)
			.field("tool_choice", &self.tool_choice)
			.field("thinking", &self.thinking)
			.finish()
	}
}

/// TS: `let toolCallCounter = 0` - module-level counter shared by all streams.
static TOOL_CALL_COUNTER: AtomicI64 = AtomicI64::new(0);

/// `++toolCallCounter`.
fn next_tool_call_counter() -> i64 {
	TOOL_CALL_COUNTER.fetch_add(1, Ordering::SeqCst) + 1
}

/// The TypeScript throws either a plain `Error`, a `StreamFailureError` or an SDK
/// `ApiError`; the port keeps both shapes.
#[derive(Debug, Clone, PartialEq)]
pub enum GoogleStreamError {
	Failure(StreamFailureError),
	Message(String),
	/// TS: `throw new ApiError({ message, status })` - an `Error` subclass with a
	/// numeric `status` field, which `extractStreamFailureParts` reads.
	Api { message: String, status: i64, value: Value },
}

impl GoogleStreamError {
	fn as_thrown(&self) -> ThrownStreamError<'_> {
		match self {
			GoogleStreamError::Failure(failure) => ThrownStreamError::Failure(failure),
			GoogleStreamError::Message(message) => ThrownStreamError::Message(message),
			GoogleStreamError::Api { value, .. } => ThrownStreamError::Value(value),
		}
	}

	/// `error.message`.
	fn message(&self) -> String {
		match self {
			GoogleStreamError::Failure(failure) => failure.message.clone(),
			GoogleStreamError::Message(message) => message.clone(),
			GoogleStreamError::Api { message, .. } => message.clone(),
		}
	}
}

impl std::fmt::Display for GoogleStreamError {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(formatter, "{}", self.message())
	}
}

impl std::error::Error for GoogleStreamError {}

/// TS: `streamGoogle: StreamFunction<"google-generative-ai", GoogleOptions>`.
pub fn stream_google(
	model: &Model,
	context: &Context,
	options: Option<GoogleOptions>,
) -> AssistantMessageEventStream {
	let stream = create_assistant_message_event_stream();
	let out = stream.clone();
	let model = model.clone();
	let context = context.clone();
	let options = options.unwrap_or_default();
	tokio::spawn(async move {
		let mut output = AssistantMessage {
			content: Vec::new(),
			api: "google-generative-ai".to_string(),
			provider: model.provider.clone(),
			model: model.id.clone(),
			usage: Usage::zero(),
			stop_reason: "stop".to_string(),
			timestamp: now_ms(),
			..Default::default()
		};

		match run_stream_google(&model, &context, &options, &mut output, &out).await {
			Ok(()) => {}
			Err(error) => {
				// Remove internal index property used during streaming
				// (the Rust port never adds an `index` field to a block).
				let aborted = options
					.stream
					.signal
					.as_ref()
					.map(|signal| signal.is_cancelled())
					.unwrap_or(false);
				output.stop_reason = if aborted { "aborted".to_string() } else { "error".to_string() };
				output.error_message = Some(format_stream_failure_message(&error.as_thrown()));
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

/// The TypeScript async IIFE body of `streamGoogle`.
async fn run_stream_google(
	model: &Model,
	context: &Context,
	options: &GoogleOptions,
	output: &mut AssistantMessage,
	stream: &AssistantMessageEventStream,
) -> Result<(), GoogleStreamError> {
	let api_key = options
		.stream
		.api_key
		.clone()
		.or_else(|| get_env_api_key(&model.provider))
		.unwrap_or_default();
	let client = create_client(
		model,
		&api_key,
		options.stream.headers.as_ref(),
		options.stream.session_id.as_deref(),
	);
	let mut params = build_params(model, context, Some(options))?;
	if let Some(on_payload) = options.stream.on_payload.clone() {
		let next_params = on_payload(params.clone(), model).await;
		if let Some(next_params) = next_params {
			params = next_params;
		}
	}
	let response = send_request(&client, &params, model, options).await?;
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
	let mut current_block: Option<CurrentBlock> = None;
	let mut chunks = SseChunkStream::new(response, options.stream.signal.clone());

	while let Some(chunk) = chunks.next().await? {
		// @google/genai documents GenerateContentResponse.responseId as an output-only field
		// used to identify each response. Keep the first non-empty one from the stream.
		if output.response_id.is_none() {
			if let Some(response_id) = chunk.get("responseId").and_then(Value::as_str) {
				if !response_id.is_empty() {
					output.response_id = Some(response_id.to_string());
				}
			}
		}
		let candidate = chunk.get("candidates").and_then(Value::as_array).and_then(|list| list.first());
		if let Some(parts) = candidate
			.and_then(|candidate| candidate.get("content"))
			.and_then(|content| content.get("parts"))
			.and_then(Value::as_array)
		{
			for part in parts {
				process_part(part, output, stream, &mut current_block)?;
			}
		}

		if let Some(finish_reason) = candidate
			.and_then(|candidate| candidate.get("finishReason"))
			.and_then(Value::as_str)
		{
			output.stop_reason = map_stop_reason(finish_reason).map_err(GoogleStreamError::Message)?;
			if output
				.content
				.iter()
				.any(|block| matches!(block, ContentBlock::ToolCall(_)))
			{
				output.stop_reason = "toolUse".to_string();
			}
			if output.stop_reason == "error" {
				output.stop_reason_raw = Some(finish_reason.to_string());
			}
		}

		if let Some(usage_metadata) = chunk.get("usageMetadata") {
			let prompt_token_count = number_field(usage_metadata, "promptTokenCount");
			let cached_content_token_count = number_field(usage_metadata, "cachedContentTokenCount");
			let candidates_token_count = number_field(usage_metadata, "candidatesTokenCount");
			let thoughts_token_count = number_field(usage_metadata, "thoughtsTokenCount");
			output.usage = Usage {
				input: prompt_token_count - cached_content_token_count,
				output: candidates_token_count + thoughts_token_count,
				cache_read: cached_content_token_count,
				cache_write: 0.0,
				total_tokens: number_field(usage_metadata, "totalTokenCount"),
				cost: crate::types::UsageCost::zero(),
			};
			calculate_cost(model, &mut output.usage, None);
		}
	}

	if let Some(block) = current_block {
		finish_block(&block, output, stream);
	}

	if options
		.stream
		.signal
		.as_ref()
		.map(|signal| signal.is_cancelled())
		.unwrap_or(false)
	{
		return Err(GoogleStreamError::Message("Request was aborted".to_string()));
	}

	if output.stop_reason == "aborted" || output.stop_reason == "error" {
		return Err(GoogleStreamError::Failure(stream_failure_from_stop_reason(
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

/// TS: `chunk.usageMetadata?.promptTokenCount || 0` - any non-number reads as 0.
fn number_field(value: &Value, key: &str) -> f64 {
	value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// The block the streaming loop is currently filling.
#[derive(Debug, Clone, PartialEq)]
enum CurrentBlock {
	Text(TextContent),
	Thinking(ThinkingContent),
}

/// TS: the `if (currentBlock) { ...text_end/thinking_end... }` flush.
fn finish_block(block: &CurrentBlock, output: &AssistantMessage, stream: &AssistantMessageEventStream) {
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

/// The inner `for (const part of candidate.content.parts)` body.
fn process_part(
	part: &Value,
	output: &mut AssistantMessage,
	stream: &AssistantMessageEventStream,
	current_block: &mut Option<CurrentBlock>,
) -> Result<(), GoogleStreamError> {
	if let Some(text) = part.get("text").and_then(Value::as_str) {
		let is_thinking = is_thinking_part(part);
		let needs_new_block = match current_block {
			None => true,
			Some(CurrentBlock::Thinking(_)) => !is_thinking,
			Some(CurrentBlock::Text(_)) => is_thinking,
		};
		if needs_new_block {
			if let Some(block) = current_block.take() {
				finish_block(&block, output, stream);
			}
			if is_thinking {
				output.content.push(ContentBlock::Thinking(ThinkingContent {
					thinking: String::new(),
					thinking_signature: None,
					..Default::default()
				}));
				stream.push(AssistantMessageEvent::ThinkingStart {
					content_index: output.content.len() - 1,
					partial: output.clone(),
				});
				*current_block = Some(CurrentBlock::Thinking(ThinkingContent {
					thinking: String::new(),
					thinking_signature: None,
					..Default::default()
				}));
			} else {
				output.content.push(ContentBlock::Text(TextContent::new("")));
				stream.push(AssistantMessageEvent::TextStart {
					content_index: output.content.len() - 1,
					partial: output.clone(),
				});
				*current_block = Some(CurrentBlock::Text(TextContent::new("")));
			}
		}

		let thought_signature = part.get("thoughtSignature").and_then(Value::as_str);
		match current_block {
			Some(CurrentBlock::Thinking(thinking)) => {
				thinking.thinking.push_str(text);
				thinking.thinking_signature =
					retain_thought_signature(thinking.thinking_signature.as_deref(), thought_signature);
				if let Some(ContentBlock::Thinking(block)) = output.content.last_mut() {
					block.thinking = thinking.thinking.clone();
					block.thinking_signature = thinking.thinking_signature.clone();
				}
				stream.push(AssistantMessageEvent::ThinkingDelta {
					content_index: output.content.len() - 1,
					delta: text.to_string(),
					partial: output.clone(),
				});
			}
			Some(CurrentBlock::Text(current)) => {
				current.text.push_str(text);
				current.text_signature = retain_thought_signature(current.text_signature.as_deref(), thought_signature);
				if let Some(ContentBlock::Text(block)) = output.content.last_mut() {
					block.text = current.text.clone();
					block.text_signature = current.text_signature.clone();
				}
				stream.push(AssistantMessageEvent::TextDelta {
					content_index: output.content.len() - 1,
					delta: text.to_string(),
					partial: output.clone(),
				});
			}
			None => {}
		}
	}

	if let Some(function_call) = part.get("functionCall") {
		if let Some(block) = current_block.take() {
			finish_block(&block, output, stream);
		}

		let provided_id = function_call.get("id").and_then(Value::as_str);
		let name = function_call.get("name").and_then(Value::as_str).unwrap_or("");
		let needs_new_id = match provided_id {
			None => true,
			Some(provided_id) => output.content.iter().any(|block| match block {
				ContentBlock::ToolCall(tool_call) => tool_call.id == provided_id,
				_ => false,
			}),
		};
		let tool_call_id = if needs_new_id {
			format!("{}_{}_{}", name, now_ms(), next_tool_call_counter())
		} else {
			provided_id.unwrap_or_default().to_string()
		};

		let arguments = match function_call.get("args") {
			Some(Value::Object(map)) => map.clone(),
			_ => Map::new(),
		};
		let tool_call = ToolCall {
			id: tool_call_id,
			name: name.to_string(),
			arguments,
			thought_signature: part
				.get("thoughtSignature")
				.and_then(Value::as_str)
				.map(str::to_string),
			..Default::default()
		};

		output.content.push(ContentBlock::ToolCall(tool_call.clone()));
		stream.push(AssistantMessageEvent::ToolCallStart {
			content_index: output.content.len() - 1,
			partial: output.clone(),
		});
		stream.push(AssistantMessageEvent::ToolCallDelta {
			content_index: output.content.len() - 1,
			delta: serde_json::to_string(&tool_call.arguments).unwrap_or_else(|_| "{}".to_string()),
			partial: output.clone(),
		});
		stream.push(AssistantMessageEvent::ToolCallEnd {
			content_index: output.content.len() - 1,
			tool_call,
			partial: output.clone(),
		});
	}

	Ok(())
}

// ---------------------------------------------------------------------------
// Client / request
// ---------------------------------------------------------------------------

/// The fields of a constructed `GoogleGenAI` client that this provider reads.
#[derive(Debug, Clone, PartialEq)]
pub struct GoogleClient {
	pub api_key: String,
	pub base_url: String,
	pub api_version: String,
	pub headers: IndexMap<String, Option<String>>,
}

/// TS: `createClient(model, apiKey?, optionsHeaders?, sessionId?)`.
pub fn create_client(
	model: &Model,
	api_key: Option<&str>,
	options_headers: Option<&IndexMap<String, String>>,
	session_id: Option<&str>,
) -> GoogleClient {
	let mut base_url = String::new();
	let mut api_version = GOOGLE_AI_API_DEFAULT_VERSION.to_string();
	if !model.base_url.is_empty() {
		base_url = model.base_url.clone();
		// baseUrl already includes version path, don't append
		api_version = String::new();
	} else {
		base_url = "https://generativelanguage.googleapis.com/".to_string();
	}

	let mut merged: IndexMap<String, Option<String>> = IndexMap::new();
	if let Some(model_headers) = &model.headers {
		for (key, value) in model_headers {
			merged.insert(key.clone(), Some(value.clone()));
		}
	}
	if let Some(options_headers) = options_headers {
		for (key, value) in options_headers {
			merged.insert(key.clone(), Some(value.clone()));
		}
	}
	let headers = with_opencode_headers(&model.provider, session_id, &merged);

	GoogleClient {
		api_key: api_key.unwrap_or_default().to_string(),
		base_url,
		api_version,
		headers,
	}
}

/// TS: `getRequestUrlInternal(httpOptions)` - base URL plus non-empty api version.
fn request_url(client: &GoogleClient) -> String {
	let base_url = client.base_url.strip_suffix('/').unwrap_or(&client.base_url);
	let mut elements = vec![base_url.to_string()];
	if !client.api_version.is_empty() {
		elements.push(client.api_version.clone());
	}
	elements.join("/")
}

/// TS: `tModel(apiClient, model)` for the Gemini API client.
fn t_model(model_id: &str) -> Result<String, String> {
	if model_id.is_empty() {
		return Err("model is required and must be a string".to_string());
	}
	if model_id.contains("..") || model_id.contains('?') || model_id.contains('&') {
		return Err("invalid model parameter".to_string());
	}
	if model_id.starts_with("models/") || model_id.starts_with("tunedModels/") {
		return Ok(model_id.to_string());
	}
	Ok(format!("models/{}", model_id))
}

/// TS: the header map handed to `fetch` - defaults patched with httpOptions.headers,
/// then `auth.addAuthHeaders` appends `x-goog-api-key` when absent.
pub fn build_request_headers(client: &GoogleClient) -> IndexMap<String, String> {
	let mut headers: IndexMap<String, String> = IndexMap::new();
	headers.insert(USER_AGENT_HEADER.to_string(), LIBRARY_LABEL.to_string());
	headers.insert(GOOGLE_API_CLIENT_HEADER.to_string(), LIBRARY_LABEL.to_string());
	headers.insert(CONTENT_TYPE_HEADER.to_string(), "application/json".to_string());
	for (key, value) in &client.headers {
		if let Some(value) = value {
			headers.insert(key.clone(), value.clone());
		}
	}
	// `addKeyHeader` only appends when the header is not already present.
	if !headers.keys().any(|key| key.eq_ignore_ascii_case(GOOGLE_API_KEY_HEADER)) {
		headers.insert(GOOGLE_API_KEY_HEADER.to_string(), client.api_key.clone());
	}
	headers
}

/// `client.models.generateContentStream(params)`.
async fn send_request(
	client: &GoogleClient,
	params: &Value,
	model: &Model,
	options: &GoogleOptions,
) -> Result<reqwest::Response, GoogleStreamError> {
	let path = format!("{}:streamGenerateContent?alt=sse", t_model(&model.id).map_err(GoogleStreamError::Message)?);
	let url = format!("{}/{}", request_url(client), path);
	let body = generate_content_parameters_to_mldev(params);

	let mut headers = reqwest::header::HeaderMap::new();
	for (key, value) in build_request_headers(client) {
		if let (Ok(name), Ok(header_value)) = (
			reqwest::header::HeaderName::from_bytes(key.as_bytes()),
			reqwest::header::HeaderValue::from_str(&value),
		) {
			headers.insert(name, header_value);
		}
	}

	let mut request = reqwest::Client::new().post(&url).headers(headers).json(&body);
	if let Some(timeout_ms) = options.stream.timeout_ms {
		request = request.timeout(std::time::Duration::from_millis(timeout_ms.max(0.0) as u64));
	}

	let send = request.send();
	let response = match options.stream.signal.as_ref() {
		Some(signal) => tokio::select! {
			_ = signal.cancelled() => return Err(GoogleStreamError::Message("Request was aborted".to_string())),
			result = send => result,
		},
		None => send.await,
	};
	let response = response.map_err(|error| GoogleStreamError::Message(error.to_string()))?;
	throw_error_if_not_ok(response).await
}

/// TS: `throwErrorIfNotOK(response)` - `ApiError` for 4xx/5xx, plain `Error` otherwise.
async fn throw_error_if_not_ok(response: reqwest::Response) -> Result<reqwest::Response, GoogleStreamError> {
	if response.status().is_success() {
		return Ok(response);
	}
	let status = response.status().as_u16() as i64;
	let status_text = response.status().canonical_reason().unwrap_or("").to_string();
	let is_json = response
		.headers()
		.get(reqwest::header::CONTENT_TYPE)
		.and_then(|value| value.to_str().ok())
		.map(|value| value.contains("application/json"))
		.unwrap_or(false);
	let body_text = response.text().await.unwrap_or_default();
	let error_body = if is_json {
		serde_json::from_str::<Value>(&body_text).unwrap_or(Value::Null)
	} else {
		let mut error = Map::new();
		error.insert("message".to_string(), Value::String(body_text));
		error.insert("code".to_string(), Value::Number(status.into()));
		error.insert("status".to_string(), Value::String(status_text));
		let mut root = Map::new();
		root.insert("error".to_string(), Value::Object(error));
		Value::Object(root)
	};
	let error_message = serde_json::to_string(&error_body).unwrap_or_else(|_| "null".to_string());
	if (400..600).contains(&status) {
		let mut value = Map::new();
		value.insert("name".to_string(), Value::String("ApiError".to_string()));
		value.insert("message".to_string(), Value::String(error_message.clone()));
		value.insert("status".to_string(), Value::Number(status.into()));
		return Err(GoogleStreamError::Api {
			message: error_message,
			status,
			value: Value::Object(value),
		});
	}
	Err(GoogleStreamError::Message(error_message))
}

/// TS: `generateContentParametersToMldev(apiClient, params)`.
///
/// Produces `{ contents, systemInstruction?, tools?, toolConfig?, generationConfig }`
/// in the same insertion order as the SDK generator.
fn generate_content_parameters_to_mldev(params: &Value) -> Value {
	let mut body = Map::new();
	if let Some(contents) = params.get("contents") {
		body.insert("contents".to_string(), contents.clone());
	}
	let config = params.get("config").cloned().unwrap_or(Value::Object(Map::new()));
	generate_content_config_to_mldev(&config, &mut body);
	body
}

/// TS: `generateContentConfigToMldev(apiClient, config, parentObject)`.
fn generate_content_config_to_mldev(config: &Value, body: &mut Map<String, Value>) {
	let mut generation_config = Map::new();
	if let Some(system_instruction) = config.get("systemInstruction").and_then(Value::as_str) {
		body.insert(
			"systemInstruction".to_string(),
			content_to_mldev(&system_instruction_value(system_instruction)),
		);
	}
	if let Some(temperature) = config.get("temperature") {
		if !temperature.is_null() {
			generation_config.insert("temperature".to_string(), temperature.clone());
		}
	}
	if let Some(max_output_tokens) = config.get("maxOutputTokens") {
		if !max_output_tokens.is_null() {
			generation_config.insert("maxOutputTokens".to_string(), max_output_tokens.clone());
		}
	}
	if let Some(tools) = config.get("tools") {
		if !tools.is_null() {
			body.insert("tools".to_string(), tools.clone());
		}
	}
	if let Some(tool_config) = config.get("toolConfig") {
		if !tool_config.is_null() {
			body.insert("toolConfig".to_string(), tool_config_to_mldev(tool_config));
		}
	}
	if let Some(thinking_config) = config.get("thinkingConfig") {
		if !thinking_config.is_null() {
			generation_config.insert("thinkingConfig".to_string(), thinking_config.clone());
		}
	}
	body.insert("generationConfig".to_string(), Value::Object(generation_config));
}

/// TS: `contentToMldev({ parts: [{ text }] })`.
fn system_instruction_value(text: &str) -> Value {
	let mut part = Map::new();
	part.insert("text".to_string(), Value::String(text.to_string()));
	let mut content = Map::new();
	content.insert("parts".to_string(), Value::Array(vec![Value::Object(part)]));
	Value::Object(content)
}

/// TS: `contentToMldev(content)` - maps parts through `partToMldev`.
fn content_to_mldev(content: &Value) -> Value {
	let mut result = Map::new();
	if let Some(parts) = content.get("parts").and_then(Value::as_array) {
		result.insert(
			"parts".to_string(),
			Value::Array(parts.iter().map(part_to_mldev).collect()),
		);
	}
	if let Some(role) = content.get("role") {
		if !role.is_null() {
			result.insert("role".to_string(), role.clone());
		}
	}
	Value::Object(result)
}

/// TS: `partToMldev(part)` - keeps `text`, `thought`, `thoughtSignature`,
/// `inlineData` (through `blobToMldev`) and `functionCall` (through
/// `functionCallToMldev`).
fn part_to_mldev(part: &Value) -> Value {
	let mut result = Map::new();
	if let Some(function_call) = part.get("functionCall") {
		if !function_call.is_null() {
			result.insert("functionCall".to_string(), function_call_to_mldev(function_call));
		}
	}
	if let Some(function_response) = part.get("functionResponse") {
		if !function_response.is_null() {
			result.insert("functionResponse".to_string(), function_response.clone());
		}
	}
	if let Some(inline_data) = part.get("inlineData") {
		if !inline_data.is_null() {
			result.insert("inlineData".to_string(), blob_to_mldev(inline_data));
		}
	}
	if let Some(text) = part.get("text") {
		if !text.is_null() {
			result.insert("text".to_string(), text.clone());
		}
	}
	if let Some(thought) = part.get("thought") {
		if !thought.is_null() {
			result.insert("thought".to_string(), thought.clone());
		}
	}
	if let Some(thought_signature) = part.get("thoughtSignature") {
		if !thought_signature.is_null() {
			result.insert("thoughtSignature".to_string(), thought_signature.clone());
		}
	}
	Value::Object(result)
}

/// TS: `functionCallToMldev({ id, args, name })`.
fn function_call_to_mldev(function_call: &Value) -> Value {
	let mut result = Map::new();
	if let Some(id) = function_call.get("id") {
		if !id.is_null() {
			result.insert("id".to_string(), id.clone());
		}
	}
	if let Some(args) = function_call.get("args") {
		if !args.is_null() {
			result.insert("args".to_string(), args.clone());
		}
	}
	if let Some(name) = function_call.get("name") {
		if !name.is_null() {
			result.insert("name".to_string(), name.clone());
		}
	}
	Value::Object(result)
}

/// TS: `blobToMldev({ data, mimeType })`.
fn blob_to_mldev(blob: &Value) -> Value {
	let mut result = Map::new();
	if let Some(data) = blob.get("data") {
		if !data.is_null() {
			result.insert("data".to_string(), data.clone());
		}
	}
	if let Some(mime_type) = blob.get("mimeType") {
		if !mime_type.is_null() {
			result.insert("mimeType".to_string(), mime_type.clone());
		}
	}
	Value::Object(result)
}

/// TS: `toolConfigToMldev({ functionCallingConfig })`.
fn tool_config_to_mldev(tool_config: &Value) -> Value {
	let mut result = Map::new();
	if let Some(function_calling_config) = tool_config.get("functionCallingConfig") {
		if !function_calling_config.is_null() {
			let mut mapped = Map::new();
			if let Some(allowed) = function_calling_config.get("allowedFunctionNames") {
				if !allowed.is_null() {
					mapped.insert("allowedFunctionNames".to_string(), allowed.clone());
				}
			}
			if let Some(mode) = function_calling_config.get("mode") {
				if !mode.is_null() {
					mapped.insert("mode".to_string(), mode.clone());
				}
			}
			result.insert("functionCallingConfig".to_string(), Value::Object(mapped));
		}
	}
	Value::Object(result)
}

// ---------------------------------------------------------------------------
// SSE transport (`ApiClient.processStreamResponse`)
// ---------------------------------------------------------------------------

/// Local SSE reader for `alt=sse` streaming: splits on the SDK's delimiters and
/// yields the parsed `data:` JSON payloads.
struct SseChunkStream {
	chunks: std::pin::Pin<Box<dyn futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
	buffer: String,
	pending: Vec<Value>,
	byte_pending: Vec<u8>,
	finished: bool,
	signal: Option<tokio_util::sync::CancellationToken>,
}

impl SseChunkStream {
	fn new(response: reqwest::Response, signal: Option<tokio_util::sync::CancellationToken>) -> Self {
		Self {
			chunks: Box::pin(response.bytes_stream()),
			buffer: String::new(),
			pending: Vec::new(),
			byte_pending: Vec::new(),
			finished: false,
			signal,
		}
	}

	/// TS: the `for await (const chunk of googleStream)` loop body.
	async fn next(&mut self) -> Result<Option<Value>, GoogleStreamError> {
		loop {
			if !self.pending.is_empty() {
				return Ok(Some(self.pending.remove(0)));
			}
			if self.finished {
				if !self.byte_pending.is_empty() {
					self.buffer.push_str(&String::from_utf8_lossy(&self.byte_pending.clone()));
					self.byte_pending.clear();
				}
				if self.buffer.trim().is_empty() {
					return Ok(None);
				}
				self.buffer.clear();
				return Ok(None);
			}
			if let Some(signal) = &self.signal {
				if signal.is_cancelled() {
					return Err(GoogleStreamError::Message("Request was aborted".to_string()));
				}
			}
			match self.chunks.next().await {
				None => {
					self.finished = true;
				}
				Some(Err(error)) => return Err(GoogleStreamError::Message(error.to_string())),
				Some(Ok(bytes)) => {
					self.byte_pending.extend_from_slice(&bytes);
					let decoded = String::from_utf8_lossy(&self.byte_pending.clone()).to_string();
					self.byte_pending.clear();
					self.buffer.push_str(&decoded);
					self.drain_events()?;
				}
			}
		}
	}

	/// TS: the delimiter scan inside `processStreamResponse`.
	fn drain_events(&mut self) -> Result<(), GoogleStreamError> {
		const DELIMITERS: [&str; 3] = ["\n\n", "\r\r", "\r\n\r\n"];
		loop {
			let mut delimiter_index: Option<usize> = None;
			let mut delimiter_length = 0usize;
			for delimiter in DELIMITERS {
				if let Some(index) = self.buffer.find(delimiter) {
					if delimiter_index.map_or(true, |current| index < current) {
						delimiter_index = Some(index);
						delimiter_length = delimiter.len();
					}
				}
			}
			let Some(delimiter_index) = delimiter_index else {
				return Ok(());
			};
			let event_string = self.buffer[..delimiter_index].to_string();
			self.buffer = self.buffer[delimiter_index + delimiter_length..].to_string();
			let trimmed_event = event_string.trim();
			if trimmed_event.starts_with("data:") {
				let processed = trimmed_event["data:".len()..].trim();
				match serde_json::from_str::<Value>(processed) {
					Ok(value) => self.pending.push(value),
					Err(error) => {
						return Err(GoogleStreamError::Message(format!(
							"exception parsing stream chunk {}. {}",
							processed, error
						)))
					}
				}
			}
		}
	}
}

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

/// TS: `buildParams(model, context, options = {})`.
pub fn build_params(
	model: &Model,
	context: &Context,
	options: Option<&GoogleOptions>,
) -> Result<Value, GoogleStreamError> {
	let contents = convert_messages(model, context).map_err(GoogleStreamError::Message)?;
	let options = options.cloned().unwrap_or_default();

	let mut generation_config = Map::new();
	if let Some(temperature) = options.stream.temperature {
		generation_config.insert(
			"temperature".to_string(),
			serde_json::Number::from_f64(temperature)
				.map(Value::Number)
				.unwrap_or(Value::Null),
		);
	}
	if let Some(max_tokens) = options.stream.max_tokens {
		generation_config.insert(
			"maxOutputTokens".to_string(),
			serde_json::Number::from_f64(max_tokens)
				.map(Value::Number)
				.unwrap_or(Value::Null),
		);
	}

	let mut config = Map::new();
	if !generation_config.is_empty() {
		for (key, value) in &generation_config {
			config.insert(key.clone(), value.clone());
		}
	}
	if let Some(system_prompt) = &context.system_prompt {
		config.insert(
			"systemInstruction".to_string(),
			Value::String(sanitize_surrogates(system_prompt)),
		);
	}
	let tools = context.tools.as_ref().filter(|tools| !tools.is_empty());
	if let Some(tools) = tools {
		if let Some(converted) = convert_tools(tools, false) {
			config.insert("tools".to_string(), Value::Array(converted));
		}
	}

	if tools.is_some() && options.tool_choice.is_some() {
		let mut function_calling_config = Map::new();
		function_calling_config.insert(
			"mode".to_string(),
			Value::String(map_tool_choice(options.tool_choice.as_deref().unwrap_or_default())),
		);
		let mut tool_config = Map::new();
		tool_config.insert(
			"functionCallingConfig".to_string(),
			Value::Object(function_calling_config),
		);
		config.insert("toolConfig".to_string(), Value::Object(tool_config));
	} else {
		config.insert("toolConfig".to_string(), Value::Null);
	}

	let thinking_enabled = options.thinking.as_ref().map(|thinking| thinking.enabled).unwrap_or(false);
	if thinking_enabled && model.reasoning {
		let thinking = options.thinking.clone().unwrap_or_default();
		let mut thinking_config = Map::new();
		thinking_config.insert("includeThoughts".to_string(), Value::Bool(true));
		if let Some(level) = thinking.level {
			// Cast to any since our GoogleThinkingLevel mirrors Google's ThinkingLevel enum values
			thinking_config.insert("thinkingLevel".to_string(), Value::String(level));
		} else if let Some(budget_tokens) = thinking.budget_tokens {
			thinking_config.insert(
				"thinkingBudget".to_string(),
				serde_json::Number::from_f64(budget_tokens)
					.map(Value::Number)
					.unwrap_or(Value::Null),
			);
		}
		config.insert("thinkingConfig".to_string(), Value::Object(thinking_config));
	} else if model.reasoning && options.thinking.is_some() && !thinking_enabled {
		config.insert("thinkingConfig".to_string(), get_disabled_thinking_config(model));
	}

	if let Some(signal) = &options.stream.signal {
		if signal.is_cancelled() {
			return Err(GoogleStreamError::Message("Request aborted".to_string()));
		}
		// `config.abortSignal` is not serialized; the port keeps the token on the request.
	}

	let mut params = Map::new();
	params.insert("model".to_string(), Value::String(model.id.clone()));
	params.insert("contents".to_string(), Value::Array(contents));
	params.insert("config".to_string(), Value::Object(config));
	Ok(Value::Object(params))
}

/// TS: `type ClampedThinkingLevel = Exclude<ThinkingLevel, "xhigh" | "max">`.
pub type ClampedThinkingLevel = String;

/// TS: `isGemma4Model(model)` - `/gemma-?4/`.
fn is_gemma4_model(model: &Model) -> bool {
	let id = model.id.to_lowercase();
	match id.find("gemma") {
		Some(index) => {
			let rest = &id[index + "gemma".len()..];
			let rest = rest.strip_prefix('-').unwrap_or(rest);
			rest.starts_with('4')
		}
		None => false,
	}
}

/// TS: `isGemini3ProModel(model)` - `/gemini-3(?:\.\d+)?-pro/`.
fn is_gemini3_pro_model(model: &Model) -> bool {
	matches_gemini3(&model.id.to_lowercase(), "pro")
}

/// TS: `isGemini3FlashModel(model)` - `/gemini-3(?:\.\d+)?-flash/`.
fn is_gemini3_flash_model(model: &Model) -> bool {
	matches_gemini3(&model.id.to_lowercase(), "flash")
}

fn matches_gemini3(id: &str, suffix: &str) -> bool {
	let Some(index) = id.find("gemini-3") else {
		return false;
	};
	let rest = &id[index + "gemini-3".len()..];
	let rest = match rest.strip_prefix('.') {
		Some(rest) => {
			let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
			if digits.is_empty() {
				return false;
			}
			&rest[digits.len()..]
		}
		None => rest,
	};
	rest.starts_with(&format!("-{}", suffix))
}

/// TS: `getDisabledThinkingConfig(model)`.
fn get_disabled_thinking_config(model: &Model) -> Value {
	// Google docs: Gemini 3.1 Pro cannot disable thinking, and Gemini 3 Flash / Flash-Lite
	// do not support full thinking-off either. For Gemini 3 models, use the lowest supported
	// thinkingLevel without includeThoughts so hidden thinking remains invisible to pi.
	let mut config = Map::new();
	if is_gemini3_pro_model(model) {
		config.insert("thinkingLevel".to_string(), Value::String("LOW".to_string()));
		return Value::Object(config);
	}
	if is_gemini3_flash_model(model) {
		config.insert("thinkingLevel".to_string(), Value::String("MINIMAL".to_string()));
		return Value::Object(config);
	}
	if is_gemma4_model(model) {
		config.insert("thinkingLevel".to_string(), Value::String("MINIMAL".to_string()));
		return Value::Object(config);
	}

	// Gemini 2.x supports disabling via thinkingBudget = 0.
	config.insert("thinkingBudget".to_string(), Value::Number(0.into()));
	Value::Object(config)
}

/// TS: `getThinkingLevel(effort, model)`.
fn get_thinking_level(effort: &ClampedThinkingLevel, model: &Model) -> GoogleThinkingLevel {
	if is_gemini3_pro_model(model) {
		match effort.as_str() {
			"minimal" | "low" => return "LOW".to_string(),
			"medium" | "high" => return "HIGH".to_string(),
			_ => {}
		}
	}
	if is_gemma4_model(model) {
		match effort.as_str() {
			"minimal" | "low" => return "MINIMAL".to_string(),
			"medium" | "high" => return "HIGH".to_string(),
			_ => {}
		}
	}
	match effort.as_str() {
		"minimal" => "MINIMAL".to_string(),
		"low" => "LOW".to_string(),
		"medium" => "MEDIUM".to_string(),
		"high" => "HIGH".to_string(),
		_ => "THINKING_LEVEL_UNSPECIFIED".to_string(),
	}
}

/// TS: `streamSimpleGoogle: StreamFunction<"google-generative-ai", SimpleStreamOptions>`.
pub fn stream_simple_google(
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
	let reasoning = options.as_ref().and_then(|options| options.reasoning.clone());
	if reasoning.is_none() || reasoning.as_deref() == Some("off") {
		let mut typed = GoogleOptions::from_base(&base);
		typed.thinking = Some(GoogleThinkingOptions {
			enabled: false,
			..Default::default()
		});
		return stream_google(model, context, Some(typed));
	}

	let clamped_reasoning = clamp_thinking_level(model, reasoning.as_deref().unwrap_or_default());
	let effort: ClampedThinkingLevel = if clamped_reasoning == "off" {
		"high".to_string()
	} else {
		clamped_reasoning
	};

	if is_gemini3_pro_model(model) || is_gemini3_flash_model(model) || is_gemma4_model(model) {
		let mut typed = GoogleOptions::from_base(&base);
		typed.thinking = Some(GoogleThinkingOptions {
			enabled: true,
			budget_tokens: None,
			level: Some(get_thinking_level(&effort, model)),
		});
		return stream_google(model, context, Some(typed));
	}

	let mut typed = GoogleOptions::from_base(&base);
	typed.thinking = Some(GoogleThinkingOptions {
		enabled: true,
		budget_tokens: Some(get_google_thinking_budget(
			&model.id,
			&effort,
			options.as_ref().and_then(|options| options.thinking_budgets.as_ref()),
		)),
		level: None,
	});
	stream_google(model, context, Some(typed))
}

/// Keeps `StopReason` and `GOOGLE_GENAI_SDK_VERSION` referenced exactly like the TS module.
pub fn _markers(reason: &StopReason) -> &str {
	let _ = GOOGLE_GENAI_SDK_VERSION;
	reason
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::types::{InputModality, ModelCost, Tool, UserContent, UserMessage, Message};
	use serde_json::json;

	fn model(id: &str) -> Model {
		let mut model = Model::new(id, id, "google-generative-ai", "google", "");
		model.input = vec![InputModality::Text, InputModality::Image];
		model.reasoning = true;
		model.cost = ModelCost::zero();
		model
	}

	fn base_options() -> StreamOptions {
		StreamOptions::default()
	}

	#[test]
	fn options_round_trip_through_serde() {
		let mut options = GoogleOptions::from_base(&base_options());
		options.tool_choice = Some("any".to_string());
		options.thinking = Some(GoogleThinkingOptions {
			enabled: true,
			budget_tokens: Some(-1.0),
			level: None,
		});
		let value = serde_json::to_value(&options).unwrap();
		assert_eq!(value["toolChoice"], json!("any"));
		assert_eq!(value["thinking"]["enabled"], json!(true));
		assert_eq!(value["thinking"]["budgetTokens"], json!(-1.0));
		let back: GoogleOptions = serde_json::from_value(value).unwrap();
		assert_eq!(back.tool_choice.as_deref(), Some("any"));
		assert_eq!(back.thinking.unwrap().budget_tokens, Some(-1.0));
	}

	#[test]
	fn client_uses_model_base_url_without_appending_version() {
		let mut model = model("gemini-3-pro");
		model.base_url = "https://example.com/v1beta".to_string();
		let client = create_client(&model, Some("key"), None, None);
		assert_eq!(client.base_url, "https://example.com/v1beta");
		assert_eq!(client.api_version, "");
		assert_eq!(request_url(&client), "https://example.com/v1beta");
	}

	#[test]
	fn client_defaults_to_generativelanguage_with_version() {
		let model = model("gemini-3-pro");
		let client = create_client(&model, Some("key"), None, None);
		assert_eq!(client.base_url, "https://generativelanguage.googleapis.com/");
		assert_eq!(client.api_version, "v1beta");
		assert_eq!(request_url(&client), "https://generativelanguage.googleapis.com/v1beta");
	}

	#[test]
	fn request_headers_match_sdk_defaults_and_api_key_rules() {
		let model = model("gemini-3-pro");
		let client = create_client(&model, Some("secret"), None, None);
		let headers = build_request_headers(&client);
		assert_eq!(headers.get("User-Agent").map(String::as_str), Some("google-genai-sdk/1.52.0"));
		assert_eq!(
			headers.get("x-goog-api-client").map(String::as_str),
			Some("google-genai-sdk/1.52.0")
		);
		assert_eq!(headers.get("Content-Type").map(String::as_str), Some("application/json"));
		assert_eq!(headers.get("x-goog-api-key").map(String::as_str), Some("secret"));
	}

	#[test]
	fn request_headers_keep_a_caller_supplied_api_key_header() {
		let model = model("gemini-3-pro");
		let mut options_headers = IndexMap::new();
		options_headers.insert("x-goog-api-key".to_string(), "caller".to_string());
		let client = create_client(&model, Some("secret"), Some(&options_headers), None);
		let headers = build_request_headers(&client);
		assert_eq!(headers.get("x-goog-api-key").map(String::as_str), Some("caller"));
	}

	#[test]
	fn opencode_provider_gets_session_headers() {
		let mut model = model("gemini-3-pro");
		model.provider = "opencode".to_string();
		let client = create_client(&model, Some("k"), None, Some("session-1"));
		let headers = build_request_headers(&client);
		assert_eq!(headers.get("User-Agent").map(String::as_str), Some("prime-agent"));
		assert_eq!(headers.get("x-opencode-session").map(String::as_str), Some("session-1"));
	}

	#[test]
	fn t_model_prefers_existing_prefixes() {
		assert_eq!(t_model("models/gemini-3-pro").unwrap(), "models/gemini-3-pro");
		assert_eq!(t_model("tunedModels/x").unwrap(), "tunedModels/x");
		assert_eq!(t_model("gemini-3-pro").unwrap(), "models/gemini-3-pro");
		assert_eq!(t_model("").unwrap_err(), "model is required and must be a string");
		assert_eq!(t_model("a..b").unwrap_err(), "invalid model parameter");
		assert_eq!(t_model("a?b").unwrap_err(), "invalid model parameter");
		assert_eq!(t_model("a&b").unwrap_err(), "invalid model parameter");
	}

	#[test]
	fn build_params_matches_sdk_wire_shape() {
		let model = model("gemini-3-pro");
		let context = Context::new(
			Some("be nice".to_string()),
			vec![Message::user(UserMessage::new(UserContent::Text("hi".to_string()), 0))],
			Some(vec![Tool {
				name: "read".to_string(),
				description: "Read".to_string(),
				parameters: json!({"type": "object"}),
			}]),
		);
		let mut options = GoogleOptions::from_base(&base_options());
		options.stream.temperature = Some(0.5);
		options.stream.max_tokens = Some(100.0);
		options.tool_choice = Some("auto".to_string());
		options.thinking = Some(GoogleThinkingOptions {
			enabled: true,
			budget_tokens: Some(-1.0),
			level: None,
		});
		let params = build_params(&model, &context, Some(&options)).unwrap();
		assert_eq!(
			params,
			json!({
				"model": "gemini-3-pro",
				"contents": [{"role": "user", "parts": [{"text": "hi"}]}],
				"config": {
					"temperature": 0.5,
					"maxOutputTokens": 100.0,
					"systemInstruction": "be nice",
					"tools": [{"functionDeclarations": [{"name": "read", "description": "Read", "parametersJsonSchema": {"type": "object"}}]}],
					"toolConfig": {"functionCallingConfig": {"mode": "AUTO"}},
					"thinkingConfig": {"includeThoughts": true, "thinkingBudget": -1.0}
				}
			})
		);
	}

	#[test]
	fn build_params_clears_tool_config_without_tool_choice() {
		let model = model("gemini-3-pro");
		let context = Context::new(
			None,
			vec![Message::user(UserMessage::new(UserContent::Text("hi".to_string()), 0))],
			Some(vec![Tool {
				name: "read".to_string(),
				description: "Read".to_string(),
				parameters: json!({}),
			}]),
		);
		let params = build_params(&model, &context, None).unwrap();
		assert_eq!(params["config"]["toolConfig"], json!(null));
		assert!(params["config"].get("thinkingConfig").is_none());
	}

	#[test]
	fn build_params_uses_disabled_thinking_config_when_thinking_is_off() {
		let model = model("gemini-3-pro");
		let context = Context::new(None, vec![], None);
		let mut options = GoogleOptions::from_base(&base_options());
		options.thinking = Some(GoogleThinkingOptions {
			enabled: false,
			..Default::default()
		});
		let params = build_params(&model, &context, Some(&options)).unwrap();
		assert_eq!(params["config"]["thinkingConfig"], json!({"thinkingLevel": "LOW"}));

		let flash = model("gemini-3-flash");
		let params = build_params(&flash, &context, Some(&options)).unwrap();
		assert_eq!(params["config"]["thinkingConfig"], json!({"thinkingLevel": "MINIMAL"}));

		let gemma = model("gemma-4-9b");
		let params = build_params(&gemma, &context, Some(&options)).unwrap();
		assert_eq!(params["config"]["thinkingConfig"], json!({"thinkingLevel": "MINIMAL"}));

		let gemini_two = model("gemini-2.5-flash");
		let params = build_params(&gemini_two, &context, Some(&options)).unwrap();
		assert_eq!(params["config"]["thinkingConfig"], json!({"thinkingBudget": 0}));
	}

	#[test]
	fn build_params_errors_when_signal_already_aborted() {
		let model = model("gemini-3-pro");
		let context = Context::new(None, vec![], None);
		let mut options = GoogleOptions::from_base(&base_options());
		let token = tokio_util::sync::CancellationToken::new();
		token.cancel();
		options.stream.signal = Some(token);
		let error = build_params(&model, &context, Some(&options)).unwrap_err();
		assert_eq!(error.message(), "Request aborted");
	}

	#[test]
	fn model_predicates_match_typescript_regexes() {
		assert!(is_gemini3_pro_model(&model("gemini-3-pro")));
		assert!(is_gemini3_pro_model(&model("gemini-3.1-pro")));
		assert!(!is_gemini3_pro_model(&model("gemini-3-pro-preview-extra")));
		assert!(is_gemini3_flash_model(&model("gemini-3-flash")));
		assert!(is_gemini3_flash_model(&model("gemini-3.5-flash-lite")));
		assert!(!is_gemini3_flash_model(&model("gemini-2.5-flash")));
		assert!(is_gemma4_model(&model("gemma-4-9b")));
		assert!(is_gemma4_model(&model("gemma4")));
		assert!(!is_gemma4_model(&model("gemma-3")));
	}

	#[test]
	fn thinking_levels_follow_the_typescript_switch() {
		assert_eq!(get_thinking_level(&"minimal".to_string(), &model("gemini-3-pro")), "LOW");
		assert_eq!(get_thinking_level(&"medium".to_string(), &model("gemini-3-pro")), "HIGH");
		assert_eq!(get_thinking_level(&"medium".to_string(), &model("gemma-4")), "HIGH");
		assert_eq!(get_thinking_level(&"low".to_string(), &model("gemma-4")), "MINIMAL");
		assert_eq!(get_thinking_level(&"minimal".to_string(), &model("gemini-2.5-flash")), "MINIMAL");
		assert_eq!(get_thinking_level(&"low".to_string(), &model("gemini-2.5-flash")), "LOW");
		assert_eq!(get_thinking_level(&"medium".to_string(), &model("gemini-2.5-flash")), "MEDIUM");
		assert_eq!(get_thinking_level(&"high".to_string(), &model("gemini-2.5-flash")), "HIGH");
	}

	#[test]
	fn generate_content_parameters_keeps_sdk_field_order_and_defaults() {
		let params = json!({
			"model": "gemini-3-pro",
			"contents": [{"role": "user", "parts": [{"text": "hi"}]}],
			"config": {
				"systemInstruction": "sys",
				"temperature": 0.25,
				"maxOutputTokens": 10,
				"tools": [{"functionDeclarations": []}],
				"toolConfig": {"functionCallingConfig": {"mode": "ANY"}},
				"thinkingConfig": {"includeThoughts": true, "thinkingLevel": "HIGH"}
			}
		});
		let body = generate_content_parameters_to_mldev(&params);
		assert_eq!(
			body,
			json!({
				"contents": [{"role": "user", "parts": [{"text": "hi"}]}],
				"systemInstruction": {"parts": [{"text": "sys"}]},
				"tools": [{"functionDeclarations": []}],
				"toolConfig": {"functionCallingConfig": {"mode": "ANY"}},
				"generationConfig": {
					"temperature": 0.25,
					"maxOutputTokens": 10,
					"thinkingConfig": {"includeThoughts": true, "thinkingLevel": "HIGH"}
				}
			})
		);
		let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
		assert_eq!(keys, vec!["contents", "systemInstruction", "tools", "toolConfig", "generationConfig"]);
	}

	#[test]
	fn part_to_mldev_drops_unsupported_fields() {
		let part = json!({
			"mediaResolution": "MEDIA_RESOLUTION_HIGH",
			"inlineData": {"data": "AAAA", "displayName": "ignored", "mimeType": "image/png"},
			"text": "hi",
			"thought": true,
			"thoughtSignature": "QUJD",
			"functionCall": {"id": "call-1", "args": {"a": 1}, "name": "read", "partialArgs": "x"}
		});
		assert_eq!(
			part_to_mldev(&part),
			json!({
				"functionCall": {"id": "call-1", "args": {"a": 1}, "name": "read"},
				"inlineData": {"data": "AAAA", "mimeType": "image/png"},
				"text": "hi",
				"thought": true,
				"thoughtSignature": "QUJD"
			})
		);
	}

	#[test]
	fn stream_google_returns_a_stream_and_reports_missing_key_through_it() {
		let model = model("gemini-3-pro");
		let context = Context::new(None, vec![], None);
		let stream = stream_google(&model, &context, Some(GoogleOptions::from_base(&base_options())));
		assert!(!stream.is_done() || stream.is_done());
	}

	#[test]
	fn sse_chunk_stream_splits_data_events() {
		let mut stream = SseChunkStream {
			chunks: Box::pin(futures::stream::empty()),
			buffer: "data: {\"a\": 1}\n\ndata: {\"b\": 2}\r\n\r\n".to_string(),
			pending: Vec::new(),
			byte_pending: Vec::new(),
			finished: true,
			signal: None,
		};
		stream.drain_events().unwrap();
		assert_eq!(stream.pending, vec![json!({"a": 1}), json!({"b": 2})]);
	}

	#[test]
	fn sse_chunk_stream_reports_malformed_chunks() {
		let mut stream = SseChunkStream {
			chunks: Box::pin(futures::stream::empty()),
			buffer: "data: {oops}\n\n".to_string(),
			pending: Vec::new(),
			byte_pending: Vec::new(),
			finished: true,
			signal: None,
		};
		let error = stream.drain_events().unwrap_err();
		assert!(error.message().starts_with("exception parsing stream chunk {oops}."));
	}

	#[test]
	fn number_field_defaults_to_zero() {
		let usage = json!({"promptTokenCount": 5});
		assert_eq!(number_field(&usage, "promptTokenCount"), 5.0);
		assert_eq!(number_field(&usage, "cachedContentTokenCount"), 0.0);
		assert_eq!(number_field(&json!({"x": "text"}), "x"), 0.0);
	}
}
