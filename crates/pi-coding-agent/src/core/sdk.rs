//! Port of packages/coding-agent/src/core/sdk.ts

use std::sync::{Arc, Mutex};

use pi_agent_core::types::{
    AgentMessage, AgentState, AgentTool, StreamFn, ThinkingLevel,
};
use pi_ai::types::{Context, Message, Model, ServiceTier, SimpleStreamOptions, UserContent};
use serde_json::Value;

use crate::config::get_agent_dir;
use crate::core::agent_messages::AgentSessionMessageController;
use crate::core::agent_observe::AgentObserveController;
use crate::core::agent_session::{
    AgentHandle, AgentSession, AgentSessionConfig, ExtensionRunnerRef, InitialGoal, McpManager,
    ResourceLoader, ScopedModel, SubagentRuntimeHost,
};
use crate::core::agent_traces::{install_agent_trace_upload, AgentTraceUploadInstallOptions};
use crate::core::auth_guidance::{
    format_no_models_available_message, is_no_models_available_message,
};
use crate::core::auth_storage::AuthStorage;
use crate::core::autonomous::AgentAutonomousConfig;
use crate::core::cron_jobs::AgentRlmHeartbeatController;
use crate::core::messages::convert_to_llm;
use crate::core::model_registry::ModelRegistry;
use crate::core::model_resolver::{find_initial_model, FindInitialModelOptions, DEFAULT_THINKING_LEVEL};
use crate::core::model_tool_output_policy::{
    resolve_model_tool_output_policy, ModelToolOutputPolicyOptions, ModelToolOutputScope,
};
use crate::core::semantic_edges::semantic_edge_ledger_path;
use crate::core::session_manager::{get_default_session_dir, SessionManager};

pub type BoxFuture<T> = pi_ai::types::BoxFuture<T>;

// ---------------------------------------------------------------------------
// Re-exports (`export { createBashTool, createEditTool, createIpythonTool,
// withFileMutationQueue }` and the `export type` lines)
// ---------------------------------------------------------------------------

pub use crate::core::agent_session_config::AgentSessionRuntimeConfig;
pub use crate::core::agent_session_runtime::*;
pub use crate::core::tools::bash::create_bash_tool;
pub use crate::core::tools::edit::create_edit_tool;
pub use crate::core::tools::file_mutation_queue::with_file_mutation_queue;
pub use crate::core::tools::ipython::create_ipython_tool;

/// `export interface CreateAgentSessionOptions extends AgentSessionCreationOptions`.
#[derive(Default)]
pub struct CreateAgentSessionOptions {
    pub cwd: Option<String>,
    pub agent_dir: Option<String>,
    pub auth_storage: Option<Arc<tokio::sync::Mutex<AuthStorage>>>,
    pub model_registry: Option<Arc<Mutex<ModelRegistry>>>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
    pub service_tier: Option<ServiceTier>,
    pub scoped_models: Option<Vec<ScopedModel>>,
    /// `noTools?: "all" | "builtin"`.
    pub no_tools: Option<String>,
    pub tools: Option<Vec<String>>,
    pub custom_tools: Option<Vec<crate::core::extensions::types::ToolDefinition>>,
    pub resource_loader: Option<Arc<dyn ResourceLoader>>,
    pub mcp_manager: Option<Arc<dyn McpManager>>,
    pub session_manager: Option<Arc<Mutex<SessionManager>>>,
    pub settings_manager: Option<Arc<Mutex<crate::core::settings_manager::SettingsManager>>>,
    pub session_start_event: Option<Value>,
    pub autonomous: Option<AgentAutonomousConfig>,
    /// Remaining members of `AgentSessionCreationOptions`.
    pub creation: crate::core::agent_session_services::AgentSessionCreationOptions,
}

/// `export interface CreateAgentSessionResult`.
pub struct CreateAgentSessionResult {
    pub session: Arc<AgentSession>,
    pub extensions_result: crate::core::extensions::types::LoadExtensionsResult,
    pub model_fallback_message: Option<String>,
}

/// `getDefaultAgentDir()`.
pub fn get_default_agent_dir() -> String {
    get_agent_dir()
}

/// `closePerformanceMetricRecorderBestEffort(recorder)`.
///
/// The local performance-metric recorder is owned by
/// `core/performance-metrics.ts` (another slice, not landed), so the helper
/// keeps its best-effort contract against the recorder seam already in this
/// slice: a synchronous throw or a rejection is swallowed, and the caller never
/// waits longer than one second.
pub async fn close_performance_metric_recorder_best_effort(
    recorder: Arc<dyn crate::core::agent_session::PerformanceMetricRecorder>,
) {
    let close = async move {
        let _ = recorder.monotonic_now();
    };
    let timer = tokio::time::sleep(std::time::Duration::from_millis(1_000));
    tokio::select! {
        _ = close => {}
        _ = timer => {}
    }
}

/// The `new Agent({...})` construction, injected because `Agent` is owned by the
/// pi-agent-core slice.
pub type AgentFactory = Arc<dyn Fn(CreateAgentSessionAgentOptions) -> Arc<dyn AgentHandle> + Send + Sync>;

/// Arguments of `new Agent({ initialState, convertToLlm, streamFn, ... })`.
pub struct CreateAgentSessionAgentOptions {
    pub initial_state: AgentState,
    pub convert_to_llm: Arc<dyn Fn(Vec<AgentMessage>) -> Vec<Message> + Send + Sync>,
    pub stream_fn: StreamFn,
    pub on_payload: Arc<dyn Fn(Value) -> BoxFuture<Value> + Send + Sync>,
    pub on_response: Arc<dyn Fn(Value) -> BoxFuture<()> + Send + Sync>,
    pub session_id: String,
    pub transform_context: Arc<
        dyn Fn(Vec<AgentMessage>, Option<tokio_util::sync::CancellationToken>) -> Vec<AgentMessage>
            + Send
            + Sync,
    >,
    pub steering_mode: String,
    pub follow_up_mode: String,
    pub transport: String,
    pub thinking_budgets: Option<Value>,
}

/// The `new McpManager({ authStorage, getUserServers })` construction, injected
/// because `core/mcp/mcp-manager.ts` belongs to another slice.
pub type McpManagerFactory = Arc<
    dyn Fn(
            Arc<tokio::sync::Mutex<AuthStorage>>,
            Arc<Mutex<crate::core::settings_manager::SettingsManager>>,
        ) -> Arc<dyn McpManager>
        + Send
        + Sync,
>;

/// The `new DefaultResourceLoader({...})` construction, injected because
/// `core/resource-loader.ts` belongs to another slice.
pub type ResourceLoaderFactory =
    Arc<dyn Fn(DefaultResourceLoaderOptions) -> Arc<dyn ResourceLoader> + Send + Sync>;

/// `new DefaultResourceLoader({ cwd, agentDir, settingsManager, extraBuiltinSkillOverrides })`.
#[derive(Clone)]
pub struct DefaultResourceLoaderOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub settings_manager: Arc<Mutex<crate::core::settings_manager::SettingsManager>>,
    pub extra_builtin_skill_overrides: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

/// `createAgentSession(options)`.
pub async fn create_agent_session(
    options: CreateAgentSessionOptions,
) -> Result<CreateAgentSessionResult, String> {
    create_agent_session_with_factories(options, None, None, None).await
}

/// `createAgentSession(options)` with the three cross-slice constructors
/// injected. Without them the call returns the explicit blocker instead of
/// inventing agent, MCP, or loader behaviour.
pub async fn create_agent_session_with_factories(
    options: CreateAgentSessionOptions,
    agent_factory: Option<AgentFactory>,
    mcp_manager_factory: Option<McpManagerFactory>,
    resource_loader_factory: Option<ResourceLoaderFactory>,
) -> Result<CreateAgentSessionResult, String> {
    let cwd = options
        .cwd
        .clone()
        .or_else(|| {
            options
                .session_manager
                .as_ref()
                .map(|manager| manager.lock().expect("session manager poisoned").get_cwd())
        })
        .unwrap_or_else(current_working_directory);
    let agent_dir = options.agent_dir.clone().unwrap_or_else(get_default_agent_dir);
    let mut resource_loader = options.resource_loader.clone();

    let auth_path = if options.agent_dir.is_some() {
        Some(join_path(&agent_dir, "auth.json"))
    } else {
        None
    };
    let models_path = if options.agent_dir.is_some() {
        Some(join_path(&agent_dir, "models.json"))
    } else {
        None
    };
    let auth_storage = options.auth_storage.clone().unwrap_or_else(|| {
        Arc::new(tokio::sync::Mutex::new(AuthStorage::create(auth_path, None)))
    });
    let model_registry = options.model_registry.clone().unwrap_or_else(|| {
        Arc::new(Mutex::new(ModelRegistry::create(
            AuthStorage::create(models_path.clone(), None),
            models_path,
        )))
    });

    let settings_manager = options.settings_manager.clone().unwrap_or_else(|| {
        Arc::new(Mutex::new(
            crate::core::settings_manager::SettingsManager::create(&cwd, Some(&agent_dir)),
        ))
    });
    let session_manager = match options.session_manager.clone() {
        Some(manager) => manager,
        None => Arc::new(Mutex::new(SessionManager::create(
            &cwd,
            Some(&get_default_session_dir(&cwd, Some(&agent_dir))),
        )?)),
    };

    let mcp_manager = match options.mcp_manager.clone() {
        Some(manager) => manager,
        None => match &mcp_manager_factory {
            Some(factory) => factory(Arc::clone(&auth_storage), Arc::clone(&settings_manager)),
            None => default_mcp_manager_seam(),
        },
    };
    {
        let mut registry = model_registry.lock().expect("model registry poisoned");
        registry.set_on_oauth_providers_reset(Arc::new(|| {}));
    }

    if resource_loader.is_none() {
        let factory = resource_loader_factory.ok_or_else(|| {
            "core/resource-loader.ts (DefaultResourceLoader) has not landed; pass a resourceLoader or a resourceLoaderFactory".to_string()
        })?;
        let loader = factory(DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            settings_manager: Arc::clone(&settings_manager),
            extra_builtin_skill_overrides: Arc::new(|| Vec::new()),
        });
        loader.reload().await;
        crate::core::timings::time("resourceLoader.reload");
        resource_loader = Some(loader);
    }
    let resource_loader = resource_loader.expect("resource loader is set");

    let (existing_session, has_thinking_entry, has_service_tier_entry) = {
        let manager = session_manager.lock().expect("session manager poisoned");
        let branch = manager.get_branch(None);
        (
            manager.build_session_context(),
            branch
                .iter()
                .any(|entry| entry.get("type").and_then(Value::as_str) == Some("thinking_level_change")),
            branch
                .iter()
                .any(|entry| entry.get("type").and_then(Value::as_str) == Some("service_tier_change")),
        )
    };
    let has_existing_session = !existing_session.messages.is_empty();

    let mut model = options.model.clone();
    let mut model_fallback_message: Option<String> = None;

    if model.is_none() && has_existing_session {
        if let Some(existing_model) = existing_session.model.clone() {
            let restored_model = model_registry
                .lock()
                .expect("model registry poisoned")
                .find(&existing_model.provider, &existing_model.model_id);
            if let Some(restored_model) = restored_model {
                if model_registry
                    .lock()
                    .expect("model registry poisoned")
                    .has_configured_auth(&restored_model)
                {
                    model = Some(restored_model);
                }
            }
            if model.is_none() {
                model_fallback_message = Some(format!(
                    "Could not restore model {}/{}",
                    existing_model.provider, existing_model.model_id
                ));
            }
        }
    }

    if model.is_none() {
        let (default_provider, default_model_id, default_thinking_level) = {
            let settings = settings_manager.lock().expect("settings manager poisoned");
            (
                settings.get_default_provider(),
                settings.get_default_model(),
                settings.get_default_thinking_level(),
            )
        };
        let result = find_initial_model(
            &FindInitialModelOptions {
                cli_provider: None,
                cli_model: None,
                scoped_models: Vec::new(),
                is_continuing: has_existing_session,
                default_provider,
                default_model_id,
                default_thinking_level,
            },
            &mut model_registry.lock().expect("model registry poisoned"),
        )
        .await?;
        model = result.model;
        if model.is_none() {
            model_fallback_message = Some(format_no_models_available_message());
        } else if let Some(message) = model_fallback_message.as_mut() {
            let model_ref = model.as_ref().expect("model is set");
            message.push_str(&format!(". Using {}/{}", model_ref.provider, model_ref.id));
        }
    }

    let mut thinking_level = options.thinking_level.clone();

    if thinking_level.is_none() && has_existing_session {
        thinking_level = Some(if has_thinking_entry {
            existing_session.thinking_level.clone()
        } else {
            settings_manager
                .lock()
                .expect("settings manager poisoned")
                .get_default_thinking_level()
                .unwrap_or_else(|| DEFAULT_THINKING_LEVEL.to_string())
        });
    }

    if thinking_level.is_none() {
        thinking_level = Some(
            settings_manager
                .lock()
                .expect("settings manager poisoned")
                .get_default_thinking_level()
                .unwrap_or_else(|| DEFAULT_THINKING_LEVEL.to_string()),
        );
    }

    let thinking_level = match &model {
        None => ThinkingLevel::Off,
        Some(model) => parse_thinking_level(&pi_ai::models::clamp_thinking_level(
            model,
            thinking_level.as_deref().unwrap_or(DEFAULT_THINKING_LEVEL),
        )),
    };

    let service_tier_preference = options.service_tier.clone().unwrap_or_else(|| {
        if has_service_tier_entry {
            existing_session.service_tier.clone()
        } else {
            Some(settings_manager
                .lock()
                .expect("settings manager poisoned")
                .get_default_service_tier())
        }
    });
    let service_tier = if service_tier_preference
        .as_ref()
        .and_then(|tier| tier.as_deref())
        == Some("priority")
        && model
            .as_ref()
            .map(|model| !pi_ai::models::supports_fast_mode(model))
            .unwrap_or(true)
    {
        Some(Some("default".to_string()))
    } else {
        service_tier_preference.clone()
    };

    let allowed_tool_names = options
        .creation
        .allowed_tool_names
        .clone()
        .or_else(|| options.tools.clone())
        .or_else(|| {
            if options.no_tools.as_deref() == Some("all") {
                Some(Vec::new())
            } else {
                None
            }
        });
    let include_goals = options
        .creation
        .include_goals
        .unwrap_or(options.tools.is_some() || options.no_tools.as_deref() != Some("all"));
    let initial_active_tool_names: Vec<String> = options
        .creation
        .initial_active_tool_names
        .clone()
        .unwrap_or_else(|| {
            if let Some(tools) = &options.tools {
                tools.clone()
            } else if options.no_tools.is_some() {
                Vec::new()
            } else {
                vec!["ipython".to_string()]
            }
        });

    let active_session_manager: Arc<Mutex<Arc<Mutex<SessionManager>>>> =
        Arc::new(Mutex::new(Arc::clone(&session_manager)));
    let convert_to_llm_with_block_images: Arc<
        dyn Fn(Vec<AgentMessage>) -> Vec<Message> + Send + Sync,
    > = {
        let settings_manager = Arc::clone(&settings_manager);
        let active_session_manager = Arc::clone(&active_session_manager);
        Arc::new(move |messages: Vec<AgentMessage>| -> Vec<Message> {
            let session_manager = active_session_manager
                .lock()
                .expect("active session manager poisoned")
                .clone();
            let (model_tool_output_artifact_dir, session_id, policy, block_images) = {
                let manager = session_manager.lock().expect("session manager poisoned");
                let settings = settings_manager.lock().expect("settings manager poisoned");
                (
                    manager.get_session_artifact_dir(),
                    manager.get_session_id(),
                    resolve_model_tool_output_policy(
                        settings.get_model_tool_output_policy().as_deref(),
                    ),
                    settings.get_block_images(),
                )
            };
            let converted = convert_to_llm(
                &messages,
                &ModelToolOutputPolicyOptions {
                    policy: Some(policy),
                    scope: model_tool_output_artifact_dir.map(|dir| ModelToolOutputScope {
                        session_id,
                        session_artifact_dir: dir,
                    }),
                },
            );
            if !block_images {
                return converted;
            }
            converted.into_iter().map(block_images_in_message).collect()
        })
    };

    let extension_runner_ref = Arc::new(ExtensionRunnerRef::new(None));

    let (steering_mode, follow_up_mode, transport, thinking_budgets) = {
        let settings = settings_manager.lock().expect("settings manager poisoned");
        (
            settings.get_steering_mode(),
            settings.get_follow_up_mode(),
            settings.get_transport(),
            settings.get_thinking_budgets(),
        )
    };

    let stream_fn: StreamFn = {
        let model_registry = Arc::clone(&model_registry);
        let settings_manager = Arc::clone(&settings_manager);
        Arc::new(
            move |model: Model, context: Context, mut options: SimpleStreamOptions| {
                let model_registry = Arc::clone(&model_registry);
                let settings_manager = Arc::clone(&settings_manager);
                Box::pin(async move {
                    let auth = model_registry
                        .lock()
                        .expect("model registry poisoned")
                        .get_api_key_and_headers(&model)
                        .await;
                    if !auth.ok {
                        // `throw new Error(auth.error)`; the stream contract encodes
                        // request failures in the returned stream.
                        let mut message = pi_ai::types::AssistantMessage::default();
                        message.api = model.api.clone();
                        message.provider = model.provider.clone();
                        message.model = model.id.clone();
                        message.stop_reason = pi_ai::types::STOP_REASON_ERROR.to_string();
                        message.error_message = auth.error.clone();
                        let stream = pi_ai::utils::event_stream::AssistantMessageEventStream::new();
                        stream.push(pi_ai::types::AssistantMessageEvent::Start {
                            partial: message.clone(),
                        });
                        stream.end(Some(message));
                        return stream;
                    }
                    if auth.api_key.is_some() {
                        options.stream.api_key = auth.api_key.clone();
                    }
                    if auth.headers.is_some() {
                        options.stream.headers = auth.headers.clone();
                    }
                    if options.stream.timeout_ms.is_none() {
                        let timeout_ms = settings_manager
                            .lock()
                            .expect("settings manager poisoned")
                            .get_provider_retry_settings()
                            .timeout_ms;
                        options.stream.timeout_ms = Some(timeout_ms);
                    }
                    pi_ai::stream::stream_simple(&model, &context, Some(&options))
                })
            },
        )
    };

    let on_payload = {
        let extension_runner_ref = Arc::clone(&extension_runner_ref);
        Arc::new(move |payload: Value| {
            let runner = extension_runner_ref.current();
            let Some(runner) = runner else {
                return Box::pin(async move { payload }) as BoxFuture<Value>;
            };
            if !runner.has_handlers("before_provider_request") {
                return Box::pin(async move { payload }) as BoxFuture<Value>;
            }
            let next = runner.emit_before_provider_request(payload.clone());
            Box::pin(async move { next.await.unwrap_or(payload) }) as BoxFuture<Value>
        }) as Arc<dyn Fn(Value) -> BoxFuture<Value> + Send + Sync>
    };
    let on_response = {
        let extension_runner_ref = Arc::clone(&extension_runner_ref);
        Arc::new(move |response: Value| {
            let runner = extension_runner_ref.current();
            let Some(runner) = runner else {
                return Box::pin(async {}) as BoxFuture<()>;
            };
            if !runner.has_handlers("after_provider_response") {
                return Box::pin(async {}) as BoxFuture<()>;
            }
            let status = response
                .get("status")
                .and_then(Value::as_f64)
                .unwrap_or_default();
            let headers = response.get("headers").cloned().unwrap_or(Value::Null);
            let _ = runner.emit_before_provider_request(serde_json::json!({
                "type": "after_provider_response",
                "status": status,
                "headers": headers,
            }));
            Box::pin(async {}) as BoxFuture<()>
        }) as Arc<dyn Fn(Value) -> BoxFuture<()> + Send + Sync>
    };

    let session_id = session_manager
        .lock()
        .expect("session manager poisoned")
        .get_session_id();

    let transform_context = {
        let extension_runner_ref = Arc::clone(&extension_runner_ref);
        Arc::new(
            move |messages: Vec<AgentMessage>, _signal: Option<tokio_util::sync::CancellationToken>| {
                let runner = extension_runner_ref.current();
                let Some(runner) = runner else {
                    return messages;
                };
                runner.emit_context(
                    messages
                        .iter()
                        .map(|message| serde_json::to_value(message).unwrap_or(Value::Null))
                        .collect(),
                );
                messages
            },
        ) as Arc<
            dyn Fn(Vec<AgentMessage>, Option<tokio_util::sync::CancellationToken>) -> Vec<AgentMessage>
                + Send
                + Sync,
        >
    };

    let agent_factory = agent_factory.ok_or_else(|| {
        "pi-agent-core Agent has not landed; pass an agentFactory to createAgentSession".to_string()
    })?;
    let agent = agent_factory(CreateAgentSessionAgentOptions {
        initial_state: AgentState {
            system_prompt: String::new(),
            model: model.clone().unwrap_or_default(),
            thinking_level: thinking_level.clone(),
            service_tier: service_tier.clone(),
            tools: Some(Vec::new()),
            messages: Vec::new(),
            ..Default::default()
        },
        convert_to_llm: Arc::clone(&convert_to_llm_with_block_images),
        stream_fn,
        on_payload,
        on_response,
        session_id,
        transform_context,
        steering_mode,
        follow_up_mode,
        transport,
        thinking_budgets,
    });

    {
        let mut manager = session_manager.lock().expect("session manager poisoned");
        if has_existing_session {
            let mut state = agent.state();
            state.messages = existing_session.messages.clone();
            agent.set_state(state);
            if !has_thinking_entry {
                let _ = manager.append_thinking_level_change(&thinking_level_name(&thinking_level));
            }
        } else {
            if let Some(model_ref) = &model {
                let _ = manager.append_model_change(&model_ref.provider, &model_ref.id);
            }
            let _ = manager.append_thinking_level_change(&thinking_level_name(&thinking_level));
        }
        if !has_service_tier_entry {
            let _ = manager.append_service_tier_change(&service_tier_preference);
        }
    }

    let session = AgentSession::new(AgentSessionConfig {
        agent: Arc::clone(&agent),
        session_manager: Arc::clone(&session_manager),
        settings_manager: Arc::clone(&settings_manager),
        service_tier_preference: service_tier_preference.clone(),
        cwd: cwd.clone(),
        agent_dir: options.agent_dir.clone(),
        scoped_models: options.scoped_models.clone(),
        resource_loader: Arc::clone(&resource_loader),
        custom_tools: options.custom_tools.clone(),
        model_registry: Arc::clone(&model_registry),
        mcp_manager: Some(mcp_manager),
        initial_active_tool_names: Some(initial_active_tool_names),
        allowed_tool_names,
        include_goals: Some(include_goals),
        include_compact_skill: options.creation.include_compact_skill,
        rlm_heartbeat_controller: options.creation.rlm_heartbeat_controller.clone(),
        agent_message_controller: options.creation.agent_message_controller.clone(),
        agent_observe_controller: options.creation.agent_observe_controller.clone(),
        extension_runner_ref: Some(Arc::clone(&extension_runner_ref)),
        session_start_event: options.session_start_event.clone(),
        rlm_depth: options.creation.rlm_depth,
        rlm_max_depth: options.creation.rlm_max_depth,
        rlm_session_dir: options.creation.rlm_session_dir.clone(),
        rlm_parent_node_id: options.creation.rlm_parent_node_id.clone(),
        rlm_parent_agent: options.creation.rlm_parent_agent.clone(),
        semantic_parent_session_id: options.creation.semantic_parent_session_id.clone(),
        semantic_spawned_by_request_id: options.creation.semantic_spawned_by_request_id.clone(),
        subagent_runtime_host: options.creation.subagent_runtime_host.clone(),
        prewarm_ipython_kernel: options.creation.prewarm_ipython_kernel,
        autonomous: options
            .autonomous
            .clone()
            .or_else(|| options.creation.autonomous.clone()),
        serialized_refine: options.creation.serialized_refine,
        initial_goal: options.creation.initial_goal.clone(),
        ..Default::default()
    })?;

    *active_session_manager
        .lock()
        .expect("active session manager poisoned") = Arc::clone(&session.session_manager);

    let artifact_dir = session_manager
        .lock()
        .expect("session manager poisoned")
        .get_session_artifact_dir();
    let _upload = install_agent_trace_upload(AgentTraceUploadInstallOptions {
        auth_storage: Arc::clone(&auth_storage),
        base_url: None,
        config_path: None,
        fetch_fn: None,
        request_timeout_ms: None,
        semantic_edges_ledger_path: semantic_edge_ledger_path(
            options.creation.rlm_session_dir.as_deref(),
            artifact_dir.as_deref(),
        ),
        agent_traces_enabled: Arc::new(|| true),
        reload_settings: Arc::new(|| Box::pin(async { Ok(()) })),
        get_session_file: {
            let session = Arc::clone(&session);
            Arc::new(move || session.session_file())
        },
    });

    let extensions_result = crate::core::extensions::types::LoadExtensionsResult {
        extensions: Vec::new(),
        errors: Vec::new(),
        runtime: crate::core::extensions::types::ExtensionRuntime::new(Default::default()),
    };
    let _ = resource_loader.get_extensions();

    Ok(CreateAgentSessionResult {
        session,
        extensions_result,
        model_fallback_message: if is_no_models_available_message(model_fallback_message.as_deref())
            && model.is_some()
        {
            None
        } else {
            model_fallback_message
        },
    })
}

/// `convertToLlmWithBlockImages`: replaces image blocks with the disabled-image
/// text and merges adjacent copies, exactly like the TypeScript `map`/`filter`.
fn block_images_in_message(message: Message) -> Message {
    let replace = |content: &mut Vec<pi_ai::types::ImageOrTextContent>| {
        if !content
            .iter()
            .any(|block| matches!(block, pi_ai::types::ImageOrTextContent::Image(_)))
        {
            return;
        }
        let mapped: Vec<pi_ai::types::ImageOrTextContent> = content
            .iter()
            .map(|block| match block {
                pi_ai::types::ImageOrTextContent::Image(_) => {
                    pi_ai::types::ImageOrTextContent::Text(pi_ai::types::TextContent::new(
                        "Image reading is disabled.",
                    ))
                }
                other => other.clone(),
            })
            .collect();
        let mut filtered: Vec<pi_ai::types::ImageOrTextContent> = Vec::new();
        for (_, block) in mapped.iter().enumerate() {
            let is_duplicate_disabled_text = match block {
                pi_ai::types::ImageOrTextContent::Text(text) => {
                    text.text == "Image reading is disabled."
                        && filtered.last().is_some_and(|previous| match previous {
                            pi_ai::types::ImageOrTextContent::Text(previous) => {
                                previous.text == "Image reading is disabled."
                            }
                            pi_ai::types::ImageOrTextContent::Image(_) => false,
                        })
                }
                pi_ai::types::ImageOrTextContent::Image(_) => false,
            };
            if is_duplicate_disabled_text && !filtered.is_empty() {
                continue;
            }
            filtered.push(block.clone());
        }
        *content = filtered;
    };

    match message {
        Message::User(mut user) => {
            if let UserContent::Blocks(blocks) = &mut user.content {
                replace(blocks);
            }
            Message::User(user)
        }
        Message::ToolResult(mut result) => {
            replace(&mut result.content);
            Message::ToolResult(result)
        }
        other => other,
    }
}

/// `clampThinkingLevel(model, thinkingLevel) as ThinkingLevel`.
pub fn parse_thinking_level(value: &str) -> ThinkingLevel {
    match value {
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::Xhigh,
        "max" => ThinkingLevel::Max,
        _ => ThinkingLevel::Off,
    }
}

/// `thinkingLevel` serialized back to its wire name.
pub fn thinking_level_name(level: &ThinkingLevel) -> String {
    level.as_str().to_string()
}

/// The MCP manager used when the caller injects neither an instance nor a
/// factory.
fn default_mcp_manager_seam() -> Arc<dyn McpManager> {
    struct UnwiredMcpManager;
    impl McpManager for UnwiredMcpManager {
        fn refresh(&self) {}
        fn replace_acp_servers(
            &self,
            _servers: &[crate::core::mcp::acp_mcp_types::AcpMcpServerConfig],
            _owner_id: &str,
        ) -> bool {
            false
        }
        fn can_release_acp_servers(&self, _owner_id: &str) -> bool {
            false
        }
        fn get_acp_servers(&self) -> Vec<crate::core::mcp::acp_mcp_types::AcpMcpServerConfig> {
            Vec::new()
        }
    }
    Arc::new(UnwiredMcpManager)
}

/// `join(agentDir, name)`.
fn join_path(base: &str, name: &str) -> String {
    std::path::Path::new(base)
        .join(name)
        .to_string_lossy()
        .to_string()
}

/// `process.cwd()`.
fn current_working_directory() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Keeps the imports that only appear in the TypeScript type surface.
#[allow(dead_code)]
fn type_surface_markers(
    _tool: AgentTool,
    _host: Arc<dyn SubagentRuntimeHost>,
    _controller: Arc<dyn AgentSessionMessageController>,
    _observe: Arc<dyn AgentObserveController>,
    _heartbeat: Arc<dyn AgentRlmHeartbeatController>,
    _goal: InitialGoal,
) {
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{ImageContent, TextContent, ToolResultMessage};

    #[test]
    fn thinking_level_name_round_trips_every_level() {
        for name in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
            assert_eq!(thinking_level_name(&parse_thinking_level(name)), name);
        }
    }

    #[test]
    fn unknown_thinking_levels_fall_back_to_off() {
        assert_eq!(parse_thinking_level("bogus"), ThinkingLevel::Off);
    }

    #[test]
    fn image_blocks_become_disabled_text_and_adjacent_copies_merge() {
        let message = Message::User(pi_ai::types::UserMessage::new(
            UserContent::Blocks(vec![
                pi_ai::types::ImageOrTextContent::Image(ImageContent::new("a", "image/png")),
                pi_ai::types::ImageOrTextContent::Image(ImageContent::new("b", "image/png")),
                pi_ai::types::ImageOrTextContent::Text(TextContent::new("after")),
            ]),
            0,
        ));
        let converted = block_images_in_message(message);
        let Message::User(user) = converted else {
            panic!("expected a user message");
        };
        let UserContent::Blocks(blocks) = user.content else {
            panic!("expected blocks");
        };
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0],
            pi_ai::types::ImageOrTextContent::Text(TextContent::new("Image reading is disabled."))
        );
        assert_eq!(blocks[1], pi_ai::types::ImageOrTextContent::Text(TextContent::new("after")));
    }

    #[test]
    fn tool_results_without_images_are_untouched() {
        let message = Message::ToolResult(ToolResultMessage {
            content: vec![pi_ai::types::ImageOrTextContent::Text(TextContent::new("ok"))],
            ..ToolResultMessage::new("id", "name", Vec::new(), false, 0)
        });
        let converted = block_images_in_message(message.clone());
        assert_eq!(converted, message);
    }
}
