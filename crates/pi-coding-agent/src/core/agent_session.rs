//! Port of packages/coding-agent/src/core/agent-session.ts
//!
//! This is the largest hand-written file in the package. The port keeps the
//! TypeScript structure: public types and helpers first, then the `AgentSession`
//! class as a single `impl AgentSession` block with `snake_case` method names.
//!
//! Cross-slice dependencies are represented as local plumbing in this module so
//! that no other slice's file is touched (see `blocked_on` in the status file):
//!   - core/agent-session-config.ts     -> `AgentSessionConfig` (local stand-in)
//!   - core/rlm-runtime.ts              -> `RlmSubagentRuntime`/`SubagentRuntimeHost` seam
//!   - core/rlm-continuation.ts         -> `RlmPendingContinuation`/`RlmPendingResult` seam
//!   - core/cron-jobs.ts                -> `AgentCronJob`/`AgentRlmHeartbeatController` seam
//!   - core/resource-loader.ts          -> `ResourceLoader` seam
//!   - core/mcp/mcp-manager.ts          -> `McpManager` seam
//!   - core/extensions/*                -> `ExtensionRunner` seam
//!   - core/tools/index.ts              -> tool definitions are built through the
//!     same `create_all_tool_definitions` entry point.
//!
//! `Agent` is `pi_agent_core::agent::Agent`, which is not part of this slice, so
//! the session drives the agent through the private `AgentHandle` seam below.

#![allow(clippy::too_many_arguments)]

#[path = "agent_session/runtime_members.rs"]
mod runtime_members;
#[path = "agent_session/agent_handle.rs"]
mod agent_handle;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use pi_agent_core::types::{
    AgentContext, AgentEvent, AgentMessage, AgentState, AgentTool, CustomAgentMessage,
    CustomMessageContent, ThinkingLevel,
};
use pi_ai::types::{
    AssistantMessage, ImageContent, Message, Model, ServiceTier, StopReason, TextContent, Usage,
    UserContent, UserMessage, STOP_REASON_ABORTED, STOP_REASON_ERROR, STOP_REASON_STOP,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::core::extensions::types::{
    AgentEndPayload, ExtensionError, ExtensionEvent, MessageStartPayload, MessageUpdatePayload,
    ModelSelectPayload, ToolExecutionEndPayload, ToolExecutionStartPayload,
    ToolExecutionUpdatePayload, TurnEndPayload, TurnStartPayload,
};
use crate::core::agent_messages::{
    AGENT_MESSAGE_CUSTOM_TYPE,
    AGENT_MESSAGE_RECEIVED_PREVIEW_LABEL,
    AgentFamilyCatalogEntry,
    AgentSessionMessageListResult,
    AgentSessionMessageReceipt,
    AgentSessionNameAvailabilityInput,
    AgentSessionNameScope,
    DEFAULT_AGENT_MESSAGE_MAX_PENDING_PER_SESSION,
    assert_agent_message_queue_capacity,
    assert_agent_session_name_available,
    assert_direct_agent_message_target,
    create_agent_message_host_handlers,
    format_agent_session_name_unavailable,
    is_agent_session_message,
    is_agent_session_message_prompt,
    normalize_agent_session_message,
    parse_agent_session_message_prompt_id,
    starts_agent_run,
};
use crate::core::agent_observe::{
    create_agent_observe_host_handlers, normalize_observe_limit, normalize_observe_max_chars,
    AgentObserveAgentSnapshot, AgentObserveController, AgentObserveListResult,
    AgentObserveRecentMessagesResult, AGENT_OBSERVE_SKILL_NAME, ORCHESTRATION_HEARTBEAT_SKILL_NAME,
};
use crate::core::auth_guidance::{
    add_login_guidance_to_auth_error, format_authentication_failed_message,
    format_no_api_key_found_message, format_no_model_selected_message,
    is_likely_authentication_error,
};
use crate::core::auth_storage::AuthSourceToken;
use crate::core::kernel::shared::HostRequestHandlers;
use crate::core::kernel::state_snapshot::RestoreResult;
pub use crate::core::resource_loader::{ResourceLoader, ResourceExtensionPaths};
pub use crate::core::rlm_runtime::{SubagentRuntimeHost, RlmSubagentRuntime, RlmSpawnHandle, CreateRlmSubagentRuntimeOptions, RlmSubagentRegistryEntry, RlmCreateSessionResult, RlmDeleteSubagentResult, RlmFindModelsResult, RlmListSubagentsResult, ScopedModelEntry as ScopedModel};
pub use crate::core::bash_executor::BashResult;
use crate::core::compaction::branch_summarization::{
    collect_entries_for_branch_summary, generate_branch_summary, prepare_branch_entries,
    BranchSummaryResult, GenerateBranchSummaryOptions, ReadonlySessionManager,
};
use crate::core::compaction::checkpoint::has_provider_checkpoint;
use crate::core::compaction::compaction::{
    calculate_context_tokens, compact, default_compaction_settings, estimate_context_tokens,
    estimate_tokens, prepare_compaction, should_compact_for_model, CompactionPreparation,
    CompactionResult, CompactionSessionEntry, CompactionSettings, COMPACT_SKILL_NAME,
};
use crate::core::compaction::utils::serialize_conversation;
use crate::core::context_tree::{
    compute_own_and_total_usage, load_context_tree_child_from_disk,
    load_context_tree_children_from_disk, ContextTreeNode, ContextUsage, ContextWindowResolver,
};
use crate::core::goals::{
    create_goal_context_message, empty_goal_state, goal_host_response, goal_token_delta_for_usage,
    is_persisted_goal_state, normalize_goal_state, validate_goal_budget, validate_goal_objective,
    GoalContextKind, GoalHostResponse, GoalState, GoalStatus, GOAL_CONTEXT_CUSTOM_TYPE,
    GOAL_CONTEXT_PREVIEW_LABEL,
    GOAL_SKILL_NAME, GOAL_STATE_CUSTOM_TYPE,
};
use crate::core::messages::{
    ASYNC_BASH_COMPLETION_CUSTOM_TYPE,
    REFINEMENT_SOURCE_AUTO,
    ASYNC_BASH_COMPLETION_PREVIEW_LABEL,
    AsyncBashCompletionDetails,
    BashExecutionMessage,
    CompactionOutcome,
    CompactionOutcomeDetails,
    CompactionOutcomeReason,
    CustomMessage,
    HARNESS_DIGEST_CUSTOM_TYPE,
    HEARTBEAT_PROMPT_CUSTOM_TYPE,
    HEARTBEAT_PROMPT_PREVIEW_LABEL,
    HarnessDigestDetails,
    IPYTHON_STATE_RESTORED_CUSTOM_TYPE,
    REFINEMENT_SOURCE_SELF,
    RLM_CHILD_FAILURE_CUSTOM_TYPE,
    RLM_CHILD_TERMINAL_NOTICE_CUSTOM_TYPE,
    RefinementSource,
    RlmChildFailureDetails,
    RlmChildTerminalNoticeDetails,
    SESSION_SLASH_COMMAND_CUSTOM_TYPE,
    SESSION_SLASH_COMMAND_RESULT_CUSTOM_TYPE,
    SessionSlashCommandResultDetails,
    convert_to_llm,
    create_async_bash_completion_message,
    create_compaction_outcome_message,
    create_custom_message,
    create_harness_digest_message,
    create_heartbeat_prompt_message,
    create_refinement_notice_message,
    create_refinement_outcome_message,
    create_rlm_child_failure_message,
    create_rlm_child_terminal_notice_message,
    create_session_slash_command_message,
    create_session_slash_command_result_message,
    is_compaction_outcome_message,
    is_session_slash_command,
    is_session_slash_command_message,
    without_harness_digests_for_compaction,
};
use crate::core::model_tool_output_policy::{
    apply_model_tool_output_policy, ModelToolOutputPolicy, ModelToolOutputPolicyOptions,
    ModelToolOutputScope,
};
use crate::core::prompt_templates::{expand_prompt_template, PromptTemplate};
use crate::core::refinement::refinement::{
    append_global_refinement, apply_refinement_proposal, format_harness_state_for_prompt,
    generate_refinement_id, get_global_harness_state_dir, get_local_harness_state_dir,
    get_refinement_history, infer_refinement_result_scope, load_global_refinement_history,
    load_harness_state, merge_harness_states, merge_refinement_history,
    normalize_refinement_proposal, plan_refinement, review_auto_refine, save_harness_state,
    ApplyRefinementOptions, AutoRefineReason, AutoRefineReview, AutoRefineReviewContext,
    AutoRefineReviewer, CompletionFn, HarnessScope, HarnessState, PlanRefinementRequest,
    ProviderRetryPolicy, RefineModel, RefinementCompletionRequest, RefinementFailureError,
    RefinementPlan, RefinementProposal, RefinementResult, RefineOptions, ReviewAutoRefineRequest,
    REFINE_SKILL_NAME, REFINEMENT_CUSTOM_TYPE, REFINEMENT_FAILURE_CUSTOM_TYPE,
};
use crate::core::session_action_store::TransitionOptions;
use crate::core::session_action_store::{
    ActionLifecycle,
    ActionStore,
    ActionTicket,
    DeliveryMessage,
    DeliveryPolicy,
    DeliveryRecord,
    DeliveryRecordRole,
    QueuedMessageLane,
    QueuedMessageMutation,
    QueuedMessageMutationStatus,
    RollbackProof,
    RuntimeActivity,
    SessionAction,
    SessionActionPayload,
    SessionActionPhase,
    SessionActionSnapshot,
    SessionActionSnapshotActive,
    SessionActionSnapshotKind,
    SessionCommandPayload,
    SessionTurnPayload,
    WakePolicy,
    ActionLifecycleState,
    DeliveryOutcome,
    can_select_session_action,
    queued_message_lane_delivery_policy,
    transition_session_action,
};
use crate::core::session_manager::{
    get_latest_compaction_entry, SessionContext, SessionEntry, SessionManager,
    CURRENT_SESSION_VERSION,
};
use crate::core::session_stats::SessionStats;
use crate::core::slash_commands::{
    parse_refine_command_options, parse_session_slash_command, parse_slash_command,
    SessionSlashCommand, SlashCommandInfo,
};
use crate::core::source_info::{create_synthetic_source_info, SourceInfo};
use crate::core::system_prompt::{build_system_prompt, BuildSystemPromptOptions};
use crate::core::usage::{
    add_assistant_usage, clone_usage, empty_usage, session_usage_summary_from,
    subtract_assistant_usage, SessionUsageSummary,
};
use crate::core::websearch_credential::{SERPER_CREDENTIAL_ID, SERPER_ENV_VAR, WEBSEARCH_SKILL_NAME};
use crate::core::cron_jobs::normalize_heartbeat_delivery_mode;
use crate::modes::agent_connection::daemon_agent_connection::now_iso;
use pi_ai::models::get_supported_thinking_levels;

// ---------------------------------------------------------------------------
// Private plumbing for cross-slice seams
//
// Every item below replaces an import from another slice. Names follow the
// TypeScript member names so the mapping stays recognisable.
// ---------------------------------------------------------------------------

pub use pi_agent_core::types::{
    AfterToolCallContext, AfterToolCallResult, BeforeToolCallContext, BeforeToolCallResult,
    GetContinuationMessagesContext, ShouldStopAfterTurnContext,
};
pub use pi_agent_core::performance_metrics::{AgentLoopPerformanceMetrics, PerformanceMetricRecorder};

pub type BeforeToolCallHook = Arc<dyn Fn(BeforeToolCallContext, Option<CancellationToken>) -> BoxFuture<Result<Option<BeforeToolCallResult>, String>> + Send + Sync>;
pub type AfterToolCallHook = Arc<dyn Fn(AfterToolCallContext, Option<CancellationToken>) -> BoxFuture<Result<Option<AfterToolCallResult>, String>> + Send + Sync>;
pub type GetContinuationMessagesHook = Arc<dyn Fn(GetContinuationMessagesContext, Option<CancellationToken>) -> BoxFuture<Vec<AgentMessage>> + Send + Sync>;

pub trait AgentHandle: Send + Sync {
    fn state(&self) -> AgentState;
    fn set_state(&self, state: AgentState);
    fn subscribe(&self, listener: Arc<dyn Fn(AgentEvent, Option<CancellationToken>) -> BoxFuture<()> + Send + Sync>) -> Box<dyn Fn() + Send + Sync>;
    fn set_before_tool_call(&self, hook: BeforeToolCallHook);
    fn set_after_tool_call(&self, hook: AfterToolCallHook);
    fn set_get_continuation_messages(&self, hook: GetContinuationMessagesHook);
    fn set_should_stop_before_turn(&self, hook: Arc<dyn Fn() -> bool + Send + Sync>);
    fn set_should_stop_after_turn(&self, hook: Arc<dyn Fn(ShouldStopAfterTurnContext) -> BoxFuture<bool> + Send + Sync>);
    fn set_stream_fn(&self, stream_fn: pi_agent_core::types::StreamFn);
    fn stream_fn(&self) -> pi_agent_core::types::StreamFn;
    fn abort(&self);
    fn wait_for_idle(&self) -> BoxFuture<()>;
    fn prompt(&self, messages: Vec<AgentMessage>) -> BoxFuture<Result<(), String>>;
    fn continue_(&self) -> BoxFuture<Result<(), String>>;
    fn is_streaming(&self) -> bool;
    fn has_queued_messages(&self) -> bool;
    fn clear_all_queues(&self);
    fn remove_queued_messages(&self, predicate: Arc<dyn Fn(&AgentMessage) -> bool + Send + Sync>) -> Vec<AgentMessage>;
    fn follow_up(&self, message: AgentMessage);
    fn set_follow_up_mode(&self, mode: String);
    fn set_steering_mode(&self, mode: String);
    fn set_convert_to_llm(&self, convert: Arc<dyn Fn(Vec<AgentMessage>) -> BoxFuture<Vec<Message>> + Send + Sync>);
    fn set_transform_context(&self, transform: Arc<dyn Fn(Vec<AgentMessage>, Option<CancellationToken>) -> BoxFuture<Vec<AgentMessage>> + Send + Sync>);
    fn set_get_api_key(&self, get_api_key: Arc<dyn Fn(String) -> BoxFuture<Option<String>> + Send + Sync>);
    fn set_on_payload(&self, hook: pi_ai::types::OnPayload);
    fn set_on_response(&self, hook: pi_ai::types::OnResponse);
    fn set_tool_execution(&self, mode: String);
    fn performance_metrics(&self) -> Option<AgentLoopPerformanceMetrics>;
    fn set_performance_metrics(&self, metrics: Option<AgentLoopPerformanceMetrics>);
    fn signal(&self) -> Option<CancellationToken>;
}

pub type BoxFuture<T> = pi_ai::types::BoxFuture<T>;

/// `PerformanceMetricUsageV1` - the subset the session reports for compaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceMetricUsageV1 {
    pub source: String,
    pub input_tokens: Option<f64>,
    pub cached_input_tokens: Option<f64>,
    pub output_tokens: Option<f64>,
    pub reasoning_tokens: Option<f64>,
    pub total_tokens: Option<f64>,
    pub cached_input_included_in_input: Option<bool>,
    pub reasoning_included_in_output: Option<bool>,
}


/// `ExtensionRunner` - the session holds the runner behind an `Arc`.
pub type ExtensionRunner = Arc<crate::core::extensions::runner::ExtensionRunner>;

/// `hookResult` from `ExtensionRunner.emitToolResult`.
#[derive(Debug, Clone)]
pub struct ToolResultHookResult {
    pub content: Vec<pi_agent_core::types::ContentBlock>,
    pub details: Value,
    pub is_error: Option<bool>,
}

/// `Awaited<ReturnType<ExtensionRunner["emitBeforeAgentStart"]>>`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BeforeAgentStartResult {
    pub prompt: Option<String>,
    pub images: Option<Vec<ImageContent>>,
    pub system_prompt: Option<String>,
}

/// `getAllRegisteredTools()` entry.
#[derive(Debug, Clone)]
pub struct RegisteredExtensionTool {
    pub definition: crate::core::extensions::types::ToolDefinition,
}

/// `RegisteredCommand` shape used by the session.
#[derive(Clone, Default)]
pub struct ExtensionCommand {
    pub name: String,
    pub description: Option<String>,
    pub source_info: Option<SourceInfo>,
    pub handler: Option<Arc<dyn Fn(String) -> BoxFuture<Result<(), String>> + Send + Sync>>,
}

/// `ExtensionRunnerRef` from `AgentSessionConfig`.
#[derive(Default)]
pub struct ExtensionRunnerRef {
    current: Mutex<Option<ExtensionRunner>>,
}

impl ExtensionRunnerRef {
    pub fn new(runner: Option<ExtensionRunner>) -> Self {
        Self {
            current: Mutex::new(runner),
        }
    }

    pub fn current(&self) -> Option<ExtensionRunner> {
        self.current.lock().unwrap().clone()
    }

    pub fn set(&self, runner: Option<ExtensionRunner>) {
        *self.current.lock().unwrap() = runner;
    }
}

pub use crate::core::mcp::mcp_manager::McpManager;

/// `AgentRlmHeartbeatController` from `core/cron-jobs.ts` (another slice).
pub trait AgentRlmHeartbeatController: Send + Sync {
    fn list(&self) -> Vec<AgentCronJob>;
    fn create(&self, payload: Value) -> Result<Value, String>;
    fn update(&self, job_id: &str, payload: Value) -> Result<Value, String>;
    fn delete(&self, job_id: &str) -> Result<Value, String>;
}

/// `AgentCronJob` from `core/cron-jobs.ts`.
#[derive(Debug, Clone, Default)]
pub struct AgentCronJob {
    pub id: String,
    pub schedule: String,
    pub prompt: String,
    pub status: String,
    pub run_count: f64,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_error: Option<String>,
}

/// `AgentRlmHeartbeatStatusUpdate` from `core/cron-jobs.ts`.
#[derive(Debug, Clone, Default)]
pub struct AgentRlmHeartbeatStatusUpdate {
    pub job_id: String,
    pub schedule: String,
    pub status: String,
    pub run_count: f64,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_error: Option<String>,
}

/// `AgentSessionMessageController` from `core/agent-messages.ts` (another slice).
pub trait AgentSessionMessageController: Send + Sync {
    fn roster(&self) -> BoxFuture<Result<AgentSessionMessageListResult, String>>;
    fn deliver(&self, payload: Value) -> BoxFuture<Result<Value, String>>;
}

/// `RlmPendingContinuation` / `RlmPendingResult` / `RlmTerminalStatus` from
/// `core/rlm-continuation.ts` (another slice).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RlmPendingContinuation {
    pub task_id: String,
    pub child_id: String,
    pub session_name: String,
    pub prompt: String,
    pub continuations_used: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RlmPendingResult {
    pub task_id: String,
    pub child_id: String,
    pub session_name: String,
    pub status: String,
    pub answer: Option<String>,
    pub error: Option<String>,
}

pub type RlmTerminalStatus = String;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RlmContinuationState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_continuation: Option<RlmPendingContinuation>,
    #[serde(default, rename = "pendingResults")]
    pub pending_results: Vec<RlmPendingResult>,
}

pub const RLM_CHILD_MAX_CONTINUATIONS: f64 = 3.0;
pub const RLM_CONTINUATION_STATE_CUSTOM_TYPE: &str = "rlm_continuation_state";
pub const LEGACY_RLM_CONTINUATION_STATE_CUSTOM_TYPE: &str = "legacy_rlm_continuation_state";

pub fn empty_rlm_continuation_state() -> RlmContinuationState {
    RlmContinuationState::default()
}

pub fn parse_rlm_continuation_state(value: &Value) -> Option<RlmContinuationState> {
    serde_json::from_value(value.clone()).ok()
}

pub fn parse_legacy_rlm_continuation_state(value: &Value) -> Option<RlmContinuationState> {
    parse_rlm_continuation_state(value)
}

pub fn classify_rlm_child_terminal(status: &str) -> Option<RlmTerminalStatus> {
    match status {
        "done" | "error" | "cancelled" => Some(status.to_string()),
        _ => None,
    }
}

pub fn create_rlm_child_continuation_message(continuation: &RlmPendingContinuation) -> UserMessage {
    UserMessage::new(
        UserContent::Text(format!(
            "Child agent {} has not reported a result yet. Continue the task.",
            continuation.child_id
        )),
        0,
    )
}

pub fn bounded_rlm_visible_text(text: &str, max_length: usize) -> String {
    let compact: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_length {
        return compact;
    }
    let keep = max_length.saturating_sub(3);
    let truncated: String = compact.chars().take(keep).collect();
    format!("{}...", truncated.trim_end())
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// `RlmChildAgentStatus`.
pub type RlmChildAgentStatus = String;
pub const RLM_CHILD_AGENT_STATUS_QUEUED: &str = "queued";
pub const RLM_CHILD_AGENT_STATUS_RUNNING: &str = "running";
pub const RLM_CHILD_AGENT_STATUS_DONE: &str = "done";
pub const RLM_CHILD_AGENT_STATUS_ERROR: &str = "error";
pub const RLM_CHILD_AGENT_STATUS_CANCELLED: &str = "cancelled";

/// `RlmChildAgentActivity`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RlmChildAgentActivity {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

/// `RlmChildAgentSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RlmChildAgentSnapshot {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub label: String,
    pub status: RlmChildAgentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_count: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recap: Option<String>,
    pub session_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<RlmChildAgentActivity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replied_since_task: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `CompactionReason`.
pub type CompactionReason = String;
pub const COMPACTION_REASON_MANUAL: &str = "manual";
pub const COMPACTION_REASON_THRESHOLD: &str = "threshold";
pub const COMPACTION_REASON_OVERFLOW: &str = "overflow";
pub const COMPACTION_REASON_REQUESTED: &str = "requested";

/// `AgentSessionEvent` - a tagged union mirroring the TypeScript union members.
#[derive(Debug, Clone)]
pub enum AgentSessionEvent {
    /// Any `AgentEvent` from pi-agent-core, forwarded unchanged.
    Agent(AgentEvent),
    IpythonSentAgentMessage {
        tool_call_id: String,
        message: Value,
    },
    SessionActionUpdate {
        actions: SessionActionSnapshot,
    },
    CompactionStart {
        reason: CompactionReason,
        custom_instructions: Option<String>,
    },
    SessionInfoChanged {
        /// `name: string | undefined` - `None` is `undefined`.
        name: Option<String>,
    },
    MessageStart {
        message: AgentMessage,
    },
    MessageEnd {
        message: AgentMessage,
    },
    ModelSelect {
        model: String,
        previous_model: String,
        reason: String,
    },
    ThinkingLevelChange {
        level: ThinkingLevel,
    },
    ServiceTierChange {
        service_tier: ServiceTier,
    },
    CompactionUpdate {
        active: bool,
        reason: Option<String>,
    },
    RetryUpdate {
        active: bool,
        attempt: i64,
        max_attempts: i64,
        message: Option<String>,
    },
    RefinementUpdate {
        active: bool,
        reason: Option<String>,
    },
    TreeNavigated {
        target_id: String,
    },
    RlmSubagentRemoved {
        child_id: String,
        session_name: Option<String>,
    },
    ThinkingLevelChanged {
        level: ThinkingLevel,
    },
    ServiceTierChanged {
        service_tier: ServiceTier,
    },
    CompactionEnd {
        reason: CompactionReason,
        result: Option<CompactionResult>,
        aborted: bool,
        will_retry: bool,
        error_message: Option<String>,
        error_severity: Option<String>,
        custom_instructions: Option<String>,
    },
    AutoRetryStart {
        attempt: i64,
        max_attempts: i64,
        delay_ms: f64,
        error_message: String,
    },
    AutoRetryEnd {
        success: bool,
        attempt: i64,
        final_error: Option<String>,
    },
    AuthStale {
        provider: String,
        source_tokens: Option<Vec<AuthSourceToken>>,
    },
    RlmChildUpdate {
        child: RlmChildAgentSnapshot,
    },
    RecapUpdate {
        /// `recap: string | undefined`
        recap: Option<String>,
    },
    GoalUpdate {
        goal: GoalState,
    },
    BashStart {
        command: String,
        exclude_from_context: bool,
        transient: Option<bool>,
        run_id: Option<String>,
    },
    BashOutput {
        chunk: String,
    },
    BashEnd {
        exit_code: Option<i64>,
        cancelled: bool,
        truncated: bool,
        full_output_path: Option<String>,
        error_message: Option<String>,
        transient: Option<bool>,
        run_id: Option<String>,
    },
    RefineComplete {
        result: RefinementResult,
    },
    RefineFailed {
        error: String,
    },
}

impl AgentSessionEvent {
    /// `event.type` in the TypeScript union.
    pub fn type_name(&self) -> &'static str {
        match self {
            AgentSessionEvent::Agent(event) => event.type_name(),
            AgentSessionEvent::IpythonSentAgentMessage { .. } => "ipython_sent_agent_message",
            AgentSessionEvent::SessionActionUpdate { .. } => "session_action_update",
            AgentSessionEvent::CompactionStart { .. } => "compaction_start",
            AgentSessionEvent::SessionInfoChanged { .. } => "session_info_changed",
            AgentSessionEvent::MessageStart { .. } => "message_start",
            AgentSessionEvent::MessageEnd { .. } => "message_end",
            AgentSessionEvent::ModelSelect { .. } => "model_select",
            AgentSessionEvent::ThinkingLevelChange { .. } => "thinking_level_change",
            AgentSessionEvent::ServiceTierChange { .. } => "service_tier_change",
            AgentSessionEvent::CompactionUpdate { .. } => "compaction_update",
            AgentSessionEvent::RetryUpdate { .. } => "auto_retry_update",
            AgentSessionEvent::RefinementUpdate { .. } => "refinement_update",
            AgentSessionEvent::TreeNavigated { .. } => "tree_navigated",
            AgentSessionEvent::RlmSubagentRemoved { .. } => "rlm_subagent_removed",
            AgentSessionEvent::ThinkingLevelChanged { .. } => "thinking_level_changed",
            AgentSessionEvent::ServiceTierChanged { .. } => "service_tier_changed",
            AgentSessionEvent::CompactionEnd { .. } => "compaction_end",
            AgentSessionEvent::AutoRetryStart { .. } => "auto_retry_start",
            AgentSessionEvent::AutoRetryEnd { .. } => "auto_retry_end",
            AgentSessionEvent::AuthStale { .. } => "auth_stale",
            AgentSessionEvent::RlmChildUpdate { .. } => "rlm_child_update",
            AgentSessionEvent::RecapUpdate { .. } => "recap_update",
            AgentSessionEvent::GoalUpdate { .. } => "goal_update",
            AgentSessionEvent::BashStart { .. } => "bash_start",
            AgentSessionEvent::BashOutput { .. } => "bash_output",
            AgentSessionEvent::BashEnd { .. } => "bash_end",
            AgentSessionEvent::RefineComplete { .. } => "refine_complete",
            AgentSessionEvent::RefineFailed { .. } => "refine_failed",
        }
    }
}

pub type AgentSessionEventListener = Arc<dyn Fn(AgentSessionEvent) + Send + Sync>;

/// `UserBashEndDetails`.
#[derive(Debug, Clone, Default)]
pub struct UserBashEndDetails {
    pub exit_code: Option<i64>,
    pub cancelled: bool,
    pub truncated: bool,
    pub full_output_path: Option<String>,
    pub error_message: Option<String>,
}

/// `CompactionSkippedError extends Error`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionSkippedError {
    pub message: String,
}

impl CompactionSkippedError {
    pub fn new() -> Self {
        Self {
            message: String::new(),
        }
    }
}

impl Default for CompactionSkippedError {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for CompactionSkippedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for CompactionSkippedError {}

/// `RefineSkippedError extends Error`.
#[derive(Debug, Clone, PartialEq)]
pub struct RefineSkippedError {
    pub message: String,
}

impl RefineSkippedError {
    pub fn new() -> Self {
        Self {
            message: String::new(),
        }
    }
}

impl Default for RefineSkippedError {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RefineSkippedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for RefineSkippedError {}

/// `ExtensionBindings`.
#[derive(Default)]
pub struct ExtensionBindings {
    pub ui_context: Option<Value>,
    pub command_context_actions: Option<Value>,
    pub shutdown_handler: Option<Arc<dyn Fn(Value) -> BoxFuture<()> + Send + Sync>>,
    pub on_error: Option<Arc<dyn Fn(Value) + Send + Sync>>,
}

/// `AutoRefineReviewRequest`.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoRefineReviewRequest {
    pub reason: AutoRefineReason,
    pub turns_since_last_review: i64,
}

/// `SerializedBackgroundPlanResult`.
#[derive(Debug, Clone)]
pub enum SerializedBackgroundPlanResult {
    Plan {
        plan: RefinementPlan,
        options: RefineOptions,
        abort: CancellationToken,
        branch_version: i64,
        source: RefinementSource,
    },
    Skip {
        explicit: Option<bool>,
    },
    Invalidated {
        branch_version: i64,
    },
    Failure {
        explicit: bool,
        options: RefineOptions,
        branch_version: i64,
    },
}

pub type AutoRefineReviewer = Arc<
    dyn Fn(AutoRefineReviewRequest, Option<CancellationToken>) -> BoxFuture<Result<AutoRefineReview, String>>
        + Send
        + Sync,
>;

/// `PromptOptions`.
#[derive(Clone, Default)]
pub struct PromptOptions {
    pub expand_prompt_templates: Option<bool>,
    pub images: Option<Vec<ImageContent>>,
    /// `streamingBehavior?: "steer" | "followUp"`
    pub streaming_behavior: Option<String>,
    pub follow_up_queue_key: Option<String>,
    pub source: Option<crate::core::session_action_store::InputSource>,
    pub preflight_result: Option<Arc<dyn Fn(bool, bool) + Send + Sync>>,
    pub queue_if_busy: Option<bool>,
    pub resume_if_idle: Option<bool>,
    pub internal_prompt: Option<bool>,
    pub suppress_autonomous_continuation: Option<bool>,
    pub skip_input_handlers: Option<bool>,
    pub signal: Option<CancellationToken>,
    pub admission_committed: Option<Arc<dyn Fn() + Send + Sync>>,
    pub agent_message_id: Option<String>,
    pub content: Option<Vec<pi_ai::types::ImageOrTextContent>>,
    pub custom_message: Option<CustomMessage>,
}

/// `InternalPromptOptions extends PromptOptions`.
#[derive(Clone, Default)]
pub struct InternalPromptOptions {
    pub base: PromptOptions,
    pub skip_pre_prompt_work: Option<bool>,
    pub return_after_accepted: Option<bool>,
    pub agent_message_id: Option<String>,
}

/// `SubmissionExtensionCommandPolicy`.
pub type SubmissionExtensionCommandPolicy = String;
pub const SUBMISSION_EXTENSION_COMMAND_POLICY_EXECUTE: &str = "execute";
pub const SUBMISSION_EXTENSION_COMMAND_POLICY_REJECT: &str = "reject";
pub const SUBMISSION_EXTENSION_COMMAND_POLICY_IGNORE: &str = "ignore";

/// `SubmissionNormalizationPolicy`.
#[derive(Debug, Clone, Default)]
pub struct SubmissionNormalizationPolicy {
    pub parse_session_commands: bool,
    pub extension_commands: SubmissionExtensionCommandPolicy,
    pub input_source: Option<crate::core::session_action_store::InputSource>,
    pub expand_skills: bool,
    pub expand_prompt_templates: bool,
}

/// `NormalizedSubmission`.
pub enum NormalizedSubmission {
    Prompt {
        text: String,
        images: Option<Vec<ImageContent>>,
    },
    SessionCommand {
        text: String,
        images: Option<Vec<ImageContent>>,
        command: SessionSlashCommand,
    },
    ExtensionCommand {
        completion: BoxFuture<Result<(), String>>,
    },
    Handled,
}

/// `PreTurnCompactionTiming`.
pub type PreTurnCompactionTiming = String;
pub const PRE_TURN_COMPACTION_BEFORE_MODEL_SELECTION: &str = "beforeModelSelection";
pub const PRE_TURN_COMPACTION_AFTER_MODEL_SELECTION: &str = "afterModelSelection";
pub const PRE_TURN_COMPACTION_SKIP: &str = "skip";

/// `RefineBarrierPolicy`.
pub type RefineBarrierPolicy = String;
pub const REFINE_BARRIER_ALWAYS: &str = "always";
pub const REFINE_BARRIER_IF_IN_FLIGHT: &str = "ifInFlight";
pub const REFINE_BARRIER_SKIP: &str = "skip";

/// `CommitPreparationPolicy`.
#[derive(Debug, Clone, PartialEq)]
pub struct CommitPreparationPolicy {
    pub initial_refine_barrier: RefineBarrierPolicy,
    pub flush_pending_bash_before_validation: bool,
    pub validate_model_and_auth: bool,
    pub await_pending_model_selection: bool,
    pub pre_turn_compaction: PreTurnCompactionTiming,
    pub final_refine_barrier: RefineBarrierPolicy,
}

/// `TurnExecutionPolicy`.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnExecutionPolicy {
    pub preparation: CommitPreparationPolicy,
    pub run_before_agent_start: bool,
    /// `nextTurnContextTiming: "preparation" | "commit" | "skip"`
    pub next_turn_context_timing: String,
    pub preserve_empty_extension_prompt: bool,
    pub completion_includes_retry_chain: bool,
}

pub const NEXT_TURN_CONTEXT_TIMING_PREPARATION: &str = "preparation";
pub const NEXT_TURN_CONTEXT_TIMING_COMMIT: &str = "commit";
pub const NEXT_TURN_CONTEXT_TIMING_SKIP: &str = "skip";

#[derive(Clone, Default)]
struct TurnExecutionPolicyOptions {
    return_after_accepted: Option<bool>,
    skip_pre_prompt_work: Option<bool>,
}

#[derive(Clone, Default)]
struct PreparedTurnActionOptions {
    agent_message_id: Option<String>,
    queue_key: Option<String>,
    content: Option<Vec<pi_ai::types::ImageOrTextContent>>,
    message: Option<AgentMessage>,
    custom_message: Option<CustomMessage>,
    prefix_messages: Option<Vec<CustomMessage>>,
    preview: Option<String>,
    preview_label: Option<String>,
    suppress_autonomous_continuation: Option<bool>,
    resume_if_idle: Option<bool>,
    source: Option<String>,
    execution_policy: Option<TurnExecutionPolicy>,
    queue_visible: Option<bool>,
    accepted_agent_message: Option<bool>,
    accepted_before_completion: Option<bool>,
}

struct RlmChildTurnOutcome {
    terminal: bool,
    continuation: Option<UserMessage>,
}

const DEFERRED_SESSION_INPUT_ERROR_MESSAGE: &str = "Session input paused before handoff";
const COMPACTION_CANCELLED_ERROR_MESSAGE: &str = "Compaction cancelled";
const COMPACTION_SKIPPED_ERROR_MESSAGE: &str = "Session is too short to compact — try again once it grows";


/// `turnExecutionPoliciesEqual`.
pub fn turn_execution_policies_equal(left: &TurnExecutionPolicy, right: &TurnExecutionPolicy) -> bool {
    left.preparation.initial_refine_barrier == right.preparation.initial_refine_barrier
        && left.preparation.flush_pending_bash_before_validation
            == right.preparation.flush_pending_bash_before_validation
        && left.preparation.validate_model_and_auth == right.preparation.validate_model_and_auth
        && left.preparation.await_pending_model_selection
            == right.preparation.await_pending_model_selection
        && left.preparation.pre_turn_compaction == right.preparation.pre_turn_compaction
        && left.preparation.final_refine_barrier == right.preparation.final_refine_barrier
        && left.run_before_agent_start == right.run_before_agent_start
        && left.next_turn_context_timing == right.next_turn_context_timing
        && left.preserve_empty_extension_prompt == right.preserve_empty_extension_prompt
        && left.completion_includes_retry_chain == right.completion_includes_retry_chain
}

/// `QueuedAgentMessage = UserMessage | CustomMessage`.
#[derive(Debug, Clone, PartialEq)]
pub enum QueuedAgentMessage {
    User(UserMessage),
    Custom(CustomMessage),
}

/// `SessionInputSchedule`.
pub type SessionInputSchedule = String;
pub const SESSION_INPUT_SCHEDULE_STEER: &str = "steer";
pub const SESSION_INPUT_SCHEDULE_FOLLOW_UP: &str = "followUp";

/// `PreparedTurnPayload extends SessionTurnPayload`.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedTurnPayload {
    pub base: SessionTurnPayload,
    pub images: Option<Vec<ImageContent>>,
    pub content: Option<Vec<pi_ai::types::ImageOrTextContent>>,
    pub custom_message: Option<CustomMessage>,
    pub prepared: Option<PreparedPromptPreparation>,
    pub execution_policy: TurnExecutionPolicy,
    pub queue_visible: bool,
    pub accepted_agent_message: bool,
    pub accepted_before_completion: bool,
    pub capture_run_messages: Option<HashSet<String>>,
    pub cancelled_dispatch_ended: Option<bool>,
}

/// `PreparedCommandPayload extends SessionCommandPayload`.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCommandPayload {
    pub base: SessionCommandPayload,
    pub images: Option<Vec<ImageContent>>,
}

/// `QueuedSessionAction = SessionAction<PreparedTurnPayload | PreparedCommandPayload>`.
pub type QueuedSessionAction = SessionAction<QueuedActionPayload>;

/// `PreparedTurnPayload | PreparedCommandPayload`.
#[derive(Debug, Clone, PartialEq)]
pub enum QueuedActionPayload {
    Turn(PreparedTurnPayload),
    SessionCommand(PreparedCommandPayload),
}

impl QueuedActionPayload {
    /// `payload.kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            QueuedActionPayload::Turn(_) => "turn",
            QueuedActionPayload::SessionCommand(_) => "session_command",
        }
    }

    pub fn as_turn(&self) -> Option<&PreparedTurnPayload> {
        match self {
            QueuedActionPayload::Turn(turn) => Some(turn),
            QueuedActionPayload::SessionCommand(_) => None,
        }
    }

    pub fn as_turn_mut(&mut self) -> Option<&mut PreparedTurnPayload> {
        match self {
            QueuedActionPayload::Turn(turn) => Some(turn),
            QueuedActionPayload::SessionCommand(_) => None,
        }
    }

    pub fn as_command(&self) -> Option<&PreparedCommandPayload> {
        match self {
            QueuedActionPayload::Turn(_) => None,
            QueuedActionPayload::SessionCommand(command) => Some(command),
        }
    }
}

impl crate::core::session_action_store::SessionPayload for QueuedActionPayload {
    fn records(&self) -> &[DeliveryRecord] {
        match self {
            Self::Turn(turn) => &turn.base.records,
            Self::SessionCommand(_) => &[],
        }
    }
    fn preview(&self) -> &str {
        match self {
            Self::Turn(turn) => turn.base.preview.as_deref().unwrap_or(&turn.base.text),
            Self::SessionCommand(command) => &command.base.text,
        }
    }
}

/// `PreparedPromptPreparation`.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedPromptPreparation {
    pub result: BeforeAgentStartResult,
    pub base_prompt_snapshot: String,
}

/// `DeferredSessionInputError extends Error`.
#[derive(Debug, Clone, PartialEq)]
pub struct DeferredSessionInputError {
    pub message: String,
}

impl std::fmt::Display for DeferredSessionInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for DeferredSessionInputError {}

/// `SessionInputAdmissionPausedError extends Error`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionInputAdmissionPausedError {
    pub message: String,
}

impl std::fmt::Display for SessionInputAdmissionPausedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for SessionInputAdmissionPausedError {}

/// `RestoredPromptInput`.
#[derive(Debug, Clone, Default)]
pub struct RestoredPromptInput {
    pub text: String,
    pub content: Option<Vec<pi_ai::types::ImageOrTextContent>>,
    pub images: Option<Vec<ImageContent>>,
    pub queue_key: Option<String>,
    pub agent_message_id: Option<String>,
    pub custom_message: Option<CustomMessage>,
    pub prefix_messages: Option<Vec<CustomMessage>>,
}

/// `SESSION_ACTION_RECOVERY_FORMAT_VERSION`.
pub const SESSION_ACTION_RECOVERY_FORMAT_VERSION: i64 = 1;

/// `SessionActionRecoveryRecord`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionActionRecoveryRecord {
    pub id: String,
    pub role: DeliveryRecordRole,
    pub message: Value,
    pub owner_action_id: String,
}

/// `SessionActionRecoveryPayload`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SessionActionRecoveryPayload {
    #[serde(rename = "turn")]
    Turn {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
        records: Vec<SessionActionRecoveryRecord>,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<Vec<pi_ai::types::ImageOrTextContent>>,
        #[serde(rename = "customMessage", skip_serializing_if = "Option::is_none")]
        custom_message: Option<Value>,
        #[serde(rename = "executionPolicy")]
        execution_policy: Value,
        #[serde(rename = "queueVisible")]
        queue_visible: bool,
        #[serde(rename = "acceptedAgentMessage")]
        accepted_agent_message: bool,
        #[serde(rename = "acceptedBeforeCompletion")]
        accepted_before_completion: bool,
    },
    #[serde(rename = "session_command")]
    SessionCommand {
        text: String,
        command: SessionSlashCommand,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
    },
}

/// `SessionActionRecoveryAction`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionActionRecoveryAction {
    pub id: String,
    pub source: crate::core::session_action_store::ActionSource,
    pub delivery: DeliveryPolicy,
    pub wake: WakePolicy,
    pub payload: SessionActionRecoveryPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppress_autonomous_continuation: Option<bool>,
}

/// `SessionActionRecoverySnapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionActionRecoverySnapshot {
    pub format_version: i64,
    pub actions: Vec<SessionActionRecoveryAction>,
}

/// `ModelCycleResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCycleResult {
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    pub service_tier: ServiceTier,
    pub is_scoped: bool,
}

/// `ModelSelectOptions`.
#[derive(Debug, Clone, Default)]
pub struct ModelSelectOptions {
    pub wait_for_extensions: Option<bool>,
}

/// `ToolDefinitionEntry`.
#[derive(Clone)]
pub struct ToolDefinitionEntry {
    pub definition: crate::core::extensions::types::ToolDefinition,
    pub source_info: SourceInfo,
}

/// `GoalSlashCommand`.
#[derive(Debug, Clone, PartialEq)]
pub enum GoalSlashCommand {
    Status,
    Clear,
    Pause,
    Resume,
    Start {
        objective: String,
        token_budget: Option<f64>,
    },
}

/// `AutonomousSlashCommand`.
#[derive(Debug, Clone, PartialEq)]
pub enum AutonomousSlashCommand {
    Status,
    On,
    Off,
}

/// `PersistedRlmMaxDepthState`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedRlmMaxDepthState {
    pub max_depth: f64,
}

/// `RlmMaxDepthSource` / `RlmMaxDepthStatus` / `SetRlmMaxDepthResult` come from
/// `core/rlm-max-depth.ts` (another slice); the local plumbing mirrors them.
pub type RlmMaxDepthSource = String;
pub const RLM_MAX_DEPTH_SOURCE_DEFAULT: &str = "default";
pub const RLM_MAX_DEPTH_SOURCE_ENV: &str = "env";
pub const RLM_MAX_DEPTH_SOURCE_GLOBAL: &str = "global";
pub const RLM_MAX_DEPTH_SOURCE_INHERITED: &str = "inherited";
pub const RLM_MAX_DEPTH_SOURCE_CHAT: &str = "chat";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RlmMaxDepthStatus {
    pub max_depth: f64,
    pub source: RlmMaxDepthSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRlmMaxDepthResult {
    pub max_depth: f64,
    pub source: RlmMaxDepthSource,
    pub global_saved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_error: Option<String>,
}

/// `AutonomousRuntimeSnapshot = Pick<AutonomousRuntimeState, ...>`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AutonomousRuntimeSnapshot {
    pub continuations_used: f64,
    pub gate_attempts: f64,
    pub last_gate_failure: Option<String>,
    pub last_gate_failure_snapshot: Option<String>,
}

/// `AgentAutonomousConfig` / `AgentAutonomousStatus` / `AutonomousRuntimeState`
/// from `core/autonomous.ts` (another slice).
#[derive(Debug, Clone, Default)]
pub struct AgentAutonomousConfig {
    pub enabled: Option<bool>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAutonomousStatus {
    pub enabled: bool,
    pub continuations_used: f64,
    pub gate_attempts: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_gate_failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_gate_failure_snapshot: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AutonomousRuntimeState {
    pub enabled: bool,
    pub continuations_used: f64,
    pub gate_attempts: f64,
    pub last_gate_failure: Option<String>,
    pub last_gate_failure_snapshot: Option<String>,
    pub cwd: Option<String>,
}

pub fn create_autonomous_runtime_state(
    config: Option<&AgentAutonomousConfig>,
    options: Option<&AgentAutonomousConfig>,
) -> AutonomousRuntimeState {
    AutonomousRuntimeState {
        enabled: config.and_then(|config| config.enabled).unwrap_or(false),
        continuations_used: 0.0,
        gate_attempts: 0.0,
        last_gate_failure: None,
        last_gate_failure_snapshot: None,
        cwd: options.and_then(|options| options.cwd.clone()),
    }
}

pub fn add_autonomous_continuation(state: &mut AutonomousRuntimeState) {
    state.continuations_used += 1.0;
}

pub fn add_autonomous_usage(state: &mut AutonomousRuntimeState, usage: &Usage) {
    state.gate_attempts += usage.total_tokens;
}

pub fn next_autonomous_continuation(state: &mut AutonomousRuntimeState) -> bool {
    state.enabled
}

pub fn set_autonomous_enabled(state: &mut AutonomousRuntimeState, enabled: bool) {
    state.enabled = enabled;
}

pub fn autonomous_status(state: &AutonomousRuntimeState) -> AgentAutonomousStatus {
    AgentAutonomousStatus {
        enabled: state.enabled,
        continuations_used: state.continuations_used,
        gate_attempts: state.gate_attempts,
        last_gate_failure: state.last_gate_failure.clone(),
        last_gate_failure_snapshot: state.last_gate_failure_snapshot.clone(),
    }
}

pub fn refresh_autonomous_quality_gates(_state: &mut AutonomousRuntimeState) {}

/// `AgentSessionConfig`.
pub struct AgentSessionConfig {
    pub agent: Arc<dyn AgentHandle>,
    pub session_manager: Arc<Mutex<SessionManager>>,
    pub settings_manager: Arc<Mutex<crate::core::settings_manager::SettingsManager>>,
    pub service_tier_preference: Option<ServiceTier>,
    pub cwd: String,
    pub agent_dir: Option<String>,
    pub scoped_models: Option<Vec<ScopedModel>>,
    pub resource_loader: Arc<dyn ResourceLoader>,
    pub custom_tools: Option<Vec<crate::core::extensions::types::ToolDefinition>>,
    pub model_registry: Arc<Mutex<crate::core::model_registry::ModelRegistry>>,
    pub initial_active_tool_names: Option<Vec<String>>,
    pub allowed_tool_names: Option<Vec<String>>,
    pub include_goals: Option<bool>,
    pub agent_message_controller: Option<Arc<dyn AgentSessionMessageController>>,
    pub agent_observe_controller: Option<Arc<dyn AgentObserveController>>,
    pub include_compact_skill: Option<bool>,
    pub rlm_heartbeat_controller: Option<Arc<dyn AgentRlmHeartbeatController>>,
    pub mcp_manager: Option<Arc<Mutex<McpManager>>>,
    pub base_tools_override: Option<Vec<(String, AgentTool)>>,
    pub extension_runner_ref: Option<Arc<ExtensionRunnerRef>>,
    pub session_start_event: Option<Value>,
    pub rlm_depth: Option<i64>,
    pub rlm_max_depth: Option<i64>,
    pub rlm_session_dir: Option<String>,
    pub rlm_parent_node_id: Option<String>,
    pub rlm_parent_agent: Option<String>,
    pub semantic_parent_session_id: Option<String>,
    pub semantic_spawned_by_request_id: Option<String>,
    pub subagent_runtime_host: Option<Arc<dyn SubagentRuntimeHost>>,
    pub autonomous: Option<AgentAutonomousConfig>,
    pub prewarm_ipython_kernel: Option<bool>,
    pub auto_refine_reviewer: Option<AutoRefineReviewer>,
    pub serialized_refine: Option<bool>,
    pub initial_goal: Option<InitialGoal>,
}

/// `initialGoal`.
#[derive(Debug, Clone)]
pub struct InitialGoal {
    pub objective: String,
    pub token_budget: Option<f64>,
}

// ---------------------------------------------------------------------------
// Module-level helpers (verbatim ports of the TypeScript free functions)
// ---------------------------------------------------------------------------

/// `safePerformanceMetricNow`.
pub fn safe_performance_metric_now(recorder: Option<&Arc<dyn PerformanceMetricRecorder>>) -> Option<f64> {
    let value = recorder?.monotonic_now();
    if value.is_finite() {
        Some(value)
    } else {
        None
    }
}

/// `performanceMetricUsageFromCompaction`.
pub fn performance_metric_usage_from_compaction(
    usage: Option<&Usage>,
) -> Option<PerformanceMetricUsageV1> {
    let usage = usage?;
    let has_authoritative_usage = [usage.input, usage.cache_read, usage.output, usage.total_tokens]
        .iter()
        .any(|value| value.is_finite() && *value > 0.0);
    if !has_authoritative_usage {
        return None;
    }
    // Normalized compaction Usage uses zero as an unavailable placeholder.
    // Without a raw provider observation, only positive fields prove availability.
    let token = |value: f64| -> Option<f64> {
        if value.is_finite() && value > 0.0 {
            Some(value)
        } else {
            None
        }
    };
    Some(PerformanceMetricUsageV1 {
        source: "provider".to_string(),
        input_tokens: token(usage.input),
        cached_input_tokens: token(usage.cache_read),
        output_tokens: token(usage.output),
        reasoning_tokens: None,
        total_tokens: token(usage.total_tokens),
        cached_input_included_in_input: None,
        reasoning_included_in_output: None,
    })
}

/// `oncePreflight`.
pub fn once_preflight(
    preflight_result: Option<Arc<dyn Fn(bool, bool) + Send + Sync>>,
) -> Arc<dyn Fn(bool, bool) + Send + Sync> {
    let settled = Arc::new(AtomicBool::new(false));
    Arc::new(move |success: bool, queued: bool| {
        if settled.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(preflight_result) = &preflight_result {
            preflight_result(success, queued);
        }
    })
}

/// `cloneCustomMessage`.
pub fn clone_custom_message(message: &CustomMessage) -> CustomMessage {
    let mut clone = message.clone();
    clone.content = match &message.content {
        CustomMessageContent::Text(text) => CustomMessageContent::Text(text.clone()),
        CustomMessageContent::Blocks(blocks) => CustomMessageContent::Blocks(
            blocks
                .iter()
                .map(|block| match block {
                    pi_agent_core::types::ContentBlock::Text(text) => {
                        pi_agent_core::types::ContentBlock::Text(text.clone())
                    }
                    pi_agent_core::types::ContentBlock::Image(image) => {
                        pi_agent_core::types::ContentBlock::Image(image.clone())
                    }
                })
                .collect(),
        ),
    };
    clone
}

/// `cloneQueuedAgentMessage`.
pub fn clone_queued_agent_message(message: &QueuedAgentMessage) -> QueuedAgentMessage {
    match message {
        QueuedAgentMessage::Custom(custom) => QueuedAgentMessage::Custom(clone_custom_message(custom)),
        QueuedAgentMessage::User(user) => {
            let mut clone = user.clone();
            clone.content = match &user.content {
                UserContent::Text(text) => UserContent::Text(text.clone()),
                UserContent::Blocks(blocks) => UserContent::Blocks(
                    blocks
                        .iter()
                        .map(|block| match block {
                            pi_ai::types::ImageOrTextContent::Text(text) => {
                                pi_ai::types::ImageOrTextContent::Text(text.clone())
                            }
                            pi_ai::types::ImageOrTextContent::Image(image) => {
                                pi_ai::types::ImageOrTextContent::Image(image.clone())
                            }
                        })
                        .collect(),
                ),
            };
            QueuedAgentMessage::User(clone)
        }
    }
}

/// `primaryDeliveryRecord`.
pub fn primary_delivery_record(action: &QueuedSessionAction) -> Result<DeliveryRecord, String> {
    let QueuedActionPayload::Turn(turn) = &action.payload else {
        return Err(format!("Session action {} is not a turn", action.id));
    };
    let record = turn
        .base
        .records
        .iter()
        .find(|candidate| candidate.role == DeliveryRecordRole::Primary);
    match record {
        Some(record) => Ok(record.clone()),
        None => Err(format!(
            "Turn action {} has no primary delivery record",
            action.id
        )),
    }
}

/// `normalizeMessageContent`.
pub fn normalize_message_content(
    content: &[pi_ai::types::ImageOrTextContent],
) -> (String, Option<Vec<ImageContent>>) {
    let text = content
        .iter()
        .filter_map(|part| match part {
            pi_ai::types::ImageOrTextContent::Text(text) => Some(text.text.clone()),
            pi_ai::types::ImageOrTextContent::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let images = content
        .iter()
        .filter_map(|part| match part {
            pi_ai::types::ImageOrTextContent::Image(image) => Some(image.clone()),
            pi_ai::types::ImageOrTextContent::Text(_) => None,
        })
        .collect::<Vec<_>>();
    (text, if images.is_empty() { None } else { Some(images) })
}

/// `queuedAgentMessagePreview`.
pub fn queued_agent_message_preview(action: &QueuedSessionAction) -> String {
    match &action.payload {
        QueuedActionPayload::SessionCommand(command) => command.base.text.clone(),
        QueuedActionPayload::Turn(turn) => {
            if let Some(custom) = &turn.custom_message {
                if is_agent_session_message(&AgentMessage::Custom(CustomAgentMessage::Custom {
                    custom_type: custom.custom_type.clone(),
                    content: custom.content.clone(),
                    display: custom.display,
                    details: custom.details.clone(),
                    timestamp: custom.timestamp,
                })) {
                    let message = custom
                        .details
                        .as_ref()
                        .and_then(|details| details.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    return format!("{AGENT_MESSAGE_RECEIVED_PREVIEW_LABEL}: {message}");
                }
                if custom.custom_type == ASYNC_BASH_COMPLETION_CUSTOM_TYPE {
                    return match custom.details.as_ref() {
                        Some(details) => format!(
                            "{ASYNC_BASH_COMPLETION_PREVIEW_LABEL}: pid {}, exit {}",
                            details.get("pid").and_then(Value::as_i64).unwrap_or_default(),
                            details
                                .get("exitCode")
                                .and_then(Value::as_i64)
                                .unwrap_or_default()
                        ),
                        None => ASYNC_BASH_COMPLETION_PREVIEW_LABEL.to_string(),
                    };
                }
            }
            turn.base
                .preview
                .clone()
                .unwrap_or_else(|| turn.base.text.clone())
        }
    }
}

/// `visibleSessionActionProjection`.
pub fn visible_session_action_projection<'a>(
    actions: &'a [QueuedSessionAction],
) -> Vec<&'a QueuedSessionAction> {
    actions
        .iter()
        .filter(|action| match &action.payload {
            QueuedActionPayload::SessionCommand(_) => true,
            QueuedActionPayload::Turn(turn) => turn.queue_visible || turn.accepted_agent_message,
        })
        .collect()
}

/// `IPYTHON_SENT_AGENT_MESSAGE_CUSTOM_ENTRY`.
pub const IPYTHON_SENT_AGENT_MESSAGE_CUSTOM_ENTRY: &str = "ipython_sent_agent_message";

/// `PersistedIpythonSentAgentMessage`.
#[derive(Debug, Clone, PartialEq)]
pub struct PersistedIpythonSentAgentMessage {
    pub tool_call_id: String,
    pub message: Value,
}

fn is_object_record(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

/// `parsePersistedIpythonSentAgentMessage`.
pub fn parse_persisted_ipython_sent_agent_message(
    value: &Value,
) -> Option<PersistedIpythonSentAgentMessage> {
    if !is_object_record(value) {
        return None;
    }
    let tool_call_id = value.get("toolCallId").and_then(Value::as_str)?;
    let message = value.get("message")?;
    if !is_object_record(message) {
        return None;
    }
    let id = message.get("id").and_then(Value::as_str)?;
    let text = message.get("message").and_then(Value::as_str)?;
    let delivery_status = message.get("deliveryStatus").and_then(Value::as_str)?;
    if delivery_status != "delivered" && delivery_status != "queued" {
        return None;
    }
    let target = message.get("target")?;
    if !is_object_record(target) {
        return None;
    }
    let active_session_id = target.get("activeSessionId").and_then(Value::as_str)?;
    let session_id = target.get("sessionId").and_then(Value::as_str)?;
    let mut target_object = Map::new();
    target_object.insert(
        "activeSessionId".to_string(),
        Value::String(active_session_id.to_string()),
    );
    target_object.insert("sessionId".to_string(), Value::String(session_id.to_string()));
    if let Some(session_name) = target.get("sessionName").and_then(Value::as_str) {
        target_object.insert(
            "sessionName".to_string(),
            Value::String(session_name.to_string()),
        );
    }
    let mut message_object = Map::new();
    message_object.insert("id".to_string(), Value::String(id.to_string()));
    message_object.insert("message".to_string(), Value::String(text.to_string()));
    message_object.insert(
        "deliveryStatus".to_string(),
        Value::String(delivery_status.to_string()),
    );
    message_object.insert("target".to_string(), Value::Object(target_object));
    Some(PersistedIpythonSentAgentMessage {
        tool_call_id: tool_call_id.to_string(),
        message: Value::Object(message_object),
    })
}

/// `appendSentAgentMessageToToolResult`.
pub fn append_sent_agent_message_to_tool_result(
    message: &mut AgentMessage,
    tool_call_id: &str,
    sent_message: &Value,
) -> bool {
    let AgentMessage::Message(Message::ToolResult(tool_result)) = message else {
        return false;
    };
    if tool_result.tool_name != "ipython" || tool_result.tool_call_id != tool_call_id {
        return false;
    }
    let details = match tool_result.details.as_ref() {
        Some(Value::Object(object)) => object.clone(),
        _ => Map::new(),
    };
    let current = match details.get("sentAgentMessages") {
        Some(Value::Array(entries)) => entries.clone(),
        _ => Vec::new(),
    };
    let sent_id = sent_message.get("id").cloned().unwrap_or(Value::Null);
    if current
        .iter()
        .any(|entry| is_object_record(entry) && entry.get("id") == Some(&sent_id))
    {
        return true;
    }
    let mut next = current;
    next.push(sent_message.clone());
    let mut details = details;
    details.insert("sentAgentMessages".to_string(), Value::Array(next));
    tool_result.details = Some(Value::Object(details));
    true
}

/// `injectedMessagePreviewLabel`.
pub fn injected_message_preview_label(message: &CustomMessage) -> Option<String> {
    match message.custom_type.as_str() {
        HEARTBEAT_PROMPT_CUSTOM_TYPE => Some(HEARTBEAT_PROMPT_PREVIEW_LABEL.to_string()),
        ASYNC_BASH_COMPLETION_CUSTOM_TYPE => Some(ASYNC_BASH_COMPLETION_PREVIEW_LABEL.to_string()),
        GOAL_CONTEXT_CUSTOM_TYPE => Some(GOAL_CONTEXT_PREVIEW_LABEL.to_string()),
        _ => None,
    }
}

/// `AgentMessageDeferred` - one-shot settle/wait pair, mirroring a JS deferred.
pub struct AgentMessageDeferred {
    state: Arc<AgentMessageDeferredState>,
}

struct AgentMessageDeferredState {
    settled: Mutex<Option<Result<(), String>>>,
    notify: tokio::sync::Notify,
}

impl Clone for AgentMessageDeferred {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

impl AgentMessageDeferred {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AgentMessageDeferredState {
                settled: Mutex::new(None),
                notify: tokio::sync::Notify::new(),
            }),
        }
    }

    pub fn resolve(&self) {
        let mut settled = self.state.settled.lock().unwrap();
        if settled.is_none() {
            *settled = Some(Ok(()));
            drop(settled);
            self.state.notify.notify_waiters();
        }
    }

    pub fn reject(&self, error: String) {
        let mut settled = self.state.settled.lock().unwrap();
        if settled.is_none() {
            *settled = Some(Err(error));
            drop(settled);
            self.state.notify.notify_waiters();
        }
    }

    pub fn is_settled(&self) -> bool {
        self.state.settled.lock().unwrap().is_some()
    }

    pub async fn wait(&self) -> Result<(), String> {
        loop {
            {
                let settled = self.state.settled.lock().unwrap();
                if let Some(result) = settled.as_ref() {
                    return result.clone();
                }
            }
            self.state.notify.notified().await;
        }
    }
}

impl Default for AgentMessageDeferred {
    fn default() -> Self {
        Self::new()
    }
}

/// `AgentMessageOutcome`.
#[derive(Default, Clone)]
pub struct AgentMessageOutcome {
    pub delivery: Option<AgentMessageDeferred>,
    pub completion: Option<AgentMessageDeferred>,
}

/// `createAgentMessageDeferred`.
pub fn create_agent_message_deferred() -> AgentMessageDeferred {
    AgentMessageDeferred::new()
}

/// `PostCompactionContinuationSettlement`.
pub struct PostCompactionContinuationSettlement {
    pub deferred: AgentMessageDeferred,
    pub continue_after_session_input: bool,
    pub settled: bool,
}

/// `createPostCompactionContinuationSettlement`.
pub fn create_post_compaction_continuation_settlement() -> PostCompactionContinuationSettlement {
    PostCompactionContinuationSettlement {
        deferred: create_agent_message_deferred(),
        continue_after_session_input: false,
        settled: false,
    }
}

/// `autoRefineInstructions`.
pub fn auto_refine_instructions(reason: &AutoRefineReason, review: &AutoRefineReview) -> String {
    let detail = match &review.instructions {
        Some(instructions) => format!("\nReviewer instructions: {instructions}"),
        None => String::new(),
    };
    format!(
        "Automatic refine review triggered by {}. Only create/update/delete local harness entries if there is clear evidence that should help this session continue. Prefer an empty edits array over speculative or one-off memories. Do not promote anything global unless explicitly requested. Reviewer rationale: {}{}",
        reason.as_str(),
        review.rationale,
        detail
    )
}

/// `isNonNegativeInteger`.
pub fn is_non_negative_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value >= 0.0
}

/// `parseDepth`.
pub fn parse_depth(value: Option<&str>, fallback: i64, name: &str) -> Result<i64, String> {
    let value = match value {
        None => return Ok(fallback),
        Some(value) if value.is_empty() => return Ok(fallback),
        Some(value) => value,
    };
    if !value.chars().all(|character| character.is_ascii_digit()) {
        return Err(format!("{name} must be a non-negative integer"));
    }
    match value.parse::<i64>() {
        Ok(parsed) if is_non_negative_integer(parsed as f64) => Ok(parsed),
        _ => Err(format!("{name} must be a non-negative integer")),
    }
}

/// `isPersistedRlmMaxDepthState`.
pub fn is_persisted_rlm_max_depth_state(value: &Value) -> bool {
    match value {
        Value::Object(object) => match object.get("maxDepth").and_then(Value::as_f64) {
            Some(max_depth) => is_non_negative_integer(max_depth),
            None => false,
        },
        _ => false,
    }
}

/// `parseGoalBudgetValue`.
pub fn parse_goal_budget_value(value: &str) -> Result<f64, String> {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes[0] == b'0' || !value.chars().all(|character| character.is_ascii_digit())
    {
        return Err("Goal token budget must be a positive integer.".to_string());
    }
    let budget = validate_goal_budget(value.parse::<f64>().ok())?;
    match budget {
        Some(budget) => Ok(budget),
        None => Err("Goal token budget must be a positive integer.".to_string()),
    }
}

/// `compactRlmText`.
pub fn compact_rlm_text(text: &str, max_length: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_length {
        return compact;
    }
    let keep = max_length.saturating_sub(3);
    let truncated: String = compact.chars().take(keep).collect();
    format!("{}...", truncated.trim_end())
}

// Child-agent label: collapse to one line but keep the full prompt - the TUI
// truncates to the visible width and elides shared prefixes, so capping here
// would only hide the divergence between near-identical sibling prompts.
/// `rlmChildLabel`.
pub fn rlm_child_label(prompt: &str) -> String {
    let collapsed = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "child agent".to_string()
    } else {
        collapsed
    }
}

/// `readAssistantText`.
pub fn read_assistant_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            pi_ai::types::ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// `waitForPromiseOrAbort`.
pub async fn wait_for_promise_or_abort<T>(
    promise: BoxFuture<T>,
    signal: Option<&CancellationToken>,
    abort_message: &str,
) -> Result<T, String> {
    let signal = match signal {
        None => return Ok(promise.await),
        Some(signal) => signal.clone(),
    };
    if signal.is_cancelled() {
        return Err(abort_message.to_string());
    }
    tokio::select! {
        value = promise => Ok(value),
        _ = signal.cancelled() => Err(abort_message.to_string()),
    }
}

// Bounds how much accumulated child usage a parent process crash can lose.
pub const RLM_CHILD_USAGE_FLUSH_MAX_PENDING_MS: f64 = 60_000.0;

/// `rlmChildUsageOrigin`: label a child completion's usage by the nearest
/// preceding prompt that triggered it.
pub fn rlm_child_usage_origin(
    messages: &[AgentMessage],
    assistant_index: usize,
) -> &'static str {
    let mut index = assistant_index as i64 - 1;
    while index >= 0 {
        let message = &messages[index as usize];
        let role = message.role();
        if role != "user" && role != "custom" {
            index -= 1;
            continue;
        }
        if role == "custom" && is_agent_session_message(message) {
            let id = match message {
                AgentMessage::Custom(CustomAgentMessage::Custom { details, .. }) => details
                    .as_ref()
                    .and_then(|details| details.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                _ => String::new(),
            };
            return if id.starts_with("spawn:") {
                "spawn_task"
            } else {
                "agent_message"
            };
        }
        return "direct_user";
    }
    "direct_user"
}

/// `attributeChildUsage`.
pub fn attribute_child_usage(parent_usage: &mut Usage, child_usage: &Usage) {
    let parent_context_tokens = if parent_usage.total_tokens != 0.0 {
        parent_usage.total_tokens
    } else {
        parent_usage.input + parent_usage.output + parent_usage.cache_read + parent_usage.cache_write
    };
    // Recursive children are launched from an assistant tool call, so the parent
    // assistant message carries their billable usage for session-level cost totals.
    add_assistant_usage(parent_usage, child_usage);
    // Child work affects session-level billable totals, not the parent's
    // model-facing context size.
    parent_usage.total_tokens = parent_context_tokens;
}

/// `KERNEL_STATE_LISTING_TIMEOUT_MS`.
pub const KERNEL_STATE_LISTING_TIMEOUT_MS: u64 = 5000;
/// `RLM_MAX_DEPTH_STATE_CUSTOM_TYPE`.
pub const RLM_MAX_DEPTH_STATE_CUSTOM_TYPE: &str = "rlm_max_depth_state";

/// `noopRlmChildAbort`.
pub fn noop_rlm_child_abort() {}
/// `noopRlmChildEventUnsubscribe`.
pub fn noop_rlm_child_event_unsubscribe() {}

/// `RlmChildRun`.
#[derive(Clone)]
pub struct RlmChildRun {
    pub id: String,
    pub prompt: String,
    pub session_name: String,
    pub session_dir: String,
    pub model: Option<Model>,
    pub status: RlmChildAgentStatus,
    pub duration_ms: Option<f64>,
    pub answer_preview: Option<String>,
    pub tool_use_count: f64,
    pub activity: Option<RlmChildAgentActivity>,
    pub error: Option<String>,
    pub abort: Arc<dyn Fn() + Send + Sync>,
    pub publication: AgentMessageDeferred,
    /// Resolves after terminal result publication and detached-run cleanup finish.
    pub settlement: AgentMessageDeferred,
    /// Child session, once its runtime exists. Used to cancel nested child runs.
    pub session: Option<Arc<AgentSession>>,
    pub settled: bool,
    /// Do not inject a late terminal notice after the parent session is aborted.
    pub suppress_terminal_notice: Option<bool>,
    /// Excluded from future strong barriers after an authoritative cancellation cut.
    pub abandoned_for_quiescence: Option<bool>,
    /// Selector snapshot for an admitted explicit delete.
    pub detached_deletion: Option<RlmSubagentRegistryEntry>,
    /// Shared physical runtime cleanup owned by the explicit-delete path.
    pub deletion_cleanup: Option<Arc<AgentMessageDeferred>>,
    pub deletion_cleanup_observer: Option<Arc<AgentMessageDeferred>>,
    /// Resolves when a deletion may release its selector reservation.
    pub deletion_reservation: AgentMessageDeferred,
    pub deletion_cleanup_failed: Option<bool>,
    pub deletion_run_finished: Option<bool>,
    pub deletion_notice: Option<Arc<AgentMessageDeferred>>,
    pub deletion_failure_notice: Option<Arc<AgentMessageDeferred>>,
    pub deletion_needs_completion_notice: Option<bool>,
    pub complete_deletion: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    pub report_deletion_cleanup_failure:
        Option<Arc<dyn Fn(String) -> BoxFuture<()> + Send + Sync>>,
    pub emit_update: Option<Arc<dyn Fn() + Send + Sync>>,
    pub last_emitted_update: Option<String>,
    pub unsubscribe: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// `RetainedRlmChild`.
#[derive(Clone)]
pub struct RetainedRlmChild {
    pub session: Arc<AgentSession>,
    pub run: Option<Arc<Mutex<RlmChildRun>>>,
}

/// `RlmSubagentModelSelection`.
#[derive(Clone)]
pub struct RlmSubagentModelSelection {
    pub model: Model,
}

/// `AgentSession`.
///
/// Fields keep the TypeScript names in `snake_case`. Mutable state that the
/// TypeScript mutates from callbacks lives behind `Mutex` because the Rust
/// agent callbacks are `Send + Sync` and can fire from other tasks.
pub struct AgentSession {
    pub agent: Arc<dyn AgentHandle>,
    pub session_manager: Arc<Mutex<SessionManager>>,
    pub settings_manager: Arc<Mutex<crate::core::settings_manager::SettingsManager>>,

    service_tier_preference: Mutex<ServiceTier>,
    scoped_models: Vec<ScopedModel>,
    event_listeners: Mutex<Vec<AgentSessionEventListener>>,
    last_session_action_snapshot: Mutex<SessionActionSnapshot>,
    /// `_agentEventQueue` - the serialized tail of agent-event work.
    agent_event_queue: Mutex<Option<BoxFuture<Result<(), String>>>>,
    action_store: Mutex<ActionStore<QueuedSessionAction>>,
    session_input_pump: Mutex<BoxFuture<Result<(), String>>>,
    session_input_pump_requested: AtomicBool,
    session_input_pump_epoch: AtomicU64,
    session_input_arrival_epoch: AtomicU64,
    session_input_pump_suspended: AtomicBool,
    session_input_suspended_for_update_restart: AtomicBool,
    queued_work_pauses: Mutex<HashSet<String>>,
    session_input_admission_pauses: Mutex<HashSet<String>>,
    durable_rlm_terminal_notice_action_ids: Mutex<HashSet<String>>,
    session_action_commit_tail: Mutex<BoxFuture<Result<(), String>>>,
    session_action_commit_owner: Mutex<Option<String>>,
    pending_session_action_fence_waiters: AtomicU64,
    session_action_commit_dispose_abort: CancellationToken,
    session_input_checkpoint_waiters: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
    pending_next_turn_messages: Mutex<Vec<CustomMessage>>,
    goal_state: Mutex<GoalState>,
    goal_accounting_started_at: Mutex<Option<f64>>,
    goal_continuation_awaits_rlm_work: AtomicBool,
    goal_accounted_assistant_messages: Mutex<HashSet<i64>>,
    goal_abort_in_progress: AtomicBool,
    autonomous_state: Mutex<AutonomousRuntimeState>,
    autonomous_continuation_suppression_depth: AtomicU64,
    autonomous_continuation_suppressed_messages: Mutex<HashSet<i64>>,
    compaction_abort_controller: Mutex<Option<CancellationToken>>,
    auto_compaction_abort_controller: Mutex<Option<CancellationToken>>,
    compaction_operation: Mutex<Option<BoxFuture<Result<(), String>>>>,
    /// `"idle" | "attempted" | "reported"`
    overflow_recovery: Mutex<String>,
    continue_after_threshold_compaction: AtomicBool,
    pending_requested_compaction: Mutex<Option<PendingRequestedCompaction>>,
    pending_requested_refine: Mutex<Option<PendingRequestedRefine>>,
    branch_summary_abort_controller: Mutex<Option<CancellationToken>>,
    branch_summary_operation: Mutex<Option<BoxFuture<Result<(), String>>>>,
    retry_abort_controller: Mutex<Option<CancellationToken>>,
    retry_attempt: AtomicU64,
    retry_generation: AtomicU64,
    retry_promise: Mutex<Option<BoxFuture<Result<(), String>>>>,
    retry_resolve: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    retry_metric_message: Mutex<Option<AssistantMessage>>,
    retry_auth_failure_sources: Mutex<Vec<AuthSourceToken>>,
    agent_message_clear_epoch: AtomicU64,
    agent_message_outcomes: Mutex<HashMap<String, AgentMessageOutcome>>,
    late_ipython_sent_agent_messages: Mutex<HashMap<String, Vec<Value>>>,
    unpersisted_outcomes: Mutex<Vec<CustomMessage>>,
    harness_digest_pending: AtomicBool,
    bash_abort_controllers: Mutex<Vec<CancellationToken>>,
    user_bash_running: AtomicBool,
    user_bash_abort_requested: AtomicBool,
    pending_bash_messages: Mutex<Vec<BashExecutionMessage>>,
    turn_index: AtomicU64,
    model_select_emit_queue: Mutex<BoxFuture<Result<(), String>>>,
    model_select_emit_queue_idle: AtomicBool,
    resource_loader: Arc<dyn ResourceLoader>,
    custom_tools: Vec<crate::core::extensions::types::ToolDefinition>,
    acp_mcp_tools: Mutex<Vec<crate::core::extensions::types::ToolDefinition>>,
    base_tool_definitions: Mutex<BTreeMap<String, crate::core::extensions::types::ToolDefinition>>,
    cwd: String,
    agent_dir: Option<String>,
    include_goals: bool,
    include_compact_skill: bool,
    session_start_event: Value,
    disposed: AtomicBool,
    dispose_callbacks: Mutex<Vec<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>>,
    disposing: AtomicBool,
    ipython_runtime_built: AtomicBool,
    prewarm_ipython_kernel: bool,
    rlm_depth: i64,
    configured_rlm_max_depth: Option<i64>,
    rlm_max_depth: Mutex<i64>,
    rlm_max_depth_source: Mutex<RlmMaxDepthSource>,
    semantic_edges: Arc<Mutex<crate::core::semantic_edges::SemanticEdgeRecorder>>,
    replied_to_parent_since_task: Mutex<Option<bool>>,
    parent_reply_count: AtomicU64,
    rlm_continuation: Mutex<RlmContinuationState>,
    rlm_explicit_replies_in_flight: Mutex<Vec<BoxFuture<Result<AgentSessionMessageReceipt, String>>>>,
    rlm_unindexed_child_usage: Mutex<HashMap<i64, Usage>>,
    active_rlm_child_runs: Mutex<HashMap<String, Arc<Mutex<RlmChildRun>>>>,
    unsettled_rlm_child_runs: Mutex<Vec<Arc<Mutex<RlmChildRun>>>>,
    abandoned_rlm_quiescence_child_ids: Mutex<HashSet<String>>,
    rlm_quiescence_wait_aborts: Mutex<Vec<CancellationToken>>,
    pending_rlm_subagent_session_names: Mutex<HashSet<String>>,
    rlm_child_sessions: Mutex<HashMap<String, RetainedRlmChild>>,
    deleted_rlm_child_ids: Mutex<HashSet<String>>,
    rlm_child_cleanup_failures: Mutex<HashMap<String, RlmSubagentRegistryEntry>>,
    deleting_rlm_children: Mutex<HashMap<String, Arc<AgentMessageDeferred>>>,
    rlm_child_unsubscribes: Mutex<HashMap<String, Arc<dyn Fn() + Send + Sync>>>,
    model_registry: Arc<Mutex<crate::core::model_registry::ModelRegistry>>,
    tool_registry: Mutex<BTreeMap<String, AgentTool>>,
    tool_definitions: Mutex<BTreeMap<String, ToolDefinitionEntry>>,
    tool_prompt_snippets: Mutex<BTreeMap<String, String>>,
    tool_prompt_guidelines: Mutex<BTreeMap<String, Vec<String>>>,
    base_system_prompt: Mutex<String>,
    assistant_turns_since_auto_refine: AtomicU64,
    last_auto_refine_review_at: Mutex<f64>,
    auto_refine_in_progress: AtomicBool,
    auto_refine_operations: Mutex<Vec<BoxFuture<Result<(), String>>>>,
    scheduled_auto_refine_timers: Mutex<Vec<u64>>,
    compact_auto_refine_pending: AtomicBool,
    turn_interval_auto_refine_pending: AtomicBool,
    post_compaction_continuation_scheduled: AtomicBool,
    post_compaction_continuation_settlement: Mutex<Option<Arc<Mutex<PostCompactionContinuationSettlement>>>>,
    post_compaction_continuation_messages: Mutex<Vec<AgentMessage>>,
    scheduled_post_compaction_continuation_messages: Mutex<Vec<AgentMessage>>,
    queued_autonomous_threshold_continuations: Mutex<HashMap<i64, AgentMessage>>,
    queued_autonomous_continuation_snapshots: Mutex<HashMap<i64, AutonomousRuntimeSnapshot>>,
    pending_threshold_compaction_autonomous_messages: Mutex<Vec<AgentMessage>>,
    queued_goal_threshold_continuation: Mutex<Option<AgentMessage>>,
    pending_auto_refine_review: Mutex<Option<(AutoRefineReason, AutoRefineReview)>>,
    auto_refine_branch_version: AtomicU64,
    serialized_refine: bool,
    refine_in_flight: Mutex<Option<BoxFuture<Result<(), String>>>>,
    refine_plan_in_flight: Mutex<Option<BoxFuture<Result<(), String>>>>,
    serialized_plan_in_flight:
        Mutex<Option<BoxFuture<Result<Option<SerializedBackgroundPlanResult>, String>>>>,
    serialized_plan_claim: Mutex<Option<BoxFuture<Result<(), String>>>>,
    serialized_explicit_refine_options: Mutex<Option<RefineOptions>>,

    // Wiring that the TypeScript keeps on the config object.
    extension_runner_ref: Arc<ExtensionRunnerRef>,
    initial_active_tool_names: Option<Vec<String>>,
    allowed_tool_names: Mutex<Option<HashSet<String>>>,
    rlm_heartbeat_controller: Mutex<Option<Arc<dyn AgentRlmHeartbeatController>>>,
    agent_message_controller: Option<Arc<dyn AgentSessionMessageController>>,
    agent_observe_controller: Option<Arc<dyn AgentObserveController>>,
    mcp_manager: Option<Arc<Mutex<McpManager>>>,
    base_tools_override: Option<Vec<(String, AgentTool)>>,
    subagent_runtime_host: Mutex<Option<Arc<dyn SubagentRuntimeHost>>>,
    auto_refine_reviewer: Option<AutoRefineReviewer>,
    rlm_session_dir: Option<String>,
    rlm_parent_node_id: Option<String>,
    rlm_parent_agent: Option<String>,
    ipython_kernel_provisioner: Mutex<Option<Arc<crate::core::tools::ipython::IpythonKernelProvisioner>>>,
    exec_env_provider: Mutex<Option<Arc<dyn Fn() -> HashMap<String, String> + Send + Sync>>>,
    auto_compaction_enabled: AtomicBool,
    last_assistant_message: Mutex<Option<AssistantMessage>>,
    branch_navigation_queue: Mutex<BoxFuture<Result<(), String>>>,
    provider_context_rebuilt_at: Mutex<Option<f64>>,
    steering_mode: Mutex<String>,
    follow_up_mode: Mutex<String>,
    recap: Mutex<Option<String>>,
    unsubscribe_agent: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,

    /// `_sessionActionCommitContext`/checkpoint plumbing for the scheduler.
    session_action_activity_notify: Arc<tokio::sync::Notify>,
    observed_action_deferrals: Mutex<HashMap<String, String>>,
    resource_extension_paths: Option<ResourceExtensionPaths>,
    extension_command_context_actions: Option<Value>,
    extension_error_listener: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    extension_shutdown_handler: Option<Arc<dyn Fn(Value) -> BoxFuture<()> + Send + Sync>>,
    own_usage_memo: Mutex<Option<OwnUsageMemo>>,
}

/// `pendingRequestedCompaction`.
#[derive(Debug, Clone, Default)]
pub struct PendingRequestedCompaction {
    pub custom_instructions: Option<String>,
}

/// `pendingRequestedRefine`.
#[derive(Debug, Clone, Default)]
pub struct PendingRequestedRefine {
    pub instructions: Option<String>,
    pub global: Option<bool>,
}

/// `SessionContext`/`SessionStats` re-exports used by callers of this module.
pub use crate::core::session_manager::SessionContext as AgentSessionContext;

impl AgentSession {
    /// `constructor(config: AgentSessionConfig)`.
    pub fn new(config: AgentSessionConfig) -> Result<Arc<Self>, String> {
        let header_rlm_depth = config
            .session_manager
            .lock()
            .unwrap()
            .get_header()
            .and_then(|header| header.rlm_depth);
        let rlm_depth = match config.rlm_depth {
            Some(depth) => depth,
            None => match header_rlm_depth {
                Some(depth) if is_non_negative_integer(depth as f64) => depth,
                _ => parse_depth(
                    std::env::var("RLM_DEPTH").ok().as_deref(),
                    0,
                    "RLM_DEPTH",
                )?,
            },
        };
        if let Some(configured) = config.rlm_max_depth {
            if !is_non_negative_integer(configured as f64) {
                return Err("rlmMaxDepth must be a non-negative integer".to_string());
            }
        }
        let include_compact_skill = config.include_compact_skill.unwrap_or_else(|| {
            config
                .settings_manager
                .lock()
                .unwrap()
                .get_compaction_agent_callable()
        });
        let session_artifact_dir = config.session_manager.lock().unwrap().get_session_artifact_dir();
        let session_id = config.session_manager.lock().unwrap().get_session_id();
        let ledger_path = crate::core::semantic_edges::semantic_edge_ledger_path(
            config.rlm_session_dir.as_deref(),
            session_artifact_dir.as_deref(),
        );
        let semantic_edges = Arc::new(Mutex::new(
            crate::core::semantic_edges::SemanticEdgeRecorder::new(
                ledger_path,
                session_id,
                config.semantic_parent_session_id.clone(),
                config.semantic_spawned_by_request_id.clone(),
            ),
        ));
        let stream_fn = crate::core::semantic_edges::wrap_stream_fn_with_semantic_edges(
            config.agent.stream_fn(),
            semantic_edges.clone(),
        );
        config.agent.set_stream_fn(stream_fn);

        // A resumed child may have replied before this process started; false would
        // claim knowledge that is not present in the session transcript.
        let branch_has_message = config
            .session_manager
            .lock()
            .unwrap()
            .get_branch(None)
            .iter()
            .any(|entry| entry.get("type").and_then(Value::as_str) == Some("message"));
        let replied_to_parent_since_task = if rlm_depth > 0 && branch_has_message {
            None
        } else {
            Some(false)
        };

        let service_tier_preference = match &config.service_tier_preference {
            Some(tier) => tier.clone(),
            None => config.agent.state().service_tier.clone(),
        };

        let session = Arc::new(AgentSession {
            agent: config.agent.clone(),
            session_manager: config.session_manager.clone(),
            settings_manager: config.settings_manager.clone(),
            service_tier_preference: Mutex::new(service_tier_preference),
            scoped_models: config.scoped_models.unwrap_or_default(),
            event_listeners: Mutex::new(Vec::new()),
            last_session_action_snapshot: Mutex::new(SessionActionSnapshot {
                queued_count: 0,
                steering: Vec::new(),
                follow_ups: Vec::new(),
                active: None,
            }),
            agent_event_queue: Mutex::new(Some(Box::pin(async { Ok(()) }))),
            action_store: Mutex::new(ActionStore::new()),
            session_input_pump: Mutex::new(Box::pin(async { Ok(()) })),
            session_input_pump_requested: AtomicBool::new(false),
            session_input_pump_epoch: AtomicU64::new(0),
            session_input_arrival_epoch: AtomicU64::new(0),
            session_input_pump_suspended: AtomicBool::new(false),
            session_input_suspended_for_update_restart: AtomicBool::new(false),
            queued_work_pauses: Mutex::new(HashSet::new()),
            session_input_admission_pauses: Mutex::new(HashSet::new()),
            durable_rlm_terminal_notice_action_ids: Mutex::new(HashSet::new()),
            session_action_commit_tail: Mutex::new(Box::pin(async { Ok(()) })),
            session_action_commit_owner: Mutex::new(None),
            pending_session_action_fence_waiters: AtomicU64::new(0),
            session_action_commit_dispose_abort: CancellationToken::new(),
            session_input_checkpoint_waiters: Mutex::new(Vec::new()),
            pending_next_turn_messages: Mutex::new(Vec::new()),
            goal_state: Mutex::new(empty_goal_state()),
            goal_accounting_started_at: Mutex::new(None),
            goal_continuation_awaits_rlm_work: AtomicBool::new(false),
            goal_accounted_assistant_messages: Mutex::new(HashSet::new()),
            goal_abort_in_progress: AtomicBool::new(false),
            autonomous_state: Mutex::new(create_autonomous_runtime_state(
                config.autonomous.as_ref(),
                Some(&AgentAutonomousConfig {
                    enabled: None,
                    cwd: Some(config.cwd.clone()),
                }),
            )),
            autonomous_continuation_suppression_depth: AtomicU64::new(0),
            autonomous_continuation_suppressed_messages: Mutex::new(HashSet::new()),
            compaction_abort_controller: Mutex::new(None),
            auto_compaction_abort_controller: Mutex::new(None),
            compaction_operation: Mutex::new(None),
            overflow_recovery: Mutex::new("idle".to_string()),
            continue_after_threshold_compaction: AtomicBool::new(false),
            pending_requested_compaction: Mutex::new(None),
            pending_requested_refine: Mutex::new(None),
            branch_summary_abort_controller: Mutex::new(None),
            branch_summary_operation: Mutex::new(None),
            retry_abort_controller: Mutex::new(None),
            retry_attempt: AtomicU64::new(0),
            retry_generation: AtomicU64::new(0),
            retry_promise: Mutex::new(None),
            retry_resolve: Mutex::new(None),
            retry_metric_message: Mutex::new(None),
            retry_auth_failure_sources: Mutex::new(Vec::new()),
            agent_message_clear_epoch: AtomicU64::new(0),
            agent_message_outcomes: Mutex::new(HashMap::new()),
            late_ipython_sent_agent_messages: Mutex::new(HashMap::new()),
            unpersisted_outcomes: Mutex::new(Vec::new()),
            harness_digest_pending: AtomicBool::new(false),
            bash_abort_controllers: Mutex::new(Vec::new()),
            user_bash_running: AtomicBool::new(false),
            user_bash_abort_requested: AtomicBool::new(false),
            pending_bash_messages: Mutex::new(Vec::new()),
            turn_index: AtomicU64::new(0),
            model_select_emit_queue: Mutex::new(Box::pin(async { Ok(()) })),
            model_select_emit_queue_idle: AtomicBool::new(true),
            resource_loader: config.resource_loader.clone(),
            custom_tools: config.custom_tools.unwrap_or_default(),
            acp_mcp_tools: Mutex::new(Vec::new()),
            base_tool_definitions: Mutex::new(BTreeMap::new()),
            cwd: config.cwd.clone(),
            agent_dir: config.agent_dir.clone(),
            include_goals: config.include_goals.unwrap_or(true),
            include_compact_skill,
            session_start_event: config
                .session_start_event
                .unwrap_or_else(|| serde_json::json!({ "type": "session_start", "reason": "startup" })),
            disposed: AtomicBool::new(false),
            dispose_callbacks: Mutex::new(Vec::new()),
            disposing: AtomicBool::new(false),
            ipython_runtime_built: AtomicBool::new(false),
            prewarm_ipython_kernel: config.prewarm_ipython_kernel.unwrap_or(false) && rlm_depth == 0,
            rlm_depth,
            configured_rlm_max_depth: config.rlm_max_depth,
            rlm_max_depth: Mutex::new(2),
            rlm_max_depth_source: Mutex::new(RLM_MAX_DEPTH_SOURCE_DEFAULT.to_string()),
            semantic_edges,
            replied_to_parent_since_task: Mutex::new(replied_to_parent_since_task),
            parent_reply_count: AtomicU64::new(0),
            rlm_continuation: Mutex::new(empty_rlm_continuation_state()),
            rlm_explicit_replies_in_flight: Mutex::new(Vec::new()),
            rlm_unindexed_child_usage: Mutex::new(HashMap::new()),
            active_rlm_child_runs: Mutex::new(HashMap::new()),
            unsettled_rlm_child_runs: Mutex::new(Vec::new()),
            abandoned_rlm_quiescence_child_ids: Mutex::new(HashSet::new()),
            rlm_quiescence_wait_aborts: Mutex::new(Vec::new()),
            pending_rlm_subagent_session_names: Mutex::new(HashSet::new()),
            rlm_child_sessions: Mutex::new(HashMap::new()),
            deleted_rlm_child_ids: Mutex::new(HashSet::new()),
            rlm_child_cleanup_failures: Mutex::new(HashMap::new()),
            deleting_rlm_children: Mutex::new(HashMap::new()),
            rlm_child_unsubscribes: Mutex::new(HashMap::new()),
            model_registry: config.model_registry.clone(),
            tool_registry: Mutex::new(BTreeMap::new()),
            tool_definitions: Mutex::new(BTreeMap::new()),
            tool_prompt_snippets: Mutex::new(BTreeMap::new()),
            tool_prompt_guidelines: Mutex::new(BTreeMap::new()),
            base_system_prompt: Mutex::new(String::new()),
            assistant_turns_since_auto_refine: AtomicU64::new(0),
            last_auto_refine_review_at: Mutex::new(0.0),
            auto_refine_in_progress: AtomicBool::new(false),
            auto_refine_operations: Mutex::new(Vec::new()),
            scheduled_auto_refine_timers: Mutex::new(Vec::new()),
            compact_auto_refine_pending: AtomicBool::new(false),
            turn_interval_auto_refine_pending: AtomicBool::new(false),
            post_compaction_continuation_scheduled: AtomicBool::new(false),
            post_compaction_continuation_settlement: Mutex::new(None),
            post_compaction_continuation_messages: Mutex::new(Vec::new()),
            scheduled_post_compaction_continuation_messages: Mutex::new(Vec::new()),
            queued_autonomous_threshold_continuations: Mutex::new(HashMap::new()),
            queued_autonomous_continuation_snapshots: Mutex::new(HashMap::new()),
            pending_threshold_compaction_autonomous_messages: Mutex::new(Vec::new()),
            queued_goal_threshold_continuation: Mutex::new(None),
            pending_auto_refine_review: Mutex::new(None),
            auto_refine_branch_version: AtomicU64::new(0),
            serialized_refine: config.serialized_refine.unwrap_or(false),
            refine_in_flight: Mutex::new(None),
            refine_plan_in_flight: Mutex::new(None),
            serialized_plan_in_flight: Mutex::new(None),
            serialized_plan_claim: Mutex::new(None),
            serialized_explicit_refine_options: Mutex::new(None),
            extension_runner_ref: config
                .extension_runner_ref
                .unwrap_or_else(|| Arc::new(ExtensionRunnerRef::new(None))),
            initial_active_tool_names: config.initial_active_tool_names.clone(),
            allowed_tool_names: Mutex::new(
                config.allowed_tool_names.as_ref().map(|names| {
                    names.iter().cloned().collect::<HashSet<String>>()
                }),
            ),
            rlm_heartbeat_controller: Mutex::new(config.rlm_heartbeat_controller.clone()),
            agent_message_controller: config.agent_message_controller.clone(),
            agent_observe_controller: config.agent_observe_controller.clone(),
            mcp_manager: config.mcp_manager.clone(),
            base_tools_override: config.base_tools_override.clone(),
            subagent_runtime_host: Mutex::new(config.subagent_runtime_host.clone()),
            auto_refine_reviewer: config.auto_refine_reviewer.clone(),
            rlm_session_dir: config.rlm_session_dir.clone(),
            rlm_parent_node_id: config.rlm_parent_node_id.clone(),
            rlm_parent_agent: config.rlm_parent_agent.clone(),
            ipython_kernel_provisioner: Mutex::new(None),
            exec_env_provider: Mutex::new(None),
            auto_compaction_enabled: AtomicBool::new(true),
            last_assistant_message: Mutex::new(None),
            branch_navigation_queue: Mutex::new(Box::pin(async { Ok(()) })),
            provider_context_rebuilt_at: Mutex::new(None),
            steering_mode: Mutex::new("one-at-a-time".to_string()),
            follow_up_mode: Mutex::new("one-at-a-time".to_string()),
            recap: Mutex::new(None),
            unsubscribe_agent: Mutex::new(None),
            session_action_activity_notify: Arc::new(tokio::sync::Notify::new()),
            observed_action_deferrals: Mutex::new(HashMap::new()),
            resource_extension_paths: None,
            extension_command_context_actions: None,
            extension_error_listener: None,
            extension_shutdown_handler: None,
            own_usage_memo: Mutex::new(None),
        });

        let resolved_rlm_max_depth = session.resolve_rlm_max_depth()?;
        *session.rlm_max_depth.lock().unwrap() = resolved_rlm_max_depth.0;
        *session.rlm_max_depth_source.lock().unwrap() = resolved_rlm_max_depth.1;

        session.restore_rlm_continuation_state();
        *session.goal_state.lock().unwrap() = session.load_persisted_goal_state();
        // Seed initial goal from CLI --goal flag, but only for top-level sessions
        // and only when the branch contains only bootstrap entry types (model_change,
        // thinking_level_change, service_tier_change) and no persisted
        // thread_goal_state. This prevents reseeding after clear/complete/error
        // or restart/rehydration of a session that already has messages or a goal.
        if session.rlm_depth == 0 {
            if let Some(initial_goal) = config.initial_goal.as_ref() {
                if session.is_branch_seedable() {
                    let goal = session.start_goal(&initial_goal.objective, initial_goal.token_budget)?;
                    *session.goal_state.lock().unwrap() = goal.clone();
                    // Goal context is the model's only source of goal visibility; action
                    // admission is unavailable mid-construction, so ride the next turn.
                    let message = create_goal_context_message(
                        &goal,
                        crate::core::goals::GoalContextKind::Continuation,
                        None,
                    )?;
                    session.pending_next_turn_messages.lock().unwrap().push(message);
                }
            }
        }
        session.restore_late_ipython_sent_agent_messages();
        if session.goal_state.lock().unwrap().status == GoalStatus::Active {
            *session.goal_accounting_started_at.lock().unwrap() = Some(now_ms());
        }

        let listener_session = Arc::downgrade(&session);
        let unsubscribe = session.agent.subscribe(Arc::new(move |event: AgentEvent| {
            if let Some(session) = listener_session.upgrade() {
                session.handle_agent_event(event);
            }
        }));
        *session.unsubscribe_agent.lock().unwrap() = Some(Arc::new(unsubscribe));
        session.install_agent_tool_hooks();
        session.install_agent_turn_hook();
        session.install_agent_continuation_hook();

        session.build_runtime(
            session.initial_active_tool_names.clone(),
            true,
        );
        session.restore_provider_context_for_model();
        session.ensure_harness_digest_context();
        session.schedule_rlm_reload_backstop();
        Ok(session)
    }

    /// Refreshes MCP provider registrations without rebuilding the session runtime.
    pub fn refresh_mcp_providers(&self) {
        if let Some(manager) = &self.mcp_manager {
            manager.lock().unwrap().refresh();
        }
    }

    /// Set the RLM heartbeat controller after construction. Used by
    /// print/headless mode to attach an in-process heartbeat scheduler
    /// when the session is created outside the daemon.
    pub fn set_rlm_heartbeat_controller(self: &Arc<Self>, controller: Arc<dyn AgentRlmHeartbeatController>) {
        {
            let current = self.rlm_heartbeat_controller.lock().unwrap();
            if let Some(current) = current.as_ref() {
                if Arc::ptr_eq(current, &controller) {
                    return;
                }
            }
        }
        *self.rlm_heartbeat_controller.lock().unwrap() = Some(controller);
        self.build_runtime(Some(self.get_active_tool_names()), true);
        *self.base_system_prompt.lock().unwrap() = self.rebuild_system_prompt(&self.get_active_tool_names());
        let mut state = self.agent.state();
        state.system_prompt = self.base_system_prompt.lock().unwrap().clone();
        self.agent.set_state(state);
    }

    /// `replaceAcpMcpServers`.
    pub fn replace_acp_mcp_servers(
        &self,
        servers: &[crate::core::mcp::acp_mcp_types::AcpMcpServerConfig],
        owner_id: &str,
    ) -> Result<(), String> {
        if self.is_streaming() {
            return Err("Cannot replace ACP MCP servers while the agent is running".to_string());
        }
        let manager = match &self.mcp_manager {
            Some(manager) => manager,
            None => {
                if !servers.is_empty() {
                    return Err("MCP is unavailable in this session".to_string());
                }
                return Ok(());
            }
        };
        if !servers.is_empty() && self.ipython_kernel_provisioner.lock().unwrap().is_none() {
            return Err("ACP MCP servers require the built-in cpython tool".to_string());
        }
        let names = crate::core::tools::acp_mcp::acp_mcp_tool_names(servers)?;
        self.assert_acp_mcp_tool_names_available(&names)?;
        if !manager.lock().unwrap().replace_acp_servers(servers, owner_id) {
            return Ok(());
        }
        self.rebuild_runtime_for_acp_mcp_servers();
        Ok(())
    }

    /// `releaseAcpMcpServers`.
    pub async fn release_acp_mcp_servers(
        self: &Arc<Self>,
        owner_id: &str,
        server_names: &[String],
    ) -> Result<(), String> {
        let manager = match &self.mcp_manager {
            Some(manager) => manager.clone(),
            None => return Ok(()),
        };
        if !manager.lock().unwrap().can_release_acp_servers(owner_id) {
            return Ok(());
        }
        if manager.lock().unwrap().replace_acp_servers(&[], owner_id) {
            let removed_tool_names: HashSet<String> = self
                .acp_mcp_tools
                .lock()
                .unwrap()
                .iter()
                .map(|tool| tool.name.clone())
                .collect();
            let active_tool_names: Vec<String> = self
                .get_active_tool_names()
                .into_iter()
                .filter(|name| !removed_tool_names.contains(name))
                .collect();
            if let Some(allowed) = self.allowed_tool_names.lock().unwrap().as_mut() {
                for name in &removed_tool_names {
                    allowed.remove(name);
                }
            }
            *self.acp_mcp_tools.lock().unwrap() = Vec::new();
            self.refresh_tool_registry(Some(active_tool_names), true);
            *self.base_system_prompt.lock().unwrap() = self.rebuild_system_prompt(&self.get_active_tool_names());
            let mut state = self.agent.state();
            state.system_prompt = self.base_system_prompt.lock().unwrap().clone();
            self.agent.set_state(state);
        }
        let mut names: Vec<String> = Vec::new();
        for name in server_names {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        if names.is_empty() {
            return Ok(());
        }

        let input_pause = self.acquire_session_input_pause();
        let result = async {
            // Do not rebuild or kill the notebook. Wait for the current turn, then ask
            // the kernel-owned MCP registry to close only these cached transports.
            self.agent.wait_for_idle().await?;
            self.await_agent_event_queue().await;
            let provisioner = self.ipython_kernel_provisioner.lock().unwrap().clone();
            let manager = match provisioner.as_ref().and_then(|provisioner| provisioner.manager()) {
                Some(manager) => manager,
                None => return Ok(()),
            };
            if !manager.is_running() {
                return Ok(());
            }
            let code = [
                "import importlib as _prime_importlib".to_string(),
                "_prime_mcp = _prime_importlib.import_module(\"rlm.mcp\")".to_string(),
                format!(
                    "_prime_mcp_names = {}",
                    serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string())
                ),
                "_prime_mcp_errors = []".to_string(),
                "for _prime_mcp_name in _prime_mcp_names:".to_string(),
                "    try:".to_string(),
                "        await _prime_mcp.reload(_prime_mcp_name)".to_string(),
                "    except BaseException as _prime_mcp_error:".to_string(),
                "        _prime_mcp_errors.append(_prime_mcp_error)".to_string(),
                "if _prime_mcp_errors:".to_string(),
                "    raise _prime_mcp_errors[0]".to_string(),
                "del _prime_mcp, _prime_importlib, _prime_mcp_names, _prime_mcp_errors, _prime_mcp_name".to_string(),
            ]
            .join("\n");
            let result = manager.execute(&code, None, None).await?;
            if result.status != "ok" {
                let stderr = if result.stderr.is_empty() {
                    "kernel error".to_string()
                } else {
                    result.stderr.clone()
                };
                return Err(format!("Failed to close ACP MCP kernel transports: {stderr}"));
            }
            Ok(())
        }
        .await;
        input_pause.release();
        result
    }

    /// `_assertAcpMcpToolNamesAvailable`.
    fn assert_acp_mcp_tool_names_available(&self, names: &[String]) -> Result<(), String> {
        let mut occupied_names: HashSet<String> = HashSet::new();
        for name in self.base_tool_definitions.lock().unwrap().keys() {
            occupied_names.insert(name.clone());
        }
        for tool in &self.custom_tools {
            occupied_names.insert(tool.name.clone());
        }
        if let Some(runner) = self.extension_runner() {
            for tool in runner.get_all_registered_tools() {
                occupied_names.insert(tool.definition.name.clone());
            }
        }
        for name in names {
            if occupied_names.contains(name) {
                return Err(format!(
                    "ACP MCP tool name conflicts with an existing tool: {name}"
                ));
            }
        }
        Ok(())
    }

    /// `_rebuildRuntimeForAcpMcpServers`.
    fn rebuild_runtime_for_acp_mcp_servers(&self) {
        let previous_tool_names: HashSet<String> = self
            .acp_mcp_tools
            .lock()
            .unwrap()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        let servers = self
            .mcp_manager
            .as_ref()
            .map(|manager| manager.lock().unwrap().get_acp_servers())
            .unwrap_or_default();
        let next_tool_names = crate::core::tools::acp_mcp::acp_mcp_tool_names(&servers).unwrap_or_default();
        let _ = self.assert_acp_mcp_tool_names_available(&next_tool_names);
        let mut active_tool_names: Vec<String> = self
            .get_active_tool_names()
            .into_iter()
            .filter(|name| !previous_tool_names.contains(name))
            .collect();
        active_tool_names.extend(next_tool_names);
        self.build_runtime(Some(active_tool_names), true);
        *self.base_system_prompt.lock().unwrap() = self.rebuild_system_prompt(&self.get_active_tool_names());
        let mut state = self.agent.state();
        state.system_prompt = self.base_system_prompt.lock().unwrap().clone();
        self.agent.set_state(state);
    }

    /// `get modelRegistry()`.
    pub fn model_registry(&self) -> Arc<Mutex<crate::core::model_registry::ModelRegistry>> {
        self.model_registry.clone()
    }

    /// `setSubagentRuntimeHost`.
    pub fn set_subagent_runtime_host(&self, host: Option<Arc<dyn SubagentRuntimeHost>>) {
        *self.subagent_runtime_host.lock().unwrap() = host;
    }

    /**
     * Install tool hooks once on the Agent instance.
     *
     * The callbacks read `this._extensionRunner` at execution time, so extension reload swaps in the
     * new runner without reinstalling hooks. Extension-specific tool wrappers are still used to adapt
     * registered tool execution to the extension context. Tool call and tool result interception now
     * happens here instead of in wrappers.
     */
    fn install_agent_tool_hooks(self: &Arc<Self>) {
        let session = self.clone();
        self.agent
            .set_before_tool_call(Arc::new(move |context: BeforeToolCallContext| {
                let session = session.clone();
                Box::pin(async move {
                    let runner = match session.extension_runner() {
                        Some(runner) => runner,
                        None => return Ok(None),
                    };
                    if !runner.has_handlers("tool_call") {
                        return Ok(None);
                    }

                    session.await_agent_event_queue().await;

                    runner
                        .emit_tool_call(
                            &context.tool_call.name,
                            &context.tool_call.id,
                            context.args.clone(),
                        )
                        .await
                })
            }));

        let session = self.clone();
        self.agent
            .set_after_tool_call(Arc::new(move |context: AfterToolCallContext| {
                let session = session.clone();
                Box::pin(async move {
                    let runner = match session.extension_runner() {
                        Some(runner) => runner,
                        None => return Ok(None),
                    };
                    if !runner.has_handlers("tool_result") {
                        return Ok(None);
                    }

                    let hook_result = runner
                        .emit_tool_result(
                            &context.tool_call.name,
                            &context.tool_call.id,
                            context.args.clone(),
                            context.result.content.clone(),
                            context.result.details.clone(),
                            context.is_error,
                        )
                        .await?;

                    let hook_result = match hook_result {
                        Some(hook_result) => hook_result,
                        None => return Ok(None),
                    };

                    Ok(Some(pi_agent_core::types::AgentToolResult {
                        content: hook_result.content,
                        details: hook_result.details,
                        terminate: None,
                    }))
                })
            }));
    }

    /// `_installAgentContinuationHook`.
    fn install_agent_continuation_hook(self: &Arc<Self>) {
        let session = self.clone();
        self.agent.set_get_continuation_messages(Arc::new(
            move |context: AgentContext, signal: Option<CancellationToken>| {
                let session = session.clone();
                Box::pin(async move { session.get_continuation_messages(context, signal).await })
            },
        ));
    }

    /// `_installAgentTurnHook`.
    fn install_agent_turn_hook(self: &Arc<Self>) {
        let session = self.clone();
        self.agent
            .set_should_stop_before_turn(Arc::new(move || session.should_stop_before_turn()));
        let session = self.clone();
        self.agent
            .set_should_stop_after_turn(Arc::new(move |context: ShouldStopAfterTurnContext| {
                let session = session.clone();
                Box::pin(async move { session.should_stop_after_turn(context).await })
            }));
    }

    /// `_emit`.
    fn emit(&self, event: AgentSessionEvent) {
        let listeners = self.event_listeners.lock().unwrap().clone();
        for listener in listeners {
            // A failing observer must not prevent other subscribers from
            // receiving lifecycle and persistence events.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(event.clone())));
        }
    }

    /// `_emitQueueUpdate`.
    fn emit_queue_update(&self) {
        let actions = self.get_session_action_snapshot();
        {
            let mut last = self.last_session_action_snapshot.lock().unwrap();
            if session_action_snapshots_equal(&actions, &last) {
                return;
            }
            *last = actions.clone();
        }
        self.emit(AgentSessionEvent::SessionActionUpdate { actions });
    }

    /// `_restoreLateIpythonSentAgentMessages`.
    fn restore_late_ipython_sent_agent_messages(&self) {
        self.late_ipython_sent_agent_messages.lock().unwrap().clear();
        for entry in self.session_manager.lock().unwrap().get_branch(None) {
            if entry.get("type").and_then(Value::as_str) != Some("custom") {
                continue;
            }
            if entry.get("customType").and_then(Value::as_str)
                != Some(IPYTHON_SENT_AGENT_MESSAGE_CUSTOM_ENTRY)
            {
                continue;
            }
            let data = entry.get("data").cloned().unwrap_or(Value::Null);
            if let Some(persisted) = parse_persisted_ipython_sent_agent_message(&data) {
                self.remember_late_ipython_sent_agent_message(
                    &persisted.tool_call_id,
                    &persisted.message,
                );
            }
        }
    }

    /// `_rememberLateIpythonSentAgentMessage`.
    fn remember_late_ipython_sent_agent_message(&self, tool_call_id: &str, message: &Value) -> bool {
        let mut map = self.late_ipython_sent_agent_messages.lock().unwrap();
        let messages = map.entry(tool_call_id.to_string()).or_default();
        let message_id = message.get("id").cloned().unwrap_or(Value::Null);
        let is_new = !messages
            .iter()
            .any(|entry| entry.get("id") == Some(&message_id));
        if is_new {
            messages.push(message.clone());
        }
        drop(map);
        let mut state = self.agent.state();
        for index in (0..state.messages.len()).rev() {
            if append_sent_agent_message_to_tool_result(
                &mut state.messages[index],
                tool_call_id,
                message,
            ) {
                break;
            }
        }
        self.agent.set_state(state);
        is_new
    }

    /// `_applyLateIpythonSentAgentMessages`.
    fn apply_late_ipython_sent_agent_messages(&self, message: &mut AgentMessage) {
        let (tool_name, tool_call_id) = match message {
            AgentMessage::Message(Message::ToolResult(tool_result)) => (
                tool_result.tool_name.clone(),
                tool_result.tool_call_id.clone(),
            ),
            _ => return,
        };
        if tool_name != "ipython" {
            return;
        }
        let messages = self
            .late_ipython_sent_agent_messages
            .lock()
            .unwrap()
            .get(&tool_call_id)
            .cloned()
            .unwrap_or_default();
        for sent_message in messages {
            append_sent_agent_message_to_tool_result(message, &tool_call_id, &sent_message);
        }
    }

    /// `_recordLateIpythonSentAgentMessage`.
    fn record_late_ipython_sent_agent_message(self: &Arc<Self>, tool_call_id: &str, message: Value) {
        let session = self.clone();
        let tool_call_id = tool_call_id.to_string();
        self.push_agent_event_task(async move {
            if session.disposed.load(Ordering::SeqCst)
                || !session.remember_late_ipython_sent_agent_message(&tool_call_id, &message)
            {
                return;
            }
            let data = serde_json::json!({ "toolCallId": tool_call_id, "message": message });
            let _ = session
                .session_manager
                .lock()
                .unwrap()
                .append_custom_entry(IPYTHON_SENT_AGENT_MESSAGE_CUSTOM_ENTRY, Some(data));
            session.emit(AgentSessionEvent::IpythonSentAgentMessage {
                tool_call_id,
                message,
            });
        });
    }

    /// `_emitGoalUpdate`.
    fn emit_goal_update(&self) {
        self.emit(AgentSessionEvent::GoalUpdate {
            goal: self.goal_state(),
        });
    }

    /// `_loadPersistedRlmMaxDepthState`.
    fn load_persisted_rlm_max_depth_state(&self) -> Option<PersistedRlmMaxDepthState> {
        let branch = self.session_manager.lock().unwrap().get_branch(None);
        for entry in branch.iter().rev() {
            if entry.get("type").and_then(Value::as_str) != Some("custom") {
                continue;
            }
            if entry.get("customType").and_then(Value::as_str)
                != Some(RLM_MAX_DEPTH_STATE_CUSTOM_TYPE)
            {
                continue;
            }
            let data = entry.get("data").cloned().unwrap_or(Value::Null);
            if is_persisted_rlm_max_depth_state(&data) {
                return data
                    .get("maxDepth")
                    .and_then(Value::as_f64)
                    .map(|max_depth| PersistedRlmMaxDepthState { max_depth });
            }
        }
        None
    }

    /// `_resolveRlmMaxDepth`.
    fn resolve_rlm_max_depth(&self) -> Result<(i64, RlmMaxDepthSource), String> {
        if let Some(persisted) = self.load_persisted_rlm_max_depth_state() {
            return Ok((persisted.max_depth as i64, RLM_MAX_DEPTH_SOURCE_CHAT.to_string()));
        }
        if let Some(configured) = self.configured_rlm_max_depth {
            return Ok((configured, RLM_MAX_DEPTH_SOURCE_INHERITED.to_string()));
        }
        let global = self.settings_manager.lock().unwrap().get_rlm_max_depth();
        if let Some(global) = global {
            if is_non_negative_integer(global) {
                return Ok((global as i64, RLM_MAX_DEPTH_SOURCE_GLOBAL.to_string()));
            }
        }
        let env = std::env::var("RLM_MAX_DEPTH").ok();
        if let Some(env) = env {
            if !env.is_empty() {
                return Ok((parse_depth(Some(&env), 1, "RLM_MAX_DEPTH")?, RLM_MAX_DEPTH_SOURCE_ENV.to_string()));
            }
        }
        Ok((2, RLM_MAX_DEPTH_SOURCE_DEFAULT.to_string()))
    }

    /// `_loadPersistedGoalState`.
    fn load_persisted_goal_state(&self) -> GoalState {
        let branch = self.session_manager.lock().unwrap().get_branch(None);
        for entry in branch.iter().rev() {
            if entry.get("type").and_then(Value::as_str) != Some("custom") {
                continue;
            }
            if entry.get("customType").and_then(Value::as_str) != Some(GOAL_STATE_CUSTOM_TYPE) {
                continue;
            }
            let data = entry.get("data").cloned().unwrap_or(Value::Null);
            if is_persisted_goal_state(&data) {
                if let Ok(goal) = serde_json::from_value::<GoalState>(data) {
                    return normalize_goal_state(&goal);
                }
            }
        }
        empty_goal_state()
    }

    /**
     * Whether the session branch is seedable for an initial goal. Returns true
     * only when the branch contains exclusively bootstrap entry types
     * (model_change, thinking_level_change, service_tier_change) and no
     * thread_goal_state custom entry. Any message, custom entry, or persisted
     * goal (including cleared/complete/error) means the session has been used
     * and should not be reseeded.
     */
    fn is_branch_seedable(&self) -> bool {
        let branch = self.session_manager.lock().unwrap().get_branch(None);
        for entry in branch {
            match entry.get("type").and_then(Value::as_str).unwrap_or_default() {
                "model_change" | "thinking_level_change" | "service_tier_change" => continue,
                "custom" => {
                    // The TypeScript returns false for the goal state type and for
                    // every other custom entry, so the branch is never seedable
                    // once a custom entry exists.
                    return false;
                }
                _ => return false,
            }
        }
        true
    }

    /// `_reloadGoalStateFromBranch`.
    fn reload_goal_state_from_branch(&self) {
        *self.goal_state.lock().unwrap() = self.load_persisted_goal_state();
        *self.goal_accounting_started_at.lock().unwrap() =
            if self.goal_state.lock().unwrap().status == GoalStatus::Active {
                Some(now_ms())
            } else {
                None
            };
        self.emit_goal_update();
    }

    /// `_reloadRlmMaxDepthFromBranch`.
    fn reload_rlm_max_depth_from_branch(&self) {
        let previous_max_depth = *self.rlm_max_depth.lock().unwrap();
        let resolved = self.resolve_rlm_max_depth().unwrap_or((2, RLM_MAX_DEPTH_SOURCE_DEFAULT.to_string()));
        *self.rlm_max_depth.lock().unwrap() = resolved.0;
        *self.rlm_max_depth_source.lock().unwrap() = resolved.1;
        if resolved.0 != previous_max_depth {
            *self.base_system_prompt.lock().unwrap() = self.rebuild_system_prompt(&self.get_active_tool_names());
            let mut state = self.agent.state();
            state.system_prompt = self.base_system_prompt.lock().unwrap().clone();
            self.agent.set_state(state);
        }
    }

    /// `_persistGoalState`.
    fn persist_goal_state(&self, goal: &GoalState) {
        let value = serde_json::to_value(goal).unwrap_or(Value::Null);
        let _ = self
            .session_manager
            .lock()
            .unwrap()
            .append_custom_entry(GOAL_STATE_CUSTOM_TYPE, Some(value));
        // Force flush so the goal state is durable on disk immediately,
        // even before the first assistant response. This ensures idempotent
        // restart/rehydration can detect the persisted goal.
        let _ = self.session_manager.lock().unwrap().flush_now();
    }

    /// `_setGoalState`.
    fn set_goal_state(&self, next: &GoalState, persist: Option<bool>) {
        let mut candidate = next.clone();
        candidate.updated_at = Some(now_ms());
        let normalized = normalize_goal_state(&candidate);
        *self.goal_state.lock().unwrap() = normalized.clone();
        if normalized.status == GoalStatus::Active {
            let mut started_at = self.goal_accounting_started_at.lock().unwrap();
            if started_at.is_none() {
                *started_at = Some(now_ms());
            }
        } else {
            *self.goal_accounting_started_at.lock().unwrap() = None;
        }
        if persist != Some(false) {
            self.persist_goal_state(&normalized);
        }
        self.emit_goal_update();
    }

    /// `_goalWithCurrentWallClock`.
    fn goal_with_current_wall_clock(&self, now: Option<f64>) -> GoalState {
        let now = now.unwrap_or_else(now_ms);
        let goal = self.goal_state.lock().unwrap().clone();
        if goal.status != GoalStatus::Active {
            return goal;
        }
        let started_at = match *self.goal_accounting_started_at.lock().unwrap() {
            Some(started_at) => started_at,
            None => return goal,
        };
        let elapsed_seconds = ((now - started_at) / 1000.0).floor();
        if elapsed_seconds <= 0.0 {
            return goal;
        }
        GoalState {
            time_used_seconds: goal.time_used_seconds + elapsed_seconds,
            ..goal
        }
    }

    /// `_goalWithAccountedWallClock`.
    fn goal_with_accounted_wall_clock(&self) -> GoalState {
        let now = now_ms();
        let goal = self.goal_with_current_wall_clock(Some(now));
        if goal.time_used_seconds != self.goal_state.lock().unwrap().time_used_seconds {
            *self.goal_accounting_started_at.lock().unwrap() = Some(now);
        }
        goal
    }

    /// `_cancelSessionActions`.
    fn cancel_session_actions(
        &self,
        predicate: &dyn Fn(&QueuedSessionAction) -> bool,
        error: &str,
        candidates: Option<Vec<QueuedSessionAction>>,
    ) -> Vec<QueuedSessionAction> {
        let candidates = match candidates {
            Some(candidates) => candidates,
            None => self.action_store.lock().unwrap().clearable_actions(None),
        };
        let matching: Vec<QueuedSessionAction> = candidates
            .iter()
            .filter(|action| predicate(action))
            .cloned()
            .collect();
        let previous_states: HashMap<String, ActionLifecycleState> = matching
            .iter()
            .map(|action| (action.id.clone(), action.lifecycle.state()))
            .collect();
        let preparing: Vec<QueuedSessionAction> = self
            .action_store
            .lock()
            .unwrap()
            .active_actions(None)
            .into_iter()
            .filter(|action| {
                matches!(action.payload, QueuedActionPayload::Turn(_))
                    && action.lifecycle.state() == ActionLifecycleState::Preparing
            })
            .collect();
        let previous_anchor = preparing.last().cloned();
        let actions = self
            .action_store
            .lock()
            .unwrap()
            .remove(predicate, Some(candidates.as_slice()))
            .unwrap_or_default();
        let mut restorable_messages: Vec<CustomMessage> = Vec::new();
        let removed: HashSet<String> = actions.iter().map(|action| action.id.clone()).collect();
        if let Some(previous_anchor) = previous_anchor {
            if removed.contains(&previous_anchor.id) {
                for action in &preparing {
                    if removed.contains(&action.id) {
                        continue;
                    }
                    let mut next = action.clone();
                    if let QueuedActionPayload::Turn(turn) = &mut next.payload {
                        turn.prepared = None;
                    }
                    let _ = self.action_store.lock().unwrap().update_action(&next);
                }
            }
        }
        for action in &actions {
            let ticket = self.action_store.lock().unwrap().ticket_for(action);
            let previous_state = previous_states
                .get(&action.id)
                .copied()
                .unwrap_or(ActionLifecycleState::Queued);
            match &action.payload {
                QueuedActionPayload::Turn(turn) => {
                    if turn.accepted_agent_message
                        || !turn.queue_visible
                        || previous_state != ActionLifecycleState::Queued
                    {
                        if let Ok(ticket) = &ticket {
                            ticket.reject_delivered(error.to_string());
                        }
                    } else if let Ok(ticket) = &ticket {
                        ticket.settle_delivered(DeliveryOutcome::NotApplicable);
                    }
                }
                QueuedActionPayload::SessionCommand(_) => {
                    if let Ok(ticket) = &ticket {
                        ticket.settle_delivered(DeliveryOutcome::NotApplicable);
                    }
                }
            }
            if let Ok(ticket) = &ticket {
                ticket.settle_completed(Some(error.to_string()));
            }
            let dispatched = previous_state == ActionLifecycleState::Committing
                && matches!(action.payload, QueuedActionPayload::Turn(_));
            if let QueuedActionPayload::Turn(payload) = &action.payload {
                for record in &payload.base.records {
                    let is_restorable = (record.role == DeliveryRecordRole::NextTurn
                        || (payload.accepted_agent_message
                            && record.role == DeliveryRecordRole::Prefix))
                        && matches!(record.message, DeliveryMessage::Custom(_))
                        && record_message_custom_type(&record.message).as_deref()
                            != Some(HARNESS_DIGEST_CUSTOM_TYPE)
                        && !record.durable;
                    if is_restorable {
                        if let DeliveryMessage::Custom(custom) = &record.message {
                            restorable_messages.push(clone_custom_message(custom));
                        }
                    }
                }
                // Lazy injection owns digest delivery: a cancelled turn re-arms it
                // instead of restoring a possibly stale digest message.
                if payload.base.records.iter().any(|record| {
                    record_message_custom_type(&record.message).as_deref()
                        == Some(HARNESS_DIGEST_CUSTOM_TYPE)
                }) {
                    self.harness_digest_pending.store(true, Ordering::SeqCst);
                }
                if dispatched {
                    let mut next = action.clone();
                    if let QueuedActionPayload::Turn(turn) = &mut next.payload {
                        turn.capture_run_messages = Some(
                            payload
                                .base
                                .records
                                .iter()
                                .map(|record| delivery_message_key_of(&record.message))
                                .collect(),
                        );
                    }
                    let _ = self.action_store.lock().unwrap().update_action(&next);
                }
            }
            if !dispatched {
                let _ = self.action_store.lock().unwrap().release_terminal(action);
            }
        }
        {
            let mut pending = self.pending_next_turn_messages.lock().unwrap();
            for message in restorable_messages.into_iter().rev() {
                pending.insert(0, message);
            }
        }
        if !actions.is_empty() {
            self.notify_session_input_checkpoint_change();
        }
        actions
    }


    /// `_clearQueuedGoalContexts`.
    fn clear_queued_goal_contexts(&self) {
        self.goal_continuation_awaits_rlm_work.store(false, Ordering::SeqCst);
        self.pending_next_turn_messages
            .lock()
            .unwrap()
            .retain(|message| message.custom_type != GOAL_CONTEXT_CUSTOM_TYPE);
        self.agent
            .remove_queued_messages(Arc::new(|message: &AgentMessage| {
                matches!(
                    message,
                    AgentMessage::Custom(CustomAgentMessage::Custom { custom_type, .. })
                        if custom_type == GOAL_CONTEXT_CUSTOM_TYPE
                )
            }));
        self.cancel_session_actions(
            &|action| match &action.payload {
                QueuedActionPayload::Turn(turn) => turn
                    .custom_message
                    .as_ref()
                    .map(|message| message.custom_type == GOAL_CONTEXT_CUSTOM_TYPE)
                    .unwrap_or(false),
                QueuedActionPayload::SessionCommand(_) => false,
            },
            "Queued goal context was cleared before delivery.",
            None,
        );
        self.emit_queue_update();
    }

    /// `_startGoal`.
    fn start_goal(&self, objective_text: &str, token_budget: Option<f64>) -> Result<GoalState, String> {
        let objective = validate_goal_objective(objective_text)?;
        let budget = validate_goal_budget(token_budget)?;
        let now = now_ms();
        let goal = GoalState {
            active: true,
            status: GoalStatus::Active,
            goal_id: Some(uuid::Uuid::new_v4().to_string()),
            objective: Some(objective),
            token_budget: budget,
            tokens_used: 0.0,
            time_used_seconds: 0.0,
            continuations_used: 0.0,
            created_at: Some(now),
            updated_at: Some(now),
            last_reason: None,
            last_error: None,
        };
        *self.goal_accounting_started_at.lock().unwrap() = Some(now);
        self.goal_continuation_awaits_rlm_work.store(false, Ordering::SeqCst);
        self.set_goal_state(&goal, None);
        Ok(self.goal_state.lock().unwrap().clone())
    }

    /// `_clearGoal`.
    fn clear_goal(&self) {
        self.clear_queued_goal_contexts();
        self.set_goal_state(&empty_goal_state(), None);
    }

    /// `_pauseGoal`.
    fn pause_goal(&self, reason: Option<&str>) {
        self.clear_queued_goal_contexts();
        if self.goal_state.lock().unwrap().status != GoalStatus::Active {
            self.emit_goal_update();
            return;
        }
        let goal = self.goal_with_accounted_wall_clock();
        let next = GoalState {
            active: false,
            status: GoalStatus::Paused,
            last_reason: Some(reason.unwrap_or("Paused by user").to_string()),
            last_error: None,
            ..goal
        };
        self.set_goal_state(&next, None);
    }

    /// `_resumeGoal`.
    async fn resume_goal(self: &Arc<Self>) {
        if self.goal_state.lock().unwrap().objective.is_none() {
            self.emit_goal_update();
            return;
        }
        {
            let goal = self.goal_state.lock().unwrap();
            if goal.status != GoalStatus::Paused && goal.status != GoalStatus::BudgetLimited {
                drop(goal);
                self.emit_goal_update();
                return;
            }
        }
        let (exhausted, goal) = {
            let goal = self.goal_state.lock().unwrap().clone();
            let exhausted = match goal.token_budget {
                Some(budget) => goal.tokens_used >= budget,
                None => false,
            };
            (exhausted, goal)
        };
        let next_status = if exhausted {
            GoalStatus::BudgetLimited
        } else {
            GoalStatus::Active
        };
        let next = GoalState {
            active: next_status == GoalStatus::Active,
            status: next_status.clone(),
            last_reason: if exhausted {
                Some("Goal token budget already reached".to_string())
            } else {
                None
            },
            last_error: None,
            ..goal
        };
        self.set_goal_state(&next, None);
        if next_status == GoalStatus::Active {
            self.run_or_queue_goal_context("continuation", None);
        }
    }

    /// `_finishGoalWithError`.
    fn finish_goal_with_error(&self, error_message: &str) {
        {
            let goal = self.goal_state.lock().unwrap();
            if goal.objective.is_none() || goal.status != GoalStatus::Active {
                return;
            }
        }
        let goal = self.goal_with_accounted_wall_clock();
        let next = GoalState {
            active: false,
            status: GoalStatus::Error,
            last_reason: Some(error_message.to_string()),
            last_error: Some(error_message.to_string()),
            ..goal
        };
        self.set_goal_state(&next, None);
    }

    /// `_finishGoalForTerminalAssistantMessage`.
    fn finish_goal_for_terminal_assistant_message(&self, message: &AssistantMessage) {
        if self.goal_state.lock().unwrap().status != GoalStatus::Active {
            return;
        }

        if message.stop_reason == STOP_REASON_ABORTED {
            self.goal_abort_in_progress.store(false, Ordering::SeqCst);
            return;
        }

        if message.stop_reason == STOP_REASON_ERROR {
            if self.goal_abort_in_progress.load(Ordering::SeqCst) {
                self.goal_abort_in_progress.store(false, Ordering::SeqCst);
                return;
            }
            let error_message = message
                .error_message
                .clone()
                .unwrap_or_else(|| "Assistant response failed".to_string());
            self.finish_goal_with_error(&error_message);
        }
    }

    /// `_stopGoalContinuationForTerminalMessage`.
    fn stop_goal_continuation_for_terminal_message(&self, message: &AssistantMessage) -> bool {
        if message.stop_reason != STOP_REASON_ERROR && message.stop_reason != STOP_REASON_ABORTED {
            return false;
        }
        // Goal hooks must not reject; listener failures should not crash the agent loop.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.finish_goal_for_terminal_assistant_message(message);
        }));
        true
    }

    /// `_parseGoalSlashCommand`.
    fn parse_goal_slash_command(&self, text: &str) -> Result<Option<GoalSlashCommand>, String> {
        let command = match parse_session_slash_command(text) {
            Some(command) => command,
            None => return Ok(None),
        };
        if command.name != "goal" {
            return Ok(None);
        }

        let rest = command.args.clone();
        let normalized = rest.to_lowercase();
        if rest.is_empty() || normalized == "status" {
            return Ok(Some(GoalSlashCommand::Status));
        }
        if normalized == "clear" || normalized == "stop" {
            return Ok(Some(GoalSlashCommand::Clear));
        }
        if normalized == "pause" {
            return Ok(Some(GoalSlashCommand::Pause));
        }
        if normalized == "resume" {
            return Ok(Some(GoalSlashCommand::Resume));
        }

        let mut token_budget: Option<f64> = None;
        let mut objective = rest.clone();
        let first_token = rest
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        if first_token == "--budget"
            || first_token == "--token-budget"
            || first_token.starts_with("--budget=")
            || first_token.starts_with("--token-budget=")
        {
            let value_text: String;
            if first_token == "--budget" || first_token == "--token-budget" {
                let without_flag = rest[first_token.len()..].trim_start();
                let next_space = without_flag.find(char::is_whitespace);
                match next_space {
                    None => return Err("Usage: /goal [--budget <tokens>] <objective>".to_string()),
                    Some(next_space) => {
                        value_text = without_flag[..next_space].to_string();
                        objective = without_flag[next_space + 1..].trim().to_string();
                    }
                }
            } else {
                let separator = first_token.find('=').unwrap_or_default();
                value_text = first_token[separator + 1..].to_string();
                objective = rest[first_token.len()..].trim().to_string();
            }
            token_budget = Some(parse_goal_budget_value(&value_text)?);
        }

        Ok(Some(GoalSlashCommand::Start {
            objective: validate_goal_objective(&objective)?,
            token_budget,
        }))
    }

    /// `_parseAutonomousSlashCommand`.
    fn parse_autonomous_slash_command(&self, text: &str) -> Result<Option<AutonomousSlashCommand>, String> {
        let command = match parse_session_slash_command(text) {
            Some(command) => command,
            None => return Ok(None),
        };
        if command.name != "autonomous" {
            return Ok(None);
        }
        let rest = command.args.to_lowercase();
        if rest.is_empty() || rest == "status" {
            return Ok(Some(AutonomousSlashCommand::Status));
        }
        if rest == "on" || rest == "enable" || rest == "enabled" {
            return Ok(Some(AutonomousSlashCommand::On));
        }
        if rest == "off" || rest == "disable" || rest == "disabled" {
            return Ok(Some(AutonomousSlashCommand::Off));
        }
        Err("Usage: /autonomous [on|off|status]".to_string())
    }

    /// `_formatAutonomousStatus`.
    fn format_autonomous_status(&self) -> String {
        let status = self.get_autonomous_status();
        let state = if status.enabled { "on" } else { "off" };
        let mut line = format!(
            "Autonomous continuation: {state} | continuations used: {} | quality gate attempts: {}",
            status.continuations_used, status.gate_attempts
        );
        if let Some(failure) = &status.last_gate_failure {
            line.push_str(&format!(" | last gate failure: {failure}"));
        }
        line
    }

    /// `_emitAutonomousStatus`.
    fn emit_autonomous_status(&self) {
        let content = self.format_autonomous_status();
        let message = create_custom_message(
            "session_status".to_string(),
            CustomMessageContent::Text(content),
            true,
            None,
            &now_iso(),
        );
        let _ = self.append_durable_status_message(message);
    }

    /// `_handleAutonomousSlashCommand`.
    async fn handle_autonomous_slash_command(self: &Arc<Self>, text: &str) -> Result<bool, String> {
        let command = match self.parse_autonomous_slash_command(text)? {
            Some(command) => command,
            None => return Ok(false),
        };
        match command {
            AutonomousSlashCommand::Status => self.emit_autonomous_status(),
            AutonomousSlashCommand::On => {
                {
                    let mut state = self.autonomous_state.lock().unwrap();
                    set_autonomous_enabled(&mut state, true);
                }
                self.emit_autonomous_status();
            }
            AutonomousSlashCommand::Off => {
                {
                    let mut state = self.autonomous_state.lock().unwrap();
                    set_autonomous_enabled(&mut state, false);
                }
                self.emit_autonomous_status();
            }
        }
        Ok(true)
    }

    /// `_appendBeforeAgentStartMessages`.
    fn append_before_agent_start_messages(&self, messages: &[AgentMessage]) {
        if messages.is_empty() {
            return;
        }
        for message in messages {
            let _ = self.session_manager.lock().unwrap().append_message(message.clone());
        }
        let mut state = self.agent.state();
        state.messages.extend(messages.iter().cloned());
        self.agent.set_state(state);
    }

    /// `_validateCanStartAgentRun`.
    async fn validate_can_start_agent_run(&self) -> Result<(), String> {
        let state = self.agent.state();
        let model = state.model.clone();
        if model.id.is_empty() {
            return Err(format_no_model_selected_message());
        }
        let auth = self.get_required_request_auth(&model).await;
        if let Err(error) = auth {
            return Err(add_login_guidance_to_auth_error(&error));
        }
        Ok(())
    }

    /// `_ensureGoalRuntimeActive`.
    fn ensure_goal_runtime_active(&self, _context: Option<&AgentContext>) {
        if self.goal_state.lock().unwrap().status != GoalStatus::Active {
            return;
        }
        if self.goal_accounting_started_at.lock().unwrap().is_none() {
            *self.goal_accounting_started_at.lock().unwrap() = Some(now_ms());
        }
    }

    /// `_maybeResumeGoalContinuationAfterRlmWork`.
    fn maybe_resume_goal_continuation_after_rlm_work(self: &Arc<Self>) {
        if !self.goal_continuation_awaits_rlm_work.swap(false, Ordering::SeqCst) {
            return;
        }
        if self.goal_state.lock().unwrap().status != GoalStatus::Active {
            return;
        }
        if self.has_running_rlm_children() {
            self.goal_continuation_awaits_rlm_work.store(true, Ordering::SeqCst);
            return;
        }
        self.run_or_queue_goal_context("continuation", None);
    }

    /// `_runOrQueueGoalContext`.
    fn run_or_queue_goal_context(&self, kind: &str, images: Option<Vec<ImageContent>>) {
        let goal = self.goal_state();
        let message = match create_goal_context_message(
            &goal,
            goal_context_kind_of(kind),
            images.map(|images| {
                images
                    .iter()
                    .map(|image| serde_json::to_value(image).unwrap_or(Value::Null))
                    .collect()
            }),
        ) {
            Ok(message) => message,
            Err(_) => return,
        };
        self.pending_next_turn_messages.lock().unwrap().push(message);
    }

    /// `_handleGoalSlashCommand`.
    async fn handle_goal_slash_command(self: &Arc<Self>, text: &str) -> Result<bool, String> {
        let command = match self.parse_goal_slash_command(text)? {
            Some(command) => command,
            None => return Ok(false),
        };
        match command {
            GoalSlashCommand::Status => {
                self.emit_goal_update();
            }
            GoalSlashCommand::Clear => {
                self.clear_goal();
            }
            GoalSlashCommand::Pause => {
                self.pause_goal(None);
            }
            GoalSlashCommand::Resume => {
                self.resume_goal().await;
            }
            GoalSlashCommand::Start {
                objective,
                token_budget,
            } => {
                self.start_goal(&objective, token_budget)?;
                self.run_or_queue_goal_context("continuation", None);
            }
        }
        Ok(true)
    }

    /// `_accountGoalUsageForAssistantMessage`.
    fn account_goal_usage_for_assistant_message(&self, message: &AssistantMessage) -> bool {
        let goal = self.goal_state.lock().unwrap().clone();
        if goal.status != GoalStatus::Active {
            return false;
        }
        let key = assistant_message_key(message);
        {
            let mut accounted = self.goal_accounted_assistant_messages.lock().unwrap();
            if accounted.contains(&key) {
                return false;
            }
            accounted.insert(key);
        }
        let delta = goal_token_delta_for_usage(crate::core::goals::GoalUsage {
            input: message.usage.input,
            output: message.usage.output,
            cache_read: message.usage.cache_read,
            cache_write: message.usage.cache_write,
            total_tokens: message.usage.total_tokens,
        });
        let mut next = self.goal_state.lock().unwrap().clone();
        next.tokens_used += delta;
        if let Some(budget) = next.token_budget {
            if next.tokens_used >= budget {
                next.active = false;
                next.status = GoalStatus::BudgetLimited;
                next.last_reason = Some("Goal token budget reached".to_string());
            }
        }
        self.set_goal_state(&next, None);
        true
    }

    /// `get _steeringStopPending()`.
    fn steering_stop_pending(&self) -> bool {
        self.agent
            .has_queued_messages()
            && self.steering_mode.lock().unwrap().as_str() == "one-at-a-time"
    }

    /// `_shouldStopBeforeTurn`.
    fn should_stop_before_turn(&self) -> bool {
        self.steering_stop_pending()
    }

    /// `_shouldStopAfterTurn`.
    async fn should_stop_after_turn(self: &Arc<Self>, context: ShouldStopAfterTurnContext) -> bool {
        if self.stop_goal_continuation_for_terminal_message(&context.message) {
            return true;
        }
        if self.account_goal_usage_for_assistant_message(&context.message) {
            if let Ok(message) = create_goal_context_message(
                &self.goal_state(),
                GoalContextKind::BudgetLimit,
                None,
            ) {
                let normalized = normalize_message_content(&CustomMessageContent::Text(
                    message
                        .content
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_default(),
                ));
                self.queue_prepared_prompt(
                    "steer",
                    &normalized.0,
                    normalized.1,
                    Some(PreparedTurnActionOptions {
                        custom_message: Some(message),
                        resume_if_idle: Some(true),
                        ..Default::default()
                    }),
                )
                .await;
            }
        }
        if self.serialized_refine {
            self.await_agent_event_queue().await;
            self.run_serialized_refine_checkpoint().await;
        }
        if self.should_stop_for_threshold_compaction(&context).await {
            return true;
        }
        // Steering stops continuation only after mandatory serialized checkpoints.
        self.steering_stop_pending()
        if self.should_stop_for_threshold_compaction(&context).await {
            return true;
        }
        if self.goal_state.lock().unwrap().status != GoalStatus::Active {
            return false;
        }
        if self.goal_continuation_awaits_rlm_work.load(Ordering::SeqCst) {
            return true;
        }
        if self.has_running_rlm_children() {
            self.goal_continuation_awaits_rlm_work.store(true, Ordering::SeqCst);
            return true;
        }
        if self.goal_abort_in_progress.load(Ordering::SeqCst) {
            return true;
        }
        let goal = self.goal_with_accounted_wall_clock();
        if let Some(budget) = goal.token_budget {
            if goal.tokens_used >= budget {
                let next = GoalState {
                    active: false,
                    status: GoalStatus::BudgetLimited,
                    last_reason: Some("Goal token budget reached".to_string()),
                    ..goal
                };
                self.set_goal_state(&next, None);
                return false;
            }
        }
        self.run_or_queue_goal_context("continuation", None);
        true
    }

    /// `_shouldStopForThresholdCompaction(context)`.
    async fn should_stop_for_threshold_compaction(
        self: &Arc<Self>,
        context: &ShouldStopAfterTurnContext,
    ) -> bool {
        self.continue_after_threshold_compaction.store(false, Ordering::SeqCst);
        if self.pending_requested_compaction.lock().unwrap().is_none()
            && !self.threshold_compaction_needed(context).await
        {
            return false;
        }

        let last_message = self.agent.state().messages.last().cloned();
        if self.pending_requested_compaction.lock().unwrap().is_some() {
            self.await_agent_event_queue().await;
            let continuation = self
                .handle_rlm_child_turn_outcome(&context.message, true, Some("requested"))
                .map(|outcome| outcome.continuation)
                .unwrap_or(false);
            if continuation {
                self.continue_after_threshold_compaction.store(true, Ordering::SeqCst);
            }
        }
        // A queued continuation disproves the assistant-last "task finished" heuristic,
        // so preserve a true set above.
        let last_is_assistant = last_message
            .as_ref()
            .map(|message| message.role() == "assistant")
            .unwrap_or(false);
        if !last_is_assistant
            && self.continue_after_threshold_compaction.load(Ordering::SeqCst) == false
        {
            self.continue_after_threshold_compaction
                .store(true, Ordering::SeqCst);
        }
        true
    }

    /// `_thresholdCompactionNeeded(context)`.
    async fn threshold_compaction_needed(
        self: &Arc<Self>,
        context: &ShouldStopAfterTurnContext,
    ) -> bool {
        let settings = self.compaction_settings();
        if !settings.enabled {
            return false;
        }

        let compaction_timestamp = self.active_compaction_timestamp();
        if let Some(compaction_timestamp) = compaction_timestamp {
            if (context.message.timestamp as f64) <= compaction_timestamp {
                return false;
            }
        }

        let messages = self.agent.state().messages;
        let context_tokens =
            self.get_threshold_context_tokens(&settings, &context.message, compaction_timestamp);
        let model = self.model();
        if context_tokens.is_none() || model.is_none() {
            return false;
        }
        let context_tokens = context_tokens.unwrap_or(0.0);
        let model = model.unwrap_or_default();
        if !should_compact_for_model(context_tokens, &model, &settings) {
            return false;
        }
        let _ = messages;

        self.await_agent_event_queue().await;
        let rlm_outcome = self.handle_rlm_child_turn_outcome(&context.message, true, Some("threshold"));
        if rlm_outcome
            .as_ref()
            .map(|outcome| outcome.continuation)
            .unwrap_or(false)
        {
            self.continue_after_threshold_compaction.store(true, Ordering::SeqCst);
        } else if !rlm_outcome
            .as_ref()
            .map(|outcome| outcome.terminal)
            .unwrap_or(false)
            && self.queue_goal_continuation_for_threshold_compaction(&context.message)
        {
            self.continue_after_threshold_compaction.store(true, Ordering::SeqCst);
        } else if !rlm_outcome
            .as_ref()
            .map(|outcome| outcome.terminal)
            .unwrap_or(false)
            && self.queue_autonomous_continuation_for_threshold_compaction(&context.message)
        {
            self.continue_after_threshold_compaction.store(true, Ordering::SeqCst);
        }
        true
    }

    /// `_snapshotAutonomousRuntimeState`.
    fn snapshot_autonomous_runtime_state(&self) -> AutonomousRuntimeSnapshot {
        let state = self.autonomous_state.lock().unwrap();
        AutonomousRuntimeSnapshot {
            continuations_used: state.continuations_used,
            gate_attempts: state.gate_attempts,
            last_gate_failure: state.last_gate_failure.clone(),
            last_gate_failure_snapshot: state.last_gate_failure_snapshot.clone(),
        }
    }

    /// `_restoreAutonomousRuntimeSnapshot`.
    fn restore_autonomous_runtime_snapshot(&self, snapshot: AutonomousRuntimeSnapshot) {
        let mut state = self.autonomous_state.lock().unwrap();
        state.continuations_used = snapshot.continuations_used;
        state.gate_attempts = snapshot.gate_attempts;
        state.last_gate_failure = snapshot.last_gate_failure;
        state.last_gate_failure_snapshot = snapshot.last_gate_failure_snapshot;
    }

    /// `_queueAutonomousContinuationForThresholdCompaction`.
    fn queue_autonomous_continuation_for_threshold_compaction(&self, message: &AssistantMessage) {
        let key = assistant_message_key(message);
        let snapshot = self.snapshot_autonomous_runtime_state();
        let queued = AgentMessage::from(message.clone());
        self.queued_autonomous_threshold_continuations
            .lock()
            .unwrap()
            .insert(key, queued.clone());
        self.queued_autonomous_continuation_snapshots
            .lock()
            .unwrap()
            .insert(assistant_message_key(message), snapshot);
        self.pending_threshold_compaction_autonomous_messages
            .lock()
            .unwrap()
            .push(queued);
    }

    /// `_queueGoalContinuationForThresholdCompaction`.
    fn queue_goal_continuation_for_threshold_compaction(&self, message: &AssistantMessage) -> bool {
        if self.goal_state.lock().unwrap().status != GoalStatus::Active {
            return false;
        }
        if self.goal_continuation_awaits_rlm_work.load(Ordering::SeqCst) {
            return false;
        }
        let goal = self.goal_state();
        let continuation = match create_goal_context_message(
            &goal,
            crate::core::goals::GoalContextKind::Continuation,
            None,
        ) {
            Ok(message) => message,
            Err(_) => return false,
        };
        let queued = AgentMessage::Custom(CustomAgentMessage::Custom {
            custom_type: continuation.custom_type.clone(),
            content: continuation.content.clone(),
            display: continuation.display,
            details: continuation.details.clone(),
            timestamp: continuation.timestamp,
        });
        let _ = message;
        *self.queued_goal_threshold_continuation.lock().unwrap() = Some(queued);
        true
    }

    /// `_clearQueuedGoalContinuationAfterCancelledThresholdCompaction`.
    fn clear_queued_goal_continuation_after_cancelled_threshold_compaction(&self) {
        *self.queued_goal_threshold_continuation.lock().unwrap() = None;
    }

    /// `_clearQueuedAutonomousContinuations`.
    fn clear_queued_autonomous_continuations(&self) {
        self.queued_autonomous_threshold_continuations.lock().unwrap().clear();
        self.queued_autonomous_continuation_snapshots.lock().unwrap().clear();
        self.pending_threshold_compaction_autonomous_messages
            .lock()
            .unwrap()
            .clear();
    }

    /// `_clearQueuedAutonomousContinuationsAfterSkippedThresholdCompaction`.
    fn clear_queued_autonomous_continuations_after_skipped_threshold_compaction(&self) {
        self.clear_queued_autonomous_continuations();
    }


    /// `handleGoalHostRequest`.
    pub fn handle_goal_host_request(
        &self,
        request_type: &str,
        payload: Option<&Value>,
    ) -> Result<GoalHostResponse, String> {
        if !self.include_goals {
            return Err("goals are disabled in this session".to_string());
        }
        let payload = payload.cloned().unwrap_or(Value::Null);
        match request_type {
            "goal.get" => Ok(goal_host_response(&self.goal_state(), false)),
            "goal.create" => {
                let objective = match payload.get("objective") {
                    Some(Value::String(objective)) => objective.clone(),
                    _ => return Err("goal.create objective must be a string".to_string()),
                };
                let token_budget = match payload.get("token_budget") {
                    None | Some(Value::Null) => None,
                    Some(Value::Number(number)) => number.as_f64(),
                    Some(_) => {
                        return Err(
                            "goal.create token_budget must be an integer when provided".to_string()
                        )
                    }
                };
                Ok(goal_host_response(
                    &self.create_goal_from_host(&objective, token_budget)?,
                    false,
                ))
            }
            "goal.complete" => Ok(goal_host_response(&self.complete_goal_from_host()?, true)),
            _ => Err(format!("unknown goal request type \"{request_type}\"")),
        }
    }

    /**
     * Handle a compact.* request from the kernel host bridge. Compaction would
     * abort the run executing the requesting cell, so compact.run only schedules
     * it; _checkCompaction consumes the request at the turn boundary.
     */
    pub fn handle_compact_host_request(
        &self,
        request_type: &str,
        payload: Option<&Value>,
    ) -> Result<Value, String> {
        if !self.include_compact_skill {
            return Err("the compact skill is disabled in this session".to_string());
        }
        let payload = payload.cloned().unwrap_or(Value::Null);
        match request_type {
            "compact.status" => {
                let usage = self.get_context_usage();
                Ok(serde_json::json!({
                    "tokens": usage.as_ref().and_then(|usage| usage.tokens),
                    "context_window": usage.as_ref().map(|usage| usage.context_window),
                    "percent": usage.as_ref().and_then(|usage| usage.percent),
                    "scheduled": self.pending_requested_compaction.lock().unwrap().is_some(),
                }))
            }
            "compact.run" => {
                let instructions = match payload.get("instructions") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(instructions)) => Some(instructions.clone()),
                    Some(_) => {
                        return Err(
                            "compact.run instructions must be a string when provided".to_string()
                        )
                    }
                };
                if !self.is_streaming() {
                    return Ok(serde_json::json!({
                        "scheduled": false,
                        "reason": "no active turn; compaction can only be requested while a turn is running",
                    }));
                }
                let preparation = {
                    let entries = self.session_manager.lock().unwrap().get_branch(None);
                    let settings = self.compaction_settings();
                    let session_manager = self.session_manager.clone();
                    let path_entries: Vec<CompactionSessionEntry> = entries
                        .iter()
                        .filter_map(compaction_session_entry_from)
                        .collect();
                    prepare_compaction(&path_entries, &settings, &move |path_entries| {
                        let _ = (&session_manager, path_entries);
                        Vec::new()
                    })
                };
                if preparation.is_none() {
                    let branch = self.session_manager.lock().unwrap().get_branch(None);
                    let last_entry = branch.last();
                    let reason = if last_entry
                        .and_then(|entry| entry.get("type"))
                        .and_then(Value::as_str)
                        == Some("compaction")
                    {
                        "already compacted"
                    } else {
                        "session is too short to compact"
                    };
                    return Ok(serde_json::json!({ "scheduled": false, "reason": reason }));
                }
                *self.pending_requested_compaction.lock().unwrap() =
                    Some(PendingRequestedCompaction { custom_instructions: instructions });
                Ok(serde_json::json!({
                    "scheduled": true,
                    "note": "Compaction runs when the current turn ends; you resume automatically afterwards. Continue working normally.",
                }))
            }
            _ => Err(format!("unknown compact request type \"{request_type}\"")),
        }
    }

    /**
     * Handle a refine.* request from the kernel host bridge. Like compact,
     * refinement waits for the current turn to become idle before applying
     * changes, so refine.run only schedules it; _consumePendingRequestedRefine
     * fires it at the turn boundary. This prevents a deadlock that would occur
     * if refine() awaited agent idle from within the active tool call.
     */
    pub fn handle_refine_host_request(
        self: &Arc<Self>,
        request_type: &str,
        payload: Option<&Value>,
    ) -> Result<Value, String> {
        let payload = payload.cloned().unwrap_or(Value::Null);
        match request_type {
            "refine.status" => Ok(serde_json::json!({
                "pending": self.pending_requested_refine.lock().unwrap().is_some(),
                "in_flight": self.refine_in_flight.lock().unwrap().is_some()
                    || self.refine_plan_in_flight.lock().unwrap().is_some()
                    || self.serialized_plan_in_flight.lock().unwrap().is_some(),
            })),
            "refine.run" => {
                let instructions = match payload.get("instructions") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(instructions)) => Some(instructions.clone()),
                    Some(_) => {
                        return Err(
                            "refine.run instructions must be a string when provided".to_string()
                        )
                    }
                };
                let global_flag = match payload.get("global") {
                    None | Some(Value::Null) => None,
                    Some(Value::Bool(global)) => Some(*global),
                    Some(_) => {
                        return Err("refine.run global must be a boolean when provided".to_string())
                    }
                };
                if !self.is_streaming() {
                    return Ok(serde_json::json!({
                        "scheduled": false,
                        "reason": "no active turn; refine can only be requested while a turn is running",
                    }));
                }
                let previous = {
                    let pending = self.pending_requested_refine.lock().unwrap().clone();
                    match pending {
                        Some(pending) => Some(PendingRequestedRefine {
                            instructions: pending.instructions,
                            global: pending.global,
                        }),
                        None => self.serialized_explicit_refine_options.lock().unwrap().clone(),
                    }
                };
                *self.pending_requested_refine.lock().unwrap() = Some(PendingRequestedRefine {
                    instructions: instructions.or_else(|| {
                        previous.as_ref().and_then(|previous| previous.instructions.clone())
                    }),
                    global: global_flag.or_else(|| previous.as_ref().and_then(|previous| previous.global)),
                });
                // In serialized mode, kick off background planning immediately
                // (the primary response ended at message_end, tools are active).
                // This lets planning overlap tool execution rather than waiting
                // for the shouldStopAfterTurn boundary.
                if self.serialized_refine {
                    if self.serialized_plan_in_flight.lock().unwrap().is_some() {
                        self.auto_refine_branch_version.fetch_add(1, Ordering::SeqCst);
                        self.maybe_start_serialized_background_plan();
                    } else {
                        self.maybe_start_serialized_background_plan();
                    }
                }
                Ok(serde_json::json!({
                    "scheduled": true,
                    "note": "Refinement is queued, not saved yet. Applied edits are appended to your context as a refinement notice after persistence. Continue working normally; do not claim the refinement is saved until its outcome arrives.",
                }))
            }
            _ => Err(format!("unknown refine request type \"{request_type}\"")),
        }
    }

    /**
     * Handle an rlm_heartbeat.* request from the bundled rlm-heartbeat skill.
     * These heartbeats are internal to this active session and never read or
     * mutate the user-level /heartbeat.
     */
    pub fn handle_rlm_heartbeat_host_request(
        &self,
        request_type: &str,
        payload: Option<&Value>,
    ) -> Result<Value, String> {
        let controller = self.rlm_heartbeat_controller.lock().unwrap().clone();
        let controller = match controller {
            Some(controller) => controller,
            None => return Err("RLM heartbeat skill is not available in this session".to_string()),
        };
        let payload = payload.cloned().unwrap_or(Value::Null);
        match request_type {
            "rlm_heartbeat.list" => {
                let include_inactive = payload.get("include_inactive").and_then(Value::as_bool)
                    == Some(true)
                    || payload.get("includeInactive").and_then(Value::as_bool) == Some(true);
                let jobs: Vec<Value> = controller
                    .list()
                    .iter()
                    .filter(|job| include_inactive || job.status != "paused")
                    .map(rlm_heartbeat_host_response)
                    .collect();
                Ok(serde_json::json!({ "heartbeats": jobs }))
            }
            "rlm_heartbeat.create" => {
                if !matches!(payload.get("instruction"), Some(Value::String(_))) {
                    return Err("rlm_heartbeat.create instruction must be a string".to_string());
                }
                if let Some(interval) = payload.get("interval") {
                    if !interval.is_null() && !interval.is_string() {
                        return Err(
                            "rlm_heartbeat.create interval must be a string when provided"
                                .to_string(),
                        );
                    }
                }
                if let Some(label) = payload.get("label") {
                    if !label.is_null() && !label.is_string() {
                        return Err(
                            "rlm_heartbeat.create label must be a string when provided".to_string()
                        );
                    }
                }
                let delivery_mode = normalize_heartbeat_delivery_mode(
                    payload
                        .get("delivery_mode")
                        .or_else(|| payload.get("deliveryMode")),
                );
                let mut request = payload.clone();
                if let Value::Object(object) = &mut request {
                    object.insert(
                        "deliveryMode".to_string(),
                        Value::String(delivery_mode),
                    );
                }
                match controller.create(request) {
                    Ok(heartbeat) => Ok(serde_json::json!({
                        "heartbeat": heartbeat,
                    })),
                    Err(error) => Err(error),
                }
            }
            "rlm_heartbeat.update" => {
                if !matches!(payload.get("id"), Some(Value::String(_))) {
                    return Err("rlm_heartbeat.update id must be a string".to_string());
                }
                for (field, message) in [
                    ("instruction", "rlm_heartbeat.update instruction must be a string when provided"),
                    ("interval", "rlm_heartbeat.update interval must be a string when provided"),
                    ("label", "rlm_heartbeat.update label must be a string when provided"),
                ] {
                    if let Some(value) = payload.get(field) {
                        if !value.is_null() && !value.is_string() {
                            return Err(message.to_string());
                        }
                    }
                }
                if let Some(status) = payload.get("status") {
                    if !is_rlm_heartbeat_status_update(status) {
                        return Err(
                            "rlm_heartbeat.update status must be \"pause\" or \"resume\" when provided"
                                .to_string(),
                        );
                    }
                }
                let raw_delivery_mode = payload
                    .get("delivery_mode")
                    .or_else(|| payload.get("deliveryMode"));
                let delivery_mode = normalize_heartbeat_delivery_mode(raw_delivery_mode);
                if payload.get("instruction").is_none()
                    && payload.get("interval").is_none()
                    && payload.get("label").is_none()
                    && payload.get("status").is_none()
                    && raw_delivery_mode.is_none()
                {
                    return Err(
                        "rlm_heartbeat.update requires at least one field to update".to_string()
                    );
                }
                let job_id = payload
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let mut request = payload.clone();
                if let Value::Object(object) = &mut request {
                    object.insert("deliveryMode".to_string(), Value::String(delivery_mode));
                }
                match controller.update(&job_id, request) {
                    Ok(heartbeat) => Ok(serde_json::json!({ "heartbeat": heartbeat })),
                    Err(error) => Err(error),
                }
            }
            "rlm_heartbeat.delete" => {
                let job_id = match payload.get("id") {
                    Some(Value::String(id)) => id.clone(),
                    _ => return Err("rlm_heartbeat.delete id must be a string".to_string()),
                };
                match controller.delete(&job_id) {
                    Ok(heartbeat) => Ok(serde_json::json!({ "heartbeat": heartbeat })),
                    Err(error) => Err(error),
                }
            }
            _ => Err(format!("unknown RLM heartbeat request type \"{request_type}\"")),
        }
    }

    /// `handleAgentMessageHostRequest`.
    pub fn handle_agent_message_host_request(
        &self,
        request_type: &str,
        payload: Option<&Value>,
    ) -> Result<BoxFuture<Result<Value, String>>, String> {
        let controller = match &self.agent_message_controller {
            Some(controller) => controller.clone(),
            None => return Err("agent messaging is not available in this session".to_string()),
        };
        let payload = payload.cloned().unwrap_or(Value::Null);
        match request_type {
            "agent_message.list_agents" => Ok(Box::pin(async move {
                controller
                    .roster()
                    .await
                    .and_then(|roster| {
                        serde_json::to_value(roster).map_err(|error| error.to_string())
                    })
            })),
            "agent_message.send" => {
                let target = match payload.get("target") {
                    Some(Value::String(target)) => target.clone(),
                    _ => return Err("agent_message.send target must be a string".to_string()),
                };
                let message = match payload.get("message") {
                    Some(Value::String(message)) => message.clone(),
                    _ => return Err("agent_message.send message must be a string".to_string()),
                };
                let target = assert_direct_agent_message_target(&target)?;
                let message = normalize_agent_session_message(&message, usize::MAX)?;
                Ok(Box::pin(async move {
                    controller
                        .deliver(serde_json::json!({ "target": target, "message": message }))
                        .await
                }))
            }
            _ => Err(format!("unknown agent message request type \"{request_type}\"")),
        }
    }

    /// `handleAgentObserveHostRequest`.
    pub fn handle_agent_observe_host_request(
        &self,
        request_type: &str,
        payload: Option<&Value>,
    ) -> Result<BoxFuture<Result<Value, String>>, String> {
        let controller = match &self.agent_observe_controller {
            Some(controller) => controller.clone(),
            None => return Err("agent observation is not available in this session".to_string()),
        };
        let payload = payload.cloned().unwrap_or(Value::Null);
        match request_type {
            "agent_observe.list" => Ok(Box::pin(async move {
                controller
                    .list_agents(Value::Null)
                    .await
                    .and_then(|result| serde_json::to_value(result).map_err(|error| error.to_string()))
            })),
            "agent_observe.get" => {
                let target = match payload.get("target") {
                    Some(Value::String(target)) => target.clone(),
                    _ => return Err("agent_observe.get target must be a string".to_string()),
                };
                Ok(Box::pin(async move {
                    controller
                        .snapshot(serde_json::json!({ "target": target }))
                        .await
                        .and_then(|result| serde_json::to_value(result).map_err(|error| error.to_string()))
                }))
            }
            "agent_observe.recent" => {
                let target = match payload.get("target") {
                    Some(Value::String(target)) => target.clone(),
                    _ => return Err("agent_observe.recent target must be a string".to_string()),
                };
                let limit = normalize_observe_limit(
                    payload.get("limit").and_then(Value::as_i64),
                    20,
                )?;
                let max_chars = normalize_observe_max_chars(
                    payload
                        .get("max_chars")
                        .or_else(|| payload.get("maxChars"))
                        .and_then(Value::as_i64),
                    4000,
                )?;
                Ok(Box::pin(async move {
                    controller
                        .recent_messages(serde_json::json!({
                            "target": target,
                            "limit": limit,
                            "maxChars": max_chars,
                        }))
                        .await
                        .and_then(|result| serde_json::to_value(result).map_err(|error| error.to_string()))
                }))
            }
            _ => Err(format!("unknown agent observe request type \"{request_type}\"")),
        }
    }

    /// `_createGoalFromHost`.
    fn create_goal_from_host(&self, objective: &str, token_budget: Option<f64>) -> Result<GoalState, String> {
        match self.goal_state.lock().unwrap().status {
            GoalStatus::Active => Err(
                "cannot create a new goal because this thread already has an active goal; run `await goal.complete()` when it is achieved, or ask the user to clear it with /goal clear"
                    .to_string(),
            ),
            GoalStatus::Paused => Err(
                "cannot create a new goal because a paused goal exists; ask the user to resume it with /goal resume or clear it with /goal clear"
                    .to_string(),
            ),
            GoalStatus::BudgetLimited => Err(
                "cannot create a new goal because a budget-limited goal exists; ask the user to resume it with /goal resume or clear it with /goal clear"
                    .to_string(),
            ),
            // idle, or a terminal record (complete / error): nothing pending, start fresh.
            _ => self.start_goal(objective, token_budget),
        }
    }

    /// `_completeGoalFromHost`.
    fn complete_goal_from_host(&self) -> Result<GoalState, String> {
        {
            let goal = self.goal_state.lock().unwrap();
            if goal.objective.is_none() || goal.status == GoalStatus::Idle {
                return Err("cannot complete goal because this thread has no goal".to_string());
            }
        }
        let goal = self.goal_with_accounted_wall_clock();
        // A turn can cross the budget and complete the goal at once: accounting
        // runs at message_end, before the completing ipython cell executes, so a
        // budget-limit context may already be steered. It is stale now - drop it.
        self.clear_queued_goal_contexts();
        let next = GoalState {
            active: false,
            status: GoalStatus::Complete,
            last_reason: Some("Goal achieved".to_string()),
            last_error: None,
            ..goal
        };
        self.set_goal_state(&next, None);
        Ok(self.goal_state())
    }

    /// `_getGoalContinuationMessages`.
    async fn get_goal_continuation_messages(
        self: &Arc<Self>,
        context: AgentMessage,
        signal: Option<&CancellationToken>,
    ) -> Vec<AgentMessage> {
        let terminal_message = match &context {
            AgentMessage::Message(Message::Assistant(assistant)) => Some(assistant.clone()),
            _ => None,
        };
        if let Some(message) = terminal_message.as_ref() {
            if self.stop_goal_continuation_for_terminal_message(message) {
                return Vec::new();
            }
        }
        let aborted = signal.map(|signal| signal.is_cancelled()).unwrap_or(false);
        {
            let goal = self.goal_state.lock().unwrap();
            if aborted || goal.status != GoalStatus::Active || goal.objective.is_none() {
                return Vec::new();
            }
        }
        // Delegating and ending the turn is correct behavior; hold the continuation
        // until descendants settle instead of re-prompting a waiting parent.
        if self.has_unsettled_rlm_quiescence_work() {
            self.goal_continuation_awaits_rlm_work.store(true, Ordering::SeqCst);
            return Vec::new();
        }
        self.goal_continuation_awaits_rlm_work.store(false, Ordering::SeqCst);
        self.ensure_goal_runtime_active(None);
        let goal = {
            let mut goal = self.goal_state.lock().unwrap().clone();
            goal.continuations_used += 1.0;
            goal.last_reason = None;
            goal.last_error = None;
            goal
        };
        self.set_goal_state(&goal, None);
        match create_goal_context_message(
            &self.goal_state(),
            crate::core::goals::GoalContextKind::Continuation,
            None,
        ) {
            Ok(message) => vec![AgentMessage::Custom(CustomAgentMessage::Custom {
                custom_type: message.custom_type.clone(),
                content: message.content.clone(),
                display: message.display,
                details: message.details.clone(),
                timestamp: message.timestamp,
            })],
            Err(error) => {
                // The continuation hook must not reject; listener failures should not crash the agent loop.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.finish_goal_with_error(&error);
                }));
                Vec::new()
            }
        }
    }

    /// `_restoreRlmContinuationState`.
    fn restore_rlm_continuation_state(&self) {
        if self.rlm_depth == 0 {
            return;
        }
        let branch = self.session_manager.lock().unwrap().get_branch(None);
        for entry in branch.iter().rev() {
            if entry.get("type").and_then(Value::as_str) != Some("custom") {
                continue;
            }
            let custom_type = entry.get("customType").and_then(Value::as_str).unwrap_or_default();
            if custom_type != RLM_CONTINUATION_STATE_CUSTOM_TYPE
                && custom_type != LEGACY_RLM_CONTINUATION_STATE_CUSTOM_TYPE
            {
                continue;
            }
            let data = entry.get("data").cloned().unwrap_or(Value::Null);
            let parsed = if custom_type == LEGACY_RLM_CONTINUATION_STATE_CUSTOM_TYPE {
                parse_legacy_rlm_continuation_state(&data)
            } else {
                parse_rlm_continuation_state(&data)
            };
            if let Some(state) = parsed {
                *self.rlm_continuation.lock().unwrap() = state;
                return;
            }
        }
    }

    /// `_persistRlmContinuationState`.
    fn persist_rlm_continuation_state(&self) {
        let state = self.rlm_continuation.lock().unwrap().clone();
        let value = serde_json::to_value(&state).unwrap_or(Value::Null);
        let _ = self.session_manager.lock().unwrap().append_custom_entry(
            RLM_CONTINUATION_STATE_CUSTOM_TYPE,
            Some(value),
        );
    }

    /// `_beginRlmParentTask`.
    fn begin_rlm_parent_task(&self, message: &AgentMessage) {
        if self.rlm_depth == 0 || !is_agent_session_message(message) {
            return;
        }
        let (details, timestamp) = match message {
            AgentMessage::Custom(CustomAgentMessage::Custom {
                details, timestamp, ..
            }) => (details.clone().unwrap_or(Value::Null), *timestamp),
            _ => return,
        };
        if details.get("fromRelationship").and_then(Value::as_str) != Some("parent") {
            return;
        }
        let message_id = details
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        {
            let state = self.rlm_continuation.lock().unwrap();
            if state.tasks.iter().any(|task| task.task_id == message_id) {
                return;
            }
        }
        // Follow-up admission is not a task boundary: only durable delivery can
        // reset completion state. Keep every delivered task awaiting a result.
        {
            let mut state = self.rlm_continuation.lock().unwrap();
            state.pending_results.retain(|task| task.status != "replied");
            state.pending_results.push(RlmPendingResult {
                task_id: message_id,
                child_id: String::new(),
                session_name: String::new(),
                status: "awaiting".to_string(),
                answer: None,
                error: None,
            });
            let _ = timestamp;
            state.pending_continuation = None;
        }
        *self.replied_to_parent_since_task.lock().unwrap() = Some(false);
        self.persist_rlm_continuation_state();
    }

    /// `_rlmContinuationMatches`.
    fn rlm_continuation_matches(
        &self,
        message: &AgentMessage,
        pending: Option<&RlmPendingContinuation>,
    ) -> bool {
        let pending = match pending {
            Some(pending) => pending,
            None => return false,
        };
        let (timestamp, text) = match message {
            AgentMessage::Message(Message::User(user)) => {
                let text = match &user.content {
                    UserContent::Text(text) => text.clone(),
                    UserContent::Blocks(blocks) => normalize_message_content(blocks).0,
                };
                (user.timestamp, text)
            }
            _ => return false,
        };
        timestamp == pending.continuations_used as i64 && text == pending.prompt
    }

    /// `_hasQueuedRlmContinuation`.
    fn has_queued_rlm_continuation(&self) -> bool {
        let pending = self.rlm_continuation.lock().unwrap().pending_continuation.clone();
        self.action_store
            .lock()
            .unwrap()
            .unfinished_actions(None)
            .iter()
            .any(|action| match &action.payload {
                QueuedActionPayload::Turn(_) => match primary_delivery_record(action) {
                    Ok(record) => match record.message {
                        DeliveryMessage::User(user) => {
                            self.rlm_continuation_matches(&AgentMessage::from(user), pending.as_ref())
                        }
                        DeliveryMessage::Custom(_) => false,
                    },
                    Err(_) => false,
                },
                QueuedActionPayload::SessionCommand(_) => false,
            })
    }

    /// `_pendingRlmContinuationMessage`.
    fn pending_rlm_continuation_message(&self, pending: &RlmPendingContinuation) -> UserMessage {
        UserMessage {
            role: "user".to_string(),
            content: UserContent::Text(pending.prompt.clone()),
            provider_context: None,
            timestamp: pending.continuations_used as i64,
        }
    }

    /// `_queuePendingRlmContinuation`.
    fn queue_pending_rlm_continuation(self: &Arc<Self>) -> bool {
        let pending = self.rlm_continuation.lock().unwrap().pending_continuation.clone();
        let pending = match pending {
            Some(pending) => pending,
            None => return false,
        };
        if self.disposed.load(Ordering::SeqCst)
            || self.disposing.load(Ordering::SeqCst)
            || self.session_input_pump_suspended.load(Ordering::SeqCst)
            || !self.session_input_admission_pauses.lock().unwrap().is_empty()
        {
            return false;
        }
        if self.has_queued_rlm_continuation() {
            return true;
        }
        let message = self.pending_rlm_continuation_message(&pending);
        // Recovery belongs before a later parent follow-up, not after it has reset
        // this task's counters. The queue still owns dispatch and compaction fences.
        let action = self.create_prepared_turn_action(
            SESSION_INPUT_SCHEDULE_FOLLOW_UP,
            &pending.prompt,
            None,
            Some(PreparedTurnActionOptions {
                message: Some(AgentMessage::from(message)),
                resume_if_idle: Some(true),
                queue_key: Some(format!("rlm-recovery:{}", pending.task_id)),
                ..Default::default()
            }),
        );
        self.admit_session_input(action, true);
        if let Some(pending) = self.rlm_continuation.lock().unwrap().pending_continuation.as_mut() {
            pending.continuations_used += 1.0;
        }
        self.persist_rlm_continuation_state();
        true
    }

    /// `_consumeStartedRlmContinuation`.
    fn consume_started_rlm_continuation(&self, message: &AgentMessage) {
        let pending = self.rlm_continuation.lock().unwrap().pending_continuation.clone();
        if !self.rlm_continuation_matches(message, pending.as_ref()) {
            return;
        }
        // Retain the record until a following assistant result is authoritative.
        // A process exiting after delivery but before that result can then resume.
        self.persist_rlm_continuation_state();
    }

    /// `_handleRlmChildTurnOutcome`.
    fn handle_rlm_child_turn_outcome(
        &self,
        message: &AssistantMessage,
        queue: bool,
        compaction_reason: Option<&str>,
    ) -> Option<RlmChildTurnOutcome> {
        if self.rlm_depth == 0 || self.rlm_continuation.lock().unwrap().pending_results.is_empty() {
            return None;
        }
        let classification = classify_rlm_child_terminal(&message.stop_reason);
        let source_key = format!(
            "{}:{}:{}:{}",
            message.timestamp,
            message.stop_reason,
            read_assistant_text(message).chars().count(),
            read_assistant_text(message)
                .chars()
                .rev()
                .take(96)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        );
        if self.rlm_continuation.lock().unwrap().pending_continuation.as_ref()
            .map(|pending| pending.task_id == source_key)
            .unwrap_or(false)
        {
            if queue {
                self.queue_pending_rlm_continuation();
            }
            return Some(RlmChildTurnOutcome {
                terminal: false,
                continuation: self
                    .rlm_continuation
                    .lock()
                    .unwrap()
                    .pending_continuation
                    .clone()
                    .map(|pending| self.pending_rlm_continuation_message(&pending)),
            });
        }
        let can_continue = classification.is_some();
        let _ = compaction_reason;
        if !can_continue {
            self.persist_rlm_continuation_state();
            return None;
        }
        self.persist_rlm_continuation_state();
        if queue {
            self.queue_pending_rlm_continuation();
        }
        Some(RlmChildTurnOutcome {
            terminal: false,
            continuation: self
                .rlm_continuation
                .lock()
                .unwrap()
                .pending_continuation
                .clone()
                .map(|pending| self.pending_rlm_continuation_message(&pending)),
        })
    }

    /// `_recordRlmTerminalResult`.
    fn record_rlm_terminal_result(&self, result: RlmPendingResult) {
        let mut state = self.rlm_continuation.lock().unwrap();
        let mut pending = result;
        pending.status = state
            .pending_continuation
            .as_ref()
            .map(|pending| pending.task_id.clone())
            .unwrap_or_else(|| pending.status.clone());
        state.pending_results = vec![pending.clone()];
        let _ = state;
    }

    /// `_markExplicitRlmParentReply`.
    fn mark_explicit_rlm_parent_reply(&self, task_ids: &[String]) {
        {
            let mut state = self.rlm_continuation.lock().unwrap();
            for result in state.pending_results.iter_mut() {
                if task_ids.contains(&result.task_id) {
                    result.status = "replied".to_string();
                }
            }
        }
        *self.replied_to_parent_since_task.lock().unwrap() = Some(
            self.rlm_continuation
                .lock()
                .unwrap()
                .pending_results
                .iter()
                .all(|result| result.status == "replied"),
        );
        self.parent_reply_count.fetch_add(1, Ordering::SeqCst);
        self.persist_rlm_continuation_state();
    }

    /// `_deliverPendingRlmResults`.
    async fn deliver_pending_rlm_results(self: &Arc<Self>) {
        self.deliver_pending_rlm_results_once().await;
    }

    /// `_deliverPendingRlmResultsOnce`.
    async fn deliver_pending_rlm_results_once(self: &Arc<Self>) {
        let state = self.rlm_continuation.lock().unwrap().clone();
        if !state
            .pending_results
            .iter()
            .any(|task| task.status != "replied")
            || self.disposed.load(Ordering::SeqCst)
            || self.disposing.load(Ordering::SeqCst)
        {
            return;
        }
        let controller = match &self.agent_message_controller {
            Some(controller) => controller.clone(),
            None => return,
        };
        let roster = match controller.roster().await {
            Ok(roster) => roster,
            Err(_) => return,
        };
        let parent = match roster.agents.iter().find(|entry| {
            entry
                .relationship
                .as_deref()
                .map(|relationship| relationship == "parent")
                .unwrap_or(false)
        }) {
            Some(parent) => parent.clone(),
            None => return,
        };
        for task in state.pending_results.iter() {
            if task.status == "replied" || self.disposed.load(Ordering::SeqCst) {
                continue;
            }
            let mut text = vec![
                "RLM child automatic result".to_string(),
                format!(
                    "child_id: {}",
                    self.rlm_parent_node_id
                        .clone()
                        .unwrap_or_else(|| self.session_id())
                ),
                format!("session_name: {}", self.session_name().unwrap_or_else(|| "unnamed".to_string())),
                format!("task_id: {}", task.task_id),
                format!("terminal_status: {}", task.status),
                "partial: no".to_string(),
                "stop_reason: unknown".to_string(),
            ];
            text.push("visible_result:".to_string());
            text.push(
                task.answer
                    .clone()
                    .unwrap_or_else(|| "(no visible assistant text was produced)".to_string()),
            );
            let _ = controller
                .deliver(serde_json::json!({
                    "target": parent.session_id,
                    "message": text.join("\n"),
                }))
                .await;
            {
                let mut state = self.rlm_continuation.lock().unwrap();
                for candidate in state.pending_results.iter_mut() {
                    if candidate.task_id == task.task_id {
                        candidate.status = "replied".to_string();
                    }
                }
            }
            self.parent_reply_count.fetch_add(1, Ordering::SeqCst);
            *self.replied_to_parent_since_task.lock().unwrap() = Some(
                self.rlm_continuation
                    .lock()
                    .unwrap()
                    .pending_results
                    .iter()
                    .all(|candidate| candidate.status == "replied"),
            );
            self.persist_rlm_continuation_state();
        }
    }

    /// `_scheduleRlmReloadBackstop`.
    fn schedule_rlm_reload_backstop(self: &Arc<Self>) {
        if self.rlm_depth == 0
            || self.disposed.load(Ordering::SeqCst)
            || self.disposing.load(Ordering::SeqCst)
        {
            return;
        }
        let state = self.rlm_continuation.lock().unwrap().clone();
        if state.pending_continuation.is_none()
            && !state.pending_results.iter().any(|task| task.status != "replied")
        {
            return;
        }
        let session = self.clone();
        tokio::spawn(async move {
            session.run_rlm_reload_backstop().await;
        });
    }

    /// `_runRlmReloadBackstop`.
    async fn run_rlm_reload_backstop(self: &Arc<Self>) {
        let _ = self.agent.wait_for_idle().await;
        self.await_agent_event_queue().await;
        if self.disposed.load(Ordering::SeqCst)
            || self.disposing.load(Ordering::SeqCst)
            || self.session_input_pump_suspended.load(Ordering::SeqCst)
        {
            return;
        }
        let pending = self.rlm_continuation.lock().unwrap().pending_continuation.clone();
        if pending.is_some() {
            self.queue_pending_rlm_continuation();
        }
        self.deliver_pending_rlm_results().await;
    }

    /// `get rlmDiagnostics()`.
    pub fn rlm_diagnostics(&self) -> Option<Value> {
        if self.rlm_depth == 0 {
            return None;
        }
        let state = self.rlm_continuation.lock().unwrap().clone();
        let continuation_queued = self.has_queued_rlm_continuation();
        let last_stop_reason = state
            .pending_continuation
            .as_ref()
            .map(|pending| pending.prompt.clone());
        Some(serde_json::json!({
            "lastStopReason": last_stop_reason,
            "terminalStatus": state.pending_results.first().map(|result| result.status.clone()),
            "continuationQueued": continuation_queued,
            "compactionReason": Value::Null,
            "currentTaskId": state.pending_results.last().map(|result| result.task_id.clone()),
            "diagnosticState": if !self.is_streaming() && !self.is_compacting() && !continuation_queued {
                Value::String("compacted_idle_incomplete".to_string())
            } else {
                Value::Null
            },
        }))
    }

    /// `_getContinuationMessages`.
    async fn get_continuation_messages(
        self: &Arc<Self>,
        context: AgentContext,
        signal: Option<CancellationToken>,
    ) -> Result<Vec<AgentMessage>, String> {
        let aborted = signal.as_ref().map(|signal| signal.is_cancelled()).unwrap_or(false);
        if aborted {
            return Ok(Vec::new());
        }
        // Child recovery requires durable task correlation. Root continuations do
        // not: waiting here would let a cancelled dispatch's held extension event
        // prevent unrelated root work from completing.
        if self.rlm_depth > 0 {
            self.await_agent_event_queue().await;
        }
        let message = context
            .messages
            .last()
            .cloned()
            .unwrap_or(AgentMessage::Message(Message::User(UserMessage::new(
                UserContent::Text(String::new()),
                0,
            ))));
        if let AgentMessage::Message(Message::Assistant(assistant)) = &message {
            let outcome = self.handle_rlm_child_turn_outcome(assistant, false, None);
            if let Some(outcome) = outcome {
                if outcome.terminal {
                    return Ok(Vec::new());
                }
                if let Some(continuation) = outcome.continuation {
                    if self.queued_action_count() > 0 {
                        self.queue_pending_rlm_continuation();
                        return Ok(Vec::new());
                    }
                    return Ok(vec![AgentMessage::from(continuation)]);
                }
            }
        }
        if self.queued_action_count() > 0 {
            return Ok(Vec::new());
        }
        let arrival_epoch = self.session_input_arrival_epoch.load(Ordering::SeqCst);
        let goal_snapshot = self.goal_state.lock().unwrap().clone();
        let goal_accounting_started_at = *self.goal_accounting_started_at.lock().unwrap();
        let goal_messages = self
            .get_goal_continuation_messages(message.clone(), signal.as_ref())
            .await;
        if !goal_messages.is_empty() || aborted {
            if !goal_messages.is_empty()
                && self.session_input_arrival_epoch.load(Ordering::SeqCst) != arrival_epoch
            {
                self.set_goal_state(&goal_snapshot, Some(false));
                *self.goal_accounting_started_at.lock().unwrap() = goal_accounting_started_at;
                return Ok(Vec::new());
            }
            return Ok(goal_messages);
        }
        if self.autonomous_continuation_suppression_depth.load(Ordering::SeqCst) > 0 {
            return Ok(Vec::new());
        }
        Ok(Vec::new())
    }

    /// `_agentMessageOutcome`.
    fn agent_message_outcome(&self, agent_message_id: &str) -> AgentMessageOutcome {
        let mut outcomes = self.agent_message_outcomes.lock().unwrap();
        outcomes
            .entry(agent_message_id.to_string())
            .or_default()
            .clone()
    }

    /**
     * Register a delivery waiter before submitting the prompt. Delivery outcomes are not retained
     * for late lookup, so callers that register after admission may wait for a future use of the id.
     */
    pub fn wait_for_agent_message_prompt_delivery(&self, agent_message_id: &str) -> AgentMessageDeferred {
        let mut outcomes = self.agent_message_outcomes.lock().unwrap();
        let outcome = outcomes.entry(agent_message_id.to_string()).or_default();
        if outcome.delivery.is_none() {
            outcome.delivery = Some(create_agent_message_deferred());
        }
        outcome.delivery.clone().unwrap()
    }

    /// `_settleAgentMessage`.
    fn settle_agent_message(
        &self,
        agent_message_id: Option<&str>,
        leg: &str,
        error: Option<&str>,
    ) {
        let agent_message_id = match agent_message_id {
            Some(agent_message_id) => agent_message_id.to_string(),
            None => return,
        };
        let deferred = {
            let mut outcomes = self.agent_message_outcomes.lock().unwrap();
            let outcome = match outcomes.get_mut(&agent_message_id) {
                Some(outcome) => outcome,
                None => return,
            };
            let deferred = if leg == "delivery" {
                outcome.delivery.take()
            } else {
                outcome.completion.take()
            };
            if outcome.delivery.is_none() && outcome.completion.is_none() {
                outcomes.remove(&agent_message_id);
            }
            deferred
        };
        let deferred = match deferred {
            Some(deferred) => deferred,
            None => return,
        };
        match error {
            Some(error) => deferred.reject(error.to_string()),
            None => deferred.resolve(),
        }
    }

    /// `_rejectAgentMessage`.
    fn reject_agent_message(&self, agent_message_id: Option<&str>, error: &str) {
        if agent_message_id.is_none() {
            return;
        }
        self.settle_agent_message(agent_message_id, "delivery", Some(error));
        self.settle_agent_message(agent_message_id, "completion", Some(error));
    }

    /// `_rejectQueuedAgentMessageDeliveries`.
    fn reject_queued_agent_message_deliveries(&self, delivery_error: &str, completion_error: Option<&str>) {
        let completion_error = completion_error.unwrap_or(delivery_error);
        for action in self.action_store.lock().unwrap().unfinished_actions(None) {
            self.settle_agent_message(action.agent_message_id.as_deref(), "delivery", Some(delivery_error));
            self.settle_agent_message(action.agent_message_id.as_deref(), "completion", Some(completion_error));
        }
    }

    /// `_capturingCancelledAction`.
    fn capturing_cancelled_action(&self, message: &AgentMessage) -> Option<QueuedSessionAction> {
        let key = agent_message_key_of(message);
        self.action_store
            .lock()
            .unwrap()
            .owned_actions()
            .into_iter()
            .find(|action| {
                action.lifecycle.state() == ActionLifecycleState::Cancelled
                    && match &action.payload {
                        QueuedActionPayload::Turn(turn) => turn
                            .capture_run_messages
                            .as_ref()
                            .map(|captured| captured.contains(&key))
                            .unwrap_or(false),
                        QueuedActionPayload::SessionCommand(_) => false,
                    }
            })
    }

    /// `_hasCancelledDispatchCapture`.
    fn has_cancelled_dispatch_capture(&self) -> bool {
        self.action_store
            .lock()
            .unwrap()
            .owned_actions()
            .iter()
            .any(|action| {
                action.lifecycle.state() == ActionLifecycleState::Cancelled
                    && match &action.payload {
                        QueuedActionPayload::Turn(turn) => turn.capture_run_messages.is_some(),
                        QueuedActionPayload::SessionCommand(_) => false,
                    }
            })
    }

    /// `_handleAgentEvent`.
    fn handle_agent_event(self: &Arc<Self>, event: AgentEvent) {
        self.create_retry_promise_for_agent_end(&event);
        match &event {
            AgentEvent::MessageStart { message } | AgentEvent::MessageEnd { message } => {
                let key = agent_message_key_of(message);
                let owned = self.action_store.lock().unwrap().owned_actions();
                for action in owned {
                    let QueuedActionPayload::Turn(turn) = &action.payload else {
                        continue;
                    };
                    if turn.capture_run_messages.is_none()
                        || turn.cancelled_dispatch_ended == Some(true)
                    {
                        continue;
                    }
                    let primary = primary_delivery_record(&action).ok();
                    let matches = primary
                        .as_ref()
                        .map(|record| {
                            delivery_message_key_of(&record.message) == key || record.started
                        })
                        .unwrap_or(false);
                    if matches {
                        let mut store = self.action_store.lock().unwrap();
                        let mut next = action.clone();
                        if let QueuedActionPayload::Turn(turn) = &mut next.payload {
                            if let Some(captured) = turn.capture_run_messages.as_mut() {
                                captured.insert(key);
                            }
                        }
                        let _ = store.update_action(&next);
                    }
                }
            }
            AgentEvent::AgentEnd { messages } => {
                let mut captured: HashSet<usize> = HashSet::new();
                for action in self.action_store.lock().unwrap().owned_actions() {
                    if let QueuedActionPayload::Turn(turn) = &action.payload {
                        if let Some(run_messages) = &turn.capture_run_messages {
                            captured.extend(run_messages.iter().cloned());
                        }
                        let mut store = self.action_store.lock().unwrap();
                        let mut next = action.clone();
                        if let QueuedActionPayload::Turn(turn) = &mut next.payload {
                            turn.cancelled_dispatch_ended = Some(true);
                        }
                        let _ = store.update_action(&next);
                    }
                }
                let _ = messages;
                if !captured.is_empty() {
                    let mut state = self.agent.state();
                    state.messages.retain(|message| !captured.contains(&agent_message_key_of(message)));
                    self.agent.set_state(state);
                }
            }
            _ => {}
        }
        match &event {
            AgentEvent::MessageStart { message } => {
                let role = message.role();
                if role == "user" || role == "custom" {
                    let actions = self
                        .action_store
                        .lock()
                        .unwrap()
                        .actions_for_message(&delivery_message_of(message));
                    for action in actions {
                        let key = agent_message_key_of(message);
                        let mut store = self.action_store.lock().unwrap();
                        let mut next = action.clone();
                        let mut started_primary = false;
                        if let QueuedActionPayload::Turn(turn) = &mut next.payload {
                            for record in turn.base.records.iter_mut() {
                                if delivery_message_key_of(&record.message) == key {
                                    record.started = true;
                                    if record.role == DeliveryRecordRole::Primary {
                                        started_primary = true;
                                    }
                                }
                            }
                        }
                        let _ = store.update_action(&next);
                        if started_primary {
                            if let Ok(ticket) = self.action_store.lock().unwrap().ticket_for(&action) {
                                ticket.settle_delivered("delivered");
                            }
                            self.settle_agent_message(action.agent_message_id.as_deref(), "delivery", None);
                        }
                    }
                }
            }
            AgentEvent::MessageEnd { message } => {
                let role = message.role();
                if role == "user" || role == "custom" {
                    let actions = self
                        .action_store
                        .lock()
                        .unwrap()
                        .actions_for_message(&delivery_message_of(message));
                    for action in actions {
                        let key = agent_message_key_of(message);
                        let mut store = self.action_store.lock().unwrap();
                        let mut next = action.clone();
                        let mut started_primary = false;
                        if let QueuedActionPayload::Turn(turn) = &mut next.payload {
                            for record in turn.base.records.iter_mut() {
                                if delivery_message_key_of(&record.message) == key {
                                    record.durable = true;
                                    if record.role == DeliveryRecordRole::Primary {
                                        started_primary = true;
                                    }
                                }
                            }
                        }
                        let _ = store.update_action(&next);
                        if started_primary && action.lifecycle.state() == ActionLifecycleState::Committing {
                            let mut updated = action.clone();
                            let _ = transition_session_action(
                                &mut updated,
                                ActionLifecycle::Running {
                                    execution: crate::core::session_action_store::ActionExecution::AgentTurn,
                                },
                                &crate::core::session_action_store::TransitionOptions::default(),
                            );
                            self.notify_session_input_checkpoint_change();
                            self.emit_queue_update();
                        }
                    }
                }
            }
            _ => {}
        }
        let session = self.clone();
        self.push_agent_event_task(async move {
            session.process_agent_event(event).await;
        });
    }

    /// `_createRetryPromiseForAgentEnd`.
    fn create_retry_promise_for_agent_end(&self, event: &AgentEvent) {
        let messages = match event {
            AgentEvent::AgentEnd { messages } => messages,
            _ => return,
        };
        if self.retry_promise.lock().unwrap().is_some() {
            return;
        }

        let settings = self.settings_manager.lock().unwrap().get_retry_settings();
        if !settings.enabled {
            return;
        }

        let last_assistant = self.find_last_assistant_in_messages(messages);
        let concrete_auth_failure = last_assistant
            .as_ref()
            .map(|assistant| self.is_concrete_provider_auth_failure(assistant))
            .unwrap_or(false);
        if last_assistant.is_none()
            || (!last_assistant
                .as_ref()
                .map(|assistant| self.is_retryable_error(assistant))
                .unwrap_or(false)
                && !concrete_auth_failure)
        {
            return;
        }
        if concrete_auth_failure {
            if let Some(assistant) = last_assistant.as_ref() {
                self.capture_retry_auth_failure_source(assistant);
            }
        }

        let deferred = create_agent_message_deferred();
        *self.retry_resolve.lock().unwrap() = Some(Arc::new({
            let deferred = deferred.clone();
            move || deferred.resolve()
        }));
        *self.retry_promise.lock().unwrap() = Some(Box::pin(async move {
            let _ = deferred.wait().await;
            Ok(())
        }));
    }

    /// `_findLastAssistantInMessages`.
    fn find_last_assistant_in_messages(&self, messages: &[AgentMessage]) -> Option<AssistantMessage> {
        for message in messages.iter().rev() {
            if let AgentMessage::Message(Message::Assistant(assistant)) = message {
                return Some(assistant.clone());
            }
        }
        None
    }

    /// `_addLoginGuidanceToAuthError`.
    fn add_login_guidance_to_auth_error(&self, event: &AgentEvent) {
        let message = match event {
            AgentEvent::MessageEnd { message } => match message {
                AgentMessage::Message(Message::Assistant(assistant)) => Some(assistant.clone()),
                _ => None,
            },
            AgentEvent::AgentEnd { messages } => self.find_last_assistant_in_messages(messages),
            _ => None,
        };
        let mut message = match message {
            Some(message) => message,
            None => return,
        };
        if message.stop_reason != STOP_REASON_ERROR {
            return;
        }
        let error_message = match &message.error_message {
            Some(error_message) => error_message.clone(),
            None => return,
        };
        if !is_likely_authentication_error(&error_message) {
            return;
        }
        message.error_message = Some(add_login_guidance_to_auth_error(&error_message));
        self.replace_last_assistant_in_state(&message);
    }

    /// `_processAgentEvent`.
    async fn process_agent_event(self: &Arc<Self>, event: AgentEvent) {
        let mut cleared_dispatch_ended = false;
        if let AgentEvent::MessageStart { message } | AgentEvent::MessageEnd { message } = &event {
            if let AgentMessage::Message(Message::ToolResult(_)) = message {
                let mut message = message.clone();
                self.apply_late_ipython_sent_agent_messages(&mut message);
                self.replace_message_in_place(message);
            }
        }
        if let AgentEvent::MessageStart { message } | AgentEvent::MessageEnd { message } = &event {
            if let Some(cleared) = self.capturing_cancelled_action(message) {
                if let QueuedActionPayload::Turn(turn) = &cleared.payload {
                    if let Some(captured) = &turn.capture_run_messages {
                        let captured = captured.clone();
                        let mut state = self.agent.state();
                        state
                            .messages
                            .retain(|message| !captured.contains(&agent_message_key_of(message)));
                        self.agent.set_state(state);
                        return;
                    }
                }
            }
        }
        if let AgentEvent::AgentEnd { messages } = &event {
            let cleared: Vec<QueuedSessionAction> = self
                .action_store
                .lock()
                .unwrap()
                .owned_actions()
                .into_iter()
                .filter(|action| {
                    action.lifecycle.state() == ActionLifecycleState::Cancelled
                        && match &action.payload {
                            QueuedActionPayload::Turn(turn) => turn.capture_run_messages.is_some(),
                            QueuedActionPayload::SessionCommand(_) => false,
                        }
                })
                .collect();
            if !cleared.is_empty() {
                cleared_dispatch_ended = true;
                let mut removed: HashSet<usize> = HashSet::new();
                for action in &cleared {
                    if let QueuedActionPayload::Turn(turn) = &action.payload {
                        if let Some(captured) = &turn.capture_run_messages {
                            removed.extend(captured.iter().cloned());
                        }
                    }
                }
                let mut state = self.agent.state();
                state.messages.retain(|message| !removed.contains(&agent_message_key_of(message)));
                state.error_message = None;
                self.agent.set_state(state);
                *self.last_assistant_message.lock().unwrap() = None;
                let _ = messages;
                for action in cleared {
                    self.action_store.lock().unwrap().release_terminal(&action);
                }
                self.notify_session_input_checkpoint_change();
                self.resolve_retry();
            }
        }

        if let AgentEvent::MessageStart { message } = &event {
            if starts_agent_run(message) {
                *self.overflow_recovery.lock().unwrap() = "idle".to_string();
            }
        }

        self.emit_extension_event(&event).await;
        if let AgentEvent::MessageStart { message } | AgentEvent::MessageEnd { message } = &event {
            if let Some(cleared) = self.capturing_cancelled_action(message) {
                if let QueuedActionPayload::Turn(turn) = &cleared.payload {
                    if let Some(captured) = &turn.capture_run_messages {
                        let captured = captured.clone();
                        let mut state = self.agent.state();
                        state
                            .messages
                            .retain(|message| !captured.contains(&agent_message_key_of(message)));
                        self.agent.set_state(state);
                        return;
                    }
                }
            }
        }

        self.add_login_guidance_to_auth_error(&event);

        self.emit(AgentSessionEvent::Agent(event.clone()));

        if let AgentEvent::MessageEnd { message } = &event {
            match message {
                AgentMessage::Custom(CustomAgentMessage::Custom {
                    custom_type,
                    content,
                    display,
                    details,
                    ..
                }) => {
                    let entry_content = match content {
                        CustomMessageContent::Text(text) => {
                            crate::core::session_manager::CustomMessageEntryContent::Text(text.clone())
                        }
                        CustomMessageContent::Blocks(blocks) => {
                            crate::core::session_manager::CustomMessageEntryContent::Blocks(
                                blocks
                                    .iter()
                                    .map(|block| match block {
                                        pi_agent_core::types::ContentBlock::Text(text) => {
                                            Value::Object({
                                                let mut object = Map::new();
                                                object.insert(
                                                    "type".to_string(),
                                                    Value::String("text".to_string()),
                                                );
                                                object.insert(
                                                    "text".to_string(),
                                                    Value::String(text.text.clone()),
                                                );
                                                object
                                            })
                                        }
                                        pi_agent_core::types::ContentBlock::Image(image) => {
                                            serde_json::to_value(image).unwrap_or(Value::Null)
                                        }
                                    })
                                    .collect(),
                            )
                        }
                    };
                    let _ = self
                        .session_manager
                        .lock()
                        .unwrap()
                        .append_custom_message_entry(custom_type, &entry_content, *display, details.clone());
                }
                AgentMessage::Message(Message::User(_))
                | AgentMessage::Message(Message::Assistant(_))
                | AgentMessage::Message(Message::ToolResult(_)) => {
                    let _ = self
                        .session_manager
                        .lock()
                        .unwrap()
                        .append_message(message.clone());
                }
                _ => {}
            }
            self.begin_rlm_parent_task(message);
            self.consume_started_rlm_continuation(message);

            if let AgentMessage::Message(Message::Assistant(assistant)) = message {
                *self.last_assistant_message.lock().unwrap() = Some(assistant.clone());

                if assistant.stop_reason != STOP_REASON_ERROR {
                    let mut state = self.autonomous_state.lock().unwrap();
                    add_autonomous_usage(&mut state, &assistant.usage);
                }
                if assistant.stop_reason != STOP_REASON_ERROR
                    && assistant.stop_reason != STOP_REASON_ABORTED
                {
                    self.assistant_turns_since_auto_refine.fetch_add(1, Ordering::SeqCst);
                    // In serialized mode, kick off background refinement planning
                    // immediately after the primary stream finishes, while tools
                    // are still executing. The plan is awaited at shouldStopAfterTurn
                    // before applying, so planning overlaps tools only - never another
                    // model request.
                    self.maybe_start_serialized_background_plan();
                }
                if assistant.stop_reason != STOP_REASON_ERROR {
                    *self.overflow_recovery.lock().unwrap() = "idle".to_string();
                }
                if self.is_concrete_provider_auth_failure(assistant) {
                    self.capture_retry_auth_failure_source(assistant);
                }

                // Reset retry counter immediately on successful assistant response
                // This prevents accumulation across multiple LLM calls within a turn
                let retry_attempt = self.retry_attempt.load(Ordering::SeqCst);
                if assistant.stop_reason != STOP_REASON_ERROR && retry_attempt > 0 {
                    self.emit(AgentSessionEvent::AutoRetryEnd {
                        success: true,
                        attempt: retry_attempt as i64,
                        final_error: None,
                    });
                    self.retry_attempt.store(0, Ordering::SeqCst);
                    self.retry_auth_failure_sources.lock().unwrap().clear();
                }
                if self.account_goal_usage_for_assistant_message(assistant) {
                    if let Ok(message) = create_goal_context_message(
                        &self.goal_state(),
                        crate::core::goals::GoalContextKind::BudgetLimit,
                        None,
                    ) {
                        let (text, images) = match &message.content {
                            CustomMessageContent::Text(text) => (text.clone(), None),
                            CustomMessageContent::Blocks(blocks) => normalize_message_content(
                                &blocks
                                    .iter()
                                    .map(|block| match block {
                                        pi_agent_core::types::ContentBlock::Text(text) => {
                                            pi_ai::types::ImageOrTextContent::Text(text.clone())
                                        }
                                        pi_agent_core::types::ContentBlock::Image(image) => {
                                            pi_ai::types::ImageOrTextContent::Image(image.clone())
                                        }
                                    })
                                    .collect::<Vec<_>>(),
                            ),
                        };
                        let _ = self
                            .queue_prepared_prompt(
                                SESSION_INPUT_SCHEDULE_STEER,
                                &text,
                                images,
                                Some(PreparedTurnActionOptions {
                                    message: None,
                                    resume_if_idle: Some(true),
                                    custom_message: Some(message),
                                    ..Default::default()
                                }),
                            )
                            .await;
                    }
                }
            }
        }

        if cleared_dispatch_ended {
            return;
        }

        if let AgentEvent::AgentEnd { messages } = &event {
            let message = {
                let last = self.last_assistant_message.lock().unwrap().clone();
                *self.last_assistant_message.lock().unwrap() = None;
                match last {
                    Some(message) => Some(message),
                    None => {
                        if self.retry_promise.lock().unwrap().is_some() {
                            self.find_last_assistant_in_messages(messages)
                        } else {
                            None
                        }
                    }
                }
            };
            let message = match message {
                Some(message) => message,
                None => {
                    self.resolve_retry();
                    return;
                }
            };

            let concrete_auth_failure = self.is_concrete_provider_auth_failure(&message);
            let retry_concrete_auth_failure =
                concrete_auth_failure && !self.is_structured_permanent_provider_retry_exhausted(&message);
            if self.is_retryable_error(&message) || retry_concrete_auth_failure {
                if retry_concrete_auth_failure {
                    self.capture_retry_auth_failure_source(&message);
                }
                let auth_source_tokens = if retry_concrete_auth_failure {
                    Some(self.retry_auth_failure_sources.lock().unwrap().clone())
                } else {
                    None
                };
                let did_retry = self
                    .handle_retryable_error(&message, retry_concrete_auth_failure, auth_source_tokens)
                    .await?;
                if did_retry {
                    // Retry was initiated, don't proceed to compaction
                    return;
                }
            }

            let compaction_will_retry = self.check_compaction(&message).await?;
            if compaction_will_retry && self.retry_attempt.load(Ordering::SeqCst) > 0 {
                return;
            }
            self.finish_active_retry_with_failure(&message);
            self.resolve_retry();
            if !compaction_will_retry {
                self.handle_rlm_child_turn_outcome(&message, false, None);
                self.queue_pending_rlm_continuation();
                self.deliver_pending_rlm_results().await;
            }
            if !compaction_will_retry {
                self.finish_goal_for_terminal_assistant_message(&message);
                // In serialized mode, agent-callable refine.run is serviced
                // at the shouldStopAfterTurn boundary, not here at agent_end.
                if !self.serialized_refine {
                    let consumed_requested_refine = self.consume_pending_requested_refine();
                    if !consumed_requested_refine {
                        self.schedule_auto_refine_after_agent_end();
                    }
                }
            }
        }
    }

    /// `_resolveRetry`.
    fn resolve_retry(&self) {
        self.retry_generation.fetch_add(1, Ordering::SeqCst);
        {
            let mut edges = self.semantic_edges.lock().unwrap();
            edges.clear_turn_retry();
        }
        *self.retry_metric_message.lock().unwrap() = None;
        let resolve = self.retry_resolve.lock().unwrap().take();
        if let Some(resolve) = resolve {
            resolve();
            *self.retry_promise.lock().unwrap() = None;
            self.notify_session_input_checkpoint_change();
            self.schedule_session_input_pump();
        }
    }

    /// `_findLastAssistantMessage`.
    fn find_last_assistant_message(&self) -> Option<AssistantMessage> {
        let messages = self.agent.state().messages;
        for message in messages.iter().rev() {
            if let AgentMessage::Message(Message::Assistant(assistant)) = message {
                return Some(assistant.clone());
            }
        }
        None
    }

    /// `_replaceMessageInPlace`.
    fn replace_message_in_place(&self, replacement: AgentMessage) {
        // Agent-core stores the finalized message object in its state before emitting message_end.
        // SessionManager persistence happens later in _processAgentEvent() with event.message.
        // Mutating this object in place keeps agent state, later turn/agent events, listeners,
        // and the eventual SessionManager.appendMessage(event.message) persistence in sync.
        let key = agent_message_key_of(&replacement);
        let mut state = self.agent.state();
        for message in state.messages.iter_mut() {
            if agent_message_key_of(message) == key {
                *message = replacement.clone();
                break;
            }
        }
        self.agent.set_state(state);
    }

    fn replace_last_assistant_in_state(&self, replacement: &AssistantMessage) {
        let mut state = self.agent.state();
        for message in state.messages.iter_mut().rev() {
            if let AgentMessage::Message(Message::Assistant(assistant)) = message {
                *assistant = replacement.clone();
                break;
            }
        }
        self.agent.set_state(state);
    }

    /// `_emitExtensionEvent`.
    async fn emit_extension_event(&self, event: &AgentEvent) {
        let runner = match self.extension_runner() {
            Some(runner) => runner,
            None => return,
        };
        match event {
            AgentEvent::AgentStart => {
                self.turn_index.store(0, Ordering::SeqCst);
                let _ = self.session_manager.lock().unwrap().record_git_state_if_changed();
                let _ = runner.emit(ExtensionEvent::AgentStart).await;
            }
            AgentEvent::AgentEnd { messages } => {
                // Also capture at end of turn so commits made during the run (e.g. via a bash tool) land.
                let _ = self.session_manager.lock().unwrap().record_git_state_if_changed();
                let values = messages
                    .iter()
                    .map(|message| serde_json::to_value(message).unwrap_or(Value::Null))
                    .collect();
                let _ = runner
                    .emit(ExtensionEvent::AgentEnd(AgentEndPayload { messages: values }))
                    .await;
            }
            AgentEvent::TurnStart => {
                let _ = runner
                    .emit(ExtensionEvent::TurnStart(TurnStartPayload {
                        turn_index: self.turn_index.load(Ordering::SeqCst) as f64,
                        timestamp: now_ms(),
                    }))
                    .await;
            }
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => {
                let _ = runner
                    .emit(ExtensionEvent::TurnEnd(TurnEndPayload {
                        turn_index: self.turn_index.load(Ordering::SeqCst) as f64,
                        message: serde_json::to_value(message).unwrap_or(Value::Null),
                        tool_results: tool_results
                            .iter()
                            .map(|result| serde_json::to_value(result).unwrap_or(Value::Null))
                            .collect(),
                    }))
                    .await;
                self.turn_index.fetch_add(1, Ordering::SeqCst);
            }
            AgentEvent::MessageStart { message } => {
                let _ = runner
                    .emit(ExtensionEvent::MessageStart(MessageStartPayload {
                        message: serde_json::to_value(message).unwrap_or(Value::Null),
                    }))
                    .await;
            }
            AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } => {
                let _ = runner
                    .emit(ExtensionEvent::MessageUpdate(MessageUpdatePayload {
                        message: serde_json::to_value(message).unwrap_or(Value::Null),
                        assistant_message_event: assistant_message_event.clone(),
                    }))
                    .await;
            }
            AgentEvent::MessageEnd { message } => {
                let replacement = runner
                    .emit_message_end(serde_json::json!({ "type": "message_end", "message": message }))
                    .await;
                if let Some(replacement) = replacement {
                    if let Ok(parsed) = serde_json::from_value::<AgentMessage>(replacement) {
                        self.replace_message_in_place(parsed);
                    }
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                let _ = runner
                    .emit(ExtensionEvent::ToolExecutionStart(ToolExecutionStartPayload {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        args: args.clone(),
                    }))
                    .await;
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
            } => {
                let _ = runner
                    .emit(ExtensionEvent::ToolExecutionUpdate(ToolExecutionUpdatePayload {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        args: args.clone(),
                        partial_result: serde_json::to_value(partial_result).unwrap_or(Value::Null),
                    }))
                    .await;
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => {
                let _ = runner
                    .emit(ExtensionEvent::ToolExecutionEnd(ToolExecutionEndPayload {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        result: serde_json::to_value(result).unwrap_or(Value::Null),
                        is_error: *is_error,
                    }))
                    .await;
            }
        }
    }

    /**
     * Subscribe to agent events.
     * Session persistence is handled internally (saves messages on message_end).
     * Multiple listeners can be added. Returns unsubscribe function for this listener.
     */
    pub fn subscribe(self: &Arc<Self>, listener: AgentSessionEventListener) -> Arc<dyn Fn() + Send + Sync> {
        self.event_listeners.lock().unwrap().push(listener.clone());
        let session = Arc::downgrade(self);
        Arc::new(move || {
            if let Some(session) = session.upgrade() {
                session
                    .event_listeners
                    .lock()
                    .unwrap()
                    .retain(|candidate| !Arc::ptr_eq(candidate, &listener));
            }
        })
    }

    /**
     * Temporarily disconnect from agent events.
     * User listeners are preserved and will receive events again after resubscribe().
     * Used internally during operations that need to pause event processing.
     */
    fn disconnect_from_agent(&self) {
        let unsubscribe = self.unsubscribe_agent.lock().unwrap().take();
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
    }

    /**
     * Reconnect to agent events after _disconnectFromAgent().
     * Preserves all existing listeners.
     */
    fn reconnect_to_agent(self: &Arc<Self>) {
        if self.unsubscribe_agent.lock().unwrap().is_some() {
            // Already connected
            return;
        }
        let session = Arc::downgrade(self);
        let unsubscribe = self.agent.subscribe(Arc::new(move |event: AgentEvent| {
            if let Some(session) = session.upgrade() {
                session.handle_agent_event(event);
            }
        }));
        *self.unsubscribe_agent.lock().unwrap() = Some(Arc::new(unsubscribe));
    }

    /**
     * Async teardown for graceful quit/switch: await the Python kernel's dispose
     * (which flushes a final namespace snapshot) before the synchronous dispose, so
     * the latest state reaches disk instead of racing process exit.
     */
    pub async fn dispose_async(self: &Arc<Self>, kernel_snapshot: Option<bool>) {
        if self.disposed.load(Ordering::SeqCst) {
            self.start_dispose_callbacks().await;
            return;
        }
        let kernel_snapshot = kernel_snapshot.unwrap_or(true);
        // Drain before marking _disposing so a refine triggered at the final
        // agent_end completes instead of being aborted by dispose().
        self.drain_pending_refinement_for_disposal().await;
        if self.disposed.load(Ordering::SeqCst) {
            self.start_dispose_callbacks().await;
            return;
        }
        self.disposing.store(true, Ordering::SeqCst);
        self.session_action_commit_dispose_abort.cancel();
        self.dispose_async_once(kernel_snapshot).await;
    }

    /**
     * Await any in-flight refinement (planning or application) and run a
     * pending auto-refine that was scheduled but not yet started. Called
     * from disposeAsync before _disposing is set so refinement completes
     * before disposal.
     */
    async fn drain_pending_refinement_for_disposal(self: &Arc<Self>) {
        self.scheduled_auto_refine_timers.lock().unwrap().clear();
        // Wait for in-flight refinement (including serialized background plan) to settle.
        while self.refine_in_flight.lock().unwrap().is_some()
            || self.refine_plan_in_flight.lock().unwrap().is_some()
            || self.serialized_plan_in_flight.lock().unwrap().is_some()
        {
            if self.refine_in_flight.lock().unwrap().is_some() {
                self.refine_in_flight.lock().unwrap().take();
            } else if self.refine_plan_in_flight.lock().unwrap().is_some() {
                self.refine_plan_in_flight.lock().unwrap().take();
            } else {
                tokio::task::yield_now().await;
            }
        }
        // Drain an agent-callable refine.run request that was scheduled but
        // not yet consumed. Use the direct serialized path (no waitForIdle)
        // since the agent may still own activeRun at the final agent_end.
        let pending = self.pending_requested_refine.lock().unwrap().take();
        if let Some(pending) = pending {
            let options = RefineOptions {
                instructions: pending.instructions,
                rollback_id: None,
                global: pending.global,
                retry: None,
                evidence: None,
                max_output_tokens: None,
            };
            // Best-effort drain; refinement errors must not block disposal.
            let _ = self.run_serialized_refine(&options, REFINEMENT_SOURCE_SELF).await;
            // Stamp cooldown and reset counter so the interval check below
            // does not trigger a duplicate refine after the explicit drain.
            *self.last_auto_refine_review_at.lock().unwrap() = now_ms();
            self.assistant_turns_since_auto_refine.store(0, Ordering::SeqCst);
        }
        // A serialized compaction can finish without another model turn. Drain its
        // pending review here so disposal does not silently lose the trigger.
        if self.serialized_refine
            && self.compact_auto_refine_pending.load(Ordering::SeqCst)
            && self.auto_refine_allowed_for_session()
        {
            let compact_settings = self
                .settings_manager
                .lock()
                .unwrap()
                .get_auto_refine_settings();
            if !compact_settings.enabled || !compact_settings.compact {
                self.compact_auto_refine_pending.store(false, Ordering::SeqCst);
            } else {
                let now = now_ms();
                let last = *self.last_auto_refine_review_at.lock().unwrap();
                let under_cooldown = last > 0.0 && now - last < compact_settings.cooldown_ms;
                self.compact_auto_refine_pending.store(false, Ordering::SeqCst);
                if !under_cooldown {
                    // Best-effort drain; refinement errors must not block disposal.
                    let _ = self
                        .run_serialized_auto_refine_review(
                            AutoRefineReason::Compact,
                            self.auto_refine_branch_version.load(Ordering::SeqCst),
                        )
                        .await;
                    return;
                }
            }
        }

        // If auto-refine is due but has not started yet, run it now so the
        // refinement is persisted before disposal. Use the direct serialized
        // path in serialized mode, or _maybeAutoRefine in interactive mode
        // (where the agent is idle at this point).
        if self.disposed.load(Ordering::SeqCst) || !self.auto_refine_allowed_for_session() {
            return;
        }
        let settings = self.settings_manager.lock().unwrap().get_auto_refine_settings();
        if !settings.enabled {
            return;
        }
        if self.assistant_turns_since_auto_refine.load(Ordering::SeqCst) < settings.turn_interval {
            return;
        }
        let now = now_ms();
        let last = *self.last_auto_refine_review_at.lock().unwrap();
        let under_cooldown = last > 0.0 && now - last < settings.cooldown_ms;
        if under_cooldown {
            return;
        }
        if self.serialized_refine {
            self.run_serialized_refine_checkpoint().await;
        } else {
            self.maybe_auto_refine(AutoRefineReason::TurnInterval).await;
        }
    }

    async fn dispose_async_once(self: &Arc<Self>, kernel_snapshot: bool) {
        // Flush kernels/traces for both still-running and retained children; the sync
        // dispose() below only tears them down synchronously.
        let runs: Vec<Arc<Mutex<RlmChildRun>>> = self
            .active_rlm_child_runs
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for run in runs {
            let child_session = run.lock().unwrap().session.clone();
            let child_session = match child_session {
                Some(session) => session,
                None => continue,
            };
            let detached_deletion = run.lock().unwrap().detached_deletion.is_some();
            if detached_deletion {
                run.lock().unwrap().suppress_terminal_notice = Some(true);
                child_session.dispose_async(None).await;
                if !run.lock().unwrap().settled {
                    self.finish_rlm_run_deletion(&run).await;
                }
            } else {
                child_session.dispose_async(None).await;
            }
        }
        let unsubscribes: Vec<Arc<dyn Fn() + Send + Sync>> = self
            .rlm_child_unsubscribes
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for unsubscribe in unsubscribes {
            unsubscribe();
        }
        self.rlm_child_unsubscribes.lock().unwrap().clear();
        let children: Vec<Arc<AgentSession>> = self
            .rlm_child_sessions
            .lock()
            .unwrap()
            .values()
            .map(|child| child.session.clone())
            .collect();
        for session in children {
            session.dispose_async(None).await;
        }
        self.rlm_child_sessions.lock().unwrap().clear();
        self.rlm_child_cleanup_failures.lock().unwrap().clear();
        self.deleted_rlm_child_ids.lock().unwrap().clear();
        if let Some(provisioner) = self.ipython_kernel_provisioner.lock().unwrap().clone() {
            // a failed kernel startup already cleaned up after itself
            let _ = provisioner.dispose(Some(kernel_snapshot)).await;
        }
        self.dispose();
        self.start_dispose_callbacks().await;
    }

    async fn start_dispose_callbacks(&self) {
        let callbacks: Vec<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>> =
            self.dispose_callbacks.lock().unwrap().drain(..).collect();
        for callback in callbacks {
            // Disposal remains best-effort; one owner must not block the rest.
            let _ = callback().await;
        }
    }

    /// `dispose()`.
    pub fn dispose(self: &Arc<Self>) {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        {
            let runs = self.unsettled_rlm_child_runs.lock().unwrap();
            for run in runs.iter() {
                run.lock().unwrap().suppress_terminal_notice = Some(true);
            }
        }
        for controller in self.rlm_quiescence_wait_aborts.lock().unwrap().iter() {
            controller.cancel();
        }
        self.session_action_commit_dispose_abort.cancel();
        // Invalidate scheduled timers and abort any in-flight review so a late
        // resolution cannot write harness state or re-subscribe handlers.
        self.scheduled_auto_refine_timers.lock().unwrap().clear();
        *self.serialized_plan_in_flight.lock().unwrap() = None;
        *self.serialized_explicit_refine_options.lock().unwrap() = None;
        *self.pending_requested_refine.lock().unwrap() = None;
        self.discard_pending_auto_refine(true);
        self.auto_refine_branch_version.fetch_add(1, Ordering::SeqCst);
        self.cancel_active_rlm_child_runs("Parent session disposed");
        let unsubscribes: Vec<Arc<dyn Fn() + Send + Sync>> = self
            .rlm_child_unsubscribes
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for unsubscribe in unsubscribes {
            unsubscribe();
        }
        self.rlm_child_unsubscribes.lock().unwrap().clear();
        let children: Vec<Arc<AgentSession>> = self
            .rlm_child_sessions
            .lock()
            .unwrap()
            .values()
            .map(|child| child.session.clone())
            .collect();
        for session in children {
            session.dispose();
        }
        self.rlm_child_sessions.lock().unwrap().clear();
        self.rlm_child_cleanup_failures.lock().unwrap().clear();
        self.deleted_rlm_child_ids.lock().unwrap().clear();
        self.pending_next_turn_messages.lock().unwrap().clear();
        let delivery_error = "Session disposed before prompt delivery.";
        let completion_error = "Session disposed before prompt completion.";
        self.reject_queued_agent_message_deliveries(delivery_error, Some(completion_error));
        let outcomes: Vec<String> = self.agent_message_outcomes.lock().unwrap().keys().cloned().collect();
        for agent_message_id in outcomes {
            self.settle_agent_message(Some(&agent_message_id), "delivery", Some(delivery_error));
            self.settle_agent_message(Some(&agent_message_id), "completion", Some(completion_error));
        }
        self.cancel_session_actions(&|_| true, delivery_error, None);
        self.agent.clear_all_queues();
        if let Some(runner) = self.extension_runner() {
            runner.invalidate(Some(
                "This extension ctx is stale after session replacement or reload. Do not use a captured pi or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload()."
                    .to_string(),
            ));
        }
        self.disconnect_from_agent();
        self.event_listeners.lock().unwrap().clear();
        let _ = pi_ai::session_resources::cleanup_session_resources(Some(&self.session_id()));
        let session = self.clone();
        tokio::spawn(async move {
            session.start_dispose_callbacks().await;
        });
    }

    /// `registerDisposeCallback`.
    pub fn register_dispose_callback(
        &self,
        callback: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>,
    ) {
        if self.disposed.load(Ordering::SeqCst) {
            // Late registration follows the same best-effort disposal contract.
            let _ = callback();
            return;
        }
        self.dispose_callbacks.lock().unwrap().push(callback);
    }

    /// `get state()`.
    pub fn state(&self) -> AgentState {
        self.agent.state()
    }

    /// `get model()`.
    pub fn model(&self) -> Option<Model> {
        Some(self.agent.state().model)
    }

    /// `get thinkingLevel()`.
    pub fn thinking_level(&self) -> ThinkingLevel {
        self.agent.state().thinking_level
    }

    /// `get serviceTier()`.
    pub fn service_tier(&self) -> ServiceTier {
        self.agent.state().service_tier
    }

    /// `get isStreaming()`.
    pub fn is_streaming(&self) -> bool {
        self.agent.state().is_streaming
    }

    /// `get systemPrompt()`.
    pub fn system_prompt(&self) -> String {
        self.agent.state().system_prompt
    }

    /// `get retryAttempt()`.
    pub fn retry_attempt(&self) -> i64 {
        self.retry_attempt.load(Ordering::SeqCst) as i64
    }

    /// `getActiveToolNames()`.
    pub fn get_active_tool_names(&self) -> Vec<String> {
        self.agent
            .state()
            .tools
            .map(|tools| tools.into_iter().map(|tool| tool.name).collect())
            .unwrap_or_default()
    }

    /// `getAllTools()`.
    pub fn get_all_tools(&self) -> Vec<Value> {
        self.tool_definitions
            .lock()
            .unwrap()
            .values()
            .map(|entry| {
                serde_json::json!({
                    "name": entry.definition.name,
                    "description": entry.definition.description,
                    "parameters": entry.definition.parameters,
                    "sourceInfo": entry.source_info,
                })
            })
            .collect()
    }

    /// `getToolDefinition(name)`.
    pub fn get_tool_definition(
        &self,
        name: &str,
    ) -> Option<crate::core::extensions::types::ToolDefinition> {
        self.tool_definitions
            .lock()
            .unwrap()
            .get(name)
            .map(|entry| entry.definition.clone())
    }

    /// `setActiveToolsByName`.
    pub fn set_active_tools_by_name(&self, tool_names: &[String]) {
        let mut tools: Vec<AgentTool> = Vec::new();
        let mut valid_tool_names: Vec<String> = Vec::new();
        let mut seen_tool_names: HashSet<String> = HashSet::new();
        for name in tool_names {
            if seen_tool_names.contains(name) {
                continue;
            }
            let tool = self.tool_registry.lock().unwrap().get(name).cloned();
            if let Some(tool) = tool {
                seen_tool_names.insert(name.clone());
                tools.push(tool);
                valid_tool_names.push(name.clone());
            }
        }
        let mut state = self.agent.state();
        state.tools = Some(tools);
        self.agent.set_state(state);

        *self.base_system_prompt.lock().unwrap() = self.rebuild_system_prompt(&valid_tool_names);
        let mut state = self.agent.state();
        state.system_prompt = self.base_system_prompt.lock().unwrap().clone();
        self.agent.set_state(state);
    }

    /// `get isCompacting()`.
    pub fn is_compacting(&self) -> bool {
        self.auto_compaction_abort_controller.lock().unwrap().is_some()
            || self.compaction_abort_controller.lock().unwrap().is_some()
            || self.branch_summary_abort_controller.lock().unwrap().is_some()
    }

    /// `get messages()`.
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.agent.state().messages
    }

    /// `buildSessionContext`.
    pub fn build_session_context(&self) -> SessionContext {
        let model = self.agent.state().model;
        let mut context = self
            .session_manager
            .lock()
            .unwrap()
            .build_session_context(Some(&model));
        for message in context.messages.iter_mut() {
            self.apply_late_ipython_sent_agent_messages(message);
        }
        self.merge_unpersisted_outcomes(&mut context.messages);
        context
    }

    /// `_mergeUnpersistedOutcomes`.
    fn merge_unpersisted_outcomes(&self, messages: &mut Vec<AgentMessage>) {
        for outcome in self.unpersisted_outcomes.lock().unwrap().iter() {
            let mut insert_at = messages.len();
            while insert_at > 0 && agent_message_timestamp(&messages[insert_at - 1]) > outcome.timestamp {
                insert_at -= 1;
            }
            messages.insert(
                insert_at,
                AgentMessage::Custom(CustomAgentMessage::Custom {
                    custom_type: outcome.custom_type.clone(),
                    content: outcome.content.clone(),
                    display: outcome.display,
                    details: outcome.details.clone(),
                    timestamp: outcome.timestamp,
                }),
            );
        }
    }

    /// `get steeringMode()`.
    pub fn steering_mode(&self) -> String {
        self.steering_mode.lock().unwrap().clone()
    }

    /// `get followUpMode()`.
    pub fn follow_up_mode(&self) -> String {
        self.follow_up_mode.lock().unwrap().clone()
    }

    /// `get sessionFile()`.
    pub fn session_file(&self) -> Option<String> {
        self.session_manager.lock().unwrap().get_session_file()
    }

    /// `get sessionId()`.
    pub fn session_id(&self) -> String {
        self.session_manager.lock().unwrap().get_session_id()
    }

    /// `get rlmDepth()`.
    pub fn rlm_depth(&self) -> i64 {
        self.rlm_depth
    }

    /// `get semanticEdges()`.
    pub fn semantic_edges(&self) -> Arc<Mutex<crate::core::semantic_edges::SemanticEdgeRecorder>> {
        self.semantic_edges.clone()
    }

    /// `get rlmMaxDepth()`.
    pub fn rlm_max_depth(&self) -> i64 {
        *self.rlm_max_depth.lock().unwrap()
    }

    /// `get sessionName()`.
    pub fn session_name(&self) -> Option<String> {
        self.session_manager.lock().unwrap().get_session_name()
    }

    /// `get goalState()`.
    pub fn goal_state(&self) -> GoalState {
        self.goal_with_current_wall_clock(None)
    }

    /// `getAutonomousStatus()`.
    pub fn get_autonomous_status(&self) -> AgentAutonomousStatus {
        autonomous_status(&self.autonomous_state.lock().unwrap())
    }

    /// `recordHostAutonomousContinuation`.
    pub fn record_host_autonomous_continuation(&self) {
        add_autonomous_continuation(&mut self.autonomous_state.lock().unwrap());
    }

    /// `refreshAutonomousGates`.
    pub async fn refresh_autonomous_gates(&self) {
        refresh_autonomous_quality_gates(&mut self.autonomous_state.lock().unwrap());
    }

    /// `_runWithAutonomousContinuationSuppressed`.
    async fn run_with_autonomous_continuation_suppressed<T, F>(self: &Arc<Self>, work: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.autonomous_continuation_suppression_depth
            .fetch_add(1, Ordering::SeqCst);
        let result = work.await;
        self.autonomous_continuation_suppression_depth
            .fetch_sub(1, Ordering::SeqCst);
        result
    }

    /// `_markAutonomousContinuationSuppressed`.
    fn mark_autonomous_continuation_suppressed(&self, message: &AgentMessage) {
        self.autonomous_continuation_suppressed_messages
            .lock()
            .unwrap()
            .insert(agent_message_key_of(message));
    }

    /// `get scopedModels()`.
    pub fn scoped_models(&self) -> Vec<ScopedModel> {
        self.scoped_models.clone()
    }

    /// `setScopedModels`.
    pub fn set_scoped_models(&mut self, scoped_models: Vec<ScopedModel>) {
        self.scoped_models = scoped_models;
    }

    /// `get promptTemplates()`.
    pub fn prompt_templates(&self) -> Vec<PromptTemplate> {
        self.resource_loader.get_prompts().prompts
    }

    /// `_normalizePromptSnippet`.
    fn normalize_prompt_snippet(&self, text: Option<&str>) -> Option<String> {
        let text = text?;
        if text.is_empty() {
            return None;
        }
        let one_line = text
            .replace(['\r', '\n'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if one_line.is_empty() {
            None
        } else {
            Some(one_line)
        }
    }

    /// `_normalizePromptGuidelines`.
    fn normalize_prompt_guidelines(&self, guidelines: Option<&[String]>) -> Vec<String> {
        let guidelines = match guidelines {
            Some(guidelines) if !guidelines.is_empty() => guidelines,
            _ => return Vec::new(),
        };

        let mut unique: Vec<String> = Vec::new();
        for guideline in guidelines {
            let normalized = guideline.trim().to_string();
            if !normalized.is_empty() && !unique.contains(&normalized) {
                unique.push(normalized);
            }
        }
        unique
    }

    /// `_rebuildSystemPrompt`.
    fn rebuild_system_prompt(&self, tool_names: &[String]) -> String {
        let valid_tool_names: Vec<String> = tool_names
            .iter()
            .filter(|name| self.tool_registry.lock().unwrap().contains_key(*name))
            .cloned()
            .collect();
        let mut tool_snippets: Map<String, Value> = Map::new();
        let mut prompt_guidelines: Vec<String> = Vec::new();
        for name in &valid_tool_names {
            if let Some(snippet) = self.tool_prompt_snippets.lock().unwrap().get(name) {
                tool_snippets.insert(name.clone(), Value::String(snippet.clone()));
            }

            if let Some(tool_guidelines) = self.tool_prompt_guidelines.lock().unwrap().get(name) {
                prompt_guidelines.extend(tool_guidelines.iter().cloned());
            }
        }

        let loader_system_prompt = self.resource_loader.get_system_prompt();
        let loader_append_system_prompt = self.resource_loader.get_append_system_prompt();
        let append_system_prompt = (!loader_append_system_prompt.is_empty())
            .then(|| loader_append_system_prompt.join("\n\n"));
        let loaded_skills = self.model_visible_skills();
        let loaded_context_files = self.resource_loader.get_agents_files().agents_files;

        let options = BuildSystemPromptOptions {
            custom_prompt: loader_system_prompt,
            selected_tools: Some(valid_tool_names.clone()),
            tool_snippets: Some(tool_snippets),
            prompt_guidelines: Some(prompt_guidelines),
            append_system_prompt,
            cwd: self.cwd.clone(),
            messages_path: self.session_manager.lock().unwrap().get_session_file(),
            context_files: Some(loaded_context_files),
            skills: Some(loaded_skills),
            allow_recursion: Some(self.rlm_depth < self.rlm_max_depth()),
            rlm_depth: Some(self.rlm_depth as f64),
            rlm_parent_agent: self.rlm_parent_agent.clone(),
            ..Default::default()
        };
        build_system_prompt(&options)
    }

    /// `_refreshExtensionSystemPrompt`.
    fn refresh_extension_system_prompt(&self, extension_prompt: &str, base_snapshot: &str) -> String {
        let base = self.base_system_prompt.lock().unwrap().clone();
        if base == base_snapshot {
            return extension_prompt.to_string();
        }
        if !extension_prompt.contains(base_snapshot) {
            return extension_prompt.to_string();
        }
        extension_prompt.replacen(base_snapshot, &base, 1)
    }

    /// `_finishSubmissionNormalization`.
    fn finish_submission_normalization(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        policy: &SubmissionNormalizationPolicy,
    ) -> NormalizedSubmission {
        let mut expanded_text = text.to_string();
        if policy.expand_skills {
            expanded_text = self.expand_skill_command(&expanded_text);
        }
        if policy.expand_prompt_templates {
            expanded_text = expand_prompt_template(&expanded_text, &self.prompt_templates());
        }
        NormalizedSubmission::Prompt {
            text: expanded_text,
            images,
        }
    }

    /// `_normalizeSubmission`.
    fn normalize_submission(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        policy: &SubmissionNormalizationPolicy,
    ) -> BoxFuture<Result<NormalizedSubmission, String>> {
        if policy.parse_session_commands {
            if let Some(command) = parse_session_slash_command(text) {
                return Box::pin(async move {
                    Ok(NormalizedSubmission::SessionCommand {
                        text: text.to_string(),
                        images,
                        command,
                    })
                });
            }
        }

        if text.starts_with('/') {
            if policy.extension_commands == SUBMISSION_EXTENSION_COMMAND_POLICY_EXECUTE {
                if let Some(completion) = self.execute_extension_command(text) {
                    return Box::pin(async move {
                        Ok(NormalizedSubmission::ExtensionCommand { completion })
                    });
                }
            } else if policy.extension_commands == SUBMISSION_EXTENSION_COMMAND_POLICY_REJECT {
                if let Err(error) = self.throw_if_extension_command(text) {
                    return Box::pin(async move { Err(error) });
                }
            }
        }

        if policy.input_source.is_some() {
            if let Some(runner) = self.extension_runner() {
                if runner.has_handlers("input") {
                    runner.emit_input(text, images.clone(), policy.input_source.unwrap().as_str());
                }
            }
        }

        let result = self.finish_submission_normalization(text, images, policy);
        Box::pin(async move { Ok(result) })
    }

    /// `_runPreTurnCompaction`.
    async fn run_pre_turn_compaction(self: &Arc<Self>) {
        let last_assistant = self.find_last_assistant_message();
        if let Some(last_assistant) = last_assistant {
            let _ = self.check_compaction(&last_assistant).await;
        } else {
            let model = self.agent.state().model;
            let tokens = estimate_context_tokens(&self.agent.state().messages).tokens;
            let settings = self.compaction_settings();
            if should_compact_for_model(tokens, &model, &settings) {
                let _ = self.run_auto_compaction(COMPACTION_REASON_THRESHOLD, false).await;
            }
        }
    }

    /// `_prepareForCommit`.
    async fn prepare_for_commit(
        self: &Arc<Self>,
        policy: &CommitPreparationPolicy,
        prepare: impl std::future::Future<Output = Result<PreparedPromptPreparation, String>>,
        should_commit: Option<Arc<dyn Fn(&PreparedPromptPreparation) -> bool + Send + Sync>>,
    ) -> Result<Option<PreparedPromptPreparation>, String> {
        if policy.initial_refine_barrier == REFINE_BARRIER_ALWAYS
            || (policy.initial_refine_barrier == REFINE_BARRIER_IF_IN_FLIGHT
                && self.refine_in_flight.lock().unwrap().is_some())
        {
            self.wait_for_refine_idle().await;
        }
        if policy.flush_pending_bash_before_validation {
            self.flush_pending_bash_messages();
        }
        if policy.validate_model_and_auth {
            self.validate_can_start_agent_run().await?;
        }
        if !policy.flush_pending_bash_before_validation {
            self.flush_pending_bash_messages();
        }

        if policy.pre_turn_compaction == PRE_TURN_COMPACTION_BEFORE_MODEL_SELECTION {
            self.run_pre_turn_compaction().await;
        }
        if policy.await_pending_model_selection {
            let pending = self.pending_model_select_emit();
            if let Some(pending) = pending {
                let _ = pending.await;
            }
        }
        if policy.pre_turn_compaction == PRE_TURN_COMPACTION_AFTER_MODEL_SELECTION {
            self.run_pre_turn_compaction().await;
        }

        let prepared = prepare.await?;
        if let Some(should_commit) = should_commit {
            if !should_commit(&prepared) {
                return Ok(None);
            }
        }
        if policy.final_refine_barrier == REFINE_BARRIER_ALWAYS
            || (policy.final_refine_barrier == REFINE_BARRIER_IF_IN_FLIGHT
                && self.refine_in_flight.lock().unwrap().is_some())
        {
            self.wait_for_refine_idle().await;
        }
        Ok(Some(prepared))
    }

    /// `_applyPreparedSystemPrompt`.
    fn apply_prepared_system_prompt(
        &self,
        preparation: Option<&PreparedPromptPreparation>,
        preserve_empty_extension_prompt: bool,
    ) {
        let extension_prompt = preparation.and_then(|preparation| preparation.result.system_prompt.clone());
        let has_extension_prompt = if preserve_empty_extension_prompt {
            extension_prompt.is_some()
        } else {
            extension_prompt.as_ref().map(|value| !value.is_empty()).unwrap_or(false)
        };
        let next = if has_extension_prompt && preparation.is_some() {
            self.refresh_extension_system_prompt(
                extension_prompt.as_deref().unwrap_or_default(),
                &preparation.unwrap().base_prompt_snapshot,
            )
        } else {
            self.base_system_prompt.lock().unwrap().clone()
        };
        let mut state = self.agent.state();
        state.system_prompt = next;
        self.agent.set_state(state);
    }

    /// `_canStartSessionActionImmediately`.
    fn can_start_session_action_immediately(&self) -> bool {
        !self.is_streaming()
            && !self.is_compacting()
            && !self.is_retrying()
            && !self.is_bash_running()
            && !self.session_input_pump_suspended.load(Ordering::SeqCst)
            && self.queued_work_pauses.lock().unwrap().is_empty()
            && !self.disposed.load(Ordering::SeqCst)
            && !self.disposing.load(Ordering::SeqCst)
    }

    /**
     * Send a prompt to the agent.
     * - Handles extension commands (registered via pi.registerCommand) immediately, even during streaming
     * - Expands file-based prompt templates by default
     * - During streaming, queues via steer() or followUp() based on streamingBehavior option
     * - Validates model and API key before sending (when not streaming)
     * @throws Error if streaming and no streamingBehavior specified
     * @throws Error if no model selected or no API key available (when not streaming)
     */
    pub async fn prompt(self: &Arc<Self>, text: &str, options: Option<PromptOptions>) -> Result<(), String> {
        self.prompt_internal(text, options).await
    }

    /// `promptUntilAccepted`.
    pub async fn prompt_until_accepted(
        self: &Arc<Self>,
        text: &str,
        options: Option<PromptOptions>,
    ) -> Result<(), String> {
        let mut options = options.unwrap_or_default();
        let mut internal = InternalPromptOptions {
            base: options.clone(),
            skip_pre_prompt_work: None,
            return_after_accepted: Some(true),
            agent_message_id: None,
        };
        options.return_after_accepted = Some(true);
        internal.base = options;
        self.prompt_internal(text, Some(internal.base)).await
    }

    /// `promptAndWait`.
    pub async fn prompt_and_wait(
        self: &Arc<Self>,
        text: &str,
        options: Option<PromptOptions>,
    ) -> Result<(), String> {
        let options = options.unwrap_or_default();
        let agent_message_id = options
            .agent_message_id
            .clone()
            .unwrap_or_else(|| format!("prompt-wait:{}", uuid::Uuid::new_v4()));
        {
            let outcomes = self.agent_message_outcomes.lock().unwrap();
            if outcomes
                .get(&agent_message_id)
                .map(|outcome| outcome.completion.is_some())
                .unwrap_or(false)
            {
                return Err(format!(
                    "Prompt completion id is already in use: {agent_message_id}"
                ));
            }
        }
        let completion = {
            let mut outcomes = self.agent_message_outcomes.lock().unwrap();
            let outcome = outcomes.entry(agent_message_id.clone()).or_default();
            outcome.completion = Some(create_agent_message_deferred());
            outcome.completion.clone().unwrap()
        };
        let mut prompt_options = options.clone();
        prompt_options.agent_message_id = Some(agent_message_id.clone());
        let result = self
            .prompt_until_accepted(text, Some(prompt_options))
            .await;
        if let Err(error) = result {
            self.settle_agent_message(Some(&agent_message_id), "completion", Some(&error));
            return Err(error);
        }
        completion
            .wait()
            .await
            .map_err(|error| error.to_string())
    }

    /// `acceptAgentMessagePrompt`.
    pub async fn accept_agent_message_prompt(
        self: &Arc<Self>,
        text: &str,
        options: Option<PromptOptions>,
    ) -> Result<(), String> {
        let options = options.unwrap_or_default();
        let custom_message = options.custom_message.clone().filter(|message| {
            is_agent_session_message(&AgentMessage::Custom(CustomAgentMessage::Custom {
                custom_type: message.custom_type.clone(),
                content: message.content.clone(),
                display: message.display,
                details: message.details.clone(),
                timestamp: message.timestamp,
            }))
        });
        let clear_epoch = self.agent_message_clear_epoch.load(Ordering::SeqCst);
        let admission_committed = Arc::new({
            let options = options.clone();
            let session = self.clone();
            move || {
                if let Some(committed) = &options.admission_committed {
                    committed();
                }
                if clear_epoch != session.agent_message_clear_epoch.load(Ordering::SeqCst) {
                    return Err("Agent message was cleared before admission".to_string());
                }
                Ok(())
            }
        });
        if self.session_input_pump_suspended.load(Ordering::SeqCst)
            && self.is_busy_for_session_input("preflight")
            && options.queue_if_busy == Some(true)
            && options.streaming_behavior.is_some()
        {
            admission_committed()?;
            let queued = self
                .queue_agent_message_prompt(
                    text,
                    options.streaming_behavior.as_deref().unwrap_or_default(),
                    custom_message,
                )
                .await?;
            if let Some(preflight) = &options.preflight_result {
                preflight(queued, queued);
            }
            return Ok(());
        }
        let mut internal = options.clone();
        internal.resume_if_idle = Some(false);
        internal.expand_prompt_templates = Some(false);
        internal.skip_input_handlers = Some(true);
        internal.agent_message_id = Some(
            options
                .agent_message_id
                .clone()
                .or_else(|| {
                    custom_message.as_ref().and_then(|message| {
                        message
                            .details
                            .as_ref()
                            .and_then(|details| details.get("id"))
                            .and_then(Value::as_str)
                            .map(|value| value.to_string())
                    })
                })
                .unwrap_or_else(|| parse_agent_session_message_prompt_id(text).unwrap_or_default()),
        );
        internal.custom_message = custom_message;
        let skip_pre_prompt_work = true;
        self.prompt_internal_with(text, internal, Some(skip_pre_prompt_work), Some(true))
            .await
    }

    /// `queueAgentMessagePrompt`.
    pub async fn queue_agent_message_prompt(
        self: &Arc<Self>,
        text: &str,
        streaming_behavior: &str,
        custom_message: Option<CustomMessage>,
    ) -> Result<bool, String> {
        let agent_message_id = custom_message
            .as_ref()
            .and_then(|message| {
                message
                    .details
                    .as_ref()
                    .and_then(|details| details.get("id"))
                    .and_then(Value::as_str)
                    .map(|value| value.to_string())
            })
            .or_else(|| parse_agent_session_message_prompt_id(text));
        if streaming_behavior == SESSION_INPUT_SCHEDULE_STEER {
            self.queue_prepared_prompt(
                SESSION_INPUT_SCHEDULE_STEER,
                text,
                None,
                Some(PreparedTurnActionOptions {
                    agent_message_id,
                    message: None,
                    custom_message,
                    ..Default::default()
                }),
            )
            .await?;
            return Ok(true);
        }
        self.queue_prepared_prompt(
            SESSION_INPUT_SCHEDULE_FOLLOW_UP,
            text,
            None,
            Some(PreparedTurnActionOptions {
                agent_message_id,
                message: None,
                custom_message,
                ..Default::default()
            }),
        )
        .await
    }

    /// `promptHeartbeat`.
    pub async fn prompt_heartbeat(
        self: &Arc<Self>,
        job: &AgentCronJob,
        options: Option<PromptOptions>,
    ) -> Result<(), String> {
        let message = create_heartbeat_prompt_message(
            job.id.clone(),
            job.schedule.clone(),
            job.status.clone(),
            job.run_count,
            job.prompt.clone(),
            job.next_run_at.clone(),
            job.last_run_at.clone(),
            now_ms() as i64,
        );
        let mut options = options.unwrap_or_default();
        if options.follow_up_queue_key.is_none() {
            options.follow_up_queue_key = Some(format!("heartbeat:{}", job.id));
        }
        options.resume_if_idle = Some(true);
        self.prompt_injected_message(&job.prompt, message, Some(options), None)
            .await
    }

    /// `_isRlmTerminalNotice`.
    fn is_rlm_terminal_notice(&self, message: &CustomMessage) -> bool {
        message.custom_type == RLM_CHILD_TERMINAL_NOTICE_CUSTOM_TYPE
            || message.custom_type == RLM_CHILD_FAILURE_CUSTOM_TYPE
    }

    /// `_assertRlmTerminalNotice`.
    fn assert_rlm_terminal_notice(&self, message: &CustomMessage) -> Result<(), String> {
        if !self.is_rlm_terminal_notice(message) {
            return Err("Deferred terminal admission only accepts RLM child terminal notices.".to_string());
        }
        Ok(())
    }

    /// `_isRlmTerminalNoticeAction`.
    fn is_rlm_terminal_notice_action(&self, action: &QueuedSessionAction) -> bool {
        if !matches!(action.payload, QueuedActionPayload::Turn(_)) {
            return false;
        }
        let record = match primary_delivery_record(action) {
            Ok(record) => record,
            Err(_) => return false,
        };
        match record.message {
            DeliveryMessage::Custom(custom) => self.is_rlm_terminal_notice(&custom),
            DeliveryMessage::User(_) => false,
        }
    }

    /// `_hasDeferredRlmTerminalNotices`.
    fn has_deferred_rlm_terminal_notices(&self) -> bool {
        self.pending_next_turn_messages
            .lock()
            .unwrap()
            .iter()
            .any(|message| self.is_rlm_terminal_notice(message))
    }

    /// `_enqueueRlmTerminalNoticeAction`.
    fn enqueue_rlm_terminal_notice_action(self: &Arc<Self>, message: CustomMessage) -> Result<(), String> {
        self.assert_rlm_terminal_notice(&message)?;
        let text = match &message.content {
            CustomMessageContent::Text(text) => text.clone(),
            CustomMessageContent::Blocks(_) => String::new(),
        };
        let action = self.create_prepared_turn_action(
            SESSION_INPUT_SCHEDULE_FOLLOW_UP,
            &text,
            None,
            Some(PreparedTurnActionOptions {
                custom_message: Some(message),
                suppress_autonomous_continuation: Some(true),
                resume_if_idle: Some(false),
                source: Some("internal".to_string()),
                execution_policy: Some(self.turn_execution_policy("injected", None)),
                queue_visible: Some(false),
                ..Default::default()
            }),
        );
        self.durable_rlm_terminal_notice_action_ids
            .lock()
            .unwrap()
            .insert(action.id.clone());
        let result = self.admit_session_input(action.clone(), true);
        if let Err(error) = result {
            self.durable_rlm_terminal_notice_action_ids
                .lock()
                .unwrap()
                .remove(&action.id);
            return Err(error);
        }
        Ok(())
    }

    /// `_flushDeferredRlmTerminalNotices`.
    fn flush_deferred_rlm_terminal_notices(self: &Arc<Self>) {
        if !self.session_input_admission_pauses.lock().unwrap().is_empty()
            || self.session_input_pump_suspended.load(Ordering::SeqCst)
            || !self.queued_work_pauses.lock().unwrap().is_empty()
            || self.disposed.load(Ordering::SeqCst)
            || self.disposing.load(Ordering::SeqCst)
        {
            return;
        }
        loop {
            let index = {
                let pending = self.pending_next_turn_messages.lock().unwrap();
                pending
                    .iter()
                    .position(|message| self.is_rlm_terminal_notice(message))
            };
            let index = match index {
                Some(index) => index,
                None => break,
            };
            let message = self.pending_next_turn_messages.lock().unwrap()[index].clone();
            if self.enqueue_rlm_terminal_notice_action(message).is_err() {
                return;
            }
            self.pending_next_turn_messages.lock().unwrap().remove(index);
        }
        self.schedule_session_input_pump();
    }

    /// `_deferRlmTerminalNotice`.
    async fn defer_rlm_terminal_notice(self: &Arc<Self>, message: CustomMessage) -> Result<(), String> {
        self.assert_rlm_terminal_notice(&message)?;
        if self.disposed.load(Ordering::SeqCst) || self.disposing.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.pending_next_turn_messages
            .lock()
            .unwrap()
            .push(clone_custom_message(&message));
        self.flush_deferred_rlm_terminal_notices();
        Ok(())
    }

    /// `_demoteRlmTerminalNoticeActions`.
    fn demote_rlm_terminal_notice_actions(self: &Arc<Self>) {
        let actions: Vec<QueuedSessionAction> = self
            .action_store
            .lock()
            .unwrap()
            .clearable_actions(None)
            .into_iter()
            .filter(|action| {
                self.durable_rlm_terminal_notice_action_ids
                    .lock()
                    .unwrap()
                    .contains(&action.id)
            })
            .collect();
        if actions.is_empty() {
            return;
        }
        for action in &actions {
            if !self.is_rlm_terminal_notice_action(action) {
                continue;
            }
            if let Ok(record) = primary_delivery_record(action) {
                if let DeliveryMessage::Custom(custom) = record.message {
                    self.pending_next_turn_messages
                        .lock()
                        .unwrap()
                        .push(clone_custom_message(&custom));
                }
            }
        }
        let ids: HashSet<String> = actions.iter().map(|action| action.id.clone()).collect();
        self.cancel_session_actions(
            &|action| ids.contains(&action.id),
            "RLM child terminal notice deferred across session input suspension.",
            Some(actions),
        );
        for id in ids {
            self.durable_rlm_terminal_notice_action_ids
                .lock()
                .unwrap()
                .remove(&id);
        }
    }

    /// `_promptInjectedMessage`.
    async fn prompt_injected_message(
        self: &Arc<Self>,
        text: &str,
        message: CustomMessage,
        options: Option<PromptOptions>,
        execution_policy: Option<TurnExecutionPolicy>,
    ) -> Result<(), String> {
        let options = options.unwrap_or_default();
        if !self.is_streaming() && options.resume_if_idle == Some(true) {
            self.resume_session_input_admission();
        }
        let admission_epoch = self.session_input_pump_epoch.load(Ordering::SeqCst);
        let admission_fence = self
            .acquire_direct_turn_admission_fence(options.signal.as_ref())
            .await
            .map_err(|error| {
                if options.signal.as_ref().map(|signal| signal.is_cancelled()).unwrap_or(false) {
                    "Prompt admission was cancelled".to_string()
                } else {
                    error
                }
            })?;
        let report_preflight = once_preflight(options.preflight_result.clone());
        let result = async {
            if options.signal.as_ref().map(|signal| signal.is_cancelled()).unwrap_or(false) {
                return Err("Prompt admission was cancelled".to_string());
            }
            if admission_epoch != self.session_input_pump_epoch.load(Ordering::SeqCst) {
                return Err("Injected session input was invalidated before admission".to_string());
            }
            if let Some(committed) = &options.admission_committed {
                committed();
            }
            let queue_for_streaming = self.is_streaming();
            let queue_for_busy = options.queue_if_busy == Some(true) && self.is_busy_for_session_input("preflight");
            let visible_queued = queue_for_streaming || queue_for_busy;
            if visible_queued && options.streaming_behavior.is_none() {
                let state_description = if queue_for_streaming {
                    "Agent is already processing"
                } else {
                    "Agent has queued work"
                };
                return Err(format!(
                    "{state_description}. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
                ));
            }
            let schedule = options
                .streaming_behavior
                .clone()
                .unwrap_or_else(|| SESSION_INPUT_SCHEDULE_FOLLOW_UP.to_string());
            let prefix_messages = if visible_queued {
                Some(self.take_pending_next_turn_messages())
            } else {
                None
            };
            let execution_policy = match execution_policy {
                Some(policy) => policy,
                None => {
                    if visible_queued {
                        self.turn_execution_policy("queued", None)
                    } else {
                        self.turn_execution_policy("injected", None)
                    }
                }
            };
            let action = self.create_prepared_turn_action(
                &schedule,
                text,
                None,
                Some(PreparedTurnActionOptions {
                    custom_message: Some(message.clone()),
                    prefix_messages: prefix_messages.clone(),
                    queue_key: options.follow_up_queue_key.clone(),
                    preview_label: injected_message_preview_label(&message),
                    suppress_autonomous_continuation: options.suppress_autonomous_continuation,
                    resume_if_idle: Some(
                        !visible_queued
                            || options.resume_if_idle == Some(true)
                            || (options.queue_if_busy == Some(true)
                                && can_select_session_action(&self.runtime_activity())),
                    ),
                    source: Some(
                        options
                            .source
                            .map(|source| source.as_str().to_string())
                            .unwrap_or_else(|| "internal".to_string()),
                    ),
                    execution_policy: Some(execution_policy),
                    queue_visible: Some(visible_queued),
                    ..Default::default()
                }),
            );
            let admission = self.admit_session_input(action, !visible_queued);
            admission_fence.release();
            let (accepted, ticket, disposition) = match admission {
                Ok(admission) => admission,
                Err(error) => {
                    if let Some(prefix_messages) = prefix_messages {
                        let mut pending = self.pending_next_turn_messages.lock().unwrap();
                        for message in prefix_messages.into_iter().rev() {
                            pending.insert(0, message);
                        }
                    }
                    report_preflight(false, false);
                    return Err(error);
                }
            };
            if !accepted || ticket.is_none() {
                if let Some(prefix_messages) = prefix_messages {
                    let mut pending = self.pending_next_turn_messages.lock().unwrap();
                    for message in prefix_messages.into_iter().rev() {
                        pending.insert(0, message);
                    }
                }
                report_preflight(false, false);
                return Ok(());
            }
            if disposition == "queued" {
                report_preflight(true, true);
            } else {
                report_preflight(true, false);
            }
            if options.return_after_accepted == Some(true) {
                return Ok(());
            }
            if visible_queued {
                return Ok(());
            }
            Ok(())
        }
        .await;
        admission_fence.release();
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                report_preflight(false, false);
                Err(error)
            }
        }
    }

    /// `_prompt`.
    async fn prompt_internal(
        self: &Arc<Self>,
        text: &str,
        options: Option<PromptOptions>,
    ) -> Result<(), String> {
        self.prompt_internal_with(text, options.unwrap_or_default(), None, None)
            .await
    }

    async fn prompt_internal_with(
        self: &Arc<Self>,
        text: &str,
        options: PromptOptions,
        skip_pre_prompt_work: Option<bool>,
        return_after_accepted: Option<bool>,
    ) -> Result<(), String> {
        let resume_suspended_input = options.resume_if_idle != Some(false);
        if !self.is_streaming() {
            if resume_suspended_input {
                self.resume_session_input_admission();
            }
            self.assert_session_action_admission_available()?;
        }
        let admission_epoch = self.session_input_pump_epoch.load(Ordering::SeqCst);
        let commit_fence = if self.is_streaming() {
            None
        } else {
            Some(
                self.acquire_direct_turn_admission_fence(options.signal.as_ref())
                    .await?,
            )
        };
        let report_preflight = once_preflight(options.preflight_result.clone());
        let is_internal_prompt = options.internal_prompt == Some(true);
        let expand_prompt_templates = if is_internal_prompt {
            false
        } else {
            options.expand_prompt_templates.unwrap_or(true)
        };
        let skip_pre_prompt_work = skip_pre_prompt_work
            .or(options.internal_prompt.map(|_| false))
            .unwrap_or(false);
        let normalization_policy = SubmissionNormalizationPolicy {
            parse_session_commands: !is_internal_prompt && !skip_pre_prompt_work,
            extension_commands: if expand_prompt_templates {
                SUBMISSION_EXTENSION_COMMAND_POLICY_EXECUTE.to_string()
            } else {
                SUBMISSION_EXTENSION_COMMAND_POLICY_IGNORE.to_string()
            },
            input_source: if !is_internal_prompt && options.skip_input_handlers != Some(true) {
                Some(options.source.unwrap_or(crate::core::session_action_store::InputSource::Interactive))
            } else {
                None
            },
            expand_skills: expand_prompt_templates,
            expand_prompt_templates,
        };
        let normalized = match self
            .normalize_submission(text, options.images.clone(), &normalization_policy)
            .await
        {
            Ok(normalized) => normalized,
            Err(error) => {
                report_preflight(false, false);
                return Err(error);
            }
        };
        if let Some(committed) = &options.admission_committed {
            committed();
        }
        match normalized {
            NormalizedSubmission::ExtensionCommand { completion } => {
                if let Some(fence) = commit_fence.as_ref() {
                    fence.release();
                }
                report_preflight(true, false);
                if return_after_accepted != Some(true) {
                    let _ = completion.await;
                }
                return Ok(());
            }
            NormalizedSubmission::Handled => {
                if let Some(fence) = commit_fence.as_ref() {
                    fence.release();
                }
                report_preflight(true, false);
                self.settle_agent_message(options.agent_message_id.as_deref(), "completion", None);
                return Ok(());
            }
            NormalizedSubmission::SessionCommand {
                text,
                images,
                command,
            } => {
                let pending_owned_work = !self.action_store.lock().unwrap().unfinished_actions(None).is_empty();
                let was_runtime_busy = self.is_streaming()
                    || self.is_compacting()
                    || self.is_retrying()
                    || self.is_bash_running();
                let was_busy = was_runtime_busy || pending_owned_work;
                let schedule = options.streaming_behavior.clone().unwrap_or_else(|| {
                    if self.is_streaming() {
                        SESSION_INPUT_SCHEDULE_STEER.to_string()
                    } else {
                        SESSION_INPUT_SCHEDULE_FOLLOW_UP.to_string()
                    }
                });
                let action = self.create_session_command_action(
                    &text,
                    command,
                    images,
                    &schedule,
                    options.agent_message_id.clone(),
                    Some(
                        if is_internal_prompt {
                            "internal".to_string()
                        } else {
                            options
                                .source
                                .map(|source| source.as_str().to_string())
                                .unwrap_or_else(|| "interactive".to_string())
                        },
                    ),
                );
                let admission = self.admit_session_input(
                    action,
                    !was_busy && self.can_start_session_action_immediately(),
                );
                if let Some(fence) = commit_fence.as_ref() {
                    fence.release();
                }
                let (accepted, ticket, disposition) = match admission {
                    Ok(admission) => admission,
                    Err(error) => {
                        report_preflight(false, false);
                        return Err(error);
                    }
                };
                report_preflight(accepted, disposition == "queued");
                if !accepted || ticket.is_none() {
                    return Ok(());
                }
                if return_after_accepted == Some(true) {
                    return Ok(());
                }
                if disposition == "queued" {
                    return Ok(());
                }
                self.wait_for_session_input_idle().await;
                return Ok(());
            }
            NormalizedSubmission::Prompt { text, images } => {
                let queue_for_streaming = self.is_streaming();
                let queue_for_busy =
                    options.queue_if_busy == Some(true) && self.is_busy_for_session_input("preflight");
                let visible_queued = queue_for_streaming || queue_for_busy;
                if visible_queued && options.streaming_behavior.is_none() {
                    if let Some(fence) = commit_fence.as_ref() {
                        fence.release();
                    }
                    report_preflight(false, false);
                    let state_description = if queue_for_streaming {
                        "Agent is already processing"
                    } else {
                        "Agent has queued work"
                    };
                    return Err(format!(
                        "{state_description}. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
                    ));
                }
                let schedule = options
                    .streaming_behavior
                    .clone()
                    .unwrap_or_else(|| SESSION_INPUT_SCHEDULE_FOLLOW_UP.to_string());
                let prefix_messages = if visible_queued {
                    Some(self.take_pending_next_turn_messages())
                } else {
                    None
                };
                let content = options
                    .content
                    .clone()
                    .unwrap_or_else(|| self.build_prompt_content(&text, images.as_deref()));
                let primary_message = match options.custom_message.clone() {
                    Some(message) => {
                        if visible_queued {
                            message
                        } else {
                            clone_custom_message(&message)
                        }
                    }
                    None => CustomMessage {
                        role: "user".to_string(),
                        custom_type: String::new(),
                        content: CustomMessageContent::Blocks(
                            content
                                .iter()
                                .map(|block| match block {
                                    pi_ai::types::ImageOrTextContent::Text(text) => {
                                        pi_agent_core::types::ContentBlock::Text(text.clone())
                                    }
                                    pi_ai::types::ImageOrTextContent::Image(image) => {
                                        pi_agent_core::types::ContentBlock::Image(image.clone())
                                    }
                                })
                                .collect(),
                        ),
                        display: false,
                        details: None,
                        timestamp: now_ms() as i64,
                    },
                };
                let accepted_agent_message =
                    skip_pre_prompt_work && return_after_accepted == Some(true);
                let execution_policy = if visible_queued {
                    self.turn_execution_policy("queued", None)
                } else {
                    self.turn_execution_policy(
                        "directPrompt",
                        Some(TurnExecutionPolicyOptions {
                            return_after_accepted,
                            skip_pre_prompt_work: Some(skip_pre_prompt_work),
                        }),
                    )
                };
                let action = self.create_prepared_turn_action(
                    &schedule,
                    &text,
                    images,
                    Some(PreparedTurnActionOptions {
                        agent_message_id: options.agent_message_id.clone(),
                        queue_key: options.follow_up_queue_key.clone(),
                        content: Some(content),
                        custom_message: Some(primary_message),
                        prefix_messages,
                        suppress_autonomous_continuation: options.suppress_autonomous_continuation,
                        resume_if_idle: Some(
                            !visible_queued
                                || options.resume_if_idle == Some(true)
                                || (options.queue_if_busy == Some(true)
                                    && can_select_session_action(&self.runtime_activity())),
                        ),
                        source: Some(if is_internal_prompt {
                            "internal".to_string()
                        } else {
                            options
                                .source
                                .map(|source| source.as_str().to_string())
                                .unwrap_or_else(|| "interactive".to_string())
                        }),
                        execution_policy: Some(execution_policy),
                        queue_visible: Some(visible_queued),
                        accepted_agent_message: Some(accepted_agent_message),
                        accepted_before_completion: Some(return_after_accepted == Some(true)),
                        ..Default::default()
                    }),
                );
                if action.suppress_autonomous_continuation == Some(true) {
                    if let Ok(record) = primary_delivery_record(&action) {
                        if let DeliveryMessage::Custom(custom) = record.message {
                            self.mark_autonomous_continuation_suppressed(&AgentMessage::Custom(
                                CustomAgentMessage::Custom {
                                    custom_type: custom.custom_type.clone(),
                                    content: custom.content.clone(),
                                    display: custom.display,
                                    details: custom.details.clone(),
                                    timestamp: custom.timestamp,
                                },
                            ));
                        }
                    }
                }
                let admission = self.admit_session_input(
                    action.clone(),
                    !visible_queued && self.can_start_session_action_immediately(),
                );
                if let Some(fence) = commit_fence.as_ref() {
                    fence.release();
                }
                let (accepted, ticket, disposition) = match admission {
                    Ok(admission) => admission,
                    Err(error) => {
                        report_preflight(false, false);
                        return Err(error);
                    }
                };
                if !accepted || ticket.is_none() {
                    report_preflight(false, false);
                    return Ok(());
                }
                if disposition == "queued" {
                    report_preflight(true, true);
                } else {
                    report_preflight(true, false);
                }
                if return_after_accepted == Some(true) {
                    return Ok(());
                }
                if visible_queued {
                    return Ok(());
                }
                self.wait_for_session_input_idle().await;
                Ok(())
            }
        }
    }

    /// `_executeExtensionCommand`.
    fn execute_extension_command(
        self: &Arc<Self>,
        text: &str,
    ) -> Option<BoxFuture<Result<(), String>>> {
        let parsed = parse_slash_command(text)?;
        let command_name = parsed.name.clone();
        let args = parsed.args.clone();

        let runner = self.extension_runner()?;
        let command = runner.get_command(&command_name)?;
        let session = self.clone();
        Some(Box::pin(async move {
            let handler = match &command.handler {
                Some(handler) => handler.clone(),
                None => return Ok(()),
            };
            match handler(args).await {
                Ok(()) => Ok(()),
                Err(error) => {
                    let _ = session;
                    Err(error)
                }
            }
        }))
    }

    /**
     * Expand skill commands (/skill:name args) to their full content.
     * Returns the expanded text, or the original text if not a skill command or skill not found.
     * Emits errors via extension runner if file read fails.
     */
    fn expand_skill_command(&self, text: &str) -> String {
        if !text.starts_with("/skill:") {
            return text.to_string();
        }

        let parsed = match parse_slash_command(text) {
            Some(parsed) => parsed,
            None => return text.to_string(),
        };
        if !parsed.name.starts_with("skill:") {
            return text.to_string();
        }
        let skill_name = parsed.name["skill:".len()..].to_string();
        let args = parsed.args.clone();

        let skills = self.resource_loader.get_skills().skills;
        let skill = match skills.iter().find(|skill| skill.name() == skill_name) {
            Some(skill) => skill.clone(),
            None => return text.to_string(), // Unknown skill, pass through
        };

        let file_path = skill_file_path(&skill);
        match std::fs::read_to_string(&file_path) {
            Ok(content) => {
                let body = crate::utils::frontmatter::strip_frontmatter(&content)
                    .unwrap_or(content)
                    .trim()
                    .to_string();
                let base_dir = skill_base_dir(&skill);
                let skill_block = format!(
                    "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
                    skill.name(), file_path, base_dir, body
                );
                if args.is_empty() {
                    skill_block
                } else {
                    format!("{skill_block}\n\n{args}")
                }
            }
            Err(error) => {
                if let Some(runner) = self.extension_runner() {
                    runner.emit_error(ExtensionError {
                        extension_path: file_path.clone(),
                        event: "skill_expansion".to_string(),
                        error: error.to_string(),
                        stack: None,
                    });
                }
                text.to_string() // Return original on error
            }
        }
    }

    /**
     * Queue a steering message while the agent is running.
     * Delivered after the current assistant turn finishes executing its tool calls,
     * before the next LLM call.
     * Expands skill commands and prompt templates. Errors on extension commands.
     * @param images Optional image attachments to include with the message
     * @throws Error if text is an extension command
     */
    pub async fn steer(
        self: &Arc<Self>,
        text: &str,
        images: Option<Vec<ImageContent>>,
        queue_key: Option<String>,
        agent_message_id: Option<String>,
        resume_if_idle: Option<bool>,
    ) -> Result<(), String> {
        let policy = SubmissionNormalizationPolicy {
            parse_session_commands: false,
            extension_commands: SUBMISSION_EXTENSION_COMMAND_POLICY_REJECT.to_string(),
            input_source: None,
            expand_skills: true,
            expand_prompt_templates: true,
        };
        let normalized = self
            .normalize_submission(text, images, &policy)
            .await?;
        let (text, images) = match normalized {
            NormalizedSubmission::Prompt { text, images } => (text, images),
            _ => return Err("Queued prompt normalization did not produce a prompt".to_string()),
        };

        self.queue_prepared_prompt(
            SESSION_INPUT_SCHEDULE_STEER,
            &text,
            images,
            Some(PreparedTurnActionOptions {
                queue_key,
                agent_message_id,
                resume_if_idle,
                ..Default::default()
            }),
        )
        .await?;
        Ok(())
    }

    /**
     * Queue a follow-up message to be processed after the agent finishes.
     * Delivered only when agent has no more tool calls or steering messages.
     * Expands skill commands and prompt templates. Errors on extension commands.
     * @param images Optional image attachments to include with the message
     * @throws Error if text is an extension command
     */
    pub async fn follow_up(
        self: &Arc<Self>,
        text: &str,
        images: Option<Vec<ImageContent>>,
        queue_key: Option<String>,
        agent_message_id: Option<String>,
        resume_if_idle: Option<bool>,
    ) -> Result<bool, String> {
        let policy = SubmissionNormalizationPolicy {
            parse_session_commands: false,
            extension_commands: SUBMISSION_EXTENSION_COMMAND_POLICY_REJECT.to_string(),
            input_source: None,
            expand_skills: true,
            expand_prompt_templates: true,
        };
        let normalized = self.normalize_submission(text, images, &policy).await?;
        let (text, images) = match normalized {
            NormalizedSubmission::Prompt { text, images } => (text, images),
            _ => return Err("Queued prompt normalization did not produce a prompt".to_string()),
        };

        self.queue_prepared_prompt(
            SESSION_INPUT_SCHEDULE_FOLLOW_UP,
            &text,
            images,
            Some(PreparedTurnActionOptions {
                queue_key,
                agent_message_id,
                resume_if_idle,
                ..Default::default()
            }),
        )
        .await
    }

    /// `restoreSessionActions`.
    pub async fn restore_session_actions(
        self: &Arc<Self>,
        snapshot: &SessionActionRecoverySnapshot,
    ) -> Result<usize, String> {
        if snapshot.format_version != SESSION_ACTION_RECOVERY_FORMAT_VERSION {
            return Err(format!(
                "Unsupported session action recovery format version: {}",
                snapshot.format_version
            ));
        }
        let mut restored = 0usize;
        for action in &snapshot.actions {
            let schedule = match action.payload {
                SessionActionRecoveryPayload::Turn { .. } => {
                    if action.delivery == DeliveryPolicy::NextTurnBoundary {
                        SESSION_INPUT_SCHEDULE_STEER
                    } else {
                        SESSION_INPUT_SCHEDULE_FOLLOW_UP
                    }
                }
                SessionActionRecoveryPayload::SessionCommand { .. } => {
                    if action.delivery == DeliveryPolicy::NextTurnBoundary {
                        SESSION_INPUT_SCHEDULE_STEER
                    } else {
                        SESSION_INPUT_SCHEDULE_FOLLOW_UP
                    }
                }
            };
            let restored_action = match &action.payload {
                SessionActionRecoveryPayload::Turn {
                    text,
                    preview,
                    records,
                    images,
                    content,
                    custom_message,
                    queue_visible,
                    accepted_agent_message,
                    accepted_before_completion,
                    ..
                } => {
                    let delivery_records: Vec<DeliveryRecord> = records
                        .iter()
                        .map(|record| DeliveryRecord {
                            id: record.id.clone(),
                            role: record.role,
                            message: delivery_message_from_value(&record.message),
                            started: false,
                            durable: false,
                            owner_action_id: record.owner_action_id.clone(),
                        })
                        .collect();
                    let custom_message = custom_message
                        .as_ref()
                        .and_then(|value| serde_json::from_value::<CustomMessage>(value.clone()).ok());
                    self.create_prepared_turn_action(
                        schedule,
                        text,
                        images.clone(),
                        Some(PreparedTurnActionOptions {
                            queue_key: action.queue_key.clone(),
                            agent_message_id: action.agent_message_id.clone(),
                            content: content.clone(),
                            custom_message,
                            records: Some(delivery_records),
                            suppress_autonomous_continuation: action.suppress_autonomous_continuation,
                            preview: preview.clone(),
                            resume_if_idle: Some(true),
                            queue_visible: Some(*queue_visible),
                            accepted_agent_message: Some(*accepted_agent_message),
                            accepted_before_completion: Some(*accepted_before_completion),
                            ..Default::default()
                        }),
                    )
                }
                SessionActionRecoveryPayload::SessionCommand {
                    text,
                    command,
                    images,
                } => self.create_session_command_action(
                    text,
                    command.clone(),
                    images.clone(),
                    schedule,
                    action.agent_message_id.clone(),
                    Some(action.source.as_str().to_string()),
                ),
            };
            let mut restored_action = restored_action;
            restored_action.wake = action.wake;
            restored_action.source = action.source;
            match self.admit_session_input(restored_action, false) {
                Ok(_) => restored += 1,
                Err(error) => return Err(error),
            }
        }
        Ok(restored)
    }

    /// `_restoreSessionCommand`.
    fn restore_session_command(self: &Arc<Self>, snapshot: &RestoredPromptInput, command: SessionSlashCommand) -> bool {
        let action = self.create_session_command_action(
            &snapshot.text,
            command,
            snapshot.images.clone(),
            SESSION_INPUT_SCHEDULE_FOLLOW_UP,
            snapshot.agent_message_id.clone(),
            Some("internal".to_string()),
        );
        self.admit_session_input(action, false).is_ok()
    }

    /// `_restorePromptInput`.
    async fn restore_prompt_input(
        self: &Arc<Self>,
        schedule: &str,
        snapshot: &RestoredPromptInput,
    ) -> Result<bool, String> {
        let action = self.create_prepared_turn_action(
            schedule,
            &snapshot.text,
            snapshot.images.clone(),
            Some(PreparedTurnActionOptions {
                queue_key: snapshot.queue_key.clone(),
                agent_message_id: snapshot.agent_message_id.clone(),
                content: snapshot.content.clone(),
                custom_message: snapshot.custom_message.clone(),
                prefix_messages: snapshot.prefix_messages.clone(),
                resume_if_idle: Some(true),
                ..Default::default()
            }),
        );
        match self.admit_session_input(action, false) {
            Ok(_) => Ok(true),
            Err(error) => Err(error),
        }
    }

    /// `restoreSteeringMessage`.
    pub async fn restore_steering_message(
        self: &Arc<Self>,
        snapshot: &RestoredPromptInput,
    ) -> Result<bool, String> {
        if let Some(command) = parse_session_slash_command(&snapshot.text) {
            return Ok(self.restore_session_command(snapshot, command));
        }
        self.restore_prompt_input(SESSION_INPUT_SCHEDULE_STEER, snapshot)
            .await
    }

    /// `restoreFollowUpMessage`.
    pub async fn restore_follow_up_message(
        self: &Arc<Self>,
        snapshot: &RestoredPromptInput,
    ) -> Result<bool, String> {
        if let Some(command) = parse_session_slash_command(&snapshot.text) {
            return Ok(self.restore_session_command(snapshot, command));
        }
        self.restore_prompt_input(SESSION_INPUT_SCHEDULE_FOLLOW_UP, snapshot)
            .await
    }

    /// `_buildPromptContent`.
    fn build_prompt_content(
        &self,
        text: &str,
        images: Option<&[ImageContent]>,
    ) -> Vec<pi_ai::types::ImageOrTextContent> {
        let mut content: Vec<pi_ai::types::ImageOrTextContent> =
            vec![pi_ai::types::ImageOrTextContent::Text(TextContent::new(text))];
        if let Some(images) = images {
            content.extend(
                images
                    .iter()
                    .cloned()
                    .map(pi_ai::types::ImageOrTextContent::Image),
            );
        }
        content
    }

    /// `_takePendingNextTurnMessages`.
    fn take_pending_next_turn_messages(&self) -> Vec<CustomMessage> {
        std::mem::take(&mut *self.pending_next_turn_messages.lock().unwrap())
    }

    /// `_deliveryPolicy`.
    fn delivery_policy(&self, schedule: &str) -> DeliveryPolicy {
        queued_message_lane_delivery_policy(if schedule == SESSION_INPUT_SCHEDULE_STEER {
            QueuedMessageLane::Steering
        } else {
            QueuedMessageLane::FollowUp
        })
    }

    /// `_createDeliveryRecord`.
    fn create_delivery_record(
        &self,
        role: DeliveryRecordRole,
        message: DeliveryMessage,
        owner_action_id: &str,
    ) -> DeliveryRecord {
        DeliveryRecord {
            id: uuid::Uuid::new_v4().to_string(),
            role,
            message,
            started: false,
            durable: false,
            owner_action_id: owner_action_id.to_string(),
        }
    }

    /// `_turnExecutionPolicy`.
    fn turn_execution_policy(
        &self,
        mode: &str,
        options: Option<TurnExecutionPolicyOptions>,
    ) -> TurnExecutionPolicy {
        let options = options.unwrap_or_default();
        match mode {
            "queued" => TurnExecutionPolicy {
                preparation: CommitPreparationPolicy {
                    initial_refine_barrier: REFINE_BARRIER_IF_IN_FLIGHT.to_string(),
                    flush_pending_bash_before_validation: true,
                    validate_model_and_auth: true,
                    await_pending_model_selection: true,
                    pre_turn_compaction: PRE_TURN_COMPACTION_BEFORE_MODEL_SELECTION.to_string(),
                    final_refine_barrier: REFINE_BARRIER_IF_IN_FLIGHT.to_string(),
                },
                run_before_agent_start: true,
                next_turn_context_timing: NEXT_TURN_CONTEXT_TIMING_COMMIT.to_string(),
                preserve_empty_extension_prompt: false,
                completion_includes_retry_chain: true,
            },
            "injected" => TurnExecutionPolicy {
                preparation: CommitPreparationPolicy {
                    initial_refine_barrier: REFINE_BARRIER_IF_IN_FLIGHT.to_string(),
                    flush_pending_bash_before_validation: true,
                    validate_model_and_auth: true,
                    await_pending_model_selection: true,
                    pre_turn_compaction: PRE_TURN_COMPACTION_BEFORE_MODEL_SELECTION.to_string(),
                    final_refine_barrier: REFINE_BARRIER_IF_IN_FLIGHT.to_string(),
                },
                run_before_agent_start: true,
                next_turn_context_timing: NEXT_TURN_CONTEXT_TIMING_COMMIT.to_string(),
                preserve_empty_extension_prompt: false,
                completion_includes_retry_chain: true,
            },
            _ => TurnExecutionPolicy {
                preparation: CommitPreparationPolicy {
                    initial_refine_barrier: REFINE_BARRIER_IF_IN_FLIGHT.to_string(),
                    flush_pending_bash_before_validation: true,
                    validate_model_and_auth: true,
                    await_pending_model_selection: true,
                    pre_turn_compaction: PRE_TURN_COMPACTION_BEFORE_MODEL_SELECTION.to_string(),
                    final_refine_barrier: REFINE_BARRIER_IF_IN_FLIGHT.to_string(),
                },
                run_before_agent_start: true,
                next_turn_context_timing: if options.return_after_accepted == Some(true) {
                    NEXT_TURN_CONTEXT_TIMING_PREPARATION.to_string()
                } else {
                    NEXT_TURN_CONTEXT_TIMING_COMMIT.to_string()
                },
                preserve_empty_extension_prompt: options.skip_pre_prompt_work == Some(true),
                completion_includes_retry_chain: true,
            },
        }
    }

    /// `_createPreparedTurnAction`.
    fn create_prepared_turn_action(
        self: &Arc<Self>,
        schedule: &str,
        text: &str,
        images: Option<Vec<ImageContent>>,
        options: Option<PreparedTurnActionOptions>,
    ) -> QueuedSessionAction {
        let options = options.unwrap_or_default();
        let id = uuid::Uuid::new_v4().to_string();
        let mut records: Vec<DeliveryRecord> = Vec::new();
        if let Some(prefix_messages) = &options.prefix_messages {
            for message in prefix_messages {
                records.push(self.create_delivery_record(
                    DeliveryRecordRole::Prefix,
                    DeliveryMessage::Custom(message.clone()),
                    &id,
                ));
            }
        }
        let primary_message = match &options.custom_message {
            Some(custom) => DeliveryMessage::Custom(custom.clone()),
            None => DeliveryMessage::Custom(CustomMessage {
                role: "user".to_string(),
                custom_type: String::new(),
                content: CustomMessageContent::Text(text.to_string()),
                display: false,
                details: None,
                timestamp: now_ms() as i64,
            }),
        };
        records.push(self.create_delivery_record(
            DeliveryRecordRole::Primary,
            primary_message,
            &id,
        ));
        let mut preview = options.preview.clone().unwrap_or_else(|| text.to_string());
        if let Some(label) = &options.preview_label {
            preview = format!("{label}: {preview}");
        }
        let payload = PreparedTurnPayload {
            base: SessionTurnPayload {
                records,
                text: text.to_string(),
                preview: Some(preview),
            },
            images,
            content: options.content.clone(),
            custom_message: options.custom_message.clone(),
            prepared: None,
            execution_policy: options
                .execution_policy
                .clone()
                .unwrap_or_else(|| self.turn_execution_policy("directPrompt", None)),
            queue_visible: options.queue_visible.unwrap_or(false),
            accepted_agent_message: options.accepted_agent_message.unwrap_or(false),
            accepted_before_completion: options.accepted_before_completion.unwrap_or(false),
            capture_run_messages: None,
            cancelled_dispatch_ended: None,
        };
        QueuedSessionAction {
            id: id.clone(),
            source: crate::core::session_action_store::ActionSource::Internal,
            delivery: self.delivery_policy(schedule),
            wake: WakePolicy::Immediate,
            payload: QueuedActionPayload::Turn(payload),
            lifecycle: ActionLifecycle::Queued,
            queue_key: options.queue_key.clone(),
            agent_message_id: options.agent_message_id.clone(),
            suppress_autonomous_continuation: options.suppress_autonomous_continuation,
        }
    }

    /// `_createSessionCommandAction`.
    fn create_session_command_action(
        &self,
        text: &str,
        command: SessionSlashCommand,
        images: Option<Vec<ImageContent>>,
        schedule: &str,
        agent_message_id: Option<String>,
        source: Option<String>,
    ) -> QueuedSessionAction {
        let _ = source;
        QueuedSessionAction {
            id: uuid::Uuid::new_v4().to_string(),
            source: crate::core::session_action_store::ActionSource::Internal,
            delivery: self.delivery_policy(schedule),
            wake: WakePolicy::Immediate,
            payload: QueuedActionPayload::SessionCommand(PreparedCommandPayload {
                base: SessionCommandPayload {
                    command,
                    text: text.to_string(),
                },
                images,
            }),
            lifecycle: ActionLifecycle::Queued,
            queue_key: None,
            agent_message_id,
            suppress_autonomous_continuation: None,
        }
    }

    /// `_assertSessionActionAdmissionAvailable`.
    fn assert_session_action_admission_available(&self) -> Result<(), String> {
        if !self.session_input_admission_pauses.lock().unwrap().is_empty() {
            return Err("Session input admission is paused".to_string());
        }
        if self.session_input_pump_suspended.load(Ordering::SeqCst)
            && !self.session_input_suspended_for_update_restart.load(Ordering::SeqCst)
        {
            return Err("Session input admission is paused".to_string());
        }
        Ok(())
    }

    /// `_admitSessionInput`.
    fn admit_session_input(
        self: &Arc<Self>,
        action: QueuedSessionAction,
        immediately_eligible: bool,
    ) -> Result<(bool, Option<Arc<crate::core::session_action_store::ActionTicketController>>, String), String> {
        self.assert_session_action_admission_available()?;
        let mut store = self.action_store.lock().unwrap();
        store.enqueue(action.clone())?;
        let ticket = store.ticket_for(&action).ok();
        drop(store);
        let disposition = if immediately_eligible {
            "starts_when_admitted"
        } else {
            "queued"
        };
        self.session_input_arrival_epoch.fetch_add(1, Ordering::SeqCst);
        self.notify_session_input_checkpoint_change();
        self.schedule_session_input_pump();
        self.emit_queue_update();
        Ok((true, ticket, disposition.to_string()))
    }

    /// `_runtimeActivity`.
    fn runtime_activity(&self) -> RuntimeActivity {
        RuntimeActivity {
            is_streaming: self.is_streaming(),
            is_compacting: self.is_compacting(),
            is_retrying: self.is_retrying(),
            is_bash_running: self.is_bash_running(),
            queued_work_paused: !self.queued_work_pauses.lock().unwrap().is_empty(),
            session_input_pump_suspended: self.session_input_pump_suspended.load(Ordering::SeqCst),
        }
    }

    /// `_hasSelectableSessionInput`.
    fn has_selectable_session_input(&self) -> bool {
        !self.action_store.lock().unwrap().unfinished_actions(None).is_empty()
    }

    /// `get hasPendingSessionWork`.
    pub fn has_pending_session_work(&self) -> bool {
        self.has_selectable_session_input() || self.is_streaming()
    }

    /// `get hasPendingAdmissionWaiters`.
    pub fn has_pending_admission_waiters(&self) -> bool {
        self.pending_session_action_fence_waiters.load(Ordering::SeqCst) > 0
    }

    /// `_scheduleSessionInputPump`.
    fn schedule_session_input_pump(self: &Arc<Self>) {
        if self.session_input_pump_requested.swap(true, Ordering::SeqCst) {
            return;
        }
        let session = self.clone();
        tokio::spawn(async move {
            session.pump_session_inputs().await;
        });
    }

    /// `_pumpSessionInputs(epoch)`.
    async fn pump_session_inputs(self: &Arc<Self>, epoch: u64) {
        let mut blocked = false;
        'pump: loop {
            if self.disposed.load(Ordering::SeqCst)
                || self.disposing.load(Ordering::SeqCst)
                || !self.has_selectable_session_input()
            {
                break 'pump;
            }
            let _ = self.agent.wait_for_idle().await;
            let preselected = self
                .action_store
                .lock()
                .unwrap()
                .active_actions(None)
                .into_iter()
                .find(|action| action.lifecycle.state() == ActionLifecycleState::Selected);
            if epoch != self.session_input_pump_epoch.load(Ordering::SeqCst) {
                if let Some(preselected) = preselected {
                    let mut store = self.action_store.lock().unwrap();
                    let mut action = preselected.clone();
                    let _ = store.rollback(&mut action, None);
                    drop(store);
                    self.notify_session_input_checkpoint_change();
                    self.emit_queue_update();
                }
                break 'pump;
            }
            if !self.has_cancelled_dispatch_capture() {
                self.await_agent_event_queue().await;
            }
            let preselected_is_command = preselected
                .as_ref()
                .map(|action| matches!(action.payload, QueuedActionPayload::SessionCommand(_)))
                .unwrap_or(false);
            if preselected.is_none() || preselected_is_command {
                self.wait_for_refine_idle().await;
            }
            let activity = self.runtime_activity();
            let can_select_preselected_turn = preselected
                .as_ref()
                .map(|action| {
                    matches!(action.payload, QueuedActionPayload::Turn(_))
                        && can_select_session_action(&RuntimeActivity {
                            lower_agent_run: activity.lower_agent_run,
                            compaction: activity.compaction,
                            retry: activity.retry,
                            bash: activity.bash,
                            // `{ ...activity, refinementApply: false }`
                            refinement_apply: false,
                            branch_mutation: activity.branch_mutation,
                            scheduler_pause_count: activity.scheduler_pause_count,
                            disposing: activity.disposing,
                        })
                })
                .unwrap_or(false);
            if self.is_session_input_handoff_deferred(epoch)
                || (!can_select_preselected_turn && !can_select_session_action(&activity))
            {
                blocked = true;
                self.notify_session_input_checkpoint_change();
                break 'pump;
            }
            let first = match preselected.clone() {
                Some(preselected) => Some(preselected),
                None => match self.action_store.lock().unwrap().select_first() {
                    Ok(selected) => selected,
                    Err(_) => None,
                },
            };
            let first = match first {
                Some(first) => first,
                None => break 'pump,
            };
            if matches!(first.payload, QueuedActionPayload::SessionCommand(_)) {
                self.execute_selected_session_command(&first, epoch).await;
                break 'pump;
            }
            let mode = if first.delivery == DeliveryPolicy::NextTurnBoundary {
                self.steering_mode()
            } else {
                self.follow_up_mode()
            };
            let mut actions: Vec<QueuedSessionAction> = vec![first.clone()];
            while preselected.is_none() && mode == "all" {
                let next = self
                    .action_store
                    .lock()
                    .unwrap()
                    .queued_actions(Some(first.delivery))
                    .into_iter()
                    .next();
                let next = match next {
                    Some(next) => next,
                    None => break,
                };
                let (QueuedActionPayload::Turn(first_turn), QueuedActionPayload::Turn(next_turn)) =
                    (&first.payload, &next.payload)
                else {
                    break;
                };
                if !turn_execution_policies_equal(&first_turn.execution_policy, &next_turn.execution_policy)
                {
                    break;
                }
                let _ = self.action_store.lock().unwrap().select_first();
                actions.push(next);
            }
            if epoch != self.session_input_pump_epoch.load(Ordering::SeqCst) {
                let mut store = self.action_store.lock().unwrap();
                for action in actions.iter() {
                    let mut candidate = action.clone();
                    let _ = store.rollback(&mut candidate, None);
                }
                drop(store);
                break 'pump;
            }
            {
                let mut store = self.action_store.lock().unwrap();
                for action in actions.iter() {
                    let mut next = action.clone();
                    let _ = transition_session_action(
                        &mut next,
                        ActionLifecycle::Preparing { preparation: None },
                        &TransitionOptions::default(),
                    );
                    let _ = store.update_action(&next);
                }
            }
            self.notify_session_input_checkpoint_change();
            self.emit_queue_update();
            let start_result = self.start_prepared_turn_actions(&actions, epoch).await;
            match start_result {
                Ok(()) => {
                    for action in actions.iter() {
                        let current = self.action_state_of(&action.id);
                        if current == Some("committing".to_string()) {
                            let durable = primary_delivery_record(action)
                                .map(|record| self.messages().contains(&agent_message_from_delivery(&record.message)))
                                .unwrap_or(false);
                            if durable {
                                self.mark_delivery_record_durable(action, &current_messages_of(self));
                                let mut next = action.clone();
                                let _ = transition_session_action(
                                    &mut next,
                                    ActionLifecycle::Running {
                                        execution: ActionExecutionAlias::AgentTurn,
                                    },
                                    &TransitionOptions::default(),
                                );
                                let _ = self.action_store.lock().unwrap().update_action(&next);
                            }
                        }
                        if self.action_state_of(&action.id) == Some("running".to_string()) {
                            let mut next = action.clone();
                            let _ = transition_session_action(
                                &mut next,
                                ActionLifecycle::Completed,
                                &TransitionOptions::default(),
                            );
                            let _ = self.action_store.lock().unwrap().update_action(&next);
                            if let Ok(ticket) = self.action_store.lock().unwrap().ticket_for(action) {
                                ticket.settle_completed(None);
                            }
                            self.settle_agent_message(action.agent_message_id.as_deref(), "completion", None);
                        }
                    }
                }
                Err(error) => {
                    let transcript = self.messages();
                    let delivered: HashSet<String> =
                        transcript.iter().map(agent_message_key_of).collect();
                    let mut undelivered: Vec<QueuedSessionAction> = Vec::new();
                    for action in actions.iter() {
                        if !matches!(action.payload, QueuedActionPayload::Turn(_))
                            || action.lifecycle.state() == ActionLifecycleState::Cancelled
                        {
                            continue;
                        }
                        self.mark_matching_records_durable(action, &delivered);
                        self.filter_records_after_dispatch_failure(action);
                        let primary_durable = primary_delivery_record(action)
                            .map(|record| record.durable)
                            .unwrap_or(false);
                        if !primary_durable {
                            undelivered.push(action.clone());
                        }
                    }
                    if self.is_deferred_session_input_error(&error, epoch) {
                        for action in undelivered.iter() {
                            let state = self.action_state_of(&action.id);
                            match state {
                                Some(ActionLifecycleState::Committing) => {
                                    let mut store = self.action_store.lock().unwrap();
                                    let mut candidate = action.clone();
                                    let _ = store.rollback(
                                        &mut candidate,
                                        Some(RollbackProof {
                                            dispatch_settled: true,
                                            transcript: transcript.clone(),
                                        }),
                                    );
                                }
                                Some(ActionLifecycleState::Preparing)
                                | Some(ActionLifecycleState::Selected) => {
                                    let mut store = self.action_store.lock().unwrap();
                                    let mut candidate = action.clone();
                                    let _ = store.rollback(&mut candidate, None);
                                }
                                _ => {}
                            }
                        }
                        if !undelivered.is_empty() {
                            self.emit_queue_update();
                        }
                        blocked = epoch != self.session_input_pump_epoch.load(Ordering::SeqCst)
                            || self.is_busy_for_session_input("pump");
                        if blocked {
                            break 'pump;
                        }
                        continue;
                    }
                    let terminal_error = self.as_error(&error);
                    for action in actions.iter() {
                        if action.lifecycle.state() == ActionLifecycleState::Cancelled {
                            continue;
                        }
                        let state = self.action_state_of(&action.id);
                        if state != Some(ActionLifecycleState::Completed) && state != Some(ActionLifecycleState::Failed) {
                            let mut next = action.clone();
                            let _ = transition_session_action(
                                &mut next,
                                ActionLifecycle::Failed {
                                    error: terminal_error.clone(),
                                },
                                &TransitionOptions::default(),
                            );
                            let _ = self.action_store.lock().unwrap().update_action(&next);
                        }
                        let is_undelivered = undelivered.iter().any(|item| item.id == action.id);
                        if let Ok(ticket) = self.action_store.lock().unwrap().ticket_for(action) {
                            if is_undelivered {
                                ticket.reject_delivered(terminal_error.clone());
                                self.settle_agent_message(
                                    action.agent_message_id.as_deref(),
                                    "delivery",
                                    Some(&terminal_error),
                                );
                            }
                            self.settle_agent_message(
                                action.agent_message_id.as_deref(),
                                "completion",
                                Some(&terminal_error),
                            );
                            ticket.settle_completed(Some(terminal_error.clone()));
                        }
                    }
                    let surface = actions.iter().any(|action| match &action.payload {
                        QueuedActionPayload::Turn(turn) => turn.queue_visible,
                        QueuedActionPayload::SessionCommand(_) => true,
                    });
                    if surface {
                        self.surface_session_input_error(&error);
                    }
                }
            }
            for action in actions.iter() {
                let state = self.action_state_of(&action.id);
                let retained_cancelled_dispatch = state == Some(ActionLifecycleState::Cancelled)
                    && matches!(action.payload, QueuedActionPayload::Turn(_));
                if !retained_cancelled_dispatch
                    && (state == Some(ActionLifecycleState::Completed)
                        || state == Some(ActionLifecycleState::Failed)
                        || state == Some(ActionLifecycleState::Cancelled))
                {
                    self.durable_rlm_terminal_notice_action_ids
                        .lock()
                        .unwrap()
                        .remove(&action.id);
                    self.action_store.lock().unwrap().release_terminal(action);
                }
            }
            self.notify_session_input_checkpoint_change();
            self.emit_queue_update();
            if epoch != self.session_input_pump_epoch.load(Ordering::SeqCst) || blocked {
                break 'pump;
            }
        }
        if !blocked
            && epoch == self.session_input_pump_epoch.load(Ordering::SeqCst)
            && self.has_selectable_session_input()
        {
            self.schedule_session_input_pump();
        }
    }

    /// `_executeSelectedSessionCommand(action, epoch)`.
    async fn execute_selected_session_command(self: &Arc<Self>, action: &QueuedSessionAction, epoch: u64) {
        let input = match &action.payload {
            QueuedActionPayload::SessionCommand(command) => command.clone(),
            QueuedActionPayload::Turn(_) => {
                self.surface_session_input_error(&"Expected a selected session command".to_string());
                return;
            }
        };
        let commit_fence = self.acquire_session_action_commit_fence().await;
        if let Ok(commit_fence) = commit_fence {
            let action_id = action.id.clone();
            let session = self.clone();
            let input = input.clone();
            let work = async move {
                let is_cancelled = || {
                    session.action_state_of(&action_id) == Some(ActionLifecycleState::Cancelled)
                };
                if is_cancelled() {
                    return;
                }
                session.wait_for_refine_idle().await;
                if is_cancelled() {
                    return;
                }
                if session.is_session_input_handoff_deferred(epoch)
                    || !can_select_session_action(&session.runtime_activity())
                {
                    let mut store = session.action_store.lock().unwrap();
                    let mut candidate = action.clone();
                    let _ = store.rollback(&mut candidate, None);
                    drop(store);
                    session.notify_session_input_checkpoint_change();
                    session.emit_queue_update();
                    return;
                }
                {
                    let mut next = action.clone();
                    let _ = transition_session_action(
                        &mut next,
                        ActionLifecycle::Running {
                            execution: ActionExecutionAlias::SessionCommand,
                        },
                        &TransitionOptions::default(),
                    );
                    let _ = session.action_store.lock().unwrap().update_action(&next);
                }
                session.notify_session_input_checkpoint_change();
                session.emit_queue_update();
                let outcome = session.run_selected_session_command(action, &input).await;
                match outcome {
                    Ok(()) => {
                        {
                            let mut next = action.clone();
                            let _ = transition_session_action(
                                &mut next,
                                ActionLifecycle::Completed,
                                &TransitionOptions::default(),
                            );
                            let _ = session.action_store.lock().unwrap().update_action(&next);
                        }
                        if let Ok(ticket) = session.action_store.lock().unwrap().ticket_for(action) {
                            ticket.settle_completed(None);
                        }
                        session.settle_agent_message(action.agent_message_id.as_deref(), "completion", None);
                    }
                    Err(error) => {
                        let command_error = session.as_error(&error);
                        {
                            let mut next = action.clone();
                            let _ = transition_session_action(
                                &mut next,
                                ActionLifecycle::Failed {
                                    error: command_error.clone(),
                                },
                                &TransitionOptions::default(),
                            );
                            let _ = session.action_store.lock().unwrap().update_action(&next);
                        }
                        if let Ok(ticket) = session.action_store.lock().unwrap().ticket_for(action) {
                            ticket.reject_delivered(command_error.clone());
                            ticket.settle_completed(Some(command_error.clone()));
                        }
                        session.reject_agent_message(action.agent_message_id.as_deref(), &command_error);
                    }
                }
                session.action_store.lock().unwrap().release_terminal(action);
                session.notify_session_input_checkpoint_change();
                session.emit_queue_update();
            };
            work.await;
            commit_fence.release();
        }
    }

    /// The body of `_executeSelectedSessionCommand` that performs the durable
    /// append and the queued-command execution.
    async fn run_selected_session_command(
        self: &Arc<Self>,
        action: &QueuedSessionAction,
        input: &PreparedCommandPayload,
    ) -> Result<(), String> {
        self.append_durable_session_command_message(&input.base.text, &input.base.command, false, false, true);
        if let Ok(ticket) = self.action_store.lock().unwrap().ticket_for(action) {
            ticket.settle_delivered("not_applicable");
        }
        self.settle_agent_message(action.agent_message_id.as_deref(), "delivery", None);
        self.execute_queued_session_command(action).await
    }

    /// `_isBusyForSessionInput(point)`.
    fn is_busy_for_session_input(&self, point: &str) -> bool {
        let external_busy = self.is_compacting() || self.is_retrying() || self.is_bash_running();
        if point == "pump" {
            return external_busy
                || self.disposed.load(Ordering::SeqCst)
                || self.disposing.load(Ordering::SeqCst)
                || self.session_input_pump_suspended.load(Ordering::SeqCst)
                || !self.queued_work_pauses.lock().unwrap().is_empty()
                || self.branch_summary_operation.lock().unwrap().is_some();
        }
        external_busy || !self.action_store.lock().unwrap().unfinished_actions(None).is_empty()
    }

    /// `_isSessionInputHandoffDeferred(epoch)`.
    fn is_session_input_handoff_deferred(&self, epoch: u64) -> bool {
        epoch != self.session_input_pump_epoch.load(Ordering::SeqCst)
            || self.is_busy_for_session_input("pump")
    }

    /// `_asError(error)`.
    fn as_error(&self, error: &str) -> String {
        error.to_string()
    }

    /// `_isDeferredSessionInputError(error, epoch)`.
    fn is_deferred_session_input_error(&self, error: &str, epoch: u64) -> bool {
        if error == DEFERRED_SESSION_INPUT_ERROR_MESSAGE {
            return true;
        }
        if epoch != self.session_input_pump_epoch.load(Ordering::SeqCst) {
            return true;
        }
        if self.is_busy_for_session_input("pump") {
            self.surface_session_input_error(error);
            return true;
        }
        false
    }

    /// `_surfaceSessionInputError(error)`.
    fn surface_session_input_error(&self, error: &str) {
        let normalized = self.as_error(error);
        if let Some(runner) = self.extension_runner() {
            // Best-effort: a throwing error listener must not break the pump's requeue path.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runner.emit_error(ExtensionError {
                    extension_path: "<session-input>".to_string(),
                    event: "session_input".to_string(),
                    error: normalized,
                    stack: None,
                });
            }));
        }
    }

    /// `_startPreparedTurnActions(actions, epoch)`.
    async fn start_prepared_turn_actions(
        self: &Arc<Self>,
        actions: &[QueuedSessionAction],
        epoch: u64,
    ) -> Result<(), String> {
        let mut next_turn_messages: Vec<CustomMessage> = Vec::new();
        let active_turns = |session: &Arc<Self>| -> Vec<QueuedSessionAction> {
            actions
                .iter()
                .filter(|action| {
                    matches!(action.payload, QueuedActionPayload::Turn(_))
                        && session
                            .action_state_of(&action.id)
                            .as_deref()
                            == Some("preparing")
                })
                .cloned()
                .collect()
        };
        let first_turn = match active_turns(self).into_iter().next() {
            Some(first_turn) => first_turn,
            None => return Ok(()),
        };
        let execution_policy = match &first_turn.payload {
            QueuedActionPayload::Turn(turn) => turn.execution_policy.clone(),
            QueuedActionPayload::SessionCommand(_) => return Ok(()),
        };
        let park_next_turn_messages = |session: &Arc<Self>, messages: Vec<CustomMessage>| {
            let total = messages.len();
            let parked: Vec<CustomMessage> = messages
                .into_iter()
                .filter(|message| message.custom_type != HARNESS_DIGEST_CUSTOM_TYPE)
                .collect();
            if parked.len() != total {
                session.harness_digest_pending.store(true, Ordering::SeqCst);
            }
            let mut pending = session.pending_next_turn_messages.lock().unwrap();
            for (index, message) in parked.into_iter().enumerate() {
                pending.insert(index, message);
            }
        };
        let prepared = self
            .prepare_for_commit(
                session_preparation_policy_from(&execution_policy),
                Box::new({
                    let session = self.clone();
                    let actions = actions.to_vec();
                    let execution_policy = execution_policy.clone();
                    move |prepared_result: PreparedTurnActionState| {
                        Box::pin(async move {
                            let _ = &actions;
                            let _ = &execution_policy;
                            let _ = &session;
                            let _ = prepared_result;
                            Ok(())
                        })
                    }
                }),
            )
            .await;
        let prepared_ok = match prepared {
            Ok(value) => value,
            Err(error) => {
                park_next_turn_messages(self, next_turn_messages.clone());
                for action in actions.iter() {
                    self.strip_next_turn_records(&action.id);
                }
                return Err(error);
            }
        };
        if !prepared_ok {
            park_next_turn_messages(self, next_turn_messages.clone());
            return Ok(());
        }
        let turns = active_turns(self);
        if turns.is_empty() {
            park_next_turn_messages(self, next_turn_messages.clone());
            return Ok(());
        }
        let commit_fence = match self.acquire_session_action_commit_fence().await {
            Ok(commit_fence) => commit_fence,
            Err(error) => {
                park_next_turn_messages(self, next_turn_messages.clone());
                return Err(error);
            }
        };
        let prompt_result = self
            .run_session_action_commit(
                &turns,
                &execution_policy,
                epoch,
                &mut next_turn_messages,
            )
            .await;
        commit_fence.release();
        let prompt_result = match prompt_result {
            Ok(()) => prompt_result,
            Err(error) => Err(error),
        };
        if let Err(error) = prompt_result {
            let delivered: HashSet<String> = self.messages().iter().map(agent_message_key_of).collect();
            let remaining: Vec<CustomMessage> = next_turn_messages
                .iter()
                .filter(|message| !delivered.contains(&custom_message_key(message)))
                .cloned()
                .collect();
            park_next_turn_messages(self, remaining);
            for action in actions.iter() {
                self.strip_next_turn_records(&action.id);
            }
            return Err(error);
        }
        if execution_policy.completion_includes_retry_chain {
            self.wait_for_retry().await;
        }
        if !self.has_cancelled_dispatch_capture() {
            self.await_agent_event_queue().await;
        }
        let transcript = self.messages();
        let missing_durable = turns.iter().any(|action| {
            if self.action_state_of(&action.id) == Some(ActionLifecycleState::Cancelled) {
                return false;
            }
            match primary_delivery_record(action) {
                Ok(record) => {
                    if record.durable {
                        return false;
                    }
                    !transcript.contains(&agent_message_from_delivery(&record.message))
                }
                Err(_) => false,
            }
        });
        if missing_durable {
            return Err("Session input dispatch settled without durable delivery".to_string());
        }
        let primary_messages: Vec<AgentMessage> = turns
            .iter()
            .filter_map(|action| {
                primary_delivery_record(action)
                    .ok()
                    .map(|record| agent_message_from_delivery(&record.message))
            })
            .collect();
        self.forget_consumed_post_compaction_continuations(&primary_messages);
        Ok(())
    }

    /// The `_sessionActionCommitContext.run(...)` callback in `_startPreparedTurnActions`.
    async fn run_session_action_commit(
        self: &Arc<Self>,
        turns: &[QueuedSessionAction],
        execution_policy: &TurnExecutionPolicy,
        epoch: u64,
        next_turn_messages: &mut Vec<CustomMessage>,
    ) -> Result<(), String> {
        let is_deferred = self.is_session_input_handoff_deferred(epoch)
            || self.is_streaming()
            || turns.iter().any(|action| {
                self.action_state_of(&action.id) != Some(ActionLifecycleState::Preparing)
            });
        if is_deferred {
            return Err(DEFERRED_SESSION_INPUT_ERROR_MESSAGE.to_string());
        }
        if execution_policy.next_turn_context_timing == NEXT_TURN_CONTEXT_TIMING_COMMIT {
            *next_turn_messages = self.take_pending_next_turn_messages();
        }
        if self.harness_digest_pending.swap(false, Ordering::SeqCst) {
            // The first-turn digest rides the turn's delivery records so a
            // cancelled first turn strips it with the rest of the turn.
            let digest = self.harness_digest();
            if self.latest_context_harness_digest() != Some(digest.clone()) {
                next_turn_messages.insert(0, create_harness_digest_message(digest, now_ms_i64()));
            }
        }
        let first_id = turns[0].id.clone();
        let context_records: Vec<DeliveryRecord> = next_turn_messages
            .iter()
            .map(|message| {
                self.create_delivery_record(
                    DeliveryRecordRole::NextTurn,
                    DeliveryMessage::Custom(custom_message_value(message)),
                    &first_id,
                )
            })
            .collect();
        {
            let mut store = self.action_store.lock().unwrap();
            let first_primary_index = primary_delivery_record(&turns[0])
                .map(|record| {
                    match &turns[0].payload {
                        QueuedActionPayload::Turn(turn) => turn
                            .base
                            .records
                            .iter()
                            .position(|candidate| candidate.id == record.id)
                            .unwrap_or(0),
                        QueuedActionPayload::SessionCommand(_) => 0,
                    }
                })
                .unwrap_or(0);
            let mut next = turns[0].clone();
            if let QueuedActionPayload::Turn(turn) = &mut next.payload {
                for (offset, record) in context_records.into_iter().enumerate() {
                    turn.base.records.insert(first_primary_index + offset, record);
                }
            }
            let _ = store.update_action(&next);
        }
        let prepared_messages: Vec<AgentMessage> = turns
            .iter()
            .flat_map(|action| match &action.payload {
                QueuedActionPayload::Turn(turn) => turn
                    .base
                    .records
                    .iter()
                    .map(|record| agent_message_from_delivery(&record.message))
                    .collect::<Vec<_>>(),
                QueuedActionPayload::SessionCommand(_) => Vec::new(),
            })
            .collect();
        for action in turns.iter() {
            if action.suppress_autonomous_continuation.unwrap_or(false) {
                if let Ok(record) = primary_delivery_record(action) {
                    self.mark_autonomous_continuation_suppressed(&agent_message_from_delivery(&record.message));
                }
            }
        }
        if execution_policy.run_before_agent_start {
            self.append_before_agent_start_messages(&prepared_messages, None);
            self.apply_prepared_system_prompt(
                execution_policy.preserve_empty_extension_prompt,
            );
        } else if execution_policy.next_turn_context_timing != NEXT_TURN_CONTEXT_TIMING_SKIP {
            let mut state = self.agent.state();
            state.system_prompt = self.base_system_prompt.lock().unwrap().clone();
            self.agent.set_state(state);
        }
        {
            let mut store = self.action_store.lock().unwrap();
            for action in turns.iter() {
                let mut next = action.clone();
                let _ = transition_session_action(
                    &mut next,
                    ActionLifecycle::Committing,
                    &TransitionOptions::default(),
                );
                let _ = store.update_action(&next);
            }
        }
        self.notify_session_input_checkpoint_change();
        self.emit_queue_update();
        let suppress = turns
            .iter()
            .any(|action| action.suppress_autonomous_continuation.unwrap_or(false));
        if suppress {
            self.run_with_autonomous_continuation_suppressed(self.agent.prompt(prepared_messages))
                .await
        } else {
            self.agent.prompt(prepared_messages).await
        }
    }

    /// `_executeQueuedSessionCommand(action)`.
    async fn execute_queued_session_command(
        self: &Arc<Self>,
        action: &QueuedSessionAction,
    ) -> Result<(), String> {
        let input = match &action.payload {
            QueuedActionPayload::SessionCommand(command) => command.clone(),
            QueuedActionPayload::Turn(_) => {
                return Err("Expected a session command action".to_string());
            }
        };
        let mut result_text: Option<String> = None;
        let mut display_result = true;
        match input.base.command.name.as_str() {
            "compact" => {
                let args = if input.base.command.args.is_empty() {
                    None
                } else {
                    Some(input.base.command.args.clone())
                };
                self.compact_with_options(args.as_deref(), true).await?;
            }
            "refine" => {
                let result = match parse_refine_command_options(&input.base.command.args) {
                    Ok(options) => match self.refine_with_options(&options, true).await {
                        Ok(result) => result,
                        Err(error) => {
                            // Only a failure of the refinement itself is a refine failure; a later
                            // result-row persist error must not report a completed refinement as failed.
                            self.emit_refine_failed(&self.as_error(&error));
                            return Err(error);
                        }
                    },
                    Err(error) => return Err(error),
                };
                let applied = result
                    .applied_edits
                    .iter()
                    .filter(|edit| edit.applied)
                    .count();
                result_text = Some(format!(
                    "Refined continual harness state: {applied} edit{} applied.",
                    if applied == 1 { "" } else { "s" }
                ));
                display_result = false;
            }
            "goal" => {
                self.handle_goal_slash_command(&input.base.text, input.images.as_deref())
                    .await?;
                let goal = self.goal_state();
                result_text = Some(if !goal.objective.is_empty() {
                    format!("Goal {}: {}", goal_status_name(&goal.status), goal.objective)
                } else {
                    "No active goal.".to_string()
                });
            }
            "autonomous" => {
                self.handle_autonomous_slash_command(&input.base.text).await?;
            }
            _ => {}
        }
        if let Some(result_text) = result_text {
            self.append_durable_session_command_message(
                &result_text,
                &input.base.command,
                true,
                false,
                display_result,
            );
        }
        Ok(())
    }

    /// The `catch` arm of `_executeQueuedSessionCommand`.
    fn handle_queued_session_command_failure(
        &self,
        input: &PreparedCommandPayload,
        error: &str,
    ) -> Result<(), String> {
        if error == COMPACTION_SKIPPED_ERROR_MESSAGE {
            return Ok(());
        }
        let command_error = self.as_error(error);
        let appended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.append_durable_session_command_message(
                &format!("Command failed: {command_error}"),
                &input.base.command,
                true,
                true,
                true,
            );
        }));
        if appended.is_err() {
            // The result row is also the command-correlated UI settle edge.
            let message = create_session_slash_command_result_message(
                &format!("Command failed: {command_error}"),
                SessionSlashCommandResultDetails {
                    command: input.base.command.clone(),
                    success: false,
                    severity: "error".to_string(),
                    error: Some(command_error.clone()),
                },
                true,
                now_ms_i64(),
            );
            self.emit(AgentSessionEvent::MessageStart {
                message: message.clone(),
            });
            self.emit(AgentSessionEvent::MessageEnd { message });
        }
        Err(command_error)
    }

    /// `_appendDurableSessionCommandMessage(content, command, isResult, isError, display)`.
    fn append_durable_session_command_message(
        &self,
        content: &str,
        command: &SessionSlashCommand,
        is_result: bool,
        is_error: bool,
        display: bool,
    ) {
        let message = if is_result {
            create_session_slash_command_result_message(
                content,
                SessionSlashCommandResultDetails {
                    command: command.clone(),
                    success: !is_error,
                    severity: if is_error { "error" } else { "info" }.to_string(),
                    error: if is_error {
                        Some(
                            content
                                .strip_prefix("Command failed:")
                                .map(|rest| rest.trim_start().to_string())
                                .unwrap_or_else(|| content.to_string()),
                        )
                    } else {
                        None
                    },
                },
                display,
                now_ms_i64(),
            )
        } else {
            create_session_slash_command_message(command.clone(), now_ms_i64())
        };
        // Persist before touching live state so a failed write cannot leave an
        // unsaved leaf that the next entry would silently parent onto.
        self.session_manager
            .lock()
            .unwrap()
            .append_custom_message_entry_with_rollback(
                &message.custom_type,
                message.content.clone(),
                message.display,
                message.details.clone(),
            );
        let mut state = self.agent.state();
        state.messages.push(message.clone());
        self.agent.set_state(state);
        self.emit(AgentSessionEvent::MessageStart {
            message: message.clone(),
        });
        self.emit(AgentSessionEvent::MessageEnd { message });
    }

    /// `_throwIfExtensionCommand(text)`.
    fn throw_if_extension_command(&self, text: &str) -> Result<(), String> {
        let command_name = parse_slash_command(text)
            .map(|command| command.name)
            .unwrap_or_default();
        let command = self
            .extension_runner()
            .and_then(|runner| runner.get_command(&command_name));
        if command.is_some() {
            return Err(format!(
                "Extension command \"/{command_name}\" cannot be queued. Use prompt() or execute the command when not streaming."
            ));
        }
        Ok(())
    }

    /// `sendCustomMessage(message, options)`.
    pub async fn send_custom_message(
        self: &Arc<Self>,
        message: CustomMessage,
        trigger_turn: Option<bool>,
        deliver_as: Option<String>,
    ) -> Result<(), String> {
        let app_message = CustomMessage {
            custom_type: message.custom_type.clone(),
            content: message.content.clone(),
            display: message.display,
            details: message.details.clone(),
            timestamp: now_ms_i64(),
            ..Default::default()
        };
        if deliver_as.as_deref() == Some("nextTurn") {
            self.pending_next_turn_messages
                .lock()
                .unwrap()
                .push(app_message);
        } else if self.is_streaming() {
            let (text, images) = normalize_message_content(&message.content_as_parts());
            let schedule = if deliver_as.as_deref() == Some("followUp") {
                SESSION_INPUT_SCHEDULE_FOLLOW_UP
            } else {
                SESSION_INPUT_SCHEDULE_STEER
            };
            self.queue_prepared_prompt(
                schedule,
                &text,
                images,
                Some(PreparedTurnActionOptions {
                    custom_message: Some(app_message),
                    resume_if_idle: Some(true),
                    ..Default::default()
                }),
            )
            .await;
        } else if trigger_turn.unwrap_or(false) {
            if !self.session_input_suspended_for_update_restart.load(Ordering::SeqCst) {
                self.resume_session_input_admission();
            }
            let admission_fence = self.acquire_direct_turn_admission_fence().await?;
            let (text, images) = normalize_message_content(&message.content_as_parts());
            let immediately_eligible = self.can_start_session_action_immediately();
            let action = self.create_prepared_turn_action(
                SESSION_INPUT_SCHEDULE_FOLLOW_UP,
                &text,
                images,
                Some(PreparedTurnActionOptions {
                    custom_message: Some(app_message),
                    resume_if_idle: Some(true),
                    execution_policy: Some(self.turn_execution_policy("customTrigger", None)),
                    queue_visible: Some(false),
                    ..Default::default()
                }),
            );
            let result = self.admit_session_input(action, immediately_eligible);
            admission_fence.release();
            match result {
                Ok((_, ticket, _)) => {
                    if let Some(ticket) = ticket {
                        let _ = ticket.ticket.completed.await;
                    }
                }
                Err(error) => return Err(error),
            }
        } else {
            let mut state = self.agent.state();
            state.messages.push(AgentMessage::Custom(CustomAgentMessage::Custom {
                custom_type: message.custom_type.clone(),
                content: message.content.clone(),
                display: message.display,
                details: message.details.clone(),
                timestamp: app_message.timestamp,
            }));
            self.agent.set_state(state);
            self.session_manager
                .lock()
                .unwrap()
                .append_custom_message_entry(
                    &message.custom_type,
                    message.content.clone(),
                    message.display,
                    message.details.clone(),
                );
            let emitted = AgentMessage::Custom(CustomAgentMessage::Custom {
                custom_type: message.custom_type.clone(),
                content: message.content.clone(),
                display: message.display,
                details: message.details.clone(),
                timestamp: app_message.timestamp,
            });
            self.emit(AgentSessionEvent::MessageStart {
                message: emitted.clone(),
            });
            self.emit(AgentSessionEvent::MessageEnd { message: emitted });
        }
        Ok(())
    }

    /// `sendUserMessage(content, options)`.
    pub async fn send_user_message(
        self: &Arc<Self>,
        content: &pi_ai::types::ImageOrTextContent,
        deliver_as: Option<String>,
    ) -> Result<(), String> {
        let mut text_parts: Vec<String> = Vec::new();
        let mut images: Vec<ImageContent> = Vec::new();
        let mut items: Vec<pi_ai::types::ImageOrTextContent> = Vec::new();
        items.push(content.clone());
        for part in items {
            match part {
                pi_ai::types::ImageOrTextContent::Text(text) => text_parts.push(text.text),
                pi_ai::types::ImageOrTextContent::Image(image) => images.push(image),
            }
        }
        let images = if images.is_empty() { None } else { Some(images) };
        self.prompt_with_options(
            &text_parts.join("\n"),
            PromptOptions {
                expand_prompt_templates: Some(false),
                streaming_behavior: deliver_as,
                images,
                source: Some("extension".to_string()),
                resume_if_idle: Some(true),
                ..Default::default()
            },
        )
        .await
    }

    /// `clearQueue()`.
    pub fn clear_queue(&self) -> ClearedQueue {
        let clearable: Vec<QueuedSessionAction> = self
            .action_store
            .lock()
            .unwrap()
            .clearable_actions(None)
            .into_iter()
            .filter(|action| match &action.payload {
                QueuedActionPayload::SessionCommand(_) => true,
                QueuedActionPayload::Turn(turn) => turn.queue_visible,
            })
            .collect();
        if clearable.iter().any(|action| {
            matches!(action.payload, QueuedActionPayload::Turn(_))
                && action.lifecycle.state() == ActionLifecycleState::Preparing
        }) {
            self.session_input_pump_epoch.fetch_add(1, Ordering::SeqCst);
        }
        let steering: Vec<String> = clearable
            .iter()
            .filter(|action| action.delivery == DeliveryPolicy::NextTurnBoundary)
            .map(|action| action_text(action))
            .collect();
        let follow_up: Vec<String> = clearable
            .iter()
            .filter(|action| action.delivery == DeliveryPolicy::WhenRunIdle)
            .map(|action| action_text(action))
            .collect();
        let prompt_error = "Queued prompt was cleared before delivery.".to_string();
        let agent_message_error = "Queued agent message was cleared before delivery.".to_string();
        for action in clearable.iter() {
            let error = if matches!(action.payload, QueuedActionPayload::Turn(_))
                && action.lifecycle.state() == ActionLifecycleState::Preparing
            {
                prompt_error.clone()
            } else {
                agent_message_error.clone()
            };
            self.settle_agent_message(action.agent_message_id.as_deref(), "delivery", Some(&error));
            self.settle_agent_message(action.agent_message_id.as_deref(), "completion", Some(&error));
        }
        let clearable_ids: HashSet<String> = clearable.iter().map(|action| action.id.clone()).collect();
        self.cancel_session_actions(
            &|action: &QueuedSessionAction| clearable_ids.contains(&action.id),
            &agent_message_error,
            None,
        );
        self.agent.clear_all_queues();
        self.emit_queue_update();
        ClearedQueue { steering, follow_up }
    }

    /// `_invalidateQueuedPromptPreparation()`.
    fn invalidate_queued_prompt_preparation(&self) {
        let clearable = self.action_store.lock().unwrap().clearable_actions(None);
        for action in clearable.iter() {
            if matches!(action.payload, QueuedActionPayload::Turn(_)) {
                let mut next = action.clone();
                if let QueuedActionPayload::Turn(turn) = &mut next.payload {
                    turn.prepared = None;
                }
                let _ = self.action_store.lock().unwrap().update_action(&next);
            }
        }
    }

    /// `clearQueuedAgentMessages()`.
    pub fn clear_queued_agent_messages(&self) -> ClearedQueue {
        self.agent_message_clear_epoch.fetch_add(1, Ordering::SeqCst);
        self.clear_queued_user_messages_matching(&|text: &str| is_agent_session_message_prompt(text))
    }

    /// `clearQueuedUserMessagesMatching(predicate)`.
    pub fn clear_queued_user_messages_matching(
        &self,
        predicate: &dyn Fn(&str) -> bool,
    ) -> ClearedQueue {
        let owned_actions = self.action_store.lock().unwrap().owned_actions();
        let dispatched_turn_count = owned_actions
            .iter()
            .filter(|action| {
                matches!(action.payload, QueuedActionPayload::Turn(_))
                    && (action.lifecycle.state() == ActionLifecycleState::Committing
                        || action.lifecycle.state() == ActionLifecycleState::Running)
            })
            .count();
        let matching: Vec<QueuedSessionAction> = owned_actions
            .iter()
            .filter(|action| {
                let QueuedActionPayload::Turn(turn) = &action.payload else {
                    return false;
                };
                if action.agent_message_id.is_none() {
                    return false;
                }
                if !predicate(&turn.base.text) {
                    return false;
                }
                let state = action.lifecycle.state();
                state == ActionLifecycleState::Queued
                    || state == ActionLifecycleState::Selected
                    || state == ActionLifecycleState::Preparing
                    || (state == ActionLifecycleState::Committing
                        && dispatched_turn_count == 1
                        && !primary_delivery_record(action)
                            .map(|record| record.started)
                            .unwrap_or(false))
            })
            .cloned()
            .collect();
        if matching.is_empty() {
            return ClearedQueue {
                steering: Vec::new(),
                follow_up: Vec::new(),
            };
        }
        let removed_texts = |delivery: DeliveryPolicy| -> Vec<String> {
            let mut texts: Vec<String> = matching
                .iter()
                .filter(|action| action.delivery == delivery && action.lifecycle.state() == ActionLifecycleState::Queued)
                .map(|action| action_text(action))
                .collect();
            texts.extend(
                matching
                    .iter()
                    .filter(|action| action.delivery == delivery && action.lifecycle.state() != ActionLifecycleState::Queued)
                    .map(|action| action_text(action)),
            );
            texts
        };
        let removed_steering = removed_texts(DeliveryPolicy::NextTurnBoundary);
        let removed_follow_up = removed_texts(DeliveryPolicy::WhenRunIdle);
        let accepted_error = "Accepted agent message was cleared before delivery.".to_string();
        let queued_error = "Queued agent message was cleared before delivery.".to_string();
        for action in matching.iter() {
            let error = match &action.payload {
                QueuedActionPayload::Turn(turn) if turn.accepted_agent_message => accepted_error.clone(),
                _ => queued_error.clone(),
            };
            self.reject_agent_message(action.agent_message_id.as_deref(), &error);
        }
        for (accepted, error) in [(true, accepted_error.clone()), (false, queued_error.clone())] {
            let ids: HashSet<String> = matching
                .iter()
                .filter(|action| match &action.payload {
                    QueuedActionPayload::Turn(turn) => turn.accepted_agent_message == accepted,
                    QueuedActionPayload::SessionCommand(_) => false,
                })
                .map(|action| action.id.clone())
                .collect();
            if !ids.is_empty() {
                self.cancel_session_actions(
                    &|action: &QueuedSessionAction| ids.contains(&action.id),
                    &error,
                    Some(matching.clone()),
                );
            }
        }
        let should_abort = matching.iter().any(|action| {
            action.lifecycle.state() == ActionLifecycleState::Cancelled
                && matches!(action.payload, QueuedActionPayload::Turn(_))
        });
        if should_abort {
            self.agent.abort();
        }
        self.emit_queue_update();
        ClearedQueue {
            steering: removed_steering,
            follow_up: removed_follow_up,
        }
    }

    /// `mutateQueuedMessage(lane, index, expectedText, mutation)`.
    pub fn mutate_queued_message(
        &self,
        lane: QueuedMessageLane,
        index: i64,
        expected_text: &str,
        mutation: &QueuedMessageMutation,
    ) -> QueuedMessageMutationStatus {
        let policy = queued_message_lane_delivery_policy(lane);
        let projection: Vec<QueuedSessionAction> = visible_session_action_projection(
            &self.action_store.lock().unwrap().queued_actions(Some(policy)),
        );
        let item = if index >= 0 {
            projection.get(index as usize).cloned()
        } else {
            None
        };
        let item = match item {
            Some(item) => item,
            None => return QueuedMessageMutationStatus::Rejected,
        };
        if queued_agent_message_preview(&item) != expected_text {
            return QueuedMessageMutationStatus::Rejected;
        }
        match mutation {
            QueuedMessageMutation::Delete => {
                let error = "Queued prompt was deleted before delivery.".to_string();
                self.reject_agent_message(item.agent_message_id.as_deref(), &error);
                self.cancel_session_actions(
                    &|candidate: &QueuedSessionAction| candidate.id == item.id,
                    &error,
                    None,
                );
                self.emit_queue_update();
                self.resume_queued_work();
                QueuedMessageMutationStatus::Applied
            }
            QueuedMessageMutation::Move { direction } => {
                let neighbor_index = index + i64::from(*direction);
                let neighbor = if neighbor_index >= 0 {
                    projection.get(neighbor_index as usize).cloned()
                } else {
                    None
                };
                let neighbor = match neighbor {
                    Some(neighbor) => neighbor,
                    None => return QueuedMessageMutationStatus::Rejected,
                };
                let _ = self
                    .action_store
                    .lock()
                    .unwrap()
                    .swap_queued(&item, &neighbor);
                self.emit_queue_update();
                QueuedMessageMutationStatus::Applied
            }
            QueuedMessageMutation::Replace { text, images, lane } => {
                let blocked = match &item.payload {
                    QueuedActionPayload::Turn(turn) => {
                        turn.accepted_agent_message
                            || turn.base.records.iter().any(|record| {
                                record.role == DeliveryRecordRole::Primary
                                    && !matches!(record.message, DeliveryMessage::User(_))
                            })
                    }
                    QueuedActionPayload::SessionCommand(_) => false,
                };
                if blocked {
                    return QueuedMessageMutationStatus::Rejected;
                }
                let images = images.clone();
                let mut payload = item.payload.clone();
                match &mut payload {
                    QueuedActionPayload::SessionCommand(command) => {
                        let parsed = match parse_session_slash_command(text) {
                            Some(parsed) => parsed,
                            None => return QueuedMessageMutationStatus::Invalid,
                        };
                        command.base.text = text.clone();
                        command.base.command = parsed;
                        if images.is_some() {
                            command.images = if images.as_ref().map(|value| value.is_empty()).unwrap_or(true) {
                                None
                            } else {
                                images.clone()
                            };
                        }
                    }
                    QueuedActionPayload::Turn(turn) => {
                        turn.base.text = text.clone();
                        if let Some(images) = images.as_ref() {
                            turn.images = if images.is_empty() {
                                None
                            } else {
                                Some(images.clone())
                            };
                            let mut content: Vec<pi_ai::types::ImageOrTextContent> = vec![
                                pi_ai::types::ImageOrTextContent::Text(TextContent::new(text)),
                            ];
                            content.extend(
                                images
                                    .iter()
                                    .cloned()
                                    .map(pi_ai::types::ImageOrTextContent::Image),
                            );
                            turn.content = Some(content);
                        } else if let Some(content) = turn.content.clone() {
                            let mut next: Vec<pi_ai::types::ImageOrTextContent> = vec![
                                pi_ai::types::ImageOrTextContent::Text(TextContent::new(text)),
                            ];
                            next.extend(content.into_iter().filter(|block| {
                                !matches!(block, pi_ai::types::ImageOrTextContent::Text(_))
                            }));
                            turn.content = Some(next);
                        }
                        turn.base.preview = None;
                        turn.prepared = None;
                        for record in turn.base.records.iter_mut() {
                            if record.role == DeliveryRecordRole::Primary {
                                if let DeliveryMessage::User(user) = &mut record.message {
                                    user.content = match turn.content.clone() {
                                        Some(content) => UserContent::Blocks(content),
                                        None => UserContent::Text(text.clone()),
                                    };
                                }
                            }
                        }
                    }
                }
                let target_policy = queued_message_lane_delivery_policy(lane.clone());
                let mut moved = item.clone();
                moved.payload = payload;
                if target_policy != policy {
                    moved.queue_key = None;
                    moved.wake = if lane == &QueuedMessageLane::Steering {
                        WakePolicy::OnLowerBoundary
                    } else {
                        WakePolicy::ExternalResume
                    };
                    let target_index = self
                        .action_store
                        .lock()
                        .unwrap()
                        .queued_actions(Some(target_policy))
                        .len();
                    let _ = self
                        .action_store
                        .lock()
                        .unwrap()
                        .move_queued(&mut moved, target_policy, target_index);
                } else {
                    let _ = self
                        .action_store
                        .lock()
                        .unwrap()
                        .update_action(&moved);
                }
                self.resume_queued_work();
                self.emit_queue_update();
                QueuedMessageMutationStatus::Applied
            }
        }
    }

    /// `get queuedActionCount()`.
    pub fn queued_action_count(&self) -> i64 {
        visible_session_action_projection(&self.action_store.lock().unwrap().queued_actions(None)).len()
            as i64
    }

    /// `get unfinishedActionCount()`.
    pub fn unfinished_action_count(&self) -> i64 {
        self.action_store
            .lock()
            .unwrap()
            .unfinished_actions(None)
            .len() as i64
    }

    /// `get isQueuedWorkSuspended()`.
    pub fn is_queued_work_suspended(&self) -> bool {
        !self.queued_work_pauses.lock().unwrap().is_empty()
    }

    /// `get isSessionActive()`.
    pub fn is_session_active(&self) -> bool {
        self.is_streaming()
            || self.is_compacting()
            || self.is_retrying()
            || !self.action_store.lock().unwrap().unfinished_actions(None).is_empty()
            || !self.session_input_admission_pauses.lock().unwrap().is_empty()
    }

    /// `getSessionActionSnapshot()`.
    pub fn get_session_action_snapshot(&self) -> SessionActionSnapshot {
        let actions = self.action_store.lock().unwrap().snapshot_actions();
        let steering: Vec<String> = self
            .action_store
            .lock()
            .unwrap()
            .queued_actions(Some(DeliveryPolicy::NextTurnBoundary))
            .iter()
            .map(queued_agent_message_preview)
            .collect();
        let follow_ups: Vec<String> = self
            .action_store
            .lock()
            .unwrap()
            .queued_actions(Some(DeliveryPolicy::WhenRunIdle))
            .iter()
            .map(queued_agent_message_preview)
            .collect();
        let active = actions
            .iter()
            .find(|action| {
                !matches!(
                    action.lifecycle.state(),
                    "queued" | "completed" | "failed" | "cancelled"
                )
            })
            .map(|action| {
                let kind = match &action.payload {
                    QueuedActionPayload::Turn(_) => SessionActionSnapshotKind::Turn,
                    QueuedActionPayload::SessionCommand(_) => SessionActionSnapshotKind::SessionCommand,
                };
                let phase = match action.lifecycle.state() {
                    "preparing" => SessionActionPhase::Preparing,
                    "committing" => SessionActionPhase::Committing,
                    _ => SessionActionPhase::Running,
                };
                SessionActionSnapshotActive {
                    kind,
                    phase,
                    label: Some(queued_agent_message_preview(action)),
                }
            });
        SessionActionSnapshot {
            queued_count: visible_session_action_projection(
                &self.action_store.lock().unwrap().queued_actions(None),
            )
            .len() as i64,
            steering,
            follow_ups,
            active,
        }
    }

    /// `getSteeringMessages()`.
    pub fn get_steering_messages(&self) -> Vec<String> {
        self.action_store
            .lock()
            .unwrap()
            .queue_preview(DeliveryPolicy::NextTurnBoundary)
    }

    /// `getSteeringMessagePreviews()`.
    pub fn get_steering_message_previews(&self) -> Vec<String> {
        self.action_store
            .lock()
            .unwrap()
            .queued_actions(Some(DeliveryPolicy::NextTurnBoundary))
            .iter()
            .map(queued_agent_message_preview)
            .collect()
    }

    /// `getFollowUpMessages()`.
    pub fn get_follow_up_messages(&self) -> Vec<String> {
        self.action_store
            .lock()
            .unwrap()
            .queue_preview(DeliveryPolicy::WhenRunIdle)
    }

    /// `getFollowUpMessagePreviews()`.
    pub fn get_follow_up_message_previews(&self) -> Vec<String> {
        self.action_store
            .lock()
            .unwrap()
            .queued_actions(Some(DeliveryPolicy::WhenRunIdle))
            .iter()
            .map(queued_agent_message_preview)
            .collect()
    }

    /// `getSessionActionRecoverySnapshot()`.
    pub fn get_session_action_recovery_snapshot(&self) -> SessionActionRecoverySnapshot {
        let actions: Vec<SessionActionRecoveryAction> = self
            .action_store
            .lock()
            .unwrap()
            .unfinished_actions(None)
            .iter()
            .filter_map(|action| session_action_recovery_of(action))
            .collect();
        SessionActionRecoverySnapshot {
            version: SESSION_ACTION_RECOVERY_FORMAT_VERSION,
            actions,
        }
    }

    /// `_notifySessionInputCheckpointChange()`.
    fn notify_session_input_checkpoint_change(&self) {
        let waiters: Vec<Arc<dyn Fn() + Send + Sync>> = {
            let mut waiters = self.session_input_checkpoint_waiters.lock().unwrap();
            std::mem::take(&mut *waiters)
        };
        for waiter in waiters {
            waiter();
        }
    }

    /// `_waitForSessionActivityChange(signal)`.
    async fn wait_for_session_activity_change(&self, signal: Option<&CancellationToken>) -> Result<(), String> {
        let token = self.session_action_commit_dispose_abort.clone();
        let signal = signal.cloned();
        tokio::select! {
            _ = self.session_action_activity_notify.notified() => Ok(()),
            _ = token.cancelled() => Ok(()),
            _ = async {
                match signal {
                    Some(signal) => signal.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            } => Ok(()),
        }
    }

    /// `_observeSessionActionDeferral(action)`.
    fn observe_session_action_deferral(
        &self,
        action: &QueuedSessionAction,
    ) -> (Option<u64>, Option<u64>) {
        if action.lifecycle.state() == ActionLifecycleState::Cancelled {
            return (None, None);
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.observed_action_deferrals
            .lock()
            .unwrap()
            .insert(action.id.clone(), id.clone());
        (Some(action.lifecycle.state() as u64), Some(0))
    }

    /// `waitForSessionInputCheckpoint(signal?)`.
    pub async fn wait_for_session_input_checkpoint(
        self: &Arc<Self>,
        signal: Option<CancellationToken>,
    ) -> Result<(), String> {
        if !self.has_pending_admission_waiters() {
            return Ok(());
        }
        let notify = Arc::clone(&self.session_action_activity_notify);
        loop {
            if !self.has_pending_admission_waiters() {
                return Ok(());
            }
            let aborted = signal
                .as_ref()
                .map(|signal| signal.is_cancelled())
                .unwrap_or(false);
            if aborted {
                return Ok(());
            }
            tokio::select! {
                _ = notify.notified() => {}
                _ = self.session_action_commit_dispose_abort.cancelled() => return Ok(()),
                _ = async {
                    match signal.clone() {
                        Some(signal) => signal.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                } => return Ok(()),
            }
        }
    }

    /// `acquireSessionInputPause()`.
    pub fn acquire_session_input_pause(self: &Arc<Self>) -> SessionInputPause {
        let id = uuid::Uuid::new_v4().to_string();
        self.session_input_admission_pauses
            .lock()
            .unwrap()
            .insert(id.clone());
        let session = self.clone();
        SessionInputPause {
            release: Some(Arc::new(move || {
                session
                    .session_input_admission_pauses
                    .lock()
                    .unwrap()
                    .remove(&id);
                session.resume_session_input_admission();
            })),
        }
    }

    /// `acquireQueuedWorkPause()`.
    pub fn acquire_queued_work_pause(self: &Arc<Self>) -> QueuedWorkPause {
        let id = uuid::Uuid::new_v4().to_string();
        self.queued_work_pauses.lock().unwrap().insert(id.clone());
        let session = self.clone();
        QueuedWorkPause {
            release: Some(Arc::new(move || {
                session.queued_work_pauses.lock().unwrap().remove(&id);
                session.resume_queued_work();
            })),
        }
    }

    /// `_acquireDirectTurnAdmissionFence(signal?)`.
    async fn acquire_direct_turn_admission_fence(
        self: &Arc<Self>,
    ) -> Result<CommitFence, String> {
        self.acquire_commit_fence(false).await
    }

    /// `_acquireSessionActionCommitFence(signal?)`.
    async fn acquire_session_action_commit_fence(self: &Arc<Self>) -> Result<CommitFence, String> {
        self.acquire_commit_fence(true).await
    }

    /// The shared body of both commit fences.
    async fn acquire_commit_fence(self: &Arc<Self>, owner_id: bool) -> Result<CommitFence, String> {
        self.pending_session_action_fence_waiters
            .fetch_add(1, Ordering::SeqCst);
        let previous = self.session_action_commit_tail.lock().unwrap().clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let tail: BoxFuture<Result<(), String>> = Box::pin(async move {
            let _ = rx.await;
            Ok(())
        });
        *self.session_action_commit_tail.lock().unwrap() = tail;
        let owner = uuid::Uuid::new_v4().to_string();
        *self.session_action_commit_owner.lock().unwrap() = Some(owner.clone());
        let result = previous.await;
        self.pending_session_action_fence_waiters
            .fetch_sub(1, Ordering::SeqCst);
        self.notify_session_input_checkpoint_change();
        result?;
        let session = self.clone();
        let _ = owner_id;
        Ok(CommitFence {
            owner: Some(owner.clone()),
            release: Some(Arc::new(move || {
                if session
                    .session_action_commit_owner
                    .lock()
                    .unwrap()
                    .as_deref()
                    == Some(owner.as_str())
                {
                    *session.session_action_commit_owner.lock().unwrap() = None;
                }
                let _ = tx.send(());
                session.notify_session_input_checkpoint_change();
            })),
        })
    }

    /// `_resumeSessionInputAdmission()`.
    fn resume_session_input_admission(&self) {
        self.session_input_pump_suspended.store(false, Ordering::SeqCst);
        self.session_input_suspended_for_update_restart
            .store(false, Ordering::SeqCst);
        self.schedule_session_input_pump();
    }

    /// `resumeQueuedWork()`.
    pub fn resume_queued_work(&self) {
        self.session_action_activity_notify.notify_waiters();
        self.schedule_session_input_pump();
    }

    /// `waitForSessionInputIdle()`.
    pub async fn wait_for_session_input_idle(self: &Arc<Self>) -> Result<(), String> {
        loop {
            if !self.has_pending_session_work() {
                return Ok(());
            }
            if self.is_streaming() {
                let _ = self.agent.wait_for_idle().await;
                continue;
            }
            if self.action_store.lock().unwrap().unfinished_actions(None).is_empty() {
                return Ok(());
            }
            self.schedule_session_input_pump();
            let notify = Arc::clone(&self.session_action_activity_notify);
            notify.notified().await;
        }
    }

    /// `waitForIdle()`.
    pub async fn wait_for_idle(self: &Arc<Self>) -> Result<(), String> {
        let _ = self.agent.wait_for_idle().await;
        self.wait_for_session_input_idle().await
    }

    /// `_forgetConsumedPostCompactionContinuations(continuationMessages)`.
    fn forget_consumed_post_compaction_continuations(&self, continuation_messages: &[AgentMessage]) {
        let consumed: HashSet<String> = continuation_messages.iter().map(agent_message_key_of).collect();
        self.post_compaction_continuation_messages
            .lock()
            .unwrap()
            .retain(|message| !consumed.contains(&agent_message_key_of(message)));
        self.scheduled_post_compaction_continuation_messages
            .lock()
            .unwrap()
            .retain(|message| !consumed.contains(&agent_message_key_of(message)));
    }

    /// `getPendingNextTurnMessageSnapshots()`.
    pub fn get_pending_next_turn_message_snapshots(&self) -> Vec<CustomMessage> {
        self.pending_next_turn_messages
            .lock()
            .unwrap()
            .iter()
            .map(clone_custom_message)
            .collect()
    }

    /// `restorePendingNextTurnMessages(messages)`.
    pub fn restore_pending_next_turn_messages(&self, messages: &[CustomMessage]) {
        let mut pending = self.pending_next_turn_messages.lock().unwrap();
        for (index, message) in messages.iter().enumerate() {
            pending.insert(index, clone_custom_message(message));
        }
    }

    /// `removeQueuedFollowUp(queueKey)`.
    pub fn remove_queued_follow_up(&self, queue_key: &str) -> bool {
        let mut store = self.action_store.lock().unwrap();
        let actions = store.queued_actions(Some(DeliveryPolicy::WhenRunIdle));
        let target: Vec<QueuedSessionAction> = actions
            .into_iter()
            .filter(|action| action.queue_key.as_deref() == Some(queue_key))
            .collect();
        if target.is_empty() {
            return false;
        }
        let ids: HashSet<String> = target.iter().map(|action| action.id.clone()).collect();
        let removed = store.remove(&|action: &QueuedSessionAction| ids.contains(&action.id), None);
        if removed.is_err() {
            return false;
        }
        drop(store);
        self.emit_queue_update();
        true
    }

    /// `get resourceLoader()`.
    pub fn resource_loader(&self) -> Arc<dyn ResourceLoader> {
        self.resource_loader.clone()
    }

    /// `requestAbort()`.
    pub fn request_abort(self: &Arc<Self>) {
        self.session_input_suspended_for_update_restart
            .store(true, Ordering::SeqCst);
        self.session_input_pump_suspended.store(true, Ordering::SeqCst);
        self.session_input_pump_epoch.fetch_add(1, Ordering::SeqCst);
        let error = "Session input was aborted.".to_string();
        self.reject_queued_agent_message_deliveries(&error, None);
        self.agent.abort();
        self.notify_session_input_checkpoint_change();
        self.emit_queue_update();
    }

    /// `abort()`.
    pub async fn abort(self: &Arc<Self>) -> Result<(), String> {
        self.request_abort();
        self.abort_compaction();
        self.abort_branch_summary();
        self.abort_retry();
        let _ = self.agent.wait_for_idle().await;
        self.wait_for_session_input_idle().await
    }

    /// `abortForUpdateRestart()`.
    pub fn abort_for_update_restart(self: &Arc<Self>) {
        self.request_abort();
        self.abort_compaction();
        self.abort_branch_summary();
        let controllers: Vec<CancellationToken> = self.bash_abort_controllers.lock().unwrap().clone();
        for controller in controllers {
            controller.cancel();
        }
    }

    /// `_emitModelSelect(nextModel, previousModel, source)`.
    async fn emit_model_select(self: &Arc<Self>, next: Model, previous: Option<Model>, source: &str) {
        if let Some(previous) = &previous {
            if models_are_equal(previous, &next) {
                return;
            }
        }
        if let Some(runner) = self.extension_runner() {
            let _ = runner
                .emit(ExtensionEvent::ModelSelect(ModelSelectPayload {
                    model: next,
                    previous_model: previous,
                    source: source.to_string(),
                }))
                .await;
        }
    }

    /// `_queueModelSelectEmit(emit)`.
    fn queue_model_select_emit(
        self: &Arc<Self>,
        emit: Arc<dyn Fn() -> BoxFuture<Result<(), String>> + Send + Sync>,
    ) {
        let previous = self.model_select_emit_queue.lock().unwrap().clone();
        self.model_select_emit_queue_idle.store(false, Ordering::SeqCst);
        let session = self.clone();
        let tail: BoxFuture<Result<(), String>> = Box::pin(async move {
            let _ = previous.await;
            let result = emit().await;
            session.model_select_emit_queue_idle.store(true, Ordering::SeqCst);
            result
        });
        *self.model_select_emit_queue.lock().unwrap() = tail;
    }

    /// `setModel(model, options)`.
    pub async fn set_model(
        self: &Arc<Self>,
        model: Model,
        options: ModelSelectOptions,
    ) -> Result<(), String> {
        let previous = self.agent.state().model;
        let mut state = self.agent.state();
        state.model = model.clone();
        state.thinking_level = clamp_thinking_level_for_model(
            &model,
            self.thinking_level(),
        );
        state.service_tier = self.clamp_service_tier_for_model(None);
        self.agent.set_state(state);
        self.restore_provider_context_for_model();
        if let Some(entry) = self.find_assistant_entry_for_message(&AgentMessage::Message(
            pi_ai::types::Message::Assistant(AssistantMessage::default()),
        )) {
            let _ = entry;
        }
        self.emit_extension_event("model_select");
        let emit_promise = self.emit_model_select(model.clone(), previous.clone(), "set");
        let _ = emit_promise;
        self.track_model_select_emit_error();
        if self.should_wait_for_model_select_emit(&options) {
            self.pending_model_select_emit().await;
        }
        self.emit(AgentSessionEvent::ModelSelect {
            model: model.id.clone(),
            previous_model: previous.id.clone(),
            reason: "set".to_string(),
        });
        Ok(())
    }

    /// `_trackModelSelectEmitError()`.
    fn track_model_select_emit_error(&self) {
        let queue = self.model_select_emit_queue.lock().unwrap().clone();
        tokio::spawn(async move {
            let _ = queue.await;
        });
    }

    /// `_shouldWaitForModelSelectEmit(options)`.
    fn should_wait_for_model_select_emit(&self, options: &ModelSelectOptions) -> bool {
        !options.defer_emit.unwrap_or(false) && self.model_select_emit_queue_idle.load(Ordering::SeqCst)
    }

    /// `_pendingModelSelectEmit()`.
    async fn pending_model_select_emit(&self) {
        let queue = self.model_select_emit_queue.lock().unwrap().clone();
        let _ = queue.await;
    }

    /// `cycleModel(direction)`.
    pub async fn cycle_model(
        self: &Arc<Self>,
        direction: Option<i64>,
        options: ModelSelectOptions,
    ) -> Result<ModelCycleResult, String> {
        let scoped = self.scoped_models();
        if !scoped.is_empty() {
            return self.cycle_scoped_model(direction.unwrap_or(1), options).await;
        }
        self.cycle_available_model(direction.unwrap_or(1), options).await
    }

    /// `_cycleScopedModel(direction, options)`.
    async fn cycle_scoped_model(
        self: &Arc<Self>,
        direction: i64,
        options: ModelSelectOptions,
    ) -> Result<ModelCycleResult, String> {
        let scoped = self.scoped_models();
        if scoped.is_empty() {
            return Err("No scoped models configured".to_string());
        }
        let current = self.model();
        let index = scoped
            .iter()
            .position(|entry| models_are_equal(&entry.model, &current))
            .unwrap_or(0);
        let next_index = ((index as i64 + direction).rem_euclid(scoped.len() as i64)) as usize;
        let next = scoped[next_index].clone();
        self.set_model(next.model.clone(), options).await?;
        if let Some(thinking) = next.thinking_level.clone() {
            self.set_thinking_level(thinking);
        }
        Ok(ModelCycleResult {
            model: next.model,
            thinking_level: next.thinking_level,
        })
    }

    /// `_cycleAvailableModel(direction, options)`.
    async fn cycle_available_model(
        self: &Arc<Self>,
        direction: i64,
        options: ModelSelectOptions,
    ) -> Result<ModelCycleResult, String> {
        let models = self.model_registry.lock().unwrap().get_available();
        if models.is_empty() {
            return Err("No models available".to_string());
        }
        let current = self.model();
        let index = models
            .iter()
            .position(|model| models_are_equal(model, &current))
            .unwrap_or(0);
        let next_index = ((index as i64 + direction).rem_euclid(models.len() as i64)) as usize;
        let next = models[next_index].clone();
        self.set_model(next.clone(), options).await?;
        Ok(ModelCycleResult {
            model: next,
            thinking_level: None,
        })
    }

    /// `setThinkingLevel(level)`.
    pub fn set_thinking_level(self: &Arc<Self>, level: ThinkingLevel) {
        let clamped = clamp_thinking_level_for_model(&self.model(), level);
        let mut state = self.agent.state();
        state.thinking_level = clamped.clone();
        self.agent.set_state(state);
        let _ = self
            .session_manager
            .lock()
            .unwrap()
            .append_thinking_level_change(&thinking_level_name(&clamped));
        self.emit(AgentSessionEvent::ThinkingLevelChange {
            level: thinking_level_name(&clamped),
        });
    }

    /// `setServiceTier(serviceTier)`.
    pub fn set_service_tier(self: &Arc<Self>, service_tier: ServiceTier) {
        let clamped = self.clamp_service_tier_for_model(Some(service_tier));
        *self.service_tier_preference.lock().unwrap() = clamped.clone();
        let mut state = self.agent.state();
        state.service_tier = clamped.clone();
        self.agent.set_state(state);
        let _ = self
            .session_manager
            .lock()
            .unwrap()
            .append_service_tier_change(&service_tier_name(&clamped));
        self.emit(AgentSessionEvent::ServiceTierChange {
            service_tier: service_tier_name(&clamped),
        });
    }

    /// `_getEffectiveServiceTier(serviceTier)`.
    fn get_effective_service_tier(&self, service_tier: ServiceTier) -> ServiceTier {
        self.clamp_service_tier_for_model(Some(service_tier))
    }

    /// `_getServiceTierForModelSwitch()`.
    fn get_service_tier_for_model_switch(&self) -> ServiceTier {
        self.service_tier_preference.lock().unwrap().clone()
    }

    /// `_clampServiceTierForModel(serviceTier)`.
    fn clamp_service_tier_for_model(&self, service_tier: Option<ServiceTier>) -> ServiceTier {
        let service_tier = service_tier.unwrap_or_else(|| self.get_service_tier_for_model_switch());
        if supports_fast_mode(&self.model()) {
            service_tier
        } else {
            ServiceTier::Standard
        }
    }

    /// `cycleThinkingLevel()`.
    pub fn cycle_thinking_level(self: &Arc<Self>) -> Option<ThinkingLevel> {
        let available = self.get_available_thinking_levels();
        if available.is_empty() {
            return None;
        }
        let current = self.thinking_level();
        let index = available
            .iter()
            .position(|level| level == &current)
            .unwrap_or(usize::MAX);
        let next = available[(index + 1) % available.len()].clone();
        self.set_thinking_level(next.clone());
        Some(next)
    }

    /// `getAvailableThinkingLevels()`.
    pub fn get_available_thinking_levels(&self) -> Vec<ThinkingLevel> {
        get_supported_thinking_levels(&self.model())
    }

    /// `supportsThinking()`.
    pub fn supports_thinking(&self) -> bool {
        !self.get_available_thinking_levels().is_empty()
    }

    /// `_getThinkingLevelForModelSwitch(explicitLevel)`.
    fn get_thinking_level_for_model_switch(&self, explicit_level: Option<ThinkingLevel>) -> ThinkingLevel {
        match explicit_level {
            Some(level) => self.clamp_thinking_level(level),
            None => self.thinking_level(),
        }
    }

    /// `_clampThinkingLevel(level, _availableLevels)`.
    fn clamp_thinking_level(&self, level: ThinkingLevel) -> ThinkingLevel {
        clamp_thinking_level_for_model(&self.model(), level)
    }

    /// `_syncKernelStateAfterCompaction()`.
    async fn sync_kernel_state_after_compaction(self: &Arc<Self>) -> Result<(), String> {
        let provisioner = self.ipython_kernel_provisioner.lock().unwrap().clone();
        match provisioner {
            Some(provisioner) => {
                let _ = provisioner;
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// `_onIpythonStateRestored(result)`.
    fn on_ipython_state_restored(&self, result: RestoreResult) {
        let message = create_custom_message(
            IPYTHON_STATE_RESTORED_CUSTOM_TYPE.to_string(),
            CustomMessageContent::Text(status_text_from_restore(&result)),
            true,
            Some(serde_json::to_value(&result).unwrap_or(Value::Null)),
            now_ms_i64(),
        );
        let _ = self
            .session_manager
            .lock()
            .unwrap()
            .append_custom_message_entry(
                &message.custom_type,
                message.content.clone(),
                message.display,
                message.details.clone(),
            );
    }

    /// `setSteeringMode(mode)`.
    pub fn set_steering_mode(&self, mode: &str) {
        *self.steering_mode.lock().unwrap() = mode.to_string();
        self.agent.set_steering_mode(mode.to_string());
    }

    /// `setFollowUpMode(mode)`.
    pub fn set_follow_up_mode(&self, mode: &str) {
        *self.follow_up_mode.lock().unwrap() = mode.to_string();
        self.agent.set_follow_up_mode(mode.to_string());
    }

    /// `compact(customInstructions, options)`.
    pub async fn compact_with_options(
        self: &Arc<Self>,
        custom_instructions: Option<&str>,
        skip_abort: bool,
    ) -> Result<(), String> {
        if !skip_abort {
            self.abort_compaction();
        }
        if self.is_compacting() {
            return Ok(());
        }
        let controller = CancellationToken::new();
        *self.compaction_abort_controller.lock().unwrap() = Some(controller.clone());
        let result = self
            .perform_compaction_unmeasured(Some(custom_instructions.map(|value| value.to_string())), controller.clone())
            .await;
        *self.compaction_abort_controller.lock().unwrap() = None;
        self.reap_deleted_rlm_subagent_runtimes_after_compaction().await;
        result
    }

    /// `_reapDeletedRlmSubagentRuntimesAfterCompaction()`.
    async fn reap_deleted_rlm_subagent_runtimes_after_compaction(self: &Arc<Self>) {
        let deleted: Vec<String> = self.deleted_rlm_child_ids.lock().unwrap().iter().cloned().collect();
        for child_id in deleted {
            let _ = self.delete_rlm_subagent(&child_id).await;
        }
    }

    /// `abortCompaction()`.
    pub fn abort_compaction(&self) {
        if let Some(controller) = self.compaction_abort_controller.lock().unwrap().clone() {
            controller.cancel();
        }
        if let Some(controller) = self.auto_compaction_abort_controller.lock().unwrap().clone() {
            controller.cancel();
        }
        self.emit(AgentSessionEvent::CompactionUpdate {
            active: false,
            reason: None,
        });
    }

    /// `_localHarnessStateDir()`.
    fn local_harness_state_dir(&self) -> Option<String> {
        get_local_harness_state_dir(self.session_file().as_deref())
    }

    /// `_autoRefineAllowedForSession()`.
    fn auto_refine_allowed_for_session(&self) -> bool {
        self.local_harness_state_dir().is_some()
    }

    /// `_settlePostCompactionContinue(error?)`.
    fn settle_post_compaction_continue(&self, error: Option<&str>) {
        let settlement = self.post_compaction_continuation_settlement.lock().unwrap().clone();
        if let Some(settlement) = settlement {
            let mut settlement = settlement.lock().unwrap();
            if settlement.settled {
                return;
            }
            settlement.settled = true;
            match error {
                Some(error) => settlement.deferred.reject(error.to_string()),
                None => settlement.deferred.resolve(),
            }
        }
    }

    /// `_cancelPostCompactionContinue()`.
    fn cancel_post_compaction_continue(&self) {
        *self.post_compaction_continuation_settlement.lock().unwrap() = None;
        self.post_compaction_continuation_scheduled
            .store(false, Ordering::SeqCst);
    }

    /// `_discardPendingAutoRefine(options)`.
    fn discard_pending_auto_refine(&self, cancel_post_compaction_continue: bool) {
        *self.pending_auto_refine_review.lock().unwrap() = None;
        self.compact_auto_refine_pending.store(false, Ordering::SeqCst);
        self.turn_interval_auto_refine_pending
            .store(false, Ordering::SeqCst);
        if cancel_post_compaction_continue {
            self.cancel_post_compaction_continue();
        }
    }

    /// `_invalidatePendingAutoRefineForBranchChange()`.
    async fn invalidate_pending_auto_refine_for_branch_change(self: &Arc<Self>) {
        self.auto_refine_branch_version.fetch_add(1, Ordering::SeqCst);
        self.discard_pending_auto_refine(true);
        self.invalidate_queued_prompt_preparation();
    }

    /// `_consumePendingRequestedRefine()`.
    fn consume_pending_requested_refine(&self) -> bool {
        self.pending_requested_refine
            .lock()
            .unwrap()
            .take()
            .is_some()
    }

    /// `_scheduleAutoRefineAfterAgentEnd()`.
    fn schedule_auto_refine_after_agent_end(self: &Arc<Self>) {
        if !self.auto_refine_allowed_for_session() {
            return;
        }
        if self.should_skip_auto_refine_for_active_agent() {
            self.schedule_deferred_auto_refine_if_idle();
            return;
        }
        self.auto_refine_branch_version.fetch_add(1, Ordering::SeqCst);
        self.emit(AgentSessionEvent::RefinementUpdate {
            active: false,
            reason: Some("turn_interval".to_string()),
        });
    }

    /// `_scheduleAutoRefineAfterCompaction(willContinueAfterCompaction)`.
    fn schedule_auto_refine_after_compaction(self: &Arc<Self>, will_continue_after_compaction: bool) {
        if !self.auto_refine_allowed_for_session() {
            return;
        }
        self.compact_auto_refine_pending.store(true, Ordering::SeqCst);
        if !will_continue_after_compaction {
            self.schedule_deferred_auto_refine_if_idle();
        }
    }

    /// `_schedulePostCompactionContinue(continueAfterSessionInput)`.
    fn schedule_post_compaction_continue(self: &Arc<Self>, continue_after_session_input: bool) {
        if self
            .post_compaction_continuation_scheduled
            .swap(true, Ordering::SeqCst)
        {
            return;
        }
        let settlement = Arc::new(Mutex::new(PostCompactionContinuationSettlement {
            deferred: create_agent_message_deferred(),
            continue_after_session_input,
            settled: false,
        }));
        *self.post_compaction_continuation_settlement.lock().unwrap() = Some(settlement.clone());
        let session = self.clone();
        tokio::spawn(async move {
            session.run_scheduled_post_compaction_continue(settlement).await;
        });
    }

    /// `_sessionOwnsScheduledContinuations(continuationMessages)`.
    fn session_owns_scheduled_continuations(&self, continuation_messages: &[AgentMessage]) -> bool {
        let scheduled = self
            .scheduled_post_compaction_continuation_messages
            .lock()
            .unwrap();
        let owned: HashSet<String> = scheduled.iter().map(agent_message_key_of).collect();
        continuation_messages
            .iter()
            .any(|message| owned.contains(&agent_message_key_of(message)))
    }

    /// `_waitForQueuedWorkResume(settlement)`.
    async fn wait_for_queued_work_resume(
        &self,
        settlement: &Arc<Mutex<PostCompactionContinuationSettlement>>,
    ) {
        loop {
            if !self.is_queued_work_suspended() {
                return;
            }
            if settlement.lock().unwrap().settled {
                return;
            }
            self.session_action_activity_notify.notified().await;
        }
    }

    /// `_runScheduledPostCompactionContinue(settlement)`.
    async fn run_scheduled_post_compaction_continue(
        self: &Arc<Self>,
        settlement: Arc<Mutex<PostCompactionContinuationSettlement>>,
    ) {
        let (deferred, continue_after_session_input) = {
            let settlement = settlement.lock().unwrap();
            (
                settlement.deferred.clone(),
                settlement.continue_after_session_input,
            )
        };
        self.wait_for_queued_work_resume(&settlement).await;
        self.wait_for_session_input_idle().await.ok();
        if !continue_after_session_input {
            self.wait_for_session_input_idle().await.ok();
        }
        self.post_compaction_continuation_scheduled
            .store(false, Ordering::SeqCst);
        deferred.resolve();
    }

    /// `_shouldSkipAutoRefineForActiveAgent()`.
    fn should_skip_auto_refine_for_active_agent(&self) -> bool {
        self.is_streaming() || self.is_compacting()
    }

    /// `_scheduleDeferredAutoRefineIfIdle()`.
    fn schedule_deferred_auto_refine_if_idle(self: &Arc<Self>) {
        if self.is_streaming() || self.is_compacting() {
            return;
        }
        self.compact_auto_refine_pending.store(false, Ordering::SeqCst);
        self.turn_interval_auto_refine_pending
            .store(false, Ordering::SeqCst);
    }

    /// `_scheduleAutoRefine(reason, branchVersion)`.
    fn schedule_auto_refine(&self, reason: &AutoRefineReason, branch_version: Option<u64>) {
        let branch_version =
            branch_version.unwrap_or_else(|| self.auto_refine_branch_version.load(Ordering::SeqCst));
        let pending = match reason {
            AutoRefineReason::Compact => &self.compact_auto_refine_pending,
            AutoRefineReason::TurnInterval => &self.turn_interval_auto_refine_pending,
        };
        pending.store(true, Ordering::SeqCst);
        let _ = branch_version;
    }

    /// `_maybeAutoRefine(reason)`.
    async fn maybe_auto_refine(self: &Arc<Self>, reason: &AutoRefineReason) -> Result<(), String> {
        if !self.auto_refine_allowed_for_session() {
            return Ok(());
        }
        if self.auto_refine_in_progress.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let result = self.maybe_auto_refine_inner(reason).await;
        self.auto_refine_in_progress.store(false, Ordering::SeqCst);
        result
    }

    /// The body of `_maybeAutoRefine`.
    async fn maybe_auto_refine_inner(self: &Arc<Self>, reason: &AutoRefineReason) -> Result<(), String> {
        self.append_harness_digest_if_stale();
        let branch_version = self.auto_refine_branch_version.load(Ordering::SeqCst);
        self.emit(AgentSessionEvent::RefinementUpdate {
            active: true,
            reason: Some(reason.as_str().to_string()),
        });
        let review = self
            .review_auto_refine(
                &AutoRefineReviewContext {
                    reason: *reason,
                    turns_since_last_review: self.assistant_turns_since_auto_refine.load(Ordering::SeqCst)
                        as i64,
                },
                None,
            )
            .await;
        let review = match review {
            Ok(review) => review,
            Err(_) => {
                *self.last_auto_refine_review_at.lock().unwrap() = now_ms();
                self.emit(AgentSessionEvent::RefinementUpdate {
                    active: false,
                    reason: Some(reason.as_str().to_string()),
                });
                self.schedule_deferred_auto_refine_if_idle();
                return Ok(());
            }
        };
        let approved_review = if review.should_refine {
            Some(review)
        } else {
            *self.last_auto_refine_review_at.lock().unwrap() = now_ms();
            None
        };
        if approved_review.is_none() {
            self.schedule_deferred_auto_refine_if_idle();
            return Ok(());
        }
        if self.auto_refine_branch_version.load(Ordering::SeqCst) != branch_version {
            self.emit(AgentSessionEvent::RefinementUpdate {
                active: false,
                reason: Some(reason.as_str().to_string()),
            });
            return Ok(());
        }
        if let Some(approved_review) = approved_review {
            self.run_approved_refine(reason, &approved_review).await?;
        }
        Ok(())
    }

    /// `_runApprovedRefine(reason, review)`.
    async fn run_approved_refine(
        self: &Arc<Self>,
        reason: &AutoRefineReason,
        review: &AutoRefineReview,
    ) -> Result<(), String> {
        self.auto_refine_in_progress.store(true, Ordering::SeqCst);
        let options = RefineOptions {
            instructions: Some(auto_refine_instructions(reason, review)),
            ..Default::default()
        };
        let outcome = self.refine_with_options(&options, false).await;
        match outcome {
            Ok(_) => {
                *self.pending_auto_refine_review.lock().unwrap() = None;
                *self.last_auto_refine_review_at.lock().unwrap() = now_ms();
                self.assistant_turns_since_auto_refine.store(0, Ordering::SeqCst);
                self.turn_interval_auto_refine_pending.store(false, Ordering::SeqCst);
                if *reason == AutoRefineReason::Compact {
                    self.compact_auto_refine_pending.store(false, Ordering::SeqCst);
                }
            }
            Err(_) => {
                // Auto-refine is opportunistic; manual /refine remains available.
                // Stamp the cooldown so a persistently failing refine does not retry
                // on every agent end.
                *self.last_auto_refine_review_at.lock().unwrap() = now_ms();
            }
        }
        self.auto_refine_in_progress.store(false, Ordering::SeqCst);
        self.schedule_deferred_auto_refine_if_idle();
        Ok(())
    }

    /// `_reviewAutoRefine(context, signal?)`.
    async fn review_auto_refine(
        self: &Arc<Self>,
        context: &AutoRefineReviewContext,
        signal: Option<CancellationToken>,
    ) -> Result<AutoRefineReview, String> {
        let request = AutoRefineReviewRequest {
            reason: context.reason,
            turns_since_last_review: context.turns_since_last_review,
        };
        if let Some(reviewer) = &self.auto_refine_reviewer {
            return reviewer(request.clone(), signal).await;
        }
        let model = match self.model() {
            Some(model) => model,
            None => {
                return Ok(AutoRefineReview {
                    should_refine: false,
                    rationale: "No model selected.".to_string(),
                    instructions: None,
                })
            }
        };
        let auth = self.get_required_request_auth(&model).await?;
        let state = self.load_merged_harness_state();
        let history = self.load_refinement_history();
        review_auto_refine(ReviewAutoRefineRequest {
            messages: &self.agent.state().messages,
            state: &state,
            history: &history,
            model: RefineModel {
                max_tokens: model.max_tokens,
            },
            api_key: auth.api_key,
            context: AutoRefineReviewContext {
                reason: context.reason,
                turns_since_last_review: context.turns_since_last_review,
            },
            headers: auth.headers.clone(),
            thinking_level: Some(thinking_level_name(&self.thinking_level())),
            retry: Some(self.provider_retry_policy()),
            complete: self.refinement_completion_fn(model),
        })
        .await
        .map_err(|error| error.message)
    }

    /// `providerRetryPolicy(this.settingsManager)`.
    fn provider_retry_policy(&self) -> ProviderRetryPolicy {
        crate::core::provider_retry::provider_retry_policy(&self.settings_manager.lock().unwrap())
    }

    /// `completeWithProviderRetry(() => completeSimple(model, context, options))`.
    fn refinement_completion_fn(&self, model: Model) -> CompletionFn {
        Arc::new(move |request: RefinementCompletionRequest| {
            let model = model.clone();
            Box::pin(async move {
                let options = pi_ai::types::SimpleStreamOptions {
                    stream: pi_ai::types::StreamOptions {
                        max_tokens: Some(request.max_tokens),
                        api_key: request.api_key.clone(),
                        headers: request
                            .headers
                            .clone()
                            .map(|headers| headers.into_iter().collect()),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let context = pi_ai::types::Context {
                    system_prompt: Some(request.system_prompt),
                    messages: request
                        .messages
                        .iter()
                        .map(|message| {
                            pi_ai::types::Message::User(pi_ai::types::UserMessage::new(
                                pi_ai::types::UserContent::Text(message.content.clone()),
                                message.timestamp,
                            ))
                        })
                        .collect(),
                    tools: None,
                };
                pi_ai::stream::complete_simple(&model, &context, Some(&options)).await
            })
        })
    }

    /// `_loadMergedHarnessState()`.
    fn load_merged_harness_state(&self) -> HarnessState {
        let global = load_harness_state(
            &get_global_harness_state_dir(&self.agent_dir.clone().unwrap_or_default()),
            HarnessScope::Global,
        );
        match self.local_harness_state_dir() {
            Some(dir) => merge_harness_states(&global, Some(&load_harness_state(&dir, HarnessScope::Local))),
            None => global,
        }
    }

    /// `_planRefine(options, signal, trigger)`: the planning phase of `refine()`.
    async fn plan_refine_with_options(
        self: &Arc<Self>,
        options: &RefineOptions,
    ) -> Result<RefinementPlan, String> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err("Cannot refine a disposed session.".to_string());
        }
        let model = match self.model() {
            Some(model) => model,
            None => return Err(format_no_model_selected_message()),
        };
        let auth = self.get_required_request_auth(&model).await?;
        let global_dir = get_global_harness_state_dir(&self.agent_dir.clone().unwrap_or_default());
        let local_dir = self.local_harness_state_dir();
        let global_state = load_harness_state(&global_dir, HarnessScope::Global);
        let local_state = local_dir
            .as_deref()
            .map(|dir| load_harness_state(dir, HarnessScope::Local));
        let planning_state = merge_harness_states(&global_state, local_state.as_ref());
        let history = self.load_refinement_history();
        let rollback_target = options
            .rollback_id
            .as_ref()
            .and_then(|rollback_id| history.iter().find(|item| &item.id == rollback_id));
        let mut baseline_scope = rollback_target
            .and_then(infer_refinement_result_scope)
            .unwrap_or(HarnessScope::Local);
        let mut baseline_dir = match baseline_scope {
            HarnessScope::Global => Some(global_dir.clone()),
            HarnessScope::Local => local_dir.clone(),
        };
        if let Some(target) = rollback_target {
            let path = &target.harness_state_path;
            let parent = Path::new(path)
                .parent()
                .map(|parent| parent.to_string_lossy().to_string());
            if let Some(parent) = parent {
                baseline_scope = if Path::new(&parent) == Path::new(&global_dir) {
                    HarnessScope::Global
                } else {
                    HarnessScope::Local
                };
                baseline_dir = Some(parent);
            }
        }
        let baseline_state = match rollback_target {
            Some(_) => load_harness_state(&baseline_dir.unwrap_or_default(), baseline_scope),
            None => match baseline_scope {
                HarnessScope::Global => global_state.clone(),
                HarnessScope::Local => local_state.clone().unwrap_or(HarnessState {
                    schema: 1.0,
                    entries: indexmap::IndexMap::new(),
                    refinements: Vec::new(),
                }),
            },
        };
        let plan = plan_refinement(PlanRefinementRequest {
            messages: &self.agent.state().messages[..],
            state: &planning_state,
            history: &history,
            model: RefineModel {
                max_tokens: model.max_tokens,
            },
            api_key: auth.api_key,
            options: options.clone(),
            headers: auth.headers.clone(),
            thinking_level: Some(thinking_level_name(&self.thinking_level())),
            complete: self.refinement_completion_fn(model.clone()),
        })
        .await
        .map_err(|error| error.message)?;
        Ok(RefinementPlan {
            baseline_state: Some(baseline_state),
            ..plan
        })
    }

    /// `_applyRefine(plan, options, refineAbort, source)`.
    async fn apply_refine(
        self: &Arc<Self>,
        plan: &RefinementPlan,
        options: &RefineOptions,
        source: &str,
    ) -> Result<RefinementResult, String> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err("Cannot refine a disposed session.".to_string());
        }
        self.disconnect_from_agent();
        let outcome = self.apply_refine_inner(plan, options, source).await;
        if !self.disposed.load(Ordering::SeqCst) {
            self.reconnect_to_agent();
        }
        outcome
    }

    /// The `_applyRefine` body between disconnect and reconnect.
    async fn apply_refine_inner(
        &self,
        plan: &RefinementPlan,
        options: &RefineOptions,
        source: &str,
    ) -> Result<RefinementResult, String> {
        let global_dir = get_global_harness_state_dir(&self.agent_dir.clone().unwrap_or_default());
        let local_dir = self.local_harness_state_dir();
        let requested_scope = if options.global.unwrap_or(false) {
            HarnessScope::Global
        } else {
            HarnessScope::Local
        };
        let history = self.load_refinement_history();
        let rollback_target = options
            .rollback_id
            .as_ref()
            .and_then(|rollback_id| history.iter().find(|item| &item.id == rollback_id));
        let mut target_scope = plan.rollback_scope.unwrap_or(requested_scope);
        let mut target_dir = match target_scope {
            HarnessScope::Global => Some(global_dir.clone()),
            HarnessScope::Local => local_dir.clone(),
        };
        if target_scope == HarnessScope::Local {
            if let Some(path) = rollback_target.and_then(|target| target.harness_state_path.clone()) {
                let parent = Path::new(&path)
                    .parent()
                    .map(|parent| parent.to_string_lossy().to_string());
                if let Some(parent) = parent {
                    target_dir = Some(parent);
                    if Path::new(&target_dir.clone().unwrap_or_default()) == Path::new(&global_dir) {
                        target_scope = HarnessScope::Global;
                    }
                }
            }
        }
        let target_dir = match target_dir {
            Some(dir) => dir,
            None => {
                return Err(
                    "Local harness refinement requires a persisted session; use global refinement instead."
                        .to_string(),
                )
            }
        };
        let mut state = load_harness_state(&target_dir, target_scope);
        let proposal = RefinementProposal {
            edits: plan
                .proposal
                .edits
                .iter()
                .map(|edit| {
                    let mut edit = edit.clone();
                    if let Some(id) = &edit.id {
                        if let Some(rest) = id.strip_prefix("local:") {
                            edit.id = Some(rest.to_string());
                        } else if let Some(rest) = id.strip_prefix("global:") {
                            edit.id = Some(rest.to_string());
                        }
                    }
                    edit
                })
                .collect(),
            ..plan.proposal.clone()
        };
        if self.disposed.load(Ordering::SeqCst) {
            return Err("Refinement cancelled because the session was disposed.".to_string());
        }
        let mut result = apply_refinement_proposal(
            &mut state,
            &proposal,
            ApplyRefinementOptions {
                id: plan.id.clone(),
                rollback_of: plan.rollback_of.clone(),
                scope: Some(target_scope),
                baseline_state: plan.baseline_state.clone(),
            },
        );
        result.harness_state_path = save_harness_state(&target_dir, &state)?;
        if target_scope == HarnessScope::Global {
            append_global_refinement(&global_dir, &result);
        }
        let _ = self
            .session_manager
            .lock()
            .unwrap()
            .append_custom_entry(REFINEMENT_CUSTOM_TYPE, Some(serde_json::to_value(&result).unwrap_or(Value::Null)));
        self.record_refinement_outcome(&result);
        self.record_refinement_notice(&result, source);
        self.emit(AgentSessionEvent::RefineComplete {
            result: result.clone(),
        });
        Ok(result)
    }

    /// `_recordRefinementNotice(result, source)`.
    fn record_refinement_notice(&self, result: &RefinementResult, source: &str) {
        if !result.applied_edits.iter().any(|edit| edit.applied) {
            return;
        }
        self.append_durable_refine_message(&create_refinement_notice_message(
            result,
            source.to_string(),
            now_ms_i64(),
        ));
    }

    /// `refine(options, internal)`.
    async fn refine_with_options(
        self: &Arc<Self>,
        options: &RefineOptions,
        skip_abort: bool,
    ) -> Result<RefinementResult, String> {
        if skip_abort && self.is_streaming() {
            return Err("Cannot refine without aborting while the agent is running.".to_string());
        }
        while self.refine_in_flight.lock().unwrap().is_some()
            || self.refine_plan_in_flight.lock().unwrap().is_some()
            || self.serialized_plan_in_flight.lock().unwrap().is_some()
        {
            if self.refine_in_flight.lock().unwrap().is_some() {
                self.wait_for_refine_idle().await;
            } else if self.refine_plan_in_flight.lock().unwrap().is_some() {
                let in_flight = self.refine_plan_in_flight.lock().unwrap().take();
                if let Some(in_flight) = in_flight {
                    let _ = in_flight.await;
                }
            } else {
                let in_flight = self.serialized_plan_in_flight.lock().unwrap().take();
                if let Some(in_flight) = in_flight {
                    let _ = in_flight.await;
                }
                if self.refine_in_flight.lock().unwrap().is_some()
                    || self.refine_plan_in_flight.lock().unwrap().is_some()
                {
                    continue;
                }
                let _ = self.agent.wait_for_idle().await;
                self.serialized_explicit_refine_options.lock().unwrap().take();
            }
        }

        let plan = self.plan_refine_with_options(options).await;
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => {
                self.schedule_session_input_pump();
                return Err(error);
            }
        };

        let session = self.clone();
        let apply = Arc::new({
            let options = options.clone();
            move || {
                let session = session.clone();
                let plan = plan.clone();
                let options = options.clone();
                Box::pin(async move { session.apply_refine(&plan, &options, options_source(&options)).await })
            }
        });
        let settle = self.create_refine_settlement(apply.clone());
        let outcome = apply().await;
        settle();
        self.notify_session_input_checkpoint_change();
        self.schedule_session_input_pump();
        outcome
    }

    /// The shared `_refineInFlight` settlement used by concurrent `refine` callers.
    fn create_refine_settlement(
        self: &Arc<Self>,
        _apply: Arc<dyn Fn() -> BoxFuture<Result<RefinementResult, String>> + Send + Sync>,
    ) -> Arc<dyn Fn() + Send + Sync> {
        let session = self.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let settled: BoxFuture<Result<(), String>> = Box::pin(async move {
            let _ = rx.await;
            Ok(())
        });
        *self.refine_in_flight.lock().unwrap() = Some(settled);
        Arc::new(move || {
            if session.refine_in_flight.lock().unwrap().is_some() {
                session.refine_in_flight.lock().unwrap().take();
            }
            let _ = tx.send(());
        })
    }

    /// `_runSerializedRefine(options, source)`.
    async fn run_serialized_refine(
        self: &Arc<Self>,
        options: &RefineOptions,
        source: &str,
    ) -> Result<(), String> {
        if self.disposed.load(Ordering::SeqCst) || self.disposing.load(Ordering::SeqCst) {
            return Ok(());
        }
        while self.serialized_plan_in_flight.lock().unwrap().is_some()
            || self.refine_in_flight.lock().unwrap().is_some()
            || self.refine_plan_in_flight.lock().unwrap().is_some()
        {
            if self.serialized_plan_in_flight.lock().unwrap().is_some() {
                let in_flight = self.serialized_plan_in_flight.lock().unwrap().take();
                if let Some(in_flight) = in_flight {
                    let _ = in_flight.await;
                }
            } else if self.refine_in_flight.lock().unwrap().is_some() {
                self.wait_for_refine_idle().await;
            } else {
                let in_flight = self.refine_plan_in_flight.lock().unwrap().take();
                if let Some(in_flight) = in_flight {
                    let _ = in_flight.await;
                }
            }
        }
        if self.disposed.load(Ordering::SeqCst) || self.disposing.load(Ordering::SeqCst) {
            return Ok(());
        }
        let plan = self.plan_refine_with_options(options).await;
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => {
                self.schedule_session_input_pump();
                return Err(error);
            }
        };
        if self.disposed.load(Ordering::SeqCst) {
            self.schedule_session_input_pump();
            return Ok(());
        }
        let outcome = self.apply_refine(&plan, options, source).await;
        self.notify_session_input_checkpoint_change();
        self.schedule_session_input_pump();
        outcome.map(|_| ())
    }

    /// `_runSerializedRefineCheckpoint()`.
    async fn run_serialized_refine_checkpoint(self: &Arc<Self>) {
        if self.disposed.load(Ordering::SeqCst) || self.disposing.load(Ordering::SeqCst) {
            return;
        }
        if self.serialized_plan_in_flight.lock().unwrap().is_some() {
            let in_flight = self.serialized_plan_in_flight.lock().unwrap().take();
            if let Some(in_flight) = in_flight {
                let _ = in_flight.await;
            }
        }
        let pending = self.pending_requested_refine.lock().unwrap().take();
        if let Some(pending) = pending {
            let options = RefineOptions {
                instructions: pending.instructions,
                rollback_id: None,
                global: pending.global,
                ..Default::default()
            };
            let _ = self.run_serialized_refine(&options, REFINEMENT_SOURCE_SELF).await;
            return;
        }
        if !self.auto_refine_allowed_for_session() {
            return;
        }
        let settings = self.settings_manager.lock().unwrap().get_auto_refine_settings();
        if !settings.enabled {
            return;
        }
        if (self.assistant_turns_since_auto_refine.load(Ordering::SeqCst) as f64) < settings.turn_interval {
            return;
        }
        let now = now_ms();
        let last = *self.last_auto_refine_review_at.lock().unwrap();
        if last > 0.0 && now - last < settings.cooldown_ms {
            return;
        }
        let options = RefineOptions::default();
        let _ = self
            .run_serialized_refine(&options, REFINEMENT_SOURCE_AUTO)
            .await;
    }

    /// `_waitForRefineIdle()`.
    async fn wait_for_refine_idle(self: &Arc<Self>) {
        while self.refine_in_flight.lock().unwrap().is_some() {
            let in_flight = self.refine_in_flight.lock().unwrap().take();
            if let Some(in_flight) = in_flight {
                let _ = in_flight.await;
            }
        }
    }


    /// `_appendHarnessDigestIfStale()`.
    fn append_harness_digest_if_stale(&self) {
        let digest = self.harness_digest();
        if digest.is_empty() {
            return;
        }
        if self.latest_context_harness_digest() == Some(digest) {
            return;
        }
        self.harness_digest_pending.store(true, Ordering::SeqCst);
    }

    /// `_latestContextHarnessDigest()`.
    fn latest_context_harness_digest(&self) -> Option<String> {
        let messages = self.agent.state().messages;
        for message in messages.iter().rev() {
            if let AgentMessage::Custom(CustomAgentMessage::Custom { custom_type, details, .. }) = message {
                if custom_type == HARNESS_DIGEST_CUSTOM_TYPE {
                    return details
                        .as_ref()
                        .and_then(|details| details.get("digest"))
                        .and_then(Value::as_str)
                        .map(|digest| digest.to_string());
                }
            }
        }
        None
    }

    /// `_harnessDigest()`.
    fn harness_digest(&self) -> String {
        let local = self.local_harness_state_dir();
        let state = match local.as_deref() {
            Some(dir) => load_harness_state(dir),
            None => HarnessState::default(),
        };
        format_harness_state_for_prompt(&state)
    }

    /// `_loadRefinementHistory()`.
    fn load_refinement_history(&self) -> Vec<RefinementResult> {
        let local = match self.local_harness_state_dir() {
            Some(dir) => dir,
            None => return Vec::new(),
        };
        let mut history = get_refinement_history(&local);
        history.extend(load_global_refinement_history());
        merge_refinement_history(history)
    }

    /// `_recordRefinementOutcome(result)`.
    fn record_refinement_outcome(&self, result: &RefinementResult) {
        self.append_durable_refine_message(&create_refinement_outcome_message(result, true, now_ms_i64()));
    }

    /// `_appendDurableRefineMessage(message)`.
    fn append_durable_refine_message(&self, message: &CustomMessage) {
        let _ = self
            .session_manager
            .lock()
            .unwrap()
            .append_custom_message_entry_with_rollback(
                &message.custom_type,
                message.content.clone(),
                message.display,
                message.details.clone(),
            );
        let mut state = self.agent.state();
        state.messages.push(AgentMessage::Custom(CustomAgentMessage::Custom {
            custom_type: message.custom_type.clone(),
            content: message.content.clone(),
            display: message.display,
            details: message.details.clone(),
            timestamp: message.timestamp,
        }));
        self.agent.set_state(state);
        self.emit(AgentSessionEvent::MessageStart {
            message: AgentMessage::Custom(CustomAgentMessage::Custom {
                custom_type: message.custom_type.clone(),
                content: message.content.clone(),
                display: message.display,
                details: message.details.clone(),
                timestamp: message.timestamp,
            }),
        });
    }

    /// `abortBranchSummary()`.
    pub fn abort_branch_summary(&self) {
        if let Some(controller) = self.branch_summary_abort_controller.lock().unwrap().clone() {
            controller.cancel();
        }
    }

    /// `_restoreProviderContextForModel()`.
    fn restore_provider_context_for_model(&self) {
        let checkpoint = has_provider_checkpoint(self.session_manager.lock().unwrap().get_branch(None));
        if !checkpoint {
            return;
        }
        *self.provider_context_rebuilt_at.lock().unwrap() = Some(now_ms());
    }

    /// `_activeCompactionTimestamp()`.
    fn active_compaction_timestamp(&self) -> Option<f64> {
        let entry = get_latest_compaction_entry(self.session_manager.lock().unwrap().get_branch(None));
        entry.and_then(|entry| entry.get("timestamp").and_then(Value::as_f64))
    }

    /// `_getThresholdContextTokens(settings)`.
    fn get_threshold_context_tokens(&self, settings: &CompactionSettings) -> Option<f64> {
        let model = self.model();
        let limit = get_model_input_limit(&model);
        if !limit.is_finite() || limit <= 0.0 {
            return None;
        }
        let reserve = settings.reserve_tokens.unwrap_or(0.0);
        Some((limit - reserve).max(0.0))
    }

    /// `_checkCompaction(settings)`.
    async fn check_compaction(self: &Arc<Self>, settings: &CompactionSettings) -> Result<bool, String> {
        if !self.auto_compaction_enabled.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let threshold = self.get_threshold_context_tokens(settings);
        let threshold = match threshold {
            Some(threshold) => threshold,
            None => return Ok(false),
        };
        let context = self.build_session_context();
        let tokens = estimate_context_tokens(&context.messages);
        if tokens < threshold {
            return Ok(false);
        }
        self.emit(AgentSessionEvent::CompactionUpdate {
            active: true,
            reason: Some(COMPACTION_REASON_THRESHOLD.to_string()),
        });
        let outcome = self.run_auto_compaction(COMPACTION_REASON_THRESHOLD).await;
        self.persist_compaction_outcome(COMPACTION_REASON_THRESHOLD, outcome.as_deref());
        self.emit(AgentSessionEvent::CompactionUpdate {
            active: false,
            reason: Some(COMPACTION_REASON_THRESHOLD.to_string()),
        });
        Ok(true)
    }

    /// `_persistCompactionOutcome(reason, outcome, message?)`.
    fn persist_compaction_outcome(&self, reason: &str, outcome: Option<&str>) {
        let message = create_compaction_outcome_message(
            outcome.unwrap_or("Compaction finished.").to_string(),
            CompactionOutcomeDetails {
                reason: reason.to_string(),
                outcome: outcome.map(|value| value.to_string()),
            },
            false,
            now_ms_i64(),
        );
        let _ = self
            .session_manager
            .lock()
            .unwrap()
            .append_custom_message_entry(
                &message.custom_type,
                message.content.clone(),
                message.display,
                message.details.clone(),
            );
        let mut state = self.agent.state();
        state.messages.push(AgentMessage::Custom(CustomAgentMessage::Custom {
            custom_type: message.custom_type.clone(),
            content: message.content.clone(),
            display: message.display,
            details: message.details.clone(),
            timestamp: message.timestamp,
        }));
        self.agent.set_state(state);
        self.emit(AgentSessionEvent::MessageEnd {
            message: AgentMessage::Custom(CustomAgentMessage::Custom {
                custom_type: message.custom_type.clone(),
                content: message.content.clone(),
                display: message.display,
                details: message.details.clone(),
                timestamp: message.timestamp,
            }),
        });
    }

    /// `_runAutoCompaction(reason)`.
    async fn run_auto_compaction(self: &Arc<Self>, reason: &str) -> Option<String> {
        let controller = CancellationToken::new();
        *self.auto_compaction_abort_controller.lock().unwrap() = Some(controller.clone());
        let result = self
            .perform_compaction_unmeasured(None, controller.clone())
            .await;
        *self.auto_compaction_abort_controller.lock().unwrap() = None;
        self.schedule_auto_refine_after_compaction(false);
        match result {
            Ok(()) => Some(format!("Compaction completed ({reason}).")),
            Err(error) => {
                if error == COMPACTION_SKIPPED_ERROR_MESSAGE {
                    return None;
                }
                Some(format!("Compaction failed: {error}"))
            }
        }
    }

    /// `setAutoCompactionEnabled(enabled)`.
    pub fn set_auto_compaction_enabled(&self, enabled: bool) {
        self.auto_compaction_enabled.store(enabled, Ordering::SeqCst);
    }

    /// `get autoCompactionEnabled()`.
    pub fn auto_compaction_enabled(&self) -> bool {
        self.auto_compaction_enabled.load(Ordering::SeqCst)
    }


}

// ---------------------------------------------------------------------------
// Private module helpers used by the appended members
// (`_agentMessageOutcome` plumbing, keys and record bookkeeping).
// ---------------------------------------------------------------------------

/// Stable key for an `AgentMessage`, mirroring the TypeScript `WeakMap` identity
/// usage: equal messages map to equal keys.
pub fn agent_message_key_of(message: &AgentMessage) -> String {
    serde_json::to_string(message).unwrap_or_default()
}

fn agent_message_timestamp(message: &AgentMessage) -> i64 {
    match message {
        AgentMessage::Message(Message::User(message)) => message.timestamp,
        AgentMessage::Message(Message::Assistant(message)) => message.timestamp,
        AgentMessage::Message(Message::ToolResult(message)) => message.timestamp,
        AgentMessage::Custom(CustomAgentMessage::Custom { timestamp, .. }
            | CustomAgentMessage::BashExecution { timestamp, .. }
            | CustomAgentMessage::BranchSummary { timestamp, .. }
            | CustomAgentMessage::CompactionSummary { timestamp, .. }) => *timestamp,
    }
}

/// `agentMessageKeyOf(record.message)`.
fn delivery_message_key_of(message: &DeliveryMessage) -> String {
    match message {
        DeliveryMessage::User(user) => {
            serde_json::to_string(&pi_ai::types::Message::User(user.clone())).unwrap_or_default()
        }
        DeliveryMessage::Custom(custom) => serde_json::to_string(custom).unwrap_or_default(),
    }
}

/// `assistantMessageKey(message)`.
fn assistant_message_key(message: &AgentMessage) -> String {
    agent_message_key_of(message)
}

/// `deliveryMessageOf(message)`.
fn delivery_message_of(message: &AgentMessage) -> DeliveryMessage {
    match message {
        AgentMessage::Message(pi_ai::types::Message::User(user)) => DeliveryMessage::User(user.clone()),
        other => DeliveryMessage::Custom(custom_message_of(other)),
    }
}

/// `deliveryMessageFromValue(value)`.
fn delivery_message_from_value(value: &Value) -> DeliveryMessage {
    if let Ok(user) = serde_json::from_value::<UserMessage>(value.clone()) {
        if user.role == "user" {
            return DeliveryMessage::User(user);
        }
    }
    match serde_json::from_value::<CustomMessage>(value.clone()) {
        Ok(custom) => DeliveryMessage::Custom(custom),
        Err(_) => DeliveryMessage::Custom(CustomMessage::default()),
    }
}

/// `record.message.customType`.
fn record_message_custom_type(message: &DeliveryMessage) -> Option<String> {
    match message {
        DeliveryMessage::Custom(custom) => Some(custom.custom_type.clone()),
        DeliveryMessage::User(_) => None,
    }
}

/// `sessionActionSnapshotsEqual(left, right)`.
fn session_action_snapshots_equal(left: &SessionActionSnapshot, right: &SessionActionSnapshot) -> bool {
    left == right
}

/// `isRlmHeartbeatStatusUpdate(value)`.
fn is_rlm_heartbeat_status_update(value: &str) -> bool {
    value == "pause" || value == "resume"
}

/// `skill.filePath`.
fn skill_file_path(skill: &crate::core::skills::Skill) -> String {
    skill.file_path().to_string()
}

/// `skill.baseDir`.
fn skill_base_dir(skill: &crate::core::skills::Skill) -> String {
    Path::new(skill.file_path())
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// `Date.now()`.
pub fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

/// `Date.now()` as an integer timestamp.
fn now_ms_i64() -> i64 {
    now_ms() as i64
}

/// `customMessageValue(message)` - the JSON shape records persist.
fn custom_message_value(message: &CustomMessage) -> CustomMessage {
    clone_custom_message(message)
}

/// `customMessageKey(message)`.
fn custom_message_key(message: &CustomMessage) -> String {
    serde_json::to_string(message).unwrap_or_default()
}

/// `agentMessageFromDelivery(message)`.
fn agent_message_from_delivery(message: &DeliveryMessage) -> AgentMessage {
    match message {
        DeliveryMessage::User(user) => AgentMessage::Message(pi_ai::types::Message::User(user.clone())),
        DeliveryMessage::Custom(custom) => AgentMessage::Custom(CustomAgentMessage::Custom {
            custom_type: custom.custom_type.clone(),
            content: custom.content.clone(),
            display: custom.display,
            details: custom.details.clone(),
            timestamp: custom.timestamp,
        }),
    }
}

/// `agentMessageFromValue(value)`.
fn agent_message_from_value(value: &Value) -> AgentMessage {
    serde_json::from_value(value.clone()).unwrap_or_else(|_| {
        AgentMessage::Custom(CustomAgentMessage::Custom {
            custom_type: String::new(),
            content: CustomMessageContent::Text(String::new()),
            display: false,
            details: None,
            timestamp: 0,
        })
    })
}

/// `customMessageOf(message)` - the custom payload of a non-user message.
fn custom_message_of(message: &AgentMessage) -> CustomMessage {
    match message {
        AgentMessage::Custom(CustomAgentMessage::Custom {
            custom_type,
            content,
            display,
            details,
            timestamp,
        }) => CustomMessage {
            custom_type: custom_type.clone(),
            content: content.clone(),
            display: *display,
            details: details.clone(),
            timestamp: *timestamp,
            ..Default::default()
        },
        _ => CustomMessage::default(),
    }
}

/// `action.payload.text` for both payload kinds.
fn action_text(action: &QueuedSessionAction) -> String {
    match &action.payload {
        QueuedActionPayload::Turn(turn) => turn.base.text.clone(),
        QueuedActionPayload::SessionCommand(command) => command.base.text.clone(),
    }
}

/// `primaryDeliveryRecord(action)` where the TypeScript throws on a missing record.
fn primary_delivery_record_of(action: &QueuedSessionAction) -> Option<DeliveryRecord> {
    primary_delivery_record(action).ok()
}

/// `goal.status` name for the `goal` slash command result row.
fn goal_status_name(status: &GoalStatus) -> String {
    status.as_str().to_string()
}

/// `GoalContextKind` discriminator from its TypeScript string form.
fn goal_context_kind_of(kind: &str) -> GoalContextKind {
    match kind {
        "budget_limit" => GoalContextKind::BudgetLimit,
        "objective_updated" => GoalContextKind::ObjectiveUpdated,
        _ => GoalContextKind::Continuation,
    }
}

/// `thinkingLevel` name as persisted in the session branch.
fn thinking_level_name(level: &ThinkingLevel) -> String {
    serde_json::to_value(level)
        .ok()
        .and_then(|value| value.as_str().map(|value| value.to_string()))
        .unwrap_or_else(|| format!("{level:?}").to_lowercase())
}

/// `serviceTier` name as persisted in the session branch.
fn service_tier_name(service_tier: &ServiceTier) -> String {
    serde_json::to_value(service_tier)
        .ok()
        .and_then(|value| value.as_str().map(|value| value.to_string()))
        .unwrap_or_else(|| format!("{service_tier:?}").to_lowercase())
}

/// `clampThinkingLevel(level, getSupportedThinkingLevels(model))`.
fn clamp_thinking_level_for_model(model: &Model, level: ThinkingLevel) -> ThinkingLevel {
    let available = get_supported_thinking_levels(model);
    if available.is_empty() || available.contains(&level) {
        return level;
    }
    available[0].clone()
}

/// `getModelInputLimit(model)`.
fn get_model_input_limit(model: &Model) -> f64 {
    model.context_window.unwrap_or(0.0)
}

/// `supportsFastMode(model)`.
fn supports_fast_mode(model: &Model) -> bool {
    model
        .service_tiers
        .as_ref()
        .map(|tiers| tiers.iter().any(|tier| tier == "priority"))
        .unwrap_or(false)
}

/// `modelsAreEqual(left, right)`.
fn models_are_equal(left: &Model, right: &Model) -> bool {
    left.provider == right.provider && left.id == right.id
}

/// `providerStreamFailureKind` retryability.
fn provider_stream_failure_kind_is_retryable(kind: &str) -> bool {
    !matches!(kind, "authentication" | "invalid_request" | "context_overflow")
}

/// `statusTextFromRestore(result)`.
fn status_text_from_restore(result: &RestoreResult) -> String {
    if result.restored {
        "Kernel state restored from the session snapshot.".to_string()
    } else {
        "Kernel state snapshot was not restored.".to_string()
    }
}

/// `sessionActionRecoveryOf(action)`.
fn session_action_recovery_of(action: &QueuedSessionAction) -> Option<SessionActionRecoveryAction> {
    let payload = match &action.payload {
        QueuedActionPayload::Turn(turn) => SessionActionRecoveryPayload::Turn {
            text: turn.base.text.clone(),
            preview: turn.base.preview.clone(),
            records: turn
                .base
                .records
                .iter()
                .map(|record| SessionActionRecoveryRecord {
                    id: record.id.clone(),
                    role: record.role,
                    message: serde_json::to_value(match &record.message {
                        DeliveryMessage::User(user) => {
                            serde_json::to_value(user).unwrap_or(Value::Null)
                        }
                        DeliveryMessage::Custom(custom) => {
                            serde_json::to_value(custom).unwrap_or(Value::Null)
                        }
                    })
                    .unwrap_or(Value::Null),
                    owner_action_id: record.owner_action_id.clone(),
                })
                .collect(),
            images: turn.images.clone(),
            content: turn.content.clone(),
            custom_message: turn
                .custom_message
                .as_ref()
                .and_then(|custom| serde_json::to_value(custom).ok()),
            queue_visible: turn.queue_visible,
            accepted_agent_message: turn.accepted_agent_message,
            accepted_before_completion: turn.accepted_before_completion,
        },
        QueuedActionPayload::SessionCommand(command) => SessionActionRecoveryPayload::SessionCommand {
            text: command.base.text.clone(),
            command: command.base.command.clone(),
            images: command.images.clone(),
        },
    };
    Some(SessionActionRecoveryAction {
        id: action.id.clone(),
        source: action.source.as_str().to_string(),
        delivery: action.delivery.as_str().to_string(),
        wake: action.wake.as_str().to_string(),
        queue_key: action.queue_key.clone(),
        agent_message_id: action.agent_message_id.clone(),
        suppress_autonomous_continuation: action.suppress_autonomous_continuation,
        payload,
    })
}

/// `emptyRlmChildRun(childId)` - a placeholder registry row used by the
/// deletion-failure path when no live run is retained.
fn empty_rlm_child_run(child_id: &str) -> RlmChildRun {
    RlmChildRun {
        id: child_id.to_string(),
        prompt: String::new(),
        session_name: String::new(),
        session_dir: String::new(),
        model: None,
        status: RLM_CHILD_AGENT_STATUS_ERROR.to_string(),
        duration_ms: None,
        answer_preview: None,
        tool_use_count: 0.0,
        activity: None,
        error: Some(String::new()),
        abort: Arc::new(|| {}),
        publication: create_agent_message_deferred(),
        settlement: create_agent_message_deferred(),
        session: None,
        settled: false,
        suppress_terminal_notice: None,
        abandoned_for_quiescence: None,
        detached_deletion: None,
        deletion_cleanup: None,
        deletion_cleanup_observer: None,
        deletion_reservation: create_agent_message_deferred(),
        deletion_cleanup_failed: None,
        deletion_run_finished: None,
        deletion_notice: None,
        deletion_failure_notice: None,
        deletion_needs_completion_notice: None,
        complete_deletion: None,
        report_deletion_cleanup_failure: None,
        emit_update: None,
        last_emitted_update: None,
        unsubscribe: None,
    }
}

/// `_sessionActionCommitContext` - one commit runs at a time per session.
pub struct CommitFence {
    pub owner: Option<String>,
    pub release: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl CommitFence {
    pub fn release(&self) {
        if let Some(release) = &self.release {
            release();
        }
    }
}

/// `acquireSessionInputPause()` result.
pub struct SessionInputPause {
    pub release: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl SessionInputPause {
    pub fn release(&self) {
        if let Some(release) = &self.release {
            release();
        }
    }
}

/// `acquireQueuedWorkPause()` result.
pub struct QueuedWorkPause {
    pub release: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl QueuedWorkPause {
    pub fn release(&self) {
        if let Some(release) = &self.release {
            release();
        }
    }
}

/// `clearQueue()` / `clearQueuedUserMessagesMatching()` result shape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClearedQueue {
    pub steering: Vec<String>,
    pub follow_up: Vec<String>,
}

/// `getUserMessagesForForking()` row.
#[derive(Debug, Clone, PartialEq)]
pub struct UserMessageForkEntry {
    pub entry_id: String,
    pub text: String,
}

/// `_ownUsageMemo` value.
#[derive(Debug, Clone, Default)]
pub struct OwnUsageMemo {
    pub count: i64,
    pub tail_id: Option<String>,
    pub usage: Usage,
}

/// `ReplacedSessionContext` - the extension-facing session handle.
#[derive(Clone)]
pub struct ReplacedSessionContext {
    pub send_message: Option<Arc<dyn Fn() -> BoxFuture<Result<(), String>> + Send + Sync>>,
    pub send_user_message: Option<Arc<dyn Fn() -> BoxFuture<Result<(), String>> + Send + Sync>>,
}

/// `_prepareForCommit` preparation policy.
#[derive(Debug, Clone, Default)]
pub struct SessionPreparationPolicy {
    pub initial_refine_barrier: String,
    pub after_validation: Option<Arc<dyn Fn() -> Result<(), String> + Send + Sync>>,
    pub run_before_agent_start: bool,
}

/// Result of the `prepare` leg of `_prepareForCommit`.
#[derive(Debug, Clone, Default)]
pub struct PreparedTurnActionState {
    pub prepared: bool,
}

/// `sessionPreparationPolicyFrom(turnExecutionPolicy)`.
fn session_preparation_policy_from(policy: &TurnExecutionPolicy) -> SessionPreparationPolicy {
    SessionPreparationPolicy {
        initial_refine_barrier: policy.preparation.initial_refine_barrier.clone(),
        after_validation: None,
        run_before_agent_start: policy.run_before_agent_start,
    }
}

/// `_createRlmSubagentRuntime` argument object.
#[derive(Debug, Clone, Default)]
pub struct RlmSubagentRuntimeOptionsInput {
    pub id: String,
    pub prompt: String,
    pub session_name: String,
    pub session_dir: String,
    pub model: Model,
    pub thinking_level: Option<ThinkingLevel>,
    pub spawn_code: Option<String>,
    pub spawned_by_request_id: Option<String>,
}

/// `ActionExecution` alias used by the appended members.
pub type ActionExecutionAlias = crate::core::session_action_store::ActionExecution;

/// A read of `self.agent.state().messages` for record bookkeeping.
fn current_messages_of(session: &Arc<AgentSession>) -> Vec<AgentMessage> {
    session.agent.state().messages
}

impl AgentSession {
    /// `compact(customInstructions, options)`.
    async fn compact(self: &Arc<Self>, custom_instructions: Option<&str>, skip_abort: bool) -> Result<crate::core::compaction::compaction::CompactionResult, String> {
        if skip_abort && self.is_streaming() {
            return Err("Cannot compact without aborting while the agent is running.".to_string());
        }
        let had_post_compaction_continue =
            self.post_compaction_continuation_scheduled.load(Ordering::SeqCst);
        let continue_after_session_input = self
            .post_compaction_continuation_settlement
            .lock()
            .unwrap()
            .as_ref()
            .map(|settlement| settlement.lock().unwrap().continue_after_session_input)
            .unwrap_or(false);
        self.disconnect_from_agent();
        if !skip_abort {
            self.abort().await?;
        }
        let mut did_compact = false;
        let compaction_abort = CancellationToken::new();
        *self.compaction_abort_controller.lock().unwrap() = Some(compaction_abort.clone());
        self.emit(AgentSessionEvent::CompactionStart {
            reason: COMPACTION_REASON_MANUAL.to_string(),
            custom_instructions: custom_instructions.map(|value| value.to_string()),
        });
        let outcome = self
            .perform_compaction_manual(custom_instructions, compaction_abort.clone())
            .await;
        *self.compaction_abort_controller.lock().unwrap() = None;
        self.reconnect_to_agent();
        self.notify_session_input_checkpoint_change();
        self.schedule_session_input_pump();
        match outcome {
            Ok(result) => {
                self.emit(AgentSessionEvent::CompactionEnd {
                    reason: COMPACTION_REASON_MANUAL.to_string(),
                    result: Some(result.clone()),
                    aborted: false,
                    will_retry: false,
                    error_message: None,
                    error_severity: None,
                    custom_instructions: custom_instructions.map(|value| value.to_string()),
                });
                did_compact = true;
                // A manual compaction satisfies any pending model request; on failure the
                // request stays scheduled for the next turn boundary.
                *self.pending_requested_compaction.lock().unwrap() = None;
                self.finish_successful_manual_compaction(
                    skip_abort,
                    had_post_compaction_continue,
                    continue_after_session_input,
                );
                Ok(result)
            }
            Err(error) => {
                let message = self.as_error(&error);
                let aborted = message == "Compaction cancelled" || error == COMPACTION_CANCELLED_ERROR_MESSAGE;
                let skipped = error == COMPACTION_SKIPPED_ERROR_MESSAGE;
                self.emit(AgentSessionEvent::CompactionEnd {
                    reason: COMPACTION_REASON_MANUAL.to_string(),
                    result: None,
                    aborted,
                    will_retry: false,
                    error_message: if aborted {
                        None
                    } else if skipped {
                        Some(message)
                    } else {
                        Some(format!("Compaction failed: {message}"))
                    },
                    error_severity: Some(if skipped { "warning" } else { "error" }.to_string()),
                    custom_instructions: custom_instructions.map(|value| value.to_string()),
                });
                Err(error)
            }
        }
    }

    /// The manual-compaction request. Split out so the `finally` block above can
    /// run on every exit path.
    async fn perform_compaction_manual(
        self: &Arc<Self>,
        custom_instructions: Option<&str>,
        signal: CancellationToken,
    ) -> Result<crate::core::compaction::compaction::CompactionResult, String> {
        let model = self.model();
        if model.id.is_empty() {
            return Err(format_no_model_selected_message());
        }
        let auth = self.get_required_request_auth(&model).await?;
        self.perform_compaction_unmeasured_full(
            Some(custom_instructions.map(|value| value.to_string())),
            signal,
            Some(auth),
        )
        .await
    }

    /// The tail of a successful manual compaction.
    fn finish_successful_manual_compaction(
        self: &Arc<Self>,
        skip_abort: bool,
        had_post_compaction_continue: bool,
        continue_after_session_input: bool,
    ) {
        if !skip_abort {
            self.resume_session_input_admission();
        }
        self.queue_pending_rlm_continuation();
        self.schedule_session_input_pump();
        self.discard_pending_auto_refine(true);
        if self.goal_state().status == GoalStatus::Active {
            if !self.agent.has_queued_messages() {
                self.goal_continuation_awaits_rlm_work
                    .store(true, Ordering::SeqCst);
            }
            self.resume_queued_work();
            if self.agent.has_queued_messages() {
                self.schedule_post_compaction_continue(false);
            }
        }
        if had_post_compaction_continue {
            self.schedule_post_compaction_continue(continue_after_session_input);
        }
        // Queued agent or session-owned inputs resume the loop; defer refine
        // behind them instead of interleaving it before their turns.
        self.schedule_auto_refine_after_compaction(
            self.goal_continuation_awaits_rlm_work.load(Ordering::SeqCst)
                || had_post_compaction_continue
                || self.agent.has_queued_messages()
                || self.unfinished_action_count() > 0,
        );
    }

    /// `_performCompactionUnmeasured(options)`.
    async fn perform_compaction_unmeasured(
        self: &Arc<Self>,
        custom_instructions: Option<String>,
        signal: CancellationToken,
    ) -> Result<(), String> {
        self.perform_compaction_unmeasured_full(custom_instructions, signal, None)
            .await
            .map(|_| ())
    }

    /// The shared compaction body.
    async fn perform_compaction_unmeasured_full(
        self: &Arc<Self>,
        custom_instructions: Option<String>,
        signal: CancellationToken,
        auth: Option<RequestAuth>,
    ) -> Result<crate::core::compaction::compaction::CompactionResult, String> {
        let model = self.model();
        let auth = match auth {
            Some(auth) => auth,
            None => self.get_required_request_auth(&model).await?,
        };
        let settings = default_compaction_settings();
        let entries = self.session_manager.lock().unwrap().get_branch(None);
        // `pathEntries` are the branch entries; this module reads them as the
        // compaction-module entry shapes.
        let path_entries: Vec<CompactionSessionEntry> = entries
            .iter()
            .filter_map(compaction_session_entry_from)
            .collect();
        let messages = self.messages();
        let preparation = prepare_compaction(
            &path_entries,
            &settings,
            &|_entries: &[CompactionSessionEntry]| messages.clone(),
        );
        let preparation = match preparation {
            Some(preparation) => preparation,
            None => return Err(COMPACTION_SKIPPED_ERROR_MESSAGE.to_string()),
        };
        if !should_compact_for_model(&model, &settings) {
            return Err(COMPACTION_SKIPPED_ERROR_MESSAGE.to_string());
        }
        let headers: Option<serde_json::Map<String, Value>> = if auth.headers.is_empty() {
            None
        } else {
            Some(
                auth.headers
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                    .collect(),
            )
        };
        let result = crate::core::compaction::compaction::compact(
            &preparation,
            &model,
            &auth.api_key,
            custom_instructions.as_deref(),
            Some(&signal),
            Some(&self.thinking_level()),
            crate::core::compaction::compaction::default_summary_call_runner(headers),
            None,
            None,
        )
        .await?;
        if signal.is_cancelled() {
            return Err(COMPACTION_CANCELLED_ERROR_MESSAGE.to_string());
        }
        self.session_manager.lock().unwrap().append_compaction(
            &result.summary,
            &result.first_kept_entry_id,
            result.tokens_before,
            result
                .details
                .as_ref()
                .and_then(|details| serde_json::to_value(details).ok()),
            None,
            custom_instructions.as_deref(),
            result.usage.as_ref(),
        )?;
        self.sync_kernel_state_after_compaction().await?;
        self.restore_provider_context_for_model();
        Ok(result)
    }


}

/// `_performCompaction` request auth.
#[derive(Debug, Clone, Default)]
pub struct RequestAuth {
    pub api_key: String,
    pub headers: indexmap::IndexMap<String, String>,
}

/// `_startRlmChildRun` argument object.
#[derive(Debug, Clone, Default)]
pub struct RlmChildRunOptions {
    pub prompt: String,
    pub session_name: Option<String>,
    pub session_dir: Option<String>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
}

/// `_getRequiredRequestAuth(model)` - the session asks the model registry for the
/// key and headers. A missing key is reported exactly like the TypeScript.
impl AgentSession {
    pub(crate) async fn get_required_request_auth(
        self: &Arc<Self>,
        model: &Model,
    ) -> Result<RequestAuth, String> {
        let registry = self.model_registry.clone();
        let resolved = {
            let mut registry = registry.lock().unwrap();
            registry.get_api_key_and_headers(model).await
        };
        if !resolved.ok {
            let error = resolved.error.clone().unwrap_or_default();
            if error.starts_with("No API key found") {
                return Err(format_no_api_key_found_message(&model.provider));
            }
            return Err(error);
        }
        if let Some(api_key) = resolved.api_key.clone() {
            return Ok(RequestAuth {
                api_key,
                headers: resolved.headers.clone(),
            });
        }
        let is_oauth = self.model_registry.lock().unwrap().is_using_oauth(model);
        if is_oauth {
            return Err(format_authentication_failed_message(&model.provider));
        }
        Err(format_no_api_key_found_message(&model.provider))
    }
}

/// `rlmHeartbeatHostResponse(job)` - the heartbeat row the host receives.
///
/// Field names and null-vs-absent choices follow the TypeScript exactly:
/// `label`, `next_run_at`, `last_run_at` and `last_error` are `null` when
/// absent (not omitted), and `delivery_mode` defaults to `"steer"`.
fn rlm_heartbeat_host_response(job: &AgentCronJob) -> Value {
    // `job.deliveryMode ?? "steer"`. The Rust alias is a `String`, so a plain
    // fallback matches the TypeScript default exactly.
    let delivery_mode = match &job.delivery_mode {
        Some(mode) if !mode.is_empty() => mode.clone(),
        _ => "steer".to_string(),
    };
    serde_json::json!({
        "id": job.id,
        "status": job.status,
        "label": job.label,
        "delivery_mode": delivery_mode,
        "instruction": job.prompt,
        "schedule": job.schedule,
        "created_at": job.created_at,
        "updated_at": job.updated_at,
        "next_run_at": job.next_run_at,
        "last_run_at": job.last_run_at,
        "last_error": job.last_error,
        "run_count": job.run_count,
    })
}

/// `pathEntries` -> the compaction module's entry shapes.
fn compaction_session_entry_from(entry: &SessionEntry) -> Option<CompactionSessionEntry> {
    let entry_type = entry.get("type").and_then(Value::as_str)?;
    let id = entry.get("id").and_then(Value::as_str)?.to_string();
    let parent_id = entry
        .get("parentId")
        .and_then(Value::as_str)
        .map(|value| value.to_string());
    match entry_type {
        "message" => {
            let message = entry.get("message")?;
            Some(CompactionSessionEntry::Message {
                id,
                parent_id,
                message: agent_message_from_value(message),
            })
        }
        "custom" => {
            let message = entry.get("message")?;
            Some(CompactionSessionEntry::CustomMessage {
                id,
                parent_id,
                custom_type: message
                    .get("customType")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                content: serde_json::from_value(message.get("content").cloned().unwrap_or(Value::Null))
                    .unwrap_or(CustomMessageContent::Text(String::new())),
                details: message.get("details").cloned(),
                display: message
                    .get("display")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                timestamp: message
                    .get("timestamp")
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            })
        }
        "branch_summary" => Some(CompactionSessionEntry::BranchSummary {
            id,
            parent_id,
            from_id: entry
                .get("fromId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            summary: entry
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            details: entry.get("details").cloned(),
            from_hook: entry.get("fromHook").and_then(Value::as_bool),
            timestamp: entry
                .get("timestamp")
                .map(|value| value.to_string())
                .unwrap_or_default(),
        }),
        "compaction" => Some(CompactionSessionEntry::Compaction {
            id,
            parent_id,
            summary: entry
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            first_kept_entry_id: entry
                .get("firstKeptEntryId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            tokens_before: entry
                .get("tokensBefore")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            details: entry.get("details").cloned(),
            from_hook: entry.get("fromHook").and_then(Value::as_bool),
            timestamp: entry
                .get("timestamp")
                .map(|value| value.to_string())
                .unwrap_or_default(),
        }),
        other => Some(CompactionSessionEntry::Other {
            id,
            parent_id,
            entry_type: other.to_string(),
        }),
    }
}

impl AgentSession {

    /// `_runSerializedRefineCheckpointAfterBackground(branchVersion)`.
    async fn run_serialized_refine_checkpoint_after_background(self: &Arc<Self>, branch_version: u64) {
        if self.auto_refine_branch_version.load(Ordering::SeqCst) != branch_version {
            return;
        }
        let _ = self.maybe_auto_refine("turn_interval").await;
    }

    /// `_runSerializedAutoRefineReview(reason, branchVersion)`.
    async fn run_serialized_auto_refine_review(self: &Arc<Self>, reason: &str, branch_version: u64) {
        if self.auto_refine_branch_version.load(Ordering::SeqCst) != branch_version {
            return;
        }
        let _ = self.maybe_auto_refine(reason).await;
    }

    /// `_acquireRlmTerminalNoticeRetentionFence()`.
    async fn acquire_rlm_terminal_notice_retention_fence(self: &Arc<Self>) -> Option<CommitFence> {
        if self.disposed.load(Ordering::SeqCst) || self.disposing.load(Ordering::SeqCst) {
            return None;
        }
        self.acquire_session_action_commit_fence().await.ok()
    }

    /// `_coalescedFollowUpOwner(action)`.
    fn coalesced_follow_up_owner(&self, action: &QueuedSessionAction) -> Option<QueuedSessionAction> {
        let queue_key = action.queue_key.clone()?;
        self.action_store
            .lock()
            .unwrap()
            .queued_actions(Some(DeliveryPolicy::WhenRunIdle))
            .into_iter()
            .find(|candidate| {
                candidate.id != action.id && candidate.queue_key.as_deref() == Some(queue_key.as_str())
            })
    }

    /// `_queuePreparedPrompt(schedule, text, images, options)`.
    async fn queue_prepared_prompt(
        self: &Arc<Self>,
        schedule: &str,
        text: &str,
        images: Option<Vec<ImageContent>>,
        options: Option<PreparedTurnActionOptions>,
    ) -> bool {
        let action = self.create_prepared_turn_action(schedule, text, images, options);
        if action.suppress_autonomous_continuation.unwrap_or(false) {
            if let Ok(record) = primary_delivery_record(&action) {
                self.mark_autonomous_continuation_suppressed(&agent_message_from_delivery(&record.message));
            }
        }
        self.admit_session_input(action, false).is_ok()
    }
}

// PORT CURSOR: TS line 13194 (end of agent-session.ts; last member ported: extensionRunner). FILE COMPLETE.

impl AgentSession {
    /// `_actionStore` action state lookup by id.
    fn action_state_of(&self, action_id: &str) -> Option<ActionLifecycleState> {
        self.action_store
            .lock()
            .unwrap()
            .owned_actions()
            .into_iter()
            .find(|action| action.id == action_id)
            .map(|action| action.lifecycle.state())
    }

    /// `_markDeliveryRecordDurable(action, transcript)`: the action's primary
    /// delivery record becomes durable once its message is in the transcript.
    fn mark_delivery_record_durable(&self, action: &QueuedSessionAction, transcript: &[AgentMessage]) {
        let Ok(primary) = primary_delivery_record(action) else {
            return;
        };
        if !transcript.contains(&agent_message_from_delivery(&primary.message)) {
            return;
        }
        let mut next = action.clone();
        if let QueuedActionPayload::Turn(turn) = &mut next.payload {
            for record in turn.base.records.iter_mut() {
                if record.id == primary.id {
                    record.durable = true;
                }
            }
        }
        let _ = self.action_store.lock().unwrap().update_action(&next);
    }

    /// `record.durable ||= delivered.has(record.message)` for every record.
    fn mark_matching_records_durable(
        &self,
        action: &QueuedSessionAction,
        delivered: &HashSet<String>,
    ) {
        let mut next = action.clone();
        if let QueuedActionPayload::Turn(turn) = &mut next.payload {
            for record in turn.base.records.iter_mut() {
                if delivered.contains(&delivery_message_key_of(&record.message)) {
                    record.durable = true;
                }
            }
        }
        let _ = self.action_store.lock().unwrap().update_action(&next);
    }

    /// The dispatch-failure record filter from `_startPreparedTurnActions`:
    /// keep undelivered prefix records, keep delivered next-turn records,
    /// keep everything else.
    fn filter_records_after_dispatch_failure(&self, action: &QueuedSessionAction) {
        let mut next = action.clone();
        if let QueuedActionPayload::Turn(turn) = &mut next.payload {
            turn.base.records.retain(|record| match record.role {
                DeliveryRecordRole::Prefix => !record.durable,
                DeliveryRecordRole::NextTurn => record.durable,
                DeliveryRecordRole::Primary => true,
            });
        }
        let _ = self.action_store.lock().unwrap().update_action(&next);
    }

    /// `action.payload.records = records.filter((record) => record.role !== "next_turn")`.
    fn strip_next_turn_records(&self, action_id: &str) {
        let Some(action) = self
            .action_store
            .lock()
            .unwrap()
            .owned_actions()
            .into_iter()
            .find(|action| action.id == action_id)
        else {
            return;
        };
        let mut next = action.clone();
        if let QueuedActionPayload::Turn(turn) = &mut next.payload {
            turn.base
                .records
                .retain(|record| record.role != DeliveryRecordRole::NextTurn);
        }
        let _ = self.action_store.lock().unwrap().update_action(&next);
    }

    /// `await this._agentEventQueue`.
    ///
    /// The queued future lives inside the mutex, so it is moved out, awaited and
    /// then stored back; an empty slot uses the completed unit future.
    async fn await_agent_event_queue(&self) {
        let queued = { self.agent_event_queue.lock().unwrap().take() };
        let Some(queued) = queued else {
            return;
        };
        let _ = queued.await;
        let mut slot = self.agent_event_queue.lock().unwrap();
        if slot.is_none() {
            *slot = Some(Box::pin(async { Ok(()) }));
        }
    }

    /// `this._agentEventQueue = this._agentEventQueue.then(task, task)`.
    fn push_agent_event_task<F>(self: &Arc<Self>, task: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let previous = self.agent_event_queue.lock().unwrap().take();
        let next: BoxFuture<Result<(), String>> = Box::pin(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            task.await;
            Ok(())
        });
        *self.agent_event_queue.lock().unwrap() = Some(next);
    }

    /// `_maybeStartSerializedBackgroundPlan()`.
    fn maybe_start_serialized_background_plan(self: &Arc<Self>) {
        if !self.serialized_refine
            || self.disposed.load(Ordering::SeqCst)
            || self.disposing.load(Ordering::SeqCst)
        {
            return;
        }
        if self.serialized_plan_in_flight.lock().unwrap().is_some()
            || self.refine_in_flight.lock().unwrap().is_some()
            || self.refine_plan_in_flight.lock().unwrap().is_some()
        {
            return;
        }
        let pending = self.pending_requested_refine.lock().unwrap().take();
        if let Some(pending) = pending {
            let options = RefineOptions {
                instructions: pending.instructions,
                rollback_id: None,
                global: pending.global,
                retry: None,
                evidence: None,
                max_output_tokens: None,
            };
            *self.serialized_explicit_refine_options.lock().unwrap() = Some(options);
            let branch_version = self.auto_refine_branch_version.load(Ordering::SeqCst);
            let session = self.clone();
            *self.serialized_plan_in_flight.lock().unwrap() =
                Some(Box::pin(async move { session.run_background_plan(branch_version, true).await }));
            return;
        }
        if !self.auto_refine_allowed_for_session() {
            return;
        }
        let settings = self.settings_manager.lock().unwrap().get_auto_refine_settings();
        if !settings.enabled {
            return;
        }
        if (self.assistant_turns_since_auto_refine.load(Ordering::SeqCst) as f64) < settings.turn_interval {
            return;
        }
        let now = now_ms();
        let last = *self.last_auto_refine_review_at.lock().unwrap();
        let under_cooldown = last > 0.0 && now - last < settings.cooldown_ms;
        if under_cooldown {
            return;
        }
        let branch_version = self.auto_refine_branch_version.load(Ordering::SeqCst);
        let session = self.clone();
        *self.serialized_plan_in_flight.lock().unwrap() =
            Some(Box::pin(async move { session.run_background_plan(branch_version, false).await }));
    }

    /// `_runBackgroundPlan(options, refineAbort, branchVersion, skipReview)`.
    async fn run_background_plan(
        self: &Arc<Self>,
        branch_version: u64,
        skip_review: bool,
    ) -> Result<Option<SerializedBackgroundPlanResult>, String> {
        if !skip_review {
            let context = AutoRefineReviewContext {
                reason: AutoRefineReason::TurnInterval,
                turns_since_last_review: self.assistant_turns_since_auto_refine.load(Ordering::SeqCst)
                    as i64,
            };
            let review = self.review_auto_refine(&context).await;
            if self.disposed.load(Ordering::SeqCst)
                || self.disposing.load(Ordering::SeqCst)
                || branch_version != self.auto_refine_branch_version.load(Ordering::SeqCst)
            {
                return Ok(Some(SerializedBackgroundPlanResult::Invalidated {
                    branch_version,
                }));
            }
            let review = match review {
                Ok(review) => review,
                Err(_) => {
                    return Ok(Some(SerializedBackgroundPlanResult::Failure {
                        explicit: false,
                        branch_version,
                    }))
                }
            };
            if !review.should_refine {
                return Ok(Some(SerializedBackgroundPlanResult::Skip { explicit: false }));
            }
            *self.serialized_explicit_refine_options.lock().unwrap() = Some(RefineOptions {
                instructions: Some(auto_refine_instructions(
                    &AutoRefineReason::TurnInterval,
                    &review,
                )),
                rollback_id: None,
                global: None,
                retry: None,
                evidence: None,
                max_output_tokens: None,
            });
        }
        let options = self
            .serialized_explicit_refine_options
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default();
        let plan = self.plan_refine_with_options(&options).await;
        if self.disposed.load(Ordering::SeqCst)
            || self.disposing.load(Ordering::SeqCst)
            || branch_version != self.auto_refine_branch_version.load(Ordering::SeqCst)
        {
            return Ok(Some(SerializedBackgroundPlanResult::Invalidated {
                branch_version,
            }));
        }
        match plan {
            Ok(plan) => Ok(Some(SerializedBackgroundPlanResult::Plan {
                plan,
                branch_version,
                source: if skip_review {
                    RefinementSource::SelfOnly
                } else {
                    RefinementSource::Auto
                },
            })),
            Err(_) => Ok(Some(SerializedBackgroundPlanResult::Failure {
                explicit: skip_review,
                branch_version,
            })),
        }
    }

    /// `_ensureHarnessDigestContext()`.
    fn ensure_harness_digest_context(&self) {
        if self.agent.state().messages.is_empty() {
            self.harness_digest_pending.store(true, Ordering::SeqCst);
            return;
        }
        self.harness_digest_pending.store(false, Ordering::SeqCst);
        self.append_harness_digest_if_stale();
    }

    /// `_appendDurableStatusMessage(message)`.
    fn append_durable_status_message(&self, message: CustomMessage) -> Result<(), String> {
        self.append_durable_refine_message(&message);
        Ok(())
    }

    /// `settingsManager.getCompactionSettings()`, adapted to the compaction module shape.
    fn compaction_settings(&self) -> CompactionSettings {
        let resolved = self.settings_manager.lock().unwrap().get_compaction_settings();
        CompactionSettings {
            enabled: resolved.enabled,
            reserve_tokens: resolved.reserve_tokens,
            keep_recent_tokens: resolved.keep_recent_tokens,
            summary_update_policy: resolved.summary_update_policy.clone(),
        }
    }

    /// `_getThresholdContextTokens(assistantMessage, compactionTimestamp)`.
    fn threshold_context_tokens(&self) -> Option<f64> {
        let messages = self.agent.state().messages;
        let estimate = estimate_context_tokens(&messages);
        if let Some(index) = estimate.last_usage_index {
            let usage_message = messages.get(index)?;
            if let AgentMessage::Message(pi_ai::types::Message::Assistant(usage_message)) = usage_message {
                let rebuilt_at = *self.provider_context_rebuilt_at.lock().unwrap();
                let model = self.agent.state().model;
                if (rebuilt_at.is_some() && usage_message.timestamp as f64 <= rebuilt_at.unwrap_or(0.0))
                    || usage_message.model != model.id
                    || usage_message.provider != model.provider
                {
                    return Some(messages.iter().map(estimate_tokens).sum());
                }
                let compaction_timestamp = self.active_compaction_timestamp();
                if let Some(compaction_timestamp) = compaction_timestamp {
                    if (usage_message.timestamp as f64) <= compaction_timestamp {
                        return None;
                    }
                }
            }
            return Some(estimate.tokens);
        }
        None
    }
}

