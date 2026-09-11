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
	let client = create_client(model, &api_key, options)?;
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
