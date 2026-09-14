//! Port of packages/ai/src/mcp/catalog.ts
//!
//! Built-in MCP integrations we ship a skill package for. User servers go in the
//! `mcpServers` setting instead.

use crate::mcp::oauth::{create_mcp_oauth_provider, McpOAuthConfig};
use crate::utils::oauth::get_oauth_provider;
use crate::utils::oauth::register_oauth_provider;

/// `Omit<McpOAuthConfig, "server" | "url"> & { kind: "oauth" }`
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpCatalogOAuth {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<String>,
    #[serde(rename = "clientId", skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpCatalogEntry {
    /// Matches the skill package import name and the auth.json key `mcp:<server>`.
    pub server: String,
    pub label: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpCatalogOAuth>,
}

pub const BUILTIN_MCP_CATALOG: [(&str, &str, &str, &str); 2] = [
    ("linear", "Linear", "https://mcp.linear.app/mcp", "Linear"),
    ("notion", "Notion", "https://mcp.notion.com/mcp", "Notion"),
];

/// `BUILTIN_MCP_CATALOG` as the TypeScript object shape.
pub fn builtin_mcp_catalog() -> Vec<McpCatalogEntry> {
    BUILTIN_MCP_CATALOG
        .iter()
        .map(|(server, label, url, oauth_label)| McpCatalogEntry {
            server: (*server).to_string(),
            label: (*label).to_string(),
            url: (*url).to_string(),
            oauth: Some(McpCatalogOAuth {
                kind: "oauth".to_string(),
                label: Some((*oauth_label).to_string()),
                scopes: None,
                client_id: None,
            }),
        })
        .collect()
}

pub fn get_catalog_entry(server: &str) -> Option<McpCatalogEntry> {
    builtin_mcp_catalog()
        .into_iter()
        .find(|entry| entry.server == server)
}

/// Register the built-in catalog's OAuth providers. Idempotent. Must be called
/// after any `reset_oauth_providers()` (e.g. ModelRegistry.refresh) since reset
/// drops everything but the model-provider built-ins.
pub fn register_builtin_mcp_oauth_providers() {
    for entry in builtin_mcp_catalog() {
        let Some(oauth) = entry.oauth.as_ref() else {
            continue;
        };
        if oauth.kind != "oauth" {
            continue;
        }
        let id = format!("mcp:{}", entry.server);
        if get_oauth_provider(&id).is_some() {
            continue;
        }
        register_oauth_provider(create_mcp_oauth_provider(McpOAuthConfig {
            server: entry.server.clone(),
            label: Some(entry.label.clone()),
            url: entry.url.clone(),
            client_id: oauth.client_id.clone(),
            scopes: oauth.scopes.clone(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::oauth::reset_oauth_providers;

    #[test]
    fn catalog_matches_typescript_entries() {
        let catalog = builtin_mcp_catalog();
        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog[0].server, "linear");
        assert_eq!(catalog[0].url, "https://mcp.linear.app/mcp");
        assert_eq!(catalog[1].server, "notion");
        assert_eq!(catalog[1].url, "https://mcp.notion.com/mcp");
        assert_eq!(
            catalog[0].oauth.as_ref().map(|oauth| oauth.kind.as_str()),
            Some("oauth")
        );
    }

    #[test]
    fn lookup_by_server() {
        assert_eq!(get_catalog_entry("linear").map(|entry| entry.label), Some("Linear".to_string()));
        assert!(get_catalog_entry("nope").is_none());
    }

    #[test]
    fn registers_mcp_oauth_providers_idempotently() {
        let _providers = crate::test_env::ScopedOAuthProviders::new();
        reset_oauth_providers();
        register_builtin_mcp_oauth_providers();
        assert!(get_oauth_provider("mcp:linear").is_some());
        assert!(get_oauth_provider("mcp:notion").is_some());
        let first = get_oauth_provider("mcp:linear").unwrap();
        register_builtin_mcp_oauth_providers();
        let second = get_oauth_provider("mcp:linear").unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(first.name, second.name);
        assert_eq!(first.uses_callback_server, Some(true));
        reset_oauth_providers();
        assert!(get_oauth_provider("mcp:linear").is_none());
    }
}
