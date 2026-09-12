//! Port of packages/coding-agent/src/modes/daemon/daemon-mode.ts
//!
//! Background daemon mode. The daemon owns live `AgentSessionRuntime` instances
//! and exposes a small JSONL protocol over a local socket.
//!
//! Slice plumbing: several modules this file imports are owned by other slices
//! and only exist as thin stubs today (`modes/daemon/daemon-protocol.ts`,
//! `modes/daemon/daemon-runtime-identity.ts`, `core/cron-jobs.ts`,
//! `core/agent-session-runtime.ts`, `core/agent-session-config.ts`,
//! `core/rlm-runtime.ts`, `core/side-question.ts`, `config.ts`). The private
//! plumbing below carries the same names and shapes those modules export so the
//! daemon-mode port stands on its own; it moves out unchanged when those slices
//! land. Everything private is marked `// slice plumbing:`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use base64::Engine;
use futures::future::BoxFuture;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex, Notify};

use pi_agent_core::types::AgentMessage;

use crate::core::agent_messages::{
    build_agent_family_roster, create_agent_session_message, create_agent_session_message_id,
    create_agent_session_message_receipt, format_agent_session_name_unavailable,
    normalize_agent_session_message, session_name_reservation_key, AgentFamilyCatalogEntry,
    AgentFamilyRelationship, AgentFamilyRosterResult, AgentSessionMessageAgentSummary,
    AgentSessionMessageController, AgentSessionMessageDeliveryStatus, AgentSessionMessageEndpoint,
    AgentSessionMessageListResult, AgentSessionMessagePayload, AgentSessionMessageRateLimiter,
    AgentSessionMessageReceipt, AgentSessionMessageSender, AGENT_FAMILY_REACH_ERROR,
    AGENT_MESSAGE_SOURCE, DEFAULT_AGENT_MESSAGE_MAX_CHARS, DEFAULT_AGENT_MESSAGE_MAX_PENDING_PER_SESSION,
    DEFAULT_AGENT_MESSAGE_RATE_LIMIT_CAPACITY, DEFAULT_AGENT_MESSAGE_RATE_LIMIT_REFILL_MS,
};
use crate::core::agent_observe::{
    create_agent_observe_message_preview, normalize_observe_limit, normalize_observe_max_chars,
    AgentObserveAgentSnapshot, AgentObserveAgentSummary, AgentObserveController, AgentObserveListResult,
    AgentObserveRecentMessagesInput, AgentObserveRecentMessagesResult,
};
use crate::core::cron_jobs::{
    is_heartbeat_cron_job, normalize_heartbeat_delivery_mode, normalize_heartbeat_schedule,
    resolve_heartbeat_streaming_behavior, should_defer_heartbeat_cron_job, AgentCronJob,
    AgentCronJobStore, AgentCronScheduler, AgentHeartbeatDeliveryMode, AgentHeartbeatManagementAction,
    AgentHeartbeatUpdateAction, DEFAULT_HEARTBEAT_SCHEDULE,
};
use crate::core::orphan_process_journal::ORPHAN_PROCESS_JOURNAL_ENV;
use crate::core::prompt_admission::{PromptAdmissionCancelledError, wait_for_prompt_admission};
use crate::core::session_action_store::{can_passivate_session, IdleEvictionMinutes, SessionPassivationSnapshot};
use crate::core::session_file_actions::{delete_session_artifacts, delete_session_file, DeleteSessionFileOptions, DeleteSessionFileResult};
use crate::core::session_lease::{acquire_session_lease, canonical_session_path, SessionLease};
use crate::core::session_manager::{
    get_session_artifact_path_for_file, order_session_context_for_transcript, read_session_info,
    resolve_session_rlm_depth, SessionHistorySnapshot, SessionInfo,
};
use crate::core::session_manager::SessionManager;
use crate::core::session_resolver::resolve_session_path;
use crate::core::settings_manager::SettingsManager;
use crate::modes::agent_connection::snapshot::{
    create_agent_connection_commands, create_agent_connection_resource_snapshot, create_agent_connection_state,
};
use crate::modes::agent_connection::tool_definition::create_agent_connection_tool_definition;
use crate::modes::agent_connection::types::AgentConnectionHeartbeat;
use crate::modes::rpc::jsonl::JsonlLineReader;
use crate::utils::child_process::{is_process_alive, spawn_hidden, wait_for_child_process, SpawnOptions};
use crate::utils::dir_lock::try_acquire_dir_lock;
use crate::utils::shell::kill_tracked_detached_children;

use super::active_session_state::{
    create_active_session_id, resolve_active_session_state, ActiveSessionState, DaemonExtensionUIResponse,
    DaemonSocketClient,
};
use super::agent_roster::{
    passivated_worker_roster_entry, roster_agent_id_for_summary, worker_roster_entry_from_summary,
    RegisteredHeartbeatFlags, RosterSessionSummary, WorkerRosterEntry,
};
use super::compact_session_stream::create_compact_assistant_delta;
use super::daemon_client::protocol::{
    collect_daemon_client_env as collect_daemon_launch_env, create_daemon_event_meta, create_daemon_replay_info,
    is_daemon_command_envelope, is_daemon_dialog_extension_ui_request, is_daemon_mutating_command,
    is_session_plane_daemon_command, salvage_daemon_command_id, DaemonCommand, DaemonEventMeta, DaemonHello,
    DaemonOutbound, DaemonReplayInfo, DaemonResponse, DaemonSavedSessionInfo, DaemonSessionSnapshot,
    DaemonUpdateRestartManifest, DaemonUpdateRestartSession, DAEMON_DEFAULT_CLIENT_CAPABILITIES,
    DAEMON_DEFAULT_SERVER_CAPABILITIES, DAEMON_SCHEMA_ID, DAEMON_SCHEMA_REVISION,
    DAEMON_SUPPORTED_CLIENT_CAPABILITIES,
};
use super::daemon_client::DaemonClient;
use super::daemon_client_env::{filter_client_env, with_client_env};
use super::daemon_errors::{
    deserialize_daemon_error, serialize_daemon_error, DaemonError, DaemonErrorInfo,
};
use super::daemon_extension_binding::{
    bind_active_session_state, ActiveSessionBindingCallbacks, DaemonExtensionBindingSession,
};
use super::daemon_session_list::{
    build_session_list, classify_session_roster_status, has_live_session_work, inactive_lifecycle_for_session,
    scheduled_job_registrations, summary_for_active_session, SessionSummary,
};
use super::daemon_session_summarizer::DaemonSessionSummarizer;
use super::daemon_socket::{
    cleanup_daemon_socket_path, default_daemon_socket_path, get_daemon_socket_identity, normalize_socket_path,
    prepare_daemon_socket_path, restrict_daemon_socket_path, DaemonSocketIdentity,
};
use super::daemon_supervisor_ownership::{
    assert_daemon_supervisor_owner_current, is_daemon_shutdown_admission_active,
};
use super::daemon_worker_protocol::{
    is_daemon_worker_frame_header, DaemonWorkerCommand, DaemonWorkerFrameHeader, DaemonWorkerPeerGrant,
    DaemonWorkerRosterOutbound, DAEMON_WORKER_ACTIVE_SESSION_ID_ENV, DAEMON_WORKER_PEER_TRANSPORT_CAPABILITY,
    DAEMON_WORKER_RECOVERY_JOURNAL_ENV, DAEMON_WORKER_ROLE_ENV, DAEMON_WORKER_ROSTER_CAPABILITY,
    DAEMON_WORKER_SUPERVISOR_SOCKET_ENV, DAEMON_WORKER_TOKEN_ENV, ROSTER_HEARTBEAT_INTERVAL_MS,
};
use super::daemon_worker_client::{encode_private_frame, PrivateFrameDecoder};
use super::mutation_drain_latch::MutationDrainLatch;
use super::rlm_ledger::{
    create_rlm_ledger_registry_seed_source, read_legacy_rlm_subagent_registry, tombstone_saved_session_delete,
    with_passive_rlm_descendant_infos, LegacyRlmSubagentRegistryEntry, RlmLedgerDeleteReason, RlmLedgerEdge,
    RlmSpawnLedger, RlmSpawnInput,
};
use super::rlm_subagent_display::{
    read_rlm_subagent_display_entry, rlm_subagent_display_path, write_rlm_subagent_display_entry,
    RlmSubagentDisplayEntry, RlmSubagentModel,
};
use super::saved_session_info::serialize_saved_session_info;
use super::snapshot_transcript_cache::{
    create_snapshot_transcript_chunks, CreateSnapshotTranscriptChunksOptions, SNAPSHOT_TARGET_CHUNK_BYTES,
};
use super::worker_recovery_journal::{WorkerRecoveryJournal, WorkerRecoveryRecordInput};

// slice plumbing: `getLogger("coding-agent.daemon")`.
static STRUCTURED_LOG: std::sync::OnceLock<pi_ai::log::Logger> = std::sync::OnceLock::new();

fn structured_log() -> &'static pi_ai::log::Logger {
    STRUCTURED_LOG.get_or_init(|| pi_ai::log::get_logger("coding-agent.daemon"))
}

const WORKER_SNAPSHOT_TERMINAL_DRAIN_TIMEOUT_MS: u64 = 1_000;
const UPDATE_RESTART_PREPARE_TIMEOUT_MS: u64 = 90_000;
const MAX_SESSION_SNAPSHOT_STABILIZATION_RETRIES: u32 = 3;
pub const INITIAL_HISTORY_WINDOW_MESSAGES: usize = 400;
pub const MAX_HISTORY_RANGE_MESSAGES: usize = 400;

// slice plumbing: `DAEMON_UPDATE_RESTART_FORMAT_VERSION` from daemon-protocol.ts.
const DAEMON_UPDATE_RESTART_FORMAT_VERSION: u32 = 1;

// slice plumbing: `VERSION` from config.ts.
const VERSION: &str = "0.9.3";

// slice plumbing: `getDaemonUpdateRestartManifestPath` from config.ts.
fn get_daemon_update_restart_manifest_path(socket_path: &str) -> String {
    format!("{socket_path}.update-restart.json")
}

// slice plumbing: `getDaemonLogPath` from config.ts.
fn get_daemon_log_path(socket_path: &str) -> String {
    let base = socket_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("daemon")
        .replace(['.'], "-");
    format!("{base}.log")
}

// slice plumbing: `appendRotatingLog` from config.ts.
fn append_rotating_log(path: &str, message: &str) {
    use std::io::Write;
    if let Some(parent) = Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}

// slice plumbing: `getCronJobsPath` from config.ts.
fn get_cron_jobs_path(agent_dir: &str) -> String {
    Path::new(agent_dir).join("cron-jobs.json").to_string_lossy().to_string()
}

// slice plumbing: `getSessionsDir` from config.ts.
fn get_sessions_dir(agent_dir: &str) -> String {
    Path::new(agent_dir).join("sessions").to_string_lossy().to_string()
}

// slice plumbing: `initTheme` from modes/interactive/theme/theme.ts.
fn init_theme_headless(theme_name: Option<&str>) {
    crate::modes::interactive::theme::theme::init_theme(theme_name, false);
}

// slice plumbing: `getDaemonRuntimeIdentity` from daemon-runtime-identity.ts.
fn get_daemon_runtime_identity() -> Value {
    serde_json::json!({
        "version": VERSION,
        "buildId": std::env::var("PRIME_AGENT_BUILD_ID").unwrap_or_else(|_| "unknown".to_string()),
        "pid": std::process::id(),
    })
}

// slice plumbing: `waitForHeadlessCompletion` from modes/headless-completion.ts.
async fn wait_for_headless_completion(_state: &ActiveSessionState) {
    // The headless-completion slice owns the real wait; the daemon command only
    // needs the completion signal it already awaits on the session.
}

// slice plumbing: `startSideQuestion` from core/side-question.ts.
async fn start_side_question(_state: &ActiveSessionState, _input: &Value) -> Result<Value, String> {
    Err("side questions are not available in this build".to_string())
}

// slice plumbing: `providerRetryPolicy` from core/provider-retry.ts, re-exported
// through the daemon session summarizer slice.
use super::daemon_session_summarizer::provider_retry_policy;

// slice plumbing: `ORPHAN_PROCESS_JOURNAL_ENV` is imported from the core slice.

/// `DaemonModeOptions.defaultSessionConfig` (`AgentSessionRuntimeConfig`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentSessionRuntimeConfig {
    pub cwd: Option<String>,
    #[serde(rename = "agentDir")]
    pub agent_dir: Option<String>,
    #[serde(rename = "sessionDir", skip_serializing_if = "Option::is_none", default)]
    pub session_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model: Option<String>,
    #[serde(rename = "thinkingLevel", skip_serializing_if = "Option::is_none", default)]
    pub thinking_level: Option<String>,
    #[serde(rename = "telemetryDisabled", skip_serializing_if = "Option::is_none", default)]
    pub telemetry_disabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub api_key: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `mergeAgentSessionRuntimeConfig` from core/agent-session-config.ts.
pub fn merge_agent_session_runtime_config(
    base: &AgentSessionRuntimeConfig,
    override_config: Option<&AgentSessionRuntimeConfig>,
) -> AgentSessionRuntimeConfig {
    let Some(override_config) = override_config else {
        return base.clone();
    };
    let mut merged = base.clone();
    if override_config.cwd.is_some() {
        merged.cwd = override_config.cwd.clone();
    }
    if override_config.agent_dir.is_some() {
        merged.agent_dir = override_config.agent_dir.clone();
    }
    if override_config.session_dir.is_some() {
        merged.session_dir = override_config.session_dir.clone();
    }
    if override_config.provider.is_some() {
        merged.provider = override_config.provider.clone();
    }
    if override_config.model.is_some() {
        merged.model = override_config.model.clone();
    }
    if override_config.thinking_level.is_some() {
        merged.thinking_level = override_config.thinking_level.clone();
    }
    if override_config.telemetry_disabled.is_some() {
        merged.telemetry_disabled = override_config.telemetry_disabled;
    }
    if override_config.api_key.is_some() {
        merged.api_key = override_config.api_key.clone();
    }
    for (key, value) in &override_config.extra {
        merged.extra.insert(key.clone(), value.clone());
    }
    merged
}

/// `DaemonModeOptions.worker`.
#[derive(Debug, Clone, Default)]
pub struct DaemonWorkerOptions {
    pub authentication_token: String,
    pub worker_instance_id: Option<String>,
    pub restore_active_session_id: Option<String>,
}

/// `DaemonModeOptions`.
#[derive(Clone)]
pub struct DaemonModeOptions {
    pub socket_path: Option<String>,
    pub default_session_config: AgentSessionRuntimeConfig,
    pub create_runtime: CreateAgentSessionRuntimeFactory,
    pub worker: Option<DaemonWorkerOptions>,
}

/// `CreateAgentSessionRuntimeFactory` from core/agent-session-runtime.ts.
pub type CreateAgentSessionRuntimeFactory = Arc<
    dyn Fn(CreateAgentSessionRuntimeInput) -> BoxFuture<'static, Result<AgentSessionRuntimeHandle, String>>
        + Send
        + Sync,
>;

/// The `createAgentSessionRuntime(factory, input)` argument.
#[derive(Debug, Clone)]
pub struct CreateAgentSessionRuntimeInput {
    pub factory: Value,
    pub cwd: String,
    pub agent_dir: Option<String>,
    pub session_manager: Arc<std::sync::Mutex<SessionManager>>,
    pub session_options: SessionRuntimeOptions,
    pub runtime_metadata: Option<Value>,
}

/// `sessionOptions` handed to the runtime factory.
#[derive(Debug, Clone, Default)]
pub struct SessionRuntimeOptions {
    pub model: Option<Value>,
    pub rlm_heartbeat_controller: Option<Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>>,
    pub agent_message_controller: Option<Arc<dyn AgentSessionMessageController>>,
    pub agent_observe_controller: Option<Arc<dyn AgentObserveController>>,
}

/// `AgentSessionRuntime` as the daemon holds it.
#[derive(Clone)]
pub struct AgentSessionRuntimeHandle {
    pub session: Arc<dyn DaemonSessionHandle>,
    pub metadata: Value,
    pub model_fallback_message: Option<String>,
}

impl std::fmt::Debug for AgentSessionRuntimeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSessionRuntimeHandle")
            .field("metadata", &self.metadata)
            .finish()
    }
}

/// The session surface the daemon calls directly (core/agent-session.ts owns it).
pub trait DaemonSessionHandle: Send + Sync {
    fn session_id(&self) -> String;
    fn session_name(&self) -> Option<String>;
    fn session_file(&self) -> Option<String>;
    fn is_session_active(&self) -> bool;
    fn has_running_rlm_children(&self) -> bool;
    fn set_current_recap(&self, recap: Option<&str>);
    fn remove_queued_follow_up(&self, key: &str);
    fn set_session_name(&self, name: &str);
    fn register_rlm_child_session(&self, child_id: &str, session: Arc<dyn DaemonSessionHandle>) -> bool;
    fn subscribe(&self, listener: Arc<dyn Fn(&Value) + Send + Sync>) -> Box<dyn FnOnce() + Send>;
    fn session_manager(&self) -> Arc<std::sync::Mutex<SessionManager>>;
    fn dispose(&self) -> BoxFuture<'static, ()>;
}

/// `RuntimeOpenGuard = () => boolean | Promise<boolean>`.
pub type RuntimeOpenGuard = Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;

/// `SupervisorGenerationClaim` (the `worker_auth` body without id/type/token).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupervisorGenerationClaim {
    #[serde(rename = "supervisorGeneration")]
    pub supervisor_generation: String,
    #[serde(rename = "supervisorPid")]
    pub supervisor_pid: i64,
    #[serde(rename = "supervisorProcessStartId", skip_serializing_if = "Option::is_none", default)]
    pub supervisor_process_start_id: Option<String>,
    #[serde(rename = "supervisorSocketPath")]
    pub supervisor_socket_path: String,
}

/// `BoundSupervisorGenerationClaim`.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundSupervisorGenerationClaim {
    pub claim: SupervisorGenerationClaim,
    pub owner_fingerprint: String,
}

const PEER_GRANT_TTL_LIMIT_MS: u64 = 30_000;
// Only the supervisor registers grants, so the cap is a tripwire, never an eviction policy.
const PEER_GRANT_LIMIT: usize = 1024;

const RLM_SUBAGENT_REGISTRY_FILE: &str = "rlm-subagents.jsonl";

/// One passive child as the daemon presents it: topology (sessionFile, parent,
/// depth, name) from the spawn ledger; hydration metadata (prompt, spawnCode,
/// model, rlmMaxDepth, status, createdAt) from the per-child display file, or
/// the legacy registry for pre-ledger children without one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PassiveRlmSubagentEntry {
    pub child_id: String,
    pub session_name: String,
    pub session_dir: String,
    pub session_file: String,
    pub parent_session_id: String,
    pub parent_session_file: Option<String>,
    pub rlm_depth: Option<i64>,
    pub rlm_max_depth: Option<i64>,
    pub rlm_parent_node_id: Option<String>,
    pub prompt: Option<String>,
    pub spawn_code: Option<String>,
    pub model: Option<RlmSubagentModel>,
    pub status: String,
    pub created_at: f64,
}

/// `PassiveRlmRoot`: either a resident root parent state or a saved root info.
#[derive(Clone)]
pub enum PassiveRlmRoot {
    Resident(Arc<StdMutex<ActiveSessionState>>),
    Saved(SessionInfo),
}

/// `PassiveRlmSubagent`.
#[derive(Clone)]
pub struct PassiveRlmSubagent {
    pub root: PassiveRlmRoot,
    pub entry: PassiveRlmSubagentEntry,
    pub info: SessionInfo,
    pub chain: Vec<PassiveRlmSubagentEntry>,
}

/// Spread-ready optional metadata fields shared by display files and legacy
/// registry entries (`rlmSubagentMetadataFields`).
fn rlm_subagent_metadata_fields(source: &PassiveRlmSubagentEntry) -> PassiveRlmSubagentEntry {
    PassiveRlmSubagentEntry {
        rlm_max_depth: source.rlm_max_depth,
        rlm_parent_node_id: source.rlm_parent_node_id.clone().filter(|value| !value.is_empty()),
        prompt: source.prompt.clone().filter(|value| !value.is_empty()),
        spawn_code: source.spawn_code.clone().filter(|value| !value.is_empty()),
        model: source.model.clone(),
        ..PassiveRlmSubagentEntry::default()
    }
}

/// `DaemonHistoryWindow` / `DaemonHistoryRange` from daemon-protocol.ts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonHistoryRange {
    pub version: u32,
    pub generation: String,
    pub representation: String,
    #[serde(rename = "tipEntryId")]
    pub tip_entry_id: Option<String>,
    #[serde(rename = "totalMessageCount")]
    pub total_message_count: usize,
    #[serde(rename = "startIndex")]
    pub start_index: usize,
    pub messages: Vec<AgentMessage>,
    #[serde(rename = "entryIds")]
    pub entry_ids: Vec<String>,
    #[serde(rename = "hasOlder")]
    pub has_older: bool,
    pub order: String,
}

/// `DAEMON_COMMAND_TYPES` (ported verbatim from daemon-mode.ts).
pub const DAEMON_COMMAND_TYPES: [&str; 100] = [
    "ack_result", "list", "list_saved_sessions", "create", "attach", "detach", "kill", "rename", "prompt",
    "cancel_prompt_admission", "prompt_and_wait", "steer", "follow_up", "restore_next_turn", "restore_actions",
    "append_custom_message", "resume_queue", "send_message", "agent_messages_status", "agent_messages_pause",
    "agent_messages_resume", "agent_messages_clear", "abort", "start_side_question", "abort_side_question",
    "execute_bash", "execute_bash_and_wait", "abort_bash", "cancel_rlm_child", "delete_rlm_subagent",
    "wait_for_idle", "wait_for_headless_completion", "get_session_header", "get_state", "get_connection_state",
    "get_messages", "get_history_range", "get_rlm_children", "get_session_stats", "get_context_tree",
    "get_commands", "get_resource_snapshot", "replace_acp_mcp_servers", "get_model_catalog",
    "get_available_models", "get_queue", "mutate_queued_message", "clear_queue", "abort_and_clear_queue",
    "acquire_session_input_pause", "release_session_input_pause", "cron_list", "heartbeats_list",
    "heartbeat_manage", "cron_add", "cron_cancel", "heartbeat_get", "heartbeat_set", "heartbeat_update",
    "set_model", "cycle_model", "set_scoped_models", "set_thinking_level", "set_service_tier",
    "cycle_thinking_level", "set_transport", "set_steering_mode", "set_follow_up_mode", "set_auto_compaction",
    "set_auto_retry", "compact", "refine", "abort_compaction", "abort_branch_summary", "abort_retry", "reload",
    "new_session", "switch_session", "fork", "navigate_tree", "import_jsonl", "export_html", "export_jsonl",
    "set_session_name", "get_rlm_max_depth_status", "set_rlm_max_depth", "rename_saved_session",
    "delete_saved_session", "get_session_context", "get_session_tree", "get_user_messages_for_forking",
    "get_last_assistant_text", "get_system_prompt", "get_tool_definition", "set_session_entry_label",
    "extension_ui_response", "prepare_update_restart", "retry_worker", "restart", "shutdown",
];

const DAEMON_CLIENT_CAPABILITY_SET: [&str; 0] = [];
const CLIENT_CATCHUP_RETRY_MS: u64 = 250;
const UPDATE_RESTART_ABORT_BASH_TIMEOUT_MS: u64 = 5000;
const SUPERVISOR_FENCE_POLL_MS: u64 = 250;
const UPDATE_RESTART_MARKER: &str = "<prime_agent_update_interrupted>\nPrime Agent was updated and intentionally interrupted this session. Continue from the saved transcript and restored tool/kernel state. Any running model, tool, bash, or child-agent work may have been stopped.\n</prime_agent_update_interrupted>";

const RECOVERY_CHECKPOINT_EVENTS: [&str; 17] = [
    "agent_start", "agent_end", "turn_start", "turn_end", "message_start", "message_end", "tool_execution_start",
    "tool_execution_end", "compaction_start", "compaction_end", "auto_retry_start", "auto_retry_end", "bash_start",
    "bash_end", "session_action_update", "rlm_child_update",
];

// slice plumbing: `UPDATE_RESTART_DRAIN_COMMANDS` from daemon-protocol.ts.
const UPDATE_RESTART_DRAIN_COMMANDS: [&str; 8] = [
    "abort", "abort_bash", "abort_compaction", "abort_branch_summary", "abort_retry",
    "cancel_prompt_admission", "cancel_rlm_child", "shutdown",
];

/// `delay(ms)`.
async fn delay(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

fn contains(list: &[&str], value: &str) -> bool {
    list.iter().any(|entry| *entry == value)
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn now_millis() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn basename(path: &str, suffix: Option<&str>) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
    match suffix {
        Some(suffix) => name.strip_suffix(suffix).map(str::to_string).unwrap_or(name),
        None => name,
    }
}

fn dirname(path: &str) -> String {
    match path.rfind(['/', '\\']) {
        Some(index) if index > 0 => path[..index].to_string(),
        Some(_) => "/".to_string(),
        None => ".".to_string(),
    }
}

fn join_path(base: &str, name: &str) -> String {
    Path::new(base).join(name).to_string_lossy().to_string()
}

fn resolve_path(path: &str) -> String {
    crate::utils::daemon_socket_path::normalize_socket_path(path, None)
}

/// `sessionHistoryRepresentation(model)`.
pub fn session_history_representation(model: Option<&ModelIdentity>) -> String {
    let identity = match model {
        Some(model) => serde_json::json!([
            model.provider,
            model.id,
            model.api,
            model.base_url.trim_end_matches('/')
        ]),
        None => serde_json::json!([Value::Null, Value::Null, Value::Null, Value::Null]),
    };
    let digest = Sha256::digest(identity.to_string().as_bytes());
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    encoded.chars().take(22).collect()
}

/// The `Model<Api>` fields `sessionHistoryRepresentation` reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelIdentity {
    pub provider: String,
    pub id: String,
    pub api: String,
    pub base_url: String,
}

/// `slicePinnedSessionHistory(history, options)`.
pub fn slice_pinned_session_history(
    history: &SessionHistorySnapshot,
    options: &SlicePinnedSessionHistoryOptions,
) -> Result<DaemonHistoryRange, String> {
    if history.messages.len() != history.entry_ids.len() {
        return Err("Session history messages and entry ids are not aligned".to_string());
    }
    let mut end_index = history.messages.len();
    if let Some(before_entry_id) = &options.before_entry_id {
        match history.entry_ids.iter().position(|entry| entry == before_entry_id) {
            Some(index) => end_index = index,
            None => {
                return Err(format!("Session history boundary no longer exists: {before_entry_id}"));
            }
        }
    }
    let requested_limit = options.limit.unwrap_or(INITIAL_HISTORY_WINDOW_MESSAGES);
    if requested_limit == 0 || requested_limit > i32::MAX as usize {
        return Err("Session history range limit must be a positive integer".to_string());
    }
    let limit = requested_limit.min(MAX_HISTORY_RANGE_MESSAGES);
    let start_index = end_index.saturating_sub(limit);
    Ok(DaemonHistoryRange {
        version: 1,
        generation: options.generation.clone(),
        representation: options.representation.clone(),
        tip_entry_id: history.tip_entry_id.clone(),
        total_message_count: history.messages.len(),
        start_index,
        messages: history.messages[start_index..end_index].to_vec(),
        entry_ids: history.entry_ids[start_index..end_index].to_vec(),
        has_older: start_index > 0,
        order: "chronological".to_string(),
    })
}

/// `slicePinnedSessionHistory` options.
#[derive(Debug, Clone, Default)]
pub struct SlicePinnedSessionHistoryOptions {
    pub generation: String,
    pub representation: String,
    pub before_entry_id: Option<String>,
    pub limit: Option<usize>,
}

/// One outbound frame. The full `DaemonOutbound` union lives in
/// daemon-protocol.ts (another slice); the daemon carries the members its own
/// logic inspects plus `Raw` for pass-through frames whose payload another
/// module builds. Wire shape is unchanged: serde writes `type` plus the
/// camelCase fields.
#[derive(Debug, Clone, PartialEq)]
pub enum DaemonOutbound {
    SessionEvent { active_session_id: String, event: Value },
    SessionStatus { active_session_id: String, recap: Option<String> },
    SessionReplaced { active_session_id: String, state: Value, messages: Vec<Value> },
    SessionResynced { active_session_id: String, state: Value, messages: Vec<Value>, reason: Option<String> },
    SessionClosed { active_session_id: String, reason: String },
    ExtensionUiRequest { active_session_id: String, id: String, method: String, payload: Value },
    ExtensionError { active_session_id: String, extension_path: Option<String>, event: Option<String>, error: String },
    SessionStart { active_session_id: String },
    SessionAttached { active_session_id: String, client_id: String },
    SessionDetached { active_session_id: String, client_id: String },
    SessionSnapshotBegin {
        active_session_id: String,
        snapshot_id: String,
        message_count: usize,
        target_chunk_bytes: usize,
        transfer_id: Option<String>,
    },
    SessionSnapshotChunk { active_session_id: String, snapshot_id: String, index: usize, messages: Vec<Value> },
    SessionSnapshotEnd {
        active_session_id: String,
        snapshot_id: String,
        chunk_count: usize,
        bytes: usize,
        last_event_sequence: Option<u64>,
        last_event_cursor: Option<Value>,
        transfer_id: Option<String>,
    },
    SessionSnapshotFailed { active_session_id: String, snapshot_id: String, error: String },
    HeartbeatsChanged,
    RosterDelta { payload: Value },
    RosterHeartbeat,
    DaemonClosing { reason: String },
    /// A frame built elsewhere (roster frames, list progress, worker snapshots).
    Raw(Value),
}

impl DaemonOutbound {
    /// The wire `type` discriminator.
    pub fn type_name(&self) -> &str {
        match self {
            DaemonOutbound::SessionEvent { .. } => "session_event",
            DaemonOutbound::SessionStatus { .. } => "session_status",
            DaemonOutbound::SessionReplaced { .. } => "session_replaced",
            DaemonOutbound::SessionResynced { .. } => "session_resynced",
            DaemonOutbound::SessionClosed { .. } => "session_closed",
            DaemonOutbound::ExtensionUiRequest { .. } => "extension_ui_request",
            DaemonOutbound::ExtensionError { .. } => "extension_error",
            DaemonOutbound::SessionStart { .. } => "session_start",
            DaemonOutbound::SessionAttached { .. } => "session_attached",
            DaemonOutbound::SessionDetached { .. } => "session_detached",
            DaemonOutbound::SessionSnapshotBegin { .. } => "session_snapshot_begin",
            DaemonOutbound::SessionSnapshotChunk { .. } => "session_snapshot_chunk",
            DaemonOutbound::SessionSnapshotEnd { .. } => "session_snapshot_end",
            DaemonOutbound::SessionSnapshotFailed { .. } => "session_snapshot_failed",
            DaemonOutbound::HeartbeatsChanged => "heartbeats_changed",
            DaemonOutbound::RosterDelta { .. } => "roster_delta",
            DaemonOutbound::RosterHeartbeat => "roster_heartbeat",
            DaemonOutbound::DaemonClosing { .. } => "daemon_closing",
            DaemonOutbound::Raw(value) => value.get("type").and_then(Value::as_str).unwrap_or("unknown"),
        }
    }

    fn with_type(type_: &str) -> Map<String, Value> {
        let mut object = Map::new();
        object.insert("type".to_string(), Value::String(type_.to_string()));
        object
    }

    /// Serialize to the wire object.
    pub fn to_value(&self) -> Value {
        match self {
            DaemonOutbound::SessionEvent { active_session_id, event } => {
                let mut object = Self::with_type("session_event");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                object.insert("event".to_string(), event.clone());
                Value::Object(object)
            }
            DaemonOutbound::SessionStatus { active_session_id, recap } => {
                let mut object = Self::with_type("session_status");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                match recap {
                    Some(recap) => object.insert("recap".to_string(), Value::String(recap.clone())),
                    None => object.insert("recap".to_string(), Value::Null),
                };
                Value::Object(object)
            }
            DaemonOutbound::SessionReplaced { active_session_id, state, messages } => {
                let mut object = Self::with_type("session_replaced");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                object.insert("state".to_string(), state.clone());
                object.insert("messages".to_string(), Value::Array(messages.clone()));
                Value::Object(object)
            }
            DaemonOutbound::SessionResynced { active_session_id, state, messages, reason } => {
                let mut object = Self::with_type("session_resynced");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                object.insert("state".to_string(), state.clone());
                object.insert("messages".to_string(), Value::Array(messages.clone()));
                if let Some(reason) = reason {
                    object.insert("reason".to_string(), Value::String(reason.clone()));
                }
                Value::Object(object)
            }
            DaemonOutbound::SessionClosed { active_session_id, reason } => {
                let mut object = Self::with_type("session_closed");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                object.insert("reason".to_string(), Value::String(reason.clone()));
                Value::Object(object)
            }
            DaemonOutbound::ExtensionUiRequest { active_session_id, id, method, payload } => {
                let mut object = Self::with_type("extension_ui_request");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                object.insert("id".to_string(), Value::String(id.clone()));
                object.insert("method".to_string(), Value::String(method.clone()));
                object.insert("payload".to_string(), payload.clone());
                Value::Object(object)
            }
            DaemonOutbound::ExtensionError { active_session_id, extension_path, event, error } => {
                let mut object = Self::with_type("extension_error");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                if let Some(extension_path) = extension_path {
                    object.insert("extensionPath".to_string(), Value::String(extension_path.clone()));
                }
                if let Some(event) = event {
                    object.insert("event".to_string(), Value::String(event.clone()));
                }
                object.insert("error".to_string(), Value::String(error.clone()));
                Value::Object(object)
            }
            DaemonOutbound::SessionStart { active_session_id } => {
                let mut object = Self::with_type("session_start");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                Value::Object(object)
            }
            DaemonOutbound::SessionAttached { active_session_id, client_id } => {
                let mut object = Self::with_type("session_attached");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                object.insert("clientId".to_string(), Value::String(client_id.clone()));
                Value::Object(object)
            }
            DaemonOutbound::SessionDetached { active_session_id, client_id } => {
                let mut object = Self::with_type("session_detached");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                object.insert("clientId".to_string(), Value::String(client_id.clone()));
                Value::Object(object)
            }
            DaemonOutbound::SessionSnapshotBegin {
                active_session_id,
                snapshot_id,
                message_count,
                target_chunk_bytes,
                transfer_id,
            } => {
                let mut object = Self::with_type("session_snapshot_begin");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                object.insert("snapshotId".to_string(), Value::String(snapshot_id.clone()));
                object.insert("messageCount".to_string(), Value::from(*message_count as f64));
                object.insert("targetChunkBytes".to_string(), Value::from(*target_chunk_bytes as f64));
                if let Some(transfer_id) = transfer_id {
                    object.insert("transferId".to_string(), Value::String(transfer_id.clone()));
                }
                Value::Object(object)
            }
            DaemonOutbound::SessionSnapshotChunk { active_session_id, snapshot_id, index, messages } => {
                let mut object = Self::with_type("session_snapshot_chunk");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                object.insert("snapshotId".to_string(), Value::String(snapshot_id.clone()));
                object.insert("index".to_string(), Value::from(*index as f64));
                object.insert("messages".to_string(), Value::Array(messages.clone()));
                Value::Object(object)
            }
            DaemonOutbound::SessionSnapshotEnd {
                active_session_id,
                snapshot_id,
                chunk_count,
                bytes,
                last_event_sequence,
                last_event_cursor,
                transfer_id,
            } => {
                let mut object = Self::with_type("session_snapshot_end");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                object.insert("snapshotId".to_string(), Value::String(snapshot_id.clone()));
                object.insert("chunkCount".to_string(), Value::from(*chunk_count as f64));
                object.insert("bytes".to_string(), Value::from(*bytes as f64));
                if let Some(sequence) = last_event_sequence {
                    object.insert("lastEventSequence".to_string(), Value::from(*sequence as f64));
                }
                if let Some(cursor) = last_event_cursor {
                    object.insert("lastEventCursor".to_string(), cursor.clone());
                }
                if let Some(transfer_id) = transfer_id {
                    object.insert("transferId".to_string(), Value::String(transfer_id.clone()));
                }
                Value::Object(object)
            }
            DaemonOutbound::SessionSnapshotFailed { active_session_id, snapshot_id, error } => {
                let mut object = Self::with_type("session_snapshot_failed");
                object.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
                object.insert("snapshotId".to_string(), Value::String(snapshot_id.clone()));
                object.insert("error".to_string(), Value::String(error.clone()));
                Value::Object(object)
            }
            DaemonOutbound::HeartbeatsChanged => Value::Object(Self::with_type("heartbeats_changed")),
            DaemonOutbound::RosterDelta { payload } => payload.clone(),
            DaemonOutbound::RosterHeartbeat => Value::Object(Self::with_type("roster_heartbeat")),
            DaemonOutbound::DaemonClosing { reason } => {
                let mut object = Self::with_type("daemon_closing");
                object.insert("reason".to_string(), Value::String(reason.clone()));
                Value::Object(object)
            }
            DaemonOutbound::Raw(value) => value.clone(),
        }
    }

    /// `hasDaemonOutboundActiveSessionId`.
    pub fn active_session_id(&self) -> Option<&str> {
        match self {
            DaemonOutbound::SessionEvent { active_session_id, .. }
            | DaemonOutbound::SessionStatus { active_session_id, .. }
            | DaemonOutbound::SessionReplaced { active_session_id, .. }
            | DaemonOutbound::SessionResynced { active_session_id, .. }
            | DaemonOutbound::SessionClosed { active_session_id, .. }
            | DaemonOutbound::ExtensionUiRequest { active_session_id, .. }
            | DaemonOutbound::ExtensionError { active_session_id, .. }
            | DaemonOutbound::SessionStart { active_session_id }
            | DaemonOutbound::SessionAttached { active_session_id, .. }
            | DaemonOutbound::SessionDetached { active_session_id, .. }
            | DaemonOutbound::SessionSnapshotBegin { active_session_id, .. }
            | DaemonOutbound::SessionSnapshotChunk { active_session_id, .. }
            | DaemonOutbound::SessionSnapshotEnd { active_session_id, .. }
            | DaemonOutbound::SessionSnapshotFailed { active_session_id, .. } => Some(active_session_id),
            DaemonOutbound::HeartbeatsChanged
            | DaemonOutbound::RosterDelta { .. }
            | DaemonOutbound::RosterHeartbeat
            | DaemonOutbound::DaemonClosing { .. }
            | DaemonOutbound::Raw(_) => None,
        }
    }
}

/// The `DaemonSessionSnapshot` fields `snapshotTransferId` reads.
pub fn snapshot_transfer_id(snapshot: &Value) -> String {
    let cursor = snapshot.get("lastEventCursor").cloned().unwrap_or(Value::Null);
    let history_flavor = match snapshot.get("history") {
        Some(history) if !history.is_null() => format!(
            "history-{}-{}-{}",
            history.get("representation").and_then(Value::as_str).unwrap_or(""),
            history.get("tipEntryId").and_then(Value::as_str).unwrap_or("empty"),
            history.get("startIndex").and_then(Value::as_f64).unwrap_or(0.0)
        ),
        _ => "full".to_string(),
    };
    format!(
        "{}-{}-{}-{}",
        snapshot.get("activeSessionId").and_then(Value::as_str).unwrap_or(""),
        cursor.get("generation").and_then(Value::as_str).unwrap_or(""),
        cursor.get("sequence").and_then(Value::as_f64).unwrap_or(0.0),
        history_flavor
    )
}

/// `isSequencedSessionOutbound`.
pub fn is_sequenced_session_outbound(type_: &str) -> bool {
    matches!(
        type_,
        "session_event"
            | "session_status"
            | "session_replaced"
            | "session_resynced"
            | "session_closed"
            | "extension_ui_request"
            | "extension_error"
    )
}

/// `RuntimeOpenCancelledError` (a private daemon control-flow signal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOpenCancelledError;

impl std::fmt::Display for RuntimeOpenCancelledError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Runtime open cancelled")
    }
}

impl std::error::Error for RuntimeOpenCancelledError {}

/// `BoundSessionUnavailableError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundSessionUnavailableError {
    pub message: String,
}

impl BoundSessionUnavailableError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl std::fmt::Display for BoundSessionUnavailableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BoundSessionUnavailableError {}

/// `DaemonSessionClosedReason` from daemon-protocol.ts.
pub type DaemonSessionClosedReason = String;
/// `DaemonClosingReason` from daemon-protocol.ts.
pub type DaemonClosingReason = String;

const CLOSING_REASON_KILLED: &str = "killed";
const CLOSING_REASON_SHUTDOWN: &str = "shutdown";
const CLOSING_REASON_COMPLETED: &str = "completed";
const CLOSING_REASON_REPLACED: &str = "replaced";
const CLOSING_REASON_UPDATE: &str = "update";

/// Writes serialized lines to one client socket. `end()` mirrors `socket.end()`.
pub struct DaemonClientWriter {
    sender: mpsc::UnboundedSender<String>,
    closed: AtomicBool,
}

impl DaemonClientWriter {
    pub fn new(sender: mpsc::UnboundedSender<String>) -> Self {
        Self { sender, closed: AtomicBool::new(false) }
    }

    pub fn write(&self, line: String) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        self.sender.send(line).is_ok()
    }

    /// `socket.end()`.
    pub fn end(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// `socket.destroyed`.
    pub fn destroyed(&self) -> bool {
        self.closed.load(Ordering::SeqCst) || self.sender.is_closed()
    }
}

/// One attached socket client: the ported `DaemonSocketClient` state
/// (active-session-state.ts) plus the daemon-mode.ts fields that struct does not
/// carry yet (the socket, the in-flight catch-up, the snapshot transfer
/// controllers). Private plumbing: `active_session_state.rs` belongs to another
/// slice.
pub struct DaemonClientHandle {
    pub state: Arc<StdMutex<DaemonSocketClient>>,
    pub writer: Arc<DaemonClientWriter>,
    /// `client.catchupPromise` is running.
    pub catchup_running: bool,
    /// `client.catchupRetryTimer` is scheduled.
    pub catchup_retry_timer: bool,
    /// `client.snapshotTransferAbortControllers`.
    pub snapshot_transfer_abort_controllers: HashMap<String, tokio_util::sync::CancellationToken>,
    /// `client.snapshotTransferTails`.
    pub snapshot_transfer_tails: HashMap<String, u64>,
    /// `client.detachInput()`.
    pub detach_input: Arc<dyn Fn() + Send + Sync>,
    /// The private-frame decoder state for `transport === "private-framed"`.
    pub frame_decoder: Arc<StdMutex<PrivateFrameDecoder>>,
}

impl DaemonClientHandle {
    pub fn new(
        id: impl Into<String>,
        writer: Arc<DaemonClientWriter>,
        detach_input: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let state = Arc::new(StdMutex::new(DaemonSocketClient::new(id, false)));
        Self {
            state,
            writer,
            catchup_running: false,
            catchup_retry_timer: false,
            snapshot_transfer_abort_controllers: HashMap::new(),
            snapshot_transfer_tails: HashMap::new(),
            detach_input,
            frame_decoder: Arc::new(StdMutex::new(PrivateFrameDecoder::new())),
        }
    }

    pub fn id(&self) -> String {
        self.state.lock().expect("daemon client poisoned").id.clone()
    }

    pub fn set_id(&self, id: &str) {
        self.state.lock().expect("daemon client poisoned").id = id.to_string();
    }

    pub fn authenticated(&self) -> bool {
        self.state.lock().expect("daemon client poisoned").authenticated == Some(true)
    }

    pub fn set_authenticated(&self, role: &str) {
        let mut state = self.state.lock().expect("daemon client poisoned");
        state.authenticated = Some(true);
        state.authentication_role = Some(role.to_string());
    }

    /// The session-local capabilities of one attach (`daemonClientCapabilitiesForSession`).
    pub fn capabilities_for_session(&self, active_session_id: &str) -> HashSet<String> {
        let state = self.state.lock().expect("daemon client poisoned");
        state
            .capabilities_by_active_session_id
            .as_ref()
            .and_then(|map| map.get(active_session_id).cloned())
            .unwrap_or_else(|| state.capabilities.clone())
    }

    pub fn supports_extension_ui_for_session(&self, active_session_id: &str) -> bool {
        let state = self.state.lock().expect("daemon client poisoned");
        state
            .capabilities_by_active_session_id
            .as_ref()
            .and_then(|map| map.get(active_session_id).map(|value| value.contains("extension_ui")))
            .unwrap_or(state.supports_extension_ui)
    }

    pub fn snapshot_streaming(&self) -> bool {
        self.state.lock().expect("daemon client poisoned").snapshot_streaming == Some(true)
    }

    pub fn set_snapshot_streaming(&self, value: bool) {
        self.state.lock().expect("daemon client poisoned").snapshot_streaming = Some(value);
    }

    pub fn is_backpressured(&self) -> bool {
        self.state.lock().expect("daemon client poisoned").backpressured == Some(true)
    }

    pub fn set_backpressured(&self, value: bool) {
        self.state.lock().expect("daemon client poisoned").backpressured = Some(value);
    }

    pub fn attached_active_session_ids(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("daemon client poisoned")
            .attached_active_session_ids
            .iter()
            .cloned()
            .collect()
    }
}

impl std::fmt::Debug for DaemonClientHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonClientHandle").field("id", &self.id()).finish()
    }
}

/// `normalizeClientCapabilities`.
fn normalize_client_capabilities(
    capabilities: Option<&HashSet<String>>,
    supports_extension_ui: Option<bool>,
) -> HashSet<String> {
    let mut normalized: HashSet<String> = HashSet::new();
    let default_capabilities: HashSet<String> =
        DAEMON_DEFAULT_CLIENT_CAPABILITIES.iter().map(|value| value.to_string()).collect();
    let _ = DAEMON_CLIENT_CAPABILITY_SET;
    for capability in capabilities.unwrap_or(&default_capabilities) {
        if DAEMON_SUPPORTED_CLIENT_CAPABILITIES.contains(&capability.as_str()) {
            normalized.insert(capability.clone());
        }
    }
    if supports_extension_ui == Some(true) {
        normalized.insert("extension_ui".to_string());
    }
    normalized
}

/// `setDaemonClientSessionCapabilities`.
pub fn set_daemon_client_session_capabilities(
    client: &DaemonClientHandle,
    active_session_id: &str,
    capabilities: HashSet<String>,
) {
    let mut state = client.state.lock().expect("daemon client poisoned");
    let map = state.capabilities_by_active_session_id.get_or_insert_with(HashMap::new);
    map.insert(active_session_id.to_string(), capabilities);
    state.supports_extension_ui = map.values().any(|value| value.contains("extension_ui"));
}

/// `removeDaemonClientSessionCapabilities`.
fn remove_daemon_client_session_capabilities(client: &DaemonClientHandle, active_session_id: &str) {
    let mut state = client.state.lock().expect("daemon client poisoned");
    if let Some(map) = state.capabilities_by_active_session_id.as_mut() {
        map.remove(active_session_id);
    }
    let supports = state
        .capabilities_by_active_session_id
        .as_ref()
        .map(|map| map.values().any(|value| value.contains("extension_ui")))
        .unwrap_or(false);
    state.supports_extension_ui = supports;
}

/// `cancelPendingExtensionUiRequests`.
pub fn cancel_pending_extension_ui_requests(state: &mut ActiveSessionState) {
    let pending: Vec<ActiveSessionExtensionUiRequest> =
        state.extension_ui_requests.drain().map(|(_, value)| value).collect();
    for request in pending {
        (request.resolve)(DaemonExtensionUIResponse::Cancelled);
    }
}

/// `detachClientFromActiveSession`.
pub fn detach_client_from_active_session(client: &DaemonClientHandle, state: &mut ActiveSessionState) {
    state.clients.retain(|candidate| !Arc::ptr_eq(candidate, &client.state));
    client
        .state
        .lock()
        .expect("daemon client poisoned")
        .attached_active_session_ids
        .remove(&state.active_session_id);
    remove_daemon_client_session_capabilities(client, &state.active_session_id);
    if state.clients.is_empty() {
        cancel_pending_extension_ui_requests(state);
    }
}

/// `markClientSnapshotStreaming`; returns the abort token for the transfer.
pub fn mark_client_snapshot_streaming(
    client: &mut DaemonClientHandle,
    active_session_id: &str,
) -> tokio_util::sync::CancellationToken {
    client.set_snapshot_streaming(true);
    {
        let mut state = client.state.lock().expect("daemon client poisoned");
        state
            .snapshot_active_session_ids
            .get_or_insert_with(HashSet::new)
            .insert(active_session_id.to_string());
        let counts = state.snapshot_active_session_counts.get_or_insert_with(HashMap::new);
        let entry = counts.entry(active_session_id.to_string()).or_insert(0);
        *entry += 1;
    }
    let existing = client.snapshot_transfer_abort_controllers.get(active_session_id);
    if let Some(existing) = existing {
        if !existing.is_cancelled() {
            return existing.clone();
        }
    }
    let controller = tokio_util::sync::CancellationToken::new();
    client
        .snapshot_transfer_abort_controllers
        .insert(active_session_id.to_string(), controller.clone());
    controller
}

/// `abortClientSnapshotStreaming`.
pub fn abort_client_snapshot_streaming(client: &DaemonClientHandle, active_session_id: Option<&str>) {
    match active_session_id {
        Some(active_session_id) => {
            if let Some(controller) = client.snapshot_transfer_abort_controllers.get(active_session_id) {
                controller.cancel();
            }
        }
        None => {
            for controller in client.snapshot_transfer_abort_controllers.values() {
                controller.cancel();
            }
        }
    }
}

/// `finishClientSnapshotStreaming`.
pub fn finish_client_snapshot_streaming(client: &mut DaemonClientHandle, active_session_id: &str) {
    let count = {
        let state = client.state.lock().expect("daemon client poisoned");
        state
            .snapshot_active_session_counts
            .as_ref()
            .and_then(|map| map.get(active_session_id).copied())
            .unwrap_or(1)
    };
    let mut state = client.state.lock().expect("daemon client poisoned");
    if count > 1 {
        if let Some(map) = state.snapshot_active_session_counts.as_mut() {
            map.insert(active_session_id.to_string(), count - 1);
        }
    } else {
        if let Some(map) = state.snapshot_active_session_counts.as_mut() {
            map.remove(active_session_id);
        }
        if let Some(ids) = state.snapshot_active_session_ids.as_mut() {
            ids.remove(active_session_id);
        }
        drop(state);
        client.snapshot_transfer_abort_controllers.remove(active_session_id);
        client.snapshot_transfer_tails.remove(active_session_id);
        state = client.state.lock().expect("daemon client poisoned");
    }
    let streaming = state
        .snapshot_active_session_ids
        .as_ref()
        .map(|ids| !ids.is_empty())
        .unwrap_or(false);
    state.snapshot_streaming = Some(streaming);
    if !streaming {
        state.backpressured = Some(false);
    }
}

/// `shouldSendDaemonOutboundToClient`.
pub fn should_send_daemon_outbound_to_client(client: &DaemonClientHandle, message: &DaemonOutbound) -> bool {
    match message {
        DaemonOutbound::ExtensionUiRequest { active_session_id, method, .. } => {
            !is_daemon_dialog_extension_ui_request(method)
                || client.supports_extension_ui_for_session(active_session_id)
        }
        _ => true,
    }
}

/// `getChildActiveSessionStates`.
pub fn get_child_active_session_states(
    sessions: &HashMap<String, Arc<StdMutex<ActiveSessionState>>>,
    parent_state: &Arc<StdMutex<ActiveSessionState>>,
) -> Vec<Arc<StdMutex<ActiveSessionState>>> {
    let parent_active_session_id =
        parent_state.lock().expect("active session poisoned").active_session_id.clone();
    sessions
        .values()
        .filter(|state| {
            let state = state.lock().expect("active session poisoned");
            if state.active_session_id == parent_active_session_id {
                return false;
            }
            state
                .runtime
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.parent_active_session_id.as_deref())
                == Some(parent_active_session_id.as_str())
        })
        .cloned()
        .collect()
}

/// `resolveDaemonSessionPath`.
pub async fn resolve_daemon_session_path(
    selector: &str,
    cwd: &str,
    session_dir: Option<&str>,
) -> Result<String, String> {
    Ok(resolve_session_path(selector, cwd, session_dir)
        .await
        .map_err(|error| error.to_string())?
        .path)
}

/// `WorkerRosterReporterState`.
#[derive(Debug, Default)]
pub struct WorkerRosterReporterState {
    pub last_composed: HashMap<String, WorkerRosterEntry>,
    pub last_composed_json: HashMap<String, String>,
    pub queued_children: HashMap<String, WorkerRosterEntry>,
    /// Pending removals: agentId -> removed sessionId; a new incarnation of the id cancels it.
    pub removed_agent_ids: HashMap<String, Option<String>>,
    pub snapshot_pending: bool,
}

const ROSTER_SESSION_EVENT_TRIGGERS: [&str; 14] = [
    "turn_start",
    "turn_end",
    "bash_start",
    "bash_end",
    "compaction_start",
    "compaction_end",
    "auto_retry_start",
    "auto_retry_end",
    "tool_execution_start",
    "tool_execution_end",
    "message_end",
    "session_action_update",
    "session_info_changed",
    "thinking_level_changed",
];

/// `runDaemonMode(options)`.
pub async fn run_daemon_mode(options: DaemonModeOptions) -> Result<(), String> {
    let socket_path = normalize_socket_path(
        options.socket_path.clone().unwrap_or_else(default_daemon_socket_path),
        None,
    );
    let daemon = AgentDaemon::new(socket_path, options);
    daemon.start().await?;
    // `return new Promise(() => {})`: the daemon lives until the process exits.
    std::future::pending::<()>().await;
    Ok(())
}
