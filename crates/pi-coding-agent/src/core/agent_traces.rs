//! Port of packages/coding-agent/src/core/agent-traces.ts

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use indexmap::IndexMap;
use pi_ai::types::BoxFuture;
use regex::Regex;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::core::auth_storage::AuthStorage;
use crate::core::prime_inference_auth::{
    load_prime_cli_config, resolve_prime_agent_traces_base_url, FetchFn, HttpRequest, HttpResponse,
    PRIME_AGENT_TRACES_PROVIDER_ID, PRIME_INFERENCE_PROVIDER_ID,
};
use crate::utils::file_lines::read_first_line_sync;

const MAX_TRACE_BYTES: u64 = 20 * 1024 * 1024;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 15_000;
const TRACE_UPLOAD_DEBOUNCE_MS: u64 = 1_000;
const TRACE_UPLOAD_MIN_INTERVAL_MS: u64 = 60_000;
const TRACE_UPLOAD_RETRY_BASE_DELAY_MS: u64 = 500;
const TRACE_UPLOAD_RETRY_MAX_DELAY_MS: u64 = 10_000;
const TRACE_UPLOAD_MAX_RETRIES: usize = 3;
const TRACE_UPLOAD_RETRY_JITTER: f64 = 0.2;
const TRACE_PREVIEW_MAX_CHARS: usize = 8_000;
const TRACE_UPLOAD_ALL_CONCURRENCY: usize = 4;
const TRACE_UPLOAD_RATE_LIMIT_REQUESTS: u64 = 5;
const TRACE_UPLOAD_RATE_LIMIT_WINDOW_MS: u64 = 60_000;
const TRACE_UPLOAD_RATE_LIMIT_SAFETY_MS: u64 = 100;
const MAX_TIMER_DELAY_MS: u64 = 2u64.pow(31) - 1;
const TRACE_UPLOAD_ALL_MIN_REQUEST_INTERVAL_MS: u64 =
    TRACE_UPLOAD_RATE_LIMIT_WINDOW_MS / TRACE_UPLOAD_RATE_LIMIT_REQUESTS + TRACE_UPLOAD_RATE_LIMIT_SAFETY_MS;

/// `VERSION` from config.ts (package.json version).
pub(crate) const VERSION: &str = "0.9.3";

pub type AgentTraceCredentialSource = &'static str;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTraceCredential {
    pub api_key: String,
    pub source: AgentTraceCredentialSource,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentTraceUploadResult {
    Uploaded {
        session_id: String,
        trace_id: String,
        bytes_stored: u64,
        key: Option<String>,
    },
    Disabled,
    Unchanged,
    MissingCredentials,
    NoSessionFile,
    EmptySession,
    InvalidSession {
        message: String,
    },
    TooLarge {
        size: u64,
        max_bytes: u64,
    },
    Failed {
        status_code: Option<u16>,
        message: String,
        retry_after_ms: Option<u64>,
    },
}

impl AgentTraceUploadResult {
    pub fn status(&self) -> &'static str {
        match self {
            AgentTraceUploadResult::Uploaded { .. } => "uploaded",
            AgentTraceUploadResult::Disabled => "disabled",
            AgentTraceUploadResult::Unchanged => "unchanged",
            AgentTraceUploadResult::MissingCredentials => "missing_credentials",
            AgentTraceUploadResult::NoSessionFile => "no_session_file",
            AgentTraceUploadResult::EmptySession => "empty_session",
            AgentTraceUploadResult::InvalidSession { .. } => "invalid_session",
            AgentTraceUploadResult::TooLarge { .. } => "too_large",
            AgentTraceUploadResult::Failed { .. } => "failed",
        }
    }
}

/// `interface AgentTraceUploadOptions` - `settingsManager` and `authStorage`
/// are the coding-agent ports of those classes.
#[derive(Clone)]
pub struct AgentTraceUploadOptions {
    pub session_file: Option<String>,
    pub auth_storage: Arc<tokio::sync::Mutex<AuthStorage>>,
    /// Require the global automatic-sharing opt-in. `false` only for an explicit
    /// one-shot upload command.
    pub require_enabled: bool,
    pub base_url: Option<String>,
    pub config_path: Option<String>,
    pub fetch_fn: Option<FetchFn>,
    pub reload_config: bool,
    pub request_timeout_ms: Option<u64>,
    pub signal: Option<CancellationToken>,
    /// `SettingsManager.getAgentTracesEnabled()`; the settings slice owns the class.
    pub agent_traces_enabled: Arc<dyn Fn() -> bool + Send + Sync>,
    /// `SettingsManager.reload()`.
    pub reload_settings: Arc<dyn Fn() -> BoxFuture<Result<(), String>> + Send + Sync>,
}

impl std::fmt::Debug for AgentTraceUploadOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentTraceUploadOptions")
            .field("session_file", &self.session_file)
            .field("require_enabled", &self.require_enabled)
            .field("base_url", &self.base_url)
            .field("config_path", &self.config_path)
            .field("reload_config", &self.reload_config)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .finish_non_exhaustive()
    }
}

/// `interface AgentTraceSessionUploadOptions` - `sessionManager` narrowed to
/// `getSessionFile()` plus `onPersist(listener)`, which the session slice owns.
#[derive(Clone)]
pub struct AgentTraceSessionUploadOptions {
    pub upload: AgentTraceUploadOptions,
    pub get_session_file: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

#[derive(Clone)]
pub struct AgentTraceUploadInstallOptions {
    pub auth_storage: Arc<tokio::sync::Mutex<AuthStorage>>,
    pub base_url: Option<String>,
    pub config_path: Option<String>,
    pub fetch_fn: Option<FetchFn>,
    pub request_timeout_ms: Option<u64>,
    /// The session's semantic-edge ledger; registered with the outbox as its own
    /// delivery kind.
    pub semantic_edges_ledger_path: Option<String>,
    pub agent_traces_enabled: Arc<dyn Fn() -> bool + Send + Sync>,
    pub reload_settings: Arc<dyn Fn() -> BoxFuture<Result<(), String>> + Send + Sync>,
    pub get_session_file: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

impl std::fmt::Debug for AgentTraceUploadInstallOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentTraceUploadInstallOptions")
            .field("base_url", &self.base_url)
            .field("config_path", &self.config_path)
            .field("semantic_edges_ledger_path", &self.semantic_edges_ledger_path)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentTracePreviewResult {
    Ready {
        session_file: String,
        session_id: String,
        trace_id: String,
        parent_session_id: Option<String>,
        cwd: String,
        size: u64,
        max_bytes: u64,
        uploadable: bool,
        endpoint: String,
        git_repo: Option<String>,
        git_commit: Option<String>,
        content_preview: String,
        truncated: bool,
    },
    NoSessionFile,
    EmptySession,
    InvalidSession {
        message: String,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct AgentTracePreviewOptions {
    pub session_file: Option<String>,
    pub base_url: Option<String>,
    pub max_content_chars: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentTraceUploadAllProgress {
    pub completed: usize,
    pub total: usize,
    pub session_file: Option<String>,
    pub result: Option<AgentTraceUploadResult>,
}

#[derive(Clone)]
pub struct AgentTraceUploadAllOptions {
    pub upload: AgentTraceUploadOptions,
    pub session_dir: Option<String>,
    pub concurrency: Option<usize>,
    pub on_progress: Option<Arc<dyn Fn(AgentTraceUploadAllProgress) + Send + Sync>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentTraceUploadAllResult {
    pub total: usize,
    pub uploaded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub bytes_stored: u64,
    pub results: Vec<(String, AgentTraceUploadResult)>,
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

fn is_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn describe_error(error: &str) -> String {
    error.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TraceUploadTimeoutError {
    timeout_ms: u64,
}

impl std::fmt::Display for TraceUploadTimeoutError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Trace upload timed out after {}ms", self.timeout_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TraceUploadError {
    Timeout(TraceUploadTimeoutError),
    Message(String),
    Cancelled,
}

impl TraceUploadError {
    fn message(&self) -> String {
        match self {
            TraceUploadError::Timeout(error) => error.to_string(),
            TraceUploadError::Message(message) => message.clone(),
            TraceUploadError::Cancelled => "Trace upload cancelled".to_string(),
        }
    }

    fn is_retriable(&self) -> bool {
        match self {
            TraceUploadError::Timeout(_) => true,
            TraceUploadError::Cancelled => false,
            TraceUploadError::Message(_) => false,
        }
    }
}

fn retriable_http_statuses() -> [u16; 7] {
    [408, 425, 500, 502, 503, 504, 0]
}

fn is_retriable_http_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 500 | 502 | 503 | 504)
}

fn string_field(data: &Map<String, Value>, key: &str) -> Option<String> {
    let value = data.get(key)?.as_str()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn number_field(data: &Map<String, Value>, key: &str) -> Option<f64> {
    let value = data.get(key)?.as_f64()?;
    if value.is_finite() {
        Some(value)
    } else {
        None
    }
}

/// `interface SessionHeader` (session-manager.ts) - the fields this module reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionHeader {
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    pub parent_session: Option<String>,
    pub rlm_depth: Option<f64>,
    pub git: Option<Map<String, Value>>,
}

fn is_session_header(value: &Value) -> Option<SessionHeader> {
    let object = is_record(value)?;
    if object.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let id = object.get("id").and_then(Value::as_str)?;
    let timestamp = object.get("timestamp").and_then(Value::as_str)?;
    let cwd = object.get("cwd").and_then(Value::as_str)?;
    let parent_session = match object.get("parentSession") {
        None => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return None,
    };
    Some(SessionHeader {
        id: id.to_string(),
        timestamp: timestamp.to_string(),
        cwd: cwd.to_string(),
        parent_session,
        rlm_depth: object.get("rlmDepth").and_then(Value::as_f64),
        git: object.get("git").and_then(is_record).cloned(),
    })
}

fn read_session_header(session_file: &str) -> Option<SessionHeader> {
    let first_line = read_first_line_sync(session_file, 0)?;
    if first_line.trim().is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_str(&first_line).ok()?;
    is_session_header(&parsed)
}

/// Active-branch git for the indexing headers: walk leaf to root, not the last
/// git_state in file order (which may belong to a sibling branch).
pub fn active_git_context(body: &str, header: &SessionHeader) -> Option<Map<String, Value>> {
    #[derive(Clone)]
    struct Entry {
        parent_id: Option<String>,
        entry_type: String,
        git: Option<Value>,
    }

    let mut by_id: IndexMap<String, Entry> = IndexMap::new();
    let mut leaf_id: Option<String> = None;
    for line in body.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(object) = is_record(&parsed) else {
            continue;
        };
        if object.get("type").and_then(Value::as_str) == Some("session") {
            continue;
        }
        let Some(id) = object.get("id").and_then(Value::as_str) else {
            continue;
        };
        by_id.insert(
            id.to_string(),
            Entry {
                parent_id: object.get("parentId").and_then(Value::as_str).map(str::to_string),
                entry_type: object
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                git: object.get("git").cloned(),
            },
        );
        leaf_id = Some(id.to_string());
    }

    let mut current = leaf_id.and_then(|id| by_id.get(&id).cloned());
    let mut depth = 0usize;
    while let Some(entry) = current {
        if depth >= by_id.len() + 1 {
            break;
        }
        if entry.entry_type == "git_state" {
            if let Some(git) = entry.git.as_ref().and_then(is_record) {
                return Some(git.clone());
            }
        }
        current = entry.parent_id.and_then(|id| by_id.get(&id).cloned());
        depth += 1;
    }
    header.git.clone()
}

fn resolve_parent_session_path(session_file: &str, parent_session: &str) -> String {
    let path = Path::new(parent_session);
    if path.is_absolute() {
        return parent_session.to_string();
    }
    let parent_dir = Path::new(session_file).parent().unwrap_or_else(|| Path::new("."));
    parent_dir.join(parent_session).to_string_lossy().to_string()
}

fn resolve_trace_context(session_file: &str, header: &SessionHeader) -> (String, Option<String>) {
    let mut trace_id = header.id.clone();
    let mut parent_session_id: Option<String> = None;
    let mut current_file = session_file.to_string();
    let mut current_header = header.clone();

    for depth in 0..32 {
        let Some(parent_session) = current_header.parent_session.clone() else {
            break;
        };

        let parent_path = resolve_parent_session_path(&current_file, &parent_session);
        let Some(parent_header) = read_session_header(&parent_path) else {
            break;
        };

        if depth == 0 {
            parent_session_id = Some(parent_header.id.clone());
        }
        trace_id = parent_header.id.clone();
        current_file = parent_path;
        current_header = parent_header;
    }

    (trace_id, parent_session_id)
}

fn parse_response_object(text: &str) -> Option<Map<String, Value>> {
    if text.trim().is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_str(text).ok()?;
    is_record(&parsed).cloned()
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

    if let Some(parsed) = parse_response_object(&text) {
        if let Some(error) = parsed.get("error").and_then(is_record) {
            if let Some(message) = string_field(error, "message") {
                return message;
            }
        }
        if let Some(detail) = string_field(&parsed, "detail") {
            return detail;
        }
        if let Some(message) = string_field(&parsed, "message") {
            return message;
        }
    }

    text.trim().to_string()
}

async fn fetch_with_timeout(
    fetch_fn: &FetchFn,
    url: &str,
    init: HttpRequest,
    timeout_ms: u64,
    signal: Option<&CancellationToken>,
) -> Result<HttpResponse, TraceUploadError> {
    if signal.map(CancellationToken::is_cancelled).unwrap_or(false) {
        return Err(TraceUploadError::Cancelled);
    }
    let mut request = init;
    request.url = url.to_string();
    request.timeout_ms = timeout_ms;
    let fetch = fetch_fn(request);
    let outcome = match signal {
        Some(signal) => {
            tokio::select! {
                result = fetch => result,
                _ = signal.cancelled() => return Err(TraceUploadError::Cancelled),
            }
        }
        None => fetch.await,
    };
    match outcome {
        Ok(response) => Ok(response),
        Err(error) => {
            if signal.map(CancellationToken::is_cancelled).unwrap_or(false) {
                return Err(TraceUploadError::Cancelled);
            }
            Err(TraceUploadError::Message(error))
        }
    }
}

async fn delay(ms: u64, signal: Option<&CancellationToken>) -> () {
    if signal.map(CancellationToken::is_cancelled).unwrap_or(false) {
        return;
    }
    match signal {
        Some(signal) => {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
                _ = signal.cancelled() => {}
            }
        }
        None => tokio::time::sleep(Duration::from_millis(ms)).await,
    }
}

fn trace_upload_retry_delay(retry_index: usize) -> u64 {
    let exponential_delay = TRACE_UPLOAD_RETRY_MAX_DELAY_MS
        .min(TRACE_UPLOAD_RETRY_BASE_DELAY_MS * 2u64.pow(retry_index as u32));
    let jitter_multiplier = 1.0 - TRACE_UPLOAD_RETRY_JITTER + rand::random::<f64>() * TRACE_UPLOAD_RETRY_JITTER * 2.0;
    (exponential_delay as f64 * jitter_multiplier).round().max(0.0) as u64
}

fn retry_after_delay(response: &HttpResponse, cap_ms: u64) -> Option<u64> {
    let value = response.header("retry-after")?.trim().to_string();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<f64>() {
        if seconds.is_finite() && seconds >= 0.0 {
            return Some(((seconds * 1_000.0).ceil() as u64).min(cap_ms));
        }
    }
    let retry_at = parse_http_date(&value)?;
    Some(((retry_at - now_millis()).max(0) as u64).min(cap_ms))
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

/// `Date.parse(value)` for the RFC 1123 date form used by `Retry-After`.
fn parse_http_date(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc2822(value)
        .map(|parsed| parsed.timestamp_millis())
        .ok()
}

/// `type BeforeTraceUploadRequest = () => Promise<void>`.
pub type BeforeTraceUploadRequest = Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>;

async fn fetch_with_retry(
    fetch_fn: &FetchFn,
    url: &str,
    init: HttpRequest,
    timeout_ms: u64,
    signal: Option<&CancellationToken>,
    before_request: Option<&BeforeTraceUploadRequest>,
) -> Result<HttpResponse, TraceUploadError> {
    let mut attempt = 0usize;
    loop {
        let mut retry_delay_ms: Option<u64> = None;
        let mut retry = false;
        match async {
            if let Some(before_request) = before_request {
                before_request().await;
            }
            if signal.map(CancellationToken::is_cancelled).unwrap_or(false) {
                return Err(TraceUploadError::Cancelled);
            }
            let response = fetch_with_timeout(fetch_fn, url, init.clone(), timeout_ms, signal).await?;
            if attempt >= TRACE_UPLOAD_MAX_RETRIES || !is_retriable_http_status(response.status) {
                return Ok(response);
            }
            if response.status == 503 {
                retry_delay_ms = retry_after_delay(&response, TRACE_UPLOAD_RATE_LIMIT_WINDOW_MS);
            }
            Err(TraceUploadError::Message("__retry__".to_string()))
        }
        .await
        {
            Ok(response) => return Ok(response),
            Err(TraceUploadError::Message(message)) if message == "__retry__" => {
                retry = true;
            }
            Err(error) => {
                if signal.map(CancellationToken::is_cancelled).unwrap_or(false) {
                    return Err(TraceUploadError::Cancelled);
                }
                if attempt >= TRACE_UPLOAD_MAX_RETRIES || !error.is_retriable() {
                    return Err(error);
                }
                retry = true;
            }
        }
        if !retry {
            return Err(TraceUploadError::Message("Trace upload failed".to_string()));
        }

        let wait = retry_delay_ms.unwrap_or_else(|| trace_upload_retry_delay(attempt));
        delay(wait, signal).await;
        if signal.map(CancellationToken::is_cancelled).unwrap_or(false) {
            return Err(TraceUploadError::Cancelled);
        }
        attempt += 1;
    }
}

fn trace_content_preview(body: &str, max_chars: usize) -> (String, bool) {
    if body.chars().count() <= max_chars {
        return (body.trim_end().to_string(), false);
    }
    let marker = "\n... middle of trace omitted ...\n";
    let available = max_chars.saturating_sub(marker.chars().count());
    let head_chars = available.div_ceil(2);
    let tail_chars = available / 2;
    let head: String = body.chars().take(head_chars).collect();
    let total = body.chars().count();
    let tail: String = body.chars().skip(total - tail_chars).collect();
    (
        format!("{}{}{}", head.trim_end(), marker, tail.trim_start()),
        true,
    )
}

/// `previewAgentTraceFile(options)`.
pub async fn preview_agent_trace_file(options: &AgentTracePreviewOptions) -> AgentTracePreviewResult {
    let Some(session_file) = options.session_file.clone() else {
        return AgentTracePreviewResult::NoSessionFile;
    };

    let metadata = match std::fs::metadata(&session_file) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return AgentTracePreviewResult::NoSessionFile,
    };
    let file_size = metadata.len();
    if file_size == 0 {
        return AgentTracePreviewResult::EmptySession;
    }

    let Some(header) = read_session_header(&session_file) else {
        return AgentTracePreviewResult::InvalidSession {
            message: "Session file is missing a valid session header".to_string(),
        };
    };

    let mut body = String::new();
    if file_size <= MAX_TRACE_BYTES {
        match std::fs::read_to_string(&session_file) {
            Ok(content) => body = content,
            Err(error) => {
                return AgentTracePreviewResult::Failed {
                    message: describe_error(&error.to_string()),
                }
            }
        }
        if body.trim().is_empty() {
            return AgentTracePreviewResult::EmptySession;
        }
    }

    let (trace_id, parent_session_id) = resolve_trace_context(&session_file, &header);
    let base_url = resolve_prime_agent_traces_base_url(options.base_url.as_deref());
    let git = if body.is_empty() {
        header.git.clone()
    } else {
        active_git_context(&body, &header)
    };
    let (content_preview, truncated) = if body.is_empty() {
        (String::new(), true)
    } else {
        trace_content_preview(&body, options.max_content_chars.unwrap_or(TRACE_PREVIEW_MAX_CHARS).max(256))
    };
    AgentTracePreviewResult::Ready {
        session_file,
        session_id: header.id.clone(),
        trace_id,
        parent_session_id,
        cwd: header.cwd.clone(),
        size: file_size,
        max_bytes: MAX_TRACE_BYTES,
        uploadable: file_size <= MAX_TRACE_BYTES,
        endpoint: format!(
            "{}/api/v1/agent-traces/sessions/{}",
            base_url,
            url_encode(&header.id)
        ),
        git_repo: git
            .as_ref()
            .and_then(|git| git.get("repoUrl"))
            .and_then(Value::as_str)
            .map(str::to_string),
        git_commit: git
            .as_ref()
            .and_then(|git| git.get("commit"))
            .and_then(Value::as_str)
            .map(str::to_string),
        content_preview,
        truncated,
    }
}

/// `encodeURIComponent`.
fn url_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn find_session_files_under(root: &Path, files: &mut HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let entry_path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            find_session_files_under(&entry_path, files);
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if file_type.is_file() && name.ends_with(".jsonl") {
            let path_string = entry_path.to_string_lossy().to_string();
            if read_session_header(&path_string).is_some() {
                files.insert(path_string);
            }
        }
    }
}

/// `getSessionsDir()` and `getSessionArtifactsRoot(sessionDir)` from config.ts /
/// session-manager.ts (other slices).
fn get_sessions_dir() -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".prime")
        .join("agent")
        .join("sessions")
        .to_string_lossy()
        .to_string()
}

fn get_session_artifacts_root(session_dir: &str) -> String {
    let parent = Path::new(session_dir).parent().unwrap_or_else(|| Path::new("."));
    parent.join("session-artifacts").to_string_lossy().to_string()
}

/// `findAgentTraceFiles(sessionDir)`.
pub async fn find_agent_trace_files(session_dir: Option<&str>) -> Vec<String> {
    let session_dir = session_dir
        .map(str::to_string)
        .unwrap_or_else(get_sessions_dir);
    let mut roots: Vec<String> = vec![absolute(&session_dir)];
    roots.push(absolute(&get_session_artifacts_root(&session_dir)));
    roots.dedup();
    let mut files: HashSet<String> = HashSet::new();
    for root in roots {
        find_session_files_under(Path::new(&root), &mut files);
    }
    let mut sorted: Vec<String> = files.into_iter().collect();
    sorted.sort();
    sorted
}

fn absolute(value: &str) -> String {
    let path = Path::new(value);
    if path.is_absolute() {
        value.to_string()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path).to_string_lossy().to_string())
            .unwrap_or_else(|_| value.to_string())
    }
}

fn create_trace_upload_all_request_gate(
    signal: Option<CancellationToken>,
) -> BeforeTraceUploadRequest {
    /// `let nextRequestAt = 0; let queue = Promise.resolve();` - the queue is a
    /// tokio mutex so each request slot runs after the previous one.
    struct Gate {
        next_request_at: Mutex<i64>,
        queue: tokio::sync::Mutex<()>,
    }

    let gate = Arc::new(Gate {
        next_request_at: Mutex::new(0),
        queue: tokio::sync::Mutex::new(()),
    });
    Arc::new(move || {
        let gate = gate.clone();
        let signal = signal.clone();
        Box::pin(async move {
            let _slot = gate.queue.lock().await;
            let wait_ms = {
                let guard = gate.next_request_at.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                (*guard - now_millis()).max(0) as u64
            };
            if wait_ms > 0 {
                delay(wait_ms, signal.as_ref()).await;
            }
            if !signal
                .as_ref()
                .map(CancellationToken::is_cancelled)
                .unwrap_or(false)
            {
                let mut guard = gate.next_request_at.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                *guard = now_millis() + TRACE_UPLOAD_ALL_MIN_REQUEST_INTERVAL_MS as i64;
            }
        }) as BoxFuture<()>
    })
}

/// `uploadAllAgentTraces(options)`.
pub async fn upload_all_agent_traces(options: &AgentTraceUploadAllOptions) -> AgentTraceUploadAllResult {
    let session_files = find_agent_trace_files(options.session_dir.as_deref()).await;
    let mut results: Vec<Option<(String, AgentTraceUploadResult)>> = vec![None; session_files.len()];
    let mut cursor = 0usize;
    let mut completed = 0usize;
    let before_request = create_trace_upload_all_request_gate(options.upload.signal.clone());
    if let Some(on_progress) = &options.on_progress {
        on_progress(AgentTraceUploadAllProgress {
            completed,
            total: session_files.len(),
            session_file: None,
            result: None,
        });
    }

    let requested_concurrency = options.concurrency.unwrap_or(TRACE_UPLOAD_ALL_CONCURRENCY);
    let normalized_concurrency = if requested_concurrency > 0 {
        requested_concurrency.max(1)
    } else {
        TRACE_UPLOAD_ALL_CONCURRENCY
    };
    let worker_count = session_files.len().min(normalized_concurrency);

    let results = Arc::new(tokio::sync::Mutex::new(results));
    let cursor_cell = Arc::new(tokio::sync::Mutex::new(cursor));
    let completed_cell = Arc::new(tokio::sync::Mutex::new(completed));
    let mut handles = Vec::new();
    for _ in 0..worker_count {
        let session_files = session_files.clone();
        let results = results.clone();
        let cursor_cell = cursor_cell.clone();
        let completed_cell = completed_cell.clone();
        let upload_options = options.upload.clone();
        let before_request = before_request.clone();
        let on_progress = options.on_progress.clone();
        handles.push(tokio::spawn(async move {
            loop {
                if upload_options
                    .signal
                    .as_ref()
                    .map(CancellationToken::is_cancelled)
                    .unwrap_or(false)
                {
                    return;
                }
                let index = {
                    let mut cursor = cursor_cell.lock().await;
                    let index = *cursor;
                    *cursor += 1;
                    index
                };
                let Some(session_file) = session_files.get(index).cloned() else {
                    return;
                };
                let mut per_file_options = upload_options.clone();
                per_file_options.session_file = Some(session_file.clone());
                per_file_options.reload_config = false;
                let result = upload_agent_trace_file_with_request_gate(
                    per_file_options,
                    Some(before_request.clone()),
                )
                .await;
                if upload_options
                    .signal
                    .as_ref()
                    .map(CancellationToken::is_cancelled)
                    .unwrap_or(false)
                    && matches!(result, AgentTraceUploadResult::Failed { .. })
                {
                    return;
                }
                {
                    let mut results = results.lock().await;
                    results[index] = Some((session_file.clone(), result.clone()));
                }
                let completed = {
                    let mut completed = completed_cell.lock().await;
                    *completed += 1;
                    *completed
                };
                if let Some(on_progress) = &on_progress {
                    on_progress(AgentTraceUploadAllProgress {
                        completed,
                        total: session_files.len(),
                        session_file: Some(session_file),
                        result: Some(result),
                    });
                }
            }
        }));
    }
    for handle in handles {
        let _ = handle.await;
    }

    let results = results.lock().await.clone();
    let completed_results: Vec<(String, AgentTraceUploadResult)> =
        results.into_iter().flatten().collect();
    let mut uploaded = 0usize;
    let mut failed = 0usize;
    let mut bytes_stored = 0u64;
    for (_, result) in &completed_results {
        match result {
            AgentTraceUploadResult::Uploaded { bytes_stored: bytes, .. } => {
                uploaded += 1;
                bytes_stored += bytes;
            }
            AgentTraceUploadResult::Failed { .. } => failed += 1,
            _ => {}
        }
    }
    AgentTraceUploadAllResult {
        total: session_files.len(),
        uploaded,
        failed,
        skipped: session_files.len().saturating_sub(uploaded + failed),
        bytes_stored,
        results: completed_results,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AgentTraceUploadedSignature {
    size: u64,
    mtime_ms: f64,
}

pub const SEMANTIC_EDGES_OUTBOX_KIND: &str = "semantic-edges";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentTraceCatchUpResult {
    pub pruned: usize,
    /// Registered semantic-edge ledgers with bytes beyond their cursor; no
    /// delivery endpoint exists yet.
    pub semantic_edge_ledgers_pending: usize,
    pub results: Vec<(String, AgentTraceUploadResult)>,
}

/// `getAgentDir()` from config.ts (other slice).
fn get_agent_dir() -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".prime")
        .join("agent")
        .to_string_lossy()
        .to_string()
}

fn get_agent_trace_outbox_dir() -> String {
    Path::new(&get_agent_dir())
        .join("agent-traces-outbox")
        .to_string_lossy()
        .to_string()
}

/// One entry file per session file (keyed by path hash): concurrent writers
/// cannot lose each other's cursors, and a bad read costs only its own entry.
fn agent_trace_outbox_entry_path(session_file: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(session_file.as_bytes());
    let key: String = format!("{:x}", hasher.finalize()).chars().take(32).collect();
    Path::new(&get_agent_trace_outbox_dir())
        .join(format!("{}.json", key))
        .to_string_lossy()
        .to_string()
}

#[derive(Debug, Clone, Default)]
struct OutboxEntry {
    session_file: String,
    kind: Option<String>,
    uploaded: Option<AgentTraceUploadedSignature>,
    uploaded_bytes: Option<u64>,
}

fn parse_outbox_entry(raw: &str) -> Option<OutboxEntry> {
    let parsed = parse_response_object(raw)?;
    let session_file = parsed.get("sessionFile").and_then(Value::as_str)?.to_string();
    let uploaded = match (parsed.get("size").and_then(Value::as_f64), parsed.get("mtimeMs").and_then(Value::as_f64))
    {
        (Some(size), Some(mtime_ms)) => Some(AgentTraceUploadedSignature { size: size as u64, mtime_ms }),
        _ => None,
    };
    Some(OutboxEntry {
        session_file,
        kind: parsed.get("kind").and_then(Value::as_str).map(str::to_string),
        uploaded,
        uploaded_bytes: parsed.get("uploadedBytes").and_then(Value::as_u64),
    })
}

/// `undefined` = no usable cursor; `null` = scheduled but never uploaded.
async fn read_agent_trace_outbox_entry(
    session_file: &str,
) -> Option<Option<AgentTraceUploadedSignature>> {
    let raw = std::fs::read_to_string(agent_trace_outbox_entry_path(session_file)).ok()?;
    let entry = parse_outbox_entry(&raw)?;
    if entry.session_file != session_file {
        return None;
    }
    Some(entry.uploaded)
}

fn signature_equals(
    a: Option<&AgentTraceUploadedSignature>,
    b: &AgentTraceUploadedSignature,
) -> bool {
    match a {
        Some(a) => a.size == b.size && a.mtime_ms == b.mtime_ms,
        None => false,
    }
}

/// Session files with a live upload controller in this process; catch-up leaves
/// them to their controller.
fn locally_managed_session_files() -> &'static Mutex<HashSet<String>> {
    static FILES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    FILES.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Best-effort and synchronous: upload intent must be on disk the moment the
/// transcript persist returns.
fn mark_agent_trace_outbox_pending_sync(session_file: &str, kind: Option<&str>) -> bool {
    let entry_path = agent_trace_outbox_entry_path(session_file);
    if Path::new(&entry_path).exists() {
        return true;
    }
    let dir = get_agent_trace_outbox_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        // A broken agent dir must not break session persists.
        return false;
    }
    let temp_path = format!("{}.{}.{}.tmp", entry_path, std::process::id(), uuid::Uuid::new_v4());
    let payload = match kind {
        None => serde_json::json!({ "sessionFile": session_file }),
        Some(kind) => serde_json::json!({ "sessionFile": session_file, "kind": kind }),
    };
    let result = (|| -> std::io::Result<()> {
        std::fs::write(&temp_path, format!("{}\n", payload))?;
        std::fs::rename(&temp_path, &entry_path)
    })();
    match result {
        Ok(()) => true,
        Err(_) => {
            let _ = std::fs::remove_file(&temp_path);
            false
        }
    }
}

async fn record_agent_trace_outbox_upload(
    session_file: &str,
    signature: AgentTraceUploadedSignature,
) -> std::io::Result<()> {
    let entry_path = agent_trace_outbox_entry_path(session_file);
    let dir = get_agent_trace_outbox_dir();
    std::fs::create_dir_all(&dir)?;
    let temp_path = format!("{}.{}.{}.tmp", entry_path, std::process::id(), uuid::Uuid::new_v4());
    let payload = serde_json::json!({
        "sessionFile": session_file,
        "size": signature.size,
        "mtimeMs": signature.mtime_ms,
    });
    let result = (|| -> std::io::Result<()> {
        std::fs::write(&temp_path, format!("{}\n", payload))?;
        std::fs::rename(&temp_path, &entry_path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// Startup catch-up: upload every outbox entry whose file content is ahead of
/// its cursor, and prune entries whose file no longer exists. Runs once per
/// process, in whichever process hosts sessions.
pub async fn catch_up_agent_trace_uploads(
    options: AgentTraceUploadOptions,
) -> AgentTraceCatchUpResult {
    let mut catch_up = AgentTraceCatchUpResult::default();
    if options.require_enabled && !get_agent_traces_enabled(&options).await {
        return catch_up;
    }
    let Ok(entries) = std::fs::read_dir(get_agent_trace_outbox_dir()) else {
        return catch_up;
    };
    let before_request = create_trace_upload_all_request_gate(options.signal.clone());
    for entry in entries.flatten() {
        if options
            .signal
            .as_ref()
            .map(CancellationToken::is_cancelled)
            .unwrap_or(false)
        {
            break;
        }
        let entry_name = entry.file_name().to_string_lossy().to_string();
        if !entry_name.ends_with(".json") {
            continue;
        }
        let entry_path = Path::new(&get_agent_trace_outbox_dir())
            .join(&entry_name)
            .to_string_lossy()
            .to_string();
        let Ok(raw) = std::fs::read_to_string(&entry_path) else {
            // Transient read error: keep the entry and retry at the next startup.
            continue;
        };
        let Some(parsed) = parse_outbox_entry(&raw) else {
            let _ = std::fs::remove_file(&entry_path);
            catch_up.pruned += 1;
            continue;
        };
        if parsed.kind.as_deref() == Some(SEMANTIC_EDGES_OUTBOX_KIND) {
            let Ok(metadata) = std::fs::metadata(&parsed.session_file) else {
                if std::fs::metadata(&parsed.session_file).is_err() {
                    let _ = std::fs::remove_file(&entry_path);
                    catch_up.pruned += 1;
                }
                continue;
            };
            if !metadata.is_file() {
                let _ = std::fs::remove_file(&entry_path);
                catch_up.pruned += 1;
                continue;
            }
            // Append-only byte cursor: a ledger whose size equals its delivered offset has nothing new.
            if Some(metadata.len()) == parsed.uploaded_bytes {
                continue;
            }
            // No delivery endpoint exists yet (verifiers#2449 consumes edges in-band over ACP
            // metadata; the trace server has no semantic-edges route). The delta and cursor stay
            // untouched so the first real sender delivers the whole backlog.
            catch_up.semantic_edge_ledgers_pending += 1;
            continue;
        }
        if parsed.kind.is_some() {
            // A newer build may register kinds this one cannot deliver; leave their cursors alone.
            continue;
        }
        if locally_managed_session_files()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&parsed.session_file)
        {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&parsed.session_file) else {
            let _ = std::fs::remove_file(&entry_path);
            catch_up.pruned += 1;
            continue;
        };
        if !metadata.is_file() {
            let _ = std::fs::remove_file(&entry_path);
            catch_up.pruned += 1;
            continue;
        }
        let signature = AgentTraceUploadedSignature {
            size: metadata.len(),
            mtime_ms: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as f64)
                .unwrap_or(0.0),
        };
        if signature_equals(parsed.uploaded.as_ref(), &signature) {
            continue;
        }
        let mut per_file_options = options.clone();
        per_file_options.session_file = Some(parsed.session_file.clone());
        per_file_options.reload_config = false;
        let result =
            upload_agent_trace_file_with_request_gate(per_file_options, Some(before_request.clone())).await;
        catch_up.results.push((parsed.session_file, result));
    }
    catch_up
}

/// `getPrimeAgentTraceCredential(authStorage, options)`.
pub async fn get_prime_agent_trace_credential(
    auth_storage: &mut AuthStorage,
    reload_auth: bool,
    config_path: Option<&str>,
) -> Option<AgentTraceCredential> {
    if let Some(trace_env_key) = string_env("PRIME_AGENT_TRACES_API_KEY") {
        return Some(AgentTraceCredential {
            api_key: trace_env_key,
            source: "environment",
            label: "PRIME_AGENT_TRACES_API_KEY".to_string(),
        });
    }

    if reload_auth {
        auth_storage.reload();
    }

    if let Ok(Some(trace_key)) = auth_storage.get_api_key(PRIME_AGENT_TRACES_PROVIDER_ID, false).await {
        return Some(AgentTraceCredential {
            api_key: trace_key,
            source: "stored",
            label: "Prime Agent Traces credential".to_string(),
        });
    }

    if let Some(prime_env_key) = string_env("PRIME_API_KEY") {
        return Some(AgentTraceCredential {
            api_key: prime_env_key,
            source: "environment",
            label: "PRIME_API_KEY".to_string(),
        });
    }

    if auth_storage.get(PRIME_INFERENCE_PROVIDER_ID).is_some() {
        if let Ok(Some(prime_key)) = auth_storage.get_api_key(PRIME_INFERENCE_PROVIDER_ID, false).await {
            return Some(AgentTraceCredential {
                api_key: prime_key,
                source: "prime-inference",
                label: "Prime Inference credential".to_string(),
            });
        }
    }

    if let Some(prime_cli_key) = load_prime_cli_config(config_path).api_key {
        return Some(AgentTraceCredential {
            api_key: prime_cli_key,
            source: "prime-cli",
            label: "Prime CLI credential".to_string(),
        });
    }

    None
}

async fn get_agent_traces_enabled(options: &AgentTraceUploadOptions) -> bool {
    if options.reload_config {
        let _ = (options.reload_settings)().await;
    }
    (options.agent_traces_enabled)()
}

async fn upload_agent_trace_file_with_request_gate(
    options: AgentTraceUploadOptions,
    before_request: Option<BeforeTraceUploadRequest>,
) -> AgentTraceUploadResult {
    let result = perform_agent_trace_upload(&options, before_request.as_ref()).await;
    log_agent_trace_outcome(options.session_file.as_deref(), &result);
    result
}

pub async fn upload_agent_trace_file(
    options: AgentTraceUploadOptions,
) -> AgentTraceUploadResult {
    upload_agent_trace_file_with_request_gate(options, None).await
}

fn log_agent_trace_outcome(session_file: Option<&str>, result: &AgentTraceUploadResult) {
    let line = match result {
        AgentTraceUploadResult::Uploaded {
            session_id,
            bytes_stored,
            ..
        } => Some(format!("uploaded session {} ({} bytes)", session_id, bytes_stored)),
        AgentTraceUploadResult::Failed {
            status_code,
            message,
            ..
        } => Some(format!(
            "upload failed{}: {}",
            status_code
                .map(|status| format!(" (HTTP {})", status))
                .unwrap_or_default(),
            message
        )),
        AgentTraceUploadResult::TooLarge { size, max_bytes } => Some(format!(
            "upload skipped: session is {} bytes (limit {})",
            size, max_bytes
        )),
        AgentTraceUploadResult::InvalidSession { message } => {
            Some(format!("upload skipped: {}", message))
        }
        AgentTraceUploadResult::MissingCredentials => Some(
            "upload skipped: no Prime credential configured (run /traces login)".to_string(),
        ),
        _ => None,
    };
    let Some(line) = line else {
        return;
    };
    let suffix = session_file
        .map(|session_file| format!(" [{}]", session_file))
        .unwrap_or_default();
    append_rotating_log(
        &get_agent_traces_log_path(),
        &format!("[{}] {}{}", iso_now(), line, suffix),
    );
}

/// `appendRotatingLog(getAgentTracesLogPath(), message)` from config.ts.
fn append_rotating_log(log_path: &str, message: &str) {
    const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
    let path = Path::new(log_path);
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    if let Ok(metadata) = std::fs::metadata(log_path) {
        if metadata.len() > MAX_LOG_BYTES {
            let old = format!("{}.old", log_path);
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(log_path, &old);
        }
    }
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        let _ = file.write_all(format!("{}\n", message).as_bytes());
    }
}

fn get_agent_traces_log_path() -> String {
    Path::new(&get_agent_dir())
        .join("logs")
        .join("agent-traces.log")
        .to_string_lossy()
        .to_string()
}

fn iso_now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

async fn perform_agent_trace_upload(
    options: &AgentTraceUploadOptions,
    before_request: Option<&BeforeTraceUploadRequest>,
) -> AgentTraceUploadResult {
    let require_enabled = options.require_enabled;
    if require_enabled && !get_agent_traces_enabled(options).await {
        return AgentTraceUploadResult::Disabled;
    }
    let Some(session_file) = options.session_file.clone() else {
        return AgentTraceUploadResult::NoSessionFile;
    };

    let signature = match std::fs::metadata(&session_file) {
        Ok(metadata) if metadata.is_file() => AgentTraceUploadedSignature {
            size: metadata.len(),
            mtime_ms: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as f64)
                .unwrap_or(0.0),
        },
        _ => return AgentTraceUploadResult::NoSessionFile,
    };
    let file_size = signature.size;
    if file_size == 0 {
        return AgentTraceUploadResult::EmptySession;
    }
    if file_size > MAX_TRACE_BYTES {
        return AgentTraceUploadResult::TooLarge {
            size: file_size,
            max_bytes: MAX_TRACE_BYTES,
        };
    }
    // Cursor invariant: an automatic upload never re-sends a file whose content
    // already matches its uploaded cursor.
    if require_enabled {
        let cursor = read_agent_trace_outbox_entry(&session_file).await;
        if signature_equals(cursor.flatten().as_ref(), &signature) {
            return AgentTraceUploadResult::Unchanged;
        }
    }

    let Some(header) = read_session_header(&session_file) else {
        return AgentTraceUploadResult::InvalidSession {
            message: "Session file is missing a valid session header".to_string(),
        };
    };

    let credential = {
        let mut auth_storage = options.auth_storage.lock().await;
        get_prime_agent_trace_credential(
            &mut auth_storage,
            options.reload_config,
            options.config_path.as_deref(),
        )
        .await
    };
    let Some(credential) = credential else {
        return AgentTraceUploadResult::MissingCredentials;
    };

    if require_enabled && !get_agent_traces_enabled(options).await {
        return AgentTraceUploadResult::Disabled;
    }

    let body = match std::fs::read_to_string(&session_file) {
        Ok(body) => body,
        Err(error) => {
            return AgentTraceUploadResult::Failed {
                status_code: None,
                message: describe_error(&error.to_string()),
                retry_after_ms: None,
            }
        }
    };
    if body.trim().is_empty() {
        return AgentTraceUploadResult::EmptySession;
    }

    let (trace_id, parent_session_id) = resolve_trace_context(&session_file, &header);
    let body_bytes = body.len() as u64;
    let mut headers: Vec<(String, String)> = vec![
        (
            "Authorization".to_string(),
            format!("Bearer {}", credential.api_key),
        ),
        ("Content-Type".to_string(), "application/x-ndjson".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("X-Trace-Id".to_string(), trace_id.clone()),
        ("X-Cwd".to_string(), header.cwd.clone()),
        ("X-Agent-Version".to_string(), VERSION.to_string()),
    ];
    if let Some(parent_session_id) = &parent_session_id {
        headers.push(("X-Parent-Session".to_string(), parent_session_id.clone()));
    }
    let git = active_git_context(&body, &header);
    if let Some(repo_url) = git
        .as_ref()
        .and_then(|git| git.get("repoUrl"))
        .and_then(Value::as_str)
    {
        headers.push(("X-Git-Repo".to_string(), repo_url.to_string()));
    }
    if let Some(commit) = git
        .as_ref()
        .and_then(|git| git.get("commit"))
        .and_then(Value::as_str)
    {
        headers.push(("X-Git-Commit".to_string(), commit.to_string()));
    }

    if require_enabled && !get_agent_traces_enabled(options).await {
        return AgentTraceUploadResult::Disabled;
    }

    let base_url = resolve_prime_agent_traces_base_url(options.base_url.as_deref());
    let url = format!(
        "{}/api/v1/agent-traces/sessions/{}",
        base_url,
        url_encode(&header.id)
    );
    let fetch_fn = options.fetch_fn.clone().unwrap_or_else(crate::core::prime_inference_auth::default_fetch);

    let response = match fetch_with_retry(
        &fetch_fn,
        &url,
        HttpRequest {
            method: "PUT".to_string(),
            url: url.clone(),
            headers,
            body: Some(body.clone()),
            timeout_ms: options.request_timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
        },
        options.request_timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
        options.signal.as_ref(),
        before_request,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            return AgentTraceUploadResult::Failed {
                status_code: None,
                message: describe_error(&error.message()),
                retry_after_ms: None,
            }
        }
    };

    if !response.ok() {
        return AgentTraceUploadResult::Failed {
            status_code: Some(response.status),
            message: read_response_message(&response),
            retry_after_ms: retry_after_delay(&response, MAX_TIMER_DELAY_MS),
        };
    }

    let response_data = parse_response_object(&response.text);
    if let Err(error) = record_agent_trace_outbox_upload(&session_file, signature).await {
        return AgentTraceUploadResult::Failed {
            status_code: None,
            message: format!(
                "stored, but recording the upload cursor failed: {}",
                describe_error(&error.to_string())
            ),
            retry_after_ms: None,
        };
    }
    AgentTraceUploadResult::Uploaded {
        session_id: response_data
            .as_ref()
            .and_then(|data| string_field(data, "session_id"))
            .unwrap_or_else(|| header.id.clone()),
        trace_id: response_data
            .as_ref()
            .and_then(|data| string_field(data, "trace_id"))
            .unwrap_or_else(|| trace_id.clone()),
        bytes_stored: response_data
            .as_ref()
            .and_then(|data| number_field(data, "bytes_stored"))
            .map(|value| value as u64)
            .unwrap_or(body_bytes),
        key: response_data
            .as_ref()
            .and_then(|data| string_field(data, "key")),
    }
}

/// `uploadAgentTraceSession(options)`.
pub async fn upload_agent_trace_session(options: AgentTraceSessionUploadOptions) -> AgentTraceUploadResult {
    let mut upload = options.upload;
    upload.session_file = (options.get_session_file)();
    upload_agent_trace_file(upload).await
}

/// `class AgentTraceUploadController` - a debounce/throttle controller driven by
/// `schedule()`. The TypeScript `setTimeout(...).unref()` maps to a tokio task.
pub struct AgentTraceUploadController {
    options: Mutex<AgentTraceUploadInstallOptions>,
    pending: Mutex<bool>,
    in_flight: tokio::sync::Mutex<()>,
    last_upload_started_at: Mutex<Option<i64>>,
    not_before_at: Mutex<i64>,
    timeout: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl AgentTraceUploadController {
    pub fn new(options: AgentTraceUploadInstallOptions) -> Arc<Self> {
        Arc::new(Self {
            options: Mutex::new(options),
            pending: Mutex::new(false),
            in_flight: tokio::sync::Mutex::new(()),
            last_upload_started_at: Mutex::new(None),
            not_before_at: Mutex::new(0),
            timeout: Mutex::new(None),
        })
    }

    pub fn update(&self, options: AgentTraceUploadInstallOptions) {
        *self.options.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = options;
    }

    pub fn schedule(self: &Arc<Self>) {
        *self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        // Intent is consent-gated at persist time: an entry created while sharing
        // is off would turn a later enable into retroactive collection of
        // opted-out sessions. Marking re-runs every persist (existsSync-cheap),
        // so an entry pruned by a racing catch-up is re-registered.
        {
            let options = self.options.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if (options.agent_traces_enabled)() {
                if let Some(session_file) = (options.get_session_file)() {
                    let already_managed = locally_managed_session_files()
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .contains(&session_file);
                    if !already_managed && mark_agent_trace_outbox_pending_sync(&session_file, None) {
                        locally_managed_session_files()
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .insert(session_file);
                    }
                }
                if let Some(ledger_path) = options.semantic_edges_ledger_path.clone() {
                    mark_agent_trace_outbox_pending_sync(&ledger_path, Some(SEMANTIC_EDGES_OUTBOX_KIND));
                }
            }
        }
        self.arm();
    }

    fn arm(self: &Arc<Self>) {
        {
            let mut timeout = self.timeout.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(handle) = timeout.take() {
                handle.abort();
            }
        }
        let elapsed = {
            let last = self
                .last_upload_started_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            last.map(|last| now_millis() - last)
        };
        let throttle_delay = match elapsed {
            None => 0,
            Some(elapsed) => (TRACE_UPLOAD_MIN_INTERVAL_MS as i64 - elapsed).max(0) as u64,
        };
        let not_before_delay = {
            let not_before_at = self
                .not_before_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (*not_before_at - now_millis()).max(0) as u64
        };
        let delay_ms = TRACE_UPLOAD_DEBOUNCE_MS.max(throttle_delay).max(not_before_delay);
        let controller = self.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            {
                let mut timeout = controller
                    .timeout
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                *timeout = None;
            }
            controller.run_scheduled_upload().await;
        });
        let mut timeout = self.timeout.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *timeout = Some(handle);
    }

    async fn run_scheduled_upload(self: &Arc<Self>) {
        let Ok(_guard) = self.in_flight.try_lock() else {
            return;
        };
        *self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = false;
        {
            let mut last = self
                .last_upload_started_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *last = Some(now_millis());
        }
        let options = {
            let options = self.options.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            options.clone()
        };
        let result = upload_agent_trace_session(AgentTraceSessionUploadOptions {
            upload: AgentTraceUploadOptions {
                session_file: None,
                auth_storage: options.auth_storage.clone(),
                require_enabled: true,
                base_url: options.base_url.clone(),
                config_path: options.config_path.clone(),
                fetch_fn: options.fetch_fn.clone(),
                reload_config: true,
                request_timeout_ms: options.request_timeout_ms,
                signal: None,
                agent_traces_enabled: options.agent_traces_enabled.clone(),
                reload_settings: options.reload_settings.clone(),
            },
            get_session_file: options.get_session_file.clone(),
        })
        .await;
        if let AgentTraceUploadResult::Failed {
            status_code,
            retry_after_ms,
            ..
        } = &result
        {
            if is_rescheduled_upload_failure(*status_code) {
                *self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
                if let Some(retry_after_ms) = retry_after_ms {
                    let mut not_before_at = self
                        .not_before_at
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    *not_before_at = now_millis() + *retry_after_ms as i64;
                }
            }
        }
        let pending = *self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending {
            self.arm();
        }
    }
}

fn is_rescheduled_upload_failure(status_code: Option<u16>) -> bool {
    match status_code {
        None => true,
        Some(status) => status == 429 || is_retriable_http_status(status),
    }
}

/// `const traceUploadControllers = new WeakMap<SessionManager, AgentTraceUploadController>()`
/// keyed by the session manager's persist-listener registry. The session slice
/// owns `SessionManager`, so the port exposes the install function and lets the
/// caller keep the returned controller per session manager.
pub fn install_agent_trace_upload(
    options: AgentTraceUploadInstallOptions,
) -> Arc<AgentTraceUploadController> {
    let controller = AgentTraceUploadController::new(options.clone());
    static CATCH_UP_TRIGGERED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    if !CATCH_UP_TRIGGERED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        let catch_up_options = AgentTraceUploadOptions {
            session_file: None,
            auth_storage: options.auth_storage.clone(),
            require_enabled: true,
            base_url: options.base_url.clone(),
            config_path: options.config_path.clone(),
            fetch_fn: options.fetch_fn.clone(),
            reload_config: true,
            request_timeout_ms: options.request_timeout_ms,
            signal: None,
            agent_traces_enabled: options.agent_traces_enabled.clone(),
            reload_settings: options.reload_settings.clone(),
        };
        tokio::spawn(async move {
            let _ = catch_up_agent_trace_uploads(catch_up_options).await;
        });
    }
    controller
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::prime_inference_auth::HttpResponse;
    use serde_json::json;

    fn write_session(dir: &Path, name: &str, lines: &[Value]) -> String {
        let path = dir.join(name);
        let body: String = lines
            .iter()
            .map(|line| format!("{}\n", line))
            .collect::<Vec<_>>()
            .join("");
        std::fs::write(&path, body).unwrap();
        path.to_string_lossy().to_string()
    }

    fn session_header(id: &str, cwd: &str) -> Value {
        json!({"type": "session", "id": id, "timestamp": "2024-01-01T00:00:00.000Z", "cwd": cwd})
    }

    fn test_options(session_file: Option<String>, fetch_fn: Option<FetchFn>) -> AgentTraceUploadOptions {
        AgentTraceUploadOptions {
            session_file,
            auth_storage: Arc::new(tokio::sync::Mutex::new(AuthStorage::in_memory(
                IndexMap::new(),
                Some(crate::core::auth_storage::AuthStorageOptions {
                    prime_cli_config_path: None,
                    use_prime_cli_config: false,
                }),
            ))),
            require_enabled: false,
            base_url: Some("https://traces.test".to_string()),
            config_path: None,
            fetch_fn,
            reload_config: false,
            request_timeout_ms: Some(1_000),
            signal: None,
            agent_traces_enabled: Arc::new(|| true),
            reload_settings: Arc::new(|| Box::pin(async { Ok(()) })),
        }
    }

    #[test]
    fn active_git_context_walks_the_active_branch() {
        let header = SessionHeader {
            id: "s1".to_string(),
            timestamp: "t".to_string(),
            cwd: "c".to_string(),
            parent_session: None,
            rlm_depth: None,
            git: Some(serde_json::from_value(json!({"repoUrl": "header"})).unwrap()),
        };
        let body = [
            json!({"type": "message", "id": "a", "parentId": null}),
            json!({"type": "git_state", "id": "b", "parentId": "a", "git": {"repoUrl": "root"}}),
            json!({"type": "git_state", "id": "c", "parentId": "b", "git": {"repoUrl": "active", "commit": "abc"}}),
        ]
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        let git = active_git_context(&body, &header).unwrap();
        assert_eq!(git.get("repoUrl").and_then(Value::as_str), Some("active"));
        assert_eq!(git.get("commit").and_then(Value::as_str), Some("abc"));
    }

    #[test]
    fn active_git_context_falls_back_to_the_header() {
        let header = SessionHeader {
            id: "s1".to_string(),
            timestamp: "t".to_string(),
            cwd: "c".to_string(),
            parent_session: None,
            rlm_depth: None,
            git: Some(serde_json::from_value(json!({"repoUrl": "header"})).unwrap()),
        };
        let body = json!({"type": "message", "id": "a", "parentId": null}).to_string();
        let git = active_git_context(&body, &header).unwrap();
        assert_eq!(git.get("repoUrl").and_then(Value::as_str), Some("header"));
    }

    #[test]
    fn trace_context_follows_parent_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let parent = write_session(dir.path(), "parent.jsonl", &[session_header("parent", "cwd")]);
        let child = write_session(
            dir.path(),
            "child.jsonl",
            &[
                json!({"type": "session", "id": "child", "timestamp": "t", "cwd": "cwd", "parentSession": "parent.jsonl"}),
            ],
        );
        let header = read_session_header(&child).unwrap();
        let (trace_id, parent_session_id) = resolve_trace_context(&child, &header);
        assert_eq!(trace_id, "parent");
        assert_eq!(parent_session_id.as_deref(), Some("parent"));
        assert!(Path::new(&parent).exists());
    }

    #[tokio::test]
    async fn preview_reports_missing_and_empty_sessions() {
        assert_eq!(
            preview_agent_trace_file(&AgentTracePreviewOptions::default()).await,
            AgentTracePreviewResult::NoSessionFile
        );
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty.jsonl");
        std::fs::write(&empty, "").unwrap();
        assert_eq!(
            preview_agent_trace_file(&AgentTracePreviewOptions {
                session_file: Some(empty.to_string_lossy().to_string()),
                base_url: None,
                max_content_chars: None,
            })
            .await,
            AgentTracePreviewResult::EmptySession
        );
    }

    #[tokio::test]
    async fn preview_reports_invalid_header_and_ready_state() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.jsonl");
        std::fs::write(&bad, "{\"type\":\"message\"}\n").unwrap();
        assert_eq!(
            preview_agent_trace_file(&AgentTracePreviewOptions {
                session_file: Some(bad.to_string_lossy().to_string()),
                base_url: None,
                max_content_chars: None,
            })
            .await,
            AgentTracePreviewResult::InvalidSession {
                message: "Session file is missing a valid session header".to_string()
            }
        );

        let good = write_session(
            dir.path(),
            "good.jsonl",
            &[session_header("s1", "C:/work"), json!({"type": "message", "id": "a", "parentId": null})],
        );
        let result = preview_agent_trace_file(&AgentTracePreviewOptions {
            session_file: Some(good.clone()),
            base_url: Some("https://traces.test".to_string()),
            max_content_chars: None,
        })
        .await;
        match result {
            AgentTracePreviewResult::Ready {
                session_id,
                trace_id,
                endpoint,
                uploadable,
                cwd,
                truncated,
                ..
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(trace_id, "s1");
                assert_eq!(endpoint, "https://traces.test/api/v1/agent-traces/sessions/s1");
                assert!(uploadable);
                assert_eq!(cwd, "C:/work");
                assert!(!truncated);
            }
            other => panic!("unexpected preview: {:?}", other),
        }
    }

    #[test]
    fn content_preview_keeps_head_and_tail() {
        let body = "a".repeat(1000);
        let (content, truncated) = trace_content_preview(&body, 100);
        assert!(truncated);
        assert!(content.contains("... middle of trace omitted ..."));
        let (content, truncated) = trace_content_preview("short", 100);
        assert!(!truncated);
        assert_eq!(content, "short");
    }

    #[test]
    fn retry_after_parses_seconds_and_dates() {
        let response = HttpResponse {
            status: 503,
            status_text: String::new(),
            headers: vec![("retry-after".to_string(), "2".to_string())],
            text: String::new(),
        };
        assert_eq!(retry_after_delay(&response, 60_000), Some(2_000));
        let response = HttpResponse {
            status: 503,
            status_text: String::new(),
            headers: vec![("retry-after".to_string(), "not-a-value".to_string())],
            text: String::new(),
        };
        assert_eq!(retry_after_delay(&response, 60_000), None);
        let response = HttpResponse {
            status: 503,
            status_text: String::new(),
            headers: Vec::new(),
            text: String::new(),
        };
        assert_eq!(retry_after_delay(&response, 60_000), None);
    }

    #[test]
    fn outbox_entries_are_per_session_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("s.jsonl");
        std::fs::write(&session, "{}\n").unwrap();
        let session = session.to_string_lossy().to_string();
        assert!(mark_agent_trace_outbox_pending_sync(&session, None));
        let raw = std::fs::read_to_string(agent_trace_outbox_entry_path(&session)).unwrap();
        let entry = parse_outbox_entry(&raw).unwrap();
        assert_eq!(entry.session_file, session);
        assert!(entry.kind.is_none());
        assert!(entry.uploaded.is_none());

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(record_agent_trace_outbox_upload(
            &session,
            AgentTraceUploadedSignature {
                size: 3,
                mtime_ms: 4.0,
            },
        ))
        .unwrap();
        let cursor = runtime
            .block_on(read_agent_trace_outbox_entry(&session))
            .unwrap()
            .unwrap();
        assert_eq!(cursor.size, 3);
        assert!(signature_equals(
            Some(&cursor),
            &AgentTraceUploadedSignature {
                size: 3,
                mtime_ms: 4.0
            }
        ));
        assert!(!signature_equals(
            Some(&cursor),
            &AgentTraceUploadedSignature {
                size: 4,
                mtime_ms: 4.0
            }
        ));
    }

    #[test]
    fn parse_outbox_entry_rejects_entries_without_a_session_file() {
        assert!(parse_outbox_entry("{}").is_none());
        assert!(parse_outbox_entry("not json").is_none());
        assert!(parse_outbox_entry("{\"sessionFile\":\"a\",\"kind\":\"semantic-edges\"}")
            .unwrap()
            .kind
            .is_some());
    }

    #[tokio::test]
    async fn upload_skips_when_disabled_or_missing_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let session = write_session(dir.path(), "s.jsonl", &[session_header("s1", "cwd")]);

        let mut options = test_options(Some(session.clone()), None);
        options.require_enabled = true;
        options.agent_traces_enabled = Arc::new(|| false);
        assert_eq!(
            upload_agent_trace_file(options).await,
            AgentTraceUploadResult::Disabled
        );

        let options = test_options(Some(session.clone()), None);
        assert_eq!(
            upload_agent_trace_file(options).await,
            AgentTraceUploadResult::MissingCredentials
        );

        let mut options = test_options(Some(session), None);
        options.session_file = None;
        assert_eq!(
            upload_agent_trace_file(options).await,
            AgentTraceUploadResult::NoSessionFile
        );
    }

    #[tokio::test]
    async fn upload_puts_the_transcript_and_records_the_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let session = write_session(
            dir.path(),
            "s.jsonl",
            &[
                session_header("s1", "C:/work"),
                json!({"type": "git_state", "id": "a", "parentId": null, "git": {"repoUrl": "https://git.test/r"}}),
            ],
        );
        let seen: Arc<Mutex<Vec<HttpRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let fetch_fn: FetchFn = Arc::new(move |request| {
            captured.lock().unwrap().push(request);
            Box::pin(async move {
                Ok(HttpResponse {
                    status: 200,
                    status_text: "OK".to_string(),
                    headers: Vec::new(),
                    text: json!({"session_id": "s1", "trace_id": "t1", "bytes_stored": 5}).to_string(),
                })
            }) as BoxFuture<Result<HttpResponse, String>>
        });
        std::env::set_var("PRIME_AGENT_TRACES_API_KEY", "trace-key");
        let mut options = test_options(Some(session.clone()), Some(fetch_fn));
        options.require_enabled = true;
        let result = upload_agent_trace_file(options).await;
        std::env::remove_var("PRIME_AGENT_TRACES_API_KEY");
        match result {
            AgentTraceUploadResult::Uploaded {
                session_id,
                trace_id,
                bytes_stored,
                key,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(trace_id, "t1");
                assert_eq!(bytes_stored, 5);
                assert!(key.is_none());
            }
            other => panic!("unexpected result: {:?}", other),
        }
        let requests = seen.lock().unwrap();
        assert_eq!(requests[0].method, "PUT");
        assert_eq!(requests[0].url, "https://traces.test/api/v1/agent-traces/sessions/s1");
        assert!(requests[0]
            .headers
            .contains(&("X-Git-Repo".to_string(), "https://git.test/r".to_string())));
        assert!(requests[0]
            .headers
            .contains(&("X-Agent-Version".to_string(), VERSION.to_string())));

        // Second automatic upload with the same file is a no-op.
        let mut options = test_options(Some(session), None);
        options.require_enabled = true;
        assert_eq!(
            upload_agent_trace_file(options).await,
            AgentTraceUploadResult::Unchanged
        );
    }

    #[tokio::test]
    async fn upload_reports_http_failure_with_status_and_retry_after() {
        let dir = tempfile::tempdir().unwrap();
        let session = write_session(dir.path(), "s.jsonl", &[session_header("s1", "cwd")]);
        let fetch_fn: FetchFn = Arc::new(|_request| {
            Box::pin(async move {
                Ok(HttpResponse {
                    status: 403,
                    status_text: "Forbidden".to_string(),
                    headers: vec![("retry-after".to_string(), "1".to_string())],
                    text: json!({"detail": "nope"}).to_string(),
                })
            }) as BoxFuture<Result<HttpResponse, String>>
        });
        std::env::set_var("PRIME_AGENT_TRACES_API_KEY", "trace-key");
        let options = test_options(Some(session), Some(fetch_fn));
        let result = upload_agent_trace_file(options).await;
        std::env::remove_var("PRIME_AGENT_TRACES_API_KEY");
        assert_eq!(
            result,
            AgentTraceUploadResult::Failed {
                status_code: Some(403),
                message: "nope".to_string(),
                retry_after_ms: Some(1_000)
            }
        );
    }

    #[tokio::test]
    async fn upload_reports_oversize_and_empty_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let session = write_session(dir.path(), "s.jsonl", &[session_header("s1", "cwd")]);
        std::fs::write(&session, vec![b'x'; (MAX_TRACE_BYTES + 1) as usize]).unwrap();
        std::env::set_var("PRIME_AGENT_TRACES_API_KEY", "trace-key");
        let result = upload_agent_trace_file(test_options(Some(session.clone()), None)).await;
        assert_eq!(
            result,
            AgentTraceUploadResult::TooLarge {
                size: MAX_TRACE_BYTES + 1,
                max_bytes: MAX_TRACE_BYTES
            }
        );
        std::fs::write(&session, "").unwrap();
        let result = upload_agent_trace_file(test_options(Some(session), None)).await;
        std::env::remove_var("PRIME_AGENT_TRACES_API_KEY");
        assert_eq!(result, AgentTraceUploadResult::EmptySession);
    }

    #[tokio::test]
    async fn credential_precedence_matches_typescript() {
        std::env::set_var("PRIME_AGENT_TRACES_API_KEY", "trace-key");
        let mut storage = AuthStorage::in_memory(
            IndexMap::new(),
            Some(crate::core::auth_storage::AuthStorageOptions {
                prime_cli_config_path: None,
                use_prime_cli_config: false,
            }),
        );
        let credential = get_prime_agent_trace_credential(&mut storage, false, None)
            .await
            .unwrap();
        assert_eq!(credential.label, "PRIME_AGENT_TRACES_API_KEY");
        std::env::remove_var("PRIME_AGENT_TRACES_API_KEY");

        std::env::set_var("PRIME_API_KEY", "prime-key");
        let credential = get_prime_agent_trace_credential(&mut storage, false, None)
            .await
            .unwrap();
        assert_eq!(credential.label, "PRIME_API_KEY");
        assert_eq!(credential.source, "environment");
        std::env::remove_var("PRIME_API_KEY");
    }

    #[tokio::test]
    async fn catch_up_prunes_missing_sessions_and_counts_ledgers() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone.jsonl").to_string_lossy().to_string();
        assert!(mark_agent_trace_outbox_pending_sync(&missing, None));
        let ledger = dir.path().join("edges.jsonl");
        std::fs::write(&ledger, "line\n").unwrap();
        assert!(mark_agent_trace_outbox_pending_sync(
            &ledger.to_string_lossy().to_string(),
            Some(SEMANTIC_EDGES_OUTBOX_KIND)
        ));

        let mut options = test_options(None, None);
        options.require_enabled = false;
        let catch_up = catch_up_agent_trace_uploads(options).await;
        assert_eq!(catch_up.pruned, 1);
        assert_eq!(catch_up.semantic_edge_ledgers_pending, 1);
        assert!(catch_up.results.is_empty());
    }

    #[tokio::test]
    async fn upload_all_aggregates_results() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_session(dir.path(), "a.jsonl", &[session_header("a", "cwd")]);
        let second = write_session(dir.path(), "b.jsonl", &[session_header("b", "cwd")]);
        assert!(Path::new(&first).exists() && Path::new(&second).exists());
        let fetch_fn: FetchFn = Arc::new(|request| {
            Box::pin(async move {
                let _ = request;
                Ok(HttpResponse {
                    status: 200,
                    status_text: "OK".to_string(),
                    headers: Vec::new(),
                    text: json!({"bytes_stored": 10}).to_string(),
                })
            }) as BoxFuture<Result<HttpResponse, String>>
        });
        std::env::set_var("PRIME_AGENT_TRACES_API_KEY", "trace-key");
        let progress: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let progress_sink = progress.clone();
        let result = upload_all_agent_traces(&AgentTraceUploadAllOptions {
            upload: test_options(None, Some(fetch_fn)),
            session_dir: Some(dir.path().to_string_lossy().to_string()),
            concurrency: Some(2),
            on_progress: Some(Arc::new(move |update| {
                progress_sink.lock().unwrap().push(update.completed);
            })),
        })
        .await;
        std::env::remove_var("PRIME_AGENT_TRACES_API_KEY");
        assert_eq!(result.total, 2);
        assert_eq!(result.uploaded, 2);
        assert_eq!(result.failed, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.bytes_stored, 20);
        assert_eq!(progress.lock().unwrap().first().copied(), Some(0));
    }

    #[tokio::test]
    async fn find_agent_trace_files_finds_valid_sessions_only() {
        let dir = tempfile::tempdir().unwrap();
        write_session(dir.path(), "good.jsonl", &[session_header("good", "cwd")]);
        std::fs::write(dir.path().join("bad.jsonl"), "{}\n").unwrap();
        std::fs::write(dir.path().join("other.txt"), "x").unwrap();
        let found = find_agent_trace_files(Some(&dir.path().to_string_lossy())).await;
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("good.jsonl"));
    }

    #[test]
    fn retriable_status_classification() {
        assert!(is_retriable_http_status(503));
        assert!(is_retriable_http_status(408));
        assert!(!is_retriable_http_status(429));
        assert!(is_rescheduled_upload_failure(None));
        assert!(is_rescheduled_upload_failure(Some(429)));
        assert!(is_rescheduled_upload_failure(Some(503)));
        assert!(!is_rescheduled_upload_failure(Some(403)));
        assert_eq!(retriable_http_statuses().len(), 7);
    }
}
