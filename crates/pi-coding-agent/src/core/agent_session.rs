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

use crate::core::agent_messages::{
    assert_agent_message_queue_capacity, assert_agent_session_name_available,
    assert_direct_agent_message_target, create_agent_message_host_handlers,
    format_agent_session_name_unavailable, is_agent_session_message,
    is_agent_session_message_prompt, normalize_agent_session_message,
    parse_agent_session_message_prompt_id, starts_agent_run, AgentFamilyCatalogEntry,
    AgentSessionMessageListResult, AgentSessionMessageReceipt, AgentSessionNameAvailabilityInput,
    AgentSessionNameScope, DEFAULT_AGENT_MESSAGE_MAX_PENDING_PER_SESSION,
    AGENT_MESSAGE_CUSTOM_TYPE, AGENT_MESSAGE_RECEIVED_PREVIEW_LABEL,
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
    GoalHostResponse, GoalState, GoalStatus, GOAL_CONTEXT_CUSTOM_TYPE, GOAL_CONTEXT_PREVIEW_LABEL,
    GOAL_SKILL_NAME, GOAL_STATE_CUSTOM_TYPE,
};
use crate::core::messages::{
    convert_to_llm, create_async_bash_completion_message, create_compaction_outcome_message,
    create_custom_message, create_harness_digest_message, create_heartbeat_prompt_message,
    create_refinement_notice_message, create_refinement_outcome_message,
    create_rlm_child_failure_message, create_rlm_child_terminal_notice_message,
    create_session_slash_command_message, create_session_slash_command_result_message,
    is_compaction_outcome_message, is_session_slash_command, is_session_slash_command_message,
    without_harness_digests_for_compaction, AsyncBashCompletionDetails, BashExecutionMessage,
    CompactionOutcome, CompactionOutcomeDetails, CompactionOutcomeReason, CustomMessage,
    HarnessDigestDetails, RefinementSource, RlmChildFailureDetails, RlmChildTerminalNoticeDetails,
    ASYNC_BASH_COMPLETION_CUSTOM_TYPE, ASYNC_BASH_COMPLETION_PREVIEW_LABEL,
    HARNESS_DIGEST_CUSTOM_TYPE, HEARTBEAT_PROMPT_CUSTOM_TYPE, HEARTBEAT_PROMPT_PREVIEW_LABEL,
    IPYTHON_STATE_RESTORED_CUSTOM_TYPE, RLM_CHILD_FAILURE_CUSTOM_TYPE,
    RLM_CHILD_TERMINAL_NOTICE_CUSTOM_TYPE, SESSION_SLASH_COMMAND_CUSTOM_TYPE,
    SESSION_SLASH_COMMAND_RESULT_CUSTOM_TYPE,
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
    AutoRefineReason, AutoRefineReview, AutoRefineReviewContext, HarnessState, PlanRefinementRequest,
    RefinementFailureError, RefinementPlan, RefinementResult, RefineOptions, REFINE_SKILL_NAME,
    REFINEMENT_FAILURE_CUSTOM_TYPE,
};
use crate::core::session_action_store::{
    can_select_session_action, queued_message_lane_delivery_policy, transition_session_action,
    ActionLifecycle, ActionStore, ActionTicket, DeliveryMessage, DeliveryPolicy, DeliveryRecord,
    DeliveryRecordRole, QueuedMessageLane, QueuedMessageMutation, QueuedMessageMutationStatus,
    RuntimeActivity, SessionAction, SessionActionPayload, SessionActionSnapshot, SessionCommandPayload,
    SessionTurnPayload, WakePolicy,
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

// ---------------------------------------------------------------------------
// Private plumbing for cross-slice seams
//
// Every item below replaces an import from another slice. Names follow the
// TypeScript member names so the mapping stays recognisable.
// ---------------------------------------------------------------------------

/// `Agent` from `@earendil-works/pi-agent-core`.
///
/// The agent module is another slice. The session only uses this surface, so the
/// seam is kept minimal and mirrors the exact TypeScript members it calls.
pub trait AgentHandle: Send + Sync {
    fn state(&self) -> AgentState;
    fn set_state(&self, state: AgentState);
    /// `agent.subscribe(listener)` - returns the unsubscribe function.
    fn subscribe(&self, listener: Arc<dyn Fn(AgentEvent) + Send + Sync>) -> Box<dyn Fn() + Send + Sync>;
    fn set_before_tool_call(&self, hook: BeforeToolCallHook);
    fn set_after_tool_call(&self, hook: AfterToolCallHook);
    fn set_get_continuation_messages(&self, hook: GetContinuationMessagesHook);
    fn set_should_stop_before_turn(&self, hook: Arc<dyn Fn() -> bool + Send + Sync>);
    fn set_should_stop_after_turn(&self, hook: Arc<dyn Fn(ShouldStopAfterTurnContext) -> bool + Send + Sync>);
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
    fn set_convert_to_llm(&self, convert: Arc<dyn Fn(Vec<AgentMessage>) -> Vec<Message> + Send + Sync>);
    fn set_transform_context(&self, transform: Arc<dyn Fn(Vec<AgentMessage>, Option<CancellationToken>) -> Vec<AgentMessage> + Send + Sync>);
    fn set_get_api_key(&self, get_api_key: Arc<dyn Fn(String) -> Option<String> + Send + Sync>);
    fn set_on_payload(&self, hook: Arc<dyn Fn(Value) + Send + Sync>);
    fn set_on_response(&self, hook: Arc<dyn Fn(Value) + Send + Sync>);
    fn set_tool_execution(&self, mode: String);
    fn performance_metrics(&self) -> Option<Arc<dyn PerformanceMetricRecorder>>;
    fn set_performance_metrics(&self, metrics: Option<Arc<dyn PerformanceMetricRecorder>>);
    fn signal(&self) -> Option<CancellationToken>;
}

/// `Agent.beforeToolCall` payload.
#[derive(Debug, Clone)]
pub struct BeforeToolCallContext {
    pub tool_call: pi_ai::types::ToolCall,
    pub args: Value,
}

/// `Agent.afterToolCall` payload.
#[derive(Debug, Clone)]
pub struct AfterToolCallContext {
    pub tool_call: pi_ai::types::ToolCall,
    pub args: Value,
    pub result: pi_agent_core::types::AgentToolResult,
    pub is_error: bool,
}

/// `BeforeToolCallResult`: `undefined` means "no override".
pub type BeforeToolCallResult = Option<Value>;
pub type AfterToolCallResult = Option<pi_agent_core::types::AgentToolResult>;

pub type BeforeToolCallHook =
    Arc<dyn Fn(BeforeToolCallContext) -> BoxFuture<Result<BeforeToolCallResult, String>> + Send + Sync>;
pub type AfterToolCallHook =
    Arc<dyn Fn(AfterToolCallContext) -> BoxFuture<Result<AfterToolCallResult, String>> + Send + Sync>;

/// `ShouldStopAfterTurnContext` from pi-agent-core.
#[derive(Debug, Clone, Default)]
pub struct ShouldStopAfterTurnContext {
    pub turn_index: i64,
    pub messages: Vec<AgentMessage>,
}

pub type GetContinuationMessagesHook = Arc<
    dyn Fn(AgentContext, Option<CancellationToken>) -> BoxFuture<Result<Vec<AgentMessage>, String>>
        + Send
        + Sync,
>;

pub type BoxFuture<T> = pi_ai::types::BoxFuture<T>;

/// `PerformanceMetricRecorder` from pi-agent-core.
pub trait PerformanceMetricRecorder: Send + Sync {
    fn monotonic_now(&self) -> Option<f64>;
}

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

/// `ExtensionRunner` from `core/extensions/runner.ts` (another slice).
pub trait ExtensionRunner: Send + Sync {
    fn has_handlers(&self, event_type: &str) -> bool;
    fn emit_tool_call(&self, tool_name: &str, tool_call_id: &str, input: Value) -> BoxFuture<Result<Option<Value>, String>>;
    fn emit_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: &str,
        input: Value,
        content: Vec<pi_agent_core::types::ContentBlock>,
        details: Value,
        is_error: bool,
    ) -> BoxFuture<Result<Option<ToolResultHookResult>, String>>;
    fn emit_before_agent_start(
        &self,
        prompt: String,
        images: Option<Vec<ImageContent>>,
        system_prompt: String,
        system_prompt_options: BuildSystemPromptOptions,
    ) -> BoxFuture<Result<BeforeAgentStartResult, String>>;
    fn emit_session_shutdown(&self, reason: &str) -> bool;
    fn emit_before_compact(&self, preparation: Value) -> BoxFuture<Result<Option<Value>, String>>;
    fn emit_before_refine(&self, request: Value) -> BoxFuture<Result<Option<Value>, String>>;
    fn emit_before_tree(&self, preparation: Value) -> BoxFuture<Result<Option<Value>, String>>;
    fn emit_resources_discover(&self, cwd: &str, reason: &str);
    fn emit_input(&self, text: &str, images: Option<Vec<ImageContent>>, source: &str);
    fn emit_message_end(&self, event: Value);
    fn emit_user_bash(&self, event: Value);
    fn emit_context(&self, messages: Vec<Value>);
    fn emit_before_provider_request(&self, payload: Value);
    fn get_all_registered_tools(&self) -> Vec<RegisteredExtensionTool>;
    fn get_tool_definition(&self, name: &str) -> Option<crate::core::extensions::types::ToolDefinition<Value>>;
    fn get_command(&self, name: &str) -> Option<ExtensionCommand>;
    fn resolve_registered_commands(&self) -> Vec<ExtensionCommand>;
    fn get_shortcuts(&self, resolved_keybindings: &Map<String, Value>) -> Vec<Value>;
    fn set_ui_context(&self, ui_context: Option<Value>);
    fn bind_command_context(&self, actions: Option<Value>);
    fn assert_active(&self) -> Result<(), String>;
    fn invalidate(&self, message: Option<String>);
    fn get_extension_paths(&self) -> Vec<String>;
    fn shutdown(&self) -> BoxFuture<()>;
}

/// `hookResult` from `ExtensionRunner.emitToolResult`.
#[derive(Debug, Clone)]
pub struct ToolResultHookResult {
    pub content: Vec<pi_agent_core::types::ContentBlock>,
    pub details: Value,
    pub is_error: Option<bool>,
}

/// `Awaited<ReturnType<ExtensionRunner["emitBeforeAgentStart"]>>`.
#[derive(Debug, Clone, Default)]
pub struct BeforeAgentStartResult {
    pub prompt: Option<String>,
    pub images: Option<Vec<ImageContent>>,
    pub system_prompt: Option<String>,
}

/// `getAllRegisteredTools()` entry.
#[derive(Debug, Clone)]
pub struct RegisteredExtensionTool {
    pub definition: crate::core::extensions::types::ToolDefinition<Value>,
}

/// `RegisteredCommand` shape used by the session.
#[derive(Debug, Clone, Default)]
pub struct ExtensionCommand {
    pub name: String,
    pub description: Option<String>,
    pub source_info: Option<SourceInfo>,
    pub handler: Option<Arc<dyn Fn(String) -> BoxFuture<Result<(), String>> + Send + Sync>>,
}

/// `ExtensionRunnerRef` from `AgentSessionConfig`.
#[derive(Default)]
pub struct ExtensionRunnerRef {
    current: Mutex<Option<Arc<dyn ExtensionRunner>>>,
}

impl ExtensionRunnerRef {
    pub fn new(runner: Option<Arc<dyn ExtensionRunner>>) -> Self {
        Self {
            current: Mutex::new(runner),
        }
    }

    pub fn current(&self) -> Option<Arc<dyn ExtensionRunner>> {
        self.current.lock().unwrap().clone()
    }

    pub fn set(&self, runner: Option<Arc<dyn ExtensionRunner>>) {
        *self.current.lock().unwrap() = runner;
    }
}

/// `ResourceLoader` from `core/resource-loader.ts` (another slice).
pub trait ResourceLoader: Send + Sync {
    fn get_extensions(&self) -> Vec<ResourceExtensionPaths>;
    fn get_skills(&self) -> Vec<crate::core::skills::Skill>;
    fn get_prompt_templates(&self) -> Vec<PromptTemplate>;
    fn get_context_files(&self) -> Vec<crate::core::system_prompt::ContextFile>;
    fn get_system_prompt_override(&self) -> Option<String>;
    fn get_append_system_prompt(&self) -> Option<String>;
    fn reload(&self) -> BoxFuture<()>;
}

/// `ResourceExtensionPaths` from `core/resource-loader.ts`.
#[derive(Debug, Clone, Default)]
pub struct ResourceExtensionPaths {
    pub path: String,
    pub extension_path: String,
}

/// `McpManager` from `core/mcp/mcp-manager.ts` (another slice).
pub trait McpManager: Send + Sync {
    fn refresh(&self);
    fn replace_acp_servers(&self, servers: &[crate::core::mcp::acp_mcp_types::AcpMcpServerConfig], owner_id: &str) -> bool;
    fn can_release_acp_servers(&self, owner_id: &str) -> bool;
    fn get_acp_servers(&self) -> Vec<crate::core::mcp::acp_mcp_types::AcpMcpServerConfig>;
}

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

/// `AgentObserveController` from `core/agent-observe.ts` (another slice).
pub trait AgentObserveController: Send + Sync {
    fn list_agents(&self, payload: Value) -> BoxFuture<Result<AgentObserveListResult, String>>;
    fn recent_messages(&self, payload: Value) -> BoxFuture<Result<AgentObserveRecentMessagesResult, String>>;
    fn snapshot(&self, payload: Value) -> BoxFuture<Result<AgentObserveAgentSnapshot, String>>;
}

/// `SubagentRuntimeHost` from `core/rlm-runtime.ts` (another slice).
pub trait SubagentRuntimeHost: Send + Sync {
    fn create_subagent_runtime(&self, options: CreateRlmSubagentRuntimeOptions) -> BoxFuture<Result<RlmSubagentRuntime, String>>;
    fn list_subagents(&self) -> BoxFuture<Result<Value, String>>;
    fn delete_subagent(&self, selector: &str) -> BoxFuture<Result<Value, String>>;
    fn find_models(&self, query: &str, limit: i64) -> BoxFuture<Result<Value, String>>;
    fn run(&self, payload: Value) -> BoxFuture<Result<Value, String>>;
    fn create_session(&self, payload: Value) -> BoxFuture<Result<Value, String>>;
}

/// `RlmSubagentRuntime` from `core/rlm-runtime.ts`.
pub struct RlmSubagentRuntime {
    pub handle: RlmSpawnHandle,
    pub runtime: Value,
}

/// `RlmSpawnHandle` from `core/rlm-runtime.ts`.
#[derive(Debug, Clone, Default)]
pub struct RlmSpawnHandle {
    pub child_id: String,
    pub session_name: String,
    pub session_dir: String,
    pub active_session_id: Option<String>,
}

/// `CreateRlmSubagentRuntimeOptions` from `core/rlm-runtime.ts`.
#[derive(Debug, Clone, Default)]
pub struct CreateRlmSubagentRuntimeOptions {
    pub prompt: String,
    pub session_name: String,
    pub session_dir: String,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
    pub parent_session_id: Option<String>,
    pub parent_session_path: Option<String>,
    pub inline: bool,
}

/// `RlmSubagentRegistryEntry` from `core/rlm-runtime.ts`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RlmSubagentRegistryEntry {
    pub id: String,
    pub session_name: String,
    pub session_dir: String,
    pub model: Option<String>,
    pub status: String,
}

/// `RlmCreateSessionResult`, `RlmDeleteSubagentResult`, `RlmFindModelsResult`,
/// `RlmListSubagentsResult` from `core/rlm-runtime.ts`.
#[derive(Debug, Clone, Default)]
pub struct RlmCreateSessionResult {
    pub child_id: String,
    pub session_name: String,
    pub session_dir: String,
    pub value: Value,
}

#[derive(Debug, Clone, Default)]
pub struct RlmDeleteSubagentResult {
    pub deleted: bool,
    pub value: Value,
}

#[derive(Debug, Clone, Default)]
pub struct RlmFindModelsResult {
    pub matches: Vec<Value>,
    pub value: Value,
}

#[derive(Debug, Clone, Default)]
pub struct RlmListSubagentsResult {
    pub agents: Vec<RlmChildAgentSnapshot>,
    pub value: Value,
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
#[derive(Debug, Clone)]
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
    pub capture_run_messages: Option<HashSet<usize>>,
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
    pub definition: crate::core::extensions::types::ToolDefinition<Value>,
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

/// `BashResult` from `core/bash-executor.ts` (another slice).
#[derive(Debug, Clone, Default)]
pub struct BashResult {
    pub output: String,
    pub exit_code: Option<i64>,
    pub cancelled: bool,
    pub truncated: bool,
    pub full_output_path: Option<String>,
}

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
    pub custom_tools: Option<Vec<crate::core::extensions::types::ToolDefinition<Value>>>,
    pub model_registry: Arc<Mutex<crate::core::model_registry::ModelRegistry>>,
    pub initial_active_tool_names: Option<Vec<String>>,
    pub allowed_tool_names: Option<Vec<String>>,
    pub include_goals: Option<bool>,
    pub agent_message_controller: Option<Arc<dyn AgentSessionMessageController>>,
    pub agent_observe_controller: Option<Arc<dyn AgentObserveController>>,
    pub include_compact_skill: Option<bool>,
    pub rlm_heartbeat_controller: Option<Arc<dyn AgentRlmHeartbeatController>>,
    pub mcp_manager: Option<Arc<dyn McpManager>>,
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

/// `scopedModels` entry.
#[derive(Clone)]
pub struct ScopedModel {
    pub model: Model,
    pub thinking_level: Option<ThinkingLevel>,
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
    match value {
        Some(value) if value.is_finite() => Some(value),
        _ => None,
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
        None => return promise.await.map_err(|error| error.to_string()),
        Some(signal) => signal.clone(),
    };
    if signal.is_cancelled() {
        return Err(abort_message.to_string());
    }
    tokio::select! {
        value = promise => value.map_err(|error| error.to_string()),
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

    service_tier_preference: ServiceTier,
    scoped_models: Vec<ScopedModel>,
    event_listeners: Mutex<Vec<AgentSessionEventListener>>,
    last_session_action_snapshot: Mutex<SessionActionSnapshot>,
    agent_event_queue: Mutex<BoxFuture<Result<(), String>>>,
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
    custom_tools: Vec<crate::core::extensions::types::ToolDefinition<Value>>,
    acp_mcp_tools: Mutex<Vec<crate::core::extensions::types::ToolDefinition<Value>>>,
    base_tool_definitions: Mutex<BTreeMap<String, crate::core::extensions::types::ToolDefinition<Value>>>,
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
    mcp_manager: Option<Arc<dyn McpManager>>,
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
            Some(session_artifact_dir.as_str()),
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
            service_tier_preference,
            scoped_models: config.scoped_models.unwrap_or_default(),
            event_listeners: Mutex::new(Vec::new()),
            last_session_action_snapshot: Mutex::new(SessionActionSnapshot {
                queued_count: 0,
                steering: Vec::new(),
                follow_ups: Vec::new(),
                active: None,
            }),
            agent_event_queue: Mutex::new(Box::pin(async { Ok(()) })),
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
            manager.refresh();
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
        if !manager.replace_acp_servers(servers, owner_id) {
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
        if !manager.can_release_acp_servers(owner_id) {
            return Ok(());
        }
        if manager.replace_acp_servers(&[], owner_id) {
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
            self.agent_event_queue.lock().unwrap().await?;
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
            .map(|manager| manager.get_acp_servers())
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

    /// `_getRequiredRequestAuth`.
    async fn get_required_request_auth(&self, model: &Model) -> Result<RequestAuth, String> {
        let result = {
            let mut registry = self.model_registry.lock().unwrap();
            registry.get_api_key_and_headers(model)
        };
        if !result.ok {
            let error = result.error.clone().unwrap_or_default();
            if error.starts_with("No API key found") {
                return Err(format_no_api_key_found_message(&model.provider));
            }
            return Err(error);
        }
        if let Some(api_key) = result.api_key.clone() {
            return Ok(RequestAuth {
                api_key,
                headers: result
                    .headers
                    .as_ref()
                    .map(|headers| headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
            });
        }

        let is_oauth = self
            .model_registry
            .lock()
            .unwrap()
            .is_using_oauth(model);
        if is_oauth {
            return Err(format_authentication_failed_message(&model.provider));
        }
        Err(format_no_api_key_found_message(&model.provider))
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

                    session.agent_event_queue.lock().unwrap().await?;

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
                block_on(async move { session.should_stop_after_turn(context).await })
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
        let previous_states: HashMap<String, String> = matching
            .iter()
            .map(|action| (action.id.clone(), action.lifecycle.state().to_string()))
            .collect();
        let preparing: Vec<QueuedSessionAction> = self
            .action_store
            .lock()
            .unwrap()
            .active_actions(None)
            .into_iter()
            .filter(|action| {
                matches!(action.payload, QueuedActionPayload::Turn(_))
                    && action.lifecycle.state() == "preparing"
            })
            .collect();
        let previous_anchor = preparing.last().cloned();
        let actions = self.action_store.lock().unwrap().remove(predicate, Some(candidates));
        let mut restorable_messages: Vec<CustomMessage> = Vec::new();
        let removed: HashSet<String> = actions.iter().map(|action| action.id.clone()).collect();
        if let Some(previous_anchor) = previous_anchor {
            if removed.contains(&previous_anchor.id) {
                for action in &preparing {
                    if removed.contains(&action.id) {
                        continue;
                    }
                    let mut store = self.action_store.lock().unwrap();
                    let mut next = action.clone();
                    if let QueuedActionPayload::Turn(turn) = &mut next.payload {
                        turn.prepared = None;
                    }
                    store.replace_payload(&action.id, next.payload);
                }
            }
        }
        for action in &actions {
            let ticket = self.action_store.lock().unwrap().ticket_for(action);
            let previous_state = previous_states.get(&action.id).cloned().unwrap_or_default();
            match &action.payload {
                QueuedActionPayload::Turn(turn) => {
                    if turn.accepted_agent_message
                        || !turn.queue_visible
                        || previous_state != "queued"
                    {
                        if let Some(ticket) = &ticket {
                            ticket.reject_delivered(error);
                        }
                    } else if let Some(ticket) = &ticket {
                        ticket.settle_delivered("not_applicable");
                    }
                }
                QueuedActionPayload::SessionCommand(_) => {
                    if let Some(ticket) = &ticket {
                        ticket.settle_delivered("not_applicable");
                    }
                }
            }
            if let Some(ticket) = &ticket {
                ticket.settle_completed(error);
            }
            let dispatched = previous_state == "committing"
                && matches!(action.payload, QueuedActionPayload::Turn(_));
            if let QueuedActionPayload::Turn(payload) = &action.payload {
                for record in &payload.base.records {
                    let is_restorable = (record.role == DeliveryRecordRole::NextTurn
                        || (payload.accepted_agent_message
                            && record.role == DeliveryRecordRole::Prefix))
                        && matches!(record.message, DeliveryMessage::Custom(_))
                        && record_message_custom_type(&record.message)
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
                    record_message_custom_type(&record.message) == Some(HARNESS_DIGEST_CUSTOM_TYPE)
                }) {
                    self.harness_digest_pending.store(true, Ordering::SeqCst);
                }
                if dispatched {
                    let capture: HashSet<usize> = payload
                        .base
                        .records
                        .iter()
                        .map(|record| record.id.len())
                        .collect();
                    let _ = capture;
                    let mut store = self.action_store.lock().unwrap();
                    let mut next = action.clone();
                    if let QueuedActionPayload::Turn(turn) = &mut next.payload {
                        turn.capture_run_messages = Some(
                            payload
                                .base
                                .records
                                .iter()
                                .map(|record| record.id.len())
                                .collect(),
                        );
                    }
                    store.replace_payload(&action.id, next.payload);
                }
            }
            if !dispatched {
                self.action_store.lock().unwrap().release_terminal(action);
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
            crate::core::goals::GoalContextKind::from_str(kind),
            images.map(|images| images.iter().map(image_to_value).collect()),
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
        let _ = context;
        if self.steering_stop_pending() {
            return true;
        }
        if self.should_stop_for_threshold_compaction().await {
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

    /// `_shouldStopForThresholdCompaction`.
    async fn should_stop_for_threshold_compaction(&self) -> bool {
        let context_tokens = match self.threshold_context_tokens() {
            Some(tokens) => tokens,
            None => return false,
        };
        let state = self.agent.state();
        let model = state.model.clone();
        let settings = self.compaction_settings();
        if !should_compact_for_model(context_tokens, &model, &settings) {
            return false;
        }
        if self.continue_after_threshold_compaction.swap(false, Ordering::SeqCst) {
            return false;
        }
        let review = self.maybe_auto_refine(AutoRefineReason::Compact).await;
        let _ = review;
        self.schedule_auto_refine_after_compaction(true);
        self.compact(None, Some(CompactOptions { skip_abort: true }))
            .await
            .is_err()
            || true
    }

    /// `_thresholdCompactionNeeded`.
    async fn threshold_compaction_needed(self: &Arc<Self>, _context: ShouldStopAfterTurnContext) -> bool {
        self.should_stop_for_threshold_compaction().await
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
                    let path_entries: Vec<CompactionSessionEntry> =
                        entries.iter().map(compaction_entry_from_session_entry).collect();
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
        let _ = self.agent_event_queue.lock().unwrap().await;
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
            self.agent_event_queue.lock().unwrap().await?;
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
                action.lifecycle.state() == "cancelled"
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
                action.lifecycle.state() == "cancelled"
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
                            agent_message_key_of_delivery(&record.message) == key || record.started
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
                        store.replace_payload(&action.id, next.payload);
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
                        store.replace_payload(&action.id, next.payload);
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
                                if agent_message_key_of_delivery(&record.message) == key {
                                    record.started = true;
                                    if record.role == DeliveryRecordRole::Primary {
                                        started_primary = true;
                                    }
                                }
                            }
                        }
                        store.replace_payload(&action.id, next.payload);
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
                                if agent_message_key_of_delivery(&record.message) == key {
                                    record.durable = true;
                                    if record.role == DeliveryRecordRole::Primary {
                                        started_primary = true;
                                    }
                                }
                            }
                        }
                        store.replace_payload(&action.id, next.payload);
                        if started_primary && action.lifecycle.state() == "committing" {
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
                    action.lifecycle.state() == "cancelled"
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
                let _ = runner.emit(serde_json::json!({ "type": "agent_start" })).await;
            }
            AgentEvent::AgentEnd { messages } => {
                // Also capture at end of turn so commits made during the run (e.g. via a bash tool) land.
                let _ = self.session_manager.lock().unwrap().record_git_state_if_changed();
                let _ = runner
                    .emit(serde_json::json!({ "type": "agent_end", "messages": messages }))
                    .await;
            }
            AgentEvent::TurnStart => {
                let _ = runner
                    .emit(serde_json::json!({
                        "type": "turn_start",
                        "turnIndex": self.turn_index.load(Ordering::SeqCst),
                        "timestamp": now_ms(),
                    }))
                    .await;
            }
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => {
                let _ = runner
                    .emit(serde_json::json!({
                        "type": "turn_end",
                        "turnIndex": self.turn_index.load(Ordering::SeqCst),
                        "message": message,
                        "toolResults": tool_results,
                    }))
                    .await;
                self.turn_index.fetch_add(1, Ordering::SeqCst);
            }
            AgentEvent::MessageStart { message } => {
                let _ = runner
                    .emit(serde_json::json!({ "type": "message_start", "message": message }))
                    .await;
            }
            AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } => {
                let _ = runner
                    .emit(serde_json::json!({
                        "type": "message_update",
                        "message": message,
                        "assistantMessageEvent": assistant_message_event,
                    }))
                    .await;
            }
            AgentEvent::MessageEnd { message } => {
                let replacement = runner
                    .emit_message_end(serde_json::json!({ "type": "message_end", "message": message }));
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
                    .emit(serde_json::json!({
                        "type": "tool_execution_start",
                        "toolCallId": tool_call_id,
                        "toolName": tool_name,
                        "args": args,
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
                    .emit(serde_json::json!({
                        "type": "tool_execution_update",
                        "toolCallId": tool_call_id,
                        "toolName": tool_name,
                        "args": args,
                        "partialResult": partial_result,
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
                    .emit(serde_json::json!({
                        "type": "tool_execution_end",
                        "toolCallId": tool_call_id,
                        "toolName": tool_name,
                        "result": result,
                        "isError": is_error,
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
    ) -> Option<crate::core::extensions::types::ToolDefinition<Value>> {
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
        self.resource_loader.get_prompt_templates()
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

        let loader_system_prompt = self.resource_loader.get_system_prompt_override();
        let loader_append_system_prompt = self.resource_loader.get_append_system_prompt();
        let append_system_prompt = loader_append_system_prompt
            .filter(|value| !value.is_empty());
        let loaded_skills = self.model_visible_skills();
        let loaded_context_files = self.resource_loader.get_context_files();

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

        let skills = self.resource_loader.get_skills();
        let skill = match skills.iter().find(|skill| skill.name == skill_name) {
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
                    skill.name, file_path, base_dir, body
                );
                if args.is_empty() {
                    skill_block
                } else {
                    format!("{skill_block}\n\n{args}")
                }
            }
            Err(error) => {
                if let Some(runner) = self.extension_runner() {
                    runner.emit_error_value(serde_json::json!({
                        "extensionPath": file_path,
                        "event": "skill_expansion",
                        "error": error.to_string(),
                    }));
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
            cancelled_dispatch_end: None,
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
