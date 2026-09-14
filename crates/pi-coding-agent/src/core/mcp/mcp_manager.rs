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
use pi_ai::utils::oauth::{register_oauth_provider, unregister_oauth_provider};
use serde_json::{json, Map, Value};

use crate::core::auth_storage::{AuthCredential, AuthStorage};
use crate::core::kernel::shared::{HostRequestHandler, HostRequestHandlers, KernelError};
use crate::core::mcp::acp_mcp_types::AcpMcpServerConfig;
use crate::core::settings_manager::{HttpMcpServerConfig, McpServerConfig};

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

/// `interface ResolvedIntegration` - private in the TypeScript class.
#[derive(Debug, Clone, PartialEq)]
struct ResolvedIntegration {
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
    host_view: Arc<std::sync::RwLock<McpManagerView>>,
}

impl McpManager {
    pub fn new(options: McpManagerOptions) -> Self {
        let host_view = Arc::new(std::sync::RwLock::new(McpManagerView {
            auth_storage: options.auth_storage.clone(),
            integrations: IndexMap::new(),
            acp_servers: IndexMap::new(),
        }));
        let mut manager = Self {
            auth_storage: options.auth_storage,
            get_user_servers: options.get_user_servers.unwrap_or_else(|| Arc::new(|| None)),
            begin_login: options.begin_login,
            integrations: IndexMap::new(),
            acp_servers: IndexMap::new(),
            acp_owner_id: None,
            registered_user_provider_ids: Vec::new(),
            host_view,
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
        match &self.acp_owner_id {
            None => true,
            Some(owner) => owner == owner_id,
        }
    }

    pub fn replace_acp_servers(
        &mut self,
        servers: &[AcpMcpServerConfig],
        owner_id: &str,
    ) -> Result<bool, String> {
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
        // `JSON.stringify(this.acpServers.get(name)) === JSON.stringify(config)`
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
        self.acp_servers = next;
        self.acp_owner_id = if self.acp_servers.is_empty() {
            None
        } else {
            Some(owner_id.to_string())
        };
        self.update_host_view();
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
                ResolvedIntegration {
                    server: entry.server.clone(),
                    label: entry.label.clone(),
                    config: http_server(entry.url.clone(), Some(true), None, None),
                    uses_oauth,
                    user_declared: false,
                },
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
        self.update_host_view();
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
            if integration.uses_oauth {
                current.push(id);
                register_oauth_provider(create_mcp_oauth_provider(McpOAuthConfig {
                    server: integration.server.clone(),
                    label: Some(integration.label.clone()),
                    url: integration.url().unwrap_or_default().to_string(),
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
        if !integration.uses_oauth && bearer_token_env_var.is_none() {
            return true;
        }
        if let Some(name) = bearer_token_env_var {
            // `process.env[name]?.trim()`
            if std::env::var(name)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                return true;
            }
        }
        // `this.authStorage.get(...)` is synchronous in the port, but the port keeps
        // the storage behind a `tokio` mutex, so the lock must be non-blocking here.
        let cred = match self.auth_storage.try_lock() {
            Ok(storage) => storage.get(&Self::provider_id(&integration.server)),
            Err(_) => None,
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
            let manager = refresh_manager.read().unwrap_or_else(|p| p.into_inner()).clone();
            Box::pin(async move {
                let server = server_from_payload(&payload, "mcp.refresh requires a server")?;
                if manager.acp_servers.contains_key(&server) {
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
            let manager = config_manager.read().unwrap_or_else(|p| p.into_inner()).clone();
            Box::pin(async move {
                let server = server_from_payload(&payload, "mcp.config requires a server")?;
                if let Some(acp_server) = manager.acp_servers.get(&server) {
                    // `const { name: _name, ...config } = acpServer;`
                    let mut value = serde_json::to_value(acp_server)
                        .map_err(|error| KernelError::new(error.to_string()))?;
                    if let Value::Object(map) = &mut value {
                        map.shift_remove("name");
                        map.insert("credentialSource".to_string(), Value::String("acp".to_string()));
                    }
                    return Ok(value);
                }
                let Some(integration) = manager.integrations.get(&server) else {
                    return Ok(json!({}));
                };
                if !integration.user_declared || get_catalog_entry(&server).is_some() {
                    return Ok(json!({}));
                }
                serde_json::to_value(&integration.config)
                    .map_err(|error| KernelError::new(error.to_string()))
            })
        });
        handlers.insert("mcp.config".to_string(), config);

        // Only expose begin_login when an interactive login is actually wired, so the
        // kernel doesn't get a handler whose only behavior is to throw.
        if let Some(begin_login) = self.begin_login.clone() {
            let begin_login: HostRequestHandler = Arc::new(move |payload: Value| {
                let begin_login = begin_login.clone();
                Box::pin(async move {
                    let server = server_from_payload(&payload, "mcp.begin_login requires a server")?;
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
        // `sort((left, right) => left.localeCompare(right))`
        servers.sort_by(|left, right| left.cmp(right));
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

    /// The manager state the host handlers read after construction.
    fn shared_view(&self) -> Arc<std::sync::RwLock<McpManagerView>> {
        self.host_view.clone()
    }

    fn update_host_view(&self) {
        // Handlers outlive their creation call and read the current session
        // configuration. Never hold this lock during OAuth/network awaits.
        *self.host_view.write().unwrap_or_else(|p| p.into_inner()) = McpManagerView {
            auth_storage: self.auth_storage.clone(),
            integrations: self.integrations.clone(),
            acp_servers: self.acp_servers.clone(),
        };
    }
}

/// `String(payload.server ?? "")`.
fn server_from_payload(payload: &Value, missing_message: &str) -> Result<String, KernelError> {
    let server = payload
        .get("server")
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default();
    if server.is_empty() {
        return Err(KernelError::new(missing_message));
    }
    Ok(server)
}

/// `{ type: "http", ... }` settings entry.
fn http_server(
    url: String,
    oauth: Option<bool>,
    bearer_token_env_var: Option<String>,
    headers: Option<Map<String, Value>>,
) -> McpServerConfig {
    McpServerConfig::Http(HttpMcpServerConfig {
        url,
        headers,
        bearer_token_env_var,
        oauth,
        enabled: None,
        enabled_tools: None,
        disabled_tools: None,
        startup_timeout_ms: None,
        call_timeout_ms: None,
    })
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
#[derive(Clone)]
struct McpManagerView {
    auth_storage: Arc<tokio::sync::Mutex<AuthStorage>>,
    integrations: IndexMap<String, ResolvedIntegration>,
    acp_servers: IndexMap<String, AcpMcpServerConfig>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::utils::oauth::{get_oauth_provider, reset_oauth_providers};
    use serde_json::Map;

    fn auth_storage() -> Arc<tokio::sync::Mutex<AuthStorage>> {
        Arc::new(tokio::sync::Mutex::new(AuthStorage::in_memory(IndexMap::new(), None)))
    }

    fn oauth_credential(access: &str, endpoint: Option<&str>) -> AuthCredential {
        let mut extra = Map::new();
        if let Some(endpoint) = endpoint {
            extra.insert("endpoint".to_string(), Value::String(endpoint.to_string()));
        }
        AuthCredential::OAuth {
            credentials: pi_ai::utils::oauth::OAuthCredentials {
                refresh: "r".to_string(),
                access: access.to_string(),
                expires: f64::MAX,
                extra,
            },
        }
    }

    fn user_servers(
        entries: Vec<(&'static str, McpServerConfig)>,
    ) -> Option<Arc<dyn Fn() -> Option<IndexMap<String, McpServerConfig>> + Send + Sync>> {
        Some(Arc::new(move || {
            let mut servers: IndexMap<String, McpServerConfig> = IndexMap::new();
            for (name, config) in entries.clone() {
                servers.insert(name.to_string(), config);
            }
            Some(servers)
        }))
    }

    fn manager(options: McpManagerOptions) -> McpManager {
        McpManager::new(options)
    }

    fn status_enabled(manager: &McpManager, server: &str) -> Option<bool> {
        manager
            .list_status()
            .into_iter()
            .find(|status| status.server == server)
            .map(|status| status.enabled)
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
        storage
            .try_lock()
            .unwrap()
            .set("mcp:linear", oauth_credential("tok", None));
        let manager = manager(McpManagerOptions {
            auth_storage: storage,
            ..Default::default()
        });
        let overrides = manager.get_disabled_builtin_skill_overrides();
        assert!(!overrides.contains(&"-linear/SKILL.md".to_string()));
        assert!(overrides.contains(&"-notion/SKILL.md".to_string()));
        assert_eq!(status_enabled(&manager, "linear"), Some(true));
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
    fn keeps_mcp_providers_registered_after_a_registry_reset() {
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
    fn re_registers_user_declared_oauth_servers_after_a_registry_reset() {
        reset_oauth_providers();
        let mut manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: user_servers(vec![(
                "acme",
                http_server("https://mcp.acme.test/mcp".to_string(), Some(true), None, None),
            )]),
            ..Default::default()
        });
        assert!(get_oauth_provider("mcp:acme").is_some());
        reset_oauth_providers();
        manager.register_user_providers();
        assert!(get_oauth_provider("mcp:acme").is_some());
    }

    #[tokio::test]
    async fn exposes_refresh_and_config_and_reports_missing_credentials() {
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
            get_user_servers: user_servers(vec![(
                "linear",
                http_server("https://proxy.test/mcp".to_string(), Some(true), None, None),
            )]),
            ..Default::default()
        });
        let config = manager.host_handlers().get("mcp.config").unwrap().clone();
        assert_eq!(config(json!({ "server": "linear" })).await.unwrap(), json!({}));
        // Catalog-only entries are reserved for their authored skills, not the generic API.
        assert_eq!(config(json!({ "server": "notion" })).await.unwrap(), json!({}));
    }

    #[test]
    fn does_not_treat_an_oauth_override_of_a_catalog_name_as_authed() {
        reset_oauth_providers();
        let storage = auth_storage();
        storage
            .try_lock()
            .unwrap()
            .set("mcp:linear", oauth_credential("official", None));
        let manager = manager(McpManagerOptions {
            auth_storage: storage,
            get_user_servers: user_servers(vec![(
                "linear",
                http_server("https://proxy.test/mcp".to_string(), Some(true), None, None),
            )]),
            ..Default::default()
        });
        assert_eq!(status_enabled(&manager, "linear"), Some(false));
    }

    #[test]
    fn does_not_enable_a_server_from_a_mismatched_or_unbound_credential() {
        reset_oauth_providers();
        let storage = auth_storage();
        {
            let mut guard = storage.try_lock().unwrap();
            guard.set("mcp:unbound", oauth_credential("unbound-token", None));
            guard.set(
                "mcp:remote",
                oauth_credential("old-token", Some("https://old.test/mcp")),
            );
        }
        let manager = manager(McpManagerOptions {
            auth_storage: storage,
            get_user_servers: user_servers(vec![
                (
                    "remote",
                    http_server("https://new.test/mcp".to_string(), Some(true), None, None),
                ),
                (
                    "unbound",
                    http_server("https://srv.test/mcp".to_string(), Some(true), None, None),
                ),
            ]),
            ..Default::default()
        });
        assert_eq!(status_enabled(&manager, "remote"), Some(false));
        assert_eq!(status_enabled(&manager, "unbound"), Some(false));
        assert!(manager.get_enabled_persistent_generic_servers().is_empty());
    }

    #[test]
    fn honors_a_bearer_token_env_var_for_user_declared_servers() {
        reset_oauth_providers();
        std::env::set_var("MY_MCP_TOKEN", "secret");
        let manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: user_servers(vec![(
                "custom",
                http_server(
                    "https://example.test/mcp".to_string(),
                    None,
                    Some("MY_MCP_TOKEN".to_string()),
                    None,
                ),
            )]),
            ..Default::default()
        });
        let enabled = status_enabled(&manager, "custom");
        std::env::remove_var("MY_MCP_TOKEN");
        assert_eq!(enabled, Some(true));
    }

    #[test]
    fn lists_only_enabled_non_catalog_user_servers_in_deterministic_order() {
        reset_oauth_providers();
        let manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: user_servers(vec![
                ("zebra", stdio_server("z", None)),
                ("disabled", stdio_server("off", Some(false))),
                ("linear", stdio_server("reserved", None)),
                (
                    "alpha",
                    http_server("https://alpha.test/mcp".to_string(), None, None, None),
                ),
            ]),
            ..Default::default()
        });
        assert_eq!(
            manager.get_enabled_persistent_generic_servers(),
            vec!["alpha".to_string(), "zebra".to_string()]
        );
    }

    #[test]
    fn picks_up_mcp_servers_added_after_construction_on_refresh() {
        reset_oauth_providers();
        let servers: Arc<std::sync::Mutex<Option<IndexMap<String, McpServerConfig>>>> =
            Arc::new(std::sync::Mutex::new(Some(IndexMap::new())));
        let servers_for_manager = servers.clone();
        let mut manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: Some(Arc::new(move || {
                servers_for_manager.lock().unwrap().clone()
            })),
            ..Default::default()
        });
        assert!(manager
            .list_status()
            .into_iter()
            .find(|status| status.server == "acme")
            .is_none());

        {
            let mut guard = servers.lock().unwrap();
            let mut next: IndexMap<String, McpServerConfig> = IndexMap::new();
            next.insert(
                "acme".to_string(),
                http_server("https://mcp.acme.test/mcp".to_string(), Some(true), None, None),
            );
            *guard = Some(next);
        }
        manager.refresh();
        assert!(manager
            .list_status()
            .into_iter()
            .find(|status| status.server == "acme")
            .is_some());
        assert!(get_oauth_provider("mcp:acme").is_some());
    }

    #[test]
    fn keeps_the_builtin_provider_when_a_user_server_uses_a_reserved_catalog_name() {
        reset_oauth_providers();
        let _ = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: user_servers(vec![(
                "linear",
                http_server("https://proxy.test/mcp".to_string(), Some(true), None, None),
            )]),
            ..Default::default()
        });
        let provider = get_oauth_provider("mcp:linear").expect("builtin provider");
        assert_eq!(provider.name, "Linear");
    }

    #[test]
    fn unregisters_a_user_servers_oauth_provider_when_it_is_removed_on_refresh() {
        reset_oauth_providers();
        let servers: Arc<std::sync::Mutex<Option<IndexMap<String, McpServerConfig>>>> =
            Arc::new(std::sync::Mutex::new({
                let mut initial: IndexMap<String, McpServerConfig> = IndexMap::new();
                initial.insert(
                    "acme".to_string(),
                    http_server("https://mcp.acme.test/mcp".to_string(), Some(true), None, None),
                );
                Some(initial)
            }));
        let servers_for_manager = servers.clone();
        let mut manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: Some(Arc::new(move || servers_for_manager.lock().unwrap().clone())),
            ..Default::default()
        });
        assert!(get_oauth_provider("mcp:acme").is_some());

        *servers.lock().unwrap() = Some(IndexMap::new());
        manager.refresh();
        assert!(get_oauth_provider("mcp:acme").is_none());
    }

    #[tokio::test]
    async fn serves_user_stdio_configuration_without_resolving_tagged_env_values() {
        reset_oauth_providers();
        let config = McpServerConfig::Stdio(crate::core::settings_manager::StdioMcpServerConfig {
            command: "node".to_string(),
            args: Some(vec!["server.js".to_string(), "--raw".to_string()]),
            cwd: Some("/tmp/work".to_string()),
            env: Some({
                let mut env = Map::new();
                env.insert("TOKEN".to_string(), json!({ "env": "MCP_TOKEN" }));
                env
            }),
            enabled: None,
            enabled_tools: Some(vec!["raw.tool/name".to_string()]),
            disabled_tools: None,
            startup_timeout_ms: None,
            call_timeout_ms: None,
        });
        let expected = serde_json::to_value(&config).unwrap();
        let manager = manager(McpManagerOptions {
            auth_storage: auth_storage(),
            get_user_servers: user_servers(vec![("local", config)]),
            ..Default::default()
        });
        let config_handler = manager.host_handlers().get("mcp.config").unwrap().clone();
        assert_eq!(config_handler(json!({ "server": "local" })).await.unwrap(), expected);
        assert_eq!(status_enabled(&manager, "local"), Some(true));
    }

    #[test]
    fn does_not_enable_an_authored_catalog_skill_when_a_generic_server_shadows_its_name() {
        reset_oauth_providers();
        for config in [
            stdio_server("node", None),
            http_server("https://proxy.test/mcp".to_string(), None, None, None),
        ] {
            let manager = manager(McpManagerOptions {
                auth_storage: auth_storage(),
                get_user_servers: user_servers(vec![("linear", config)]),
                ..Default::default()
            });
            assert!(manager
                .get_disabled_builtin_skill_overrides()
                .contains(&"-linear/SKILL.md".to_string()));
        }
    }

    #[tokio::test]
    async fn keeps_acp_credentials_session_scoped_and_isolated_from_stored_oauth() {
        reset_oauth_providers();
        let storage = auth_storage();
        storage.try_lock().unwrap().set(
            "mcp:task",
            oauth_credential("stored-oauth-token", Some("https://user.example/mcp")),
        );
        let mut manager = manager(McpManagerOptions {
            auth_storage: storage.clone(),
            get_user_servers: user_servers(vec![(
                "task",
                http_server("https://user.example/mcp".to_string(), Some(true), None, None),
            )]),
            ..Default::default()
        });
        let acp_server = AcpMcpServerConfig::Http(crate::core::mcp::acp_mcp_types::AcpMcpHttpServerConfig {
            name: "task".to_string(),
            url: "https://task.example/mcp".to_string(),
            headers: {
                let mut headers = Map::new();
                headers.insert(
                    "Authorization".to_string(),
                    Value::String("Bearer task-token".to_string()),
                );
                headers
            },
        });
        assert!(manager.replace_acp_servers(&[acp_server], "owner-a").unwrap());

        let handlers = manager.host_handlers();
        let config = handlers.get("mcp.config").unwrap().clone();
        assert_eq!(
            config(json!({ "server": "task" })).await.unwrap(),
            json!({
                "type": "http",
                "url": "https://task.example/mcp",
                "headers": { "Authorization": "Bearer task-token" },
                "credentialSource": "acp",
            })
        );
        let refresh = handlers.get("mcp.refresh").unwrap().clone();
        let error = refresh(json!({ "server": "task" })).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "ACP MCP server task does not use host OAuth"
        );
        assert!(manager
            .get_acp_servers()
            .iter()
            .any(|server| server.name() == "task"));

        assert!(!manager.replace_acp_servers(&[], "owner-b").unwrap());
        let other = AcpMcpServerConfig::Http(crate::core::mcp::acp_mcp_types::AcpMcpHttpServerConfig {
            name: "other".to_string(),
            url: "https://other.example/mcp".to_string(),
            headers: Map::new(),
        });
        assert!(manager.replace_acp_servers(&[other], "owner-b").is_err());
        assert_eq!(
            config(json!({ "server": "task" })).await.unwrap().get("url"),
            Some(&json!("https://task.example/mcp"))
        );

        assert!(manager.replace_acp_servers(&[], "owner-a").unwrap());
        assert_eq!(
            config(json!({ "server": "task" })).await.unwrap(),
            json!({ "type": "http", "url": "https://user.example/mcp", "oauth": true })
        );
        let stored = storage
            .try_lock()
            .unwrap()
            .get("mcp:task")
            .expect("stored credential");
        match stored {
            AuthCredential::OAuth { credentials } => assert_eq!(credentials.access, "stored-oauth-token"),
            other => panic!("unexpected credential {other:?}"),
        }
    }

    fn stdio_server(command: &str, enabled: Option<bool>) -> McpServerConfig {
        McpServerConfig::Stdio(crate::core::settings_manager::StdioMcpServerConfig {
            command: command.to_string(),
            args: None,
            cwd: None,
            env: None,
            enabled,
            enabled_tools: None,
            disabled_tools: None,
            startup_timeout_ms: None,
            call_timeout_ms: None,
        })
    }
}
