//! `DaemonSession` adapter over the real `AgentSession`.
//!
//! `modes/daemon/daemon_mode.rs` holds resident sessions as
//! `Arc<dyn DaemonSession>` (the 97-method seam `DaemonSessionState.session` and
//! `AgentSessionRuntimeHandle.session` both use). The only implementor today is
//! the fail-loud `MissingSession` stand-in, so every real resident session falls
//! back to "Active session is not resident".
//!
//! `add_runtime` builds `AgentSessionRuntimeHandle` from an
//! `AgentSessionRuntime`, whose live session is a concrete `Arc<AgentSession>`.
//! This module supplies the missing adapter so that handle can carry the real
//! session instead of the stand-in.
//!
//! Only members with a real owner are implemented. The rest are omitted (see
//! `NOT IMPLEMENTED` below) so the compiler reports the true gap instead of this
//! file returning placeholders.

use std::sync::{Arc, Mutex as StdMutex};

use futures::future::BoxFuture;
use serde_json::{json, Map, Value};

use pi_agent_core::types::{AgentMessage, ThinkingLevel};
use pi_ai::types::ServiceTier;

use crate::core::agent_session::{AgentSession, RlmChildAgentStatus, RlmMaxDepthStatus};
use crate::core::session_manager::SessionManager;
use crate::core::settings_manager::SettingsManager;
use crate::modes::daemon::daemon_mode::{
    DaemonSession, ModelIdentity, RunUserBashOptions,
    SessionInputPause as DaemonSessionInputPause,
};

/// `DaemonSession` over a live `AgentSession`.
///
/// The daemon keeps one of these per resident session, so the seam's members
/// forward to the `AgentSession` methods the TypeScript reads off
/// `state.runtime.session`.
pub struct AgentSessionDaemonAdapter {
    session: Arc<AgentSession>,
}

impl AgentSessionDaemonAdapter {
    pub fn new(session: Arc<AgentSession>) -> Self {
        Self { session }
    }

    /// The wrapped session, for callers that already hold the adapter.
    pub fn session(&self) -> &Arc<AgentSession> {
        &self.session
    }
}

/// `session.thinkingLevel` as the daemon wire carries it.
fn thinking_level_value(level: ThinkingLevel) -> String {
    level.as_str().to_string()
}

/// `session.serviceTier` as the daemon wire carries it.
fn service_tier_value(service_tier: &ServiceTier) -> Option<String> {
    service_tier.clone().flatten()
}


/// `BashResult` as the daemon wire carries it (`BashResult` is not serde).
fn bash_result_value(result: &crate::core::bash_executor::BashResult) -> Value {
    let mut object = Map::new();
    object.insert("output".to_string(), Value::String(result.output.clone()));
    object.insert(
        "exitCode".to_string(),
        match result.exit_code {
            Some(code) => Value::from(code),
            None => Value::Null,
        },
    );
    object.insert("cancelled".to_string(), Value::Bool(result.cancelled));
    object.insert("truncated".to_string(), Value::Bool(result.truncated));
    if let Some(path) = &result.full_output_path {
        object.insert("fullOutputPath".to_string(), Value::String(path.clone()));
    }
    Value::Object(object)
}

/// `Model<Api>` as the daemon seam names it. `ModelIdentity` is a re-export of the canonical
/// `pi_ai::types::Model` (daemon_mode.rs:850), not a narrower wire view, so this is the identity:
/// the daemon reads `model.provider` / `model.id` off the same object the registry holds.
fn model_identity_of(model: pi_ai::types::Model) -> ModelIdentity {
    model
}

/// `buildSessionContext()` as the daemon's `get_session_context` wire expects
/// (`session-manager.ts` `SessionContext`).
fn session_context_value(context: &crate::core::session_manager::SessionContext) -> Value {
    let mut object = Map::new();
    object.insert(
        "messages".to_string(),
        serde_json::to_value(&context.messages).unwrap_or(Value::Null),
    );
    object.insert(
        "thinkingLevel".to_string(),
        Value::String(context.thinking_level.clone()),
    );
    object.insert(
        "serviceTier".to_string(),
        serde_json::to_value(&context.service_tier).unwrap_or(Value::Null),
    );
    object.insert(
        "model".to_string(),
        match &context.model {
            Some(model) => json!({ "provider": model.provider, "modelId": model.model_id }),
            None => Value::Null,
        },
    );
    Value::Object(object)
}

/// `thinkingLevel` as `ThinkingLevel`; an unknown string falls back to `Off`,
/// which is the same member the daemon wire emits for "off".
fn parse_thinking_level(level: &str) -> ThinkingLevel {
    match level {
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::Xhigh,
        "max" => ThinkingLevel::Max,
        _ => ThinkingLevel::Off,
    }
}

/// `ServiceTier` is `string | null` on the wire.
fn parse_service_tier(service_tier: &str) -> ServiceTier {
    Some(Some(service_tier.to_string()))
}

/// `getToolDefinition(name)` as the daemon serializes it (the same seven-field
/// projection `create_agent_connection_tool_definition` uses).
fn tool_definition_value(definition: &crate::core::extensions::types::ToolDefinition) -> Value {
    json!({
        "name": definition.name,
        "label": definition.label,
        "description": definition.description,
        "promptSnippet": definition.prompt_snippet,
        "promptGuidelines": definition.prompt_guidelines,
        "parameters": definition.parameters,
        "renderShell": definition.render_shell,
        "replayBuiltInToolName": definition.replay_built_in_tool_name,
    })
}

impl DaemonSession for AgentSessionDaemonAdapter {
    // NOT IMPLEMENTED (27 of the 97 trait members). The trait has no default
    // bodies, so an omitted member is an E0046 at this impl.
    //
    // blocked_on: `settings_manager` - the trait returns `Option<Arc<SettingsManager>>`
    // (no `Mutex`), so the daemon can call `set_transport` directly on the shared
    // value; `AgentSession` holds `Arc<Mutex<SettingsManager>>`. `Arc<Mutex<T>>`
    // cannot become `Arc<T>`, so the daemon seam's assumption that
    // `SettingsManager` is internally synchronized is a design gap for the
    // daemon slice, not something this adapter can bridge. (The daemon's own body
    // at daemon_mode.rs:5640 fails with "no method named `lock`" for the same
    // reason, so this member is already broken for `MissingSession` too.)
    // blocked_on: `subscribe` - `AgentSessionEvent` (core/agent_session.rs)
    // derives only `Debug, Clone`, and the daemon listener takes a serialized
    // `&Value`; no `AgentSessionEvent` -> wire converter exists in any slice (the
    // same blocker as the in-process adapter's `session_subscribe`).
    // blocked_on: `runtime` - needs `Arc<dyn DaemonRuntimeApi>`. That trait
    // (daemon_mode.rs:511) is implemented only by `MissingRuntime`; wrapping
    // `AgentSessionRuntime::new_session/switch_session/fork/import_from_jsonl`
    // is a separate runtime-level adapter, not a session-level one.
    // blocked_on: `session_dir` - `AgentSession` has no `session_dir` accessor;
    // `AgentSessionRuntime::runtime_config()` holds the session dir.
    // blocked_on: `connection_view` - needs `DaemonConnectionView`, which the
    // daemon builds itself (`connection_commands`/`connection_resource_snapshot`
    // read registries the session owns via `ResourceLoader`); the view struct is
    // declared in daemon_mode.rs and is not reachable from this module.
    // blocked_on: `register_rlm_child_session` / `release_rlm_child_session` -
    // these take `Arc<dyn DaemonSession>` and populate the daemon's own
    // `rlm_child_sessions` map; `AgentSession` tracks children by
    // `RlmChildRun`/`Arc<AgentSession>`, so the daemon-side registry is the owner.
    // blocked_on: `prompt_until_accepted` / `prompt_and_wait` /
    // `prompt_heartbeat` / `accept_agent_message_prompt` / `steer` / `follow_up` -
    // the daemon passes `PromptInvocation`, whose `signal`, `admission_committed`,
    // `preflight_result`, `prefix_messages` and `expand_prompt_templates` members
    // have no matching `PromptOptions` member (see `prompt_options_from_invocation`
    // in `agent_connection` for the same projection the connection layer makes);
    // `prefix_messages` in particular is consumed by the daemon's admission pump,
    // not by `AgentSession::prompt`.
    // blocked_on: `restore_steering_message` / `restore_follow_up_message` -
    // `AgentSession` takes `&RestoredPromptInput`; the daemon seam takes
    // `(message, images, options)`, so the restore payload's `queueKey` and
    // `customMessage` fields would have to be rebuilt here.
    // blocked_on: `restore_pending_next_turn_messages` - `AgentSession` takes
    // `&[CustomMessage]`; the daemon seam takes an untyped `&Value` whose shape is
    // owned by the daemon's worker-recovery journal.
    // blocked_on: `restore_session_actions` - `AgentSession` takes
    // `&SessionActionRecoverySnapshot` and returns `usize`; the daemon seam takes
    // `&Value` and returns `f64`, and the daemon's `restore_session_actions`
    // already parses that value itself.
    // blocked_on: `send_custom_message` - `AgentSession` takes an owned
    // `CustomMessage` plus `trigger_turn`/`deliver_as`; the daemon seam takes
    // `&Value` and the deserialization target is the daemon's wire type.
    // blocked_on: `clear_queued_agent_messages` / `clear_queue` -
    // `AgentSession` returns `ClearedQueue`; the daemon seam returns `Value` so
    // the daemon's own `serialize_cleared_queue`-shaped projection is the owner.
    // blocked_on: `mutate_queued_message` - the daemon's queue mutation body
    // parses the lane/mutation from the wire (`QueuedMessageLane`,
    // `QueuedMessageMutation`); `AgentSession` takes those already-parsed types,
    // and the status-string projection back to `Value` belongs to the daemon.
    // blocked_on: `resume_queued_work` - `AgentSession::resume_queued_work`
    // returns `()`; the daemon seam returns `bool` ("did anything resume"), which
    // only the daemon's `queued_action_count` bookkeeping can answer.
    // blocked_on: `get_steering_message_previews` / `get_follow_up_message_previews`
    // - `AgentSession` returns `Vec<String>`; the daemon seam returns `Vec<Value>`
    // carrying the queue keys the daemon assigns.
    // blocked_on: `wait_for_headless_completion` - needs an
    // `impl HeadlessCompletionSession for AgentSession`; the same three missing
    // members (`wait_for_rlm_quiescence`, `wait_for_headless_idle`,
    // `prompt_headless_continuation`) block the in-process adapter.
    // blocked_on: `refresh_available_models` / `refresh_model_catalog` - the
    // daemon seam returns `Vec<ModelIdentity>` / `Value` from the async
    // `ModelRegistry::refresh_available_models` / `refresh_model_catalog`; the
    // session holds the registry as `Arc<Mutex<ModelRegistry>>` and a sync member
    // cannot await the refresh (the same blocker as the in-process adapter's
    // `session_model_catalog`).
    // blocked_on: `set_transport` - `AgentHandle` has no transport setter, so the
    // live `Agent::transport` cannot be written through the session.
    // blocked_on: `compact` / `refine` - both need the private
    // `perform_compaction_unmeasured_full` / `refine_with_options` to return their
    // result types; `compact_with_options` returns `()`.
    // blocked_on: `reload` - `AgentSession::reload_with_options` lives on
    // `runtime_members.rs` and takes `Option<ExtensionBindings>`; the daemon seam
    // passes no bindings, so this would be a thin forward, and it is omitted here
    // because the same call is already reachable as `session_reload` through the
    // in-process adapter.
    // blocked_on: `get_context_tree` / `get_rlm_child_snapshots` - same missing
    // owners as the in-process adapter's `session_context_tree` /
    // `session_rlm_children`.
    // blocked_on: `export_to_html` / `export_to_jsonl` - no owner; see the
    // in-process adapter.
    // blocked_on: `get_last_assistant_text` - `AgentSession` has no
    // `lastAssistantText` accessor; the in-process adapter rebuilds it from
    // `messages()`, and the daemon seam would need the same projection.
    // blocked_on: `start_side_question` / `abort_side_question` - `AgentSession`
    // has no side-question owner; `core/side_question.rs` drives its own runs.
    // blocked_on: `new_session` / `switch_session` / `fork` / `import_from_jsonl`
    // - these are `AgentSessionRuntime` members, not `AgentSession` members; the
    // daemon reaches them through `AgentSessionRuntimeHandle`'s own closures.
    // blocked_on: `dispose` - `AgentSession::dispose` is synchronous; the daemon
    // seam returns a future. The async variant `dispose_async` exists but is the
    // one that carries the non-`Send` `ipython_kernel_provisioner` guard bug.
    //
    // REPAIR CURSOR: after the three `HeadlessCompletionSession` members land on
    // `AgentSession`, add `wait_for_headless_completion` (it is the last member
    // that both adapters share as a blocker).

    /// `session.model` as the daemon's `ModelIdentity` carries it.
    fn model_identity(&self) -> Option<ModelIdentity> {
        self.session.model().map(model_identity_of)
    }

    /// `session.modelRegistry.find(provider, modelId)`.
    fn find_model(&self, provider: &str, model_id: &str) -> Option<ModelIdentity> {
        let registry = self.session.model_registry();
        let registry = registry.lock().unwrap();
        registry.find(provider, model_id).map(model_identity_of)
    }

    /// `session.modelRegistry.getProviderAuthStatus(provider).source`.
    fn get_provider_auth_status_source(&self, provider: &str) -> Option<String> {
        let registry = self.session.model_registry();
        let registry = registry.lock().unwrap();
        registry.get_provider_auth_status(provider).source
    }

    fn session_manager(&self) -> Arc<StdMutex<SessionManager>> {
        Arc::clone(&self.session.session_manager)
    }

    fn session_id(&self) -> String {
        self.session.session_id()
    }

    fn session_name(&self) -> Option<String> {
        self.session.session_name()
    }

    fn session_file(&self) -> Option<String> {
        self.session.session_file()
    }

    fn is_streaming(&self) -> bool {
        self.session.is_streaming()
    }

    fn is_compacting(&self) -> bool {
        self.session.is_compacting()
    }

    fn is_bash_running(&self) -> bool {
        self.session.is_bash_running()
    }

    fn is_retrying(&self) -> bool {
        self.session.is_retrying()
    }

    fn is_session_active(&self) -> bool {
        self.session.is_session_active()
    }

    fn has_running_rlm_children(&self) -> bool {
        self.session.has_running_rlm_children()
    }

    /// `session.unfinishedActionCount` (`_queuedActionCount() + in-flight`).
    fn unfinished_action_count(&self) -> f64 {
        self.session.unfinished_action_count() as f64
    }

    fn messages(&self) -> Vec<AgentMessage> {
        self.session.messages()
    }

    /// `session.rlmDepth ?? null` - `None` is `null` on the wire.
    fn rlm_depth(&self) -> Option<i64> {
        Some(self.session.rlm_depth())
    }

    fn thinking_level(&self) -> Option<String> {
        Some(thinking_level_value(self.session.thinking_level()))
    }

    fn service_tier(&self) -> Option<String> {
        service_tier_value(&self.session.service_tier())
    }

    fn system_prompt(&self) -> Option<String> {
        Some(self.session.system_prompt())
    }

    fn set_current_recap(&self, recap: Option<&str>) {
        self.session
            .set_current_recap(recap.map(|recap| recap.to_string()));
    }

    fn set_session_name(&self, name: &str) {
        // The daemon seam returns unit; `setSessionName` reports a failure, so the
        // error is logged rather than dropped silently.
        if let Err(error) = self.session.set_session_name(name) {
            eprintln!("Warning: Could not set session name: {error}");
        }
    }

    /// `session.getRlmChildRunStatus(childId)` - `RlmChildAgentStatus` is already
    /// the wire string the daemon uses.
    fn get_rlm_child_run_status(&self, child_id: &str) -> Option<String> {
        let status: RlmChildAgentStatus = self.session.get_rlm_child_run_status(child_id)?;
        Some(status)
    }

    fn remove_queued_follow_up(&self, key: &str) {
        self.session.remove_queued_follow_up(key);
    }

    /// `session.runUserBash(command, options)`.
    fn run_user_bash(
        &self,
        command: &str,
        options: RunUserBashOptions,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let session = Arc::clone(&self.session);
        let command = command.to_string();
        // `transient`/`runId` are daemon-side bash-event identity fields; the
        // session's `runUserBash(command, excludeFromContext)` does not accept them.
        Box::pin(async move {
            let result = session
                .run_user_bash(&command, options.exclude_from_context)
                .await?;
            Ok(bash_result_value(&result))
        })
    }

    /// `session.executeBash(command)`.
    fn execute_bash(&self, command: &str) -> BoxFuture<'static, Result<Value, String>> {
        let session = Arc::clone(&self.session);
        let command = command.to_string();
        Box::pin(async move {
            let result = session.execute_bash(&command, None, None).await?;
            Ok(bash_result_value(&result))
        })
    }

    fn request_abort(&self) {
        self.session.request_abort();
    }

    fn abort_bash(&self) {
        self.session.abort_bash();
    }

    /// The daemon seam carries the pause as a bare release closure; the session
    /// returns the `SessionInputPause` struct, so its `release` is wrapped.
    fn acquire_session_input_pause(&self) -> DaemonSessionInputPause {
        let pause = self.session.acquire_session_input_pause();
        Arc::new(move || pause.release())
    }

    fn set_steering_mode(&self, mode: &str) {
        self.session.set_steering_mode(mode);
    }

    fn set_follow_up_mode(&self, mode: &str) {
        self.session.set_follow_up_mode(mode);
    }

    fn set_auto_compaction_enabled(&self, enabled: bool) {
        self.session.set_auto_compaction_enabled(enabled);
    }

    fn set_auto_retry_enabled(&self, enabled: bool) {
        self.session.set_auto_retry_enabled(enabled);
    }

    fn abort_compaction(&self) {
        self.session.abort_compaction();
    }

    fn abort_branch_summary(&self) {
        self.session.abort_branch_summary();
    }

    fn abort_retry(&self) {
        self.session.abort_retry();
    }

    fn replied_to_parent_since_task(&self) -> Option<bool> {
        self.session.replied_to_parent_since_task()
    }

    fn cycle_thinking_level(&self) -> Option<String> {
        self.session.cycle_thinking_level().map(thinking_level_value)
    }

    fn get_rlm_max_depth_status(&self) -> Value {
        let status: RlmMaxDepthStatus = self.session.get_rlm_max_depth_status();
        serde_json::to_value(status).unwrap_or(Value::Null)
    }

    fn build_session_context(&self) -> Value {
        session_context_value(&self.session.build_session_context())
    }

    fn get_session_stats(&self) -> Value {
        serde_json::to_value(self.session.get_session_stats()).unwrap_or(Value::Null)
    }

    fn get_user_messages_for_forking(&self) -> Vec<Value> {
        self.session
            .get_user_messages_for_forking()
            .into_iter()
            .map(|entry| {
                let mut object = Map::new();
                object.insert("entryId".to_string(), Value::String(entry.entry_id));
                object.insert("text".to_string(), Value::String(entry.text));
                Value::Object(object)
            })
            .collect()
    }

    fn get_tool_definition(&self, name: &str) -> Option<Value> {
        self.session
            .get_tool_definition(name)
            .map(|definition| tool_definition_value(&definition))
    }

    fn wait_for_idle(&self) -> BoxFuture<'static, ()> {
        let session = Arc::clone(&self.session);
        Box::pin(async move {
            // The daemon seam reports nothing; `waitForIdle` rejects on a session
            // failure, so the error is logged.
            if let Err(error) = session.wait_for_idle().await {
                eprintln!("Warning: Could not wait for session idle: {error}");
            }
        })
    }

    fn release_acp_mcp_servers(
        &self,
        owner_id: &str,
        server_names: &[String],
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.session);
        let owner_id = owner_id.to_string();
        let server_names = server_names.to_vec();
        Box::pin(async move { session.release_acp_mcp_servers(&owner_id, &server_names).await })
    }

    fn replace_acp_mcp_servers(
        &self,
        servers: &[Value],
        owner_id: &str,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.session);
        let owner_id = owner_id.to_string();
        let servers = servers
            .iter()
            .cloned()
            .map(serde_json::from_value)
            .collect::<Result<Vec<_>, _>>();
        Box::pin(async move {
            let servers = servers.map_err(|error| error.to_string())?;
            session.replace_acp_mcp_servers(&servers, &owner_id)
        })
    }

    fn set_thinking_level(&self, level: &str) {
        self.session.set_thinking_level(parse_thinking_level(level));
    }

    fn set_service_tier(&self, service_tier: &str) {
        self.session
            .set_service_tier(parse_service_tier(service_tier));
    }

}
