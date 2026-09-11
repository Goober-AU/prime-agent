//! Port of packages/ai/src/providers/bedrock-responses-client.ts
//!
//! The TypeScript builds an OpenAI SDK client whose `fetch` signs each request with
//! `@smithy/signature-v4` (SigV4) and lets `@aws-sdk/credential-provider-node` resolve the
//! credentials. The Rust port builds the same request itself with `reqwest`, signs it with the
//! SigV4 implementation in `amazon_bedrock.rs` (HMAC-SHA256 over `sha2`, because the `hmac`
//! crate is not in the workspace dependency list) and reads credentials from the same
//! environment variables with the same precedence. No STS/IMDS network calls are made.

use std::time::Duration;

use futures::StreamExt;
use indexmap::IndexMap;
use serde_json::Value;

use crate::types::{Model, StreamOptions};
use crate::utils::now_ms;

use super::amazon_bedrock::{resolve_aws_credentials, sign_request_for_service, AwsCredentials};

/// TS: `interface BedrockResponsesAuthOptions extends StreamOptions`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BedrockResponsesAuthOptions {
	#[serde(flatten)]
	pub stream: StreamOptions,
	/// Explicit signing region; otherwise taken from the selected endpoint.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub region: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub profile: Option<String>,
	/// TS: `credentialProvider?: ReturnType<typeof defaultProvider>`.
	///
	/// NOTE: the TypeScript accepts an injected AWS credential provider. The port cannot call
	/// into an SDK provider object, so an injected provider is represented by
	/// `credentials`, and `has_credential_provider` keeps the TS branches
	/// (`!options?.profile && !options?.credentialProvider`) reachable.
	#[serde(skip)]
	pub credentials: Option<AwsCredentials>,
	#[serde(skip)]
	pub has_credential_provider: bool,
}

impl BedrockResponsesAuthOptions {
	/// TS: the caller passes `StreamOptions & Record<string, unknown>`; this keeps the
	/// non-serializable fields (signal, on_payload, on_response, on_usage_observation).
	pub fn from_base(base: &StreamOptions) -> Self {
		Self {
			stream: base.clone(),
			..Default::default()
		}
	}
}

/// TS: `createBedrockResponsesClient(model, options): OpenAI`.
///
/// The port returns the resolved connection instead of an SDK client: the base URL, the region,
/// the SigV4 service name and the auth mode the request must use.
#[derive(Debug, Clone, PartialEq)]
pub struct BedrockResponsesClient {
	pub base_url: String,
	/// `new OpenAI({ apiKey: bearerToken || "<aws-sigv4>" })`.
	pub api_key: String,
	pub bearer_token: Option<String>,
	pub signer: Option<BedrockResponsesSigner>,
	/// `Object.fromEntries(defaultHeaders)` - a `None` value means "delete this header".
	pub default_headers: IndexMap<String, Option<String>>,
	pub max_retries: i64,
	/// The origin the credentials may be sent to.
	pub origin: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BedrockResponsesSigner {
	pub region: String,
	pub service: String,
}

/// `export function createBedrockResponsesClient(model, options?)`.
pub fn create_bedrock_responses_client(
	model: &Model,
	options: Option<&BedrockResponsesAuthOptions>,
) -> Result<BedrockResponsesClient, String> {
	let base_url = std::env::var("AWS_BEDROCK_BASE_URL")
		.map(|value| value.trim().to_string())
		.ok()
		.filter(|value| !value.is_empty())
		.unwrap_or_else(|| model.base_url.clone());
	let url = url::Url::parse(&base_url).map_err(|_| "Invalid URL".to_string())?;
	if url.query().is_some()
		|| url.fragment().is_some()
		|| !url.username().is_empty()
		|| url.password().is_some()
		|| !(url.scheme() == "http" || url.scheme() == "https")
	{
		return Err(
			"Bedrock base URL must be an HTTP(S) API root without credentials, query, or fragment."
				.to_string(),
		);
	}
	let endpoint = match_endpoint_host(url.host_str().unwrap_or_default());
	let model_endpoint = url::Url::parse(&model.base_url)
		.ok()
		.and_then(|url| url.host_str().map(str::to_string))
		.and_then(|host| match_endpoint_host(&host));
	let runtime_model = is_runtime_model(&model.id);
	let service = if runtime_model { "bedrock" } else { "bedrock-mantle" };
	let region = options
		.and_then(|options| options.region.clone())
		.or_else(|| endpoint.as_ref().map(|endpoint| endpoint.region.clone()))
		.or_else(|| model_endpoint.as_ref().map(|endpoint| endpoint.region.clone()))
		.or_else(|| std::env::var("AWS_REGION").ok().filter(|value| !value.is_empty()))
		.or_else(|| std::env::var("AWS_DEFAULT_REGION").ok().filter(|value| !value.is_empty()));
	let Some(region) = region else {
		return Err("Set AWS_REGION or pass a signing region for the Bedrock proxy.".to_string());
	};
	if let (Some(endpoint), Some(options)) = (endpoint.as_ref(), options) {
		if let Some(explicit_region) = options.region.as_ref() {
			if explicit_region != &endpoint.region {
				return Err(format!(
					"Bedrock endpoint region {} does not match signing region {}.",
					endpoint.region, explicit_region
				));
			}
		}
	}
	if let Some(endpoint) = endpoint.as_ref() {
		if (endpoint.service == "bedrock-runtime") != runtime_model {
			return Err(
				"Use openai.gpt-6-astra with Mantle, or a global./us. inference profile with Bedrock Runtime."
					.to_string(),
			);
		}
	}
	if model.id == "openai.gpt-6-astra" {
		if let Some(endpoint) = endpoint.as_ref() {
			if endpoint.region != "us-west-2" {
				return Err("GPT-6 Astra on Bedrock Mantle requires us-west-2 (Oregon).".to_string());
			}
		}
	}
	let explicit_key = options
		.and_then(|options| options.stream.api_key.clone())
		.filter(|key| key != "<authenticated>");
	let has_profile = options.and_then(|options| options.profile.clone()).is_some();
	let has_credential_provider = options.map(|options| options.has_credential_provider).unwrap_or(false);
	if explicit_key.is_some() && (has_profile || has_credential_provider) {
		return Err("Choose either a Bedrock bearer token or explicit AWS credentials.".to_string());
	}
	let bearer_token = explicit_key.or_else(|| {
		if !has_profile && !has_credential_provider {
			std::env::var("AWS_BEARER_TOKEN_BEDROCK")
				.ok()
				.filter(|value| !value.is_empty())
		} else {
			None
		}
	});
	let signer = if bearer_token.is_some() {
		None
	} else {
		Some(BedrockResponsesSigner {
			region: region.clone(),
			service: service.to_string(),
		})
	};
	let mut default_headers: IndexMap<String, Option<String>> = IndexMap::new();
	if let Some(model_headers) = model.headers.as_ref() {
		for (key, value) in model_headers {
			default_headers.insert(key.clone(), Some(value.clone()));
		}
	}
	if let Some(options) = options {
		if let Some(headers) = options.stream.headers.as_ref() {
			for (key, value) in headers {
				default_headers.insert(key.clone(), Some(value.clone()));
			}
		}
	}
	if default_headers
		.keys()
		.any(|key| key.to_lowercase() == "authorization")
	{
		return Err("Use Bedrock apiKey or AWS credentials instead of an Authorization header.".to_string());
	}
	let origin = match url.port() {
		Some(port) => format!("{}://{}:{}", url.scheme(), url.host_str().unwrap_or_default(), port),
		None => format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default()),
	};

	Ok(BedrockResponsesClient {
		base_url,
		api_key: bearer_token
			.clone()
			.unwrap_or_else(|| "<aws-sigv4>".to_string()),
		bearer_token,
		signer,
		default_headers,
		max_retries: 0,
		origin,
	})
}

/// `/^(bedrock-mantle|bedrock-runtime)\.([a-z0-9-]+)\.(?:api\.aws|amazonaws\.com)$/`
#[derive(Debug, Clone, PartialEq)]
pub struct BedrockEndpointMatch {
	pub service: String,
	pub region: String,
}

fn match_endpoint_host(hostname: &str) -> Option<BedrockEndpointMatch> {
	let lower = hostname.to_lowercase();
	let rest = lower
		.strip_prefix("bedrock-mantle.")
		.map(|rest| ("bedrock-mantle", rest))
		.or_else(|| lower.strip_prefix("bedrock-runtime.").map(|rest| ("bedrock-runtime", rest)))?;
	let (service, rest) = rest;
	let region = rest
		.strip_suffix(".api.aws")
		.or_else(|| rest.strip_suffix(".amazonaws.com"))?;
	if region.is_empty() || !region.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
		return None;
	}
	Some(BedrockEndpointMatch {
		service: service.to_string(),
		region: region.to_string(),
	})
}

/// `/^(?:global|us)\.openai\./`
fn is_runtime_model(model_id: &str) -> bool {
	model_id.starts_with("global.openai.") || model_id.starts_with("us.openai.")
}

/// The headers the `fetch` wrapper sets before sending: SigV4 or bearer.
pub fn build_signed_request_headers(
	client: &BedrockResponsesClient,
	target_url: &str,
	method: &str,
	headers: &IndexMap<String, String>,
	body: &str,
	options: Option<&BedrockResponsesAuthOptions>,
) -> Result<IndexMap<String, String>, String> {
	let target = url::Url::parse(target_url).map_err(|_| "Invalid URL".to_string())?;
	let target_origin = match target.port() {
		Some(port) => format!(
			"{}://{}:{}",
			target.scheme(),
			target.host_str().unwrap_or_default(),
			port
		),
		None => format!("{}://{}", target.scheme(), target.host_str().unwrap_or_default()),
	};
	if target_origin != client.origin {
		return Err("Refusing to send AWS credentials outside the configured Bedrock endpoint.".to_string());
	}

	let mut outgoing: IndexMap<String, String> = headers
		.iter()
		.filter(|(key, _)| key.to_lowercase() != "authorization")
		.map(|(key, value)| (key.clone(), value.clone()))
		.collect();

	if let Some(bearer_token) = client.bearer_token.clone() {
		outgoing.insert("authorization".to_string(), format!("Bearer {}", bearer_token));
	} else if let Some(signer) = client.signer.clone() {
		outgoing.insert("host".to_string(), target.host_str().unwrap_or_default().to_string());
		outgoing.shift_remove("x-amz-date");
		outgoing.shift_remove("x-amz-security-token");
		outgoing.shift_remove("x-amz-content-sha256");

		let credentials = match options.and_then(|options| options.credentials.clone()) {
			Some(credentials) => credentials,
			None => resolve_aws_credentials(&crate::providers::amazon_bedrock::BedrockOptions {
				profile: options.and_then(|options| options.profile.clone()),
				..Default::default()
			})
			.map_err(|error| error.to_string())?,
		};

		let mut query: Vec<(String, String)> = Vec::new();
		for (key, value) in target.query_pairs() {
			query.push((key.to_string(), value.to_string()));
		}

		let signed = sign_request_for_service(
			method,
			target.path(),
			&query,
			&outgoing,
			body,
			&signer.region,
			&signer.service,
			&credentials,
			now_ms(),
		);
		for (key, value) in signed.headers {
			outgoing.insert(key, value);
		}
	}

	Ok(outgoing)
}

impl BedrockResponsesClient {
	/// `client.responses.create(params, requestOptions)` for the Bedrock proxy: POST
	/// `<baseURL>/responses` with the SigV4 signature (or the bearer token) and return the
	/// raw response so the caller can read the status, headers and SSE body.
	pub async fn send_responses(
		&self,
		params: &serde_json::Map<String, Value>,
		options: Option<&BedrockResponsesAuthOptions>,
	) -> Result<reqwest::Response, String> {
		send_signed_responses_request(self, &Value::Object(params.clone()), options).await
	}
}

/// `globalThis.fetch(new Request(request, { headers, redirect: "manual" }))` for the
/// `/responses` endpoint: POST with the signed headers and the JSON body.
pub async fn send_signed_responses_request(
	client: &BedrockResponsesClient,
	params: &Value,
	options: Option<&BedrockResponsesAuthOptions>,
) -> Result<reqwest::Response, String> {
	let url = format!("{}/responses", client.base_url.trim_end_matches('/'));
	let body = params.to_string();

	let mut headers: IndexMap<String, String> = IndexMap::new();
	headers.insert("content-type".to_string(), "application/json".to_string());
	for (key, value) in client.default_headers.iter() {
		let Some(value) = value else {
			continue;
		};
		headers.insert(key.clone(), value.clone());
	}
	headers.insert(
		"host".to_string(),
		url::Url::parse(&url)
			.ok()
			.and_then(|url| url.host_str().map(str::to_string))
			.unwrap_or_default(),
	);

	let signed = build_signed_request_headers(client, &url, "POST", &headers, &body, options)?;

	let mut request = reqwest::Client::builder()
		.redirect(reqwest::redirect::Policy::none())
		.build()
		.map_err(|error| error.to_string())?
		.post(&url)
		.body(body);
	for (key, value) in signed {
		request = request.header(key, value);
	}
	if let Some(timeout_ms) = options.and_then(|options| options.stream.timeout_ms) {
		request = request.timeout(Duration::from_millis(timeout_ms.max(0.0) as u64));
	}

	let send = request.send();
	let response = match options.and_then(|options| options.stream.signal.as_ref()) {
		Some(signal) => tokio::select! {
			_ = signal.cancelled() => return Err("Request was aborted".to_string()),
			result = send => result,
		},
		None => send.await,
	};
	response.map_err(|error| error.to_string())
}

/// The boxed response byte stream used by `responses_event_stream`.
pub type BedrockByteStream =
	std::pin::Pin<Box<dyn futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>;

/// The local SSE reader for the Responses transport (`data: ...` frames).
#[derive(Default)]
pub struct BedrockResponsesSseBuffer {
	buffer: String,
}

impl BedrockResponsesSseBuffer {
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

/// Drain a `reqwest` response body into a `processResponsesStream` input stream.
pub fn responses_event_stream(
	response: reqwest::Response,
) -> crate::providers::openai_responses_shared::ResponsesEventStream {
	let byte_stream: BedrockByteStream = Box::pin(response.bytes_stream());
	Box::pin(futures::stream::unfold(
		(
			byte_stream,
			BedrockResponsesSseBuffer::default(),
			std::collections::VecDeque::<Value>::new(),
		),
		|(mut byte_stream, mut buffer, mut pending)| async move {
			loop {
				if let Some(event) = pending.pop_front() {
					return Some((event, (byte_stream, buffer, pending)));
				}
				match byte_stream.next().await {
					Some(Ok(bytes)) => pending.extend(buffer.push(&bytes)),
					_ => return None,
				}
			}
		},
	))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::types::Model;

	/// The client reads `AWS_*` environment variables; tests that touch them must not run
	/// concurrently with tests that depend on them.
	struct CleanAwsEnv {
		_guard: std::sync::MutexGuard<'static, ()>,
		saved: Vec<(&'static str, Option<String>)>,
	}

	const AWS_ENV_NAMES: [&str; 7] = [
		"AWS_BEDROCK_BASE_URL",
		"AWS_REGION",
		"AWS_DEFAULT_REGION",
		"AWS_BEARER_TOKEN_BEDROCK",
		"AWS_PROFILE",
		"AWS_ACCESS_KEY_ID",
		"AWS_SECRET_ACCESS_KEY",
	];

	fn env_lock() -> &'static std::sync::Mutex<()> {
		static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
		&LOCK
	}

	impl CleanAwsEnv {
		fn new() -> Self {
			let guard = env_lock()
				.lock()
				.unwrap_or_else(|poisoned| poisoned.into_inner());
			let saved: Vec<(&'static str, Option<String>)> = AWS_ENV_NAMES
				.iter()
				.map(|name| (*name, std::env::var(name).ok()))
				.collect();
			for name in AWS_ENV_NAMES {
				std::env::remove_var(name);
			}
			Self {
				_guard: guard,
				saved,
			}
		}
	}

	impl Drop for CleanAwsEnv {
		fn drop(&mut self) {
			for (name, value) in &self.saved {
				match value {
					Some(value) => std::env::set_var(name, value),
					None => std::env::remove_var(name),
				}
			}
		}
	}

	fn model(id: &str, base_url: &str) -> Model {
		Model::new(id, id, "bedrock-responses", "amazon-bedrock", base_url)
	}

	#[test]
	fn endpoint_host_pattern_matches_mantle_and_runtime() {
		assert_eq!(
			match_endpoint_host("bedrock-mantle.us-west-2.api.aws"),
			Some(BedrockEndpointMatch {
				service: "bedrock-mantle".to_string(),
				region: "us-west-2".to_string()
			})
		);
		assert_eq!(
			match_endpoint_host("bedrock-runtime.us-east-1.amazonaws.com"),
			Some(BedrockEndpointMatch {
				service: "bedrock-runtime".to_string(),
				region: "us-east-1".to_string()
			})
		);
		assert_eq!(match_endpoint_host("bedrock-runtime.us-east-1.example.com"), None);
		assert_eq!(match_endpoint_host("api.openai.com"), None);
		assert_eq!(match_endpoint_host("bedrock-mantle.US-WEST-2.API.AWS"), match_endpoint_host("bedrock-mantle.us-west-2.api.aws"));
	}

	#[test]
	fn runtime_models_are_global_and_us_openai_profiles() {
		assert!(is_runtime_model("global.openai.gpt-6-astra"));
		assert!(is_runtime_model("us.openai.gpt-6-astra"));
		assert!(!is_runtime_model("openai.gpt-6-astra"));
		assert!(!is_runtime_model("eu.openai.gpt-6-astra"));
	}

	#[test]
	fn base_url_rejects_credentials_query_and_fragment() {
		let _env = CleanAwsEnv::new();
		let error = create_bedrock_responses_client(&model("global.openai.gpt-6-astra", "https://x.api.aws/openai/v1?a=1"), None)
			.unwrap_err();
		assert_eq!(
			error,
			"Bedrock base URL must be an HTTP(S) API root without credentials, query, or fragment."
		);
		let error = create_bedrock_responses_client(&model("global.openai.gpt-6-astra", "ftp://x.api.aws/openai/v1"), None)
			.unwrap_err();
		assert_eq!(
			error,
			"Bedrock base URL must be an HTTP(S) API root without credentials, query, or fragment."
		);
	}

	#[test]
	fn signing_region_comes_from_option_endpoint_or_env() {
		let _env = CleanAwsEnv::new();
		std::env::remove_var("AWS_REGION");
		std::env::remove_var("AWS_DEFAULT_REGION");

		let mut options = BedrockResponsesAuthOptions::default();
		options.region = Some("eu-west-1".to_string());
		let client = create_bedrock_responses_client(
			&model("global.openai.gpt-6-astra", "https://bedrock-runtime.ap-southeast-2.amazonaws.com/openai/v1"),
			Some(&options),
		)
		.unwrap();
		assert_eq!(client.signer.as_ref().unwrap().region, "eu-west-1");

		let client = create_bedrock_responses_client(
			&model("global.openai.gpt-6-astra", "https://bedrock-runtime.ap-southeast-2.amazonaws.com/openai/v1"),
			None,
		)
		.unwrap();
		assert_eq!(client.signer.as_ref().unwrap().region, "ap-southeast-2");
		assert_eq!(client.signer.as_ref().unwrap().service, "bedrock");
		assert_eq!(client.api_key, "<aws-sigv4>");

		let error = create_bedrock_responses_client(&model("m", "https://example.com/v1"), None).unwrap_err();
		assert_eq!(error, "Set AWS_REGION or pass a signing region for the Bedrock proxy.");
	}

	#[test]
	fn endpoint_region_must_match_the_signing_region() {
		let _env = CleanAwsEnv::new();
		let mut options = BedrockResponsesAuthOptions::default();
		options.region = Some("us-east-1".to_string());
		let error = create_bedrock_responses_client(
			&model("global.openai.gpt-6-astra", "https://bedrock-runtime.ap-southeast-2.amazonaws.com/openai/v1"),
			Some(&options),
		)
		.unwrap_err();
		assert_eq!(
			error,
			"Bedrock endpoint region ap-southeast-2 does not match signing region us-east-1."
		);
	}

	#[test]
	fn runtime_and_mantle_endpoints_must_match_the_model() {
		let _env = CleanAwsEnv::new();
		let error = create_bedrock_responses_client(
			&model("openai.gpt-6-astra", "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1"),
			None,
		)
		.unwrap_err();
		assert_eq!(
			error,
			"Use openai.gpt-6-astra with Mantle, or a global./us. inference profile with Bedrock Runtime."
		);

		let error = create_bedrock_responses_client(
			&model("global.openai.gpt-6-astra", "https://bedrock-mantle.us-west-2.api.aws/openai/v1"),
			None,
		)
		.unwrap_err();
		assert_eq!(
			error,
			"Use openai.gpt-6-astra with Mantle, or a global./us. inference profile with Bedrock Runtime."
		);
	}

	#[test]
	fn astra_on_mantle_requires_oregon() {
		let _env = CleanAwsEnv::new();
		let error = create_bedrock_responses_client(
			&model("openai.gpt-6-astra", "https://bedrock-mantle.us-east-1.api.aws/openai/v1"),
			None,
		)
		.unwrap_err();
		assert_eq!(error, "GPT-6 Astra on Bedrock Mantle requires us-west-2 (Oregon).");

		let client = create_bedrock_responses_client(
			&model("openai.gpt-6-astra", "https://bedrock-mantle.us-west-2.api.aws/openai/v1"),
			None,
		)
		.unwrap();
		assert_eq!(client.signer.as_ref().unwrap().service, "bedrock-mantle");
	}

	#[test]
	fn bearer_token_and_explicit_credentials_are_mutually_exclusive() {
		let _env = CleanAwsEnv::new();
		std::env::remove_var("AWS_BEARER_TOKEN_BEDROCK");
		let mut options = BedrockResponsesAuthOptions::default();
		options.stream.api_key = Some("token".to_string());
		options.profile = Some("profile".to_string());
		let error = create_bedrock_responses_client(
			&model("global.openai.gpt-6-astra", "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1"),
			Some(&options),
		)
		.unwrap_err();
		assert_eq!(error, "Choose either a Bedrock bearer token or explicit AWS credentials.");

		let mut options = BedrockResponsesAuthOptions::default();
		options.stream.api_key = Some("token".to_string());
		let client = create_bedrock_responses_client(
			&model("global.openai.gpt-6-astra", "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1"),
			Some(&options),
		)
		.unwrap();
		assert_eq!(client.api_key, "token");
		assert_eq!(client.bearer_token.as_deref(), Some("token"));
		assert!(client.signer.is_none());
		assert_eq!(client.max_retries, 0);

		// "<authenticated>" from the auth store is not an explicit bearer token.
		let mut options = BedrockResponsesAuthOptions::default();
		options.stream.api_key = Some("<authenticated>".to_string());
		let client = create_bedrock_responses_client(
			&model("global.openai.gpt-6-astra", "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1"),
			Some(&options),
		)
		.unwrap();
		assert_eq!(client.api_key, "<aws-sigv4>");
		assert!(client.bearer_token.is_none());
	}

	#[test]
	fn default_headers_merge_model_then_options_and_reject_authorization() {
		let _env = CleanAwsEnv::new();
		let mut model = model("global.openai.gpt-6-astra", "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1");
		model.headers = Some(IndexMap::from([("x-model".to_string(), "1".to_string())]));
		let mut options = BedrockResponsesAuthOptions::default();
		options.stream.headers = Some(IndexMap::from([
			("x-option".to_string(), "2".to_string()),
			("x-model".to_string(), "3".to_string()),
		]));
		let client = create_bedrock_responses_client(&model, Some(&options)).unwrap();
		assert_eq!(client.default_headers.get("x-model"), Some(&Some("3".to_string())));
		assert_eq!(client.default_headers.get("x-option"), Some(&Some("2".to_string())));

		let mut model = model("global.openai.gpt-6-astra", "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1");
		model.headers = Some(IndexMap::from([("Authorization".to_string(), "Bearer x".to_string())]));
		let error = create_bedrock_responses_client(&model, None).unwrap_err();
		assert_eq!(error, "Use Bedrock apiKey or AWS credentials instead of an Authorization header.");
	}

	#[test]
	fn base_url_env_override_wins_over_the_model() {
		let _env = CleanAwsEnv::new();
		std::env::set_var("AWS_BEDROCK_BASE_URL", "  https://bedrock-runtime.eu-central-1.amazonaws.com/openai/v1  ");
		let client = create_bedrock_responses_client(
			&model("global.openai.gpt-6-astra", "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1"),
			None,
		)
		.unwrap();
		assert_eq!(
			client.base_url,
			"https://bedrock-runtime.eu-central-1.amazonaws.com/openai/v1"
		);
		assert_eq!(client.signer.as_ref().unwrap().region, "eu-central-1");
		std::env::remove_var("AWS_BEDROCK_BASE_URL");
	}

	#[test]
	fn sigv4_headers_are_added_for_the_target_origin_only() {
		let _env = CleanAwsEnv::new();
		let credentials = AwsCredentials {
			access_key_id: "AKID".to_string(),
			secret_access_key: "secret".to_string(),
			session_token: Some("token".to_string()),
		};
		let mut options = BedrockResponsesAuthOptions::default();
		options.credentials = Some(credentials);
		options.has_credential_provider = true;

		let client = create_bedrock_responses_client(
			&model("global.openai.gpt-6-astra", "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1"),
			Some(&options),
		)
		.unwrap();
		assert!(client.bearer_token.is_none());

		let mut headers: IndexMap<String, String> = IndexMap::new();
		headers.insert("content-type".to_string(), "application/json".to_string());
		headers.insert("host".to_string(), "bedrock-runtime.us-west-2.amazonaws.com".to_string());
		headers.insert("authorization".to_string(), "Bearer stale".to_string());

		let signed = build_signed_request_headers(
			&client,
			"https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1/responses",
			"POST",
			&headers,
			"{\"model\":\"m\"}",
			Some(&options),
		)
		.unwrap();
		assert_eq!(signed["x-amz-security-token"], "token");
		assert!(signed["authorization"].starts_with("AWS4-HMAC-SHA256 Credential=AKID/"));
		assert!(signed["authorization"].contains("/us-west-2/bedrock/aws4_request"));
		assert_eq!(signed["x-amz-content-sha256"], crate::providers::amazon_bedrock::sha256_hex(b"{\"model\":\"m\"}"));

		let error = build_signed_request_headers(
			&client,
			"https://evil.example.com/openai/v1/responses",
			"POST",
			&headers,
			"{}",
			Some(&options),
		)
		.unwrap_err();
		assert_eq!(
			error,
			"Refusing to send AWS credentials outside the configured Bedrock endpoint."
		);
	}

	#[test]
	fn bearer_tokens_are_sent_as_authorization_headers() {
		let _env = CleanAwsEnv::new();
		let mut options = BedrockResponsesAuthOptions::default();
		options.stream.api_key = Some("token".to_string());
		let client = create_bedrock_responses_client(
			&model("global.openai.gpt-6-astra", "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1"),
			Some(&options),
		)
		.unwrap();

		let mut headers: IndexMap<String, String> = IndexMap::new();
		headers.insert("host".to_string(), "bedrock-runtime.us-west-2.amazonaws.com".to_string());
		let signed = build_signed_request_headers(
			&client,
			"https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1/responses",
			"POST",
			&headers,
			"{}",
			Some(&options),
		)
		.unwrap();
		assert_eq!(signed["authorization"], "Bearer token");
		assert!(signed.get("x-amz-date").is_none());
	}

	#[test]
	fn sse_buffer_decodes_data_frames() {
		let mut buffer = BedrockResponsesSseBuffer::default();
		assert!(buffer.push(b"data: {\"type\":\"response.crea").is_empty());
		let events = buffer.push(b"ted\",\"response\":{\"id\":\"r1\"}}\n\ndata: [DONE]\n\n");
		assert_eq!(events.len(), 1);
		assert_eq!(events[0]["response"]["id"], "r1");
	}
}
