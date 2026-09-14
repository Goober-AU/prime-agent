//! Port of packages/coding-agent/src/core/telemetry.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::types::{AssistantMessage, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::settings_manager::SettingsManager;

const DEFAULT_TELEMETRY_ENDPOINT: &str = "https://api.primeintellect.ai/api/v1/agent-analytics/events";
const TELEMETRY_STATE_FILE: &str = "telemetry.json";
const TELEMETRY_STATE_VERSION: i64 = 1;
const DEFAULT_BATCH_SIZE: usize = 10;
const DEFAULT_FLUSH_INTERVAL_MS: u64 = 10_000;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 1_500;

/// `TelemetryPrimitive`.
pub type TelemetryPrimitive = Value;

/// `TelemetryProperties = Record<string, TelemetryPrimitive>`.
///
/// Property order is observable in the serialized payload, so this is an
/// insertion-ordered map.
pub type TelemetryProperties = IndexMap<String, TelemetryPrimitive>;

/// `type TelemetryEventName` - a closed union of five literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TelemetryEventName {
    #[serde(rename = "agent started")]
    AgentStarted,
    #[serde(rename = "onboarding completed")]
    OnboardingCompleted,
    #[serde(rename = "agent command used")]
    AgentCommandUsed,
    #[serde(rename = "agent run completed")]
    AgentRunCompleted,
    #[serde(rename = "agent session ended")]
    AgentSessionEnded,
}

impl TelemetryEventName {
    pub fn as_str(self) -> &'static str {
        match self {
            TelemetryEventName::AgentStarted => "agent started",
            TelemetryEventName::OnboardingCompleted => "onboarding completed",
            TelemetryEventName::AgentCommandUsed => "agent command used",
            TelemetryEventName::AgentRunCompleted => "agent run completed",
            TelemetryEventName::AgentSessionEnded => "agent session ended",
        }
    }
}

/// `TelemetryExecutionMode = AgentExecutionMode | "unknown"`.
pub type TelemetryExecutionMode = String;

/// `TelemetryOnboardingOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryOnboardingOutcome {
    Success,
    Error,
    Aborted,
}

impl TelemetryOnboardingOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            TelemetryOnboardingOutcome::Success => "success",
            TelemetryOnboardingOutcome::Error => "error",
            TelemetryOnboardingOutcome::Aborted => "aborted",
        }
    }
}

/// `TelemetryAuthCategory`.
pub type TelemetryAuthCategory = &'static str;

/// `interface TelemetryEvent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryEvent {
    pub id: String,
    pub name: TelemetryEventName,
    pub timestamp: String,
    pub properties: TelemetryProperties,
}

/// `interface TelemetryBatch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryBatch {
    pub installation_id: String,
    pub events: Vec<TelemetryEvent>,
}


/// `interface TelemetrySink`.
pub trait TelemetrySink: Send + Sync {
    fn capture(&self, name: TelemetryEventName, properties: TelemetryProperties);
    fn flush(&self) -> pi_ai::types::BoxFuture<()>;
}

/// `interface TelemetryState`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryState {
    pub version: i64,
    #[serde(rename = "installationId")]
    pub installation_id: String,
}

/// `interface TelemetryClientOptions`.
pub struct TelemetryClientOptions {
    pub agent_dir: String,
    pub endpoint: Option<String>,
    /// `fetch?: typeof fetch`.
    pub fetch: Option<crate::core::prime_inference_auth::FetchFn>,
    pub now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    pub random_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    pub batch_size: Option<usize>,
    pub flush_interval_ms: Option<u64>,
    pub request_timeout_ms: Option<u64>,
}

impl TelemetryClientOptions {
    pub fn new(agent_dir: impl Into<String>) -> Self {
        Self {
            agent_dir: agent_dir.into(),
            endpoint: None,
            fetch: None,
            now: None,
            random_id: None,
            batch_size: None,
            flush_interval_ms: None,
            request_timeout_ms: None,
        }
    }
}

/// `interface InstallAgentTelemetryOptions`.
pub struct InstallAgentTelemetryOptions {
    pub agent_dir: String,
    pub settings_manager: Arc<Mutex<SettingsManager>>,
    /// `executionMode?: AgentExecutionMode`.
    pub execution_mode: Option<crate::core::agent_session_config::AgentExecutionMode>,
    pub sink: Option<Arc<dyn TelemetrySink>>,
    pub now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    pub random_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
}

/// `interface CaptureOnboardingCompletedOptions`.
pub struct CaptureOnboardingCompletedOptions {
    pub agent_dir: String,
    pub settings_manager: Arc<Mutex<SettingsManager>>,
    pub duration_ms: f64,
    pub outcome: TelemetryOnboardingOutcome,
    pub provider: Option<String>,
    /// `authSource?: AuthStatus["source"]`.
    pub auth_source: Option<String>,
    /// `storedCredentialType?: AuthCredential["type"]`.
    pub stored_credential_type: Option<String>,
    pub sink: Option<Arc<dyn TelemetrySink>>,
    pub now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    pub random_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
}

/// `interface CaptureAgentCommandUsedOptions`.
pub struct CaptureAgentCommandUsedOptions {
    pub agent_dir: String,
    pub settings_manager: Arc<Mutex<SettingsManager>>,
    pub command_name: String,
    pub sink: Option<Arc<dyn TelemetrySink>>,
    pub now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    pub random_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
}

/// `interface UsageTotals extends Usage { modelCallCount: number }`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageTotals {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total_tokens: f64,
    pub cost: pi_ai::types::UsageCost,
    pub model_call_count: f64,
}

/// `interface ActiveRun`.
#[derive(Debug, Clone)]
pub struct ActiveRun {
    pub started_at: f64,
    pub agent_ended: bool,
    /// `firstTurnStartedAt?`
    pub first_turn_started_at: Option<f64>,
    /// `firstModelEventMs?`
    pub first_model_event_ms: Option<f64>,
    /// `visibleTtftMs?`
    pub visible_ttft_ms: Option<f64>,
    /// `currentTurnStartedAt?`
    pub current_turn_started_at: Option<f64>,
    pub model_latency_ms: f64,
    pub max_model_latency_ms: f64,
    pub turn_count: f64,
    pub tool_call_count: f64,
    pub tool_error_count: f64,
    pub compaction_count: f64,
    pub retry_count: f64,
    pub usage: UsageTotals,
    pub last_assistant: Option<AssistantMessage>,
}

/// `interface SessionTotals`.
#[derive(Debug, Clone)]
pub struct SessionTotals {
    pub started_at: f64,
    pub run_count: f64,
    pub successful_run_count: f64,
    pub failed_run_count: f64,
    pub aborted_run_count: f64,
    pub prompt_count: f64,
    pub tool_call_count: f64,
    pub compaction_count: f64,
    pub usage: UsageTotals,
}

/// `EMPTY_USAGE_TOTALS`.
pub fn empty_usage_totals() -> UsageTotals {
    UsageTotals::default()
}

/// `newUsageTotals()` (`structuredClone(EMPTY_USAGE_TOTALS)`).
fn new_usage_totals() -> UsageTotals {
    UsageTotals::default()
}

/// `addUsage(target, usage)`.
fn add_usage(target: &mut UsageTotals, usage: &Usage) {
    target.input += usage.input;
    target.output += usage.output;
    target.cache_read += usage.cache_read;
    target.cache_write += usage.cache_write;
    target.total_tokens += usage.total_tokens;
    target.cost.input += usage.cost.input;
    target.cost.output += usage.cost.output;
    target.cost.cache_read += usage.cost.cache_read;
    target.cost.cache_write += usage.cost.cache_write;
    target.cost.total += usage.cost.total;
    target.model_call_count += 1.0;
}

/// `mergeUsage(target, usage)`.
fn merge_usage(target: &mut UsageTotals, usage: &UsageTotals) {
    target.input += usage.input;
    target.output += usage.output;
    target.cache_read += usage.cache_read;
    target.cache_write += usage.cache_write;
    target.total_tokens += usage.total_tokens;
    target.cost.input += usage.cost.input;
    target.cost.output += usage.cost.output;
    target.cost.cache_read += usage.cost.cache_read;
    target.cost.cache_write += usage.cost.cache_write;
    target.cost.total += usage.cost.total;
    target.model_call_count += usage.model_call_count;
}

/// `parseBooleanOverride`.
///
/// The return type distinguishes "unset" from "set but unrecognized": both are
/// `undefined` in the TypeScript, so a single `Option<bool>` is exact here.
fn parse_boolean_override(value: Option<&str>) -> Option<bool> {
    let value = value?;
    let normalized = value.trim().to_lowercase();
    if ["1", "true", "yes", "on"].contains(&normalized.as_str()) {
        return Some(true);
    }
    if ["0", "false", "no", "off"].contains(&normalized.as_str()) {
        return Some(false);
    }
    None
}

/// `isTelemetryEnabled`.
pub fn is_telemetry_enabled(settings_manager: &SettingsManager) -> bool {
    if parse_boolean_override(env_var("PI_OFFLINE").as_deref()) == Some(true) {
        return false;
    }
    if parse_boolean_override(env_var("DO_NOT_TRACK").as_deref()) == Some(true) {
        return false;
    }
    if let Some(override_) = parse_boolean_override(env_var("PRIME_AGENT_TELEMETRY").as_deref()) {
        return override_;
    }
    settings_manager.get_telemetry_enabled()
}

/// `process.env[name]` - an empty value is not "set" for this module's checks.
fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}


/// `isInstallationId`.
fn is_installation_id(value: &str) -> bool {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
            .expect("valid regex literal")
    });
    // The TypeScript uses the `i` flag.
    pattern.is_match(&value.to_lowercase())
}

/// `readInstallationId`.
fn read_installation_id(path: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&content).ok()?;
    let version = parsed.get("version").and_then(Value::as_i64)?;
    let installation_id = parsed.get("installationId").and_then(Value::as_str)?;
    if version == TELEMETRY_STATE_VERSION && is_installation_id(installation_id) {
        return Some(installation_id.to_string());
    }
    None
}

/// `writeTelemetryStateAtomically(path, state)`.
fn write_telemetry_state_atomically(path: &str, state: &TelemetryState) {
    let data = serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".to_string());
    let _ = crate::utils::atomic_file::write_file_atomic_sync(
        path,
        &data,
        crate::utils::atomic_file::WriteFileAtomicOptions {
            mode: Some(0o600),
            fsync: false,
            fsync_dir: false,
            before_rename: None,
        },
    );
}

/// Node error codes this module distinguishes.
fn error_code(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::AlreadyExists => "EEXIST",
        _ => "UNKNOWN",
    }
}

/// `getOrCreateTelemetryInstallationId(agentDir, randomId = randomUUID)`.
///
/// Every failure is returned instead of thrown so `capture()` can contain it the
/// way the TypeScript `try`/`catch` does.
pub fn get_or_create_telemetry_installation_id(
    agent_dir: &str,
    random_id: &dyn Fn() -> String,
) -> Result<String, String> {
    let path = join_path(agent_dir, TELEMETRY_STATE_FILE);
    let mut replace_invalid_state = false;
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            // `lstatSync` + `isFile()`: a symlink or directory is rejected.
            if !metadata.is_file() {
                return Err("Telemetry state path must be a regular file".to_string());
            }
            if let Some(existing) = read_installation_id(&path) {
                return Ok(existing);
            }
            replace_invalid_state = true;
        }
        Err(error) => {
            if error_code(&error) != "ENOENT" {
                return Err(error.to_string());
            }
        }
    }

    let installation_id = random_id();
    if !is_installation_id(&installation_id) {
        return Err("Telemetry installation ID generator returned an invalid UUID".to_string());
    }
    std::fs::create_dir_all(agent_dir).map_err(|error| error.to_string())?;
    let state = TelemetryState {
        version: TELEMETRY_STATE_VERSION,
        installation_id: installation_id.clone(),
    };
    if replace_invalid_state {
        write_telemetry_state_atomically(&path, &state);
        return Ok(installation_id);
    }

    // `writeFileSync(path, data, { flag: "wx", mode: 0o600 })`.
    let data = serde_json::to_string_pretty(&state).unwrap_or_else(|_| "{}".to_string());
    let mut open_options = std::fs::OpenOptions::new();
    open_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open_options.mode(0o600);
    }
    match open_options.open(&path) {
        Ok(mut file) => {
            use std::io::Write;
            let _ = file.write_all(data.as_bytes());
            Ok(installation_id)
        }
        Err(error) => {
            if error_code(&error) != "EEXIST" {
                return Err(error.to_string());
            }
            Ok(read_installation_id(&path).unwrap_or(installation_id))
        }
    }
}

/// `modelCategory`.
fn model_category(model: &str) -> String {
    let normalized = model.to_lowercase();
    let categories = [
        "claude", "gpt", "o1", "o3", "o4", "gemini", "glm", "kimi", "qwen", "deepseek", "llama", "mistral",
    ];
    categories
        .iter()
        .find(|category| normalized.contains(**category))
        .map(|category| category.to_string())
        .unwrap_or_else(|| "custom".to_string())
}

/// `telemetryProviderCategory`.
pub fn telemetry_provider_category(provider: Option<&str>) -> String {
    let Some(provider) = provider else {
        return "unknown".to_string();
    };
    let normalized = provider.to_lowercase();
    let categories = [
        "anthropic",
        "openai",
        "google",
        "prime",
        "openrouter",
        "bedrock",
        "vertex",
        "mistral",
        "groq",
        "xai",
    ];
    categories
        .iter()
        .find(|category| normalized.contains(**category))
        .map(|category| category.to_string())
        .unwrap_or_else(|| "custom".to_string())
}

/// `AuthCredential["type"]` - the discriminant the category table switches on.
pub fn auth_credential_type(credential: &crate::core::auth_storage::AuthCredential) -> &'static str {
    match credential {
        crate::core::auth_storage::AuthCredential::ApiKey { .. } => "api_key",
        crate::core::auth_storage::AuthCredential::OAuth { .. } => "oauth",
    }
}

/// `telemetryAuthCategory`.
pub fn telemetry_auth_category(
    source: Option<&str>,
    stored_credential_type: Option<&str>,
) -> TelemetryAuthCategory {
    match source {
        Some("stored") => match stored_credential_type {
            Some(credential_type) => match credential_type {
                "api_key" => "api_key",
                "oauth" => "oauth",
                _ => "stored",
            },
            None => "stored",
        },
        Some("runtime") => "runtime_api_key",
        Some("environment") => "environment",
        Some("prime_cli") => "prime_cli",
        Some("models_json_key") | Some("models_json_command") => "models_json",
        Some("fallback") => "fallback",
        Some("stale") => "stale",
        _ => "none",
    }
}

/// `baseProperties(executionMode)`.
fn base_properties(execution_mode: &str) -> TelemetryProperties {
    properties(&[
        ("version", string_value(crate::config::version())),
        (
            "os_family",
            string_value(crate::utils::pi_user_agent::process_platform()),
        ),
        (
            "architecture",
            string_value(crate::utils::pi_user_agent::process_arch()),
        ),
        (
            "install_method",
            string_value(crate::config::detect_install_method()),
        ),
        ("execution_mode", string_value(execution_mode)),
    ])
}

/// `path.join` for the one join this module performs.
fn join_path(base: &str, leaf: &str) -> String {
    std::path::Path::new(base).join(leaf).to_string_lossy().to_string()
}


/// Object-literal helper. A map keeps JSON key order identical to the
/// TypeScript literal order, which the payload preserves.
fn properties(pairs: &[(&str, TelemetryPrimitive)]) -> TelemetryProperties {
    let mut map: TelemetryProperties = IndexMap::new();
    for (key, value) in pairs {
        map.insert(key.to_string(), value.clone());
    }
    map
}

fn string_value(value: &str) -> TelemetryPrimitive {
    Value::String(value.to_string())
}

/// `JSON.stringify` keeps integral numbers integral.
fn number_value(value: f64) -> TelemetryPrimitive {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9.007_199_254_740_992e15 {
        Value::Number(serde_json::Number::from(value as i64))
    } else {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

fn null_value() -> TelemetryPrimitive {
    Value::Null
}

/// Shared client state.
///
/// `flush()` in the TypeScript returns one coalesced promise. A Rust caller
/// cannot observe promise identity, so the port preserves the observable part of
/// that contract: a tokio mutex serializes drains, every `flush()` awaits the
/// same drain, and no caller can grow a waiter queue.
#[derive(Default)]
struct TelemetryClientInner {
    installation_id: Mutex<Option<String>>,
    queue: Mutex<Vec<TelemetryEvent>>,
    flush_lock: tokio::sync::Mutex<()>,
    disabled: std::sync::atomic::AtomicBool,
    /// `flushTimer` armed.
    timer_scheduled: std::sync::atomic::AtomicBool,
}

/// `TelemetryClient implements TelemetrySink`.
pub struct TelemetryClient {
    endpoint: String,
    fetch_impl: crate::core::prime_inference_auth::FetchFn,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
    random_id: Arc<dyn Fn() -> String + Send + Sync>,
    batch_size: usize,
    flush_interval_ms: u64,
    request_timeout_ms: u64,
    agent_dir: String,
    inner: Arc<TelemetryClientInner>,
}

impl TelemetryClient {
    /// `constructor(options)`.
    pub fn new(options: TelemetryClientOptions) -> Self {
        let endpoint = options
            .endpoint
            .clone()
            .or_else(|| std::env::var("PRIME_AGENT_TELEMETRY_ENDPOINT").ok())
            .unwrap_or_else(|| DEFAULT_TELEMETRY_ENDPOINT.to_string());
        let fetch_impl = options
            .fetch
            .clone()
            .unwrap_or_else(crate::core::prime_inference_auth::default_fetch);
        let now = options.now.clone().unwrap_or_else(|| Arc::new(utc_now_ms));
        let random_id = options
            .random_id
            .clone()
            .unwrap_or_else(|| Arc::new(|| uuid::Uuid::new_v4().to_string()));
        Self {
            endpoint,
            fetch_impl,
            now,
            random_id,
            batch_size: options.batch_size.unwrap_or(DEFAULT_BATCH_SIZE),
            flush_interval_ms: options.flush_interval_ms.unwrap_or(DEFAULT_FLUSH_INTERVAL_MS),
            request_timeout_ms: options
                .request_timeout_ms
                .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
            agent_dir: options.agent_dir.clone(),
            inner: Arc::new(TelemetryClientInner::default()),
        }
    }

    /// `capture(name, properties)`.
    pub fn capture(&self, name: TelemetryEventName, properties: TelemetryProperties) {
        use std::sync::atomic::Ordering;
        if self.inner.disabled.load(Ordering::SeqCst) {
            return;
        }
        if self.inner.installation_id.lock().unwrap().is_none() {
            match get_or_create_telemetry_installation_id(&self.agent_dir, &*self.random_id) {
                Ok(value) => *self.inner.installation_id.lock().unwrap() = Some(value),
                Err(_) => {
                    // Product analytics is best-effort and must never affect the agent.
                    self.inner.disabled.store(true, Ordering::SeqCst);
                    self.inner.queue.lock().unwrap().clear();
                    return;
                }
            }
        }
        if self.inner.disabled.load(Ordering::SeqCst) {
            return;
        }

        let event = TelemetryEvent {
            id: (self.random_id)(),
            name,
            timestamp: to_iso_string((self.now)()),
            properties,
        };
        let queue_length = {
            let mut queue = self.inner.queue.lock().unwrap();
            queue.push(event);
            queue.len()
        };
        if queue_length >= self.batch_size {
            return;
        }
        self.schedule_flush();
    }

    /// `private scheduleFlush()`.
    ///
    /// `setTimeout(...).unref()` maps to a detached task: the timer never keeps
    /// the process alive and nothing waits for it.
    fn schedule_flush(&self) {
        use std::sync::atomic::Ordering;
        if self
            .inner
            .timer_scheduled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            // No runtime: the queue is drained by an explicit `flush()`.
            self.inner.timer_scheduled.store(false, Ordering::SeqCst);
            return;
        };
        let inner = Arc::clone(&self.inner);
        let endpoint = self.endpoint.clone();
        let fetch_impl = Arc::clone(&self.fetch_impl);
        let request_timeout_ms = self.request_timeout_ms;
        let batch_size = self.batch_size;
        let interval = self.flush_interval_ms;
        handle.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
            inner.timer_scheduled.store(false, Ordering::SeqCst);
            flush_inner(&inner, &endpoint, &fetch_impl, batch_size, request_timeout_ms).await;
        });
    }

    /// `flush()`.
    pub fn flush(&self) -> pi_ai::types::BoxFuture<()> {
        let inner = Arc::clone(&self.inner);
        let endpoint = self.endpoint.clone();
        let fetch_impl = Arc::clone(&self.fetch_impl);
        let request_timeout_ms = self.request_timeout_ms;
        let batch_size = self.batch_size;
        Box::pin(async move {
            flush_inner(&inner, &endpoint, &fetch_impl, batch_size, request_timeout_ms).await
        })
    }
}

impl TelemetrySink for TelemetryClient {
    fn capture(&self, name: TelemetryEventName, properties: TelemetryProperties) {
        TelemetryClient::capture(self, name, properties);
    }

    fn flush(&self) -> pi_ai::types::BoxFuture<()> {
        TelemetryClient::flush(self)
    }
}

/// `flush()` body: clear the timer, then drain every queued batch.
///
/// The timer is cleared first, which is what the TypeScript does before it looks
/// at `flushInFlight`, so a scheduled flush can never race the explicit one.
async fn flush_inner(
    inner: &Arc<TelemetryClientInner>,
    endpoint: &str,
    fetch_impl: &crate::core::prime_inference_auth::FetchFn,
    batch_size: usize,
    request_timeout_ms: u64,
) {
    use std::sync::atomic::Ordering;
    inner.timer_scheduled.store(false, Ordering::SeqCst);

    // One drain at a time; concurrent callers await the same flight.
    let _drain = inner.flush_lock.lock().await;
    loop {
        if inner.queue.lock().unwrap().is_empty() || inner.installation_id.lock().unwrap().is_none() {
            return;
        }
        drain_queue(inner, endpoint, fetch_impl, batch_size, request_timeout_ms).await;
        if inner.queue.lock().unwrap().is_empty() {
            return;
        }
    }
}

/// `private async drainQueue()`.
async fn drain_queue(
    inner: &Arc<TelemetryClientInner>,
    endpoint: &str,
    fetch_impl: &crate::core::prime_inference_auth::FetchFn,
    batch_size: usize,
    request_timeout_ms: u64,
) {
    loop {
        let installation_id = inner.installation_id.lock().unwrap().clone();
        let Some(installation_id) = installation_id else {
            return;
        };
        let events: Vec<TelemetryEvent> = {
            let mut queue = inner.queue.lock().unwrap();
            if queue.is_empty() {
                return;
            }
            let take = queue.len().min(batch_size);
            queue.drain(0..take).collect()
        };
        let batch = TelemetryBatch {
            installation_id,
            events,
        };
        send_batch(fetch_impl, endpoint, &batch, request_timeout_ms).await;
    }
}

/// `private async send(batch)`.
async fn send_batch(
    fetch_impl: &crate::core::prime_inference_auth::FetchFn,
    endpoint: &str,
    batch: &TelemetryBatch,
    request_timeout_ms: u64,
) {
    let headers = vec![
        ("content-type".to_string(), "application/json".to_string()),
        (
            "user-agent".to_string(),
            format!("prime-agent/{}", crate::config::VERSION),
        ),
    ];
    let request = crate::core::prime_inference_auth::HttpRequest {
        method: "POST".to_string(),
        url: endpoint.to_string(),
        headers,
        body: Some(serde_json::to_string(batch).unwrap_or_else(|_| "{}".to_string())),
        timeout_ms: request_timeout_ms,
    };
    // `AbortSignal.timeout(this.requestTimeoutMs)` covers the request; the fetch
    // implementation applies the same timeout and the response is discarded.
    let _ = fetch_impl(request).await;
}

/// `Date.now()` in milliseconds.
fn utc_now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

/// Node's `Date#toISOString()` (millisecond precision, `Z` suffix).
fn to_iso_string(millis: f64) -> String {
    match chrono::DateTime::from_timestamp_millis(millis as i64) {
        Some(value) => value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        None => "1970-01-01T00:00:00.000Z".to_string(),
    }
}


/// `captureOnboardingCompleted(options)`.
pub async fn capture_onboarding_completed(options: CaptureOnboardingCompletedOptions) {
    {
        let settings = options.settings_manager.lock().unwrap();
        if !is_telemetry_enabled(&settings) {
            return;
        }
    }

    let now = options.now.clone().unwrap_or_else(|| Arc::new(utc_now_ms));
    let random_id = options
        .random_id
        .clone()
        .unwrap_or_else(|| Arc::new(|| uuid::Uuid::new_v4().to_string()));
    let sink = options.sink.clone().unwrap_or_else(|| {
        Arc::new(TelemetryClient::new(TelemetryClientOptions {
            agent_dir: options.agent_dir.clone(),
            endpoint: None,
            fetch: None,
            now: Some(Arc::clone(&now)),
            random_id: Some(Arc::clone(&random_id)),
            batch_size: None,
            flush_interval_ms: None,
            request_timeout_ms: None,
        }))
    });

    let mut props = base_properties("interactive");
    props.insert(
        "duration_ms".to_string(),
        number_value(options.duration_ms.max(0.0)),
    );
    props.insert(
        "outcome".to_string(),
        string_value(options.outcome.as_str()),
    );
    props.insert(
        "auth_category".to_string(),
        string_value(telemetry_auth_category(
            options.auth_source.as_deref(),
            options.stored_credential_type.as_deref(),
        )),
    );
    props.insert(
        "provider_category".to_string(),
        string_value(&telemetry_provider_category(options.provider.as_deref())),
    );
    sink.capture(TelemetryEventName::OnboardingCompleted, props);
    sink.flush().await;
}

/// `captureAgentCommandUsed(options)`.
pub async fn capture_agent_command_used(options: CaptureAgentCommandUsedOptions) {
    if !crate::core::slash_commands::is_builtin_slash_command_name(&options.command_name) {
        return;
    }
    {
        let settings = options.settings_manager.lock().unwrap();
        if !is_telemetry_enabled(&settings) {
            return;
        }
    }
    let command_name = crate::core::slash_commands::resolve_builtin_slash_command_name(&options.command_name);

    let now = options.now.clone().unwrap_or_else(|| Arc::new(utc_now_ms));
    let random_id = options
        .random_id
        .clone()
        .unwrap_or_else(|| Arc::new(|| uuid::Uuid::new_v4().to_string()));
    let sink = options.sink.clone().unwrap_or_else(|| {
        Arc::new(TelemetryClient::new(TelemetryClientOptions {
            agent_dir: options.agent_dir.clone(),
            endpoint: None,
            fetch: None,
            now: Some(Arc::clone(&now)),
            random_id: Some(Arc::clone(&random_id)),
            batch_size: None,
            flush_interval_ms: None,
            request_timeout_ms: None,
        }))
    });

    let mut props = base_properties("interactive");
    props.insert("command_name".to_string(), string_value(&command_name));
    sink.capture(TelemetryEventName::AgentCommandUsed, props);
    sink.flush().await;
}

/// `errorCategory(message)`.
fn error_category(message: Option<&AssistantMessage>) -> TelemetryPrimitive {
    let Some(message) = message else {
        return null_value();
    };
    if message.stop_reason != pi_ai::types::STOP_REASON_ERROR {
        return null_value();
    }
    let error = message
        .error_message
        .clone()
        .unwrap_or_default()
        .to_lowercase();
    let matches = |pattern: &str| {
        regex::Regex::new(pattern)
            .map(|regex| regex.is_match(&error))
            .unwrap_or(false)
    };
    if matches(r"\b401\b|\b403\b|auth|api.?key|credential|unauthori[sz]ed|forbidden") {
        return string_value("authentication");
    }
    if matches(r"\b429\b|rate.?limit|quota") {
        return string_value("rate_limit");
    }
    if matches(r"timeout|timed out") {
        return string_value("timeout");
    }
    if matches(r"context|token.*limit|too long|maximum.*length") {
        return string_value("context_limit");
    }
    if matches(r"network|socket|connection|fetch") {
        return string_value("network");
    }
    if matches(r"\b5\d\d\b|overload|unavailable") {
        return string_value("provider_unavailable");
    }
    string_value("other")
}

/// `runOutcome(message)`.
fn run_outcome(message: Option<&AssistantMessage>) -> &'static str {
    if let Some(message) = message {
        if message.stop_reason == pi_ai::types::STOP_REASON_ABORTED {
            return "aborted";
        }
        if message.stop_reason != pi_ai::types::STOP_REASON_ERROR {
            return "success";
        }
    }
    "error"
}

/// `createActiveRun(now)`.
fn create_active_run(now: &Arc<dyn Fn() -> f64 + Send + Sync>) -> ActiveRun {
    ActiveRun {
        started_at: (now)(),
        agent_ended: false,
        first_turn_started_at: None,
        first_model_event_ms: None,
        visible_ttft_ms: None,
        current_turn_started_at: None,
        model_latency_ms: 0.0,
        max_model_latency_ms: 0.0,
        turn_count: 0.0,
        tool_call_count: 0.0,
        tool_error_count: 0.0,
        compaction_count: 0.0,
        retry_count: 0.0,
        usage: new_usage_totals(),
        last_assistant: None,
    }
}


/// Shared aggregation state for one installed session.
struct InstalledTelemetryState {
    session_totals: SessionTotals,
    active_run: Option<ActiveRun>,
    turn_action_active: bool,
}

/// `event.message.role === "assistant"` narrowing for `message_end`.
fn assistant_message_from_agent_message(
    message: &pi_agent_core::types::AgentMessage,
) -> Option<AssistantMessage> {
    match message {
        pi_agent_core::types::AgentMessage::Message(pi_ai::types::Message::Assistant(assistant)) => {
            Some(assistant.clone())
        }
        _ => None,
    }
}

/// `installAgentTelemetry(session, options)`.
pub fn install_agent_telemetry(
    session: &std::sync::Arc<crate::core::agent_session::AgentSession>,
    options: InstallAgentTelemetryOptions,
) {
    {
        let settings = options.settings_manager.lock().unwrap();
        if !is_telemetry_enabled(&settings) {
            return;
        }
    }

    let now = options.now.clone().unwrap_or_else(|| Arc::new(utc_now_ms));
    let random_id = options
        .random_id
        .clone()
        .unwrap_or_else(|| Arc::new(|| uuid::Uuid::new_v4().to_string()));
    let sink = options.sink.clone().unwrap_or_else(|| {
        Arc::new(TelemetryClient::new(TelemetryClientOptions {
            agent_dir: options.agent_dir.clone(),
            endpoint: None,
            fetch: None,
            now: Some(Arc::clone(&now)),
            random_id: Some(Arc::clone(&random_id)),
            batch_size: None,
            flush_interval_ms: None,
            request_timeout_ms: None,
        }))
    });
    let session_id = (random_id)();
    let execution_mode = options
        .execution_mode
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    let state = Arc::new(Mutex::new(InstalledTelemetryState {
        session_totals: SessionTotals {
            started_at: (now)(),
            run_count: 0.0,
            successful_run_count: 0.0,
            failed_run_count: 0.0,
            aborted_run_count: 0.0,
            prompt_count: 0.0,
            tool_call_count: 0.0,
            compaction_count: 0.0,
            usage: new_usage_totals(),
        },
        active_run: None,
        turn_action_active: false,
    }));

    // `commonProperties()`.
    let common_properties: Arc<dyn Fn() -> TelemetryProperties + Send + Sync> = {
        let session_id = session_id.clone();
        let execution_mode = execution_mode.clone();
        Arc::new(move || {
            let mut props = base_properties(&execution_mode);
            props.insert("session_id".to_string(), string_value(&session_id));
            props
        })
    };

    // `finalizeRun()`.
    let finalize_run: Arc<dyn Fn() + Send + Sync> = {
        let sink = Arc::clone(&sink);
        let state = Arc::clone(&state);
        let common_properties = Arc::clone(&common_properties);
        let now = Arc::clone(&now);
        Arc::new(move || {
            let run = {
                let mut guard = state.lock().unwrap();
                guard.active_run.take()
            };
            let Some(run) = run else {
                return;
            };
            let outcome = run_outcome(run.last_assistant.as_ref());
            {
                let mut guard = state.lock().unwrap();
                guard.session_totals.run_count += 1.0;
                guard.session_totals.tool_call_count += run.tool_call_count;
                guard.session_totals.compaction_count += run.compaction_count;
                if outcome == "success" {
                    guard.session_totals.successful_run_count += 1.0;
                } else if outcome == "aborted" {
                    guard.session_totals.aborted_run_count += 1.0;
                } else {
                    guard.session_totals.failed_run_count += 1.0;
                }
                merge_usage(&mut guard.session_totals.usage, &run.usage);
            }
            let last_assistant = run.last_assistant.clone();
            let mut props = common_properties();
            props.insert("outcome".to_string(), string_value(outcome));
            props.insert(
                "duration_ms".to_string(),
                number_value(((now)() - run.started_at).max(0.0)),
            );
            props.insert(
                "visible_ttft_ms".to_string(),
                match run.visible_ttft_ms {
                    Some(value) => number_value(value),
                    None => null_value(),
                },
            );
            props.insert(
                "first_model_event_ms".to_string(),
                match run.first_model_event_ms {
                    Some(value) => number_value(value),
                    None => null_value(),
                },
            );
            props.insert("model_latency_ms".to_string(), number_value(run.model_latency_ms));
            props.insert(
                "max_model_latency_ms".to_string(),
                number_value(run.max_model_latency_ms),
            );
            props.insert(
                "model_call_count".to_string(),
                number_value(run.usage.model_call_count),
            );
            props.insert("turn_count".to_string(), number_value(run.turn_count));
            props.insert("tool_call_count".to_string(), number_value(run.tool_call_count));
            props.insert("tool_error_count".to_string(), number_value(run.tool_error_count));
            props.insert("input_tokens".to_string(), number_value(run.usage.input));
            props.insert("output_tokens".to_string(), number_value(run.usage.output));
            props.insert("cache_read_tokens".to_string(), number_value(run.usage.cache_read));
            props.insert("cache_write_tokens".to_string(), number_value(run.usage.cache_write));
            props.insert("total_tokens".to_string(), number_value(run.usage.total_tokens));
            props.insert("compaction_count".to_string(), number_value(run.compaction_count));
            props.insert("retry_count".to_string(), number_value(run.retry_count));
            props.insert(
                "provider_category".to_string(),
                string_value(&telemetry_provider_category(
                    last_assistant.as_ref().map(|message| message.provider.as_str()),
                )),
            );
            props.insert(
                "model_category".to_string(),
                string_value(&match last_assistant.as_ref() {
                    Some(message) => model_category(&message.model),
                    None => "unknown".to_string(),
                }),
            );
            props.insert(
                "error_category".to_string(),
                error_category(last_assistant.as_ref()),
            );
            sink.capture(TelemetryEventName::AgentRunCompleted, props);
        })
    };

    sink.capture(TelemetryEventName::AgentStarted, common_properties());

    let unsubscribe = {
        use crate::core::agent_session::AgentSessionEvent;
        use pi_agent_core::types::AgentEvent;
        let state = Arc::clone(&state);
        let now = Arc::clone(&now);
        let finalize_run = Arc::clone(&finalize_run);
        session.subscribe(Arc::new(move |event: AgentSessionEvent| match event {
            AgentSessionEvent::SessionActionUpdate { actions } => {
                let turn_action_active = actions
                    .active
                    .as_ref()
                    .map(|active| {
                        active.kind == crate::core::session_action_store::SessionActionSnapshotKind::Turn
                    })
                    .unwrap_or(false);
                let should_finalize = {
                    let mut guard = state.lock().unwrap();
                    guard.turn_action_active = turn_action_active;
                    !turn_action_active
                        && guard
                            .active_run
                            .as_ref()
                            .map(|run| run.agent_ended)
                            .unwrap_or(false)
                };
                if should_finalize {
                    finalize_run();
                }
            }
            AgentSessionEvent::Agent(AgentEvent::AgentStart) => {
                let needs_finalize = {
                    let guard = state.lock().unwrap();
                    guard
                        .active_run
                        .as_ref()
                        .map(|run| run.agent_ended && !guard.turn_action_active)
                        .unwrap_or(false)
                };
                if needs_finalize {
                    finalize_run();
                }
                let mut guard = state.lock().unwrap();
                if guard.active_run.is_none() {
                    guard.active_run = Some(create_active_run(&now));
                }
                if let Some(run) = guard.active_run.as_mut() {
                    run.agent_ended = false;
                }
            }
            AgentSessionEvent::Agent(AgentEvent::MessageStart { message })
            | AgentSessionEvent::MessageStart { message } => {
                if message.role() == "user" {
                    let mut guard = state.lock().unwrap();
                    guard.session_totals.prompt_count += 1.0;
                }
            }
            AgentSessionEvent::Agent(AgentEvent::TurnStart) => {
                let mut guard = state.lock().unwrap();
                if let Some(run) = guard.active_run.as_mut() {
                    run.current_turn_started_at = Some((now)());
                    if run.first_turn_started_at.is_none() {
                        run.first_turn_started_at = run.current_turn_started_at;
                    }
                    run.turn_count += 1.0;
                }
            }
            AgentSessionEvent::Agent(AgentEvent::MessageUpdate {
                assistant_message_event,
                ..
            }) => {
                let mut guard = state.lock().unwrap();
                let Some(run) = guard.active_run.as_mut() else {
                    return;
                };
                if run.first_model_event_ms.is_none() {
                    if let Some(started) = run.first_turn_started_at {
                        run.first_model_event_ms = Some(((now)() - started).max(0.0));
                    }
                }
                if run.visible_ttft_ms.is_none() {
                    if let (Some(started), pi_ai::types::AssistantMessageEvent::TextDelta { delta, .. }) =
                        (run.first_turn_started_at, &assistant_message_event)
                    {
                        if !delta.is_empty() {
                            run.visible_ttft_ms = Some(((now)() - started).max(0.0));
                        }
                    }
                }
            }
            // The TypeScript union is flat, so `message_start` / `message_end`
            // arrive for agent messages and for the session's own messages
            // through the same case. Both Rust variants therefore share one arm.
            AgentSessionEvent::Agent(AgentEvent::MessageEnd { message })
            | AgentSessionEvent::MessageEnd { message } => {
                let Some(message) = assistant_message_from_agent_message(&message) else {
                    return;
                };
                let mut guard = state.lock().unwrap();
                let Some(run) = guard.active_run.as_mut() else {
                    return;
                };
                run.last_assistant = Some(message.clone());
                add_usage(&mut run.usage, &message.usage);
                if let Some(started) = run.current_turn_started_at {
                    let latency = ((now)() - started).max(0.0);
                    run.model_latency_ms += latency;
                    run.max_model_latency_ms = run.max_model_latency_ms.max(latency);
                    run.current_turn_started_at = None;
                }
            }
            AgentSessionEvent::Agent(AgentEvent::ToolExecutionEnd { is_error, .. }) => {
                let mut guard = state.lock().unwrap();
                if let Some(run) = guard.active_run.as_mut() {
                    run.tool_call_count += 1.0;
                    if is_error {
                        run.tool_error_count += 1.0;
                    }
                }
            }
            AgentSessionEvent::CompactionEnd { result, aborted, .. } => {
                let mut guard = state.lock().unwrap();
                if let Some(run) = guard.active_run.as_mut() {
                    if result.is_some() && !aborted {
                        run.compaction_count += 1.0;
                    }
                }
            }
            AgentSessionEvent::AutoRetryStart { .. } => {
                let mut guard = state.lock().unwrap();
                if let Some(run) = guard.active_run.as_mut() {
                    run.retry_count += 1.0;
                }
            }
            AgentSessionEvent::Agent(AgentEvent::AgentEnd { .. }) => {
                let should_finalize = {
                    let mut guard = state.lock().unwrap();
                    let turn_action_active = guard.turn_action_active;
                    match guard.active_run.as_mut() {
                        Some(run) => {
                            run.agent_ended = true;
                            !turn_action_active
                        }
                        None => false,
                    }
                };
                if should_finalize {
                    finalize_run();
                }
            }
            _ => {}
        }))
    };

    {
        let state = Arc::clone(&state);
        let sink = Arc::clone(&sink);
        let common_properties = Arc::clone(&common_properties);
        let now = Arc::clone(&now);
        let finalize_run = Arc::clone(&finalize_run);
        let unsubscribe = Arc::clone(&unsubscribe);
        session.register_dispose_callback(Arc::new(move || {
            let state = Arc::clone(&state);
            let sink = Arc::clone(&sink);
            let common_properties = Arc::clone(&common_properties);
            let now = Arc::clone(&now);
            let finalize_run = Arc::clone(&finalize_run);
            let unsubscribe = Arc::clone(&unsubscribe);
            Box::pin(async move {
                unsubscribe();
                finalize_run();
                let mut props = common_properties();
                {
                    let guard = state.lock().unwrap();
                    props.insert(
                        "duration_ms".to_string(),
                        number_value(((now)() - guard.session_totals.started_at).max(0.0)),
                    );
                    props.insert(
                        "prompt_count".to_string(),
                        number_value(guard.session_totals.prompt_count),
                    );
                    props.insert("run_count".to_string(), number_value(guard.session_totals.run_count));
                    props.insert(
                        "successful_run_count".to_string(),
                        number_value(guard.session_totals.successful_run_count),
                    );
                    props.insert(
                        "failed_run_count".to_string(),
                        number_value(guard.session_totals.failed_run_count),
                    );
                    props.insert(
                        "aborted_run_count".to_string(),
                        number_value(guard.session_totals.aborted_run_count),
                    );
                    props.insert(
                        "tool_call_count".to_string(),
                        number_value(guard.session_totals.tool_call_count),
                    );
                    props.insert(
                        "compaction_count".to_string(),
                        number_value(guard.session_totals.compaction_count),
                    );
                    props.insert(
                        "model_call_count".to_string(),
                        number_value(guard.session_totals.usage.model_call_count),
                    );
                    props.insert(
                        "input_tokens".to_string(),
                        number_value(guard.session_totals.usage.input),
                    );
                    props.insert(
                        "output_tokens".to_string(),
                        number_value(guard.session_totals.usage.output),
                    );
                    props.insert(
                        "cache_read_tokens".to_string(),
                        number_value(guard.session_totals.usage.cache_read),
                    );
                    props.insert(
                        "cache_write_tokens".to_string(),
                        number_value(guard.session_totals.usage.cache_write),
                    );
                    props.insert(
                        "total_tokens".to_string(),
                        number_value(guard.session_totals.usage.total_tokens),
                    );
                }
                sink.capture(TelemetryEventName::AgentSessionEnded, props);
                sink.flush().await;
            }) as pi_ai::types::BoxFuture<()>
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid_generator() -> Arc<dyn Fn() -> String + Send + Sync> {
        let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        Arc::new(move || {
            let value = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            format!("00000000-0000-4000-8000-{:012}", value)
        })
    }

    fn temp_agent_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pi-telemetry-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct RecordingSink {
        events: Mutex<Vec<(TelemetryEventName, TelemetryProperties)>>,
        flush_count: std::sync::atomic::AtomicU64,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
                flush_count: std::sync::atomic::AtomicU64::new(0),
            }
        }

        fn events(&self) -> Vec<(TelemetryEventName, TelemetryProperties)> {
            self.events.lock().unwrap().clone()
        }
    }

    impl TelemetrySink for RecordingSink {
        fn capture(&self, name: TelemetryEventName, properties: TelemetryProperties) {
            self.events.lock().unwrap().push((name, properties));
        }

        fn flush(&self) -> pi_ai::types::BoxFuture<()> {
            self.flush_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    /// A fetch stub that records requests and never reaches the network.
    fn recording_fetch(
        requests: Arc<Mutex<Vec<crate::core::prime_inference_auth::HttpRequest>>>,
    ) -> crate::core::prime_inference_auth::FetchFn {
        Arc::new(move |request| {
            let requests = Arc::clone(&requests);
            Box::pin(async move {
                requests.lock().unwrap().push(request);
                Ok(crate::core::prime_inference_auth::HttpResponse {
                    status: 204,
                    ..Default::default()
                })
            })
        })
    }

    #[test]
    fn creates_a_private_stable_installation_id() {
        let agent_dir = temp_agent_dir("identity");
        let random_id = uuid_generator();
        let first = get_or_create_telemetry_installation_id(&agent_dir.to_string_lossy(), &*random_id).unwrap();
        let second = get_or_create_telemetry_installation_id(&agent_dir.to_string_lossy(), &*random_id).unwrap();
        assert_eq!(second, first);

        let path = agent_dir.join("telemetry.json");
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["version"], serde_json::json!(1));
        assert_eq!(parsed["installationId"], serde_json::json!(first));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(&agent_dir);
    }

    #[test]
    fn replaces_invalid_persisted_installation_state() {
        let agent_dir = temp_agent_dir("invalid");
        let path = agent_dir.join("telemetry.json");
        std::fs::write(&path, "not-json").unwrap();
        let random_id = uuid_generator();
        let installation_id =
            get_or_create_telemetry_installation_id(&agent_dir.to_string_lossy(), &*random_id).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["installationId"], serde_json::json!(installation_id));
        // A second call is stable once the state is valid.
        assert_eq!(
            get_or_create_telemetry_installation_id(&agent_dir.to_string_lossy(), &*random_id).unwrap(),
            installation_id
        );
        let _ = std::fs::remove_dir_all(&agent_dir);
    }

    #[test]
    fn rejects_a_non_regular_telemetry_state_path() {
        let agent_dir = temp_agent_dir("directory-state");
        std::fs::create_dir_all(agent_dir.join("telemetry.json")).unwrap();
        let random_id = uuid_generator();
        let result = get_or_create_telemetry_installation_id(&agent_dir.to_string_lossy(), &*random_id);
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&agent_dir);
    }

    #[tokio::test]
    async fn batches_events_through_the_configured_endpoint_without_network_access() {
        let agent_dir = temp_agent_dir("batch");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let client = TelemetryClient::new(TelemetryClientOptions {
            agent_dir: agent_dir.to_string_lossy().to_string(),
            endpoint: Some("https://api.example.test/api/v1/agent-analytics/events".to_string()),
            fetch: Some(recording_fetch(Arc::clone(&requests))),
            random_id: Some(uuid_generator()),
            now: Some(Arc::new(|| 1_774_483_200_000.0)),
            ..TelemetryClientOptions::new("")
        });

        client.capture(
            TelemetryEventName::AgentStarted,
            properties(&[("version", string_value("1.2.3")), ("os_family", string_value("darwin"))]),
        );
        client.flush().await;

        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].url, "https://api.example.test/api/v1/agent-analytics/events");
        assert_eq!(recorded[0].method, "POST");
        let body: Value = serde_json::from_str(recorded[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["events"][0]["name"], serde_json::json!("agent started"));
        assert_eq!(
            body["events"][0]["timestamp"],
            serde_json::json!("2026-03-26T00:00:00.000Z")
        );
        assert_eq!(body["events"][0]["properties"]["version"], serde_json::json!("1.2.3"));
        drop(recorded);
        let _ = std::fs::remove_dir_all(&agent_dir);
    }

    #[tokio::test]
    async fn never_throws_when_the_analytics_endpoint_fails() {
        let agent_dir = temp_agent_dir("failing-endpoint");
        let failing: crate::core::prime_inference_auth::FetchFn = Arc::new(|_request| {
            Box::pin(async { Err("network failed".to_string()) })
        });
        let client = TelemetryClient::new(TelemetryClientOptions {
            agent_dir: agent_dir.to_string_lossy().to_string(),
            fetch: Some(failing),
            random_id: Some(uuid_generator()),
            ..TelemetryClientOptions::new("")
        });
        client.capture(TelemetryEventName::AgentStarted, IndexMap::new());
        client.flush().await;
        let _ = std::fs::remove_dir_all(&agent_dir);
    }

    #[tokio::test]
    async fn drains_every_queued_batch_before_flush_resolves() {
        let agent_dir = temp_agent_dir("drain");
        let batch_sizes = Arc::new(Mutex::new(Vec::new()));
        let sizes = Arc::clone(&batch_sizes);
        let fetch: crate::core::prime_inference_auth::FetchFn = Arc::new(move |request| {
            let sizes = Arc::clone(&sizes);
            Box::pin(async move {
                let body: Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
                sizes
                    .lock()
                    .unwrap()
                    .push(body["events"].as_array().unwrap().len());
                Ok(crate::core::prime_inference_auth::HttpResponse {
                    status: 204,
                    ..Default::default()
                })
            })
        });
        let client = TelemetryClient::new(TelemetryClientOptions {
            agent_dir: agent_dir.to_string_lossy().to_string(),
            fetch: Some(fetch),
            random_id: Some(uuid_generator()),
            batch_size: Some(2),
            ..TelemetryClientOptions::new("")
        });
        for index in 0..5 {
            client.capture(
                TelemetryEventName::AgentStarted,
                properties(&[("index", number_value(index as f64))]),
            );
        }
        client.flush().await;
        let sizes = batch_sizes.lock().unwrap().clone();
        assert_eq!(sizes.iter().sum::<usize>(), 5);
        assert!(sizes.iter().all(|size| *size <= 2));
        let _ = std::fs::remove_dir_all(&agent_dir);
    }

    #[tokio::test]
    async fn never_throws_when_the_local_state_cannot_be_written() {
        let parent = temp_agent_dir("unwritable");
        let agent_dir = parent.join("not-a-directory");
        std::fs::write(&agent_dir, "occupied").unwrap();
        let client = TelemetryClient::new(TelemetryClientOptions {
            agent_dir: agent_dir.to_string_lossy().to_string(),
            random_id: Some(uuid_generator()),
            ..TelemetryClientOptions::new("")
        });
        client.capture(TelemetryEventName::AgentStarted, IndexMap::new());
        client.flush().await;
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn parses_boolean_overrides_exactly_like_the_typescript() {
        assert_eq!(parse_boolean_override(None), None);
        assert_eq!(parse_boolean_override(Some("  1 ")), Some(true));
        assert_eq!(parse_boolean_override(Some("TRUE")), Some(true));
        assert_eq!(parse_boolean_override(Some("yes")), Some(true));
        assert_eq!(parse_boolean_override(Some("ON")), Some(true));
        assert_eq!(parse_boolean_override(Some("0")), Some(false));
        assert_eq!(parse_boolean_override(Some("False")), Some(false));
        assert_eq!(parse_boolean_override(Some("no")), Some(false));
        assert_eq!(parse_boolean_override(Some("off")), Some(false));
        assert_eq!(parse_boolean_override(Some("maybe")), None);
    }

    #[test]
    fn categorizes_auth_sources_and_providers() {
        assert_eq!(telemetry_auth_category(Some("stored"), Some("oauth")), "oauth");
        assert_eq!(telemetry_auth_category(Some("stored"), Some("api_key")), "api_key");
        assert_eq!(telemetry_auth_category(Some("stored"), None), "stored");
        assert_eq!(telemetry_auth_category(Some("runtime"), None), "runtime_api_key");
        assert_eq!(telemetry_auth_category(Some("environment"), None), "environment");
        assert_eq!(telemetry_auth_category(Some("prime_cli"), None), "prime_cli");
        assert_eq!(telemetry_auth_category(Some("models_json_key"), None), "models_json");
        assert_eq!(telemetry_auth_category(Some("models_json_command"), None), "models_json");
        assert_eq!(telemetry_auth_category(Some("fallback"), None), "fallback");
        assert_eq!(telemetry_auth_category(Some("stale"), None), "stale");
        assert_eq!(telemetry_auth_category(None, None), "none");

        assert_eq!(telemetry_provider_category(None), "unknown");
        assert_eq!(telemetry_provider_category(Some("Prime")), "prime");
        assert_eq!(telemetry_provider_category(Some("openai-codex")), "openai");
        assert_eq!(telemetry_provider_category(Some("my-gateway")), "custom");
    }

    #[test]
    fn classifies_error_messages() {
        fn failing(error_message: &str) -> AssistantMessage {
            let mut message = AssistantMessage::default();
            message.stop_reason = pi_ai::types::STOP_REASON_ERROR.to_string();
            message.error_message = Some(error_message.to_string());
            message
        }
        assert_eq!(error_category(None), Value::Null);
        assert_eq!(error_category(Some(&failing("HTTP 401 unauthorized"))), string_value("authentication"));
        assert_eq!(error_category(Some(&failing("429 rate limit"))), string_value("rate_limit"));
        assert_eq!(error_category(Some(&failing("request timed out"))), string_value("timeout"));
        assert_eq!(error_category(Some(&failing("maximum context length"))), string_value("context_limit"));
        assert_eq!(error_category(Some(&failing("socket hang up"))), string_value("network"));
        assert_eq!(error_category(Some(&failing("503 overloaded"))), string_value("provider_unavailable"));
        assert_eq!(error_category(Some(&failing("something else"))), string_value("other"));
        let mut stopped = failing("ignored");
        stopped.stop_reason = pi_ai::types::STOP_REASON_STOP.to_string();
        assert_eq!(error_category(Some(&stopped)), Value::Null);
    }

    #[test]
    fn maps_run_outcomes_from_stop_reasons() {
        assert_eq!(run_outcome(None), "error");
        let mut message = AssistantMessage::default();
        assert_eq!(run_outcome(Some(&message)), "success");
        message.stop_reason = pi_ai::types::STOP_REASON_ABORTED.to_string();
        assert_eq!(run_outcome(Some(&message)), "aborted");
        message.stop_reason = pi_ai::types::STOP_REASON_ERROR.to_string();
        assert_eq!(run_outcome(Some(&message)), "error");
    }

    #[tokio::test]
    async fn captures_only_allowlisted_builtin_command_names() {
        let sink = Arc::new(RecordingSink::new());
        std::env::set_var("PRIME_AGENT_TELEMETRY", "1");
        std::env::set_var("DO_NOT_TRACK", "0");
        for command in ["model", "private-extension-command"] {
            capture_agent_command_used(CaptureAgentCommandUsedOptions {
                agent_dir: "/not-used".to_string(),
                settings_manager: Arc::new(Mutex::new(SettingsManager::in_memory(
                    crate::core::settings_manager::Settings::new(),
                ))),
                command_name: command.to_string(),
                sink: Some(Arc::clone(&sink) as Arc<dyn TelemetrySink>),
                now: None,
                random_id: None,
            })
            .await;
        }
        std::env::remove_var("PRIME_AGENT_TELEMETRY");
        std::env::remove_var("DO_NOT_TRACK");

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, TelemetryEventName::AgentCommandUsed);
        assert_eq!(events[0].1["command_name"], serde_json::json!("model"));
        assert_eq!(events[0].1["execution_mode"], serde_json::json!("interactive"));
    }

    #[tokio::test]
    async fn captures_onboarding_completion_with_categorized_auth_and_provider_data() {
        let sink = Arc::new(RecordingSink::new());
        std::env::set_var("PRIME_AGENT_TELEMETRY", "1");
        std::env::set_var("DO_NOT_TRACK", "0");
        capture_onboarding_completed(CaptureOnboardingCompletedOptions {
            agent_dir: "/not-used".to_string(),
            settings_manager: Arc::new(Mutex::new(SettingsManager::in_memory(
                crate::core::settings_manager::Settings::new(),
            ))),
            duration_ms: 250.0,
            outcome: TelemetryOnboardingOutcome::Success,
            provider: Some("prime".to_string()),
            auth_source: Some("stored".to_string()),
            stored_credential_type: Some("oauth".to_string()),
            sink: Some(Arc::clone(&sink) as Arc<dyn TelemetrySink>),
            now: None,
            random_id: None,
        })
        .await;
        std::env::remove_var("PRIME_AGENT_TELEMETRY");
        std::env::remove_var("DO_NOT_TRACK");

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, TelemetryEventName::OnboardingCompleted);
        assert_eq!(events[0].1["duration_ms"], serde_json::json!(250));
        assert_eq!(events[0].1["outcome"], serde_json::json!("success"));
        assert_eq!(events[0].1["auth_category"], serde_json::json!("oauth"));
        assert_eq!(events[0].1["provider_category"], serde_json::json!("prime"));
        assert_eq!(sink.flush_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn usage_totals_accumulate_like_add_and_merge_usage() {
        let mut totals = new_usage_totals();
        let usage = Usage {
            input: 100.0,
            output: 20.0,
            cache_read: 50.0,
            cache_write: 0.0,
            total_tokens: 170.0,
            cost: pi_ai::types::UsageCost {
                input: 0.001,
                output: 0.002,
                cache_read: 0.0001,
                cache_write: 0.0,
                total: 0.0031,
            },
        };
        add_usage(&mut totals, &usage);
        assert_eq!(totals.model_call_count, 1.0);
        assert_eq!(totals.total_tokens, 170.0);
        assert_eq!(totals.cost.total, 0.0031);

        let mut merged = new_usage_totals();
        merge_usage(&mut merged, &totals);
        assert_eq!(merged.model_call_count, 1.0);
        assert_eq!(merged.input, 100.0);
    }
}
