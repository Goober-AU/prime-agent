//! Live daemon session and runtime adapters.

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
use crate::modes::agent_connection::in_process_agent_connection::InProcessRuntimeHost;
use crate::modes::agent_connection::snapshot::{self, AgentsFileEntry, RegisteredCommandEntry, PromptTemplateEntry, SkillEntry, ResourceSkillEntry, ResourcePromptEntry, ResourceExtensionEntry, ResourceThemeEntry, ExtensionLoadError};
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
    runtime: Arc<super::AgentSessionRuntime>,
}

impl AgentSessionDaemonAdapter {
    pub fn new(runtime: Arc<super::AgentSessionRuntime>) -> Self {
        Self { runtime }
    }

    /// The wrapped session, for callers that already hold the adapter.
    pub fn session(&self) -> Arc<AgentSession> {
        self.runtime.session()
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
    /// `session.model` as the daemon's `ModelIdentity` carries it.
    fn model_identity(&self) -> Option<ModelIdentity> {
        self.runtime.session().model().map(model_identity_of)
    }

    /// `session.modelRegistry.find(provider, modelId)`.
    fn find_model(&self, provider: &str, model_id: &str) -> Option<ModelIdentity> {
        let registry = self.runtime.session().model_registry();
        let registry = registry.lock().unwrap();
        registry.find(provider, model_id).map(model_identity_of)
    }

    /// `session.modelRegistry.getProviderAuthStatus(provider).source`.
    fn get_provider_auth_status_source(&self, provider: &str) -> Option<String> {
        let registry = self.runtime.session().model_registry();
        let registry = registry.lock().unwrap();
        registry.get_provider_auth_status(provider).source
    }

    fn session_manager(&self) -> Arc<StdMutex<SessionManager>> {
        Arc::clone(&self.runtime.session().session_manager)
    }

    fn session_id(&self) -> String {
        self.runtime.session().session_id()
    }

    fn session_name(&self) -> Option<String> {
        self.runtime.session().session_name()
    }

    fn session_file(&self) -> Option<String> {
        self.runtime.session().session_file()
    }

    fn is_streaming(&self) -> bool {
        self.runtime.session().is_streaming()
    }

    fn is_compacting(&self) -> bool {
        self.runtime.session().is_compacting()
    }

    fn is_bash_running(&self) -> bool {
        self.runtime.session().is_bash_running()
    }

    fn is_retrying(&self) -> bool {
        self.runtime.session().is_retrying()
    }

    fn is_session_active(&self) -> bool {
        self.runtime.session().is_session_active()
    }

    fn has_running_rlm_children(&self) -> bool {
        self.runtime.session().has_running_rlm_children()
    }

    /// `session.unfinishedActionCount` (`_queuedActionCount() + in-flight`).
    fn unfinished_action_count(&self) -> f64 {
        self.runtime.session().unfinished_action_count() as f64
    }

    fn messages(&self) -> Vec<AgentMessage> {
        self.runtime.session().messages()
    }

    /// `session.rlmDepth ?? null` - `None` is `null` on the wire.
    fn rlm_depth(&self) -> Option<i64> {
        Some(self.runtime.session().rlm_depth())
    }

    fn thinking_level(&self) -> Option<String> {
        Some(thinking_level_value(self.runtime.session().thinking_level()))
    }

    fn service_tier(&self) -> Option<String> {
        service_tier_value(&self.runtime.session().service_tier())
    }

    fn system_prompt(&self) -> Option<String> {
        Some(self.runtime.session().system_prompt())
    }

    fn set_current_recap(&self, recap: Option<&str>) {
        self.runtime.session()
            .set_current_recap(recap.map(|recap| recap.to_string()));
    }

    fn set_session_name(&self, name: &str) {
        // The daemon seam returns unit; `setSessionName` reports a failure, so the
        // error is logged rather than dropped silently.
        if let Err(error) = self.runtime.session().set_session_name(name) {
            eprintln!("Warning: Could not set session name: {error}");
        }
    }

    /// `session.getRlmChildRunStatus(childId)` - `RlmChildAgentStatus` is already
    /// the wire string the daemon uses.
    fn get_rlm_child_run_status(&self, child_id: &str) -> Option<String> {
        let status: RlmChildAgentStatus = self.runtime.session().get_rlm_child_run_status(child_id)?;
        Some(status)
    }

    fn remove_queued_follow_up(&self, key: &str) {
        self.runtime.session().remove_queued_follow_up(key);
    }

    /// `session.runUserBash(command, options)`.
    fn run_user_bash(
        &self,
        command: &str,
        options: RunUserBashOptions,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let session = Arc::clone(&self.runtime.session());
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
        let session = Arc::clone(&self.runtime.session());
        let command = command.to_string();
        Box::pin(async move {
            let result = session.execute_bash(&command, None, None).await?;
            Ok(bash_result_value(&result))
        })
    }

    fn request_abort(&self) {
        self.runtime.session().request_abort();
    }

    fn abort_bash(&self) {
        self.runtime.session().abort_bash();
    }

    /// The daemon seam carries the pause as a bare release closure; the session
    /// returns the `SessionInputPause` struct, so its `release` is wrapped.
    fn acquire_session_input_pause(&self) -> DaemonSessionInputPause {
        let pause = self.runtime.session().acquire_session_input_pause();
        Arc::new(move || pause.release())
    }

    fn set_steering_mode(&self, mode: &str) {
        self.runtime.session().set_steering_mode(mode);
    }

    fn set_follow_up_mode(&self, mode: &str) {
        self.runtime.session().set_follow_up_mode(mode);
    }

    fn set_auto_compaction_enabled(&self, enabled: bool) {
        self.runtime.session().set_auto_compaction_enabled(enabled);
    }

    fn set_auto_retry_enabled(&self, enabled: bool) {
        self.runtime.session().set_auto_retry_enabled(enabled);
    }

    fn abort_compaction(&self) {
        self.runtime.session().abort_compaction();
    }

    fn abort_branch_summary(&self) {
        self.runtime.session().abort_branch_summary();
    }

    fn abort_retry(&self) {
        self.runtime.session().abort_retry();
    }

    fn replied_to_parent_since_task(&self) -> Option<bool> {
        self.runtime.session().replied_to_parent_since_task()
    }

    fn cycle_thinking_level(&self) -> Option<String> {
        self.runtime.session().cycle_thinking_level().map(thinking_level_value)
    }

    fn get_rlm_max_depth_status(&self) -> Value {
        let status: RlmMaxDepthStatus = self.runtime.session().get_rlm_max_depth_status();
        serde_json::to_value(status).unwrap_or(Value::Null)
    }

    fn build_session_context(&self) -> Value {
        session_context_value(&self.runtime.session().build_session_context())
    }

    fn get_session_stats(&self) -> Value {
        serde_json::to_value(self.runtime.session().get_session_stats()).unwrap_or(Value::Null)
    }

    fn get_user_messages_for_forking(&self) -> Vec<Value> {
        self.runtime.session()
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
        self.runtime.session()
            .get_tool_definition(name)
            .map(|definition| tool_definition_value(&definition))
    }

    fn wait_for_idle(&self) -> BoxFuture<'static, ()> {
        let session = Arc::clone(&self.runtime.session());
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
        let session = Arc::clone(&self.runtime.session());
        let owner_id = owner_id.to_string();
        let server_names = server_names.to_vec();
        Box::pin(async move { session.release_acp_mcp_servers(&owner_id, &server_names).await })
    }

    fn replace_acp_mcp_servers(
        &self,
        servers: &[Value],
        owner_id: &str,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.runtime.session());
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
        self.runtime.session().set_thinking_level(parse_thinking_level(level));
    }

    fn set_service_tier(&self, service_tier: &str) {
        self.runtime.session()
            .set_service_tier(parse_service_tier(service_tier));
    }


    // ---------------------------------------------------------------------
    // Members whose canonical owner is missing return an explicit failure or
    // an empty value with a `blocked_on:` note. They are NOT omitted: the trait
    // has no default bodies, so an omitted member is an E0046 that stops the
    // whole crate from compiling and keeps every test from running.
    // ---------------------------------------------------------------------

    fn runtime(&self) -> Arc<dyn DaemonRuntimeApi> { Arc::new(DaemonRuntimeAdapter(self.runtime.clone())) }

    fn settings_manager(&self) -> Option<Arc<StdMutex<SettingsManager>>> { Some(self.runtime.services().settings_manager.clone()) }

    fn session_dir(&self) -> Option<String> { Some(self.session().session_manager.lock().unwrap().get_session_dir()) }

    fn set_exec_env_provider(&self, client_env: Option<HashMap<String, String>>) {
        // blocked_on: no `setExecEnvProvider` owner on `AgentSession`; the daemon seam sets it on the
        // binding/session-manager side.
        let _ = client_env;
    }

    fn set_runtime_env_scope(&self, client_env: Option<HashMap<String, String>>) {
        // blocked_on: no `setRuntimeEnvScope` owner on `AgentSession`.
        let _ = client_env;
    }

    fn set_subagent_runtime_host(&self, host: Option<Arc<dyn crate::core::rlm_runtime::SubagentRuntimeHost>>) {
        if host.is_some() {
            self.session().set_subagent_runtime_host(host);
        }
    }

    fn set_rebind_session(&self, rebind: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>) {
        self.runtime.set_rebind_session(Some(Arc::new(move |_| rebind())));
    }

    fn bind_extensions(&self, binding: ExtensionBindingInput) -> BoxFuture<'static, Result<(), String>> {
        let session = self.session();
        let bindings = crate::core::agent_session::ExtensionBindings {
            ui_context: Some(binding.ui_context),
            command_context_actions: None,
            shutdown_handler: Some(binding.shutdown_handler),
            on_error: Some(Arc::new(move |error| (binding.on_error)(&crate::modes::daemon::daemon_extension_binding::ExtensionBindingError {
                extension_path: Some(error.extension_path), event: Some(error.event), error: error.error,
            }))),
        };
        Box::pin(async move { session.bind_extensions(&bindings).await })
    }

    fn abort_for_update_restart(&self) {
        self.runtime.session().abort_for_update_restart();
    }

    fn connection_view(&self) -> DaemonConnectionView {
        let host = super::InProcessRuntimeHostAdapter::new(self.runtime.clone());
        let mut view = DaemonConnectionView {
            session_id: self.session().session_id(), cwd: self.runtime.cwd(), ..Default::default()
        };
        for command in host.session_commands() {
            match command.source.as_str() {
                "extension" => view.registered_commands.push(RegisteredCommandEntry {
                    invocation_name: command.name.clone(), name: command.registered_name.unwrap_or(command.name),
                    description: command.description, source_info: command.source_info,
                }),
                "prompt" => view.prompt_templates.push(PromptTemplateEntry {
                    name: command.name, description: command.description, argument_hint: command.argument_hint,
                    source_info: command.source_info,
                }),
                "skill" => view.skills.push(SkillEntry {
                    name: command.name.strip_prefix("skill:").unwrap_or(&command.name).to_string(),
                    description: command.description, source_info: command.source_info,
                }),
                _ => unreachable!("the canonical command projection has a closed source set"),
            }
        }
        let resources = host.session_resource_snapshot();
        view.agents_files = resources.context_files.into_iter().map(|entry| AgentsFileEntry { path: entry.path }).collect();
        view.resource_skills = resources.skills.into_iter().map(|entry| ResourceSkillEntry { name: entry.name, description: entry.description, file_path: entry.file_path, source_info: entry.source_info }).collect();
        view.resource_prompts = resources.prompts.into_iter().map(|entry| ResourcePromptEntry { name: entry.name, description: entry.description, argument_hint: entry.argument_hint, file_path: entry.file_path, source_info: entry.source_info }).collect();
        view.resource_extensions = resources.extensions.into_iter().map(|entry| ResourceExtensionEntry { path: entry.path, source_info: entry.source_info }).collect();
        view.resource_themes = resources.themes.into_iter().map(|entry| ResourceThemeEntry { name: entry.name, source_path: entry.source_path, source_info: entry.source_info }).collect();
        view.skill_diagnostics = resources.diagnostics.skills;
        view.prompt_diagnostics = resources.diagnostics.prompts;
        view.theme_diagnostics = resources.diagnostics.themes;
        view.extension_load_errors = resources.diagnostics.extensions.into_iter().map(|entry| ExtensionLoadError { error: entry.message, path: entry.path }).collect();
        view
    }

    fn connection_state(&self, active_session_id: Option<String>) -> Value {
        let host = super::InProcessRuntimeHostAdapter::new(self.runtime.clone());
        serde_json::to_value(snapshot::create_agent_connection_state(&host.snapshot_source(), active_session_id))
            .expect("connection state serializes")
    }

    fn register_rlm_child_session(&self, child_id: &str, session: Arc<dyn DaemonSession>) -> bool {
        // blocked_on: no `registerRlmChildSession` owner on `AgentSession`.
        let _ = (child_id, session);
        false
    }

    fn subscribe(&self, listener: Arc<dyn Fn(&Value) + Send + Sync>) -> Box<dyn Fn() + Send + Sync> {
        let unsubscribe = self.session().subscribe(Arc::new(move |event| {
            match serde_json::to_value(event) {
                Ok(event) => listener(&event),
                Err(error) => eprintln!("Could not serialize session event: {error}"),
            }
        }));
        Box::new(move || unsubscribe())
    }

    fn prompt_until_accepted(
        &self,
        message: &str,
        options: PromptInvocation,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.runtime.session());
        let message = message.to_string();
        let options = prompt_options_from_invocation(options);
        Box::pin(async move { session.prompt_until_accepted(&message, Some(options)).await })
    }

    fn prompt_and_wait(
        &self,
        message: &str,
        options: PromptInvocation,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.runtime.session());
        let message = message.to_string();
        let options = prompt_options_from_invocation(options);
        Box::pin(async move { session.prompt_and_wait(&message, Some(options)).await })
    }

    fn prompt_heartbeat(
        &self,
        job: &AgentCronJob,
        options: PromptInvocation,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.runtime.session());
        let job = job.clone();
        let options = prompt_options_from_invocation(options);
        Box::pin(async move { session.prompt_heartbeat(&job, Some(options)).await })
    }

    fn accept_agent_message_prompt(
        &self,
        message: &str,
        options: PromptInvocation,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.runtime.session());
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
        let session = Arc::clone(&self.runtime.session());
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
        let session = Arc::clone(&self.runtime.session());
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

    fn restore_steering_message(&self, message: &str, images: Option<Value>, options: PromptInvocation) -> BoxFuture<'static, Result<(), String>> {
        let session = self.session();
        let snapshot = restored_prompt(message, images, options);
        Box::pin(async move { session.restore_steering_message(&snapshot?).await.map(|_| ()) })
    }

    fn restore_follow_up_message(&self, message: &str, images: Option<Value>, options: PromptInvocation) -> BoxFuture<'static, Result<bool, String>> {
        let session = self.session();
        let snapshot = restored_prompt(message, images, options);
        Box::pin(async move { session.restore_follow_up_message(&snapshot?).await })
    }

    fn restore_pending_next_turn_messages(&self, messages: &Value) {
        match serde_json::from_value::<Vec<crate::core::messages::CustomMessage>>(messages.clone()) {
            Ok(messages) => self.session().restore_pending_next_turn_messages(&messages),
            Err(error) => eprintln!("Could not restore pending messages: {error}"),
        }
    }

    fn restore_session_actions(&self, snapshot: &Value) -> BoxFuture<'static, Result<f64, String>> {
        let session = self.session();
        let snapshot = serde_json::from_value(snapshot.clone()).map_err(|error| error.to_string());
        Box::pin(async move { session.restore_session_actions(&snapshot?).await.map(|count| count as f64) })
    }

    fn send_custom_message(&self, message: &Value) -> BoxFuture<'static, Result<(), String>> {
        let session = self.session();
        let message = serde_json::from_value(message.clone()).map_err(|error| error.to_string());
        Box::pin(async move { session.send_custom_message(message?, None, None).await })
    }

    fn resume_queued_work(&self) -> bool {
        let session = self.session();
        let pending = session.has_pending_session_work();
        session.resume_queued_work();
        pending
    }

    fn clear_queued_agent_messages(&self) -> Value {
        let cleared = self.session().clear_queued_agent_messages();
        json!({"steering":cleared.steering,"followUp":cleared.follow_up})
    }

    fn clear_queue(&self) -> Value {
        let cleared = self.session().clear_queue();
        json!({"steering":cleared.steering,"followUp":cleared.follow_up})
    }

    fn mutate_queued_message(&self, lane: &str, index: f64, expected_text: &str, mutation: &Value) -> Value {
        let lane = serde_json::from_value(Value::String(lane.to_string()));
        let mutation = serde_json::from_value(mutation.clone());
        if !index.is_finite() || index.fract() != 0.0 || index < 0.0 || index > i64::MAX as f64 { return json!("invalid"); }
        match (lane, mutation) {
            (Ok(lane), Ok(mutation)) => json!(self.session().mutate_queued_message(lane, index as i64, expected_text, &mutation).as_str()),
            _ => json!("invalid"),
        }
    }

    fn get_steering_message_previews(&self) -> Vec<Value> {
        self.runtime.session()
            .get_steering_message_previews()
            .into_iter()
            .map(Value::String)
            .collect()
    }

    fn get_follow_up_message_previews(&self) -> Vec<Value> {
        self.runtime.session()
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
        let session = Arc::clone(&self.runtime.session());
        let child_id = child_id.to_string();
        Box::pin(async move {
            session
                .delete_inactive_rlm_subagent(&child_id, is_resident_child_running)
                .await
        })
    }

    fn wait_for_headless_completion(&self, options: HeadlessCompletionOptions) -> BoxFuture<'static, Result<Value, String>> {
        let session = self.session();
        Box::pin(async move {
            let status = crate::modes::headless_completion::wait_for_headless_completion(Arc::new(session),
                crate::modes::headless_completion::HeadlessCompletionOptions { wait_for_rlm_quiescence: options.wait_for_rlm_quiescence }).await?;
            serde_json::to_value(status).map_err(|error| error.to_string())
        })
    }

    fn refresh_available_models(&self) -> BoxFuture<'static, Result<Vec<pi_ai::types::Model>, String>> {
        let registry = self.session().model_registry();
        Box::pin(async move { crate::core::sdk::with_model_registry(registry, |registry| Box::pin(registry.refresh_available_models())).await })
    }

    fn refresh_model_catalog(&self) -> BoxFuture<'static, Result<Value, String>> {
        let registry = self.session().model_registry();
        Box::pin(async move {
            let catalog = crate::core::sdk::with_model_registry(registry, |registry| Box::pin(registry.refresh_model_catalog())).await?;
            Ok(json!({"models":catalog.models,"configuredProviders":catalog.configured_providers}))
        })
    }

    fn set_model(
        &self,
        model: &pi_ai::types::Model,
        wait_for_extensions: bool,
    ) -> BoxFuture<'static, Result<(), String>> {
        let session = Arc::clone(&self.runtime.session());
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

    fn cycle_model(&self, direction: &str, wait_for_extensions: bool) -> BoxFuture<'static, Result<Option<pi_ai::types::Model>, String>> {
        let session = self.session();
        let direction = if direction == "backward" { -1 } else { 1 };
        Box::pin(async move { session.cycle_model(Some(direction), ModelSelectOptions { wait_for_extensions: Some(wait_for_extensions) }).await.map(|result| Some(result.model)) })
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
        let session = Arc::clone(&self.runtime.session());
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
        let session = self.session();
        Box::pin(async move { session.reload_with_options(None).await })
    }

    fn set_rlm_max_depth(
        &self,
        max_depth: Value,
        global: bool,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let session = Arc::clone(&self.runtime.session());
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
        let session_file = self.runtime.session().session_file();
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
        self.runtime.session()
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
        let session = Arc::clone(&self.runtime.session());
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

    fn new_session(&self, options: Option<NewSessionRuntimeOptions>) -> BoxFuture<'static, Result<Value, String>> { self.runtime().new_session(options) }

    fn release_rlm_child_session(
        &self,
        child_id: &str,
        session: Arc<dyn DaemonSession>,
    ) -> Option<Box<dyn FnOnce() + Send>> {
        // blocked_on: the owner (runtime_members.rs:1783) has a different signature than this seam's.
        let _ = (child_id, session);
        None
    }

    fn switch_session(&self, path: &str, options: SessionPathOptions) -> BoxFuture<'static, Result<Value, String>> { self.runtime().switch_session(path, options) }

    fn fork(&self, entry: &str, options: ForkOptions) -> BoxFuture<'static, Result<Value, String>> { self.runtime().fork(entry, options) }

    fn import_from_jsonl(&self, path: &str, cwd: Option<&str>) -> BoxFuture<'static, Result<Value, String>> { self.runtime().import_from_jsonl(path, cwd) }

    fn dispose(&self) -> BoxFuture<'static, ()> {
        let runtime = self.runtime.clone();
        Box::pin(async move { if let Err(error) = runtime.dispose(None).await { eprintln!("Could not dispose runtime: {error}"); } })
    }
}


fn restored_prompt(message: &str, images: Option<Value>, options: PromptInvocation) -> Result<crate::core::agent_session::RestoredPromptInput, String> {
    Ok(crate::core::agent_session::RestoredPromptInput {
        text: message.to_string(),
        content: options.content.map(serde_json::from_value).transpose().map_err(|error| error.to_string())?,
        images: images.or(options.images).map(serde_json::from_value).transpose().map_err(|error| error.to_string())?,
        queue_key: options.queue_key,
        agent_message_id: options.agent_message_id,
        custom_message: options.custom_message.map(serde_json::from_value).transpose().map_err(|error| error.to_string())?,
        prefix_messages: options.prefix_messages.map(serde_json::from_value).transpose().map_err(|error| error.to_string())?,
    })
}

struct DaemonRuntimeAdapter(Arc<super::AgentSessionRuntime>);

impl DaemonRuntimeApi for DaemonRuntimeAdapter {
    fn new_session(&self, options: Option<NewSessionRuntimeOptions>) -> BoxFuture<'static, Result<Value, String>> {
        let runtime = self.0.clone();
        Box::pin(async move {
            let parent_session = options.and_then(|options| options.parent_session)
                .map(|value| value.as_str().map(str::to_string).ok_or_else(|| "parentSession must be a string".to_string())).transpose()?;
            let result = runtime.new_session(Some(super::NewSessionOptionsInput { parent_session, ..Default::default() })).await?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        })
    }
    fn switch_session(&self, path: &str, options: SessionPathOptions) -> BoxFuture<'static, Result<Value, String>> {
        let runtime = self.0.clone();
        let path = path.to_string();
        Box::pin(async move {
            let result = runtime.switch_session(&path, Some(super::SwitchSessionOptions { cwd_override: options.cwd_override, with_session: None })).await?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        })
    }
    fn fork(&self, entry: &str, options: ForkOptions) -> BoxFuture<'static, Result<Value, String>> {
        let runtime = self.0.clone();
        let entry = entry.to_string();
        Box::pin(async move {
            let result = runtime.fork(&entry, Some(super::ForkOptionsInput { position: options.position, with_session: None })).await?;
            let mut result_value = json!({"cancelled":result.cancelled});
            if let Some(text) = result.selected_text { result_value["selectedText"] = Value::String(text); }
            Ok(result_value)
        })
    }
    fn import_from_jsonl(&self, path: &str, cwd: Option<&str>) -> BoxFuture<'static, Result<Value, String>> {
        let runtime = self.0.clone();
        let path = path.to_string();
        let cwd = cwd.map(str::to_string);
        Box::pin(async move {
            let result = runtime.import_from_jsonl(&path, cwd.as_deref()).await?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        })
    }
}

pub(crate) async fn run_native_daemon_mode(options: crate::main_entry::DaemonModeSeamOptions) -> Result<(), String> {
    use crate::modes::daemon::daemon_mode as daemon;
    let default_config = options.default_session_config.clone();
    let daemon_config = serde_json::from_value(serde_json::to_value(&default_config).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let factory = options.create_runtime;
    let create_runtime: daemon::CreateAgentSessionRuntimeFactory = Arc::new(move |input| {
        let factory = factory.clone();
        let default_config = default_config.clone();
        Box::pin(async move {
            let metadata = input.runtime_metadata.map(serde_json::from_value).transpose().map_err(|error| error.to_string())?;
            let model = input.session_options.model.map(serde_json::from_value).transpose().map_err(|error| error.to_string())?;
            let runtime = super::create_agent_session_runtime(factory, super::CreateAgentSessionRuntimeInput {
                cwd: input.cwd,
                agent_dir: input.agent_dir.or(default_config.agent_dir.clone()).unwrap_or_else(crate::config::get_agent_dir),
                session_manager: input.session_manager,
                session_start_event: Some(json!({"type":"session_start","reason":"startup"})),
                session_config: input.session_config.or(Some(default_config)),
                session_options: Some(crate::core::agent_session_services::AgentSessionCreationOptions {
                    model,
                    agent_message_controller: input.session_options.agent_message_controller,
                    agent_observe_controller: input.session_options.agent_observe_controller,
                    rlm_heartbeat_controller: input.session_options.rlm_heartbeat_controller,
                    ..Default::default()
                }),
                runtime_metadata: metadata,
                session_lease: None,
            }).await?;
            let metadata = serde_json::from_value(serde_json::to_value(runtime.metadata()).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
            let adapter = Arc::new(AgentSessionDaemonAdapter::new(runtime.clone()));
            Ok(daemon::AgentSessionRuntimeHandle {
                session: adapter.clone(), metadata,
                model_fallback_message: runtime.model_fallback_message(),
                new_session: Some({ let adapter = adapter.clone(); Arc::new(move |options| adapter.new_session(options)) }),
                switch_session: Some({ let adapter = adapter.clone(); Arc::new(move |path, options| adapter.switch_session(&path, options)) }),
                fork: Some({ let adapter = adapter.clone(); Arc::new(move |entry, options| adapter.fork(&entry, options)) }),
                import_from_jsonl: Some({ let adapter = adapter.clone(); Arc::new(move |path, cwd| adapter.import_from_jsonl(&path, cwd.as_deref())) }),
            })
        })
    });
    daemon::run_daemon_mode(daemon::DaemonModeOptions {
        socket_path: options.socket_path, default_session_config: daemon_config, create_runtime,
        worker: options.worker.map(|worker| daemon::DaemonWorkerOptions {
            authentication_token: worker.authentication_token,
            worker_instance_id: worker.worker_instance_id,
            restore_active_session_id: worker.restore_active_session_id,
        }),
    }).await
}
