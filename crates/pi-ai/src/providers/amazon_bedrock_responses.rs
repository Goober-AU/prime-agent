//! Port of packages/ai/src/providers/amazon-bedrock-responses.ts
//!
//! The TypeScript talks to the OpenAI SDK pointed at the Bedrock Responses endpoint; the Rust
//! port builds the same JSON request, signs it with SigV4 (see `bedrock_responses_client.rs`),
//! decodes the SSE stream locally and hands the events to `process_responses_stream`, exactly
//! like the TypeScript hands them to `processResponsesStream`.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::models::clamp_thinking_level;
use crate::types::{AssistantMessage, AssistantMessageEvent, Context, Model, SimpleStreamOptions, StreamOptions, Usage};
use crate::utils::event_stream::{create_assistant_message_event_stream, AssistantMessageEventStream};
use crate::utils::headers::header_map_to_record;
use crate::utils::now_ms;
use crate::utils::stream_failure::{
	format_stream_failure_message, record_stream_failure, stream_failure_from_stop_reason, ThrownStreamError,
};

use super::bedrock_responses_client::{
	create_bedrock_responses_client, responses_event_stream, send_signed_responses_request,
	BedrockResponsesAuthOptions,
};
use super::openai_responses_shared::{
	convert_responses_messages, convert_responses_tools, process_responses_stream, ConvertResponsesToolsOptions,
	OpenAIResponsesStreamOptions,
};
use super::simple_options::build_base_options;

/// TS: `const BEDROCK_TOOL_CALL_PROVIDERS = new Set(["amazon-bedrock"])`.
pub const BEDROCK_TOOL_CALL_PROVIDERS: [&str; 1] = ["amazon-bedrock"];

/// TS: `BEDROCK_TOOL_CALL_PROVIDERS.has(provider)`.
pub fn is_bedrock_tool_call_provider(provider: &str) -> bool {
	BEDROCK_TOOL_CALL_PROVIDERS.contains(&provider)
}

/// TS: `interface BedrockResponsesOptions extends BedrockResponsesAuthOptions`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BedrockResponsesOptions {
	#[serde(flatten)]
	pub stream: StreamOptions,
	/// `BedrockResponsesAuthOptions.region`
	#[serde(skip_serializing_if = "Option::is_none")]
	pub region: Option<String>,
	/// `BedrockResponsesAuthOptions.profile`
	#[serde(skip_serializing_if = "Option::is_none")]
	pub profile: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub reasoning_effort: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub reasoning_summary: Option<String>,
	/// TS: `credentialProvider?: ReturnType<typeof defaultProvider>`.
	#[serde(skip)]
	pub credentials: Option<super::amazon_bedrock::AwsCredentials>,
	#[serde(skip)]
	pub has_credential_provider: bool,
}

impl BedrockResponsesOptions {
	/// TS: the caller passes `StreamOptions & Record<string, unknown>`; this keeps the
	/// non-serializable fields (signal, on_payload, on_response, on_usage_observation).
	pub fn from_base(base: &StreamOptions) -> Self {
		Self {
			stream: base.clone(),
			..Default::default()
		}
	}

	/// The `BedrockResponsesAuthOptions` view the client factory takes.
	pub fn auth_options(&self) -> BedrockResponsesAuthOptions {
		BedrockResponsesAuthOptions {
			stream: self.stream.clone(),
			region: self.region.clone(),
			profile: self.profile.clone(),
			credentials: self.credentials.clone(),
			has_credential_provider: self.has_credential_provider,
		}
	}
}

/// TS: `streamBedrockResponses(model, context, options?)` registered as a builtin.
pub fn stream_bedrock_responses(
	model: &Model,
	context: &Context,
	options: Option<&StreamOptions>,
) -> AssistantMessageEventStream {
	stream_bedrock_responses_with_options(
		model,
		context,
		options.map(BedrockResponsesOptions::from_base),
	)
}

/// The typed entry point, mirroring the TypeScript signature.
pub fn stream_bedrock_responses_with_options(
	model: &Model,
	context: &Context,
	options: Option<BedrockResponsesOptions>,
) -> AssistantMessageEventStream {
	let stream = create_assistant_message_event_stream();

	let model = model.clone();
	let context = context.clone();
	let options = options.unwrap_or_default();
	let producer = stream.clone();
	let out = stream.clone();

	// TS: `(async () => { try { ... } catch (error) { ... } })();`
	producer.spawn(async move {
		let mut output = AssistantMessage {
			role: "assistant".to_string(),
			content: Vec::new(),
			api: "bedrock-responses".to_string(),
			provider: model.provider.clone(),
			model: model.id.clone(),
			usage: Usage::zero(),
			stop_reason: "stop".to_string(),
			timestamp: now_ms(),
			..Default::default()
		};

		let result = run_bedrock_responses_stream(&model, &context, &options, &mut output, &out).await;

		match result {
			Ok(()) => {
				out.push(AssistantMessageEvent::Done {
					reason: output.stop_reason.clone(),
					message: output,
				});
				out.end(None);
			}
			Err(error) => {
				let aborted = options
					.stream
					.signal
					.as_ref()
					.map(|signal| signal.is_cancelled())
					.unwrap_or(false);
				output.stop_reason = if aborted { "aborted" } else { "error" }.to_string();
				// TS: `output.errorMessage = formatStreamFailureMessage(error);`
				let thrown = error.as_thrown();
				output.error_message = Some(format_stream_failure_message(&thrown));
				record_stream_failure(&model, &mut output, &thrown);
				out.push(AssistantMessageEvent::Error {
					reason: output.stop_reason.clone(),
					error: output,
				});
				out.end(None);
			}
		}
	});

	stream
}

/// TS: `streamSimpleBedrockResponses(model, context, options?)`.
pub fn stream_simple_bedrock_responses(
	model: &Model,
	context: &Context,
	options: Option<&StreamOptions>,
) -> AssistantMessageEventStream {
	stream_simple_bedrock_responses_with_options(model, context, options.map(|base| SimpleStreamOptions {
		stream: base.clone(),
		reasoning: None,
		thinking_budgets: None,
	}))
}

/// The typed entry point, mirroring the TypeScript `SimpleStreamOptions` signature.
pub fn stream_simple_bedrock_responses_with_options(
	model: &Model,
	context: &Context,
	options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
	// TS: `const base = buildBaseOptions(model, options);`
	let base = build_base_options(model, options.as_ref(), None);
	// TS: `clampThinkingLevel(model, options?.reasoning ?? "medium")`
	let reasoning_effort = clamp_thinking_level(
		model,
		options
			.as_ref()
			.and_then(|options| options.reasoning.as_deref())
			.unwrap_or("medium"),
	);
	// TS: `{ ...base, onUsageObservation: options?.onUsageObservation, reasoningEffort }`
	let mut stream_options = base;
	stream_options.on_usage_observation =
		options.as_ref().and_then(|options| options.stream.on_usage_observation.clone());
	stream_bedrock_responses_with_options(
		model,
		context,
		Some(BedrockResponsesOptions {
			stream: stream_options,
			reasoning_effort: Some(reasoning_effort),
			..Default::default()
		}),
	)
}

/// The Rust counterpart of a value thrown out of the TypeScript `try` block.
#[derive(Debug, Clone)]
pub enum ResponsesRunError {
	/// `throw streamFailureFromStopReason(...)` / a `StreamFailureError` from the shared stream loop.
	Failure(Box<crate::utils::stream_failure::StreamFailureError>),
	/// `throw new Error(...)`.
	Message(String),
}

impl ResponsesRunError {
	pub fn message(message: impl Into<String>) -> Self {
		ResponsesRunError::Message(message.into())
	}

	/// The thrown value as the shared `stream-failure` helpers expect it.
	pub fn as_thrown(&self) -> ThrownStreamError<'_> {
		match self {
			ResponsesRunError::Failure(failure) => ThrownStreamError::Failure(failure),
			ResponsesRunError::Message(message) => ThrownStreamError::Message(message),
		}
	}
}

impl std::fmt::Display for ResponsesRunError {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			ResponsesRunError::Failure(failure) => write!(formatter, "{}", failure.message),
			ResponsesRunError::Message(message) => write!(formatter, "{}", message),
		}
	}
}

impl std::error::Error for ResponsesRunError {}

/// TS: `buildParams(model, context, options?)`.
pub fn build_params(
	model: &Model,
	context: &Context,
	options: Option<&BedrockResponsesOptions>,
) -> Result<Value, String> {
	if let Some(service_tier) = options.and_then(|options| options.stream.service_tier.as_ref()).flatten() {
		if service_tier != "default" && service_tier != "auto" {
			return Err("Bedrock Astra supports only the Standard service tier.".to_string());
		}
	}

	let mut params: Map<String, Value> = Map::new();
	params.insert("model".to_string(), Value::String(model.id.clone()));
	params.insert(
		"input".to_string(),
		Value::Array(convert_responses_messages(
			model,
			context,
			&is_bedrock_tool_call_provider,
			None,
		)?),
	);
	params.insert("stream".to_string(), Value::Bool(true));
	params.insert("store".to_string(), Value::Bool(false));
	params.insert("service_tier".to_string(), Value::String("default".to_string()));
	// TS: `max_output_tokens: options?.maxTokens` - `undefined` drops the key on the wire.
	if let Some(max_tokens) = options.and_then(|options| options.stream.max_tokens) {
		params.insert("max_output_tokens".to_string(), json!(max_tokens));
	}
	if let Some(tools) = context.tools.as_ref() {
		if !tools.is_empty() {
			params.insert(
				"tools".to_string(),
				Value::Array(convert_responses_tools(tools, None)),
			);
		}
	}
	if model.reasoning {
		let effort = options
			.and_then(|options| options.reasoning_effort.clone())
			.unwrap_or_else(|| "medium".to_string());
		if !["low", "medium", "high", "xhigh", "max"].contains(&effort.as_str()) {
			return Err("Bedrock Astra reasoning effort must be low, medium, high, xhigh, or max.".to_string());
		}
		params.insert(
			"reasoning".to_string(),
			json!({
				"effort": effort,
				"summary": options
					.and_then(|options| options.reasoning_summary.clone())
					.unwrap_or_else(|| "auto".to_string()),
			}),
		);
		params.insert("include".to_string(), json!(["reasoning.encrypted_content"]));
	}
	Ok(Value::Object(params))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::types::{Message, ModelCost, ProviderUsageObservation, Tool, UserContent, UserMessage};

	fn model(id: &str) -> Model {
		let mut model = Model::new(
			id,
			"Astra",
			"bedrock-responses",
			"amazon-bedrock",
			"https://bedrock-mantle.us-west-2.api.aws/openai/v1",
		);
		model.reasoning = true;
		model.cost = ModelCost::default();
		model
	}

	fn context(text: &str) -> Context {
		Context::new(
			None,
			vec![Message::user(UserMessage::new(UserContent::Text(text.to_string()), 0))],
			None,
		)
	}

	#[test]
	fn tool_call_providers_match_the_typescript_set() {
		assert!(is_bedrock_tool_call_provider("amazon-bedrock"));
		assert!(!is_bedrock_tool_call_provider("openai"));
	}

	#[test]
	fn astra_context_pricing_applies_above_the_threshold() {
		let mut usage = Usage {
			input: 100_000.0,
			cache_read: 100_000.0,
			cache_write: 72_000.0,
			output: 1_000.0,
			total_tokens: 0.0,
			cost: crate::types::UsageCost {
				input: 1.0,
				output: 1.0,
				cache_read: 1.0,
				cache_write: 1.0,
				total: 4.0,
			},
		};
		// 272_000 is the boundary: `<= 272_000` returns early.
		apply_bedrock_astra_context_pricing(&mut usage);
		assert_eq!(usage.cost.input, 1.0);

		usage.input = 100_001.0;
		apply_bedrock_astra_context_pricing(&mut usage);
		assert_eq!(usage.cost.input, 2.0);
		assert_eq!(usage.cost.cache_read, 2.0);
		assert_eq!(usage.cost.cache_write, 2.0);
		assert_eq!(usage.cost.output, 1.5);
		assert_eq!(usage.cost.total, 7.5);
	}

	#[test]
	fn build_params_matches_the_typescript_shape() {
		let params = build_params(&model("openai.gpt-6-astra"), &context("hello"), None).unwrap();
		let keys: Vec<&String> = params.as_object().unwrap().keys().collect();
		assert_eq!(
			keys,
			vec!["model", "input", "stream", "store", "service_tier", "reasoning", "include"]
		);
		assert_eq!(params["model"], json!("openai.gpt-6-astra"));
		assert_eq!(params["stream"], json!(true));
		assert_eq!(params["store"], json!(false));
		assert_eq!(params["service_tier"], json!("default"));
		assert_eq!(params["reasoning"], json!({ "effort": "medium", "summary": "auto" }));
		assert_eq!(params["include"], json!(["reasoning.encrypted_content"]));
		// `max_output_tokens: undefined` drops the key.
		assert!(params.get("max_output_tokens").is_none());
		assert!(params.get("tools").is_none());
	}

	#[test]
	fn build_params_carries_max_tokens_tools_and_reasoning_options() {
		let mut context = context("hello");
		context.tools = Some(vec![Tool {
			name: "read".to_string(),
			description: "Read a file".to_string(),
			parameters: json!({ "type": "object" }),
		}]);
		let options = BedrockResponsesOptions {
			stream: StreamOptions {
				max_tokens: Some(4096.0),
				..Default::default()
			},
			reasoning_effort: Some("high".to_string()),
			reasoning_summary: Some("concise".to_string()),
			..Default::default()
		};
		let params = build_params(&model("openai.gpt-6-astra"), &context, Some(&options)).unwrap();
		assert_eq!(params["max_output_tokens"], json!(4096.0));
		assert_eq!(params["tools"][0]["type"], json!("function"));
		assert_eq!(params["tools"][0]["name"], json!("read"));
		assert_eq!(params["tools"][0]["strict"], json!(false));
		assert_eq!(params["reasoning"], json!({ "effort": "high", "summary": "concise" }));

		// Empty tool lists do not add the key.
		let mut context = context("hello");
		context.tools = Some(Vec::new());
		let params = build_params(&model("openai.gpt-6-astra"), &context, None).unwrap();
		assert!(params.get("tools").is_none());
	}

	#[test]
	fn build_params_rejects_unsupported_service_tiers_and_efforts() {
		let options = BedrockResponsesOptions {
			stream: StreamOptions {
				service_tier: Some(Some("flex".to_string())),
				..Default::default()
			},
			..Default::default()
		};
		assert_eq!(
			build_params(&model("openai.gpt-6-astra"), &context("hi"), Some(&options)).unwrap_err(),
			"Bedrock Astra supports only the Standard service tier."
		);

		for allowed in ["default", "auto"] {
			let options = BedrockResponsesOptions {
				stream: StreamOptions {
					service_tier: Some(Some(allowed.to_string())),
					..Default::default()
				},
				..Default::default()
			};
			assert!(build_params(&model("openai.gpt-6-astra"), &context("hi"), Some(&options)).is_ok());
		}

		let options = BedrockResponsesOptions {
			reasoning_effort: Some("minimal".to_string()),
			..Default::default()
		};
		assert_eq!(
			build_params(&model("openai.gpt-6-astra"), &context("hi"), Some(&options)).unwrap_err(),
			"Bedrock Astra reasoning effort must be low, medium, high, xhigh, or max."
		);

		for allowed in ["low", "medium", "high", "xhigh", "max"] {
			let options = BedrockResponsesOptions {
				reasoning_effort: Some(allowed.to_string()),
				..Default::default()
			};
			let params = build_params(&model("openai.gpt-6-astra"), &context("hi"), Some(&options)).unwrap();
			assert_eq!(params["reasoning"]["effort"], json!(allowed));
		}
	}

	#[test]
	fn build_params_skips_reasoning_for_non_reasoning_models() {
		let mut model = model("openai.gpt-6-astra");
		model.reasoning = false;
		let params = build_params(&model, &context("hi"), None).unwrap();
		assert!(params.get("reasoning").is_none());
		assert!(params.get("include").is_none());
	}

	#[test]
	fn options_round_trip_through_serde() {
		let options = BedrockResponsesOptions {
			stream: StreamOptions {
				max_tokens: Some(10.0),
				..Default::default()
			},
			region: Some("us-west-2".to_string()),
			profile: Some("dev".to_string()),
			reasoning_effort: Some("high".to_string()),
			reasoning_summary: Some("auto".to_string()),
			..Default::default()
		};
		let value = serde_json::to_value(&options).unwrap();
		assert_eq!(value["maxTokens"], json!(10.0));
		assert_eq!(value["region"], json!("us-west-2"));
		assert_eq!(value["profile"], json!("dev"));
		assert_eq!(value["reasoningEffort"], json!("high"));
		assert_eq!(value["reasoningSummary"], json!("auto"));

		let parsed: BedrockResponsesOptions = serde_json::from_value(value).unwrap();
		assert_eq!(parsed.region, options.region);
		assert_eq!(parsed.stream.max_tokens, Some(10.0));
		assert!(parsed.credentials.is_none());

		let base = StreamOptions {
			temperature: Some(0.2),
			..Default::default()
		};
		let typed = BedrockResponsesOptions::from_base(&base);
		assert_eq!(typed.stream.temperature, Some(0.2));
		assert!(typed.reasoning_effort.is_none());
		let auth = typed.auth_options();
		assert_eq!(auth.stream.temperature, Some(0.2));
		assert!(auth.region.is_none());
	}

	#[test]
	fn responses_run_error_preserves_the_thrown_shape() {
		let error = ResponsesRunError::message("Request was aborted");
		assert_eq!(error.to_string(), "Request was aborted");
		match error.as_thrown() {
			ThrownStreamError::Message(message) => assert_eq!(message, "Request was aborted"),
			other => panic!("unexpected {:?}", other),
		}

		let failure = stream_failure_from_stop_reason(Some("malformed_model_output"), Some("req-1"));
		let error = ResponsesRunError::Failure(Box::new(failure));
		match error.as_thrown() {
			ThrownStreamError::Failure(failure) => {
				assert_eq!(failure.info.kind, "malformed_response");
				assert_eq!(failure.info.request_id.as_deref(), Some("req-1"));
			}
			other => panic!("unexpected {:?}", other),
		}
	}

	#[test]
	fn usage_observation_is_forwarded_from_simple_options() {
		// The simple entry point copies `onUsageObservation` onto the base options.
		let simple = SimpleStreamOptions {
			stream: StreamOptions {
				on_usage_observation: Some(Arc::new(
					|_observation: ProviderUsageObservation, _model: &Model| {
						Box::pin(async {}) as crate::types::BoxFuture<()>
					},
				)),
				..Default::default()
			},
			reasoning: Some("high".to_string()),
			thinking_budgets: None,
		};
		let mut model = model("openai.gpt-6-astra");
		model.max_tokens = 1000.0;
		let base = build_base_options(&model, Some(&simple), None);
		assert!(base.on_usage_observation.is_some());

		// `clampThinkingLevel(model, options?.reasoning ?? "medium")` on a model without a
		// thinking-level map keeps the requested level.
		let mut clamped_model = model("openai.gpt-6-astra");
		clamped_model.reasoning = true;
		assert_eq!(clamp_thinking_level(&clamped_model, "high"), "high");
		assert_eq!(clamp_thinking_level(&clamped_model, "medium"), "medium");

		// A `null` level in the map is unsupported, so the clamp walks to another level.
		let mut mapped = clamped_model.clone();
		let mut map = crate::types::ThinkingLevelMap::new();
		map.insert("high".to_string(), None);
		map.insert("xhigh".to_string(), None);
		map.insert("max".to_string(), None);
		mapped.thinking_level_map = Some(map);
		assert_eq!(clamp_thinking_level(&mapped, "high"), "medium");
	}
}
