//! Port of packages/ai/src/utils/oauth/github-copilot.ts

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::copilot_client_version::{COPILOT_CLIENT_HEADERS, COPILOT_CLIENT_USER_AGENT};
use crate::models::get_models;
use crate::types::{BoxFuture, Model};
use crate::utils::oauth::types::{
    OAuthCredentials, OAuthLoginCallbacks, OAuthPrompt, OAuthProviderInterface,
};
use crate::utils::oauth::decode_base64;

pub fn client_id() -> String {
    decode_base64("SXYxLmI1MDdhMDhjODdlY2ZlOTg=")
}

pub const INITIAL_POLL_INTERVAL_MULTIPLIER: f64 = 1.2;
pub const SLOW_DOWN_POLL_INTERVAL_MULTIPLIER: f64 = 1.4;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: f64,
    pub expires_in: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeviceTokenSuccessResponse {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeviceTokenErrorResponse {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<f64>,
}

/// `normalizeDomain(input)`.
pub fn normalize_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{}", trimmed)
    };
    url::Url::parse(&candidate)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
}

#[derive(Debug, Clone, PartialEq)]
pub struct CopilotUrls {
    pub device_code_url: String,
    pub access_token_url: String,
    pub copilot_token_url: String,
}

pub fn get_urls(domain: &str) -> CopilotUrls {
    CopilotUrls {
        device_code_url: format!("https://{}/login/device/code", domain),
        access_token_url: format!("https://{}/login/oauth/access_token", domain),
        copilot_token_url: format!("https://api.{}/copilot_internal/v2/token", domain),
    }
}

/// Parse the proxy-ep from a Copilot token and convert to API base URL.
/// Token format: tid=...;exp=...;proxy-ep=proxy.individual.githubcopilot.com;...
/// Returns API URL like https://api.individual.githubcopilot.com
pub fn get_base_url_from_token(token: &str) -> Option<String> {
    let regex = regex::Regex::new(r"proxy-ep=([^;]+)").ok()?;
    let captures = regex.captures(token)?;
    let proxy_host = captures.get(1)?.as_str();
    let api_host = proxy_host.strip_prefix("proxy.").map(|rest| format!("api.{}", rest));
    let api_host = api_host.unwrap_or_else(|| proxy_host.to_string());
    Some(format!("https://{}", api_host))
}

pub fn get_github_copilot_base_url(token: Option<&str>, enterprise_domain: Option<&str>) -> String {
    if let Some(token) = token {
        if let Some(url_from_token) = get_base_url_from_token(token) {
            return url_from_token;
        }
    }
    if let Some(enterprise_domain) = enterprise_domain {
        return format!("https://copilot-api.{}", enterprise_domain);
    }
    "https://api.individual.githubcopilot.com".to_string()
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .build()
        .map_err(|error| error.to_string())
}

async fn fetch_json(url: &str, init: FetchInit) -> Result<Value, String> {
    let client = http_client()?;
    let mut request = if init.method == "POST" {
        client.post(url)
    } else {
        client.get(url)
    };
    for (name, value) in &init.headers {
        request = request.header(name.as_str(), value.as_str());
    }
    if let Some(body) = init.body.as_deref() {
        request = request.body(body.to_string());
    }
    let response = request.send().await.map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(format!(
            "{} {}: {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or_default(),
            text
        ));
    }
    response.json::<Value>().await.map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Default)]
pub struct FetchInit {
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

fn copilot_headers() -> Vec<(String, String)> {
    COPILOT_CLIENT_HEADERS
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

async fn start_device_flow(domain: &str) -> Result<DeviceCodeResponse, String> {
    let urls = get_urls(domain);
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &client_id())
        .append_pair("scope", "read:user")
        .finish();
    let data = fetch_json(
        &urls.device_code_url,
        FetchInit {
            method: "POST".to_string(),
            headers: vec![
                ("Accept".to_string(), "application/json".to_string()),
                (
                    "Content-Type".to_string(),
                    "application/x-www-form-urlencoded".to_string(),
                ),
                ("User-Agent".to_string(), COPILOT_CLIENT_USER_AGENT.to_string()),
            ],
            body: Some(body),
        },
    )
    .await?;

    if !data.is_object() {
        return Err("Invalid device code response".to_string());
    }

    let device_code = data.get("device_code").and_then(Value::as_str);
    let user_code = data.get("user_code").and_then(Value::as_str);
    let verification_uri = data.get("verification_uri").and_then(Value::as_str);
    let interval = data.get("interval").and_then(Value::as_f64);
    let expires_in = data.get("expires_in").and_then(Value::as_f64);

    let (Some(device_code), Some(user_code), Some(verification_uri), Some(interval), Some(expires_in)) =
        (device_code, user_code, verification_uri, interval, expires_in)
    else {
        return Err("Invalid device code response fields".to_string());
    };

    Ok(DeviceCodeResponse {
        device_code: device_code.to_string(),
        user_code: user_code.to_string(),
        verification_uri: verification_uri.to_string(),
        interval,
        expires_in,
    })
}

/// `abortableSleep(ms, signal)`.
async fn abortable_sleep(ms: u64, signal: Option<&CancellationToken>) -> Result<(), String> {
    match signal {
        Some(token) => {
            if token.is_cancelled() {
                return Err("Login cancelled".to_string());
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
                _ = token.cancelled() => Err("Login cancelled".to_string()),
            }
        }
        None => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(())
        }
    }
}

async fn poll_for_github_access_token(
    domain: &str,
    device_code: &str,
    interval_seconds: f64,
    expires_in: f64,
    signal: Option<&CancellationToken>,
) -> Result<String, String> {
    let urls = get_urls(domain);
    let deadline = crate::utils::now_ms() as f64 + expires_in * 1000.0;
    let mut interval_ms = 1000.0f64.max((interval_seconds * 1000.0).floor());
    let mut interval_multiplier = INITIAL_POLL_INTERVAL_MULTIPLIER;
    let mut slow_down_responses = 0u32;

    while (crate::utils::now_ms() as f64) < deadline {
        if signal.map(|token| token.is_cancelled()).unwrap_or(false) {
            return Err("Login cancelled".to_string());
        }

        let remaining_ms = deadline - crate::utils::now_ms() as f64;
        let wait_ms = (interval_ms * interval_multiplier).ceil().min(remaining_ms);
        abortable_sleep(wait_ms.max(0.0) as u64, signal).await?;

        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", &client_id())
            .append_pair("device_code", device_code)
            .append_pair("grant_type", "urn:ietf:params:oauth:grant-type:device_code")
            .finish();
        let raw = fetch_json(
            &urls.access_token_url,
            FetchInit {
                method: "POST".to_string(),
                headers: vec![
                    ("Accept".to_string(), "application/json".to_string()),
                    (
                        "Content-Type".to_string(),
                        "application/x-www-form-urlencoded".to_string(),
                    ),
                    ("User-Agent".to_string(), COPILOT_CLIENT_USER_AGENT.to_string()),
                ],
                body: Some(body),
            },
        )
        .await?;

        if let Some(access_token) = raw.get("access_token").and_then(Value::as_str) {
            return Ok(access_token.to_string());
        }

        if let Some(error) = raw.get("error").and_then(Value::as_str) {
            let description = raw.get("error_description").and_then(Value::as_str);
            if error == "authorization_pending" {
                continue;
            }

            if error == "slow_down" {
                slow_down_responses += 1;
                let interval = raw.get("interval").and_then(Value::as_f64);
                interval_ms = match interval {
                    Some(interval) if interval > 0.0 => interval * 1000.0,
                    _ => 1000.0f64.max(interval_ms + 5000.0),
                };
                interval_multiplier = SLOW_DOWN_POLL_INTERVAL_MULTIPLIER;
                continue;
            }

            let description_suffix = description
                .map(|description| format!(": {}", description))
                .unwrap_or_default();
            return Err(format!("Device flow failed: {}{}", error, description_suffix));
        }
    }

    if slow_down_responses > 0 {
        return Err(
            "Device flow timed out after one or more slow_down responses. This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM clock and try again."
                .to_string(),
        );
    }

    Err("Device flow timed out".to_string())
}

pub async fn refresh_github_copilot_token(
    refresh_token: &str,
    enterprise_domain: Option<&str>,
) -> Result<OAuthCredentials, String> {
    let domain = enterprise_domain.unwrap_or("github.com");
    let urls = get_urls(domain);

    let mut headers = vec![
        ("Accept".to_string(), "application/json".to_string()),
        (
            "Authorization".to_string(),
            format!("Bearer {}", refresh_token),
        ),
    ];
    headers.extend(copilot_headers());

    let raw = fetch_json(
        &urls.copilot_token_url,
        FetchInit {
            method: "GET".to_string(),
            headers,
            body: None,
        },
    )
    .await?;

    if !raw.is_object() {
        return Err("Invalid Copilot token response".to_string());
    }

    let token = raw.get("token").and_then(Value::as_str);
    let expires_at = raw.get("expires_at").and_then(Value::as_f64);

    let (Some(token), Some(expires_at)) = (token, expires_at) else {
        return Err("Invalid Copilot token response fields".to_string());
    };

    let mut extra = serde_json::Map::new();
    if let Some(enterprise_domain) = enterprise_domain {
        extra.insert(
            "enterpriseUrl".to_string(),
            Value::String(enterprise_domain.to_string()),
        );
    }

    Ok(OAuthCredentials {
        refresh: refresh_token.to_string(),
        access: token.to_string(),
        expires: expires_at * 1000.0 - 5.0 * 60.0 * 1000.0,
        extra,
    })
}

/// Enable a model for the user's GitHub Copilot account.
/// This is required for some models (like Claude, Grok) before they can be used.
async fn enable_github_copilot_model(
    token: &str,
    model_id: &str,
    enterprise_domain: Option<&str>,
) -> bool {
    let base_url = get_github_copilot_base_url(Some(token), enterprise_domain);
    let url = format!("{}/models/{}/policy", base_url, model_id);

    let client = match http_client() {
        Ok(client) => client,
        Err(_) => return false,
    };
    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", token))
        .header("openai-intent", "chat-policy")
        .header("x-interaction-type", "chat-policy");
    for (name, value) in copilot_headers() {
        request = request.header(name, value);
    }
    match request.body(json!({"state": "enabled"}).to_string()).send().await {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

/// Enable all known GitHub Copilot models that may require policy acceptance.
/// Called after successful login to ensure all models are available.
async fn enable_all_github_copilot_models(
    token: &str,
    enterprise_domain: Option<&str>,
    on_progress: Option<&Arc<dyn Fn(String, bool) + Send + Sync>>,
) {
    let models: Vec<String> = get_models("github-copilot")
        .into_iter()
        .map(|model| model.id.clone())
        .collect();
    let mut tasks = Vec::new();
    for model_id in models {
        let token = token.to_string();
        let enterprise_domain = enterprise_domain.map(str::to_string);
        let on_progress = on_progress.cloned();
        tasks.push(tokio::spawn(async move {
            let success =
                enable_github_copilot_model(&token, &model_id, enterprise_domain.as_deref()).await;
            if let Some(on_progress) = on_progress {
                on_progress(model_id, success);
            }
        }));
    }
    for task in tasks {
        let _ = task.await;
    }
}

/// Options accepted by `loginGitHubCopilot`.
pub struct GitHubCopilotLoginOptions {
    pub on_auth: Option<Arc<dyn Fn(String, Option<String>) + Send + Sync>>,
    pub on_prompt: Option<Arc<dyn Fn(OAuthPrompt) -> BoxFuture<String> + Send + Sync>>,
    pub on_progress: Option<Arc<dyn Fn(String) + Send + Sync>>,
    pub signal: Option<CancellationToken>,
    /// `enableAllGitHubCopilotModels` progress callback.
    pub on_model_progress: Option<Arc<dyn Fn(String, bool) + Send + Sync>>,
}

/// Login with GitHub Copilot OAuth (device code flow).
pub async fn login_github_copilot(
    options: GitHubCopilotLoginOptions,
) -> Result<OAuthCredentials, String> {
    let input = match options.on_prompt.as_ref() {
        Some(on_prompt) => {
            on_prompt(OAuthPrompt {
                message: "GitHub Enterprise URL/domain (blank for github.com)".to_string(),
                placeholder: Some("company.ghe.com".to_string()),
                allow_empty: Some(true),
            })
            .await
        }
        None => String::new(),
    };

    if options.signal.as_ref().map(|token| token.is_cancelled()).unwrap_or(false) {
        return Err("Login cancelled".to_string());
    }

    let trimmed = input.trim().to_string();
    let enterprise_domain = normalize_domain(&input);
    if !trimmed.is_empty() && enterprise_domain.is_none() {
        return Err("Invalid GitHub Enterprise URL/domain".to_string());
    }
    let domain = enterprise_domain.clone().unwrap_or_else(|| "github.com".to_string());

    let device = start_device_flow(&domain).await?;
    if let Some(on_auth) = options.on_auth.as_ref() {
        on_auth(
            device.verification_uri.clone(),
            Some(format!("Enter code: {}", device.user_code)),
        );
    }

    let github_access_token = poll_for_github_access_token(
        &domain,
        &device.device_code,
        device.interval,
        device.expires_in,
        options.signal.as_ref(),
    )
    .await?;
    let credentials =
        refresh_github_copilot_token(&github_access_token, enterprise_domain.as_deref()).await?;

    if let Some(on_progress) = options.on_progress.as_ref() {
        on_progress("Enabling models...".to_string());
    }
    enable_all_github_copilot_models(
        &credentials.access,
        enterprise_domain.as_deref(),
        options.on_model_progress.as_ref(),
    )
    .await;
    Ok(credentials)
}

/// `githubCopilotOAuthProvider`.
pub fn github_copilot_oauth_provider() -> OAuthProviderInterface {
    OAuthProviderInterface {
        id: "github-copilot".to_string(),
        name: "GitHub Copilot".to_string(),
        uses_callback_server: None,
        login: Arc::new(|callbacks: OAuthLoginCallbacks| {
            Box::pin(async move {
                let on_auth = callbacks.on_auth.clone();
                login_github_copilot(GitHubCopilotLoginOptions {
                    on_auth: on_auth.map(|on_auth| {
                        Arc::new(move |url: String, instructions: Option<String>| {
                            on_auth(OAuthAuthInfoForCopilot { url, instructions })
                        }) as Arc<dyn Fn(String, Option<String>) + Send + Sync>
                    }),
                    on_prompt: callbacks.on_prompt.clone(),
                    on_progress: callbacks.on_progress.clone(),
                    signal: callbacks.signal.clone(),
                    on_model_progress: None,
                })
                .await
            })
        }),
        refresh_token: Arc::new(|credentials: OAuthCredentials| {
            Box::pin(async move {
                let enterprise_url = credentials
                    .extra
                    .get("enterpriseUrl")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                refresh_github_copilot_token(&credentials.refresh, enterprise_url.as_deref()).await
            })
        }),
        get_api_key: Arc::new(|credentials: &OAuthCredentials| credentials.access.clone()),
        modify_models: Some(Arc::new(|models: Vec<Model>, credentials: &OAuthCredentials| {
            let enterprise_url = credentials
                .extra
                .get("enterpriseUrl")
                .and_then(Value::as_str)
                .map(str::to_string);
            let domain = enterprise_url
                .as_deref()
                .and_then(normalize_domain);
            let base_url = get_github_copilot_base_url(Some(&credentials.access), domain.as_deref());
            models
                .into_iter()
                .map(|mut model| {
                    if model.provider == "github-copilot" {
                        model.base_url = base_url.clone();
                    }
                    model
                })
                .collect()
        })),
    }
}

/// Small struct so the copilot onAuth adapter can reuse the shared OAuthAuthInfo.
pub struct OAuthAuthInfoForCopilot {
    pub url: String,
    pub instructions: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_id_matches_decoded_typescript_literal() {
        assert_eq!(client_id(), "Iu1.b507a08c87ecfe98");
    }

    #[test]
    fn normalize_domain_accepts_hosts_and_urls() {
        assert_eq!(normalize_domain("company.ghe.com"), Some("company.ghe.com".to_string()));
        assert_eq!(
            normalize_domain("https://company.ghe.com/path"),
            Some("company.ghe.com".to_string())
        );
        assert_eq!(normalize_domain("  "), None);
        assert_eq!(normalize_domain("not a domain"), None);
    }

    #[test]
    fn urls_follow_typescript_pattern() {
        let urls = get_urls("github.com");
        assert_eq!(urls.device_code_url, "https://github.com/login/device/code");
        assert_eq!(urls.access_token_url, "https://github.com/login/oauth/access_token");
        assert_eq!(
            urls.copilot_token_url,
            "https://api.github.com/copilot_internal/v2/token"
        );
    }

    #[test]
    fn base_url_from_token_replaces_proxy_prefix() {
        let token = "tid=1;exp=2;proxy-ep=proxy.individual.githubcopilot.com;other";
        assert_eq!(
            get_base_url_from_token(token).as_deref(),
            Some("https://api.individual.githubcopilot.com")
        );
        assert_eq!(get_base_url_from_token("no proxy here"), None);
        assert_eq!(
            get_base_url_from_token("proxy-ep=host.example.com"),
            Some("https://host.example.com".to_string())
        );
    }

    #[test]
    fn base_url_prefers_token_then_enterprise_then_default() {
        assert_eq!(
            get_github_copilot_base_url(Some("proxy-ep=proxy.individual.githubcopilot.com"), None),
            "https://api.individual.githubcopilot.com"
        );
        assert_eq!(
            get_github_copilot_base_url(None, Some("company.ghe.com")),
            "https://copilot-api.company.ghe.com"
        );
        assert_eq!(
            get_github_copilot_base_url(None, None),
            "https://api.individual.githubcopilot.com"
        );
    }

    #[test]
    fn poll_multipliers_match_typescript() {
        assert_eq!(INITIAL_POLL_INTERVAL_MULTIPLIER, 1.2);
        assert_eq!(SLOW_DOWN_POLL_INTERVAL_MULTIPLIER, 1.4);
    }

    #[test]
    fn provider_identity_and_modify_models() {
        let provider = github_copilot_oauth_provider();
        assert_eq!(provider.id, "github-copilot");
        assert_eq!(provider.name, "GitHub Copilot");

        let credentials = OAuthCredentials {
            refresh: "r".to_string(),
            access: "proxy-ep=proxy.individual.githubcopilot.com".to_string(),
            expires: 0.0,
            extra: serde_json::Map::new(),
        };
        let mut model = Model::new("gpt", "GPT", "openai-completions", "github-copilot", "https://old");
        model.provider = "github-copilot".to_string();
        let other = Model::new("x", "X", "openai-completions", "openai", "https://other");
        let modify = provider.modify_models.unwrap();
        let models = modify(vec![model, other.clone()], &credentials);
        assert_eq!(models[0].base_url, "https://api.individual.githubcopilot.com");
        assert_eq!(models[1].base_url, "https://other");
    }
}
