//! Port of packages/coding-agent/src/core/mcp/acp-mcp-types.ts

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `AcpMcpServerConfig` - a session-scoped MCP server supplied by an ACP client.
///
/// The TypeScript union discriminates on `type`; serde uses the same tag so the
/// wire shape is unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AcpMcpServerConfig {
    Http(AcpMcpHttpServerConfig),
    Stdio(AcpMcpStdioServerConfig),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpMcpHttpServerConfig {
    pub name: String,
    pub url: String,
    pub headers: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpMcpStdioServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: Map<String, Value>,
}

impl AcpMcpServerConfig {
    pub fn name(&self) -> &str {
        match self {
            AcpMcpServerConfig::Http(config) => &config.name,
            AcpMcpServerConfig::Stdio(config) => &config.name,
        }
    }

    /// The TypeScript `type` discriminant.
    pub fn transport(&self) -> &'static str {
        match self {
            AcpMcpServerConfig::Http(_) => "http",
            AcpMcpServerConfig::Stdio(_) => "stdio",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_variant_serialises_with_a_lowercase_type_tag() {
        let config = AcpMcpServerConfig::Http(AcpMcpHttpServerConfig {
            name: "acp-http".to_string(),
            url: "https://example.com/mcp".to_string(),
            headers: Map::new(),
        });
        let value = serde_json::to_value(&config).unwrap();
        assert_eq!(value["type"], serde_json::json!("http"));
        assert_eq!(value["name"], serde_json::json!("acp-http"));
        assert_eq!(config.transport(), "http");
        assert_eq!(config.name(), "acp-http");
    }

    #[test]
    fn stdio_variant_keeps_the_declared_fields() {
        let config = AcpMcpServerConfig::Stdio(AcpMcpStdioServerConfig {
            name: "acp-stdio".to_string(),
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            cwd: "/tmp".to_string(),
            env: Map::new(),
        });
        let value = serde_json::to_value(&config).unwrap();
        assert_eq!(value["type"], serde_json::json!("stdio"));
        assert_eq!(value["args"], serde_json::json!(["server.js"]));
        assert_eq!(value["cwd"], serde_json::json!("/tmp"));
    }
}
