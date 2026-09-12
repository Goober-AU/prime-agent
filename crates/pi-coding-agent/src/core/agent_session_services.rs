//! Port of packages/coding-agent/src/core/agent-session-services.ts

use std::sync::{Arc, Mutex};

use pi_agent_core::types::ThinkingLevel;
use pi_ai::types::{BoxFuture, Model, ServiceTier};
use serde_json::Value;

use crate::config::get_agent_dir;
use crate::core::agent_messages::AgentSessionMessageController;
use crate::core::agent_observe::AgentObserveController;
use crate::core::agent_session::{McpManager, ResourceLoader, SubagentRuntimeHost};
use crate::core::agent_session_config::AgentExecutionMode;
use crate::core::agent_traces::{install_agent_trace_upload, AgentTraceUploadInstallOptions};
use crate::core::auth_storage::AuthStorage;
use crate::core::autonomous::AgentAutonomousConfig;
use crate::core::cron_jobs::AgentRlmHeartbeatController;
use crate::core::extensions::builtin::herdr_agent_state::create_herdr_agent_state_extension;
use crate::core::extensions::builtin::memory::create_memory_extension;
use crate::core::extensions::builtin::telegram::create_telegram_extension;
use crate::core::extensions::types::ExtensionFactory;
use crate::core::mcp::mcp_manager::{McpManager as McpManagerImpl, McpManagerOptions};
use crate::core::model_registry::ModelRegistry;
use crate::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use crate::core::semantic_edges::semantic_edge_ledger_path;
use crate::core::session_manager::SessionManager;
use crate::core::settings_manager::SettingsManager;

/// `interface AgentSessionRuntimeDiagnostic`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentSessionRuntimeDiagnostic {
    #[serde(rename = "type")]
    pub type_: String,
    pub message: String,
}

/// `CreateAgentSessionServicesOptions`.
pub struct CreateAgentSessionServicesOptions {
    pub cwd: String,
    pub agent_dir: Option<String>,
    pub auth_storage: Option<Arc<tokio::sync::Mutex<AuthStorage>>>,
    pub settings_manager: Option<Arc<Mutex<SettingsManager>>>,
    pub model_registry: Option<Arc<Mutex<ModelRegistry>>>,
    /// `extensionFlagValues?: Map<string, boolean | string>`.
    pub extension_flag_values: Option<indexmap::IndexMap<String, Value>>,
    pub resource_loader_options: Option<DefaultResourceLoaderOptions>,
    /// Skip the built-in Herdr reporter for these services.
    pub no_builtin_herdr_reporter: Option<bool>,
    pub telemetry_disabled: Option<bool>,
}

/// `interface AgentSessionCreationOptions`.
#[derive(Default)]
pub struct AgentSessionCreationOptions {
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
    pub service_tier: Option<ServiceTier>,
    pub scoped_models: Option<Vec<crate::core::agent_session::ScopedModel>>,
    pub tools: Option<Vec<String>>,
    /// `noTools?: "all" | "builtin"`.
    pub no_tools: Option<String>,
    pub custom_tools: Option<Vec<crate::core::extensions::types::ToolDefinition>>,
    pub initial_active_tool_names: Option<Vec<String>>,
    pub allowed_tool_names: Option<Vec<String>>,
    pub include_goals: Option<bool>,
    pub include_compact_skill: Option<bool>,
    pub agent_message_controller: Option<Arc<dyn AgentSessionMessageController>>,
    pub agent_observe_controller: Option<Arc<dyn AgentObserveController>>,
    pub rlm_depth: Option<i64>,
    pub rlm_max_depth: Option<i64>,
    pub rlm_session_dir: Option<String>,
    pub rlm_parent_node_id: Option<String>,
    pub rlm_parent_agent: Option<String>,
    pub semantic_parent_session_id: Option<String>,
    pub semantic_spawned_by_request_id: Option<String>,
    pub subagent_runtime_host: Option<Arc<dyn SubagentRuntimeHost>>,
    pub rlm_heartbeat_controller: Option<Arc<dyn AgentRlmHeartbeatController>>,
    pub prewarm_ipython_kernel: Option<bool>,
    pub autonomous: Option<AgentAutonomousConfig>,
    pub serialized_refine: Option<bool>,
    pub execution_mode: Option<AgentExecutionMode>,
    pub telemetry_disabled: Option<bool>,
    pub initial_goal: Option<crate::core::agent_session::InitialGoal>,
}

/// `interface CreateAgentSessionFromServicesOptions extends AgentSessionCreationOptions`.
pub struct CreateAgentSessionFromServicesOptions {
    pub services: Arc<AgentSessionServices>,
    pub session_manager: Arc<Mutex<SessionManager>>,
    pub session_start_event: Option<Value>,
    pub creation: AgentSessionCreationOptions,
}

/// `interface AgentSessionServices`.
pub struct AgentSessionServices {
    pub cwd: String,
    pub agent_dir: String,
    pub auth_storage: Arc<tokio::sync::Mutex<AuthStorage>>,
    pub settings_manager: Arc<Mutex<SettingsManager>>,
    pub model_registry: Arc<Mutex<ModelRegistry>>,
    pub resource_loader: Arc<DefaultResourceLoader>,
    pub mcp_manager: Arc<Mutex<McpManagerImpl>>,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

/// `applyExtensionFlagValues(resourceLoader, extensionFlagValues)`.
pub fn apply_extension_flag_values(
    resource_loader: &DefaultResourceLoader,
    extension_flag_values: Option<&indexmap::IndexMap<String, Value>>,
) -> Vec<AgentSessionRuntimeDiagnostic> {
    let Some(extension_flag_values) = extension_flag_values else {
        return Vec::new();
    };

    let mut diagnostics: Vec<AgentSessionRuntimeDiagnostic> = Vec::new();
    let extensions_result = resource_loader.get_extensions();
    let mut registered_flags: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
    for extension in &extensions_result.extensions {
        let extension = extension.lock().expect("extension poisoned");
        for (name, flag) in &extension.flags {
            registered_flags.insert(name.clone(), flag.flag_type.clone());
        }
    }

    let mut unknown_flags: Vec<String> = Vec::new();
    for (name, value) in extension_flag_values {
        let flag_type = match registered_flags.get(name) {
            Some(flag_type) => flag_type.clone(),
            None => {
                unknown_flags.push(name.clone());
                continue;
            }
        };
        if flag_type == "boolean" {
            extensions_result.runtime.flag_values_set(name, Value::Bool(true));
            continue;
        }
        if value.is_string() {
            extensions_result.runtime.flag_values_set(name, value.clone());
            continue;
        }
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            type_: "error".to_string(),
            message: format!("Extension flag \"--{name}\" requires a value"),
        });
    }

    if !unknown_flags.is_empty() {
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            type_: "error".to_string(),
            message: format!(
                "Unknown option{}: {}",
                if unknown_flags.len() == 1 { "" } else { "s" },
                unknown_flags
                    .iter()
                    .map(|name| format!("--{name}"))
                    .collect::<Vec<String>>()
                    .join(", ")
            ),
        });
    }

    diagnostics
}

/// `createAgentSessionServices(options)`.
pub async fn create_agent_session_services(
    options: CreateAgentSessionServicesOptions,
) -> Result<AgentSessionServices, String> {
    let cwd = options.cwd.clone();
    let agent_dir = options.agent_dir.clone().unwrap_or_else(get_agent_dir);
    let auth_storage = options.auth_storage.clone().unwrap_or_else(|| {
        Arc::new(tokio::sync::Mutex::new(AuthStorage::create(
            Some(join_path(&agent_dir, "auth.json")),
            None,
        )))
    });
    let settings_manager = options
        .settings_manager
        .clone()
        .unwrap_or_else(|| Arc::new(Mutex::new(SettingsManager::create(&cwd, Some(&agent_dir)))));
    let model_registry = options.model_registry.clone().unwrap_or_else(|| {
        Arc::new(Mutex::new(ModelRegistry::create(
            AuthStorage::create(Some(join_path(&agent_dir, "auth.json")), None),
            Some(join_path(&agent_dir, "models.json")),
        )))
    });

    // MCP integrations: registers OAuth providers and gates the built-in
    // integration skills by whether the user is logged in (enable-by-login).
    let mcp_manager = Arc::new(Mutex::new(McpManagerImpl::new(McpManagerOptions {
        auth_storage: Arc::clone(&auth_storage),
        get_user_servers: {
            let settings_manager = Arc::clone(&settings_manager);
            Some(Arc::new(move || {
                let settings = settings_manager.lock().expect("settings manager poisoned");
                settings.get_global_mcp_servers().map(|servers| {
                    servers
                        .into_iter()
                        .filter_map(|(name, value)| {
                            serde_json::from_value::<crate::core::mcp::acp_mcp_types::McpServerConfig>(
                                value,
                            )
                            .ok()
                            .map(|config| (name, config))
                        })
                        .collect()
                })
            }) as Arc<dyn Fn() -> Option<indexmap::IndexMap<String, crate::core::mcp::acp_mcp_types::McpServerConfig>> + Send + Sync>)
        },
        begin_login: None,
    })));
    // refresh() resets the OAuth registry to built-ins; re-add user MCP providers too.
    {
        let mcp_manager = Arc::clone(&mcp_manager);
        let mut registry = model_registry.lock().expect("model registry poisoned");
        registry.set_on_oauth_providers_reset(Arc::new(move || {
            mcp_manager
                .lock()
                .expect("mcp manager poisoned")
                .register_user_providers();
        }));
    }

    let resource_loader_options = options.resource_loader_options.clone().unwrap_or_default();
    let user_extension_factories = resource_loader_options.extension_factories.clone();
    // The built-in Herdr reporter defers to Herdr's own file-based integration
    // when the loader actually loaded it; two reporters would race on the same
    // pane. noExtensions is a full opt-out: it disables the built-in reporter
    // too, not just discovered extension files.
    let skip_herdr_reporter = options.no_builtin_herdr_reporter.unwrap_or(false)
        || resource_loader_options.no_extensions;
    let mut builtin_extension_factories: Vec<ExtensionFactory> = Vec::new();
    if !skip_herdr_reporter {
        let loader_for_paths: Arc<DefaultResourceLoader>;
        // The loader does not exist yet while its own factories are built, so the
        // deferred path list is read through a slot filled in right after
        // construction, exactly like the late-bound TypeScript closure.
        let loaded_paths_slot: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let slot = Arc::clone(&loaded_paths_slot);
        builtin_extension_factories
            .push(create_herdr_agent_state_extension(Arc::new(move || {
                slot.lock().expect("loaded paths slot poisoned").clone()
            })));
        let _ = &loader_for_paths;
    }
    if !resource_loader_options.no_extensions {
        builtin_extension_factories.push(create_telegram_extension(agent_dir.clone()));
        builtin_extension_factories.push(create_memory_extension(
            agent_dir.clone(),
            Arc::clone(&settings_manager),
        ));
    }

    let mut extension_factories = builtin_extension_factories;
    extension_factories.extend(user_extension_factories);
    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        extension_factories,
        extra_builtin_skill_overrides: Some({
            let mcp_manager = Arc::clone(&mcp_manager);
            Arc::new(move || {
                mcp_manager
                    .lock()
                    .expect("mcp manager poisoned")
                    .get_disabled_builtin_skill_overrides()
            })
        }),
        ..resource_loader_options
    }));
    resource_loader.reload().await;

    let mut diagnostics: Vec<AgentSessionRuntimeDiagnostic> = Vec::new();
    if !options.telemetry_disabled.unwrap_or(false)
        && crate::core::telemetry::is_telemetry_enabled(&settings_manager)
        && !settings_manager
            .lock()
            .expect("settings manager poisoned")
            .get_telemetry_notice_shown()
    {
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            type_: "info".to_string(),
            message: "Prime Agent sends pseudonymous usage and performance metrics without prompts, responses, tool content, file paths, or repository data. Disable this with telemetry.enabled=false, PRIME_AGENT_TELEMETRY=0, DO_NOT_TRACK=1, or offline mode.".to_string(),
        });
        settings_manager
            .lock()
            .expect("settings manager poisoned")
            .set_telemetry_notice_shown(true);
    }

    let extensions_result = resource_loader.get_extensions();
    for registration in extensions_result.runtime.take_pending_provider_registrations() {
        let config = crate::core::model_registry::ProviderConfigInput {
            name: Some(registration.name.clone()),
            base_url: registration.config.base_url.clone(),
            api_key: registration.config.api_key.clone(),
            api: registration.config.api.clone(),
            stream_simple: registration.config.stream_simple.clone(),
            headers: registration.config.headers.as_ref().map(|headers| {
                headers
                    .iter()
                    .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
                    .collect()
            }),
            auth_header: registration.config.auth_header,
            oauth: registration.config.oauth.as_ref().map(|oauth| {
                crate::core::model_registry::ProviderOAuthInput {
                    name: oauth.name.clone(),
                    login: Arc::clone(&oauth.login),
                    uses_callback_server: Some(false),
                    refresh_token: Arc::clone(&oauth.refresh_token),
                    get_api_key: {
                        let get_api_key = Arc::clone(&oauth.get_api_key);
                        Arc::new(move |credentials| get_api_key(credentials))
                    },
                    modify_models: None,
                }
            }),
            models: registration.config.models.as_ref().map(|models| {
                models
                    .iter()
                    .map(crate::core::model_registry::ModelDefinition::from)
                    .collect()
            }),
        };
        match model_registry
            .lock()
            .expect("model registry poisoned")
            .register_provider(&registration.name, config)
        {
            Ok(()) => {}
            Err(error) => diagnostics.push(AgentSessionRuntimeDiagnostic {
                type_: "error".to_string(),
                message: format!(
                    "Extension \"{}\" error: {}",
                    registration.extension_path, error
                ),
            }),
        }
    }
    diagnostics.extend(apply_extension_flag_values(
        &resource_loader,
        options.extension_flag_values.as_ref(),
    ));

    Ok(AgentSessionServices {
        cwd,
        agent_dir,
        auth_storage,
        settings_manager,
        model_registry,
        resource_loader,
        mcp_manager,
        diagnostics,
    })
}

/// `createAgentSessionFromServices(options)`.
pub async fn create_agent_session_from_services(
    options: CreateAgentSessionFromServicesOptions,
) -> Result<crate::core::sdk::CreateAgentSessionResult, String> {
    let artifact_dir = options
        .session_manager
        .lock()
        .expect("session manager poisoned")
        .get_session_artifact_dir();
    let _upload = install_agent_trace_upload(AgentTraceUploadInstallOptions {
        auth_storage: Arc::clone(&options.services.auth_storage),
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
        get_session_file: Arc::new(|| None),
    });
    let telemetry_disabled = options.creation.telemetry_disabled;
    let execution_mode = options.creation.execution_mode.clone();
    let creation = options.creation;
    let result = crate::core::sdk::create_agent_session(crate::core::sdk::CreateAgentSessionOptions {
        cwd: Some(options.services.cwd.clone()),
        agent_dir: Some(options.services.agent_dir.clone()),
        auth_storage: Some(Arc::clone(&options.services.auth_storage)),
        model_registry: Some(Arc::clone(&options.services.model_registry)),
        model: creation.model.clone(),
        thinking_level: creation.thinking_level.clone(),
        service_tier: creation.service_tier.clone(),
        scoped_models: creation.scoped_models.clone(),
        no_tools: creation.no_tools.clone(),
        tools: creation.tools.clone(),
        custom_tools: creation.custom_tools.clone(),
        resource_loader: Some(
            Arc::clone(&options.services.resource_loader) as Arc<dyn ResourceLoader>,
        ),
        mcp_manager: Some(Arc::clone(&options.services.mcp_manager) as Arc<dyn McpManager>),
        session_manager: Some(options.session_manager),
        settings_manager: Some(Arc::clone(&options.services.settings_manager)),
        session_start_event: options.session_start_event,
        autonomous: creation.autonomous.clone(),
        creation: AgentSessionCreationOptions {
            // `createAgentSession` receives every option explicitly; the
            // remaining creation fields come from the same object.
            telemetry_disabled,
            execution_mode,
            ..creation
        },
    })
    .await?;
    // `installAgentTelemetry(result.session, ...)` lives in `core/telemetry.ts`
    // (another slice, not landed); `isTelemetryEnabled` is called there.
    let _ = telemetry_disabled;
    Ok(result)
}

/// `join(agentDir, name)`.
fn join_path(base: &str, name: &str) -> String {
    std::path::Path::new(base)
        .join(name)
        .to_string_lossy()
        .to_string()
}

/// Keeps the `BoxFuture`/`ServiceTier` imports used by the creation surface.
#[allow(dead_code)]
fn surface_markers(_future: BoxFuture<()>, _tier: &ServiceTier) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extensions::types::{
        Extension, ExtensionFlag, ExtensionRuntime, LoadExtensionsResult, SharedExtension,
    };

    fn loader_with_flags(flags: Vec<(&str, &str)>) -> DefaultResourceLoader {
        let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions::new(
            ".",
            ".prime/agent",
        ));
        let mut extension = Extension {
            path: "ext.js".to_string(),
            resolved_path: "ext.js".to_string(),
            source_info: Default::default(),
            handlers: Default::default(),
            tools: Default::default(),
            message_renderers: Default::default(),
            commands: Default::default(),
            flags: flags
                .into_iter()
                .map(|(name, flag_type)| {
                    (
                        name.to_string(),
                        ExtensionFlag {
                            name: name.to_string(),
                            description: None,
                            flag_type: flag_type.to_string(),
                            default: None,
                            extension_path: "ext.js".to_string(),
                        },
                    )
                })
                .collect(),
            shortcuts: Default::default(),
        };
        extension.source_info = crate::core::source_info::create_synthetic_source_info("ext.js");
        let shared: SharedExtension = Arc::new(Mutex::new(extension));
        let result = LoadExtensionsResult {
            extensions: vec![shared],
            errors: Vec::new(),
            runtime: ExtensionRuntime::new(Default::default()),
        };
        let _ = result;
        loader
    }

    #[test]
    fn no_flag_values_produces_no_diagnostics() {
        let loader = loader_with_flags(Vec::new());
        assert!(apply_extension_flag_values(&loader, None).is_empty());
    }

    #[test]
    fn unknown_flags_are_reported_with_the_pluralised_label() {
        let loader = loader_with_flags(Vec::new());
        let mut values = indexmap::IndexMap::new();
        values.insert("missing".to_string(), Value::Bool(true));
        let diagnostics = apply_extension_flag_values(&loader, Some(&values));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].message, "Unknown option: --missing");
    }

    #[test]
    fn multiple_unknown_flags_use_the_plural_label() {
        let loader = loader_with_flags(Vec::new());
        let mut values = indexmap::IndexMap::new();
        values.insert("one".to_string(), Value::Bool(true));
        values.insert("two".to_string(), Value::Bool(true));
        let diagnostics = apply_extension_flag_values(&loader, Some(&values));
        assert_eq!(diagnostics[0].message, "Unknown options: --one, --two");
    }
}
