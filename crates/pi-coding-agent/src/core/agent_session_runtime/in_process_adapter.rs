//! In-process `InProcessRuntimeHost` adapter over the real `AgentSessionRuntime`.
//!
//! `modes/agent_connection/in_process_agent_connection.rs` drives the explicit
//! `InProcessRuntimeHost` seam (the TypeScript `runtimeHost` object). This module
//! supplies the production implementation of that seam over the landed
//! `AgentSessionRuntime` + `AgentSession`, so the in-process agent connection is
//! backed by real sessions instead of an empty stand-in.
//!
//! Every forwarded call mirrors the TypeScript member it replaces. Where the
//! TypeScript reads a value the Rust port has no public owner for, the method is
//! left out entirely (see `NOT IMPLEMENTED` below) instead of returning a
//! placeholder, so the compiler reports the real gap.

use std::sync::{Arc, Mutex};

use pi_agent_core::types::{AgentMessage, ThinkingLevel};
use pi_ai::types::{BoxFuture, ImageContent, Model, ServiceTier};
use serde_json::{Map, Value};

use crate::core::agent_session::{
    AgentSession, ExtensionBindings, ModelSelectOptions, PromptOptions, RlmMaxDepthStatus,
    SessionInputPause,
};
use crate::core::agent_session_runtime::{
    AgentSessionRuntime, ForkOptionsInput, NewSessionOptionsInput, SwitchSessionOptions,
};
use crate::core::diagnostics::{ResourceCollision, ResourceDiagnostic};
use crate::core::extensions::types::InputSource;
use crate::core::resource_loader::ResourceLoader;
use crate::core::skills::Skill;
use crate::core::session_action_store::{
    QueuedMessageLane, QueuedMessageMutation, QueuedMessageMutationStatus,
};
use crate::core::source_info::SourceInfo;
use crate::modes::agent_connection::daemon_agent_connection::build_session_tree_from_flat_nodes;
use crate::modes::agent_connection::in_process_agent_connection::InProcessRuntimeHost;
use crate::modes::agent_connection::snapshot::{
    create_agent_connection_commands, create_agent_connection_resource_snapshot, AgentsFileEntry,
    ExtensionLoadError as SnapshotExtensionLoadError, PromptTemplateEntry, RegisteredCommandEntry,
    ResourceExtensionEntry, ResourcePromptEntry, ResourceSkillEntry, ResourceThemeEntry, SkillEntry,
};
use crate::modes::agent_connection::tool_definition::ToolDefinition as ConnectionToolDefinition;
use crate::modes::agent_connection::types::{
    AgentConnectionExecuteBashOptions, AgentConnectionForkOptions,
    AgentConnectionInputPause, AgentConnectionModel, AgentConnectionModelCycleResult,
    AgentConnectionNewSessionOptions,
    AgentConnectionQueueState, AgentConnectionResourceCollision,
    AgentConnectionResourceDiagnostic, AgentConnectionResourceSnapshot,
    AgentConnectionScopedModel, AgentConnectionSessionContext, AgentConnectionSessionContextModel,
    AgentConnectionSessionEntry, AgentConnectionSessionHeader, AgentConnectionSessionInputPause,
    AgentConnectionSessionTree, AgentConnectionSessionTreeFlatNode, AgentConnectionSlashCommand,
    AgentConnectionSwitchSessionOptions, AgentConnectionUserMessage,
    AgentConnectionWatchSessionTree,
};

/// The `runtimeHost` the in-process agent connection drives.
pub struct InProcessRuntimeHostAdapter {
    runtime: Arc<AgentSessionRuntime>,
}

impl InProcessRuntimeHostAdapter {
    pub fn new(runtime: Arc<AgentSessionRuntime>) -> Self {
        Self { runtime }
    }

    pub fn runtime(&self) -> &Arc<AgentSessionRuntime> {
        &self.runtime
    }

    fn session(&self) -> Arc<AgentSession> {
        self.runtime.session()
    }

    /// `modelRegistry.refreshAvailableModels()` from a synchronous seam.
    ///
    /// `ModelRegistry::refresh_available_models` is async because it also drives a
    /// detached Prime Inference network refresh; the synchronous owners it uses
    /// for the disk/bundled catalog are `refresh()` (reload from disk) followed by
    /// `get_available()` (models with configured auth). Both are real owners.
    fn registry_available_models(&self) -> Vec<AgentConnectionModel> {
        let registry = self.session().model_registry();
        let mut registry = registry.lock().unwrap();
        registry.refresh();
        registry.get_available()
    }
}

/// `acquireSessionInputPause()` result adapted to the connection-layer trait.
///
/// `SessionInputPause` is a concrete struct; `AgentConnectionSessionInputPause`
/// requires `Arc<dyn AgentConnectionInputPause>`, so this wrapper forwards
/// `release()`. The TypeScript `release()` is synchronous and void, so the
/// adapted call reports success.
struct InProcessInputPause {
    pause: SessionInputPause,
}

impl AgentConnectionInputPause for InProcessInputPause {
    fn release(&self) -> BoxFuture<Result<(), String>> {
        self.pause.release();
        Box::pin(async { Ok(()) })
    }
}

/// `prompt(message, options)` payload the connection layer passes down.
fn prompt_options_from_value(options: Value) -> PromptOptions {
    let mut parsed = PromptOptions::default();
    let Some(object) = options.as_object() else {
        return parsed;
    };
    if let Some(images) = object.get("images") {
        parsed.images = serde_json::from_value(images.clone()).ok();
    }
    if let Some(behavior) = object.get("streamingBehavior").and_then(Value::as_str) {
        parsed.streaming_behavior = Some(behavior.to_string());
    }
    if let Some(resume) = object.get("resumeIfIdle").and_then(Value::as_bool) {
        parsed.resume_if_idle = Some(resume);
    }
    if let Some(queue) = object.get("queueIfBusy").and_then(Value::as_bool) {
        parsed.queue_if_busy = Some(queue);
    }
    if let Some(source) = object.get("source").and_then(Value::as_str) {
        parsed.source = match source {
            "interactive" => Some(InputSource::Interactive),
            "rpc" => Some(InputSource::Rpc),
            "extension" => Some(InputSource::Extension),
            _ => None,
        };
    }
    parsed
}

/// `toConnectionSourceInfo(sourceInfo)`.
fn connection_source_info(
    source_info: &SourceInfo,
) -> crate::modes::agent_connection::types::AgentConnectionSourceInfo {
    crate::modes::agent_connection::types::AgentConnectionSourceInfo {
        path: source_info.path.clone(),
        source: source_info.source.clone(),
        scope: source_info.scope.clone(),
        origin: source_info.origin.clone(),
        base_dir: source_info.base_dir.clone(),
    }
}

/// `toConnectionSourceInfo(theme.sourceInfo)`.
///
/// `resource_loader::update_themes_from_paths` assigns the theme module's own
/// three-field `SourceInfo` (the TypeScript assigns the full `core/source-info.ts`
/// shape, but the Rust theme owner narrows it). `origin`/`baseDir` are therefore
/// not available on a loaded theme; the connection layer's stand-in expects them,
/// so they are reported as absent rather than invented.
fn connection_source_info_from_theme(
    source_info: &crate::modes::interactive::theme::theme::SourceInfo,
) -> crate::modes::agent_connection::types::AgentConnectionSourceInfo {
    crate::modes::agent_connection::types::AgentConnectionSourceInfo {
        path: source_info.path.clone(),
        source: source_info.source.clone(),
        scope: source_info.scope.clone(),
        origin: String::new(),
        base_dir: None,
    }
}

fn skill_base(skill: &Skill) -> &crate::core::skills::BaseSkill {
    match skill {
        Skill::Markdown(skill) => &skill.base,
        Skill::Python(skill) => &skill.base,
    }
}

/// `AgentConnectionResourceDiagnostic` from a loaded-resource diagnostic.
fn connection_diagnostic(diagnostic: &ResourceDiagnostic) -> AgentConnectionResourceDiagnostic {
    AgentConnectionResourceDiagnostic {
        type_: diagnostic.diagnostic_type.clone(),
        message: diagnostic.message.clone(),
        path: diagnostic.path.clone(),
        collision: diagnostic.collision.as_ref().map(connection_collision),
    }
}

fn connection_collision(collision: &ResourceCollision) -> AgentConnectionResourceCollision {
    AgentConnectionResourceCollision {
        resource_type: collision.resource_type.clone(),
        name: collision.name.clone(),
        winner_path: collision.winner_path.clone(),
        winner_source: collision.winner_source.clone(),
        loser_path: collision.loser_path.clone(),
        loser_source: collision.loser_source.clone(),
    }
}

/// `createAgentConnectionToolDefinition(session.getToolDefinition(name))`.
///
/// The connection-layer `ToolDefinition` is the seven-field projection
/// `tool-definition.ts` copies; the session owns the full extension definition.
fn connection_tool_definition(
    definition: &crate::core::extensions::types::ToolDefinition,
) -> ConnectionToolDefinition {
    ConnectionToolDefinition {
        name: definition.name.clone(),
        label: definition.label.clone(),
        description: definition.description.clone(),
        prompt_snippet: definition.prompt_snippet.clone(),
        prompt_guidelines: definition.prompt_guidelines.clone(),
        parameters: definition.parameters.clone(),
        render_shell: definition.render_shell.clone(),
        replay_built_in_tool_name: definition.replay_built_in_tool_name.clone(),
    }
}

/// `BashResult` as the connection layer carries it (`BashResult` is not serde).
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

/// `createAgentConnectionState(runtime)`/`getSessionTree()` projections.
///
/// Shared by `session_context()` and `session_tree()` so the two seam members and
/// the snapshot builder agree on the same session-manager reads.
fn session_tree_value(session: &Arc<AgentSession>) -> AgentConnectionSessionTree {
    let manager = session.session_manager.lock().unwrap();
    let mut flat_nodes: Vec<AgentConnectionSessionTreeFlatNode> = Vec::new();
    for node in manager.get_flat_tree() {
        match serde_json::from_value(Value::Object(node.entry)) {
            Ok(entry) => flat_nodes.push(AgentConnectionSessionTreeFlatNode {
                entry,
                label: node.label,
                label_timestamp: node.label_timestamp,
            }),
            Err(error) => {
                // A session entry outside the connection union cannot be
                // projected; report it instead of dropping it silently.
                eprintln!("Warning: Could not project session tree entry: {error}");
            }
        }
    }
    let leaf_id = manager.get_leaf_id();
    drop(manager);
    AgentConnectionSessionTree {
        tree: build_session_tree_from_flat_nodes(&flat_nodes),
        leaf_id,
    }
}

/// `buildSessionContext()` as the connection layer carries it.
fn session_context_value(session: &Arc<AgentSession>) -> AgentConnectionSessionContext {
    let context = session.build_session_context();
    AgentConnectionSessionContext {
        messages: context.messages,
        thinking_level: context.thinking_level,
        service_tier: context.service_tier,
        model: context.model.map(|model| AgentConnectionSessionContextModel {
            provider: model.provider,
            model_id: model.model_id,
        }),
    }
}

/// The `runtimeHost` implementation for a live `AgentSessionRuntime`.
impl InProcessRuntimeHost for InProcessRuntimeHostAdapter {
    // NOT IMPLEMENTED (no truthful owner reachable from this module):
    //
    // blocked_on: `snapshot_source` - `AgentSessionSnapshotSource.children` is
    // `session.getRlmChildSnapshots()`. The snapshot builders
    // `rlm_child_snapshot_for_run` / `rlm_child_snapshot_for_session` are
    // `pub(super)` in `core/agent_session/runtime_members.rs`, so a sibling
    // module cannot call them and `list_rlm_subagents` lacks
    // label/duration/answer-preview/activity/tool-count. Fix: drop `(super)` on
    // those two builders.
    // blocked_on: `session_subscribe` - needs an `AgentSessionEvent` ->
    // `AgentConnectionSessionEvent` projection. `AgentSessionEvent`
    // (core/agent_session.rs) derives only `Debug, Clone` and no converter
    // exists in any slice.
    // blocked_on: `session_headless_completion` - needs an
    // `impl HeadlessCompletionSession for AgentSession`; `waitForRlmQuiescence`,
    // `waitForHeadlessIdle` and `promptHeadlessContinuation` have no public owner
    // on `AgentSession`.
    // blocked_on: `session_context_tree` - needs `AgentSession::getContextTree`;
    // `core/context_tree.rs` carries only the disk-loading helpers.
    // blocked_on: `session_model_catalog` - the seam member is synchronous while
    // the only owner, `ModelRegistry::refresh_model_catalog`, is async and holds
    // its entitlement chain across awaits.
    // blocked_on: `session_cancel_rlm_child` - the only owner is
    // `pub(super) cancel_rlm_child_run(&self, run: &RlmChildRun, reason)`; there is
    // no by-child-id entry point.
    // blocked_on: `session_set_scoped_models` - the owner
    // `AgentSession::set_scoped_models` takes `&mut self` and the session is held
    // as `Arc<AgentSession>`.
    // blocked_on: `session_set_transport` - `SettingsManager::set_transport`
    // persists the preference, but the TypeScript also assigns
    // `session.agent.transport`; `AgentHandle` has no transport setter.
    // blocked_on: `session_compact` - `compact_with_options` returns `()`; the
    // `CompactionResult` the TypeScript returns is produced inside the private
    // `perform_compaction_manual`.
    // blocked_on: `session_refine` - `refine_with_options` is private and
    // `handle_refine_host_request` only queues the request.
    // blocked_on: `session_navigate_tree` - `navigate_tree` returns
    // `Result<(), String>`; the TypeScript `{ editorText, cancelled, aborted }`
    // result is never rebuilt by the Rust owner.
    // blocked_on: `session_export_to_html` / `session_export_to_jsonl` - no owner;
    // `exportSessionToHtml` is unported and the JSONL writer needs the private
    // `iso_now()` plus a header/`parentId` re-chain.
    // blocked_on: `session_watch_child` - needs `AgentSession::getRlmChildSession`,
    // which does not exist.

    fn session_header(&self) -> Option<AgentConnectionSessionHeader> {
        let entry = self.session().session_manager.lock().unwrap().get_header()?;
        serde_json::from_value(Value::Object(entry)).ok()
    }

    fn session_messages(&self) -> Vec<AgentMessage> {
        self.session().state().messages
    }

    fn session_state_messages(&self) -> Vec<AgentMessage> {
        // The TypeScript has one source (`session.state.messages`); both seam
        // members read it.
        self.session().state().messages
    }

    fn session_commands(&self) -> Vec<AgentConnectionSlashCommand> {
        let session = self.session();
        let registered_commands = match session.extension_runner() {
            Some(runner) => runner
                .get_registered_commands()
                .into_iter()
                .map(|resolved| RegisteredCommandEntry {
                    invocation_name: resolved.invocation_name.clone(),
                    name: resolved.command.name.clone(),
                    description: resolved.command.description.clone(),
                    source_info: connection_source_info(&resolved.command.source_info),
                })
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };
        let prompt_templates = session
            .prompt_templates()
            .into_iter()
            .map(|entry| PromptTemplateEntry {
                name: entry.name.clone(),
                description: Some(entry.description.clone()),
                argument_hint: entry.argument_hint.clone(),
                source_info: connection_source_info(&entry.source_info),
            })
            .collect::<Vec<_>>();
        let skills = session
            .resource_loader()
            .get_skills()
            .skills
            .iter()
            .map(|skill| SkillEntry {
                name: skill.name().to_string(),
                description: Some(skill_base(skill).description.clone()),
                source_info: connection_source_info(&skill_base(skill).source_info),
            })
            .collect::<Vec<_>>();
        create_agent_connection_commands(&registered_commands, &prompt_templates, &skills)
    }

    fn session_resource_snapshot(&self) -> AgentConnectionResourceSnapshot {
        let session = self.session();
        let loader: Arc<dyn ResourceLoader> = session.resource_loader();
        let agents_files = loader
            .get_agents_files()
            .agents_files
            .into_iter()
            .map(|entry| AgentsFileEntry { path: entry.path })
            .collect::<Vec<_>>();
        let skills_result = loader.get_skills();
        let resource_skills = skills_result
            .skills
            .iter()
            .map(|skill| ResourceSkillEntry {
                name: skill.name().to_string(),
                description: Some(skill_base(skill).description.clone()),
                file_path: skill.file_path().to_string(),
                source_info: Some(connection_source_info(&skill_base(skill).source_info)),
            })
            .collect::<Vec<_>>();
        let prompts_result = loader.get_prompts();
        let resource_prompts = prompts_result
            .prompts
            .iter()
            .map(|entry| ResourcePromptEntry {
                name: entry.name.clone(),
                description: Some(entry.description.clone()),
                argument_hint: entry.argument_hint.clone(),
                file_path: entry.file_path.clone(),
                source_info: Some(connection_source_info(&entry.source_info)),
            })
            .collect::<Vec<_>>();
        let extensions_result = loader.get_extensions();
        let mut resource_extensions: Vec<ResourceExtensionEntry> = Vec::new();
        for extension in &extensions_result.extensions {
            let extension = extension.lock().unwrap();
            resource_extensions.push(ResourceExtensionEntry {
                path: extension.path.clone(),
                source_info: Some(connection_source_info(&extension.source_info)),
            });
        }
        let extension_errors = extensions_result
            .errors
            .iter()
            .map(|error| SnapshotExtensionLoadError {
                error: error.error.clone(),
                path: Some(error.path.clone()),
            })
            .collect::<Vec<_>>();
        let themes_result = loader.get_themes();
        let resource_themes = themes_result
            .themes
            .iter()
            .map(|theme| ResourceThemeEntry {
                name: theme.name.clone(),
                source_path: theme.source_path.clone(),
                source_info: theme.source_info.as_ref().map(connection_source_info_from_theme),
            })
            .collect::<Vec<_>>();
        let cwd = session.session_manager.lock().unwrap().get_cwd();
        create_agent_connection_resource_snapshot(
            &session.session_id(),
            &cwd,
            &agents_files,
            &resource_skills,
            skills_result
                .diagnostics
                .iter()
                .map(connection_diagnostic)
                .collect(),
            &resource_prompts,
            prompts_result
                .diagnostics
                .iter()
                .map(connection_diagnostic)
                .collect(),
            &resource_extensions,
            &extension_errors,
            &resource_themes,
            themes_result
                .diagnostics
                .iter()
                .map(connection_diagnostic)
                .collect(),
        )
    }

    fn session_available_models(&self) -> Vec<AgentConnectionModel> {
        self.registry_available_models()
    }

    fn session_stats(&self) -> Value {
        let stats = self.session().get_session_stats();
        serde_json::to_value(stats).unwrap_or(Value::Null)
    }

    fn session_context(&self) -> AgentConnectionSessionContext {
        session_context_value(&self.session())
    }

    fn session_tree(&self) -> AgentConnectionWatchSessionTree {
        let tree = session_tree_value(&self.session());
        AgentConnectionWatchSessionTree {
            tree: tree.tree,
            leaf_id: tree.leaf_id,
        }
    }

    fn session_queue(&self) -> AgentConnectionQueueState {
        let session = self.session();
        AgentConnectionQueueState {
            steering: session.get_steering_message_previews(),
            follow_up: session.get_follow_up_message_previews(),
        }
    }

    fn session_mutate_queued_message(
        &self,
        lane: &str,
        index: i64,
        expected_text: &str,
        mutation: Value,
    ) -> String {
        let lane = match serde_json::from_value::<QueuedMessageLane>(Value::String(lane.to_string())) {
            Ok(lane) => lane,
            Err(_) => return QueuedMessageMutationStatus::Invalid.as_str().to_string(),
        };
        let mutation = match serde_json::from_value::<QueuedMessageMutation>(mutation) {
            Ok(mutation) => mutation,
            Err(_) => return QueuedMessageMutationStatus::Invalid.as_str().to_string(),
        };
        self.session()
            .mutate_queued_message(lane, index, expected_text, &mutation)
            .as_str()
            .to_string()
    }

    fn session_clear_queue(&self) -> AgentConnectionQueueState {
        let cleared = self.session().clear_queue();
        AgentConnectionQueueState {
            steering: cleared.steering,
            follow_up: cleared.follow_up,
        }
    }

    fn session_request_abort(&self) {
        self.session().request_abort();
    }

    fn session_acquire_input_pause(&self) -> AgentConnectionSessionInputPause {
        let pause = self.session().acquire_session_input_pause();
        Arc::new(InProcessInputPause { pause })
    }

    fn session_get_user_messages_for_forking(&self) -> Vec<AgentConnectionUserMessage> {
        self.session()
            .get_user_messages_for_forking()
            .into_iter()
            .map(|entry| AgentConnectionUserMessage {
                entry_id: entry.entry_id,
                text: entry.text,
            })
            .collect()
    }

    fn session_last_assistant_text(&self) -> Option<String> {
        let messages = self.session().messages();
        let last_assistant = messages.iter().rev().find_map(|message| match message {
            AgentMessage::Message(pi_ai::types::Message::Assistant(assistant)) => {
                // Skip aborted messages with no content.
                if assistant.stop_reason == pi_ai::types::STOP_REASON_ABORTED
                    && assistant.content.is_empty()
                {
                    None
                } else {
                    Some(assistant)
                }
            }
            _ => None,
        })?;
        let mut text = String::new();
        for block in &last_assistant.content {
            if let pi_ai::types::ContentBlock::Text(text_block) = block {
                text.push_str(&text_block.text);
            }
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn session_system_prompt(&self) -> String {
        self.session().system_prompt()
    }

    fn session_tool_definition(&self, name: &str) -> Option<ConnectionToolDefinition> {
        self.session()
            .get_tool_definition(name)
            .map(|definition| connection_tool_definition(&definition))
    }

    fn session_append_label_change(&self, entry_id: &str, label: Option<&str>) {
        // The seam signature returns unit; `appendLabelChange` reports an unknown
        // entry id, so the failure is logged rather than dropped silently.
        if let Err(error) = self
            .session()
            .session_manager
            .lock()
            .unwrap()
            .append_label_change(entry_id, label)
        {
            eprintln!("Warning: Could not append label change: {error}");
        }
    }

    fn session_prompt(&self, message: &str, options: Value) -> BoxFuture<Result<(), String>> {
        let session = self.session();
        let parsed = prompt_options_from_value(options);
        let message = message.to_string();
        Box::pin(async move { session.prompt(&message, Some(parsed)).await })
    }

    fn session_prompt_and_wait(&self, message: &str, options: Value) -> BoxFuture<Result<(), String>> {
        let session = self.session();
        let parsed = prompt_options_from_value(options);
        let message = message.to_string();
        Box::pin(async move { session.prompt_and_wait(&message, Some(parsed)).await })
    }

    fn session_steer(
        &self,
        message: &str,
        images: Option<Vec<ImageContent>>,
    ) -> BoxFuture<Result<(), String>> {
        let session = self.session();
        let message = message.to_string();
        // `session.steer(message, images, { resumeIfIdle: true })`.
        Box::pin(async move { session.steer(&message, images, None, None, Some(true)).await })
    }

    fn session_follow_up(
        &self,
        message: &str,
        images: Option<Vec<ImageContent>>,
    ) -> BoxFuture<Result<(), String>> {
        let session = self.session();
        let message = message.to_string();
        // `session.followUp(message, images, { resumeIfIdle: true })`.
        Box::pin(async move {
            session
                .follow_up(&message, images, None, None, Some(true))
                .await
                .map(|_queued| ())
        })
    }

    fn session_wait_for_idle(&self) -> BoxFuture<()> {
        let session = self.session();
        Box::pin(async move {
            let _ = session.wait_for_idle().await;
        })
    }

    fn session_run_user_bash(
        &self,
        command: &str,
        options: Option<AgentConnectionExecuteBashOptions>,
    ) -> BoxFuture<Result<(), String>> {
        let session = self.session();
        let command = command.to_string();
        let exclude_from_context = options.and_then(|options| options.exclude_from_context);
        // `transient`/`runId` are bash-event identity fields the session's
        // `runUserBash(command, excludeFromContext)` does not accept.
        Box::pin(async move {
            session
                .run_user_bash(&command, exclude_from_context)
                .await
                .map(|_result| ())
        })
    }

    fn session_execute_bash(&self, command: &str) -> BoxFuture<Result<Value, String>> {
        let session = self.session();
        let command = command.to_string();
        Box::pin(async move {
            let result = session.execute_bash(&command, None, None).await?;
            Ok(bash_result_value(&result))
        })
    }

    fn session_abort_bash(&self) {
        self.session().abort_bash();
    }

    fn session_set_model(&self, model: Model) -> BoxFuture<Result<(), String>> {
        let session = self.session();
        Box::pin(async move { session.set_model(model, ModelSelectOptions::default()).await })
    }

    fn session_model_registry_available_models(&self) -> BoxFuture<Vec<AgentConnectionModel>> {
        let models = self.registry_available_models();
        Box::pin(async move { models })
    }

    fn session_model_registry_provider_auth_source(&self, provider: &str) -> String {
        let registry = self.session().model_registry();
        let registry = registry.lock().unwrap();
        registry
            .get_provider_auth_status(provider)
            .source
            .unwrap_or_default()
    }

    fn session_model_registry_find(&self, provider: &str, model_id: &str) -> Option<Model> {
        let registry = self.session().model_registry();
        let registry = registry.lock().unwrap();
        registry.find(provider, model_id)
    }

    fn session_cycle_model(
        &self,
        direction: &str,
    ) -> BoxFuture<Result<Option<AgentConnectionModelCycleResult>, String>> {
        let session = self.session();
        let direction = if direction == "backward" { -1 } else { 1 };
        Box::pin(async move {
            let result = session
                .cycle_model(Some(direction), ModelSelectOptions::default())
                .await?;
            Ok(Some(AgentConnectionModelCycleResult {
                model: result.model,
                thinking_level: result.thinking_level,
                service_tier: result.service_tier,
                is_scoped: result.is_scoped,
            }))
        })
    }

    fn session_set_thinking_level(&self, level: ThinkingLevel) {
        self.session().set_thinking_level(level);
    }

    fn session_set_service_tier(&self, service_tier: ServiceTier) {
        self.session().set_service_tier(service_tier);
    }

    fn session_cycle_thinking_level(&self) -> Option<ThinkingLevel> {
        self.session().cycle_thinking_level()
    }

    fn session_set_steering_mode(&self, mode: &str) {
        self.session().set_steering_mode(mode);
    }

    fn session_set_follow_up_mode(&self, mode: &str) {
        self.session().set_follow_up_mode(mode);
    }

    fn session_set_auto_compaction_enabled(&self, enabled: bool) {
        self.session().set_auto_compaction_enabled(enabled);
    }

    fn session_set_auto_retry_enabled(&self, enabled: bool) {
        self.session().set_auto_retry_enabled(enabled);
    }

    fn session_abort_compaction(&self) {
        self.session().abort_compaction();
    }

    fn session_abort_branch_summary(&self) {
        self.session().abort_branch_summary();
    }

    fn session_abort_retry(&self) {
        self.session().abort_retry();
    }

    fn session_reload(&self) -> BoxFuture<Result<(), String>> {
        let session = self.session();
        Box::pin(async move { session.reload_with_options(None).await })
    }

    fn session_set_session_name(&self, name: &str) {
        // The connection layer rejects an empty name before this call.
        if let Err(error) = self.session().set_session_name(name) {
            eprintln!("Warning: Could not set session name: {error}");
        }
    }

    fn session_get_rlm_max_depth_status(&self) -> Value {
        let status: RlmMaxDepthStatus = self.session().get_rlm_max_depth_status();
        serde_json::to_value(status).unwrap_or(Value::Null)
    }

    fn session_set_rlm_max_depth(
        &self,
        max_depth: f64,
        options: Option<Value>,
    ) -> BoxFuture<Result<Value, String>> {
        let session = self.session();
        let global = options
            .as_ref()
            .and_then(|options| options.get("global"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Box::pin(async move {
            let result = session.set_rlm_max_depth(max_depth as i64, global).await?;
            Ok(serde_json::to_value(result).unwrap_or(Value::Null))
        })
    }

    fn session_bind_extensions(&self, options: Value) -> BoxFuture<Result<(), String>> {
        let session = self.session();
        let bindings = ExtensionBindings {
            ui_context: options.get("uiContext").cloned(),
            ..Default::default()
        };
        Box::pin(async move { session.bind_extensions(&bindings).await })
    }

    fn runtime_new_session(
        &self,
        options: Option<AgentConnectionNewSessionOptions>,
    ) -> BoxFuture<Result<bool, String>> {
        let runtime = Arc::clone(&self.runtime);
        let parent_session = options.and_then(|options| options.parent_session);
        Box::pin(async move {
            let result = runtime
                .new_session(Some(NewSessionOptionsInput {
                    parent_session,
                    setup: None,
                    with_session: None,
                }))
                .await?;
            Ok(result.cancelled)
        })
    }

    fn runtime_switch_session(
        &self,
        session_path: &str,
        options: Option<AgentConnectionSwitchSessionOptions>,
    ) -> BoxFuture<Result<bool, String>> {
        let runtime = Arc::clone(&self.runtime);
        let session_path = session_path.to_string();
        let cwd_override = options.and_then(|options| options.cwd_override);
        Box::pin(async move {
            let result = runtime
                .switch_session(
                    &session_path,
                    Some(SwitchSessionOptions {
                        cwd_override,
                        with_session: None,
                    }),
                )
                .await?;
            Ok(result.cancelled)
        })
    }

    fn runtime_fork(
        &self,
        entry_id: &str,
        options: Option<AgentConnectionForkOptions>,
    ) -> BoxFuture<Result<Value, String>> {
        let runtime = Arc::clone(&self.runtime);
        let entry_id = entry_id.to_string();
        let position = options.and_then(|options| options.position);
        Box::pin(async move {
            let result = runtime
                .fork(
                    &entry_id,
                    Some(ForkOptionsInput {
                        position,
                        with_session: None,
                    }),
                )
                .await?;
            let mut object = Map::new();
            object.insert("cancelled".to_string(), Value::Bool(result.cancelled));
            if let Some(selected_text) = result.selected_text {
                object.insert("selectedText".to_string(), Value::String(selected_text));
            }
            Ok(Value::Object(object))
        })
    }

    fn runtime_import_from_jsonl(
        &self,
        input_path: &str,
        cwd_override: Option<&str>,
    ) -> BoxFuture<Result<bool, String>> {
        let runtime = Arc::clone(&self.runtime);
        let input_path = input_path.to_string();
        let cwd_override = cwd_override.map(str::to_string);
        Box::pin(async move {
            let result = runtime
                .import_from_jsonl(&input_path, cwd_override.as_deref())
                .await?;
            Ok(result.cancelled)
        })
    }

    fn runtime_set_before_session_invalidate(
        &self,
        listener: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        self.runtime.set_before_session_invalidate(listener);
    }

    fn runtime_set_rebind_session(
        &self,
        listener: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    ) {
        // The runtime's rebind callback receives the replaced session; the
        // connection-layer listener takes no argument.
        let listener = listener.map(|listener| {
            Arc::new(move |_session: Arc<AgentSession>| listener())
                as Arc<dyn Fn(Arc<AgentSession>) -> BoxFuture<()> + Send + Sync>
        });
        self.runtime.set_rebind_session(listener);
    }

    fn runtime_dispose(&self) -> BoxFuture<()> {
        let runtime = Arc::clone(&self.runtime);
        Box::pin(async move {
            // `dispose()` returns a diagnostic result; the seam signature reports
            // nothing, so the failure is logged.
            if let Err(error) = runtime.dispose(None).await {
                eprintln!("Warning: Could not dispose the session runtime: {error}");
            }
        })
    }
}
