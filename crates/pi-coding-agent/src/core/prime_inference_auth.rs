//! Port of packages/coding-agent/src/core/prime-inference-auth.ts
//!
//! `fetch` is modelled as an injectable `FetchFn` so the browser-login and
//! access-check flows stay testable without network access.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use pi_ai::types::BoxFuture;
use pi_ai::utils::oauth::types::OAuthAuthInfo;

pub const PRIME_INFERENCE_PROVIDER_ID: &str = "prime-inference";
pub const PRIME_INFERENCE_PROVIDER_NAME: &str = "Prime Inference";
pub const PRIME_AGENT_TRACES_PROVIDER_ID: &str = "prime-agent-traces";
pub const PRIME_AGENT_TRACES_PROVIDER_NAME: &str = "Prime Agent Traces";

const DEFAULT_PRIME_API_BASE_URL: &str = "https://api.primeintellect.ai";
const DEFAULT_PRIME_FRONTEND_URL: &str = "https://app.primeintellect.ai";
const DEFAULT_PRIME_INFERENCE_URL: &str = "https://api.pinference.ai/api/v1";
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_POLL_INTERVAL_MS: u64 = 5_000;

pub type PrimeInferenceAuthSource = &'static str;
pub const PRIME_INFERENCE_AUTH_SOURCE_PRIME_CLI: PrimeInferenceAuthSource = "prime-cli";
pub const PRIME_INFERENCE_AUTH_SOURCE_BROWSER: PrimeInferenceAuthSource = "browser";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimeInferenceLoginResult {
    pub api_key: String,
    pub source: PrimeInferenceAuthSource,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrimeCliConfig {
    pub api_key: Option<String>,
    pub base_url: String,
    pub frontend_url: String,
    pub inference_url: String,
    pub path: String,
    pub team_id: Option<String>,
    pub team_name: Option<String>,
    pub team_role: Option<String>,
    pub team_id_from_env: bool,
}

#[derive(Clone)]
pub struct PrimeInferenceLoginCallbacks {
    pub on_auth: Arc<dyn Fn(OAuthAuthInfo) + Send + Sync>,
    pub on_progress: Option<Arc<dyn Fn(String) + Send + Sync>>,
    pub signal: Option<CancellationToken>,
}

impl PrimeInferenceLoginCallbacks {
    pub fn new(on_auth: Arc<dyn Fn(OAuthAuthInfo) + Send + Sync>) -> Self {
        Self {
            on_auth,
            on_progress: None,
            signal: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct PrimeInferenceLoginOptions {
    pub config_path: Option<String>,
    pub fetch_fn: Option<FetchFn>,
    pub poll_interval_ms: Option<u64>,
    pub request_timeout_ms: Option<u64>,
}

impl std::fmt::Debug for PrimeInferenceLoginOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PrimeInferenceLoginOptions")
            .field("config_path", &self.config_path)
            .field("poll_interval_ms", &self.poll_interval_ms)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PrimeChallengeConfig {
    base_url: String,
    frontend_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PrimeChallengeResponse {
    challenge: String,
    status_auth_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimeInferenceAccessResult {
    Ok,
    Failed {
        status: Option<u16>,
        message: String,
    },
}

type PrimeAccessScope = &'static str;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrimeTeam {
    pub team_id: String,
    pub name: String,
    pub slug: Option<String>,
    pub role: Option<String>,
    pub created_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Injectable HTTP
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub text: String,
}

impl HttpResponse {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub type FetchFn = Arc<dyn Fn(HttpRequest) -> BoxFuture<Result<HttpResponse, String>> + Send + Sync>;

/// The Rust equivalent of the global `fetch` used by the TypeScript module.
pub fn default_fetch() -> FetchFn {
    Arc::new(|request: HttpRequest| {
        Box::pin(async move {
            let method = reqwest::Method::from_bytes(request.method.as_bytes())
                .map_err(|error| error.to_string())?;
            let client = reqwest::Client::builder()
                .timeout(Duration::from_millis(request.timeout_ms))
                .build()
                .map_err(|error| error.to_string())?;
            let mut builder = client.request(method, &request.url);
            for (key, value) in &request.headers {
                builder = builder.header(key, value);
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }
            let response = builder.send().await.map_err(|error| error.to_string())?;
            let status = response.status();
            let status_text = status.canonical_reason().unwrap_or("").to_string();
            let headers = response
                .headers()
                .iter()
                .map(|(key, value)| {
                    (
                        key.as_str().to_string(),
                        value.to_str().unwrap_or("").to_string(),
                    )
                })
                .collect();
            let text = response.text().await.map_err(|error| error.to_string())?;
            Ok(HttpResponse {
                status: status.as_u16(),
                status_text,
                headers,
                text,
            })
        }) as BoxFuture<Result<HttpResponse, String>>
    })
}

// ---------------------------------------------------------------------------
// Config file helpers
// ---------------------------------------------------------------------------

fn default_prime_cli_config_path() -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".prime")
        .join("config.json")
        .to_string_lossy()
        .to_string()
}

pub fn get_prime_cli_config_path(config_path: Option<&str>) -> String {
    config_path
        .map(|value| value.to_string())
        .unwrap_or_else(default_prime_cli_config_path)
}

fn normalize_base_url(value: Option<&str>) -> String {
    let raw = value.map(str::trim).filter(|value| !value.is_empty());
    let trimmed = raw.unwrap_or(DEFAULT_PRIME_API_BASE_URL);
    let without_slashes = trimmed.trim_end_matches('/');
    without_slashes
        .strip_suffix("/api/v1")
        .unwrap_or(without_slashes)
        .to_string()
}

fn normalize_url(value: Option<&str>, fallback: &str) -> String {
    let raw = value.unwrap_or(fallback);
    raw.trim().trim_end_matches('/').to_string()
}

fn string_field(data: &Map<String, Value>, key: &str) -> Option<String> {
    let value = data.get(key)?;
    let text = value.as_str()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn string_env(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) => {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(_) => None,
    }
}

fn number_field(data: &Map<String, Value>, key: &str) -> Option<f64> {
    let value = data.get(key)?;
    let number = value.as_f64()?;
    if number.is_finite() {
        Some(number)
    } else {
        None
    }
}

fn is_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn read_prime_cli_config_data(config_path: &str) -> Map<String, Value> {
    let mut data = Map::new();
    if Path::new(config_path).exists() {
        if let Ok(content) = std::fs::read_to_string(config_path) {
            match serde_json::from_str::<Value>(&content) {
                Ok(parsed) => {
                    if let Some(object) = is_record(&parsed) {
                        data = object.clone();
                    }
                }
                Err(_) => data = Map::new(),
            }
        }
    }
    data
}

fn write_prime_cli_config_data(config_path: &str, data: &Map<String, Value>) -> std::io::Result<()> {
    let path = Path::new(config_path);
    if let Some(dir) = path.parent() {
        if !dir.exists() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = std::fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                builder.create(dir)?;
            }
            #[cfg(not(unix))]
            {
                std::fs::create_dir_all(dir)?;
            }
        }
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    let nonce: u64 = rand::random();
    let temp_path = dir.join(format!(
        ".{}.{}.{}.{}.tmp",
        file_name,
        std::process::id(),
        now_millis(),
        base36(nonce)
    ));
    let payload = format!("{}\n", serde_json::to_string_pretty(&Value::Object(data.clone()))?);
    let result = (|| -> std::io::Result<()> {
        {
            let mut open = std::fs::OpenOptions::new();
            open.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                open.mode(0o600);
            }
            let mut file = open.open(&temp_path)?;
            use std::io::Write;
            file.write_all(payload.as_bytes())?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&temp_path, path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    })();
    if temp_path.exists() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("ascii digits")
}

fn clear_prime_team_fields(data: &mut Map<String, Value>) {
    data.remove("team_id");
    data.remove("team_name");
    data.remove("team_role");
}

pub fn load_prime_cli_config(config_path: Option<&str>) -> PrimeCliConfig {
    let path = get_prime_cli_config_path(config_path);
    let data = read_prime_cli_config_data(&path);
    let team_id_from_env = string_env("PRIME_TEAM_ID");
    let team_id = team_id_from_env
        .clone()
        .or_else(|| string_field(&data, "team_id"));

    let mut config = PrimeCliConfig {
        base_url: normalize_base_url(string_field(&data, "base_url").as_deref()),
        frontend_url: normalize_url(string_field(&data, "frontend_url").as_deref(), DEFAULT_PRIME_FRONTEND_URL),
        inference_url: normalize_url(string_field(&data, "inference_url").as_deref(), DEFAULT_PRIME_INFERENCE_URL),
        path,
        team_id_from_env: team_id_from_env.is_some(),
        ..Default::default()
    };
    if let Some(api_key) = string_field(&data, "api_key") {
        config.api_key = Some(api_key);
    }
    if let Some(team_id) = team_id {
        config.team_id = Some(team_id);
    }
    if !config.team_id_from_env {
        if let Some(team_name) = string_field(&data, "team_name") {
            config.team_name = Some(team_name);
        }
        if let Some(team_role) = string_field(&data, "team_role") {
            config.team_role = Some(team_role);
        }
    }
    config
}

pub fn save_prime_cli_api_key(api_key: &str, config_path: Option<&str>) -> std::io::Result<PrimeCliConfig> {
    let path = get_prime_cli_config_path(config_path);
    let mut data = read_prime_cli_config_data(&path);
    data.insert("api_key".to_string(), Value::String(api_key.to_string()));
    clear_prime_team_fields(&mut data);
    write_prime_cli_config_data(&path, &data)?;
    Ok(load_prime_cli_config(Some(&path)))
}

pub fn clear_prime_cli_credentials(config_path: Option<&str>) -> std::io::Result<PrimeCliConfig> {
    let path = get_prime_cli_config_path(config_path);
    let mut data = read_prime_cli_config_data(&path);
    data.remove("api_key");
    clear_prime_team_fields(&mut data);
    write_prime_cli_config_data(&path, &data)?;
    Ok(load_prime_cli_config(Some(&path)))
}

pub fn save_prime_cli_team_selection(
    team: Option<&PrimeTeam>,
    config_path: Option<&str>,
) -> std::io::Result<PrimeCliConfig> {
    let path = get_prime_cli_config_path(config_path);
    let mut data = read_prime_cli_config_data(&path);
    match team {
        Some(team) => {
            data.insert("team_id".to_string(), Value::String(team.team_id.clone()));
            data.insert("team_name".to_string(), Value::String(team.name.clone()));
            match &team.role {
                Some(role) => {
                    data.insert("team_role".to_string(), Value::String(role.clone()));
                }
                None => {
                    data.remove("team_role");
                }
            }
        }
        None => clear_prime_team_fields(&mut data),
    }
    write_prime_cli_config_data(&path, &data)?;
    Ok(load_prime_cli_config(Some(&path)))
}

pub fn resolve_prime_agent_traces_base_url(base_url: Option<&str>) -> String {
    match base_url {
        Some(value) => normalize_base_url(Some(value)),
        None => normalize_base_url(string_env("PRIME_AGENT_TRACES_BASE_URL").as_deref()),
    }
}

fn resolve_prime_agent_traces_challenge_config(config: &PrimeCliConfig) -> PrimeChallengeConfig {
    PrimeChallengeConfig {
        base_url: resolve_prime_agent_traces_base_url(None),
        frontend_url: if string_env("PRIME_AGENT_TRACES_BASE_URL").is_some() {
            config.frontend_url.clone()
        } else {
            DEFAULT_PRIME_FRONTEND_URL.to_string()
        },
    }
}

// ---------------------------------------------------------------------------
// Cancellation and timers
// ---------------------------------------------------------------------------

fn throw_if_cancelled(signal: Option<&CancellationToken>) -> Result<(), String> {
    if let Some(signal) = signal {
        if signal.is_cancelled() {
            return Err("Login cancelled".to_string());
        }
    }
    Ok(())
}

async fn abortable_sleep(ms: u64, signal: Option<&CancellationToken>) -> Result<(), String> {
    throw_if_cancelled(signal)?;
    match signal {
        Some(signal) => {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
                _ = signal.cancelled() => Err("Login cancelled".to_string()),
            }
        }
        None => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(())
        }
    }
}

async fn fetch_with_timeout(
    fetch_fn: &FetchFn,
    request: HttpRequest,
    timeout_ms: u64,
    signal: Option<&CancellationToken>,
) -> Result<HttpResponse, String> {
    throw_if_cancelled(signal)?;
    let mut request = request;
    request.timeout_ms = timeout_ms;
    match signal {
        Some(signal) => tokio::select! {
            result = fetch_fn(request) => match result {
                Ok(response) => Ok(response),
                Err(error) => {
                    if signal.is_cancelled() {
                        Err("Login cancelled".to_string())
                    } else {
                        Err(error)
                    }
                }
            },
            _ = signal.cancelled() => Err("Login cancelled".to_string()),
        },
        None => fetch_fn(request).await,
    }
}

fn read_response_message(response: &HttpResponse) -> String {
    let text = response.text.clone();
    if text.trim().is_empty() {
        return if response.status_text.is_empty() {
            "Unknown error".to_string()
        } else {
            response.status_text.clone()
        };
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
        if let Some(object) = is_record(&parsed) {
            if let Some(error) = object.get("error").and_then(is_record) {
                if let Some(message) = string_field(error, "message") {
                    return message;
                }
            }
            if let Some(detail) = string_field(object, "detail") {
                return detail;
            }
            if let Some(message) = string_field(object, "message") {
                return message;
            }
        }
    }
    text.trim().to_string()
}

fn read_json_object(response: &HttpResponse, context: &str) -> Result<Map<String, Value>, String> {
    let parsed: Value = serde_json::from_str(&response.text)
        .map_err(|_| format!("{} returned an invalid response", context))?;
    match is_record(&parsed) {
        Some(object) => Ok(object.clone()),
        None => Err(format!("{} returned an invalid response", context)),
    }
}

fn parse_prime_team(value: &Value) -> Option<PrimeTeam> {
    let object = is_record(value)?;
    let team_id = string_field(object, "teamId")?;
    let mut team = PrimeTeam {
        team_id,
        name: string_field(object, "name").unwrap_or_else(|| "Unknown".to_string()),
        slug: None,
        role: None,
        created_at: None,
    };
    team.slug = string_field(object, "slug");
    team.role = string_field(object, "role");
    team.created_at = string_field(object, "createdAt");
    Some(team)
}

pub async fn fetch_prime_teams(
    api_key: &str,
    base_url: &str,
    fetch_fn: Option<FetchFn>,
    request_timeout_ms: Option<u64>,
    signal: Option<CancellationToken>,
) -> Result<Vec<PrimeTeam>, String> {
    let fetch_fn = fetch_fn.unwrap_or_else(default_fetch);
    let request_timeout_ms = request_timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
    let mut teams: Vec<PrimeTeam> = Vec::new();
    let mut offset = 0u32;
    let limit = 100u32;

    loop {
        let url = format!(
            "{}/api/v1/user/teams?offset={}&limit={}",
            normalize_base_url(Some(base_url)),
            offset,
            limit
        );
        let response = fetch_with_timeout(
            &fetch_fn,
            HttpRequest {
                method: "GET".to_string(),
                url,
                headers: vec![
                    ("Authorization".to_string(), format!("Bearer {}", api_key)),
                    ("Accept".to_string(), "application/json".to_string()),
                ],
                body: None,
                timeout_ms: request_timeout_ms,
            },
            request_timeout_ms,
            signal.as_ref(),
        )
        .await?;

        if !response.ok() {
            return Err(format!(
                "Failed to fetch Prime teams: {}",
                read_response_message(&response)
            ));
        }

        let data = read_json_object(&response, "Prime teams")?;
        let batch = match data.get("data") {
            Some(Value::Array(items)) => items.clone(),
            _ => return Err("Prime teams response missing team data".to_string()),
        };

        for item in &batch {
            if let Some(team) = parse_prime_team(item) {
                teams.push(team);
            }
        }

        let total_count = number_field(&data, "total_count").unwrap_or(teams.len() as f64);
        if batch.is_empty() || teams.len() as f64 >= total_count {
            break;
        }
        offset += limit;
    }

    Ok(teams)
}

// ---------------------------------------------------------------------------
// RSA challenge login
// ---------------------------------------------------------------------------

/// `createPrimeChallengeKeypair()` - RSA-2048 PKCS#8 private key + SPKI public key.
fn create_prime_challenge_keypair() -> Result<(String, String), String> {
    let bits = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048)
        .map_err(|error| format!("Failed to generate Prime login keypair: {}", error))?;
    let private = {
        use rsa::pkcs8::{EncodePrivateKey, LineEnding};
        let pem = bits.to_pkcs8_pem(LineEnding::LF).map_err(|error| error.to_string())?;
        pem.as_str().to_string()
    };
    let public = {
        use rsa::pkcs8::{EncodePublicKey, LineEnding};
        let pem = bits
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .map_err(|error| error.to_string())?;
        pem
    };
    Ok((private, public))
}

/// `decryptPrimeChallengeResult(privateKey, encryptedResult)`.
fn decrypt_prime_challenge_result(private_key: &str, encrypted_result: &str) -> Result<String, String> {
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::Oaep;
    let key = rsa::RsaPrivateKey::from_pkcs8_pem(private_key).map_err(|error| error.to_string())?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encrypted_result)
        .map_err(|error| error.to_string())?;
    let padding = Oaep::new::<sha2::Sha256>();
    let decrypted = key.decrypt(padding, &bytes).map_err(|error| error.to_string())?;
    String::from_utf8(decrypted).map_err(|error| error.to_string())
}

async fn generate_prime_challenge(
    config: &PrimeChallengeConfig,
    public_key: &str,
    fetch_fn: &FetchFn,
    timeout_ms: u64,
    signal: Option<&CancellationToken>,
) -> Result<PrimeChallengeResponse, String> {
    let response = fetch_with_timeout(
        fetch_fn,
        HttpRequest {
            method: "POST".to_string(),
            url: format!("{}/api/v1/auth_challenge/generate", config.base_url),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: Some(
                serde_json::json!({ "encryptionPublicKey": public_key }).to_string(),
            ),
            timeout_ms,
        },
        timeout_ms,
        signal,
    )
    .await?;

    if !response.ok() {
        return Err(format!(
            "Failed to generate Prime login challenge: {}",
            read_response_message(&response)
        ));
    }

    let data = read_json_object(&response, "Prime login challenge")?;
    let challenge = string_field(&data, "challenge");
    let status_auth_token = string_field(&data, "status_auth_token");
    match (challenge, status_auth_token) {
        (Some(challenge), Some(status_auth_token)) => Ok(PrimeChallengeResponse {
            challenge,
            status_auth_token,
        }),
        _ => Err("Prime login challenge response missing required fields".to_string()),
    }
}

async fn poll_prime_challenge_result(
    config: &PrimeChallengeConfig,
    challenge: &PrimeChallengeResponse,
    private_key: &str,
    fetch_fn: &FetchFn,
    timeout_ms: u64,
    poll_interval_ms: u64,
    signal: Option<&CancellationToken>,
) -> Result<String, String> {
    loop {
        throw_if_cancelled(signal)?;

        let status_url = format!(
            "{}/api/v1/auth_challenge/status?challenge={}",
            config.base_url,
            url_encode(&challenge.challenge)
        );
        let response = fetch_with_timeout(
            fetch_fn,
            HttpRequest {
                method: "GET".to_string(),
                url: status_url,
                headers: vec![(
                    "Authorization".to_string(),
                    format!("Bearer {}", challenge.status_auth_token),
                )],
                body: None,
                timeout_ms,
            },
            timeout_ms,
            signal,
        )
        .await?;

        if response.status == 404 {
            return Err("Prime login challenge expired".to_string());
        }
        if !response.ok() {
            return Err(format!(
                "Failed to check Prime login status: {}",
                read_response_message(&response)
            ));
        }

        let data = read_json_object(&response, "Prime login status")?;
        if let Some(encrypted_result) = string_field(&data, "result") {
            return decrypt_prime_challenge_result(private_key, &encrypted_result);
        }

        abortable_sleep(poll_interval_ms, signal).await?;
    }
}

/// `encodeURIComponent`.
fn url_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

async fn run_prime_browser_login(
    config: &PrimeChallengeConfig,
    callbacks: &PrimeInferenceLoginCallbacks,
    fetch_fn: &FetchFn,
    timeout_ms: u64,
    poll_interval_ms: u64,
    scope: Option<PrimeAccessScope>,
) -> Result<String, String> {
    let (private_key, public_key) = create_prime_challenge_keypair()?;
    let challenge =
        generate_prime_challenge(config, &public_key, fetch_fn, timeout_ms, callbacks.signal.as_ref()).await?;
    let mut url = format!("{}/dashboard/tokens/challenge", config.frontend_url);
    url.push_str(&format!("?code={}", url_encode(&challenge.challenge)));
    if let Some(scope) = scope {
        url.push_str(&format!("&scope={}", url_encode(scope)));
    }
    (callbacks.on_auth)(OAuthAuthInfo {
        url,
        instructions: Some(format!("Code: {}", challenge.challenge)),
    });
    poll_prime_challenge_result(
        config,
        &challenge,
        &private_key,
        fetch_fn,
        timeout_ms,
        poll_interval_ms,
        callbacks.signal.as_ref(),
    )
    .await
}

async fn check_prime_scope_access(
    api_key: &str,
    base_url: &str,
    scope_name: PrimeAccessScope,
    scope_label: &str,
    fetch_fn: Option<FetchFn>,
    request_timeout_ms: Option<u64>,
    signal: Option<CancellationToken>,
) -> Result<PrimeInferenceAccessResult, String> {
    let fetch_fn = fetch_fn.unwrap_or_else(default_fetch);
    let request_timeout_ms = request_timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
    let url = format!("{}/api/v1/user/whoami", normalize_base_url(Some(base_url)));
    let response = fetch_with_timeout(
        &fetch_fn,
        HttpRequest {
            method: "GET".to_string(),
            url,
            headers: vec![
                ("Authorization".to_string(), format!("Bearer {}", api_key)),
                ("Accept".to_string(), "application/json".to_string()),
            ],
            body: None,
            timeout_ms: request_timeout_ms,
        },
        request_timeout_ms,
        signal.as_ref(),
    )
    .await?;

    if !response.ok() {
        return Ok(PrimeInferenceAccessResult::Failed {
            status: Some(response.status),
            message: read_response_message(&response),
        });
    }

    let data = read_json_object(&response, "Prime whoami")?;
    let user = match data.get("data").and_then(is_record) {
        Some(user) => user.clone(),
        None => {
            return Ok(PrimeInferenceAccessResult::Failed {
                status: None,
                message: "Prime whoami response missing user data".to_string(),
            })
        }
    };

    let scope = match user.get("scope").and_then(is_record) {
        Some(scope) => scope.clone(),
        None => {
            return Ok(PrimeInferenceAccessResult::Failed {
                status: None,
                message: "Prime token is missing permission scope data".to_string(),
            })
        }
    };

    let scoped_permission = match scope.get(scope_name).and_then(is_record) {
        Some(permission) => permission,
        None => {
            return Ok(PrimeInferenceAccessResult::Failed {
                status: None,
                message: format!("Prime token is missing {} permissions", scope_label),
            })
        }
    };

    if scoped_permission.get("write") == Some(&Value::Bool(true)) {
        return Ok(PrimeInferenceAccessResult::Ok);
    }

    Ok(PrimeInferenceAccessResult::Failed {
        status: None,
        message: format!("Prime token does not have {} write permission", scope_label),
    })
}

pub async fn check_prime_inference_access(
    api_key: &str,
    base_url: &str,
    fetch_fn: Option<FetchFn>,
    request_timeout_ms: Option<u64>,
    signal: Option<CancellationToken>,
) -> Result<PrimeInferenceAccessResult, String> {
    check_prime_scope_access(
        api_key,
        base_url,
        "inference",
        "inference",
        fetch_fn,
        request_timeout_ms,
        signal,
    )
    .await
}

pub async fn check_prime_agent_traces_access(
    api_key: &str,
    base_url: &str,
    fetch_fn: Option<FetchFn>,
    request_timeout_ms: Option<u64>,
    signal: Option<CancellationToken>,
) -> Result<PrimeInferenceAccessResult, String> {
    check_prime_scope_access(
        api_key,
        base_url,
        "agent_traces",
        "agent trace",
        fetch_fn,
        request_timeout_ms,
        signal,
    )
    .await
}

fn format_access_failure(result: &PrimeInferenceAccessResult) -> String {
    match result {
        PrimeInferenceAccessResult::Ok => String::new(),
        PrimeInferenceAccessResult::Failed { status, message } => match status {
            Some(status) => format!("HTTP {}: {}", status, message),
            None => message.clone(),
        },
    }
}

pub async fn login_prime_inference(
    callbacks: &PrimeInferenceLoginCallbacks,
    options: PrimeInferenceLoginOptions,
) -> Result<PrimeInferenceLoginResult, String> {
    let config = load_prime_cli_config(options.config_path.as_deref());
    let fetch_fn = options.fetch_fn.clone().unwrap_or_else(default_fetch);
    let request_timeout_ms = options.request_timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
    let poll_interval_ms = options.poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);
    let challenge_config = PrimeChallengeConfig {
        base_url: config.base_url.clone(),
        frontend_url: config.frontend_url.clone(),
    };

    if let Some(api_key) = config.api_key.clone() {
        progress(callbacks, "Checking existing Prime CLI credentials...");
        let access = check_prime_inference_access(
            &api_key,
            &config.base_url,
            Some(fetch_fn.clone()),
            Some(request_timeout_ms),
            callbacks.signal.clone(),
        )
        .await?;
        if access == PrimeInferenceAccessResult::Ok {
            throw_if_cancelled(callbacks.signal.as_ref())?;
            return Ok(PrimeInferenceLoginResult {
                api_key,
                source: PRIME_INFERENCE_AUTH_SOURCE_PRIME_CLI,
            });
        }
        progress(
            callbacks,
            &format!(
                "Existing Prime CLI key cannot access Prime Inference ({}). Starting browser login...",
                format_access_failure(&access)
            ),
        );
    } else {
        progress(callbacks, "No Prime CLI API key found. Starting browser login...");
    }

    let api_key = run_prime_browser_login(
        &challenge_config,
        callbacks,
        &fetch_fn,
        request_timeout_ms,
        poll_interval_ms,
        None,
    )
    .await?;
    throw_if_cancelled(callbacks.signal.as_ref())?;
    progress(callbacks, "Checking Prime Inference access...");
    let access = check_prime_inference_access(
        &api_key,
        &config.base_url,
        Some(fetch_fn),
        Some(request_timeout_ms),
        callbacks.signal.clone(),
    )
    .await?;
    if access != PrimeInferenceAccessResult::Ok {
        return Err(format!(
            "Prime API key does not have Prime Inference access ({})",
            format_access_failure(&access)
        ));
    }

    throw_if_cancelled(callbacks.signal.as_ref())?;
    Ok(PrimeInferenceLoginResult {
        api_key,
        source: PRIME_INFERENCE_AUTH_SOURCE_BROWSER,
    })
}

pub async fn login_prime_agent_traces(
    callbacks: &PrimeInferenceLoginCallbacks,
    options: PrimeInferenceLoginOptions,
) -> Result<PrimeInferenceLoginResult, String> {
    let config = load_prime_cli_config(options.config_path.as_deref());
    let trace_config = resolve_prime_agent_traces_challenge_config(&config);
    let fetch_fn = options.fetch_fn.clone().unwrap_or_else(default_fetch);
    let request_timeout_ms = options.request_timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
    let poll_interval_ms = options.poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);

    if let Some(api_key) = config.api_key.clone() {
        progress(callbacks, "Checking existing Prime CLI credentials...");
        let access = check_prime_agent_traces_access(
            &api_key,
            &trace_config.base_url,
            Some(fetch_fn.clone()),
            Some(request_timeout_ms),
            callbacks.signal.clone(),
        )
        .await?;
        if access == PrimeInferenceAccessResult::Ok {
            throw_if_cancelled(callbacks.signal.as_ref())?;
            return Ok(PrimeInferenceLoginResult {
                api_key,
                source: PRIME_INFERENCE_AUTH_SOURCE_PRIME_CLI,
            });
        }
        progress(
            callbacks,
            &format!(
                "Existing Prime CLI key cannot upload Prime Agent traces ({}). Starting browser login...",
                format_access_failure(&access)
            ),
        );
    } else {
        progress(callbacks, "No Prime CLI API key found. Starting browser login...");
    }

    let api_key = run_prime_browser_login(
        &trace_config,
        callbacks,
        &fetch_fn,
        request_timeout_ms,
        poll_interval_ms,
        Some("agent_traces"),
    )
    .await?;
    throw_if_cancelled(callbacks.signal.as_ref())?;
    progress(callbacks, "Checking Prime Agent trace access...");
    let access = check_prime_agent_traces_access(
        &api_key,
        &trace_config.base_url,
        Some(fetch_fn),
        Some(request_timeout_ms),
        callbacks.signal.clone(),
    )
    .await?;
    if access != PrimeInferenceAccessResult::Ok {
        return Err(format!(
            "Prime API key does not have Prime Agent trace access ({})",
            format_access_failure(&access)
        ));
    }

    throw_if_cancelled(callbacks.signal.as_ref())?;
    Ok(PrimeInferenceLoginResult {
        api_key,
        source: PRIME_INFERENCE_AUTH_SOURCE_BROWSER,
    })
}

fn progress(callbacks: &PrimeInferenceLoginCallbacks, message: &str) {
    if let Some(on_progress) = &callbacks.on_progress {
        on_progress(message.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn json_response(status: u16, body: Value) -> HttpResponse {
        HttpResponse {
            status,
            status_text: String::new(),
            headers: Vec::new(),
            text: body.to_string(),
        }
    }

    fn recording_fetch(
        responses: Vec<HttpResponse>,
        seen: Arc<Mutex<Vec<HttpRequest>>>,
    ) -> FetchFn {
        let responses = Arc::new(Mutex::new(responses.into_iter()));
        Arc::new(move |request: HttpRequest| {
            seen.lock().unwrap().push(request);
            let next = responses.lock().unwrap().next();
            Box::pin(async move {
                next.ok_or_else(|| "no more responses".to_string())
            }) as BoxFuture<Result<HttpResponse, String>>
        })
    }

    #[test]
    fn base_url_normalisation_matches_typescript() {
        assert_eq!(normalize_base_url(None), DEFAULT_PRIME_API_BASE_URL);
        assert_eq!(normalize_base_url(Some("https://x.test/api/v1/")), "https://x.test");
        assert_eq!(normalize_base_url(Some("  https://x.test//  ")), "https://x.test");
        assert_eq!(normalize_url(Some("https://y.test/"), DEFAULT_PRIME_FRONTEND_URL), "https://y.test");
        assert_eq!(normalize_url(None, DEFAULT_PRIME_FRONTEND_URL), DEFAULT_PRIME_FRONTEND_URL);
    }

    #[test]
    fn config_load_defaults_without_file() {
        let missing = format!(
            "{}/.port-env-tmp-prime-cli-missing-{}.json",
            std::env::temp_dir().to_string_lossy(),
            std::process::id()
        );
        let config = load_prime_cli_config(Some(&missing));
        assert_eq!(config.base_url, DEFAULT_PRIME_API_BASE_URL);
        assert_eq!(config.frontend_url, DEFAULT_PRIME_FRONTEND_URL);
        assert_eq!(config.inference_url, DEFAULT_PRIME_INFERENCE_URL);
        assert!(config.api_key.is_none());
        assert!(!config.team_id_from_env);
    }

    #[test]
    fn save_api_key_clears_team_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let path = path.to_string_lossy().to_string();
        std::fs::write(&path, r#"{"api_key":"old","team_id":"t1","team_name":"T1","team_role":"admin"}"#).unwrap();
        let config = save_prime_cli_api_key("new", Some(&path)).unwrap();
        assert_eq!(config.api_key.as_deref(), Some("new"));
        assert!(config.team_id.is_none());
        assert!(config.team_name.is_none());
        assert!(config.team_role.is_none());
    }

    #[test]
    fn save_team_selection_writes_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let path = path.to_string_lossy().to_string();
        let team = PrimeTeam {
            team_id: "t1".to_string(),
            name: "Team One".to_string(),
            slug: Some("team-one".to_string()),
            role: Some("admin".to_string()),
            created_at: None,
        };
        let config = save_prime_cli_team_selection(Some(&team), Some(&path)).unwrap();
        assert_eq!(config.team_id.as_deref(), Some("t1"));
        assert_eq!(config.team_name.as_deref(), Some("Team One"));
        assert_eq!(config.team_role.as_deref(), Some("admin"));

        let config = save_prime_cli_team_selection(None, Some(&path)).unwrap();
        assert!(config.team_id.is_none());
        assert!(config.team_name.is_none());
    }

    #[test]
    fn clear_credentials_removes_api_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let path = path.to_string_lossy().to_string();
        std::fs::write(&path, r#"{"api_key":"k","base_url":"https://b.test"}"#).unwrap();
        let config = clear_prime_cli_credentials(Some(&path)).unwrap();
        assert!(config.api_key.is_none());
        assert_eq!(config.base_url, "https://b.test");
    }

    #[tokio::test]
    async fn fetch_prime_teams_paginates_and_stops_at_total() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let first = json_response(
            200,
            serde_json::json!({
                "data": [{"teamId": "t1", "name": "One"}, {"teamId": "t2", "name": "Two", "slug": "two"}],
                "total_count": 3
            }),
        );
        let second = json_response(
            200,
            serde_json::json!({
                "data": [{"teamId": "t3", "name": "Three", "role": "admin", "createdAt": "2024-01-01"}],
                "total_count": 3
            }),
        );
        let fetch_fn = recording_fetch(vec![first, second], seen.clone());
        let teams = fetch_prime_teams("k", "https://b.test", Some(fetch_fn), None, None)
            .await
            .unwrap();
        assert_eq!(teams.len(), 3);
        assert_eq!(teams[0].team_id, "t1");
        assert_eq!(teams[1].slug.as_deref(), Some("two"));
        assert_eq!(teams[2].role.as_deref(), Some("admin"));
        assert_eq!(teams[2].created_at.as_deref(), Some("2024-01-01"));
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].url.ends_with("/api/v1/user/teams?offset=0&limit=100"));
        assert!(requests[1].url.ends_with("/api/v1/user/teams?offset=100&limit=100"));
    }

    #[tokio::test]
    async fn fetch_prime_teams_reports_missing_team_data() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let fetch_fn = recording_fetch(vec![json_response(200, serde_json::json!({"data": "nope"}))], seen);
        let error = fetch_prime_teams("k", "https://b.test", Some(fetch_fn), None, None)
            .await
            .unwrap_err();
        assert_eq!(error, "Prime teams response missing team data");
    }

    #[tokio::test]
    async fn check_inference_access_requires_write_scope() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let allowed = recording_fetch(
            vec![json_response(
                200,
                serde_json::json!({"data": {"scope": {"inference": {"write": true}}}}),
            )],
            seen.clone(),
        );
        assert_eq!(
            check_prime_inference_access("k", "https://b.test", Some(allowed), None, None)
                .await
                .unwrap(),
            PrimeInferenceAccessResult::Ok
        );

        let denied = recording_fetch(
            vec![json_response(
                200,
                serde_json::json!({"data": {"scope": {"inference": {"write": false}}}}),
            )],
            seen.clone(),
        );
        assert_eq!(
            check_prime_inference_access("k", "https://b.test", Some(denied), None, None)
                .await
                .unwrap(),
            PrimeInferenceAccessResult::Failed {
                status: None,
                message: "Prime token does not have inference write permission".to_string()
            }
        );

        let missing = recording_fetch(vec![json_response(200, serde_json::json!({"data": {"scope": {}}}))], seen);
        assert_eq!(
            check_prime_agent_traces_access("k", "https://b.test", Some(missing), None, None)
                .await
                .unwrap(),
            PrimeInferenceAccessResult::Failed {
                status: None,
                message: "Prime token is missing agent trace permissions".to_string()
            }
        );
    }

    #[tokio::test]
    async fn access_failure_keeps_status_and_message() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let fetch_fn = recording_fetch(
            vec![HttpResponse {
                status: 403,
                status_text: "Forbidden".to_string(),
                headers: Vec::new(),
                text: r#"{"detail":"nope"}"#.to_string(),
            }],
            seen,
        );
        let result = check_prime_inference_access("k", "https://b.test", Some(fetch_fn), None, None)
            .await
            .unwrap();
        assert_eq!(
            result,
            PrimeInferenceAccessResult::Failed {
                status: Some(403),
                message: "nope".to_string()
            }
        );
        assert_eq!(format_access_failure(&result), "HTTP 403: nope");
    }

    #[tokio::test]
    async fn cancelled_login_callbacks_stop_early() {
        let signal = CancellationToken::new();
        signal.cancel();
        let callbacks = PrimeInferenceLoginCallbacks {
            on_auth: Arc::new(|_| {}),
            on_progress: None,
            signal: Some(signal),
        };
        let error = login_prime_inference(&callbacks, PrimeInferenceLoginOptions::default())
            .await
            .unwrap_err();
        assert_eq!(error, "Login cancelled");
    }

    #[test]
    fn url_encoding_matches_encode_uri_component() {
        assert_eq!(url_encode("a b/c"), "a+b%2Fc");
        assert_eq!(url_encode("plain"), "plain");
    }

    #[test]
    fn response_message_prefers_error_message() {
        let response = HttpResponse {
            status: 400,
            status_text: "Bad Request".to_string(),
            headers: Vec::new(),
            text: r#"{"error":{"message":"inner"}}"#.to_string(),
        };
        assert_eq!(read_response_message(&response), "inner");
        let empty = HttpResponse {
            status: 500,
            status_text: "Server Error".to_string(),
            headers: Vec::new(),
            text: "".to_string(),
        };
        assert_eq!(read_response_message(&empty), "Server Error");
    }
}
