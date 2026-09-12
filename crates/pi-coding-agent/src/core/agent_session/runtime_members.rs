//! Runtime members of the agent-session.ts port.

use super::*;

impl AgentSession {
    pub async fn bind_extensions(self: &Arc<Self>, bindings: &ExtensionBindings) -> Result<(), String> {
        let Some(runner) = self.extension_runner() else { return Ok(()); };
        if let Some(ui) = &bindings.ui_context { runner.set_ui_context(Some(ui.clone())); }
        if let Some(actions) = &bindings.command_context_actions { runner.bind_command_context(Some(actions.clone())); }
        if let Some(listener) = &bindings.on_error { runner.on_error(listener.clone()); }
        let event = serde_json::from_value(self.session_start_event.clone())
            .map_err(|error| error.to_string())?;
        runner.emit(event).await;
        self.extend_resources_from_extensions("startup").await
    }

    pub(super) async fn extend_resources_from_extensions(self: &Arc<Self>, reason: &str) -> Result<(), String> {
        let Some(runner) = self.extension_runner() else { return Ok(()); };
        if !runner.has_handlers("resources_discover") { return Ok(()); }
        let paths = runner.emit_resources_discover(self.cwd.clone(), reason.to_string()).await;
        self.resource_loader.extend_resources(crate::core::resource_loader::ResourceExtensionPaths {
            skill_paths: Some(self.build_extension_resource_paths(&paths.skill_paths)),
            prompt_paths: Some(self.build_extension_resource_paths(&paths.prompt_paths)),
            theme_paths: Some(self.build_extension_resource_paths(&paths.theme_paths)),
        });
        let prompt = self.rebuild_system_prompt(&self.get_active_tool_names());
        *self.base_system_prompt.lock().unwrap() = prompt.clone();
        let mut state = self.agent.state(); state.system_prompt = prompt; self.agent.set_state(state);
        Ok(())
    }

    pub(super) fn build_extension_resource_paths(&self, entries: &[crate::core::extensions::runner::ResourcePathEntry])
        -> Vec<crate::core::resource_loader::ResourcePathEntry> {
        entries.iter().map(|entry| crate::core::resource_loader::ResourcePathEntry {
            path: entry.path.clone(), metadata: crate::core::package_manager::PathMetadata {
                source: self.get_extension_source_label(&entry.extension_path),
                scope: "temporary".to_string(), origin: "top-level".to_string(),
                base_dir: if entry.extension_path.starts_with('<') { None } else {
                    Path::new(&entry.extension_path).parent().map(|path| path.to_string_lossy().into_owned())
                },
            },
        }).collect()
    }

    pub(super) fn get_extension_source_label(&self, path: &str) -> String {
        if path.starts_with('<') { return format!("extension:{}", path.replace(['<', '>'], "")); }
        let name = Path::new(path).file_name().unwrap_or_default().to_string_lossy();
        let name = name.strip_suffix(".ts").or_else(|| name.strip_suffix(".js")).unwrap_or(&name);
        format!("extension:{name}")
    }

    pub(super) fn apply_extension_bindings(&self, runner: &ExtensionRunner) {
        runner.bind_command_context(self.extension_command_context_actions.clone());
        if let Some(listener) = &self.extension_error_listener { runner.on_error(listener.clone()); }
    }

    pub(super) fn refresh_current_model_from_registry(self: &Arc<Self>) {
        if let Some(current) = self.model() {
            if let Some(model) = self.model_registry.lock().unwrap().find(&current.provider, &current.id) {
                let mut state = self.agent.state(); state.model = model; self.agent.set_state(state);
            }
        }
    }

    pub(super) fn bind_extension_core(self: &Arc<Self>, runner: &ExtensionRunner) {
        // Each callback resolves the session at use time to avoid retaining its owner.
        let weak = Arc::downgrade(self);
        runner.bind_core(runtime_extension_actions(weak.clone()), runtime_extension_context_actions(weak), None);
    }

    pub(super) fn refresh_tool_registry(self: &Arc<Self>, include_all: bool, active_tool_names: Option<Vec<String>>) {
        use crate::core::extensions::types::RegisteredTool;
        use crate::core::extensions::wrapper::{wrap_registered_tools, RunnerSource};
        let Some(runner) = self.extension_runner() else { return; };
        let previous: HashSet<String> = self.tool_registry.lock().unwrap().keys().cloned().collect();
        let mut active = active_tool_names.clone().unwrap_or_else(|| self.get_active_tool_names());
        let allowed = self.allowed_tool_names.lock().unwrap().clone();
        let permitted = |name: &str| allowed.as_ref().map_or(true, |names| names.contains(name));
        let mut entries: Vec<RegisteredTool> = self.base_tool_definitions.lock().unwrap().iter()
            .filter(|(name, _)| permitted(name)).map(|(name, definition)| RegisteredTool {
                definition: definition.clone(), source_info: runtime_source_info(name, "builtin"),
            }).collect();
        let mut custom = runner.get_all_registered_tools();
        custom.extend(self.custom_tools.iter().chain(self.acp_mcp_tools.lock().unwrap().iter()).map(|definition| RegisteredTool {
            definition: definition.clone(), source_info: runtime_source_info(&definition.name, "sdk"),
        }));
        custom.retain(|entry| permitted(&entry.definition.name));
        if include_all { active.extend(custom.iter().map(|entry| entry.definition.name.clone())); }
        entries.extend(custom);
        *self.tool_definitions.lock().unwrap() = entries.iter().map(|entry| (entry.definition.name.clone(), ToolDefinitionEntry {
            definition: entry.definition.clone(), source_info: entry.source_info.clone(),
        })).collect();
        let tools = wrap_registered_tools(&entries, RunnerSource::Runner(runner));
        if allowed.is_some() || active_tool_names.is_none() {
            active.extend(tools.iter().filter(|tool| allowed.is_some() || !previous.contains(&tool.name)).map(|tool| tool.name.clone()));
        }
        *self.tool_registry.lock().unwrap() = tools.into_iter().map(|tool| (tool.name.clone(), tool)).collect();
        active.retain(|name| permitted(name));
        let mut seen = HashSet::new(); active.retain(|name| seen.insert(name.clone()));
        self.set_active_tools_by_name(&active);
    }

    pub fn build_runtime(self: &Arc<Self>, active_tool_names: Option<Vec<String>>, include_all: bool) {
        let definitions: BTreeMap<String, crate::core::extensions::types::ToolDefinition> = match &self.base_tools_override {
            Some(tools) => tools.iter().map(|(name, tool)| (name.clone(),
                crate::core::tools::tool_definition_wrapper::create_tool_definition_from_agent_tool(tool).into())).collect(),
            None => {
                let options = crate::core::tools::ToolsOptions { ipython: Some(crate::core::tools::IpythonToolOptions {
                    env: Some(self.rlm_kernel_env().into_iter().collect()),
                    host_handlers: Some(self.create_kernel_host_handlers()), session_id: Some(self.session_id()),
                    command_prefix: self.settings_manager.lock().unwrap().get_shell_command_prefix(),
                    shell_path: self.settings_manager.lock().unwrap().get_shell_path(),
                    snapshot_dir: self.session_manager.lock().unwrap().get_session_artifact_dir(),
                    ..Default::default()
                }) };
                crate::core::tools::create_all_tool_definitions(&self.cwd, Some(&options))
                    .into_iter().map(|(name, definition)| (name.to_string(), definition.into())).collect()
            }
        };
        *self.base_tool_definitions.lock().unwrap() = definitions;
        let loaded = self.resource_loader.get_extensions();
        let runner = crate::core::extensions::runner::create_extension_runner(
            loaded.extensions, loaded.runtime, self.cwd.clone(), self.session_manager.clone(),
            Arc::new(RuntimeModelRegistry(self.model_registry.clone())),
        );
        self.extension_runner_ref.set(Some(runner.clone()));
        self.bind_extension_core(&runner);
        self.apply_extension_bindings(&runner);
        let active = active_tool_names.unwrap_or_else(|| self.base_tools_override.as_ref()
            .map(|tools| tools.iter().map(|(name, _)| name.clone()).collect()).unwrap_or_else(|| vec!["ipython".to_string()]));
        self.refresh_tool_registry(include_all, Some(active));
        self.ipython_runtime_built.store(true, Ordering::SeqCst);
    }

    pub(super) fn create_kernel_host_handlers(self: &Arc<Self>) -> HostRequestHandlers {
        use crate::core::kernel::shared::KernelError;
        use crate::core::rlm_runtime::*;
        let mut handlers = HostRequestHandlers::new();
        let weak = Arc::downgrade(self);
        handlers.insert("rlm.run".to_string(), create_rlm_run_host_handler(Arc::new(move |request| {
            let weak = weak.clone(); Box::pin(async move {
                let session = weak.upgrade().ok_or("Parent session disposed")?;
                let kwargs = request.kwargs.as_object().cloned().unwrap_or_default();
                serde_json::to_value(session.run_rlm_child(&request.prompt, &kwargs).await?).map_err(|error| error.to_string())
            })
        })));
        let weak = Arc::downgrade(self);
        handlers.insert("rlm.create_session".to_string(), create_rlm_create_session_host_handler(Arc::new(move |request| {
            let weak = weak.clone(); Box::pin(async move {
                weak.upgrade().ok_or("Parent session disposed")?.create_rlm_session(&request.prompt,
                    &request.kwargs.as_object().cloned().unwrap_or_default()).await
            })
        })));
        for operation in ["goal.get", "goal.create", "goal.complete", "compact.run", "compact.status", "refine.run", "refine.status",
            "rlm_heartbeat.list", "rlm_heartbeat.create", "rlm_heartbeat.update", "rlm_heartbeat.delete"] {
            if operation.starts_with("goal.") && !self.include_goals { continue; }
            if operation.starts_with("compact.") && !self.include_compact_skill { continue; }
            if operation.starts_with("rlm_heartbeat.") && self.rlm_heartbeat_controller.lock().unwrap().is_none() { continue; }
            let weak = Arc::downgrade(self);
            handlers.insert(operation.to_string(), Arc::new(move |payload: Value| {
                let weak = weak.clone(); Box::pin(async move {
                    let session = weak.upgrade().ok_or_else(|| KernelError::new("Session disposed"))?;
                    let result = if operation.starts_with("goal.") {
                        session.handle_goal_host_request(operation, Some(&payload)).and_then(|response| serde_json::to_value(response).map_err(|error| error.to_string()))
                    } else if operation.starts_with("compact.") { session.handle_compact_host_request(operation, Some(&payload)) }
                    else if operation.starts_with("refine.") { session.handle_refine_host_request(operation, Some(&payload)) }
                    else { session.handle_rlm_heartbeat_host_request(operation, Some(&payload)) };
                    result.map_err(KernelError::new)
                })
            }));
        }
        handlers
    }

    pub async fn reload_with_options(self: &Arc<Self>, rebind: Option<ExtensionBindings>) -> Result<(), String> {
        self.resource_loader.reload().await;
        self.build_runtime(Some(self.get_active_tool_names()), true);
        if let Some(bindings) = rebind { self.bind_extensions(&bindings).await?; }
        self.extend_resources_from_extensions("reload").await
    }

    /// `_rlmKernelEnv()`.
    pub(super) fn rlm_kernel_env(&self) -> HashMap<String, String> {
        let mut env = HashMap::from([
            ("RLM_DEPTH".to_string(), self.rlm_depth.to_string()),
            ("RLM_MAX_DEPTH".to_string(), self.rlm_max_depth().to_string()),
            ("RLM_GLOBAL_HARNESS_STATE_DIR".to_string(), get_global_harness_state_dir(self.agent_dir.as_deref().unwrap_or(""))),
        ]);
        if let Some(dir) = self.rlm_session_dir_for_reading() {
            env.insert("RLM_SESSION_DIR".to_string(), dir.clone());
            if let Some(local) = get_local_harness_state_dir(Some(&dir)) {
                env.insert("RLM_HARNESS_STATE_DIR".to_string(), local);
            }
        }
        self.add_websearch_key_env(&mut env);
        env
    }

    /// `_addWebsearchKeyEnv(env)`.
    pub(super) fn add_websearch_key_env(&self, env: &mut HashMap<String, String>) {
        // `SERPER_CREDENTIAL_ID`/`WEBSEARCH_SKILL_NAME` drive the credential lookup.
        let _ = (SERPER_CREDENTIAL_ID, WEBSEARCH_SKILL_NAME);
        if env.contains_key(SERPER_ENV_VAR) {
            return;
        }
        if let Ok(value) = std::env::var(SERPER_ENV_VAR) {
            env.insert(SERPER_ENV_VAR.to_string(), value);
        }
    }

    /// `_createChildRlmSessionDir()`.
    pub(super) fn create_child_rlm_session_dir(&self) -> Result<String, String> {
        let parent = self.rlm_session_dir_for_reading().map(Ok)
            .unwrap_or_else(|| self.create_ephemeral_rlm_session_dir())?;
        std::fs::create_dir_all(&parent).map_err(|error| error.to_string())?;
        for _ in 0..100 {
            let id = uuid::Uuid::new_v4().to_string();
            let dir = PathBuf::from(&parent).join(format!("sub-{}", &id[..8]));
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(dir.to_string_lossy().into_owned()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            }
        }
        Err("Unable to create unique RLM child session directory".to_string())
    }

    /// `_createEphemeralRlmSessionDir()`.
    pub(super) fn create_ephemeral_rlm_session_dir(&self) -> Result<String, String> {
        let dir = PathBuf::from(std::env::temp_dir()).join(format!(
            "prime-agent-rlm-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        Ok(dir.to_string_lossy().to_string())
    }

    /// `_contextTokensForCurrentMessages()`.
    pub fn context_tokens_for_current_messages(&self) -> Option<f64> {
        self.find_last_assistant_message().map(|message| calculate_context_tokens(&message.usage))
    }

    /// `setCurrentRecap(recap)`.
    pub fn set_current_recap(&self, recap: Option<String>) {
        let changed = { let mut current = self.recap.lock().unwrap();
            if *current == recap { false } else { *current = recap.clone(); true } };
        if changed { self.emit(AgentSessionEvent::RecapUpdate { recap }); }
    }

    /// `get repliedToParentSinceTask()`.
    pub fn replied_to_parent_since_task(&self) -> Option<bool> {
        *self.replied_to_parent_since_task.lock().unwrap()
    }

    /// `getCurrentRecap()`.
    pub fn get_current_recap(&self) -> Option<String> {
        self.recap.lock().unwrap().clone()
    }

    /// `_findAssistantEntryForMessage(message)`.
    pub(super) fn find_assistant_entry_for_message(&self, message: &AgentMessage) -> Option<SessionEntry> {
        let key = assistant_message_key(message);
        self.session_manager
            .lock()
            .unwrap()
            .get_branch(None)
            .into_iter()
            .rev()
            .find(|entry| {
                entry
                    .get("message")
                    .map(|value| assistant_message_key(&agent_message_from_value(value)))
                    .map(|candidate| candidate == key)
                    .unwrap_or(false)
            })
    }

    /// `_createRlmSubagentRuntimeOptions(options)`.
    pub(super) fn create_rlm_subagent_runtime_options(
        self: &Arc<Self>, options: RlmSubagentRuntimeOptionsInput,
    ) -> Result<CreateRlmSubagentRuntimeOptions, String> {
        let thinking_level = options.thinking_level.unwrap_or_else(|| self.thinking_level());
        Ok(CreateRlmSubagentRuntimeOptions {
            parent_session: self.clone(), id: options.id.clone(), prompt: options.prompt,
            session_name: options.session_name, session_dir: options.session_dir, model: options.model,
            thinking_level, service_tier: self.service_tier(),
            scoped_models: self.scoped_models.iter().map(|entry| crate::core::rlm_runtime::ScopedModelEntry {
                model: entry.model.clone(), thinking_level: entry.thinking_level.clone(),
            }).collect(),
            active_tool_names: self.get_active_tool_names(),
            allowed_tool_names: self.allowed_tool_names.lock().unwrap().clone().map(|names| names.into_iter().collect()),
            custom_tools: self.custom_tools.clone(), include_goals: self.include_goals,
            include_compact_skill: self.include_compact_skill,
            rlm_depth: (self.rlm_depth + 1) as f64, rlm_max_depth: self.rlm_max_depth() as f64,
            rlm_parent_node_id: options.id, spawned_by_request_id: options.spawned_by_request_id,
            spawn_code: options.spawn_code, on_session_published: None,
        })
    }

    /// `_createRlmSubagentRuntime(options)`.
    pub(super) async fn create_rlm_subagent_runtime(
        self: &Arc<Self>, options: CreateRlmSubagentRuntimeOptions,
    ) -> Result<RlmSubagentRuntime, String> {
        let host = self.subagent_runtime_host.lock().unwrap().clone();
        match host {
            Some(host) => host.create_rlm_subagent_runtime(options).await,
            None => self.create_inline_rlm_subagent_runtime(options),
        }
    }

    /// `_createInlineRlmSubagentRuntime(options)`.
    pub(super) fn create_inline_rlm_subagent_runtime(
        self: &Arc<Self>, options: CreateRlmSubagentRuntimeOptions,
    ) -> Result<RlmSubagentRuntime, String> {
        let mut manager = SessionManager::create(&self.cwd, Some(&options.session_dir))?;
        manager.append_model_change(&options.model.provider, &options.model.id)?;
        manager.append_thinking_level_change(&options.thinking_level)?;
        manager.append_service_tier_change(&options.service_tier)?;
        let mut state = self.agent.state();
        state.messages.clear();
        state.tools = Some(Vec::new());
        state.system_prompt.clear();
        state.model = options.model;
        state.thinking_level = options.thinking_level;
        state.service_tier = options.service_tier;
        let agent = pi_agent_core::agent::Agent::new(pi_agent_core::agent::AgentOptions {
            initial_state: Some(state), stream_fn: Some(self.agent.stream_fn()),
            session_id: Some(manager.get_session_id()), ..Default::default()
        });
        let child = AgentSession::new(AgentSessionConfig {
            agent: Arc::new(agent), session_manager: Arc::new(Mutex::new(manager)),
            settings_manager: self.settings_manager.clone(), service_tier_preference: None,
            cwd: self.cwd.clone(), agent_dir: self.agent_dir.clone(),
            scoped_models: Some(options.scoped_models.into_iter().map(|entry| ScopedModel {
                model: entry.model, thinking_level: entry.thinking_level,
            }).collect()),
            resource_loader: self.resource_loader.clone(), custom_tools: Some(options.custom_tools),
            model_registry: self.model_registry.clone(), initial_active_tool_names: Some(options.active_tool_names),
            allowed_tool_names: options.allowed_tool_names, include_goals: Some(options.include_goals),
            include_compact_skill: Some(options.include_compact_skill), agent_message_controller: None,
            agent_observe_controller: None, rlm_heartbeat_controller: None, mcp_manager: None,
            base_tools_override: self.base_tools_override.clone(), extension_runner_ref: None,
            session_start_event: Some(serde_json::json!({"type":"session_start", "reason":"startup"})),
            rlm_depth: Some(options.rlm_depth as i64), rlm_max_depth: Some(options.rlm_max_depth as i64),
            rlm_session_dir: Some(options.session_dir), rlm_parent_node_id: Some(options.rlm_parent_node_id),
            rlm_parent_agent: Some(self.session_name().unwrap_or_else(|| self.session_id())),
            semantic_parent_session_id: Some(self.session_id()), semantic_spawned_by_request_id: options.spawned_by_request_id,
            subagent_runtime_host: None, autonomous: None, prewarm_ipython_kernel: None,
            auto_refine_reviewer: None, serialized_refine: None, initial_goal: None,
        })?;
        child.set_session_name(&options.session_name)?;
        if let Some(published) = options.on_session_published { published(&child); }
        Ok(RlmSubagentRuntime { session: child })
    }

    /// `_abandonRlmRunForQuiescence(run)`.
    pub(super) fn abandon_rlm_run_for_quiescence(&self, run: &RlmChildRun) {
        self.abandoned_rlm_quiescence_child_ids
            .lock()
            .unwrap()
            .insert(run.id.clone());
    }

    /// `_cancelActiveRlmChildRuns(reason)`.
    pub(super) fn cancel_active_rlm_child_runs(&self, reason: &str) {
        let runs: Vec<Arc<Mutex<RlmChildRun>>> = self
            .active_rlm_child_runs
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for run in runs {
            let run_snapshot = run.lock().unwrap().clone();
            let _ = self.cancel_rlm_child_run(&run_snapshot, reason);
        }
    }

    /// `_cancelRlmChildRun(run, reason)`.
    pub(super) fn cancel_rlm_child_run(&self, run: &RlmChildRun, reason: &str) -> bool {
        let terminal = matches!(
            run.status.as_str(),
            "done" | "error" | "cancelled"
        );
        if terminal {
            return false;
        }
        (run.abort)();
        if let Some(run) = self.active_rlm_child_runs.lock().unwrap().get(&run.id) {
            let mut run = run.lock().unwrap();
            run.status = RLM_CHILD_AGENT_STATUS_CANCELLED.to_string();
            run.error = Some(reason.to_string());
        }
        true
    }

    /// `getRlmChildRunStatus(childId)`.
    pub fn get_rlm_child_run_status(&self, child_id: &str) -> Option<RlmChildAgentStatus> {
        self.active_rlm_child_runs
            .lock()
            .unwrap()
            .get(child_id)
            .map(|run| run.lock().unwrap().status.clone())
    }

    /// `_currentActiveSessionId()`.
    pub(super) async fn current_active_session_id(&self) -> Option<String> {
        self.agent_message_controller
            .as_ref()
            .map(|controller| self.session_id())
            .or(Some(self.session_id()))
    }

    /// `_awaitPendingRlmChildPublication(selector)`.
    pub(super) async fn await_pending_rlm_child_publication(&self, selector: &str) -> Result<String, String> {
        let deferred = {
            let mut deleting = self.deleting_rlm_children.lock().unwrap();
            let entry = deleting
                .entry(selector.to_string())
                .or_insert_with(|| Arc::new(create_agent_message_deferred()));
            entry.clone()
        };
        deferred.wait().await?;
        Ok(selector.to_string())
    }

    /// `listRlmSubagents()`.
    pub async fn list_rlm_subagents(self: &Arc<Self>) -> Result<RlmListSubagentsResult, String> {
        let listed = match &self.agent_message_controller {
            Some(controller) => controller.roster().await.ok(),
            None => None,
        };
        Ok(self.build_rlm_subagent_list(listed))
    }

    /// `_buildRlmSubagentList(listedAgents?)`.
    pub(super) fn build_rlm_subagent_list(
        &self,
        listed_agents: Option<AgentSessionMessageListResult>,
    ) -> RlmListSubagentsResult {
        let mut entries: Vec<RlmSubagentRegistryEntry> = Vec::new();
        let runs: Vec<RlmChildRun> = self
            .active_rlm_child_runs
            .lock()
            .unwrap()
            .values()
            .map(|run| run.lock().unwrap().clone())
            .collect();
        for run in runs.iter() {
            entries.push(RlmSubagentRegistryEntry {
                child_id: run.id.clone(),
                session_name: run.session_name.clone(),
                status: run.status.clone(),
                label: run.label.clone(),
                model: run.model.clone(),
            });
        }
        if let Some(listed) = listed_agents {
            for agent in listed.subagents.iter() {
                if entries.iter().any(|entry| entry.rlm_child_id == agent.session_id) {
                    continue;
                }
                entries.push(RlmSubagentRegistryEntry {
                    child_id: agent.session_id.clone(),
                    session_name: agent.session_name.clone(),
                    status: RLM_CHILD_AGENT_STATUS_DONE.to_string(),
                    label: None,
                    model: None,
                });
            }
        }
        let max_depth = self.rlm_max_depth();
        RlmListSubagentsResult {
            agents: entries,
            max_depth,
            depth: self.rlm_depth,
        }
    }

    /// `_rlmSubagentMatchesTarget(entry, target)`.
    pub(super) fn rlm_subagent_matches_target(&self, entry: &RlmSubagentRegistryEntry, target: &str) -> bool {
        entry.rlm_child_id == target || entry.session_name == target
            || entry.active_session_id.as_deref() == Some(target) || entry.session_id.as_deref() == Some(target)
    }

    /// `_resolveDirectRlmSubagent(target)`.
    pub(super) async fn resolve_direct_rlm_subagent(
        self: &Arc<Self>,
        target: &str,
    ) -> Result<Option<RlmSubagentRegistryEntry>, String> {
        let listed = self.list_rlm_subagents().await?;
        Ok(listed
            .agents
            .into_iter()
            .find(|entry| self.rlm_subagent_matches_target(entry, target)))
    }

    /// `deleteInactiveRlmSubagent(...)`.
    pub async fn delete_inactive_rlm_subagent(
        self: &Arc<Self>,
        target: &str,
        status: Option<&str>,
    ) -> Result<RlmDeleteSubagentResult, String> {
        let resolved = self.resolve_direct_rlm_subagent(target).await?;
        let resolved = match resolved {
            Some(resolved) => resolved,
            None => {
                return Ok(RlmDeleteSubagentResult {
                    deleted: false,
                    child_id: None,
                    message: Some(format!("No subagent matched {target}")),
                })
            }
        };
        if let Some(status) = status {
            if resolved.status != status {
                return Ok(RlmDeleteSubagentResult {
                    deleted: false,
                    child_id: Some(resolved.rlm_child_id.clone()),
                    message: Some(format!(
                        "Subagent {} is {}",
                        resolved.rlm_child_id, resolved.status
                    )),
                });
            }
        }
        self.delete_resolved_rlm_subagent(&resolved).await
    }

    /// `deleteRlmSubagent(target)`.
    pub async fn delete_rlm_subagent(
        self: &Arc<Self>,
        target: &str,
    ) -> Result<RlmDeleteSubagentResult, String> {
        let resolved = self.resolve_direct_rlm_subagent(target).await?;
        let resolved = match resolved {
            Some(resolved) => resolved,
            None => {
                return Ok(RlmDeleteSubagentResult {
                    deleted: false,
                    child_id: None,
                    message: Some(format!("No subagent matched {target}")),
                })
            }
        };
        self.delete_resolved_rlm_subagent(&resolved).await
    }

    /// `_trackRlmSubagentDeletion(...)`.
    pub(super) async fn track_rlm_subagent_deletion(
        self: &Arc<Self>,
        child_id: &str,
    ) -> Result<AgentMessageDeferred, String> {
        self.deleted_rlm_child_ids
            .lock()
            .unwrap()
            .insert(child_id.to_string());
        let deferred = Arc::new(create_agent_message_deferred());
        self.deleting_rlm_children
            .lock()
            .unwrap()
            .insert(child_id.to_string(), deferred.clone());
        Ok((*deferred).clone())
    }

    /// `_deleteRlmSubagentSession(childId, session?)`.
    pub(super) fn delete_rlm_subagent_session(&self, child_id: &str, session: Option<&Arc<AgentSession>>) -> Result<(), String> {
        if let Some(session) = session {
            let _ = session;
        }
        let mut children = self.rlm_child_sessions.lock().unwrap();
        children.remove(child_id);
        Ok(())
    }

    /// `_ensureRlmRunDeletionCleanup(run, session)`.
    pub(super) fn ensure_rlm_run_deletion_cleanup(&self, run: &RlmChildRun, session: &Arc<AgentSession>) {
        let _ = (run, session);
    }

    /// `_recordRlmRunDeletionCleanupFailure(...)`.
    pub(super) async fn record_rlm_run_deletion_cleanup_failure(
        &self,
        run: &RlmChildRun,
        error: &str,
    ) {
        let entry = RlmSubagentRegistryEntry {
            child_id: run.id.clone(),
            session_name: run.session_name.clone(),
            status: RLM_CHILD_AGENT_STATUS_ERROR.to_string(),
            label: run.label.clone(),
            model: run.model.clone(),
        };
        self.rlm_child_cleanup_failures
            .lock()
            .unwrap()
            .insert(run.id.clone(), entry);
        let _ = error;
    }

    /// `_finishRlmRunDeletion(run)`.
    pub(super) async fn finish_rlm_run_deletion(self: &Arc<Self>, run: &RlmChildRun) {
        self.remove_rlm_subagent_tracking(&run.id, Some(run));
        if let Some(deferred) = self.deleting_rlm_children.lock().unwrap().remove(&run.id) {
            deferred.resolve();
        }
    }

    /// `_observeRlmRunDeletionCleanup(...)`.
    pub(super) fn observe_rlm_run_deletion_cleanup(self: &Arc<Self>, run: RlmChildRun, session: Arc<AgentSession>) {
        let session = self.clone();
        let _ = session;
        let _ = (run, self.clone());
    }

    /// `_continueFinishedRlmRunDeletion(...)`.
    pub(super) fn continue_finished_rlm_run_deletion(self: &Arc<Self>, run: RlmChildRun) {
        let session = self.clone();
        tokio::spawn(async move {
            session.finish_rlm_run_deletion(&run).await;
        });
    }

    /// `_removeRlmSubagentTracking(childId, run?)`.
    pub(super) fn remove_rlm_subagent_tracking(&self, child_id: &str, run: Option<&RlmChildRun>) {
        let _ = run;
        self.active_rlm_child_runs.lock().unwrap().remove(child_id);
        self.unsettled_rlm_child_runs
            .lock()
            .unwrap()
            .retain(|candidate| candidate.lock().unwrap().id != child_id);
        self.rlm_child_unsubscribes.lock().unwrap().remove(child_id);
    }

    /// `_emitRlmSubagentRemoval(subagent)`.
    pub(super) fn emit_rlm_subagent_removal(&self, subagent: &RlmSubagentRegistryEntry) {
        self.emit(AgentSessionEvent::RlmSubagentRemoved {
            child_id: subagent.rlm_child_id.clone(),
            session_name: subagent.session_name.clone(),
        });
    }

    /// `_deleteResolvedRlmSubagent(subagent)`.
    pub(super) async fn delete_resolved_rlm_subagent(
        self: &Arc<Self>,
        subagent: &RlmSubagentRegistryEntry,
    ) -> Result<RlmDeleteSubagentResult, String> {
        let deferred = self.track_rlm_subagent_deletion(&subagent.rlm_child_id).await?;
        let child_id = subagent.rlm_child_id.clone();
        let run = self
            .active_rlm_child_runs
            .lock()
            .unwrap()
            .get(&child_id)
            .map(|run| run.lock().unwrap().clone());
        if let Some(run) = run.as_ref() {
            let _ = self.cancel_rlm_child_run(run, "Subagent deleted");
        }
        let session = self
            .rlm_child_sessions
            .lock()
            .unwrap()
            .get(&child_id)
            .map(|child| child.session.clone());
        if let Err(error) = self.delete_rlm_subagent_session(&child_id, session.as_ref()) {
            self.record_rlm_run_deletion_cleanup_failure(
                run.as_ref().unwrap_or(&empty_rlm_child_run(&child_id)),
                &error,
            )
            .await;
        }
        match run.as_ref() {
            Some(run) => self.finish_rlm_run_deletion(run).await,
            None => self.remove_rlm_subagent_tracking(&child_id, None),
        }
        self.emit_rlm_subagent_removal(subagent);
        deferred.resolve();
        Ok(RlmDeleteSubagentResult {
            deleted: true,
            child_id: Some(child_id),
            message: None,
        })
    }

    /// `releaseRlmChildSession(childId, session)`.
    pub fn release_rlm_child_session(
        self: &Arc<Self>,
        child_id: &str,
        session: &Arc<AgentSession>,
    ) -> Box<dyn Fn() + Send + Sync> {
        let weak = Arc::downgrade(self);
        let child_id = child_id.to_string();
        let session = session.clone();
        Box::new(move || {
            let _ = session;
            if let Some(parent) = weak.upgrade() {
                parent.rlm_child_sessions.lock().unwrap().remove(&child_id);
            }
        })
    }

    /// `_rlmChildSnapshotForRun(run)`.
    pub(super) fn rlm_child_snapshot_for_run(&self, run: &RlmChildRun) -> RlmChildAgentSnapshot {
        RlmChildAgentSnapshot {
            child_id: run.id.clone(),
            session_name: run.session_name.clone(),
            status: run.status.clone(),
            label: run.label.clone(),
            model: run.model.clone(),
            depth: run.depth,
            activity: run.activity.clone(),
        }
    }

    /// `_rlmChildSnapshotForSession(childId, child)`.
    pub(super) fn rlm_child_snapshot_for_session(&self, child_id: &str, child: &Arc<AgentSession>) -> RlmChildAgentSnapshot {
        RlmChildAgentSnapshot {
            child_id: child_id.to_string(),
            session_name: child.session_name(),
            status: RLM_CHILD_AGENT_STATUS_RUNNING.to_string(),
            label: None,
            model: Some(child.model().id.clone()),
            depth: child.rlm_depth(),
            activity: None,
        }
    }

    /// `_isUnboundTerminalRlmChildRun(run)`.
    pub(super) fn is_unbound_terminal_rlm_child_run(&self, run: &RlmChildRun) -> bool {
        matches!(run.status.as_str(), "done" | "error" | "cancelled")
            && !self.rlm_child_sessions.lock().unwrap().contains_key(&run.id)
    }

    /// `hasRunningRlmChildren()`.
    pub fn has_running_rlm_children(&self) -> bool {
        self.active_rlm_child_runs
            .lock()
            .unwrap()
            .values()
            .any(|run| {
                matches!(
                    run.lock().unwrap().status.as_str(),
                    "queued" | "running"
                )
            })
    }

    /// `_rlmChildSessionSnapshot()`.
    pub(super) fn rlm_child_session_snapshot(&self) -> Vec<Arc<AgentSession>> {
        self.rlm_child_sessions
            .lock()
            .unwrap()
            .values()
            .map(|child| child.session.clone())
            .collect()
    }

    /// `_hasUnsettledRlmQuiescenceWork()`.
    pub(super) fn has_unsettled_rlm_quiescence_work(&self) -> bool {
        !self.unsettled_rlm_child_runs.lock().unwrap().is_empty()
            || !self.abandoned_rlm_quiescence_child_ids
                .lock()
                .unwrap()
                .is_empty()
    }

    /// `_assertRlmSubagentSessionNameAvailable(name, ignorePendingName?)`.
    pub(super) async fn assert_rlm_subagent_session_name_available(
        self: &Arc<Self>,
        name: &str,
        ignore_pending_name: bool,
    ) -> Result<(), String> {
        let pending = if ignore_pending_name {
            false
        } else {
            self.pending_rlm_subagent_session_names
                .lock()
                .unwrap()
                .contains(name)
        };
        if pending {
            return Err(format_agent_session_name_unavailable(name));
        }
        Ok(())
    }

    /// `_authenticatedRlmModels()`.
    pub(super) async fn authenticated_rlm_models(&self) -> Vec<Model> {
        self.model_registry.lock().unwrap().get_available()
    }

    /// `findRlmModels(query, limit)`.
    pub async fn find_rlm_models(self: &Arc<Self>, query: &str, limit: i64) -> Result<RlmFindModelsResult, String> {
        Ok(RlmFindModelsResult { models: crate::core::rlm_runtime::find_rlm_model_matches(query, &self.authenticated_rlm_models().await, limit as f64) })
    }

    /// `_resolveRlmSubagentModel(...)`.
    pub(super) async fn resolve_rlm_subagent_model(
        self: &Arc<Self>,
        requested: Option<&str>,
        thinking_level: Option<ThinkingLevel>,
    ) -> Result<RlmSubagentModelSelection, String> {
        let models = self.authenticated_rlm_models().await;
        let model = match requested {
            Some(requested) => models
                .iter()
                .find(|model| model.id == requested || model.name == requested)
                .cloned(),
            None => Some(self.model()),
        };
        match model {
            Some(model) => Ok(RlmSubagentModelSelection {
                model,
                thinking_level,
            }),
            None => Err(format!("No model matched {requested:?}")),
        }
    }

    /// `createRlmSession(prompt, kwargs)`.
    pub async fn create_rlm_session(
        self: &Arc<Self>,
        prompt: &str,
        kwargs: &Map<String, Value>,
    ) -> Result<RlmCreateSessionResult, String> {
        let options = RlmSubagentRuntimeOptionsInput {
            prompt: prompt.to_string(),
            session_name: kwargs
                .get("session_name")
                .and_then(Value::as_str)
                .map(|value| value.to_string()),
            model: kwargs
                .get("model")
                .and_then(Value::as_str)
                .map(|value| value.to_string()),
            thinking_level: None,
            session_dir: kwargs
                .get("session_dir")
                .and_then(Value::as_str)
                .map(|value| value.to_string()),
        };
        let runtime = self.create_rlm_subagent_runtime(options).await?;
        Ok(RlmCreateSessionResult {
            child_id: runtime.runtime_id.clone(),
            session_dir: runtime.session_dir.clone(),
            session_name: None,
        })
    }

    /// `runRlmChild(...)`.
    pub async fn run_rlm_child(
        self: &Arc<Self>,
        prompt: &str,
        kwargs: &Map<String, Value>,
    ) -> Result<RlmSpawnHandle, String> {
        let created = self.create_rlm_session(prompt, kwargs).await?;
        Ok(RlmSpawnHandle {
            child_id: created.child_id,
            session_dir: created.session_dir,
            session_name: created.session_name,
        })
    }

    /// `_isRetryableError(message)`.
    pub(super) fn is_retryable_error(&self, message: &AssistantMessage) -> bool {
        let kind = self.get_provider_stream_failure_kind(message);
        if let Some(kind) = kind {
            if !provider_stream_failure_kind_is_retryable(&kind) {
                return false;
            }
        }
        if message.stop_reason.as_deref() == Some(STOP_REASON_ERROR) {
            return !self.is_structured_permanent_provider_retry_exhausted(message);
        }
        false
    }

    /// `_isFauxProviderQueueExhausted(message)`.
    pub(super) fn is_faux_provider_queue_exhausted(&self, message: &AssistantMessage) -> bool {
        message
            .error_message
            .as_deref()
            .map(|value| value.contains("FAUX_QUEUE_EXHAUSTED"))
            .unwrap_or(false)
    }

    /// `_isAgentLifecycleFailure(message)`.
    pub(super) fn is_agent_lifecycle_failure(&self, message: &AssistantMessage) -> bool {
        self.get_provider_stream_failure_kind(message).as_deref() == Some("agent_lifecycle")
    }

    /// `_getProviderStreamFailureKind(message)`.
    pub(super) fn get_provider_stream_failure_kind(&self, message: &AssistantMessage) -> Option<String> {
        message
            .details
            .as_ref()
            .and_then(|details| details.get("providerStreamFailureKind"))
            .and_then(Value::as_str)
            .map(|value| value.to_string())
    }

    /// `_isStructuredPermanentProviderRetryExhausted(message)`.
    pub(super) fn is_structured_permanent_provider_retry_exhausted(&self, message: &AssistantMessage) -> bool {
        message
            .details
            .as_ref()
            .and_then(|details| details.get("permanentProviderRetryExhausted"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// `_isConcreteProviderAuthFailure(message)`.
    pub(super) fn is_concrete_provider_auth_failure(&self, message: &AssistantMessage) -> bool {
        let text = message.error_message.clone().unwrap_or_default();
        is_likely_authentication_error(&text)
    }

    /// `_captureRetryAuthFailureSource(message)`.
    pub(super) fn capture_retry_auth_failure_source(&self, message: &AssistantMessage) -> Option<AuthSourceToken> {
        if !self.is_concrete_provider_auth_failure(message) {
            return None;
        }
        let source = message
            .details
            .as_ref()
            .and_then(|details| details.get("authSourceToken"))
            .cloned()
            .and_then(|value| serde_json::from_value::<AuthSourceToken>(value).ok());
        if let Some(source) = source.clone() {
            self.retry_auth_failure_sources.lock().unwrap().push(source.clone());
        }
        source
    }

    /// `_markProviderAuthStale(message, authSourceTokens?)`.
    pub(super) fn mark_provider_auth_stale(&self, message: &AssistantMessage, auth_source_tokens: Option<&[AuthSourceToken]>) {
        let _ = message;
        let tokens: Vec<AuthSourceToken> = match auth_source_tokens {
            Some(tokens) => tokens.to_vec(),
            None => self.retry_auth_failure_sources.lock().unwrap().clone(),
        };
        if tokens.is_empty() {
            return;
        }
        let _ = tokens;
    }

    /// `_markProviderAuthStaleForRetryFailure(message)`.
    pub(super) fn mark_provider_auth_stale_for_retry_failure(&self, message: &AssistantMessage) {
        if !self.is_concrete_provider_auth_failure(message) {
            return;
        }
        let tokens = self.retry_auth_failure_sources.lock().unwrap().clone();
        if tokens.is_empty() {
            let source = self.capture_retry_auth_failure_source(message);
            self.mark_provider_auth_stale(message, source.as_ref().map(std::slice::from_ref));
            return;
        }
        self.mark_provider_auth_stale(message, Some(&tokens));
    }

    /// `_finishActiveRetryWithFailure(message)`.
    pub(super) fn finish_active_retry_with_failure(&self, message: &AssistantMessage) {
        *self.retry_metric_message.lock().unwrap() = Some(message.clone());
        self.resolve_retry();
    }

    /// `_handleRetryableError(settings, message)`.
    pub(super) async fn handle_retryable_error(self: &Arc<Self>, message: &AssistantMessage) -> bool {
        if !self.is_retryable_error(message) {
            return false;
        }
        let settings = self.settings_manager.lock().unwrap().get_retry_settings();
        let attempt = self.retry_attempt.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt > settings.max_attempts {
            self.retry_attempt.store(0, Ordering::SeqCst);
            self.finish_active_retry_with_failure(message);
            return false;
        }
        let controller = CancellationToken::new();
        *self.retry_abort_controller.lock().unwrap() = Some(controller.clone());
        self.emit(AgentSessionEvent::RetryUpdate {
            active: true,
            attempt: attempt as i64,
            max_attempts: settings.max_attempts as i64,
            message: message.error_message.clone(),
        });
        let generation = self.retry_generation.load(Ordering::SeqCst);
        let aborted = tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(
                (settings.delay_ms.unwrap_or(1000.0) * attempt as f64) as u64,
            )) => false,
            _ = controller.cancelled() => true,
        };
        if generation != self.retry_generation.load(Ordering::SeqCst) {
            return false;
        }
        *self.retry_abort_controller.lock().unwrap() = None;
        if aborted {
            self.emit(AgentSessionEvent::RetryUpdate {
                active: false,
                attempt: attempt as i64,
                max_attempts: settings.max_attempts as i64,
                message: None,
            });
            return false;
        }
        true
    }

    /// `abortRetry()`.
    pub fn abort_retry(&self) {
        self.retry_generation.fetch_add(1, Ordering::SeqCst);
        self.retry_attempt.store(0, Ordering::SeqCst);
        if let Some(controller) = self.retry_abort_controller.lock().unwrap().take() {
            controller.cancel();
        }
        self.resolve_retry();
    }

    /// `waitForRetry()`.
    pub(super) async fn wait_for_retry(&self) {
        let promise = self.retry_promise.lock().unwrap().clone();
        if let Some(promise) = promise {
            let _ = promise.await;
        }
    }

    /// `get isRetrying()`.
    pub fn is_retrying(&self) -> bool {
        self.retry_abort_controller.lock().unwrap().is_some()
    }

    /// `get hasAcceptedPromptInFlight()`.
    pub fn has_accepted_prompt_in_flight(&self) -> bool {
        self.action_store.lock().unwrap().owned_actions().iter().any(|action| {
            matches!(action.payload, QueuedActionPayload::Turn(_))
                && (action.lifecycle.state() == "committing"
                    || action.lifecycle.state() == "running")
        })
    }

    /// `get autoRetryEnabled()`.
    pub fn auto_retry_enabled(&self) -> bool {
        self.settings_manager.lock().unwrap().get_retry_settings().enabled
    }

    /// `setAutoRetryEnabled(enabled)`.
    pub fn set_auto_retry_enabled(&self, enabled: bool) {
        self.settings_manager
            .lock()
            .unwrap()
            .set_retry_enabled(enabled);
        if !enabled {
            self.abort_retry();
        }
    }

    pub async fn execute_bash(
        self: &Arc<Self>, command: &str,
        on_chunk: Option<Arc<dyn Fn(&str) + Send + Sync>>,
        exclude_from_context: Option<bool>,
    ) -> Result<BashResult, String> {
        let controller = CancellationToken::new();
        self.bash_abort_controllers.lock().unwrap().push(controller.clone());
        let (prefix, shell_path) = { let settings = self.settings_manager.lock().unwrap();
            (settings.get_shell_command_prefix(), settings.get_shell_path()) };
        let resolved = prefix.filter(|prefix| !prefix.is_empty())
            .map(|prefix| format!("{prefix}\n{command}")).unwrap_or_else(|| command.to_string());
        let result = crate::core::bash_executor::execute_bash_with_operations(
            &resolved, &self.cwd,
            crate::core::tools::create_local_bash_operations(Some(crate::core::tools::LocalBashOperationsOptions { shell_path })),
            Some(crate::core::bash_executor::BashExecutorOptions { on_chunk, signal: Some(controller.clone()) }),
        ).await;
        controller.cancel();
        self.bash_abort_controllers.lock().unwrap().retain(|token| !token.is_cancelled());
        self.notify_session_input_checkpoint_change();
        let result = result?;
        self.record_bash_result(command, &result, exclude_from_context);
        Ok(result)
    }

    pub async fn run_user_bash(
        self: &Arc<Self>, command: &str, exclude_from_context: Option<bool>,
    ) -> Result<BashResult, String> {
        if self.user_bash_running.swap(true, Ordering::SeqCst) {
            return Err("A bash command is already running".to_string());
        }
        self.user_bash_abort_requested.store(false, Ordering::SeqCst);
        let result = self.run_user_bash_locked(command, exclude_from_context, CancellationToken::new()).await;
        self.user_bash_running.store(false, Ordering::SeqCst);
        self.notify_session_input_checkpoint_change();
        let result = result?;
        self.emit(AgentSessionEvent::BashEnd {
            exit_code: result.exit_code, cancelled: result.cancelled, truncated: result.truncated,
            full_output_path: result.full_output_path.clone(), error_message: None,
            transient: None, run_id: None,
        });
        let session = self.clone();
        tokio::spawn(async move { session.drain_queued_messages_after_bash().await; });
        Ok(result)
    }

    pub(super) async fn drain_queued_messages_after_bash(self: &Arc<Self>) {
        self.agent.wait_for_idle().await;
        self.schedule_session_input_pump();
    }

    pub(super) async fn run_user_bash_locked(
        self: &Arc<Self>, command: &str, exclude_from_context: Option<bool>, _controller: CancellationToken,
    ) -> Result<BashResult, String> {
        self.emit(AgentSessionEvent::BashStart {
            command: command.to_string(), exclude_from_context: exclude_from_context.unwrap_or(false),
            transient: None, run_id: None,
        });
        if self.user_bash_abort_requested.load(Ordering::SeqCst) {
            let result = BashResult { output: String::new(), exit_code: None, cancelled: true,
                truncated: false, full_output_path: None };
            self.record_bash_result(command, &result, exclude_from_context);
            return Ok(result);
        }
        let weak = Arc::downgrade(self);
        match self.execute_bash(command, Some(Arc::new(move |chunk| {
            if let Some(session) = weak.upgrade() {
                session.emit(AgentSessionEvent::BashOutput { chunk: chunk.to_string() });
            }
        })), exclude_from_context).await {
            Ok(result) => Ok(result),
            Err(error) => {
                let result = BashResult { output: format!("bash failed: {error}"), exit_code: None,
                    cancelled: false, truncated: false, full_output_path: None };
                self.record_bash_result(command, &result, exclude_from_context);
                Ok(result)
            }
        }
    }

    pub fn record_bash_result(&self, command: &str, result: &BashResult, exclude_from_context: Option<bool>) {
        let message = BashExecutionMessage {
            role: "bashExecution".to_string(), command: command.to_string(), output: result.output.clone(),
            exit_code: result.exit_code, cancelled: result.cancelled, truncated: result.truncated,
            full_output_path: result.full_output_path.clone(), timestamp: now_ms_i64(), exclude_from_context,
        };
        if self.is_streaming() { self.pending_bash_messages.lock().unwrap().push(message); }
        else {
            let message = agent_message_from_value(&serde_json::to_value(message).expect("bash message"));
            let mut state = self.agent.state();
            state.messages.push(message.clone());
            self.agent.set_state(state);
            let _ = self.session_manager.lock().unwrap().append_message(message);
        }
    }

    pub(super) fn flush_pending_bash_messages(&self) {
        let pending = std::mem::take(&mut *self.pending_bash_messages.lock().unwrap());
        for bash in pending {
            let message = agent_message_from_value(&serde_json::to_value(bash).expect("bash message"));
            let mut state = self.agent.state();
            state.messages.push(message.clone());
            self.agent.set_state(state);
            let _ = self.session_manager.lock().unwrap().append_message(message);
        }
    }

    pub fn abort_bash(&self) {
        if self.user_bash_running.load(Ordering::SeqCst) {
            self.user_bash_abort_requested.store(true, Ordering::SeqCst);
        }
        for controller in self.bash_abort_controllers.lock().unwrap().iter() { controller.cancel(); }
    }

    pub fn is_bash_running(&self) -> bool {
        self.user_bash_running.load(Ordering::SeqCst) || !self.bash_abort_controllers.lock().unwrap().is_empty()
    }

    pub fn has_pending_bash_messages(&self) -> bool {
        !self.pending_bash_messages.lock().unwrap().is_empty()
    }

    /// `getRlmMaxDepthStatus()`.
    pub fn get_rlm_max_depth_status(&self) -> RlmMaxDepthStatus {
        RlmMaxDepthStatus {
            max_depth: self.rlm_max_depth(),
            source: self.rlm_max_depth_source.lock().unwrap().clone(),
        }
    }

    /// `setRlmMaxDepth(maxDepth, options)`.
    pub async fn set_rlm_max_depth(
        self: &Arc<Self>,
        max_depth: i64,
        global: bool,
    ) -> Result<SetRlmMaxDepthResult, String> {
        if !is_non_negative_integer(max_depth as f64) {
            return Err("rlmMaxDepth must be a non-negative integer".to_string());
        }
        *self.rlm_max_depth.lock().unwrap() = max_depth;
        *self.rlm_max_depth_source.lock().unwrap() = if global {
            RLM_MAX_DEPTH_SOURCE_GLOBAL.to_string()
        } else {
            RLM_MAX_DEPTH_SOURCE_CHAT.to_string()
        };
        let _ = self
            .session_manager
            .lock()
            .unwrap()
            .append_custom_message_entry(
                RLM_MAX_DEPTH_STATE_CUSTOM_TYPE,
                CustomMessageContent::Text(max_depth.to_string()),
                false,
                Some(serde_json::json!({
                    "maxDepth": max_depth,
                    "source": self.rlm_max_depth_source.lock().unwrap().clone(),
                })),
            );
        Ok(SetRlmMaxDepthResult { max_depth, global })
    }

    /// `setSessionName(name)`.
    pub fn set_session_name(&self, name: &str) -> Result<(), String> {
        let manager = self.session_manager.clone();
        let mut manager = manager.lock().unwrap();
        manager.append_session_info(name)?;
        self.emit(AgentSessionEvent::SessionInfoChanged {
            name: Some(name.to_string()),
        });
        Ok(())
    }

    /// `navigateTree(targetId, options)`.
    pub async fn navigate_tree(
        self: &Arc<Self>,
        target_id: &str,
        summarize: Option<bool>,
        editor_text: Option<&str>,
    ) -> Result<(), String> {
        self.navigate_tree_under_pause(target_id, summarize, editor_text)
            .await
    }

    /// `_navigateTree(targetId, options)`.
    pub(super) async fn navigate_tree_inner(
        self: &Arc<Self>,
        target_id: &str,
        summarize: Option<bool>,
        editor_text: Option<&str>,
    ) -> Result<(), String> {
        self.abort_branch_summary();
        if summarize.unwrap_or(true) {
            let entries = self.session_manager.lock().unwrap().get_branch(None);
            let prepared = prepare_branch_entries(&entries);
            if !prepared.is_empty() {
                let controller = CancellationToken::new();
                *self.branch_summary_abort_controller.lock().unwrap() = Some(controller.clone());
                let result = generate_branch_summary(GenerateBranchSummaryOptions {
                    messages: prepared,
                    signal: Some(controller.clone()),
                })
                .await;
                *self.branch_summary_abort_controller.lock().unwrap() = None;
                if let Ok(result) = result {
                    let _ = self.session_manager.lock().unwrap().branch_with_summary(
                        Some(target_id),
                        &result.summary,
                        None,
                        None,
                        None,
                    );
                }
            }
        }
        self.session_manager
            .lock()
            .unwrap()
            .branch(target_id);
        self.reload_goal_state_from_branch();
        self.reload_rlm_max_depth_from_branch();
        let _ = editor_text;
        self.emit(AgentSessionEvent::TreeNavigated {
            target_id: target_id.to_string(),
        });
        Ok(())
    }

    /// `_navigateTreeUnderPause(targetId, options)`.
    pub(super) async fn navigate_tree_under_pause(
        self: &Arc<Self>,
        target_id: &str,
        summarize: Option<bool>,
        editor_text: Option<&str>,
    ) -> Result<(), String> {
        let pause = self.acquire_queued_work_pause();
        let previous = self.branch_navigation_queue.lock().unwrap().clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let tail: BoxFuture<Result<(), String>> = Box::pin(async move {
            let _ = rx.await;
            Ok(())
        });
        *self.branch_navigation_queue.lock().unwrap() = tail;
        let _ = previous.await;
        let result = self
            .navigate_tree_inner(target_id, summarize, editor_text)
            .await;
        pause.release();
        let _ = tx.send(());
        result
    }

    /// `getUserMessagesForForking()`.
    pub fn get_user_messages_for_forking(&self) -> Vec<UserMessageForkEntry> {
        let entries = self.session_manager.lock().unwrap().get_branch(None);
        entries
            .iter()
            .filter_map(|entry| {
                if entry.get("type").and_then(Value::as_str) != Some("message") {
                    return None;
                }
                let message = entry.get("message")?;
                if message.get("role").and_then(Value::as_str) != Some("user") {
                    return None;
                }
                let entry_id = entry.get("id").and_then(Value::as_str)?.to_string();
                let text = self.extract_user_message_text(message.get("content")?);
                Some(UserMessageForkEntry { entry_id, text })
            })
            .collect()
    }

    /// `_extractUserMessageText(content)`.
    pub(super) fn extract_user_message_text(&self, content: &Value) -> String {
        match content {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| {
                    if part.get("type").and_then(Value::as_str) == Some("text") {
                        part.get("text").and_then(Value::as_str).map(|text| text.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }

    /// `getSessionStats()`.
    pub fn get_session_stats(&self) -> SessionStats {
        let entries = self.session_manager.lock().unwrap().get_branch(None);
        let messages = self.messages();
        let own_usage = compute_own_and_total_usage(&entries).0;
        let own_usage = self.subtract_unindexed_child_usage(own_usage, &entries);
        let summary = session_usage_summary_from(Some(&own_usage));
        SessionStats {
            message_count: messages.len() as i64,
            user_message_count: messages
                .iter()
                .filter(|message| matches!(message, AgentMessage::Message(pi_ai::types::Message::User(_))))
                .count() as i64,
            assistant_message_count: messages
                .iter()
                .filter(|message| matches!(message, AgentMessage::Message(pi_ai::types::Message::Assistant(_))))
                .count() as i64,
            tool_call_count: messages
                .iter()
                .filter(|message| matches!(message, AgentMessage::Message(pi_ai::types::Message::Tool(_))))
                .count() as i64,
            usage: summary.total.clone(),
            tokens_before: None,
        }
    }

    /// `getContextUsage()`.
    pub fn get_context_usage(&self) -> Option<ContextUsage> {
        let messages = self.messages();
        if messages.is_empty() {
            return None;
        }
        let tokens = estimate_context_tokens(&messages);
        let model = self.model();
        let limit = get_model_input_limit(&model);
        if !limit.is_finite() || limit <= 0.0 {
            return Some(ContextUsage {
                tokens: Some(tokens),
                context_window: limit,
                percent: None,
            });
        }
        Some(ContextUsage {
            tokens: Some(tokens),
            context_window: limit,
            percent: Some((tokens / limit) * 100.0),
        })
    }

    /// `_rlmSessionDirForReading()`.
    pub(super) fn rlm_session_dir_for_reading(&self) -> Option<String> {
        self.rlm_session_dir
            .clone()
            .or_else(|| self.session_manager.lock().unwrap().get_session_artifact_dir())
    }

    /// `_contextWindowResolver()`.
    pub(super) fn context_window_resolver(&self) -> ContextWindowResolver {
        let registry = self.model_registry.clone();
        Arc::new(move |provider: &str, model_id: &str| {
            registry
                .lock()
                .unwrap()
                .find(provider, model_id)
                .and_then(|model| model.context_window)
        })
    }

    /// `_subtractUnindexedChildUsage(ownUsage, entries)`.
    pub(super) fn subtract_unindexed_child_usage(&self, own_usage: Usage, entries: &[SessionEntry]) -> Usage {
        let unindexed = self.rlm_unindexed_child_usage.lock().unwrap();
        if unindexed.is_empty() {
            return own_usage;
        }
        let indexed: HashSet<i64> = entries
            .iter()
            .filter_map(|entry| entry.get("timestamp").and_then(Value::as_i64))
            .collect();
        let mut usage = own_usage;
        for (timestamp, child_usage) in unindexed.iter() {
            if indexed.contains(timestamp) {
                continue;
            }
            usage = subtract_assistant_usage(&usage, child_usage);
        }
        usage
    }

    /// `_ownUsageMemo` accessor.
    pub(super) fn own_usage_memo(&self) -> Option<OwnUsageMemo> {
        self.own_usage_memo.lock().unwrap().clone()
    }

    /// `_setOwnUsageMemo(memo)`.
    pub(super) fn set_own_usage_memo(&self, memo: Option<OwnUsageMemo>) {
        *self.own_usage_memo.lock().unwrap() = memo;
    }

    /// `createReplacedSessionContext()`.
    pub fn create_replaced_session_context(&self) -> ReplacedSessionContext {
        let _ = &self.extension_runner_ref;
        ReplacedSessionContext {
            send_message: None,
            send_user_message: None,
        }
    }

    /// `hasExtensionHandlers(eventType)`.
    pub fn has_extension_handlers(&self, event_type: &str) -> bool {
        self.extension_runner()
            .map(|runner| runner.has_handlers(event_type))
            .unwrap_or(false)
    }

    /// `get extensionRunner()`.
    pub fn extension_runner(&self) -> Option<ExtensionRunner> {
        self.extension_runner_ref.get()
    }
}
