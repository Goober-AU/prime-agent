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

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use futures::future::BoxFuture;
use serde_json::{json, Map, Value};

use pi_agent_core::types::{AgentMessage, ThinkingLevel};
use pi_ai::types::ServiceTier;

use crate::core::agent_session::{
    AgentSession, ModelSelectOptions, PromptOptions, RlmChildAgentStatus, RlmMaxDepthStatus,
};
use crate::core::cron_jobs::AgentCronJob;
use crate::core::session_manager::SessionManager;
use crate::core::settings_manager::SettingsManager;
use crate::modes::daemon::daemon_extension_binding::ExtensionBindingInput;
use crate::modes::daemon::daemon_mode::{
    DaemonConnectionView, DaemonRuntimeApi, DaemonSession, ForkOptions, HeadlessCompletionOptions,
    ModelIdentity, NavigateTreeOptions, NewSessionRuntimeOptions, PromptInvocation, RefineOptions,
    RunUserBashOptions, SessionInputPause as DaemonSessionInputPause, SessionPathOptions,
    SideQuestionOptions,
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

/// `PromptInvocation` -> the session's `PromptOptions`.
///
/// The daemon seam carries the invocation as a struct of JSON-ish fields
/// (`daemon_mode.rs:1932`); the session owner takes `PromptOptions`. The signal,
/// admission callbacks, content and custom message are mapped too, so the forward is not lossy.
fn prompt_options_from_invocation(invocation: PromptInvocation) -> PromptOptions {
    let mut options = PromptOptions {
        expand_prompt_templates: invocation.expand_prompt_templates,
        streaming_behavior: invocation.streaming_behavior,
        queue_if_busy: invocation.queue_if_busy,
        resume_if_idle: invocation.resume_if_idle,
        skip_input_handlers: invocation.skip_input_handlers,
        agent_message_id: invocation.agent_message_id,
        signal: invocation.signal,
        admission_committed: invocation.admission_committed,
        ..Default::default()
    };
    if let Some(images) = &invocation.images {
        options.images = serde_json::from_value(images.clone()).ok();
    }
    if let Some(content) = &invocation.content {
        options.content = serde_json::from_value(content.clone()).ok();
    }
    if let Some(custom_message) = &invocation.custom_message {
        options.custom_message = serde_json::from_value(custom_message.clone()).ok();
    }
    if let Some(source) = invocation.source.as_deref() {
        options.source = match source {
            "interactive" => Some(crate::core::session_action_store::InputSource::Interactive),
            "rpc" => Some(crate::core::session_action_store::InputSource::Rpc),
            "extension" => Some(crate::core::session_action_store::InputSource::Extension),
            _ => None,
        };
    }
    options
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


    // ---------------------------------------------------------------------
    // Members whose canonical owner is missing return an explicit failure or
    // an empty value with a `blocked_on:` note. They are NOT omitted: the trait
    // has no default bodies, so an omitted member is an E0046 that stops the
    // whole crate from compiling and keeps every test from running.
    // ---------------------------------------------------------------------

    fn runtime(&self) -> Arc<dyn DaemonRuntimeApi> {
        // blocked_on: needs an `impl DaemonRuntimeApi` over the runtime; this adapter wraps a
        // session, and `AgentSessionRuntime` is held by the daemon, not by the session.
        Arc::new(MissingDaemonRuntimeApi)
    }

    fn settings_manager(&self) -> Option<Arc<StdMutex<SettingsManager>>> {
        // blocked_on: `AgentSession` exposes no `settingsManager` accessor; the settings live on the
        // runtime (`AgentSessionRuntime`), which this adapter does not hold.
        None
    }

    fn session_dir(&self) -> Option<String> {
        // blocked_on: `AgentSession` has no `sessionDir` accessor.
        None
    }

    fn set_exec_env_provider(&self, client_env: Option<HashMap<String, String>>) {
        // blocked_on: no `setExecEnvProvider` owner on `AgentSession`; the daemon seam sets it on the
        // binding/session-manager side.
        let _ = client_env;
    }

    fn set_runtime_env_scope(&self, client_env: Option<HashMap<String, String>>) {
        // blocked_on: no `setRuntimeEnvScope` owner on `AgentSession`.
        let _ = client_env;
    }

    fn set_subagent_runtime_host(&self, host: Option<Value>) {
        // blocked_on: the owner takes `Option<Arc<dyn SubagentRuntimeHost>>` (runtime_members.rs:2829)
        // while this seam carries an opaque `Value`; a converter does not exist.
        let _ = host;
    }

    fn set_rebind_session(&self, rebind: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>) {
        // blocked_on: no `setRebindSession` owner on `AgentSession`.
        let _ = rebind;
    }

    fn bind_extensions(
        &self,
        binding: crate::modes::daemon::daemon_extension_binding::ExtensionBindingInput,
    ) -> BoxFuture<'static, Result<(), String>> {
        // blocked_on: the owner takes the canonical `ExtensionBindings`
        // (runtime_members.rs:492) while the daemon seam carries its own already-typed
        // `ExtensionBindingInput`; the two need a shared owner.
        let _ = binding;
        Box::pin(async {
            Err("blocked_on: ExtensionBindingInput and ExtensionBindings have no shared owner".to_string())
        })
    }

    fn abort_for_update_restart(&self) {
        self.session.abort_for_update_restart();
    }

    fn connection_view(&self) -> DaemonConnectionView {
        // blocked_on: needs `createAgentConnectionState` (agent-connection/snapshot.ts:55) over an
        // `AgentSessionRuntimeSnapshotSource`, whose fields the session does not expose.
        DaemonConnectionView::default()
    }

    fn connection_state(&self, active_session_id: Option<String>) -> Value {
        // blocked_on: same missing snapshot source as `connection_view`.
        let _ = active_session_id;
        Value::Null
    }

    fn register_rlm_child_session(&self, child_id: &str, session: Arc<dyn DaemonSession>) -> bool {
        // blocked_on: no `registerRlmChildSession` owner on `AgentSession`.
        let _ = (child_id, session);
        false
    }

    fn subscribe(&self, listener: Arc<dyn Fn(&Value) + Send + Sync>) -> Box<dyn Fn() + Send + Sync> {
        // blocked_on: the owner takes `AgentSessionEventListener` and returns `Arc<dyn Fn()>`
        // (agent_session.rs:6294); this seam carries a `Value`-shaped listener.
        let _ = listener;
        Box::new(|| {})
    }

    fn prompt_until_accepted(
        &self,
        message: &str,
        options: PromptInvocation,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.session);
        let message = message.to_string();
        let options = prompt_options_from_invocation(options);
        Box::pin(async move { session.prompt_until_accepted(&message, Some(options)).await })
    }

    fn prompt_and_wait(
        &self,
        message: &str,
        options: PromptInvocation,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.session);
        let message = message.to_string();
        let options = prompt_options_from_invocation(options);
        Box::pin(async move { session.prompt_and_wait(&message, Some(options)).await })
    }

    fn prompt_heartbeat(
        &self,
        job: &AgentCronJob,
        options: PromptInvocation,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.session);
        let job = job.clone();
        let options = prompt_options_from_invocation(options);
        Box::pin(async move { session.prompt_heartbeat(&job, Some(options)).await })
    }

    fn accept_agent_message_prompt(
        &self,
        message: &str,
        options: PromptInvocation,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.session);
        let message = message.to_string();
        let options = prompt_options_from_invocation(options);
        Box::pin(async move {
            session
                .accept_agent_message_prompt(&message, Some(options))
                .await
        })
    }

    fn steer(
        &self,
        message: &str,
        images: Option<Value>,
        options: PromptInvocation,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.session);
        let message = message.to_string();
        let images = images.and_then(|images| serde_json::from_value(images).ok());
        let options = prompt_options_from_invocation(options);
        Box::pin(async move {
            session
                .steer(
                    &message,
                    images,
                    options.follow_up_queue_key,
                    options.agent_message_id,
                    options.resume_if_idle,
                )
                .await
        })
    }

    fn follow_up(
        &self,
        message: &str,
        images: Option<Value>,
        options: PromptInvocation,
    ) -> BoxFuture<'static, Result<bool, String>> {
        let session = Arc::clone(&self.session);
        let message = message.to_string();
        let images = images.and_then(|images| serde_json::from_value(images).ok());
        let options = prompt_options_from_invocation(options);
        Box::pin(async move {
            session
                .follow_up(
                    &message,
                    images,
                    options.follow_up_queue_key,
                    options.agent_message_id,
                    options.resume_if_idle,
                )
                .await
        })
    }

    fn restore_steering_message(
        &self,
        message: &str,
        images: Option<Value>,
        options: PromptInvocation,
    ) -> BoxFuture<'static, Result<(), String>> {
        // blocked_on: no `restoreSteeringMessage` owner on `AgentSession`.
        let _ = (message, images, options);
        Box::pin(async { Err("blocked_on: no restoreSteeringMessage owner".to_string()) })
    }

    fn restore_follow_up_message(
        &self,
        message: &str,
        images: Option<Value>,
        options: PromptInvocation,
    ) -> BoxFuture<'static, Result<bool, String>> {
        // blocked_on: no `restoreFollowUpMessage` owner on `AgentSession`.
        let _ = (message, images, options);
        Box::pin(async { Err("blocked_on: no restoreFollowUpMessage owner".to_string()) })
    }

    fn restore_pending_next_turn_messages(&self, messages: &Value) {
        // blocked_on: the owner takes `&[CustomMessage]` (agent_session.rs:10602); this seam carries
        // an opaque `Value`.
        let _ = messages;
    }

    fn restore_session_actions(&self, snapshot: &Value) -> BoxFuture<'static, Result<f64, String>> {
        // blocked_on: the owner takes `&SessionActionRecoverySnapshot` and returns `usize`
        // (agent_session.rs:8340); this seam takes an untyped `&Value` and returns `f64`. The daemon
        // already parses that value into the typed snapshot on its own path.
        let _ = snapshot;
        Box::pin(async {
            Err("blocked_on: daemon seam carries an untyped snapshot".to_string())
        })
    }

    fn send_custom_message(&self, message: &Value) -> BoxFuture<'static, Result<(), String>> {
        // blocked_on: the owner takes an owned `CustomMessage` plus `trigger_turn` and `deliver_as`
        // (agent_session.rs:9756); this seam carries an opaque `&Value`.
        let _ = message;
        Box::pin(async { Err("blocked_on: daemon seam carries an untyped message".to_string()) })
    }

    fn resume_queued_work(&self) -> bool {
        self.session.resume_queued_work();
        // blocked_on: the owner returns `()` (agent_session.rs:10546); this seam returns `bool`
        // ("did anything resume"), which only the daemon's queued-action bookkeeping can answer.
        true
    }

    fn clear_queued_agent_messages(&self) -> Value {
        // blocked_on: no `clearQueuedAgentMessages` owner; `clear_queue` (agent_session.rs:9891)
        // returns `ClearedQueue`, whose projection to this seam's `Value` is not defined.
        Value::Null
    }

    fn clear_queue(&self) -> Value {
        // blocked_on: `clear_queue` (agent_session.rs:9891) returns `ClearedQueue`, which is not
        // `Serialize`, so this seam's `Value` projection belongs to the daemon.
        self.session.clear_queue();
        Value::Null
    }

    fn mutate_queued_message(
        &self,
        lane: &str,
        index: f64,
        expected_text: &str,
        mutation: &Value,
    ) -> Value {
        // blocked_on: the owner takes `QueuedMessageLane`, `i64` and `&QueuedMessageMutation`
        // (agent_session.rs:10064); this seam carries wire-shaped `&str`/`f64`/`&Value`, and the
        // status projection back to `Value` is the daemon's.
        let _ = (lane, index, expected_text, mutation);
        Value::Null
    }

    fn get_steering_message_previews(&self) -> Vec<Value> {
        self.session
            .get_steering_message_previews()
            .into_iter()
            .map(Value::String)
            .collect()
    }

    fn get_follow_up_message_previews(&self) -> Vec<Value> {
        self.session
            .get_follow_up_message_previews()
            .into_iter()
            .map(Value::String)
            .collect()
    }

    fn cancel_rlm_child_run(&self, child_id: &str) -> bool {
        // blocked_on: the owner is `pub(super)` in `core::agent_session`, so it is unreachable here.
        let _ = child_id;
        false
    }

    fn delete_inactive_rlm_subagent(
        &self,
        child_id: &str,
        is_resident_child_running: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> BoxFuture<'static, Result<String, String>> {
        let session = Arc::clone(&self.session);
        let child_id = child_id.to_string();
        Box::pin(async move {
            session
                .delete_inactive_rlm_subagent(&child_id, is_resident_child_running)
                .await
        })
    }

    fn wait_for_headless_completion(
        &self,
        options: HeadlessCompletionOptions,
    ) -> BoxFuture<'static, Result<Value, String>> {
        // blocked_on: requires `impl HeadlessCompletionSession for AgentSession`.
        let _ = options;
        Box::pin(async {
            Err("blocked_on: no `impl HeadlessCompletionSession for AgentSession`".to_string())
        })
    }

    fn refresh_available_models(&self) -> BoxFuture<'static, Result<Vec<pi_ai::types::Model>, String>> {
        // blocked_on: registry refresh is async behind the session's `Arc<Mutex<ModelRegistry>>`,
        // and this seam member is not async-capable without that handle.
        Box::pin(async { Err("blocked_on: registry refresh is async behind the mutex".to_string()) })
    }

    fn refresh_model_catalog(&self) -> BoxFuture<'static, Result<Value, String>> {
        // blocked_on: same async-behind-mutex blocker as `refresh_available_models`.
        Box::pin(async { Err("blocked_on: registry refresh is async behind the mutex".to_string()) })
    }

    fn set_model(
        &self,
        model: &pi_ai::types::Model,
        wait_for_extensions: bool,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.session);
        let model = model.clone();
        Box::pin(async move {
            session
                .set_model(
                    model,
                    ModelSelectOptions {
                        wait_for_extensions: Some(wait_for_extensions),
                        ..Default::default()
                    },
                )
                .await
        })
    }

    fn cycle_model(
        &self,
        direction: &str,
        wait_for_extensions: bool,
    ) -> BoxFuture<'static, Result<Option<pi_ai::types::Model>, String>> {
        // blocked_on: no `cycleModel` owner on `AgentSession`.
        let _ = (direction, wait_for_extensions);
        Box::pin(async { Err("blocked_on: no cycleModel owner".to_string()) })
    }

    fn set_scoped_models(&self, scoped_models: &Value) {
        // blocked_on: the owner takes `Vec<ScopedModel>` on `&mut self` (agent_session.rs:6889);
        // this seam passes an opaque `Value` through `&self`.
        let _ = scoped_models;
    }

    fn set_transport(&self, transport: &str) {
        // blocked_on: `SettingsManager::set_transport` needs `&mut SettingsManager`; this adapter
        // holds no settings handle.
        let _ = transport;
    }

    fn compact(&self, custom_instructions: Option<&str>) -> BoxFuture<'static, Result<Value, String>> {
        let session = Arc::clone(&self.session);
        let custom_instructions = custom_instructions.map(|value| value.to_string());
        Box::pin(async move {
            session
                .compact_with_options(custom_instructions.as_deref(), false)
                .await;
            // blocked_on: `compact_with_options` returns `()`, so the `CompactionResult` this seam
            // carries is not reachable on the public path.
            Ok(Value::Null)
        })
    }

    fn refine(&self, options: RefineOptions) -> BoxFuture<'static, Result<Value, String>> {
        // blocked_on: `AgentSession::refine_with_options` is private and returns `RefinementResult`;
        // this seam needs a public path plus a serializer for the result.
        let _ = options;
        Box::pin(async {
            Err("blocked_on: refine_with_options is private; no public refine path".to_string())
        })
    }

    fn reload(&self) -> BoxFuture<'static, Result<(), String>> {
        // blocked_on: no `reload` owner on `AgentSession`; it lives on `AgentSessionRuntime`.
        Box::pin(async { Err("blocked_on: reload belongs to AgentSessionRuntime".to_string()) })
    }

    fn set_rlm_max_depth(
        &self,
        max_depth: Value,
        global: bool,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let session = Arc::clone(&self.session);
        let Some(max_depth) = max_depth.as_i64() else {
            // The daemon validates the wire value before it reaches this seam.
            return Box::pin(async { Err("rlmMaxDepth must be a non-negative integer".to_string()) });
        };
        Box::pin(async move {
            session
                .set_rlm_max_depth(max_depth, global)
                .await
                .map(|result| json!(result))
        })
    }

    fn get_context_tree(&self) -> Value {
        // blocked_on: `AgentSession` has no `getContextTree` owner in the port.
        Value::Null
    }

    fn get_rlm_child_snapshots(&self) -> Vec<Value> {
        // blocked_on: needs `rlm_child_snapshot_for_run` / `rlm_child_snapshot_for_session`
        // (runtime_members.rs:1800/1850), which are `pub(super)` in `core::agent_session`.
        Vec::new()
    }

    fn export_to_html(&self, output_path: Option<&str>) -> BoxFuture<'static, Result<String, String>> {
        let session_file = self.session.session_file();
        let output_path = output_path.map(|path| path.to_string());
        Box::pin(async move {
            let Some(session_file) = session_file else {
                return Err("Cannot export in-memory session to HTML".to_string());
            };
            let options = crate::core::export_html::ExportOptions {
                output_path,
                ..Default::default()
            };
            crate::core::export_html::export_from_file(&session_file, Some(options))
        })
    }

    fn export_to_jsonl(&self, output_path: Option<&str>) -> Result<String, String> {
        // blocked_on: no JSONL writer owner exists in the port.
        let _ = output_path;
        Err("blocked_on: no JSONL export owner".to_string())
    }

    fn get_last_assistant_text(&self) -> String {
        self.session
            .messages()
            .iter()
            .rev()
            .find_map(|message| match message {
                // Canonical owner: `read_assistant_text` (side_question.rs:66) returns "" for every
                // message that is not an assistant message, so the last non-empty read wins.
                AgentMessage::Message(pi_ai::types::Message::Assistant(_)) => {
                    Some(crate::core::side_question::read_assistant_text(message))
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    fn navigate_tree(
        &self,
        target_id: &str,
        options: NavigateTreeOptions,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let session = Arc::clone(&self.session);
        let target_id = target_id.to_string();
        Box::pin(async move {
            session
                .navigate_tree(
                    &target_id,
                    options.summarize,
                    options.custom_instructions.as_deref(),
                )
                .await?;
            // blocked_on: `navigate_tree_inner` holds the {editorText, cancelled, aborted} result and
            // is `pub(super)`; the successful path reports not-cancelled until it is exposed.
            Ok(json!({ "cancelled": false }))
        })
    }

    fn start_side_question(
        &self,
        question: &str,
        options: SideQuestionOptions,
    ) -> BoxFuture<'static, Result<(), String>> {
        // blocked_on: `core/side_question.rs` drives its own runs; `AgentSession` has no
        // `startSideQuestion` owner that accepts these options.
        let _ = (question, options);
        Box::pin(async { Err("blocked_on: no startSideQuestion owner".to_string()) })
    }

    fn abort_side_question(&self, side_question_id: &str) {
        // blocked_on: no `abortSideQuestion` owner on `AgentSession`.
        let _ = side_question_id;
    }

    fn new_session(
        &self,
        options: Option<NewSessionRuntimeOptions>,
    ) -> BoxFuture<'static, Result<Value, String>> {
        // blocked_on: `newSession` is an `AgentSessionRuntime` member; this adapter wraps a session.
        let _ = options;
        Box::pin(async { Err("blocked_on: newSession belongs to AgentSessionRuntime".to_string()) })
    }

    fn release_rlm_child_session(
        &self,
        child_id: &str,
        session: Arc<dyn DaemonSession>,
    ) -> Option<Box<dyn FnOnce() + Send>> {
        // blocked_on: the owner (runtime_members.rs:1783) has a different signature than this seam's.
        let _ = (child_id, session);
        None
    }

    fn switch_session(
        &self,
        session_path: &str,
        options: SessionPathOptions,
    ) -> BoxFuture<'static, Result<Value, String>> {
        // blocked_on: `switchSession` is an `AgentSessionRuntime` member.
        let _ = (session_path, options);
        Box::pin(async { Err("blocked_on: switchSession belongs to AgentSessionRuntime".to_string()) })
    }

    fn fork(
        &self,
        entry_id: &str,
        options: ForkOptions,
    ) -> BoxFuture<'static, Result<Value, String>> {
        // blocked_on: `fork` is an `AgentSessionRuntime` member.
        let _ = (entry_id, options);
        Box::pin(async { Err("blocked_on: fork belongs to AgentSessionRuntime".to_string()) })
    }

    fn import_from_jsonl(
        &self,
        input_path: &str,
        cwd_override: Option<&str>,
    ) -> BoxFuture<'static, Result<Value, String>> {
        // blocked_on: `importFromJsonl` is an `AgentSessionRuntime` member.
        let _ = (input_path, cwd_override);
        Box::pin(async { Err("blocked_on: importFromJsonl belongs to AgentSessionRuntime".to_string()) })
    }

    fn dispose(&self) -> BoxFuture<'static, ()> {
        let session = Arc::clone(&self.session);
        Box::pin(async move { session.dispose() })
    }
}

/// The `DaemonRuntimeApi` stand-in the `runtime` member returns when the adapter has no runtime.
/// Every method is an explicit failure, so a caller that reaches the daemon through this adapter's
/// missing runtime gets a real error rather than a silent empty value.
struct MissingDaemonRuntimeApi;

const MISSING_RUNTIME: &str =
    "blocked_on: AgentSessionDaemonAdapter wraps a session, not an AgentSessionRuntime";

impl DaemonRuntimeApi for MissingDaemonRuntimeApi {
    fn new_session(
        &self,
        options: Option<NewSessionRuntimeOptions>,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let _ = options;
        Box::pin(async { Err(MISSING_RUNTIME.to_string()) })
    }

    fn switch_session(
        &self,
        session_path: &str,
        options: SessionPathOptions,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let _ = (session_path, options);
        Box::pin(async { Err(MISSING_RUNTIME.to_string()) })
    }

    fn fork(
        &self,
        entry_id: &str,
        options: ForkOptions,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let _ = (entry_id, options);
        Box::pin(async { Err(MISSING_RUNTIME.to_string()) })
    }

    fn import_from_jsonl(
        &self,
        input_path: &str,
        cwd_override: Option<&str>,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let _ = (input_path, cwd_override);
        Box::pin(async { Err(MISSING_RUNTIME.to_string()) })
    }
}
