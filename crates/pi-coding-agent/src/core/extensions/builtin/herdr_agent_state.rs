//! Port of packages/coding-agent/src/core/extensions/builtin/herdr-agent-state.ts
//!
//! Built-in Herdr integration extension.
//!
//! Reports agent lifecycle state (working/idle/blocked) to the Herdr terminal
//! workspace manager via its Unix socket. This is the in-tree equivalent of the
//! extension that `herdr integration install pi` writes, so Prime Agent works
//! inside Herdr panes out of the box without a manual install step.
//!
//! Unlike the file-based integration (re-evaluated per session load by jiti),
//! this module is statically linked and evaluated once per process. All env
//! capture and state therefore live inside the factory, which the resource
//! loader invokes per session load — inside the daemon's client-env window —
//! so each daemon session captures its own pane identity.
//!
//! The factory is a complete no-op when `HERDR_ENV` is not `"1"` (i.e. when
//! not running inside a Herdr pane), so it is safe to always load.

use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use crate::core::extensions::types::{
    ExtensionApi, ExtensionContext, ExtensionEvent, ExtensionFactory, ExtensionHandler,
};

/// `type AgentState = "working" | "blocked" | "idle"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Working,
    Blocked,
    Idle,
}

impl AgentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentState::Working => "working",
            AgentState::Blocked => "blocked",
            AgentState::Idle => "idle",
        }
    }
}

/// True when Herdr's own file-based Pi integration (`herdr integration install
/// pi`) is among the extension files the loader actually loaded this cycle.
/// That extension reports with the same `herdr:pi` source but its own seq
/// counter, so running the built-in alongside it would make the two reporters
/// race on one pane.
///
/// Loaded paths — not raw disk existence — are the deferral source of truth:
/// a file that exists but never loads (settings `!` overrides, noExtensions,
/// paths outside the discovery dirs such as the legacy `~/.pi/agent/`) never
/// becomes an active reporter, and deferring to it would leave the pane with
/// no reporter at all.
pub fn has_file_based_herdr_integration(loaded_extension_paths: &[String]) -> bool {
    loaded_extension_paths.iter().any(|path| {
        let base = Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        base == "herdr-agent-state.ts" || base == "herdr-agent-state.js"
    })
}

/// Windows dials local-domain sockets inside `\\.\pipe\`; Herdr exports a
/// unix-style path, so map it (namespaced paths pass through).
pub fn herdr_socket_target(socket_path: &str, platform: &str) -> String {
    // The pipe namespace is case-insensitive, so only the prefix check lowercases.
    let lowered = socket_path.to_lowercase();
    if platform != "win32" || lowered.starts_with("\\\\.\\pipe\\") || lowered.starts_with("\\\\?\\pipe\\") {
        return socket_path.to_string();
    }
    format!("\\\\.\\pipe\\{}", socket_path.trim_start_matches('\\'))
}

fn current_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

/// `interface QueuedState`.
#[derive(Debug, Clone)]
struct QueuedState {
    state: AgentState,
    message: Option<String>,
    seq: i64,
}

fn parse_duration_env(name: &str, fallback: u64) -> u64 {
    let Ok(raw) = std::env::var(name) else {
        return fallback;
    };
    if raw.is_empty() {
        return fallback;
    }
    match raw.trim().parse::<i64>() {
        Ok(parsed) if parsed >= 0 => parsed as u64,
        _ => fallback,
    }
}

fn last_assistant_message(messages: &[Value]) -> Option<&Value> {
    for index in (0..messages.len()).rev() {
        let message = &messages[index];
        if message.get("role").and_then(Value::as_str) == Some("assistant") {
            return Some(message);
        }
    }
    None
}

/// Error message of the turn's final assistant message, if it ended in error.
///
/// The agent's auto-retry treats nearly every provider error as retryable and
/// kicks in after extension `agent_end` fires, so any error end may be followed
/// by a retry. Rather than second-guessing the agent's classification with a
/// pattern list, hold "working" for every error through the retry grace
/// window: if a retry starts, `agent_start` keeps the pane working; if none
/// does, the hold settles to blocked with the error message.
fn error_hold_message(event: &Value) -> Option<String> {
    let messages = event
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let assistant = last_assistant_message(&messages)?;
    if assistant.get("stopReason").and_then(Value::as_str) != Some("error") {
        return None;
    }
    let message = assistant
        .get("errorMessage")
        .map(|value| match value {
            Value::String(text) => text.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        })
        .unwrap_or_default();
    if message.is_empty() {
        Some("provider error".to_string())
    } else {
        Some(message)
    }
}

/// Monotonic across all extension instances in this process. Herdr guards
/// `pane.report_agent` with a per-source seq and silently drops lower-seq
/// reports, so a successor session instance (after /new, resume, fork, or
/// reload) must never restart below a seq the previous instance already used —
/// otherwise its idle reports are dropped and the pane sticks at "working".
static REPORT_SEQ: Mutex<i64> = Mutex::new(0);

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn next_report_seq() -> i64 {
    let mut seq = REPORT_SEQ.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if *seq == 0 {
        *seq = now_ms().saturating_mul(1000);
    }
    *seq = (*seq + 1).max(now_ms().saturating_mul(1000));
    *seq
}

fn random_suffix() -> String {
    use rand::Rng;
    let value: f64 = rand::thread_rng().gen();
    // `Math.random().toString(36).slice(2)`.
    let mut digits: u64 = (value * (1u64 << 53) as f64) as u64;
    if digits == 0 {
        return "0".to_string();
    }
    let alphabet = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while digits > 0 {
        out.push(alphabet[(digits % 36) as usize]);
        digits /= 36;
    }
    out.reverse();
    String::from_utf8_lossy(&out).to_string()
}

/// Send one JSON line to the Herdr socket and wait for the first response byte,
/// EOF, an error, or the 500 ms timeout — matching `sendRequest` in the
/// TypeScript, which resolves on whichever comes first.
async fn send_request(target: &str, request: Value) -> Result<(), String> {
    let line = format!("{}\n", request);
    let connect = connect_socket(target);
    let exchange = async move {
        let mut stream = connect?;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        stream.write_all(line.as_bytes()).await?;
        let mut buffer = [0u8; 1024];
        // Resolve on the first response byte, like `socket.on("data", finish)`.
        let _ = stream.read(&mut buffer).await;
        Ok::<(), String>(())
    };
    match tokio::time::timeout(std::time::Duration::from_millis(500), exchange).await {
        Ok(result) => result,
        Err(_) => Ok(()),
    }
}

#[cfg(unix)]
async fn connect_socket(target: &str) -> Result<tokio::net::UnixStream, String> {
    tokio::net::UnixStream::connect(target)
        .await
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
async fn connect_socket(target: &str) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, String> {
    use tokio::net::windows::named_pipe::ClientOptions;
    ClientOptions::new()
        .open(target)
        .map_err(|error| error.to_string())
}

/// Schedule `callback` after `delay_ms`, ignoring the handle like the unref'd
/// `setTimeout`. Outside a Tokio runtime (unit tests) the callback runs
/// immediately so behaviour stays deterministic.
fn schedule_timeout(delay_ms: u64, callback: Arc<dyn Fn() + Send + Sync>) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                callback();
            });
        }
        Err(_) => callback(),
    }
}

/// Reporter state shared by every handler of one factory invocation.
struct Reporter {
    socket_path: String,
    pane_id: String,
    idle_debounce_ms: u64,
    retry_grace_ms: u64,
    state: Mutex<ReporterState>,
}

#[derive(Default)]
struct ReporterState {
    current_agent_session_id: Option<String>,
    current_agent_session_path: Option<String>,
    /// Object identity of the first session manager that starts, like
    /// `boundSessionManager` in the TypeScript.
    bound_session_manager: Option<*const ()>,
    send_in_flight: bool,
    queued_state: Option<QueuedState>,
    released: bool,
    agent_active: bool,
    retry_hold_active: bool,
    failure_blocked: bool,
    failure_message: Option<String>,
    blocked_count: i64,
    blocked_message: Option<String>,
    last_state: Option<AgentState>,
    last_message: Option<String>,
    idle_timer_generation: u64,
    retry_timer_generation: u64,
}

const SOURCE: &str = "herdr:pi";
const AGENT_LABEL: &str = "prime-agent";

fn session_manager_ptr(context: &Arc<dyn ExtensionContext>) -> Option<*const ()> {
    let manager = context.session_manager();
    Some(Arc::as_ptr(&manager) as *const ())
}

impl Reporter {
    fn is_bound_session(&self, context: &Arc<dyn ExtensionContext>) -> bool {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match state.bound_session_manager {
            None => true,
            Some(bound) => session_manager_ptr(context) == Some(bound),
        }
    }

    fn update_session_ref(&self, context: &Arc<dyn ExtensionContext>) {
        let manager = context.session_manager();
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.current_agent_session_path = manager.get_session_file().filter(|file| file.starts_with('/'));
        state.current_agent_session_id = manager.get_session_id().filter(|id| !id.is_empty());
    }

    fn with_session_ref(&self, params: Map<String, Value>) -> Map<String, Value> {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut params = params;
        if let Some(path) = &state.current_agent_session_path {
            params.insert("agent_session_path".to_string(), Value::String(path.clone()));
        } else if let Some(id) = &state.current_agent_session_id {
            params.insert("agent_session_id".to_string(), Value::String(id.clone()));
        }
        params
    }

    fn build_state_request(&self, state: AgentState, message: Option<String>, seq: i64) -> Value {
        let mut params = Map::new();
        params.insert("pane_id".to_string(), Value::String(self.pane_id.clone()));
        params.insert("source".to_string(), Value::String(SOURCE.to_string()));
        params.insert("agent".to_string(), Value::String(AGENT_LABEL.to_string()));
        params.insert("state".to_string(), Value::String(state.as_str().to_string()));
        if let Some(message) = message {
            params.insert("message".to_string(), Value::String(message));
        }
        params.insert("seq".to_string(), json!(seq));
        let params = self.with_session_ref(params);
        json!({
            "id": format!("{}:{}:{}", SOURCE, now_ms(), random_suffix()),
            "method": "pane.report_agent",
            "params": Value::Object(params),
        })
    }

    fn queue_state(&self, state: AgentState, message: Option<String>) {
        let seq = next_report_seq();
        let should_drain = {
            let mut guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.released {
                // The pane was released on quit; a late report would reclaim it
                // and leave Herdr showing an agent that already exited.
                return;
            }
            guard.queued_state = Some(QueuedState {
                state,
                message,
                seq,
            });
            !guard.send_in_flight
        };
        if should_drain {
            self.clone().spawn_drain();
        }
    }

    fn spawn_drain(self: Arc<Self>) {
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(self.drain_state_queue());
        } else {
            // No runtime (unit tests): run the loop on a fresh current-thread
            // runtime so the queue still drains in order.
            if let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                runtime.block_on(self.drain_state_queue());
            }
        }
    }

    async fn drain_state_queue(self: Arc<Self>) {
        {
            let mut guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.send_in_flight {
                return;
            }
            guard.send_in_flight = true;
        }

        loop {
            let next = {
                let mut guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                guard.queued_state.take()
            };
            let Some(next) = next else { break };
            let request = self.build_state_request(next.state, next.message, next.seq);
            let _ = send_request(&self.socket_path, request).await;
        }

        let restart = {
            let mut guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.send_in_flight = false;
            guard.queued_state.is_some()
        };
        if restart {
            self.spawn_drain();
        }
    }

    async fn release_agent(self: Arc<Self>) {
        // Stop new reports, drop anything still queued, and wait for the
        // in-flight send to finish so the release is the last write on the wire;
        // a report landing after the release would reclaim the pane.
        {
            let mut guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.released = true;
            guard.queued_state = None;
        }
        // `await activeDrain.catch(() => undefined)`: wait for any in-flight drain.
        while self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .send_in_flight
        {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        let request = json!({
            "id": format!("{}:release:{}:{}", SOURCE, now_ms(), random_suffix()),
            "method": "pane.release_agent",
            "params": {
                "pane_id": self.pane_id,
                "source": SOURCE,
                "agent": AGENT_LABEL,
                "seq": next_report_seq(),
            },
        });
        let _ = send_request(&self.socket_path, request).await;
    }

    fn clear_pending_timers(&self) {
        let mut guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.idle_timer_generation += 1;
        guard.retry_timer_generation += 1;
    }

    fn clear_failure_state(&self) {
        let mut guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.retry_hold_active = false;
        guard.failure_blocked = false;
        guard.failure_message = None;
    }

    fn desired_state(&self) -> (AgentState, Option<String>) {
        let guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.blocked_count > 0 {
            return (AgentState::Blocked, guard.blocked_message.clone());
        }
        if guard.failure_blocked {
            return (AgentState::Blocked, guard.failure_message.clone());
        }
        if guard.agent_active || guard.retry_hold_active {
            return (AgentState::Working, None);
        }
        (AgentState::Idle, None)
    }

    fn publish_state(&self, force: bool) {
        let (state, message) = self.desired_state();
        {
            let mut guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if !force && state == guard.last_state.unwrap_or(AgentState::Idle) && message == guard.last_message {
                return;
            }
            guard.last_state = Some(state);
            guard.last_message = message.clone();
        }
        self.queue_state(state, message);
    }

    fn schedule_idle(self: &Arc<Self>) {
        self.clear_pending_timers();
        self.clear_failure_state();
        let generation = {
            let mut guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.idle_timer_generation += 1;
            guard.idle_timer_generation
        };
        let reporter = self.clone();
        schedule_timeout(
            self.idle_debounce_ms,
            Arc::new(move || {
                let current = reporter
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .idle_timer_generation;
                if current != generation {
                    return;
                }
                reporter.publish_state(false);
            }),
        );
    }

    fn hold_for_retry(self: &Arc<Self>, message: String) {
        self.clear_pending_timers();
        {
            let mut guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.retry_hold_active = true;
            guard.failure_blocked = false;
            guard.failure_message = Some(message);
        }
        self.publish_state(false);

        let generation = {
            let mut guard = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.retry_timer_generation += 1;
            guard.retry_timer_generation
        };
        let reporter = self.clone();
        schedule_timeout(
            self.retry_grace_ms,
            Arc::new(move || {
                let current = {
                    let mut guard = reporter.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if guard.retry_timer_generation != generation {
                        return;
                    }
                    guard.retry_hold_active = false;
                    guard.failure_blocked = true;
                    guard.retry_timer_generation
                };
                let _ = current;
                reporter.publish_state(false);
            }),
        );
    }
}
