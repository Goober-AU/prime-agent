//! Port of packages/coding-agent/src/modes/acp/acp-mcp.ts

use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Local minimal stand-in for `core/mcp/acp-mcp-types.ts` (`AcpMcpServerConfig`).
/// The owning slice has not landed yet; see blocked_on in evidence/status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AcpMcpServerConfig {
    Http {
        name: String,
        url: String,
        headers: Vec<(String, String)>,
    },
    Stdio {
        name: String,
        command: String,
        args: Vec<String>,
        cwd: String,
        env: Vec<(String, String)>,
    },
}

impl AcpMcpServerConfig {
    pub fn http(name: String, url: String, headers: Vec<(String, String)>) -> Self {
        AcpMcpServerConfig::Http { name, url, headers }
    }

    pub fn stdio(name: String, command: String, args: Vec<String>, cwd: String, env: Vec<(String, String)>) -> Self {
        AcpMcpServerConfig::Stdio {
            name,
            command,
            args,
            cwd,
            env,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            AcpMcpServerConfig::Http { name, .. } => name,
            AcpMcpServerConfig::Stdio { name, .. } => name,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            AcpMcpServerConfig::Http { .. } => "http",
            AcpMcpServerConfig::Stdio { .. } => "stdio",
        }
    }

    pub fn cwd_value(&self) -> Option<String> {
        match self {
            AcpMcpServerConfig::Stdio { cwd, .. } => Some(cwd.clone()),
            AcpMcpServerConfig::Http { .. } => None,
        }
    }
}

static SERVER_NAME_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$").expect("static regex"));

/// ACP `McpServer` union: a stdio server, an HTTP server, or an SSE server.
///
/// The TypeScript discriminates on `"command" in server` first, then on
/// `server.type !== "http"`, so an SSE server reaches the unsupported-transport
/// branch. The Rust port keeps the same shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpMcpServer {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<McpEnvEntry>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Vec<McpEnvEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct McpEnvEntry {
    pub name: String,
    pub value: String,
}

/// `RequestError.invalidParams({ reason })` as a Rust error.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{reason}")]
pub struct AcpRequestError {
    pub reason: String,
}

impl AcpRequestError {
    pub fn invalid_params(reason: impl Into<String>) -> Self {
        Self { reason: reason.into() }
    }
}

/// HTTP header name validation, matching Node's `validateHeaderName`.
fn is_valid_header_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.bytes().all(|byte| {
        matches!(
            byte,
            b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|'
                | b'~'
        ) || byte.is_ascii_alphanumeric()
    })
}

/// HTTP header value validation, matching Node's `validateHeaderValue`.
fn is_valid_header_value(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte == b'\t' || (0x20..=0x7e).contains(&byte) || byte >= 0x80)
}

fn entries(server: &str, label: &str, values: &[McpEnvEntry]) -> Result<Vec<(String, String)>, AcpRequestError> {
    let mut result: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for entry in values {
        if entry.name.is_empty() {
            return Err(AcpRequestError::invalid_params(format!(
                "MCP server {server} has an empty {label} name"
            )));
        }
        let identity = if label == "header" {
            entry.name.to_lowercase()
        } else {
            entry.name.clone()
        };
        if seen.contains(&identity) {
            return Err(AcpRequestError::invalid_params(format!(
                "MCP server {server} has duplicate {label} {}",
                entry.name
            )));
        }
        if label == "header" {
            if !is_valid_header_name(&entry.name) || !is_valid_header_value(&entry.value) {
                return Err(AcpRequestError::invalid_params(format!(
                    "MCP server {server} has an invalid HTTP header"
                )));
            }
        } else if entry.name.contains('=') || entry.name.contains('\0') || entry.value.contains('\0') {
            return Err(AcpRequestError::invalid_params(format!(
                "MCP server {server} has an invalid environment entry"
            )));
        }
        seen.insert(identity);
        result.push((entry.name.clone(), entry.value.clone()));
    }
    Ok(result)
}

pub fn resolve_acp_mcp_servers(
    servers: &[AcpMcpServer],
    cwd: &str,
) -> Result<Vec<AcpMcpServerConfig>, AcpRequestError> {
    let mut names: HashSet<String> = HashSet::new();
    let mut resolved = Vec::with_capacity(servers.len());
    for server in servers {
        if !SERVER_NAME_PATTERN.is_match(&server.name) {
            return Err(AcpRequestError::invalid_params(
                "MCP server names must start with an alphanumeric character and contain at most 64 alphanumeric, underscore, or hyphen characters",
            ));
        }
        if names.contains(&server.name) {
            return Err(AcpRequestError::invalid_params(format!(
                "duplicate MCP server name: {}",
                server.name
            )));
        }
        names.insert(server.name.clone());

        if let Some(command) = server.command.as_ref() {
            if command.is_empty() {
                return Err(AcpRequestError::invalid_params(format!(
                    "MCP server {} has no stdio command",
                    server.name
                )));
            }
            if command.contains('\0') || server.args.iter().any(|argument| argument.contains('\0')) {
                return Err(AcpRequestError::invalid_params(format!(
                    "MCP server {} has an invalid stdio command",
                    server.name
                )));
            }
            resolved.push(AcpMcpServerConfig::stdio(
                server.name.clone(),
                command.clone(),
                server.args.clone(),
                cwd.to_string(),
                entries(&server.name, "environment", &server.env)?,
            ));
            continue;
        }

        if server.type_.as_deref() != Some("http") {
            return Err(AcpRequestError::invalid_params(format!(
                "MCP server {} uses unsupported {} transport",
                server.name,
                server.type_.clone().unwrap_or_default()
            )));
        }
        let raw_url = server.url.clone().unwrap_or_default();
        let url = match url::Url::parse(&raw_url) {
            Ok(url) => url,
            Err(_) => {
                return Err(AcpRequestError::invalid_params(format!(
                    "MCP server {} has an invalid HTTP URL",
                    server.name
                )))
            }
        };
        if (url.scheme() != "http" && url.scheme() != "https") || !url.username().is_empty() || url.password().is_some()
        {
            return Err(AcpRequestError::invalid_params(format!(
                "MCP server {} must use an HTTP(S) URL without embedded credentials",
                server.name
            )));
        }
        resolved.push(AcpMcpServerConfig::http(
            server.name.clone(),
            url.to_string(),
            entries(&server.name, "header", &server.headers)?,
        ));
    }
    Ok(resolved)
}

/// `McpServer[]` read from an ACP `session/new` payload.
pub fn parse_mcp_servers(value: Option<&Value>) -> Vec<AcpMcpServer> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| serde_json::from_value::<AcpMcpServer>(item.clone()).ok())
            .collect(),
        _ => Vec::new(),
    }
}

pub fn server_config_to_value(config: &AcpMcpServerConfig) -> Value {
    serde_json::to_value(config).unwrap_or(Value::Object(Map::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio_server(name: &str, command: &str) -> AcpMcpServer {
        AcpMcpServer {
            name: name.to_string(),
            command: Some(command.to_string()),
            args: vec!["--serve".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn resolves_stdio_servers_with_cwd() {
        let resolved = resolve_acp_mcp_servers(&[stdio_server("alpha", "npx")], "C:/work").unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].type_name(), "stdio");
        assert_eq!(resolved[0].cwd_value().as_deref(), Some("C:/work"));
    }

    #[test]
    fn rejects_invalid_names_and_duplicates() {
        let error = resolve_acp_mcp_servers(&[stdio_server("-bad", "npx")], ".").unwrap_err();
        assert!(error.reason.starts_with("MCP server names must start"));
        let error =
            resolve_acp_mcp_servers(&[stdio_server("same", "npx"), stdio_server("same", "npx")], ".").unwrap_err();
        assert_eq!(error.reason, "duplicate MCP server name: same");
    }

    #[test]
    fn rejects_credentials_and_bad_transport() {
        let http = AcpMcpServer {
            name: "web".to_string(),
            type_: Some("http".to_string()),
            url: Some("https://user:pass@example.com/mcp".to_string()),
            ..Default::default()
        };
        let error = resolve_acp_mcp_servers(&[http], ".").unwrap_err();
        assert!(error.reason.contains("without embedded credentials"));

        let sse = AcpMcpServer {
            name: "sse".to_string(),
            type_: Some("sse".to_string()),
            url: Some("https://example.com/sse".to_string()),
            ..Default::default()
        };
        let error = resolve_acp_mcp_servers(&[sse], ".").unwrap_err();
        assert_eq!(error.reason, "MCP server sse uses unsupported sse transport");
    }

    #[test]
    fn rejects_duplicate_headers_case_insensitively() {
        let server = AcpMcpServer {
            name: "web".to_string(),
            type_: Some("http".to_string()),
            url: Some("https://example.com/mcp".to_string()),
            headers: vec![
                McpEnvEntry {
                    name: "Authorization".to_string(),
                    value: "a".to_string(),
                },
                McpEnvEntry {
                    name: "authorization".to_string(),
                    value: "b".to_string(),
                },
            ],
            ..Default::default()
        };
        let error = resolve_acp_mcp_servers(&[server], ".").unwrap_err();
        assert_eq!(error.reason, "MCP server web has duplicate header authorization");
    }

    #[test]
    fn rejects_empty_environment_names() {
        let mut server = stdio_server("alpha", "npx");
        server.env = vec![McpEnvEntry {
            name: String::new(),
            value: "1".to_string(),
        }];
        let error = resolve_acp_mcp_servers(&[server], ".").unwrap_err();
        assert_eq!(error.reason, "MCP server alpha has an empty environment name");
    }
}
