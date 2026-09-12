//! Port of packages/ai/src/providers/openai-responses.ts

use std::sync::Arc;

use futures::StreamExt;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::compaction::{CompactionOptions, ProviderCompactionResult};
use crate::env_api_keys::get_env_api_key;
use crate::models::clamp_thinking_level;
use crate::providers::cloudflare::{is_cloudflare_provider, resolve_cloudflare_base_url};
use crate::providers::github_copilot_headers::{
    build_copilot_dynamic_headers, has_copilot_vision_input, CopilotDynamicHeaderParams,
};
use crate::providers::openai_compaction::{
    request_openai_compaction, supports_openai_compaction, validated_native_compaction_endpoint,
};
use crate::providers::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, process_responses_stream, ConvertResponsesMessagesOptions,
    OpenAIResponsesStreamOptions, ResponsesEventStream, ResponsesStreamError,
};
use crate::providers::opencode_headers::with_opencode_headers;
use crate::providers::simple_options::build_base_options;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, CacheRetention, Context, Model, ProviderResponse, SimpleStreamOptions,
    StreamOptions, Usage,
};
use crate::utils::event_stream::{
    create_assistant_message_event_stream, AssistantMessageEventStream,
};
use crate::utils::headers::header_map_to_record;
use crate::utils::now_ms;
use crate::utils::stream_failure::{
    format_stream_failure_message, record_stream_failure, stream_failure_from_stop_reason, StreamFailureError,
    ThrownStreamError,
};

/// `export interface OpenAIResponsesOptions extends StreamOptions`.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OpenAIResponsesOptions {
    #[serde(flatten)]
    pub stream: StreamOptions,
    /// `reasoningEffort?: "minimal" | "low" | "medium" | "high" | "xhigh" | "max"`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// `reasoningSummary?: "auto" | "detailed" | "concise" | null`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<Option<String>>,
    /// `serviceTier?: ResponseCreateParamsStreaming["service_tier"]`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

impl OpenAIResponsesOptions {
    /// TS: the caller passes `StreamOptions & Record<string, unknown>`; this keeps the
    /// non-serializable fields (signal, on_payload, on_response, on_usage_observation).
    pub fn from_base(base: &StreamOptions) -> Self {
        Self {
            stream: base.clone(),
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
        }
    }
}

/// `const OPENAI_TOOL_CALL_PROVIDERS = new Set(["openai", "openai-codex", "opencode"])`.
pub const OPENAI_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];
/// `const AZURE_MANAGED_COMPACTION_TOOL_CALL_PROVIDERS = new Set([...OPENAI_TOOL_CALL_PROVIDERS, "azure-openai-managed"])`.
pub const AZURE_MANAGED_COMPACTION_TOOL_CALL_PROVIDERS: [&str; 4] =
    ["openai", "openai-codex", "opencode", "azure-openai-managed"];

/// The Rust counterpart of a `throw` inside the TypeScript stream body.
enum RunError {
    Failure(StreamFailureError),
    Message(String),
}

impl RunError {
    fn thrown(&self) -> ThrownStreamError<'_> {
        match self {
            RunError::Failure(failure) => ThrownStreamError::Failure(failure),
            RunError::Message(message) => ThrownStreamError::Message(message),
        }
    }
}

impl From<String> for RunError {
    fn from(message: String) -> Self {
        RunError::Message(message)
    }
}

/// `compactOpenAIResponses: CompactFunction<"openai-responses">`.
pub async fn compact_openai_responses(
    model: &Model,
    context: &Context,
    options: Option<&CompactionOptions>,
) -> Option<ProviderCompactionResult> {
    try_compact_openai_responses(model, context, options)
        .await
        .unwrap_or(None)
}

/// [`compact_openai_responses`] with the TypeScript `throw` turned into `Err`.
pub async fn try_compact_openai_responses(
    model: &Model,
    context: &Context,
    options: Option<&CompactionOptions>,
) -> Result<Option<ProviderCompactionResult>, String> {
    if !supports_openai_compaction(model) {
        return Ok(None);
    }
    let native_endpoint = validated_native_compaction_endpoint(model);
    let tool_call_providers: &[&str] = if native_endpoint.is_some() {
        &AZURE_MANAGED_COMPACTION_TOOL_CALL_PROVIDERS
    } else {
        &OPENAI_TOOL_CALL_PROVIDERS
    };
    let api_key = options
        .and_then(|options| options.simple.stream.api_key.clone())
        .or_else(|| get_env_api_key(&model.provider));
    let Some(api_key) = api_key else {
        return Err(format!("No API key for provider: {}", model.provider));
    };
    let mut headers: IndexMap<String, String> = model.headers.clone().unwrap_or_default();
    if let Some(options_headers) = options.and_then(|options| options.simple.stream.headers.clone()) {
        for (key, value) in options_headers {
            headers.insert(key, value);
        }
    }
    headers.insert("Authorization".to_string(), format!("Bearer {}", api_key));
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    let instructions = [
        context.system_prompt.clone(),
        options.and_then(|options| options.custom_instructions.clone()),
    ]
    .into_iter()
    .flatten()
    .filter(|instruction| !instruction.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    let url = native_endpoint
        .clone()
        .unwrap_or_else(|| format!("{}/responses/compact", model.base_url.trim_end_matches('/')));

    // `JSON.stringify` drops `undefined` keys, so absent options are omitted while an
    // explicit `null` service tier is kept.
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.id.clone()));
    body.insert(
        "input".to_string(),
        Value::Array(convert_responses_messages(
            model,
            context,
            &|provider: &str| tool_call_providers.contains(&provider),
            Some(&ConvertResponsesMessagesOptions {
                include_system_prompt: Some(false),
            }),
        )?),
    );
    body.insert("instructions".to_string(), Value::String(instructions));
    if let Some(session_id) = options.and_then(|options| options.simple.stream.session_id.clone()) {
        body.insert("prompt_cache_key".to_string(), Value::String(session_id));
    }
    match options.and_then(|options| options.simple.stream.service_tier.clone()) {
        Some(Some(service_tier)) => {
            body.insert("service_tier".to_string(), Value::String(service_tier));
        }
        Some(None) => {
            body.insert("service_tier".to_string(), Value::Null);
        }
        None => {}
    }

    let mut result = request_openai_compaction(model, &url, &headers, body, options, None)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(result) = result.as_mut() {
        if let Some(usage) = result.usage.as_mut() {
            let service_tier = options
                .and_then(|options| options.simple.stream.service_tier.clone())
                .flatten();
            apply_service_tier_pricing(usage, service_tier.as_deref(), model);
        }
    }
    Ok(result)
}

/// `resolveCacheRetention(cacheRetention?)`.
pub fn resolve_cache_retention(cache_retention: Option<&CacheRetention>) -> CacheRetention {
    if let Some(cache_retention) = cache_retention {
        return cache_retention.clone();
    }
    if std::env::var("PI_CACHE_RETENTION").ok().as_deref() == Some("long") {
        return "long".to_string();
    }
    "short".to_string()
}

/// `getCompat(model): Required<OpenAIResponsesCompat>` - returns
/// `(sendSessionIdHeader, supportsLongCacheRetention)`.
pub fn get_compat(model: &Model) -> (bool, bool) {
    let compat = model.compat_responses();
    (
        compat.and_then(|compat| compat.send_session_id_header).unwrap_or(true),
        compat
            .and_then(|compat| compat.supports_long_cache_retention)
            .unwrap_or(true),
    )
}

/// `getPromptCacheRetention(compat, cacheRetention)`.
pub fn get_prompt_cache_retention(
    supports_long_cache_retention: bool,
    cache_retention: &CacheRetention,
) -> Option<String> {
    if cache_retention == "long" && supports_long_cache_retention {
        Some("24h".to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// streamOpenAIResponses
// ---------------------------------------------------------------------------

/// `streamOpenAIResponses: StreamFunction<"openai-responses", OpenAIResponsesOptions>`.
pub fn stream_openai_responses(
    model: &Model,
    context: &Context,
    options: Option<OpenAIResponsesOptions>,
) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let out = stream.clone();
    let model = model.clone();
    let context = context.clone();
    let options = options.unwrap_or_default();
    tokio::spawn(async move {
        let mut output = AssistantMessage::new(model.api.clone(), model.provider.clone(), model.id.clone(), now_ms());
        output.usage = Usage::zero();

        match run_openai_responses(&model, &context, &options, &mut output, &out).await {
            Ok(()) => {}
            Err(error) => {
                // partialJson is only a streaming scratch buffer; never persist it.
                // (The port keeps it outside the block, so there is nothing to delete.)
                let aborted = options
                    .stream
                    .signal
                    .as_ref()
                    .map(|signal| signal.is_cancelled())
                    .unwrap_or(false);
                output.stop_reason = if aborted { "aborted".to_string() } else { "error".to_string() };
                output.error_message = Some(format_stream_failure_message(&error.thrown()));
                record_stream_failure(&model, &mut output, &error.thrown());
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

/// The TypeScript async IIFE body of `streamOpenAIResponses`.
async fn run_openai_responses(
    model: &Model,
    context: &Context,
    options: &OpenAIResponsesOptions,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) -> Result<(), RunError> {
    let api_key = options
        .stream
        .api_key
        .clone()
        .or_else(|| get_env_api_key(&model.provider))
        .unwrap_or_default();
    let cache_retention = resolve_cache_retention(options.stream.cache_retention.as_ref());
    let cache_session_id = if cache_retention == "none" {
        None
    } else {
        options.stream.session_id.clone()
    };
    let client = create_client(
        model,
        context,
        Some(&api_key),
        options.stream.headers.as_ref(),
        cache_session_id.as_deref(),
        options.stream.session_id.as_deref(),
    )?;

    let mut params = build_params(model, context, Some(options));
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
                headers: header_map_to_record(response.headers()),
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
            .scan(SseBuffer::default(), |buffer, chunk| {
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

    let stream_options = OpenAIResponsesStreamOptions {
        on_output_item_done: None,
        service_tier: options.service_tier.clone(),
        resolve_service_tier: None,
        apply_service_tier_pricing: Some(Arc::new({
            let model = model.clone();
            move |usage: &mut Usage, service_tier: Option<&str>| {
                apply_service_tier_pricing(usage, service_tier, &model)
            }
        })),
        on_usage_observation: options.stream.on_usage_observation.clone(),
    };

    process_responses_stream(events, output, stream, model, Some(&stream_options))
        .await
        .map_err(|error| match error {
            ResponsesStreamError::StreamFailure(failure) => RunError::Failure(failure),
            ResponsesStreamError::Message(message) => RunError::Message(message),
        })?;

    if options
        .stream
        .signal
        .as_ref()
        .map(|signal| signal.is_cancelled())
        .unwrap_or(false)
    {
        return Err(RunError::Message("Request was aborted".to_string()));
    }

    if output.stop_reason == "aborted" || output.stop_reason == "error" {
        return Err(RunError::Failure(stream_failure_from_stop_reason(
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

/// `streamSimpleOpenAIResponses: StreamFunction<"openai-responses", SimpleStreamOptions>`.
pub fn stream_simple_openai_responses(
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
    let reasoning_effort = clamped_reasoning.filter(|reasoning| reasoning != "off");

    let mut typed = OpenAIResponsesOptions::from_base(&base);
    typed.reasoning_effort = reasoning_effort;
    stream_openai_responses(model, context, Some(typed))
}

// ---------------------------------------------------------------------------
// Client / params
// ---------------------------------------------------------------------------

/// `createClient(model, context, apiKey?, optionsHeaders?, cacheSessionId?, conversationId?)`
/// returns the SDK client; the port returns the request description it builds.
pub struct ResponsesClient {
    pub api_key: String,
    pub base_url: String,
    pub default_headers: IndexMap<String, Option<String>>,
}

/// `createClient(...)`.
pub fn create_client(
    model: &Model,
    context: &Context,
    api_key: Option<&str>,
    options_headers: Option<&IndexMap<String, String>>,
    cache_session_id: Option<&str>,
    conversation_id: Option<&str>,
) -> Result<ResponsesClient, String> {
    let mut api_key = api_key.map(str::to_string);
    if api_key.as_deref().map(str::is_empty).unwrap_or(true) {
        match std::env::var("OPENAI_API_KEY") {
            Ok(value) if !value.is_empty() => api_key = Some(value),
            _ => {
                return Err(
                    "OpenAI API key is required. Set OPENAI_API_KEY environment variable or pass it as an argument."
                        .to_string(),
                )
            }
        }
    }
    let api_key = api_key.unwrap_or_default();

    let (send_session_id_header, _supports_long_cache_retention) = get_compat(model);
    let mut headers: IndexMap<String, Option<String>> = IndexMap::new();
    if let Some(model_headers) = model.headers.as_ref() {
        for (key, value) in model_headers {
            headers.insert(key.clone(), Some(value.clone()));
        }
    }
    if model.provider == "github-copilot" {
        let has_images = has_copilot_vision_input(&context.messages);
        let copilot_headers = build_copilot_dynamic_headers(CopilotDynamicHeaderParams {
            messages: &context.messages,
            has_images,
        });
        for (key, value) in copilot_headers {
            headers.insert(key, Some(value));
        }
    }

    if let Some(cache_session_id) = cache_session_id {
        if send_session_id_header {
            headers.insert("session_id".to_string(), Some(cache_session_id.to_string()));
        }
        headers.insert("x-client-request-id".to_string(), Some(cache_session_id.to_string()));
    }

    if let Some(options_headers) = options_headers {
        for (key, value) in options_headers {
            headers.insert(key.clone(), Some(value.clone()));
        }
    }

    let default_headers = if model.provider == "cloudflare-ai-gateway" {
        let mut headers = headers.clone();
        if !headers.contains_key("Authorization") {
            headers.insert("Authorization".to_string(), None);
        }
        headers.insert(
            "cf-aig-authorization".to_string(),
            Some(format!("Bearer {}", api_key)),
        );
        headers
    } else {
        headers
    };

    let base_url = if is_cloudflare_provider(&model.provider) {
        resolve_cloudflare_base_url(model)?
    } else {
        model.base_url.clone()
    };

    Ok(ResponsesClient {
        api_key,
        base_url,
        default_headers: with_opencode_headers(&model.provider, conversation_id, &default_headers),
    })
}

/// `buildParams(model, context, options?)`.
pub fn build_params(model: &Model, context: &Context, options: Option<&OpenAIResponsesOptions>) -> Map<String, Value> {
    let messages = convert_responses_messages(
        model,
        context,
        &|provider: &str| OPENAI_TOOL_CALL_PROVIDERS.contains(&provider),
        None,
    )
    .unwrap_or_default();

    let cache_retention = resolve_cache_retention(options.and_then(|options| options.stream.cache_retention.as_ref()));
    let (_send_session_id_header, supports_long_cache_retention) = get_compat(model);
    let mut params = Map::new();
    params.insert("model".to_string(), Value::String(model.id.clone()));
    params.insert("input".to_string(), Value::Array(messages));
    params.insert("stream".to_string(), Value::Bool(true));
    if cache_retention != "none" {
        if let Some(session_id) = options.and_then(|options| options.stream.session_id.clone()) {
            params.insert("prompt_cache_key".to_string(), Value::String(session_id));
        }
    }
    match get_prompt_cache_retention(supports_long_cache_retention, &cache_retention) {
        Some(retention) => {
            params.insert("prompt_cache_retention".to_string(), Value::String(retention));
        }
        None => {
            params.insert("prompt_cache_retention".to_string(), Value::Null);
        }
    }
    params.insert("store".to_string(), Value::Bool(false));

    if let Some(max_tokens) = options.and_then(|options| options.stream.max_tokens) {
        if max_tokens != 0.0 {
            params.insert(
                "max_output_tokens".to_string(),
                serde_json::Number::from_f64(max_tokens)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
            );
        }
    }

    if let Some(temperature) = options.and_then(|options| options.stream.temperature) {
        params.insert(
            "temperature".to_string(),
            serde_json::Number::from_f64(temperature)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
    }

    // GitHub Copilot rejects the service_tier FIELD itself (400) for every value.
    // Elsewhere it is always sent: absence means "auto" (project tier), not "default".
    if let Some(service_tier) = options.and_then(|options| options.service_tier.clone()) {
        if model.provider != "github-copilot" {
            params.insert("service_tier".to_string(), Value::String(service_tier));
        }
    }

    if let Some(tools) = context.tools.as_ref() {
        if !tools.is_empty() {
            params.insert("tools".to_string(), Value::Array(convert_responses_tools(tools, None)));
        }
    }

    if model.reasoning {
        let reasoning_effort = options.and_then(|options| options.reasoning_effort.clone());
        let reasoning_summary = options.and_then(|options| options.reasoning_summary.clone()).flatten();
        if reasoning_effort.is_some() || reasoning_summary.is_some() {
            let effort = match reasoning_effort {
                Some(effort) => model
                    .thinking_level_map_get(&effort)
                    .flatten()
                    .unwrap_or(effort),
                None => "medium".to_string(),
            };
            let mut reasoning = Map::new();
            reasoning.insert("effort".to_string(), Value::String(effort));
            reasoning.insert(
                "summary".to_string(),
                Value::String(reasoning_summary.unwrap_or_else(|| "auto".to_string())),
            );
            params.insert("reasoning".to_string(), Value::Object(reasoning));
            params.insert(
                "include".to_string(),
                Value::Array(vec![Value::String("reasoning.encrypted_content".to_string())]),
            );
        } else if model.provider != "github-copilot"
            && !matches!(model.thinking_level_map_get("off"), Some(None))
        {
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

/// `getServiceTierCostMultiplier(model, serviceTier)`.
pub fn get_service_tier_cost_multiplier(model: &Model, service_tier: Option<&str>) -> f64 {
    match service_tier {
        Some("flex") => 0.5,
        Some("priority") => {
            if model.id == "gpt-5.5" {
                2.5
            } else {
                2.0
            }
        }
        _ => 1.0,
    }
}

/// `applyServiceTierPricing(usage, serviceTier, model)`.
pub fn apply_service_tier_pricing(usage: &mut Usage, service_tier: Option<&str>, model: &Model) {
    let multiplier = get_service_tier_cost_multiplier(model, service_tier);
    if multiplier == 1.0 {
        return;
    }

    usage.cost.input *= multiplier;
    usage.cost.output *= multiplier;
    usage.cost.cache_read *= multiplier;
    usage.cost.cache_write *= multiplier;
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
}

/// `client.responses.create(params, requestOptions).withResponse()`.
async fn send_request(
    client: &ResponsesClient,
    params: &Map<String, Value>,
    options: &OpenAIResponsesOptions,
) -> Result<reqwest::Response, RunError> {
    let url = format!("{}/responses", client.base_url.trim_end_matches('/'));
    let mut headers = reqwest::header::HeaderMap::new();
    for (key, value) in client.default_headers.iter() {
        let Some(value) = value else {
            continue;
        };
        if let (Ok(name), Ok(header_value)) = (
            reqwest::header::HeaderName::from_bytes(key.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            headers.insert(name, header_value);
        }
    }
    if !headers.contains_key(reqwest::header::AUTHORIZATION) {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", client.api_key)) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
    }

    let mut request = reqwest::Client::new()
        .post(&url)
        .headers(headers)
        .json(&Value::Object(params.clone()));
    if let Some(timeout_ms) = options.stream.timeout_ms {
        request = request.timeout(std::time::Duration::from_millis(timeout_ms.max(0.0) as u64));
    }

    let send = request.send();
    let response = match options.stream.signal.as_ref() {
        Some(signal) => tokio::select! {
            _ = signal.cancelled() => return Err(RunError::Message("Request was aborted".to_string())),
            result = send => result,
        },
        None => send.await,
    };
    let response = response.map_err(|error| RunError::Message(error.to_string()))?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(RunError::Message(format!("{}: {}", status.as_u16(), text)));
    }
    Ok(response)
}

/// Local SSE buffer for the OpenAI Responses transport (`data: ...` frames).
#[derive(Default)]
pub struct SseBuffer {
    buffer: String,
    pub error: Option<String>,
}

impl SseBuffer {
    /// Appends a chunk and returns the complete `data:` payloads it completed.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Value> {
        self.buffer.push_str(&String::from_utf8_lossy(bytes));
        let mut events: Vec<Value> = Vec::new();
        while let Some(index) = self.buffer.find("\n\n") {
            let chunk = self.buffer[..index].to_string();
            self.buffer = self.buffer[index + 2..].to_string();
            let data = chunk
                .split('\n')
                .filter(|line| line.starts_with("data:"))
                .map(|line| line[5..].trim().to_string())
                .collect::<Vec<_>>()
                .join("\n");
            let data = data.trim().to_string();
            if !data.is_empty() && data != "[DONE]" {
                if let Ok(parsed) = serde_json::from_str::<Value>(&data) {
                    events.push(parsed);
                }
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{InputModality, ModelCost, Tool};
    use indexmap::IndexMap;
    use serde_json::json;

    fn model() -> Model {
        let mut model = Model::new("gpt-5.4", "GPT-5.4", "openai-responses", "openai", "https://api.openai.com/v1");
        model.reasoning = true;
        model.input = vec![InputModality::Text];
        model.cost = ModelCost::default();
        model
    }

    #[test]
    fn resolve_cache_retention_defaults_to_short() {
        std::env::remove_var("PI_CACHE_RETENTION");
        assert_eq!(resolve_cache_retention(None), "short");
        assert_eq!(resolve_cache_retention(Some(&"long".to_string())), "long");
        std::env::set_var("PI_CACHE_RETENTION", "long");
        assert_eq!(resolve_cache_retention(None), "long");
        std::env::remove_var("PI_CACHE_RETENTION");
    }

    #[test]
    fn compat_defaults_match_typescript() {
        let model = model();
        assert_eq!(get_compat(&model), (true, true));
    }

    #[test]
    fn prompt_cache_retention_only_for_long_retention() {
        assert_eq!(get_prompt_cache_retention(true, &"long".to_string()), Some("24h".to_string()));
        assert_eq!(get_prompt_cache_retention(false, &"long".to_string()), None);
        assert_eq!(get_prompt_cache_retention(true, &"short".to_string()), None);
        assert_eq!(get_prompt_cache_retention(true, &"none".to_string()), None);
    }

    #[test]
    fn service_tier_multipliers_match_typescript() {
        let model = model();
        assert_eq!(get_service_tier_cost_multiplier(&model, Some("flex")), 0.5);
        assert_eq!(get_service_tier_cost_multiplier(&model, Some("priority")), 2.0);
        assert_eq!(get_service_tier_cost_multiplier(&model, Some("default")), 1.0);
        assert_eq!(get_service_tier_cost_multiplier(&model, None), 1.0);

        let gpt_5_5 = Model::new("gpt-5.5", "GPT-5.5", "openai-responses", "openai", "https://api.openai.com/v1");
        assert_eq!(get_service_tier_cost_multiplier(&gpt_5_5, Some("priority")), 2.5);

        let mut usage = Usage {
            input: 1_000_000.0,
            output: 1_000_000.0,
            cache_read: 1_000_000.0,
            cache_write: 1_000_000.0,
            total_tokens: 4_000_000.0,
            cost: crate::types::UsageCost {
                input: 1.0,
                output: 2.0,
                cache_read: 3.0,
                cache_write: 4.0,
                total: 10.0,
            },
        };
        apply_service_tier_pricing(&mut usage, Some("flex"), &model);
        assert_eq!(usage.cost.input, 0.5);
        assert_eq!(usage.cost.output, 1.0);
        assert_eq!(usage.cost.cache_read, 1.5);
        assert_eq!(usage.cost.cache_write, 2.0);
        assert_eq!(usage.cost.total, 5.0);
    }

    #[test]
    fn build_params_sets_defaults_and_omits_absent_optionals() {
        let model = model();
        let context = Context::new(
            Some("be terse".to_string()),
            vec![crate::types::Message::user(crate::types::UserMessage::new(
                crate::types::UserContent::Text("hi".to_string()),
                1,
            ))],
            None,
        );
        let params = build_params(&model, &context, None);
        assert_eq!(params["model"], json!("gpt-5.4"));
        assert_eq!(params["stream"], json!(true));
        assert_eq!(params["store"], json!(false));
        assert_eq!(params["prompt_cache_retention"], json!(null));
        assert!(!params.contains_key("prompt_cache_key"));
        assert!(!params.contains_key("max_output_tokens"));
        assert!(!params.contains_key("temperature"));
        assert!(!params.contains_key("tools"));
        // reasoning defaults to the "off" mapping when no effort is requested
        assert_eq!(params["reasoning"], json!({ "effort": "none" }));
        assert_eq!(params["input"][0]["role"], json!("developer"));
    }

    #[test]
    fn build_params_sends_service_tier_except_for_copilot() {
        let mut model = model();
        let context = Context::default();
        let options = OpenAIResponsesOptions {
            service_tier: Some("flex".to_string()),
            ..Default::default()
        };
        let params = build_params(&model, &context, Some(&options));
        assert_eq!(params["service_tier"], json!("flex"));

        model.provider = "github-copilot".to_string();
        let params = build_params(&model, &context, Some(&options));
        assert!(!params.contains_key("service_tier"));
    }

    #[test]
    fn build_params_maps_reasoning_effort_through_the_level_map() {
        let mut model = model();
        model.thinking_level_map = Some(
            [
                ("high".to_string(), Some("high-value".to_string())),
                ("off".to_string(), None),
            ]
            .into_iter()
            .collect(),
        );
        let context = Context::default();
        let options = OpenAIResponsesOptions {
            reasoning_effort: Some("high".to_string()),
            ..Default::default()
        };
        let params = build_params(&model, &context, Some(&options));
        assert_eq!(params["reasoning"], json!({ "effort": "high-value", "summary": "auto" }));
        assert_eq!(params["include"], json!(["reasoning.encrypted_content"]));

        // reasoningSummary alone uses the "medium" default effort.
        let options = OpenAIResponsesOptions {
            reasoning_summary: Some(Some("concise".to_string())),
            ..Default::default()
        };
        let params = build_params(&model, &context, Some(&options));
        assert_eq!(params["reasoning"], json!({ "effort": "medium", "summary": "concise" }));
    }

    #[test]
    fn build_params_omits_reasoning_when_off_is_null() {
        let mut model = model();
        model.thinking_level_map = Some([("off".to_string(), None)].into_iter().collect());
        let params = build_params(&model, &Context::default(), None);
        assert!(!params.contains_key("reasoning"));
    }

    #[test]
    fn build_params_includes_tools_and_limits() {
        let model = model();
        let context = Context::new(
            None,
            Vec::new(),
            Some(vec![Tool {
                name: "bash".to_string(),
                description: "Run a command".to_string(),
                parameters: json!({ "type": "object" }),
            }]),
        );
        let options = OpenAIResponsesOptions {
            stream: StreamOptions {
                max_tokens: Some(4096.0),
                temperature: Some(0.5),
                session_id: Some("session-1".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let params = build_params(&model, &context, Some(&options));
        assert_eq!(params["max_output_tokens"], json!(4096.0));
        assert_eq!(params["temperature"], json!(0.5));
        assert_eq!(params["prompt_cache_key"], json!("session-1"));
        assert_eq!(params["tools"][0]["strict"], json!(false));
    }

    #[test]
    fn create_client_sets_session_and_copilot_headers() {
        let model = model();
        let context = Context::default();
        let client = create_client(&model, &context, Some("key"), None, Some("cache-1"), Some("conv-1")).unwrap();
        assert_eq!(
            client.default_headers.get("session_id"),
            Some(&Some("cache-1".to_string()))
        );
        assert_eq!(
            client.default_headers.get("x-client-request-id"),
            Some(&Some("cache-1".to_string()))
        );
        assert_eq!(client.base_url, "https://api.openai.com/v1");

        let mut copilot = model.clone();
        copilot.provider = "github-copilot".to_string();
        let client = create_client(&copilot, &context, Some("key"), None, None, None).unwrap();
        assert_eq!(
            client.default_headers.get("X-Initiator"),
            Some(&Some("user".to_string()))
        );
        assert_eq!(
            client.default_headers.get("Openai-Intent"),
            Some(&Some("conversation-edits".to_string()))
        );
    }

    #[test]
    fn create_client_requires_an_api_key() {
        let model = model();
        let previous = std::env::var("OPENAI_API_KEY").ok();
        std::env::remove_var("OPENAI_API_KEY");
        let error = match create_client(&model, &Context::default(), Some(""), None, None, None) {
            Err(error) => error,
            Ok(_) => panic!("expected create_client to fail without an API key"),
        };
        assert_eq!(
            error,
            "OpenAI API key is required. Set OPENAI_API_KEY environment variable or pass it as an argument."
        );
        if let Some(previous) = previous {
            std::env::set_var("OPENAI_API_KEY", previous);
        }
    }

    #[test]
    fn cloudflare_gateway_sets_the_gateway_authorization_header() {
        let mut model = model();
        model.provider = "cloudflare-ai-gateway".to_string();
        model.base_url = "https://gateway.ai.cloudflare.com/v1/acct/gw/openai".to_string();
        let client = create_client(&model, &Context::default(), Some("key"), None, None, None).unwrap();
        assert_eq!(
            client.default_headers.get("cf-aig-authorization"),
            Some(&Some("Bearer key".to_string()))
        );
        // `Authorization: headers.Authorization ?? null` keeps the key with a null value.
        assert_eq!(client.default_headers.get("Authorization"), Some(&None));
    }

    #[test]
    fn opencode_headers_are_applied_last() {
        let mut model = model();
        model.provider = "opencode".to_string();
        let mut headers: IndexMap<String, String> = IndexMap::new();
        headers.insert("X-Test".to_string(), "1".to_string());
        let client = create_client(&model, &Context::default(), Some("key"), Some(&headers), None, Some("session-1"))
            .unwrap();
        assert_eq!(
            client.default_headers.get("User-Agent"),
            Some(&Some("prime-agent".to_string()))
        );
        assert_eq!(
            client.default_headers.get("x-opencode-session"),
            Some(&Some("session-1".to_string()))
        );
        assert_eq!(client.default_headers.get("x-test"), Some(&Some("1".to_string())));
    }

    #[test]
    fn sse_buffer_splits_frames_and_skips_done() {
        let mut buffer = SseBuffer::default();
        let first = buffer.push(b"data: {\"type\":\"response.created\"}\n\ndata: {\"type\":");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["type"], json!("response.created"));
        let second = buffer.push(b"\"response.completed\"}\n\ndata: [DONE]\n\n");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0]["type"], json!("response.completed"));
    }

    #[test]
    fn options_round_trip_from_base_options_keeps_callbacks() {
        let base = StreamOptions {
            temperature: Some(0.2),
            session_id: Some("session".to_string()),
            ..Default::default()
        };
        let typed = OpenAIResponsesOptions::from_base(&base);
        assert_eq!(typed.stream.temperature, Some(0.2));
        assert_eq!(typed.reasoning_effort, None);
        // serde round-trip through register_builtins keeps the serializable fields.
        let value = serde_json::to_value(&typed).unwrap();
        assert_eq!(value["temperature"], json!(0.2));
        assert_eq!(value["sessionId"], json!("session"));
    }

    #[test]
    fn try_compact_returns_none_for_unsupported_models() {
        let model = Model::new("gpt-4o", "GPT-4o", "openai-responses", "openai", "https://api.openai.com/v1");
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(try_compact_openai_responses(&model, &Context::default(), None))
            .unwrap();
        assert!(result.is_none());
    }
}
