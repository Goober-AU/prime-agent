//! Port of packages/coding-agent/src/core/mcp/mcp-manager.ts
//!
//! Host side of MCP integrations. The protocol itself runs Python-side in the
//! kernel; the host only registers OAuth providers, gates integration skills by
//! auth, and serves `mcp.*` host-requests.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::mcp::catalog::{builtin_mcp_catalog, get_catalog_entry, register_builtin_mcp_oauth_providers};
use pi_ai::mcp::oauth::{create_mcp_oauth_provider, McpOAuthConfig};
use pi_ai::utils::oauth::{get_oauth_provider, register_oauth_provider, unregister_oauth_provider};
use serde_json::{json, Map, Value};

use crate::core::auth_storage::{AuthCredential, AuthStorage};
use crate::core::kernel::shared::{HostRequestHandler, HostRequestHandlers, KernelError};
use crate::core::mcp::acp_mcp_types::AcpMcpServerConfig;
use crate::core::settings_manager::{HttpMcpServerConfig, McpServerConfig, StdioMcpServerConfig};

/// `interface McpManagerOptions`.
pub struct McpManagerOptions {
    pub auth_storage: Arc<tokio::sync::Mutex<AuthStorage>>,
    /// Reads the current `Settings.mcpServers` (name -> config). Re-read on `refresh()`.
    pub get_user_servers: Option<Arc<dyn Fn() -> Option<IndexMap<String, McpServerConfig>> + Send + Sync>>,
    /// Start an interactive host-side login for a server. Provided by the UI mode.
    pub begin_login: Option<Arc<dyn Fn(String) -> pi_ai::types::BoxFuture<Result<(), String>> + Send + Sync>>,
}

impl Default for McpManagerOptions {
    fn default() -> Self {
        Self {
            auth_storage: Arc::new(tokio::sync::Mutex::new(AuthStorage::in_memory(
                IndexMap::new(),
                None,
            ))),
            get_user_servers: None,
            begin_login: None,
        }
    }
}

/// A resolved integration: a catalog/user entry plus its provider id.
const GENERIC_SERVER_NAME_PATTERN: &str = r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$";

/// `interface ResolvedIntegration`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedIntegration {
    pub server: String,
    pub label: String,
    pub config: McpServerConfig,
    pub uses_oauth: bool,
    /// True when this came from `Settings.mcpServers` (may override a catalog name).
    pub user_declared: bool,
}

impl ResolvedIntegration {
    /// `{ type: "http", url, oauth: true }` for a catalog entry.
    fn from_catalog(server: &str, label: &str, url: &str, oauth: bool) -> Self {
        Self {
            server: server.to_string(),
            label: label.to_string(),
            config: McpServerConfig::Http(HttpMcpServerConfig {
                url: url.to_string(),
                headers: None,
                bearer_token_env_var: None,
                oauth: Some(true),
                enabled: None,
                enabled_tools: None,
                disabled_tools: None,
                startup_timeout_ms: None,
                call_timeout_ms: None,
            }),
            uses_oauth: oauth,
            user_declared: false,
        }
    }

    fn uses_oauth(&self) -> bool {
        match &self.config {
            McpServerConfig::Http(http) => http.oauth == Some(true),
            McpServerConfig::Stdio(_) => false,
        }
    }

    fn enabled(&self) -> Option<bool> {
        match &self.config {
            McpServerConfig::Http(http) => http.enabled,
            McpServerConfig::Stdio(stdio) => stdio.enabled,
        }
    }

    fn url(&self) -> Option<&str> {
        match &self.config {
            McpServerConfig::Http(http) => Some(&http.url),
            McpServerConfig::Stdio(_) => None,
        }
    }

    fn bearer_token_env_var(&self) -> Option<&str> {
        match &self.config {
            McpServerConfig::Http(http) => http.bearer_token_env_var.as_deref(),
            McpServerConfig::Stdio(_) => None,
        }
    }

    fn transport(&self) -> &'static str {
        match &self.config {
            McpServerConfig::Http(_) => "http",
            McpServerConfig::Stdio(_) => "stdio",
        }
    }
}

/// `class McpManager`.
pub struct McpManager {
    auth_storage: Arc<tokio::sync::Mutex<AuthStorage>>,
    get_user_servers: Arc<dyn Fn() -> Option<IndexMap<String, McpServerConfig>> + Send + Sync>,
    begin_login: Option<Arc<dyn Fn(String) -> pi_ai::types::BoxFuture<Result<(), String>> + Send + Sync>>,
    integrations: IndexMap<String, ResolvedIntegration>,
    acp_servers: IndexMap<String, AcpMcpServerConfig>,
    acp_owner_id: Option<String>,
    /// Provider ids we registered for user servers, so refresh can drop removed ones.
    registered_user_provider_ids: Vec<String>,
}

impl McpManager {
    pub fn new(options: McpManagerOptions) -> Self {
        let mut manager = Self {
            auth_storage: options.auth_storage,
            get_user_servers: options
                .get_user_servers
                .unwrap_or_else(|| Arc::new(|| None)),
            begin_login: options.begin_login,
            integrations: IndexMap::new(),
            acp_servers: IndexMap::new(),
            acp_owner_id: None,
            registered_user_provider_ids: Vec::new(),
        };
        manager.resolve_integrations();
        manager.register_providers();
        manager
    }

    /// Re-read settings and re-register providers; call after a session reload.
    pub fn refresh(&mut self) {
        self.resolve_integrations();
        self.register_providers();
    }

    pub fn can_release_acp_servers(&self, owner_id: &str) -> bool {
        self.acp_owner_id.is_none() || self.acp_owner_id.as_deref() == Some(owner_id)
    }

    pub fn replace_acp_servers(&mut self, servers: &[AcpMcpServerConfig], owner_id: &str) -> Result<bool, String> {
        if owner_id.is_empty() {
            return Err("ACP MCP owner id is required".to_string());
        }
        if servers.is_empty() && self.acp_owner_id.as_deref() != Some(owner_id) {
            return Ok(false);
        }
        if !servers.is_empty()
            && self
                .acp_owner_id
                .as_deref()
                .map(|owner| owner != owner_id)
                .unwrap_or(false)
        {
            return Err("ACP MCP configuration is owned by another client".to_string());
        }

        let mut next: IndexMap<String, AcpMcpServerConfig> = IndexMap::new();
        for server in servers {
            if next.contains_key(server.name()) {
                return Err(format!("Duplicate ACP MCP server: {}", server.name()));
            }
            next.insert(server.name().to_string(), server.clone());
        }
        let unchanged = next.len() == self.acp_servers.len()
            && next.iter().all(|(name, config)| {
                self.acp_servers
                    .get(name)
                    .map(|existing| {
                        serde_json::to_string(existing).unwrap_or_default()
                            == serde_json::to_string(config).unwrap_or_default()
                    })
                    .unwrap_or(false)
            });
        if unchanged {
            return Ok(false);
        }
        self.acp_owner_id = if next.is_empty() { None } else { Some(owner_id.to_string()) };
        self.acp_servers = next;
        Ok(true)
    }

    fn provider_id(server: &str) -> String {
        format!("mcp:{server}")
    }

    fn resolve_integrations(&mut self) {
        let mut integrations: IndexMap<String, ResolvedIntegration> = IndexMap::new();
        for entry in builtin_mcp_catalog() {
            let uses_oauth = entry
                .oauth
                .as_ref()
                .map(|oauth| oauth.kind == "oauth")
                .unwrap_or(false);
            integrations.insert(
                entry.server.clone(),
                ResolvedIntegration::from_catalog(&entry.server, &entry.label, &entry.url, uses_oauth),
            );
        }
        for (server, config) in (self.get_user_servers)().unwrap_or_default() {
            let uses_oauth = matches!(&config, McpServerConfig::Http(http) if http.oauth == Some(true));
            integrations.insert(
                server.clone(),
                ResolvedIntegration {
                    server: server.clone(),
                    label: server.clone(),
                    config,
                    uses_oauth,
                    user_declared: true,
                },
            );
        }
        self.integrations = integrations;
    }

    fn register_providers(&mut self) {
        register_builtin_mcp_oauth_providers();
        self.register_user_providers();
    }

    /// Register OAuth providers for user-declared (non-catalog) servers. Public so it
    /// can run after `ModelRegistry.refresh()` resets the registry - otherwise custom
    /// `mcp:<server>` providers vanish on every refresh (e.g. post-login).
    pub fn register_user_providers(&mut self) {
        let mut current: Vec<String> = Vec::new();
        let integrations: Vec<ResolvedIntegration> = self.integrations.values().cloned().collect();
        for integration in integrations {
            if !integration.user_declared
                || integration.transport() != "http"
                || get_catalog_entry(&integration.server).is_some()
            {
                continue;
            }
            let id = Self::provider_id(&integration.server);
            if integration.uses_oauth() {
                current.push(id.clone());
                let url = integration.url().unwrap_or_default().to_string();
                register_oauth_provider(create_mcp_oauth_provider(McpOAuthConfig {
                    server: integration.server.clone(),
                    label: Some(integration.label.clone()),
                    url,
                    client_id: None,
                    scopes: None,
                }));
            }
        }
        // Drop providers for user servers removed since the last registration.
        for id in &self.registered_user_provider_ids {
            if !current.contains(id) {
                unregister_oauth_provider(id);
            }
        }
        self.registered_user_provider_ids = current;
    }

    /// True when valid credentials exist for the integration (drives enablement).
    fn is_authed(&self, integration: &ResolvedIntegration) -> bool {
        if integration.enabled() == Some(false) {
            return false;
        }
        if integration.user_declared && get_catalog_entry(&integration.server).is_some() {
            return false;
        }
        if integration.transport() == "stdio" {
            return true;
        }
        let bearer_token_env_var = integration.bearer_token_env_var();
        if !integration.uses_oauth() && bearer_token_env_var.is_none() {
            return true;
        }
        if let Some(name) = bearer_token_env_var {
            if std::env::var(name)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                return true;
            }
        }
        let cred = {
            // `authStorage.get(...)` is synchronous in the port.
            let storage = self.auth_storage.try_lock();
            match storage {
                Ok(storage) => storage.get(&Self::provider_id(&integration.server)),
                Err(_) => None,
            }
        };
        let Some(cred) = cred else {
            return false;
        };
        // Builtin URLs are code-constant; only user-declared endpoints can be retargeted, so only their
        // tokens must prove where they belong. Mismatched or unbound tokens require re-login.
        if !integration.user_declared {
            return true;
        }
        let endpoint = credential_endpoint(&cred);
        match (endpoint, integration.url()) {
            (Some(endpoint), Some(url)) => endpoint == url,
            _ => false,
        }
    }

    /// `-<server>/SKILL.md` overrides for every built-in integration the user isn't logged into.
    pub fn get_disabled_builtin_skill_overrides(&self) -> Vec<String> {
        let mut overrides: Vec<String> = Vec::new();
        for entry in builtin_mcp_catalog() {
            let Some(integration) = self.integrations.get(&entry.server) else {
                continue;
            };
            if !self.is_authed(integration) {
                overrides.push(format!("-{}/SKILL.md", entry.server));
            }
        }
        overrides
    }

    /// Host-request handlers exposed to the kernel.
    pub fn host_handlers(&self) -> HostRequestHandlers {
        let mut handlers: HostRequestHandlers = HashMap::new();

        let refresh_manager = self.shared_view();
        let refresh: HostRequestHandler = Arc::new(move |payload: Value| {
            let manager = refresh_manager.clone();
            Box::pin(async move {
                let server = payload.get("server").and_then(Value::as_str).unwrap_or("").to_string();
                if server.is_empty() {
                    return Err(KernelError::new("mcp.refresh requires a server"));
                }
                if manager.acp_server_names().contains(&server) {
                    return Err(KernelError::new(format!(
                        "ACP MCP server {server} does not use host OAuth"
                    )));
                }
                // getApiKey refreshes + rewrites auth.json under lock; Python re-reads.
                // Surface failure (throw) instead of a false success so the kernel can
                // report a refresh error rather than a misleading "not enabled".
                let key = {
                    let mut storage = manager.auth_storage.lock().await;
                    storage
                        .get_api_key(&McpManager::provider_id(&server), true)
                        .await
                        .map_err(KernelError::new)?
                };
                if key.map(|value| value.is_empty()).unwrap_or(true) {
                    return Err(KernelError::new(format!(
                        "Could not refresh credentials for {server}"
                    )));
                }
                Ok(json!({}))
            })
        });
        handlers.insert("mcp.refresh".to_string(), refresh);

        // Resolved config so the kernel skill connects to the same URL the host
        // registered/authenticated (honors a user's mcpServers `url` override).
        let config_manager = self.shared_view();
        let config: HostRequestHandler = Arc::new(move |payload: Value| {
            let manager = config_manager.clone();
            Box::pin(async move {
                let server = payload.get("server").and_then(Value::as_str).unwrap_or("").to_string();
                if server.is_empty() {
                    return Err(KernelError::new("mcp.config requires a server"));
                }
                if let Some(acp_server) = manager.acp_servers.get(&server) {
                    // `const { name: _name, ...config } = acpServer;`
                    let mut value = serde_json::to_value(acp_server).map_err(|error| KernelError::new(error.to_string()))?;
                    if let Value::Object(map) = &mut value {
                        map.shift_remove("name");
                        map.insert("credentialSource".to_string(), Value::String("acp".to_string()));
                        return Ok(Value::Object(map.clone()));
                    }
                    return Ok(value);
                }
                let Some(integration) = manager.integrations.get(&server) else {
                    return Ok(json!({}));
                };
                if !integration.user_declared || get_catalog_entry(&server).is_some() {
                    return Ok(json!({}));
                }
                serde_json::to_value(&integration.config).map_err(|error| KernelError::new(error.to_string()))
            })
        });
        handlers.insert("mcp.config".to_string(), config);

        // Only expose begin_login when an interactive login is actually wired, so the
        // kernel doesn't get a handler whose only behavior is to throw.
        if let Some(begin_login) = self.begin_login.clone() {
            let begin_login: HostRequestHandler = Arc::new(move |payload: Value| {
                let begin_login = begin_login.clone();
                Box::pin(async move {
                    let server = payload.get("server").and_then(Value::as_str).unwrap_or("").to_string();
                    if server.is_empty() {
                        return Err(KernelError::new("mcp.begin_login requires a server"));
                    }
                    begin_login(server).await.map_err(KernelError::new)?;
                    Ok(json!({}))
                })
            });
            handlers.insert("mcp.begin_login".to_string(), begin_login);
        }

        handlers
    }

    /// Session-scoped servers supplied by the active ACP client.
    pub fn get_acp_servers(&self) -> Vec<AcpMcpServerConfig> {
        self.acp_servers.values().cloned().collect()
    }

    /// Enabled user-declared servers available through the generic kernel API.
    pub fn get_enabled_persistent_generic_servers(&self) -> Vec<String> {
        let pattern = regex::Regex::new(GENERIC_SERVER_NAME_PATTERN).expect("static regex");
        let mut servers: Vec<String> = self
            .integrations
            .values()
            .filter(|integration| {
                integration.user_declared
                    && pattern.is_match(&integration.server)
                    && get_catalog_entry(&integration.server).is_none()
                    && self.is_authed(integration)
            })
            .map(|integration| integration.server.clone())
            .collect();
        servers.sort();
        servers
    }

    /// Status for the `/mcp list` command.
    pub fn list_status(&self) -> Vec<McpStatusEntry> {
        self.integrations
            .values()
            .map(|integration| McpStatusEntry {
                server: integration.server.clone(),
                label: integration.label.clone(),
                enabled: self.is_authed(integration),
                uses_oauth: integration.uses_oauth,
            })
            .collect()
    }

    /// The subset of manager state the kernel handlers read after construction.
    fn shared_view(&self) -> Arc<McpManagerView> {
        Arc::new(McpManagerView {
            auth_storage: self.auth_storage.clone(),
            integrations: self.integrations.clone(),
            acp_servers: self.acp_servers.clone(),
        })
    }
}

/// `Array<{ server; label; enabled; usesOAuth }>` returned by `listStatus`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatusEntry {
    pub server: String,
    pub label: String,
    pub enabled: bool,
    pub uses_oauth: bool,
}

/// Read-only manager state captured by the host-request closures.
struct McpManagerView {
    auth_storage: Arc<tokio::sync::Mutex<AuthStorage>>,
    integrations: IndexMap<String, ResolvedIntegration>,
    acp_servers: IndexMap<String, AcpMcpServerConfig>,
}

impl McpManagerView {
    fn acp_server_names(&self) -> Vec<String> {
        self.acp_servers.keys().cloned().collect()
    }
}

/// `(cred as { endpoint?: string }).endpoint`.
fn credential_endpoint(credential: &AuthCredential) -> Option<String> {
    match credential {
        AuthCredential::ApiKey { .. } => None,
        AuthCredential::OAuth { credentials } => credentials
            .extra
            .get("endpoint")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// `getCatalogEntry(server)` is used by `registerUserProviders`; expose the
/// stdio/http config helper so callers can build their own servers.
pub fn http_mcp_server_config(url: &str) -> McpServerConfig {
    McpServerConfig::Http(HttpMcpServerConfig {
        url: url.to_string(),
        headers: None,
        bearer_token_env_var: None,
        oauth: None,
        enabled: None,
        enabled_tools: None,
        disabled_tools: None,
        startup_timeout_ms: None,
        call_timeout_ms: None,
    })
}

/// `getOAuthProvider(id)` lookup used by tests and embedders.
pub fn has_registered_oauth_provider(id: &str) -> bool {
    get_oauth_provider(id).is_some()
}

/// `json!({})` helper kept for readability of the handler bodies.
#[allow(dead_code)]
fn empty_record() -> Value {
    Value::Object(Map::new())
}

/// `StdioMcpServerConfig` is part of the settings surface this module reads.
#[allow(dead_code)]
fn stdio_config(command: &str) -> McpServerConfig {
    McpServerConfig::Stdio(StdioMcpServerConfig {
        command: command.to_string(),
        args: None,
        cwd: None,
        env: None,
        enabled: None,
        enabled_tools: None,
        disabled_tools: None,
        startup_timeout_ms: None,
        call_timeout_ms: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::utils::oauth::reset_oauth_providers;

    fn manager(options: McpManagerOptions) -> McpManager {
        McpManager::new(options)
    }

    fn auth_storage() -> Arc<tokio::sync::Mutex<AuthStorage>> {
        Arc::new(tokio::sync::Mutex::new(AuthStorage::in_memory(
            IndexMap::new(),
            None,
        )))
    }

    #[test]
    fn disables_every_builtin_integration_when_no_credentials_exist() {
        reset_oauth_providers();
        let manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            ..Default::default()
        });
        let overrides = manager.get_disabled_builtin_skill_overrides();
        assert!(overrides.contains(&"-linear/SKILL.md".to_string()));
        assert!(overrides.contains(&"-notion/SKILL.md".to_string()));
    }

    #[test]
    fn enables_an_integration_once_credentials_are_stored() {
        reset_oauth_providers();
        let storage = auth_storage();
        storage.try_lock().unwrap().set(
            "mcp:linear",
            AuthCredential::OAuth {
                credentials: pi_ai::utils::oauth::OAuthCredentials {
                    refresh: "r".to_string(),
                    access: "tok".to_string(),
                    expires: 1_800_000_000_000.0,
                    extra: Map::new(),
                },
            },
        );
        let manager = manager(McpManagerOptions {
            auth_storage: storage,
            ..Default::default()
        });
        let overrides = manager.get_disabled_builtin_skill_overrides();
        assert!(!overrides.contains(&"-linear/SKILL.md".to_string()));
        assert!(overrides.contains(&"-notion/SKILL.md".to_string()));
        let status = manager
            .list_status()
            .into_iter()
            .find(|status| status.server == "linear");
        assert_eq!(status.map(|status| status.enabled), Some(true));
    }

    #[test]
    fn registers_an_oauth_provider_per_builtin_integration() {
        reset_oauth_providers();
        let _ = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            ..Default::default()
        });
        assert!(get_oauth_provider("mcp:linear").is_some());
        assert!(get_oauth_provider("mcp:notion").is_some());
    }

    #[test]
    fn refresh_re_registers_builtin_providers_after_a_registry_reset() {
        reset_oauth_providers();
        let mut manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            ..Default::default()
        });
        // `ModelRegistry.refresh()` calls `resetOAuthProviders()`.
        reset_oauth_providers();
        manager.refresh();
        assert!(get_oauth_provider("mcp:linear").is_some());
        assert!(get_oauth_provider("mcp:notion").is_some());
    }

    #[test]
    fn re_registers_user_declared_oauth_servers() {
        reset_oauth_providers();
        let mut manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: Some(Arc::new(|| {
                let mut servers = IndexMap::new();
                servers.insert(
                    "acme".to_string(),
                    http_mcp_server_config("https://mcp.acme.test/mcp"),
                );
                Some(servers)
            })),
            ..Default::default()
        });
        manager.register_user_providers();
        assert!(get_oauth_provider("mcp:acme").is_none());
        // `oauth: true` is what registers the provider.
        let mut manager = manager_with_acme_oauth();
        manager.register_user_providers();
        assert!(get_oauth_provider("mcp:acme").is_some());
        reset_oauth_providers();
        manager.refresh();
        assert!(get_oauth_provider("mcp:acme").is_some());
    }

    fn manager_with_acme_oauth() -> McpManager {
        manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: Some(Arc::new(|| {
                let mut servers = IndexMap::new();
                servers.insert(
                    "acme".to_string(),
                    oauth_http_server("https://mcp.acme.test/mcp"),
                );
                Some(servers)
            })),
            ..Default::default()
        })
    }

    fn oauth_http_server(url: &str) -> McpServerConfig {
        McpServerConfig::Http(HttpMcpServerConfig {
            url: url.to_string(),
            headers: None,
            bearer_token_env_var: None,
            oauth: Some(true),
            enabled: None,
            enabled_tools: None,
            disabled_tools: None,
            startup_timeout_ms: None,
            call_timeout_ms: None,
        })
    }

    #[tokio::test]
    async fn exposes_only_refresh_and_config_when_no_interactive_login_is_wired() {
        reset_oauth_providers();
        let manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            ..Default::default()
        });
        let handlers = manager.host_handlers();
        let mut keys: Vec<String> = handlers.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, vec!["mcp.config".to_string(), "mcp.refresh".to_string()]);

        let refresh = handlers.get("mcp.refresh").unwrap().clone();
        let error = refresh(json!({ "server": "linear" })).await.unwrap_err();
        assert_eq!(error.to_string(), "Could not refresh credentials for linear");
        let error = refresh(json!({})).await.unwrap_err();
        assert_eq!(error.to_string(), "mcp.refresh requires a server");
    }

    #[tokio::test]
    async fn exposes_begin_login_only_when_it_is_provided() {
        reset_oauth_providers();
        let called = Arc::new(std::sync::Mutex::new(String::new()));
        let called_for_login = called.clone();
        let manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            begin_login: Some(Arc::new(move |server: String| {
                let called = called_for_login.clone();
                Box::pin(async move {
                    *called.lock().unwrap() = server;
                    Ok(())
                })
            })),
            ..Default::default()
        });
        let handlers = manager.host_handlers();
        let mut keys: Vec<String> = handlers.keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "mcp.begin_login".to_string(),
                "mcp.config".to_string(),
                "mcp.refresh".to_string()
            ]
        );
        handlers
            .get("mcp.begin_login")
            .unwrap()
            .clone()(json!({ "server": "linear" }))
            .await
            .unwrap();
        assert_eq!(*called.lock().unwrap(), "linear".to_string());
    }

    #[tokio::test]
    async fn config_keeps_catalog_names_reserved_from_generic_overrides() {
        reset_oauth_providers();
        let manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: Some(Arc::new(|| {
                let mut servers = IndexMap::new();
                servers.insert("linear".to_string(), oauth_http_server("https://proxy.test/mcp"));
                Some(servers)
            })),
            ..Default::default()
        });
        let handlers = manager.host_handlers();
        let config = handlers.get("mcp.config").unwrap().clone();
        assert_eq!(config(json!({ "server": "linear" })).await.unwrap(), json!({}));
        assert_eq!(config(json!({ "server": "notion" })).await.unwrap(), json!({}));
    }

    #[test]
    fn an_oauth_override_of_a_catalog_name_is_not_authed_by_the_official_credential() {
        reset_oauth_providers();
        let storage = auth_storage();
        storage.try_lock().unwrap().set(
            "mcp:linear",
            AuthCredential::OAuth {
                credentials: pi_ai::utils::oauth::OAuthCredentials {
                    refresh: "r".to_string(),
                    access: "official".to_string(),
                    expires: 1_800_000_000_000.0,
                    extra: Map::new(),
                },
            },
        );
        let manager = manager(McpManagerOptions {
            auth_storage: storage,
            get_user_servers: Some(Arc::new(|| {
                let mut servers = IndexMap::new();
                servers.insert("linear".to_string(), oauth_http_server("https://proxy.test/mcp"));
                Some(servers)
            })),
            ..Default::default()
        });
        let status = manager
            .list_status()
            .into_iter()
            .find(|status| status.server == "linear");
        assert_eq!(status.map(|status| status.enabled), Some(false));
    }

    #[test]
    fn a_credential_bound_to_another_endpoint_does_not_enable_a_server() {
        reset_oauth_providers();
        let storage = auth_storage();
        {
            let mut guard = storage.try_lock().unwrap();
            let mut extra = Map::new();
            extra.insert(
                "endpoint".to_string(),
                Value::String("https://old.test/mcp".to_string()),
            );
            guard.set(
                "mcp:remote",
                AuthCredential::OAuth {
                    credentials: pi_ai::utils::oauth::OAuthCredentials {
                        refresh: "r".to_string(),
                        access: "old-token".to_string(),
                        expires: 1_800_000_000_000.0,
                        extra,
                    },
                },
            );
            guard.set(
                "mcp:unbound",
                AuthCredential::OAuth {
                    credentials: pi_ai::utils::oauth::OAuthCredentials {
                        refresh: "r".to_string(),
                        access: "unbound-token".to_string(),
                        expires: 1_800_000_000_000.0,
                        extra: Map::new(),
                    },
                },
            );
        }
        let manager = manager(McpManagerOptions {
            auth_storage: storage,
            get_user_servers: Some(Arc::new(|| {
                let mut servers = IndexMap::new();
                servers.insert("remote".to_string(), oauth_http_server("https://new.test/mcp"));
                servers.insert("unbound".to_string(), oauth_http_server("https://unbound.test/mcp"));
                Some(servers)
            })),
            ..Default::default()
        });
        let status = manager.list_status();
        let enabled = |server: &str| {
            status
                .iter()
                .find(|status| status.server == server)
                .map(|status| status.enabled)
        };
        assert_eq!(enabled("remote"), Some(false));
        assert_eq!(enabled("unbound"), Some(false));
    }

    #[test]
    fn generic_servers_are_name_pattern_gated_and_sorted() {
        reset_oauth_providers();
        let storage = auth_storage();
        {
            let mut guard = storage.try_lock().unwrap();
            guard.set(
                "mcp:zeta",
                AuthCredential::ApiKey {
                    key: "k".to_string(),
                    prime_team: None,
                },
            );
        }
        let manager = manager(McpManagerOptions {
            auth_storage: storage,
            get_user_servers: Some(Arc::new(|| {
                let mut servers = IndexMap::new();
                servers.insert("zeta".to_string(), http_mcp_server_config("https://z.test/mcp"));
                servers.insert("has space".to_string(), http_mcp_server_config("https://s.test/mcp"));
                servers.insert("linear".to_string(), http_mcp_server_config("https://l.test/mcp"));
                Some(servers)
            })),
            ..Default::default()
        });
        let servers = manager.get_enabled_persistent_generic_servers();
        assert_eq!(servers, vec!["zeta".to_string()]);
    }

    #[test]
    fn acp_servers_replace_only_for_the_owner() {
        reset_oauth_providers();
        let mut manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            ..Default::default()
        });
        assert!(manager.can_release_acp_servers("a"));
        assert!(manager.replace_acp_servers(&[], "a").unwrap() == false);

        let server = AcpMcpServerConfig::Http(crate::core::mcp::acp_mcp_types::AcpMcpHttpServerConfig {
            name: "acp".to_string(),
            url: "https://acp.test/mcp".to_string(),
            headers: Map::new(),
        });
        assert!(manager.replace_acp_servers(&[server.clone()], "a").unwrap());
        assert!(!manager.replace_acp_servers(&[server.clone()], "a").unwrap());
        assert!(manager.replace_acp_servers(&[server.clone()], "b").is_err());
        assert!(!manager.can_release_acp_servers("b"));
        assert_eq!(manager.get_acp_servers().len(), 1);

        let duplicate = AcpMcpServerConfig::Http(crate::core::mcp::acp_mcp_types::AcpMcpHttpServerConfig {
            name: "acp".to_string(),
            url: "https://other.test/mcp".to_string(),
            headers: Map::new(),
        });
        assert!(manager
            .replace_acp_servers(&[server.clone(), duplicate], "a")
            .is_err());
    }

    #[tokio::test]
    async fn config_reports_the_acp_credential_source() {
        reset_oauth_providers();
        let mut manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            ..Default::default()
        });
        let server = AcpMcpServerConfig::Http(crate::core::mcp::acp_mcp_types::AcpMcpHttpServerConfig {
            name: "acp".to_string(),
            url: "https://acp.test/mcp".to_string(),
            headers: Map::new(),
        });
        manager.replace_acp_servers(&[server], "a").unwrap();
        let handlers = manager.host_handlers();
        let config = handlers.get("mcp.config").unwrap().clone();
        let value = config(json!({ "server": "acp" })).await.unwrap();
        assert_eq!(value.get("credentialSource"), Some(&json!("acp")));
        assert!(value.get("name").is_none());
        assert_eq!(value.get("url"), Some(&json!("https://acp.test/mcp")));
    }

    #[tokio::test]
    async fn refresh_rejects_acp_servers() {
        reset_oauth_providers();
        let mut manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            ..Default::default()
        });
        let server = AcpMcpServerConfig::Http(crate::core::mcp::acp_mcp_types::AcpMcpHttpServerConfig {
            name: "acp".to_string(),
            url: "https://acp.test/mcp".to_string(),
            headers: Map::new(),
        });
        manager.replace_acp_servers(&[server], "a").unwrap();
        let handlers = manager.host_handlers();
        let refresh = handlers.get("mcp.refresh").unwrap().clone();
        let error = refresh(json!({ "server": "acp" })).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "ACP MCP server acp does not use host OAuth"
        );
    }

    #[test]
    fn stdio_servers_are_authed_by_default() {
        reset_oauth_providers();
        let manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: Some(Arc::new(|| {
                let mut servers = IndexMap::new();
                servers.insert("local".to_string(), stdio_config("node"));
                Some(servers)
            })),
            ..Default::default()
        });
        let status = manager
            .list_status()
            .into_iter()
            .find(|status| status.server == "local");
        assert_eq!(status.map(|status| status.enabled), Some(true));
    }

    #[test]
    fn a_disabled_config_is_never_authed() {
        reset_oauth_providers();
        let manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: Some(Arc::new(|| {
                let mut servers = IndexMap::new();
                let mut config = http_mcp_server_config("https://x.test/mcp");
                if let McpServerConfig::Http(http) = &mut config {
                    http.enabled = Some(false);
                }
                servers.insert("x".to_string(), config);
                Some(servers)
            })),
            ..Default::default()
        });
        let status = manager
            .list_status()
            .into_iter()
            .find(|status| status.server == "x");
        assert_eq!(status.map(|status| status.enabled), Some(false));
    }

    #[test]
    fn bearer_token_env_var_marks_a_server_authed() {
        reset_oauth_providers();
        std::env::set_var("MCP_MANAGER_TEST_TOKEN", "secret");
        let manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: Some(Arc::new(|| {
                let mut servers = IndexMap::new();
                let mut config = oauth_http_server("https://bearer.test/mcp");
                if let McpServerConfig::Http(http) = &mut config {
                    http.bearer_token_env_var = Some("MCP_MANAGER_TEST_TOKEN".to_string());
                }
                servers.insert("bearer".to_string(), config);
                Some(servers)
            })),
            ..Default::default()
        });
        std::env::remove_var("MCP_MANAGER_TEST_TOKEN");
        let status = manager
            .list_status()
            .into_iter()
            .find(|status| status.server == "bearer");
        assert_eq!(status.map(|status| status.enabled), Some(true));
    }
}
