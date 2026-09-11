//! Port of packages/ai/src/providers/azure-openai-responses.ts
use std::time::Duration;

use futures::StreamExt;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::env_api_keys::get_env_api_key;
use crate::models::clamp_thinking_level;
use crate::types::{
	Api, AssistantMessage, AssistantMessageEvent, Context, Model, ProviderResponse, SimpleStreamOptions,
	StreamFunction, StreamOptions, Usage,
};
use crate::utils::event_stream::{create_assistant_message_event_stream, AssistantMessageEventStream};
use crate::utils::stream_failure::{
	format_stream_failure_message, record_stream_failure, stream_failure_from_stop_reason,
};

use super::openai_responses_shared::{
	convert_responses_messages, convert_responses_tools, process_responses_stream, OpenAIResponsesStreamOptions,
	ResponsesEventStream, ResponsesStreamError,
};
use super::simple_options::build_base_options;

const DEFAULT_AZURE_API_VERSION: &str = "v1";

/// TS: `AZURE_TOOL_CALL_PROVIDERS`
fn azure_tool_call_providers(provider: &str) -> bool {
	matches!(provider, "openai" | "openai-codex" | "opencode" | "azure-openai-responses")
}

/// TS: `parseDeploymentNameMap(value)`
fn parse_deployment_name_map(value: Option<&str>) -> IndexMap<String, String> {
	let mut map: IndexMap<String, String> = IndexMap::new();
	let Some(value) = value else {
		return map;
	};
	for entry in value.split(',') {
		let trimmed = entry.trim();
		if trimmed.is_empty() {
			continue;
		}
		let mut parts = trimmed.splitn(2, '=');
		let model_id = parts.next().unwrap_or("");
		let deployment_name = parts.next().unwrap_or("");
		if model_id.is_empty() || deployment_name.is_empty() {
			continue;
		}
		map.insert(model_id.trim().to_string(), deployment_name.trim().to_string());
	}
	map
}

/// TS: `resolveDeploymentName(model, options?)`
fn resolve_deployment_name(model: &Model, options: Option<&AzureOpenAIResponsesOptions>) -> String {
	if let Some(name) = options.and_then(|options| options.azure_deployment_name.clone()) {
		if !name.is_empty() {
			return name;
		}
	}
	let env_value = std::env::var("AZURE_OPENAI_DEPLOYMENT_NAME_MAP").ok();
	let mapped = parse_deployment_name_map(env_value.as_deref()).get(&model.id).cloned();
	mapped.unwrap_or_else(|| model.id.clone())
}

/// TS: `AzureOpenAIResponsesOptions extends StreamOptions`
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AzureOpenAIResponsesOptions {
	#[serde(flatten)]
	pub stream: StreamOptions,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub reasoning_effort: Option<String>,
	/// `reasoningSummary?: "auto" | "detailed" | "concise" | null`
	#[serde(skip_serializing_if = "Option::is_none")]
	pub reasoning_summary: Option<Option<String>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub azure_api_version: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub azure_resource_name: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub azure_base_url: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub azure_deployment_name: Option<String>,
}

impl std::fmt::Debug for AzureOpenAIResponsesOptions {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter
			.debug_struct("AzureOpenAIResponsesOptions")
			.field("stream", &self.stream)
			.field("reasoning_effort", &self.reasoning_effort)
			.field("reasoning_summary", &self.reasoning_summary)
			.field("azure_api_version", &self.azure_api_version)
			.field("azure_resource_name", &self.azure_resource_name)
			.field("azure_base_url", &self.azure_base_url)
			.field("azure_deployment_name", &self.azure_deployment_name)
			.finish()
	}
}

impl AzureOpenAIResponsesOptions {
	/// TS: the caller passes `StreamOptions`; keep the non-serialisable fields.
	pub fn from_base(base: &StreamOptions) -> Self {
		Self {
			stream: base.clone(),
			reasoning_effort: None,
			reasoning_summary: None,
			azure_api_version: None,
			azure_resource_name: None,
			azure_base_url: None,
			azure_deployment_name: None,
		}
	}
}

/// TS: `streamAzureOpenAIResponses`
pub fn stream_azure_openai_responses(
	model: &Model,
	context: &Context,
	options: Option<AzureOpenAIResponsesOptions>,
) -> AssistantMessageEventStream {
	let stream = create_assistant_message_event_stream();
	let out = stream.clone();
	let model = model.clone();
	let context = context.clone();
	let options = options.unwrap_or_default();
	tokio::spawn(async move {
		let deployment_name = resolve_deployment_name(&model, Some(&options));

		let mut output = AssistantMessage::new(
			"azure-openai-responses".to_string(),
			model.provider.clone(),
			model.id.clone(),
			crate::utils::now_ms(),
		);
		output.usage = Usage::zero();
		output.stop_reason = "stop".to_string();

		match run_azure_openai_responses(&model, &context, &options, &deployment_name, &mut output, &out).await {
			Ok(()) => {}
			Err(error) => {
				// `partialJson` is only a streaming scratch buffer; never persist it.
				// (The port keeps it outside the block, so there is nothing to delete.)
				let aborted = options
					.stream
					.signal
					.as_ref()
					.map(|signal| signal.is_cancelled())
					.unwrap_or(false);
				output.stop_reason = if aborted { "aborted".to_string() } else { "error".to_string() };
				let thrown = error.to_thrown();
				output.error_message = Some(format_stream_failure_message(&thrown));
				record_stream_failure(&model, &mut output, &thrown);
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

/// The TypeScript async IIFE body of `streamAzureOpenAIResponses`.
async fn run_azure_openai_responses(
	model: &Model,
	context: &Context,
	options: &AzureOpenAIResponsesOptions,
	deployment_name: &str,
	output: &mut AssistantMessage,
	stream: &AssistantMessageEventStream,
) -> Result<(), ResponsesStreamError> {
	let api_key = options
		.stream
		.api_key
		.clone()
		.or_else(|| get_env_api_key(&model.provider))
		.unwrap_or_default();
	let client = create_client(model, &api_key, Some(options)).map_err(ResponsesStreamError::Message)?;
	let mut params = build_params(model, context, Some(options), deployment_name);

	if let Some(on_payload) = options.stream.on_payload.clone() {
		let next_params = on_payload(Value::Object(params.clone()), model).await;
		if let Some(next_params) = next_params {
			params = match next_params {
				Value::Object(map) => map,
				_ => Map::new(),
			};
		}
	}

	let response = send_request(&client, &params, options).await?;
	if let Some(on_response) = options.stream.on_response.clone() {
		on_response(
			ProviderResponse {
				status: response.status().as_u16() as i64,
				headers: crate::utils::headers::header_map_to_record(response.headers()),
			},
			model,
		)
		.await;
	}
	let request_id = response
		.headers()
		.get("x-request-id")
		.and_then(|value| value.to_str().ok())
		.map(str::to_string);

	stream.push(AssistantMessageEvent::Start {
		partial: output.clone(),
	});

	let events: ResponsesEventStream = Box::pin(
		response
			.bytes_stream()
			.scan(super::openai_responses::SseBuffer::default(), |buffer, chunk| {
				let ready = match chunk {
					Ok(bytes) => buffer.push(&bytes),
					Err(error) => {
						buffer.error = Some(error.to_string());
						Vec::new()
					}
				};
				futures::future::ready(Some(ready))
			})
			.flat_map(futures::stream::iter),
	);
	process_responses_stream(
		events,
		output,
		stream,
		model,
		Some(&OpenAIResponsesStreamOptions {
			on_usage_observation: options.stream.on_usage_observation.clone(),
			..Default::default()
		}),
	)
	.await?;

	if options
		.stream
		.signal
		.as_ref()
		.map(|signal| signal.is_cancelled())
		.unwrap_or(false)
	{
		return Err(ResponsesStreamError::Message("Request was aborted".to_string()));
	}

	if output.stop_reason == "aborted" || output.stop_reason == "error" {
		return Err(ResponsesStreamError::StreamFailure(stream_failure_from_stop_reason(
			output.stop_reason_raw.as_deref(),
			request_id.as_deref(),
		)));
	}

	stream.push(AssistantMessageEvent::Done {
		reason: output.stop_reason.clone(),
		message: output.clone(),
	});
	stream.end(None);
	Ok(())
}

/// TS: `streamSimpleAzureOpenAIResponses`
pub fn stream_simple_azure_openai_responses(
	model: &Model,
	context: &Context,
	options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
	let stream = create_assistant_message_event_stream();
	let out = stream.clone();
	let model_owned = model.clone();
	let context_owned = context.clone();
	let options = options.unwrap_or_default();
	tokio::spawn(async move {
		let api_key = options
			.stream
			.api_key
			.clone()
			.or_else(|| get_env_api_key(&model_owned.provider));
		let Some(api_key) = api_key else {
			let mut output = AssistantMessage::new(
				"azure-openai-responses".to_string(),
				model_owned.provider.clone(),
				model_owned.id.clone(),
				crate::utils::now_ms(),
			);
			output.usage = Usage::zero();
			output.stop_reason = "error".to_string();
			output.error_message = Some(format!("No API key for provider: {}", model_owned.provider));
			out.push(AssistantMessageEvent::Error {
				reason: "error".to_string(),
				error: output.clone(),
			});
			out.end(None);
			return;
		};

		let base = build_base_options(&model_owned, Some(&options), Some(&api_key));
		let clamped_reasoning = options
			.reasoning
			.as_ref()
			.map(|level| clamp_thinking_level(&model_owned, level));
		let reasoning_effort = match clamped_reasoning.as_deref() {
			Some("off") | None => None,
			Some(level) => Some(level.to_string()),
		};

		let typed = AzureOpenAIResponsesOptions {
			reasoning_effort,
			..AzureOpenAIResponsesOptions::from_base(&base)
		};
		let inner = stream_azure_openai_responses(&model_owned, &context_owned, Some(typed));
		while let Some(event) = inner.next().await {
			out.push(event);
		}
		out.end(None);
	});
	stream
}

/// TS: `normalizeAzureBaseUrl(baseUrl)`
fn normalize_azure_base_url(base_url: &str) -> Result<String, String> {
	let trimmed = base_url.trim().trim_end_matches('/');
	let mut url = match url::Url::parse(trimmed) {
		Ok(url) => url,
		Err(_) => return Err(format!("Invalid Azure OpenAI base URL: {base_url}")),
	};

	let host = url.host_str().unwrap_or_default().to_string();
	let is_azure_host = host.ends_with(".openai.azure.com") || host.ends_with(".cognitiveservices.azure.com");
	let normalized_path = url.path().trim_end_matches('/').to_string();

	// Ensure Azure hosts have /openai/v1 as base path so the AzureOpenAI SDK
	// can append /deployments/<model>/... and ?api-version=v1 correctly.
	if is_azure_host && (normalized_path.is_empty() || normalized_path == "/" || normalized_path == "/openai") {
		url.set_path("/openai/v1");
		url.set_query(None);
	}

	Ok(url.to_string().trim_end_matches('/').to_string())
}

/// TS: `buildDefaultBaseUrl(resourceName)`
fn build_default_base_url(resource_name: &str) -> String {
	format!("https://{resource_name}.openai.azure.com/openai/v1")
}

/// TS: `resolveAzureConfig(model, options?)`
fn resolve_azure_config(
	model: &Model,
	options: Option<&AzureOpenAIResponsesOptions>,
) -> Result<(String, String), String> {
	let api_version = options
		.and_then(|options| options.azure_api_version.clone())
		.filter(|value| !value.is_empty())
		.or_else(|| std::env::var("AZURE_OPENAI_API_VERSION").ok())
		.unwrap_or_else(|| DEFAULT_AZURE_API_VERSION.to_string());

	let base_url = options
		.and_then(|options| options.azure_base_url.clone())
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
		.or_else(|| {
			std::env::var("AZURE_OPENAI_BASE_URL")
				.ok()
				.map(|value| value.trim().to_string())
				.filter(|value| !value.is_empty())
		});
	let resource_name = options
		.and_then(|options| options.azure_resource_name.clone())
		.filter(|value| !value.is_empty())
		.or_else(|| std::env::var("AZURE_OPENAI_RESOURCE_NAME").ok());

	let mut resolved_base_url = base_url;

	if resolved_base_url.is_none() {
		if let Some(resource_name) = resource_name.as_deref() {
			resolved_base_url = Some(build_default_base_url(resource_name));
		}
	}

	if resolved_base_url.is_none() && !model.base_url.is_empty() {
		resolved_base_url = Some(model.base_url.clone());
	}

	let Some(resolved_base_url) = resolved_base_url else {
		return Err(
			"Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl.".to_string(),
		);
	};

	Ok((normalize_azure_base_url(&resolved_base_url)?, api_version))
}

/// TS: the `AzureOpenAI` client fields the provider uses.
#[derive(Clone, Debug)]
pub struct AzureClient {
	pub api_key: String,
	pub base_url: String,
	pub api_version: String,
	pub default_headers: IndexMap<String, String>,
}

/// TS: `createClient(model, apiKey, options?)`
fn create_client(
	model: &Model,
	api_key: &str,
	options: Option<&AzureOpenAIResponsesOptions>,
) -> Result<AzureClient, String> {
	let mut api_key = api_key.to_string();
	if api_key.is_empty() {
		match std::env::var("AZURE_OPENAI_API_KEY") {
			Ok(value) if !value.is_empty() => api_key = value,
			_ => {
				return Err(
					"Azure OpenAI API key is required. Set AZURE_OPENAI_API_KEY environment variable or pass it as an argument.".to_string(),
				)
			}
		}
	}

	let mut headers = model.headers.clone().unwrap_or_default();

	if let Some(options) = options {
		if let Some(option_headers) = &options.stream.headers {
			for (key, value) in option_headers {
				headers.insert(key.clone(), value.clone());
			}
		}
	}

	let (base_url, api_version) = resolve_azure_config(model, options)?;

	Ok(AzureClient {
		api_key,
		base_url,
		api_version,
		default_headers: headers,
	})
}

/// TS: `buildParams(model, context, options, deploymentName)`
fn build_params(
	model: &Model,
	context: &Context,
	options: Option<&AzureOpenAIResponsesOptions>,
	deployment_name: &str,
) -> Map<String, Value> {
	let messages = convert_responses_messages(model, context, &azure_tool_call_providers, None)
		.unwrap_or_else(|_| Vec::new());

	let mut params: Map<String, Value> = Map::new();
	params.insert("model".to_string(), Value::String(deployment_name.to_string()));
	params.insert("input".to_string(), Value::Array(messages));
	params.insert("stream".to_string(), Value::Bool(true));
	if let Some(session_id) = options.and_then(|options| options.stream.session_id.clone()) {
		params.insert("prompt_cache_key".to_string(), Value::String(session_id));
	}

	if let Some(max_tokens) = options.and_then(|options| options.stream.max_tokens) {
		params.insert("max_output_tokens".to_string(), serde_json::json!(max_tokens));
	}

	if let Some(temperature) = options.and_then(|options| options.stream.temperature) {
		params.insert("temperature".to_string(), serde_json::json!(temperature));
	}

	if let Some(tools) = &context.tools {
		if !tools.is_empty() {
			params.insert(
				"tools".to_string(),
				Value::Array(convert_responses_tools(tools, None)),
			);
		}
	}

	if model.reasoning {
		let reasoning_effort = options.and_then(|options| options.reasoning_effort.clone());
		let reasoning_summary = options.and_then(|options| options.reasoning_summary.clone());
		let has_summary = matches!(reasoning_summary, Some(Some(_)));
		if reasoning_effort.is_some() || has_summary {
			let effort = match reasoning_effort {
				Some(effort) => model.thinking_level_map_get(&effort).unwrap_or(Some(effort)).unwrap_or_else(|| "none".to_string()),
				None => "medium".to_string(),
			};
			let summary = match reasoning_summary {
				Some(Some(summary)) => summary,
				_ => "auto".to_string(),
			};
			let mut reasoning = Map::new();
			reasoning.insert("effort".to_string(), Value::String(effort));
			reasoning.insert("summary".to_string(), Value::String(summary));
			params.insert("reasoning".to_string(), Value::Object(reasoning));
			params.insert(
				"include".to_string(),
				Value::Array(vec![Value::String("reasoning.encrypted_content".to_string())]),
			);
		} else if model.thinking_level_map_get("off") != Some(None) {
			let effort = model
				.thinking_level_map_get("off")
				.flatten()
				.unwrap_or_else(|| "none".to_string());
			let mut reasoning = Map::new();
			reasoning.insert("effort".to_string(), Value::String(effort));
			params.insert("reasoning".to_string(), Value::Object(reasoning));
		}
	}

	params
}

/// The TypeScript calls `client.responses.create(params, { signal, timeout }).withResponse()`.
/// The Rust port builds the Azure request itself: the AzureOpenAI SDK prefixes `/deployments/<model>`
/// for the deployment endpoints and appends `?api-version=<version>`, and authenticates with
/// the `api-key` header.
async fn send_request(
	client: &AzureClient,
	params: &Map<String, Value>,
	options: &AzureOpenAIResponsesOptions,
) -> Result<reqwest::Response, ResponsesStreamError> {
	let base_url = client.base_url.trim_end_matches('/');
	let model = params.get("model").and_then(Value::as_str).unwrap_or_default();
	// `_deployments_endpoints` in the OpenAI Azure SDK does not list `/responses`, so the SDK
	// does not insert the deployment path for this route.
	let url = format!("{base_url}/responses?api-version={}", client.api_version);

	let mut headers = reqwest::header::HeaderMap::new();
	for (key, value) in client.default_headers.iter() {
		if let (Ok(name), Ok(header_value)) = (
			reqwest::header::HeaderName::from_bytes(key.as_bytes()),
			reqwest::header::HeaderValue::from_str(value),
		) {
			headers.insert(name, header_value);
		}
	}
	if !headers.contains_key("api-key") {
		if let Ok(value) = reqwest::header::HeaderValue::from_str(&client.api_key) {
			headers.insert(reqwest::header::HeaderName::from_static("api-key"), value);
		}
	}
	let _ = model;

	let mut request = reqwest::Client::new()
		.post(&url)
		.headers(headers)
		.json(&Value::Object(params.clone()));
	if let Some(timeout_ms) = options.stream.timeout_ms {
		request = request.timeout(Duration::from_millis(timeout_ms.max(0.0) as u64));
	}

	let send = request.send();
	let response = match options.stream.signal.as_ref() {
		Some(signal) => tokio::select! {
			_ = signal.cancelled() => return Err(ResponsesStreamError::Message("Request was aborted".to_string())),
			result = send => result,
		},
		None => send.await,
	};
	let response = response.map_err(|error| ResponsesStreamError::Message(error.to_string()))?;
	if !response.status().is_success() {
		let status = response.status();
		let text = response.text().await.unwrap_or_default();
		return Err(ResponsesStreamError::Message(format!("{}: {}", status.as_u16(), text)));
	}
	Ok(response)
}

/// Re-exported alias so `register_builtins.rs` can name the api without a type import.
pub type AzureOpenAIResponsesStreamFunction = StreamFunction;

/// Kept so the module references the same `Api` alias the TypeScript module does.
pub fn _api_marker(_api: &Api) {}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::types::{InputModality, Message, Tool, UserContent, UserMessage};

	fn model(provider: &str, id: &str, base_url: &str) -> Model {
		Model {
			id: id.to_string(),
			provider: provider.to_string(),
			api: "azure-openai-responses".to_string(),
			base_url: base_url.to_string(),
			input: vec![InputModality::Text],
			..Default::default()
		}
	}

	fn context() -> Context {
		Context {
			system_prompt: None,
			messages: vec![Message::user(UserMessage::new(UserContent::Text("hi".to_string()), 0))],
			tools: None,
		}
	}

	#[test]
	fn parse_deployment_name_map_matches_typescript() {
		let map = parse_deployment_name_map(Some(" gpt-4=deploy-a , broken , =x , gpt-5 = deploy-b "));
		assert_eq!(map.get("gpt-4").map(String::as_str), Some("deploy-a"));
		assert_eq!(map.get("gpt-5").map(String::as_str), Some("deploy-b"));
		assert_eq!(map.len(), 2);
		assert!(parse_deployment_name_map(None).is_empty());
		assert!(parse_deployment_name_map(Some("")).is_empty());
	}

	#[test]
	fn resolve_deployment_name_prefers_option_then_env_then_model() {
		std::env::set_var("AZURE_OPENAI_DEPLOYMENT_NAME_MAP", "model-1=deploy-env");
		let model = model("azure-openai-responses", "model-1", "https://example.openai.azure.com");
		assert_eq!(resolve_deployment_name(&model, None), "deploy-env");

		let options = AzureOpenAIResponsesOptions {
			azure_deployment_name: Some("explicit".to_string()),
			..Default::default()
		};
		assert_eq!(resolve_deployment_name(&model, Some(&options)), "explicit");

		let other = model("azure-openai-responses", "unknown-model", "https://example.openai.azure.com");
		assert_eq!(resolve_deployment_name(&other, None), "unknown-model");
		std::env::remove_var("AZURE_OPENAI_DEPLOYMENT_NAME_MAP");
	}

	#[test]
	fn normalize_azure_base_url_rewrites_azure_hosts() {
		assert_eq!(
			normalize_azure_base_url("https://res.openai.azure.com").unwrap(),
			"https://res.openai.azure.com/openai/v1"
		);
		assert_eq!(
			normalize_azure_base_url("https://res.openai.azure.com/").unwrap(),
			"https://res.openai.azure.com/openai/v1"
		);
		assert_eq!(
			normalize_azure_base_url("https://res.openai.azure.com/openai").unwrap(),
			"https://res.openai.azure.com/openai/v1"
		);
		assert_eq!(
			normalize_azure_base_url("https://res.cognitiveservices.azure.com/openai/v1/").unwrap(),
			"https://res.cognitiveservices.azure.com/openai/v1"
		);
	}

	#[test]
	fn normalize_azure_base_url_keeps_custom_paths_and_rejects_invalid() {
		assert_eq!(
			normalize_azure_base_url("https://gateway.example.com/azure-openai/v1").unwrap(),
			"https://gateway.example.com/azure-openai/v1"
		);
		assert_eq!(
			normalize_azure_base_url("https://res.openai.azure.com/custom").unwrap(),
			"https://res.openai.azure.com/custom"
		);
		assert_eq!(
			normalize_azure_base_url("not a url").unwrap_err(),
			"Invalid Azure OpenAI base URL: not a url"
		);
	}

	#[test]
	fn build_default_base_url_matches_typescript() {
		assert_eq!(
			build_default_base_url("my-resource"),
			"https://my-resource.openai.azure.com/openai/v1"
		);
	}

	#[test]
	fn resolve_azure_config_precedence() {
		std::env::remove_var("AZURE_OPENAI_BASE_URL");
		std::env::remove_var("AZURE_OPENAI_RESOURCE_NAME");
		std::env::remove_var("AZURE_OPENAI_API_VERSION");

		let model = model("azure-openai-responses", "m", "https://model.openai.azure.com");
		let (base_url, api_version) = resolve_azure_config(&model, None).unwrap();
		assert_eq!(base_url, "https://model.openai.azure.com/openai/v1");
		assert_eq!(api_version, DEFAULT_AZURE_API_VERSION);

		let options = AzureOpenAIResponsesOptions {
			azure_resource_name: Some("res".to_string()),
			azure_api_version: Some("2024-10-21".to_string()),
			..Default::default()
		};
		let (base_url, api_version) = resolve_azure_config(&model, Some(&options)).unwrap();
		assert_eq!(base_url, "https://res.openai.azure.com/openai/v1");
		assert_eq!(api_version, "2024-10-21");

		let options = AzureOpenAIResponsesOptions {
			azure_base_url: Some("  https://explicit.example.com/v1/  ".to_string()),
			..Default::default()
		};
		let (base_url, _) = resolve_azure_config(&model, Some(&options)).unwrap();
		assert_eq!(base_url, "https://explicit.example.com/v1");
	}

	#[test]
	fn resolve_azure_config_errors_without_any_base_url() {
		std::env::remove_var("AZURE_OPENAI_BASE_URL");
		std::env::remove_var("AZURE_OPENAI_RESOURCE_NAME");
		let model = model("azure-openai-responses", "m", "");
		let error = resolve_azure_config(&model, None).unwrap_err();
		assert_eq!(
			error,
			"Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl."
		);
	}

	#[test]
	fn create_client_requires_api_key() {
		std::env::remove_var("AZURE_OPENAI_API_KEY");
		let model = model("azure-openai-responses", "m", "https://res.openai.azure.com");
		let error = create_client(&model, "", None).unwrap_err();
		assert_eq!(
			error,
			"Azure OpenAI API key is required. Set AZURE_OPENAI_API_KEY environment variable or pass it as an argument."
		);
	}

	#[test]
	fn create_client_merges_model_and_option_headers() {
		let mut model = model("azure-openai-responses", "m", "https://res.openai.azure.com");
		let mut model_headers = IndexMap::new();
		model_headers.insert("X-Model".to_string(), "1".to_string());
		model.headers = Some(model_headers);
		let mut option_headers = IndexMap::new();
		option_headers.insert("X-Option".to_string(), "2".to_string());
		let options = AzureOpenAIResponsesOptions {
			stream: StreamOptions {
				headers: Some(option_headers),
				..Default::default()
			},
			..Default::default()
		};
		let client = create_client(&model, "key", Some(&options)).unwrap();
		assert_eq!(client.api_key, "key");
		assert_eq!(client.base_url, "https://res.openai.azure.com/openai/v1");
		assert_eq!(client.api_version, "v1");
		assert_eq!(client.default_headers.get("X-Model").map(String::as_str), Some("1"));
		assert_eq!(client.default_headers.get("X-Option").map(String::as_str), Some("2"));
	}

	#[test]
	fn build_params_matches_typescript_defaults() {
		let model = model("azure-openai-responses", "m", "https://res.openai.azure.com");
		let params = build_params(&model, &context(), None, "deployment-x");
		assert_eq!(params.get("model").and_then(Value::as_str), Some("deployment-x"));
		assert_eq!(params.get("stream").and_then(Value::as_bool), Some(true));
		assert!(params.get("input").and_then(Value::as_array).is_some());
		// `prompt_cache_key`, `max_output_tokens` and `temperature` are omitted when unset.
		assert!(!params.contains_key("prompt_cache_key"));
		assert!(!params.contains_key("max_output_tokens"));
		assert!(!params.contains_key("temperature"));
		assert!(!params.contains_key("tools"));
		assert!(!params.contains_key("reasoning"));
	}

	#[test]
	fn build_params_includes_session_tokens_temperature_and_tools() {
		let model = model("azure-openai-responses", "m", "https://res.openai.azure.com");
		let mut context = context();
		context.tools = Some(vec![Tool {
			name: "read".to_string(),
			description: "read".to_string(),
			parameters: serde_json::json!({"type": "object"}),
		}]);
		let options = AzureOpenAIResponsesOptions {
			stream: StreamOptions {
				session_id: Some("session-1".to_string()),
				max_tokens: Some(512.0),
				temperature: Some(0.25),
				..Default::default()
			},
			..Default::default()
		};
		let params = build_params(&model, &context, Some(&options), "m");
		assert_eq!(params.get("prompt_cache_key").and_then(Value::as_str), Some("session-1"));
		assert_eq!(params.get("max_output_tokens").and_then(Value::as_f64), Some(512.0));
		assert_eq!(params.get("temperature").and_then(Value::as_f64), Some(0.25));
		let tools = params.get("tools").and_then(Value::as_array).expect("tools");
		assert_eq!(tools.len(), 1);
		assert_eq!(tools[0].get("type").and_then(Value::as_str), Some("function"));
		assert_eq!(tools[0].get("name").and_then(Value::as_str), Some("read"));
		assert_eq!(tools[0].get("strict").and_then(Value::as_bool), Some(false));
	}

	#[test]
	fn build_params_reasoning_branches() {
		let mut model = model("azure-openai-responses", "m", "https://res.openai.azure.com");
		model.reasoning = true;
		let params = build_params(&model, &context(), None, "m");
		let reasoning = params.get("reasoning").expect("reasoning");
		assert_eq!(reasoning.get("effort").and_then(Value::as_str), Some("none"));
		assert!(!params.contains_key("include"));

		let options = AzureOpenAIResponsesOptions {
			reasoning_effort: Some("high".to_string()),
			reasoning_summary: Some(Some("concise".to_string())),
			..Default::default()
		};
		let params = build_params(&model, &context(), Some(&options), "m");
		let reasoning = params.get("reasoning").expect("reasoning");
		assert_eq!(reasoning.get("effort").and_then(Value::as_str), Some("high"));
		assert_eq!(reasoning.get("summary").and_then(Value::as_str), Some("concise"));
		assert_eq!(
			params.get("include").and_then(Value::as_array).map(|values| values.len()),
			Some(1)
		);
	}

	#[test]
	fn build_params_uses_thinking_level_map_for_effort() {
		let mut model = model("azure-openai-responses", "m", "https://res.openai.azure.com");
		model.reasoning = true;
		model.thinking_level_map = Some(
			[("high".to_string(), Some("high-mapped".to_string()))]
				.into_iter()
				.collect(),
		);
		let options = AzureOpenAIResponsesOptions {
			reasoning_effort: Some("high".to_string()),
			..Default::default()
		};
		let params = build_params(&model, &context(), Some(&options), "m");
		let reasoning = params.get("reasoning").expect("reasoning");
		assert_eq!(reasoning.get("effort").and_then(Value::as_str), Some("high-mapped"));
		// No summary requested -> the TS default "auto".
		assert_eq!(reasoning.get("summary").and_then(Value::as_str), Some("auto"));
	}

	#[test]
	fn from_base_keeps_non_serialisable_fields() {
		let base = StreamOptions {
			temperature: Some(0.5),
			signal: Some(tokio_util::sync::CancellationToken::new()),
			..Default::default()
		};
		let options = AzureOpenAIResponsesOptions::from_base(&base);
		assert_eq!(options.stream.temperature, Some(0.5));
		assert!(options.stream.signal.is_some());
		assert!(options.azure_base_url.is_none());
	}

	#[test]
	fn azure_tool_call_providers_matches_typescript_set() {
		for provider in ["openai", "openai-codex", "opencode", "azure-openai-responses"] {
			assert!(azure_tool_call_providers(provider), "{provider}");
		}
		assert!(!azure_tool_call_providers("anthropic"));
	}
}
