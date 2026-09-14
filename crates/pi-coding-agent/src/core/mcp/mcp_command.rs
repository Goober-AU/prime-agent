//! Port of packages/coding-agent/src/core/mcp/mcp-command.ts

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::core::settings_manager::{
    HttpMcpServerConfig, McpServerConfig, SettingsManager, StdioMcpServerConfig,
};

pub type McpManagementAction = String;

/// `McpManagementResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpManagementResult {
    pub action: McpManagementAction,
    pub message: String,
    pub changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_change: Option<McpServerChange>,
}

/// `serverChange` block of `McpManagementResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerChange {
    pub name: String,
    pub transport: String,
    pub verb: String,
    pub uses_oauth: bool,
}

/// `NAME_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/`.
fn validate_name(name: &str) -> Result<String, String> {
    let invalid = || {
        "MCP server names must be 1-64 letters, numbers, underscores, or hyphens and start with a letter or number."
            .to_string()
    };
    let chars: Vec<char> = name.chars().collect();
    if chars.is_empty() || chars.len() > 64 {
        return Err(invalid());
    }
    if !chars[0].is_ascii_alphanumeric() {
        return Err(invalid());
    }
    if !chars[1..]
        .iter()
        .all(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
    {
        return Err(invalid());
    }
    Ok(name.to_string())
}

/// `ENV_NAME_PATTERN = /^[A-Za-z_][A-Za-z0-9_]*$/`.
fn validate_env_name(value: &str, option: &str) -> Result<String, String> {
    let chars: Vec<char> = value.chars().collect();
    let valid = !chars.is_empty()
        && (chars[0].is_ascii_alphabetic() || chars[0] == '_')
        && chars[1..]
            .iter()
            .all(|ch| ch.is_ascii_alphanumeric() || *ch == '_');
    if !valid {
        return Err(format!("{option} requires an environment variable name."));
    }
    Ok(value.to_string())
}

/// Disk-verified removal: throws when the credential may still exist on disk.
pub trait McpCredentialStore: Send + Sync {
    fn remove_verified(&mut self, provider: &str) -> Result<(), String>;
}

/// blocked_on: needs pi_ai::mcp::catalog::get_catalog_entry
fn get_catalog_entry(server: &str) -> Option<()> {
    if server == "linear" || server == "notion" {
        Some(())
    } else {
        None
    }
}

/// `runMcpManagementCommand`.
pub async fn run_mcp_management_command(
    args: &[String],
    settings_manager: &mut SettingsManager,
    auth_storage: Option<&mut dyn McpCredentialStore>,
) -> Result<McpManagementResult, String> {
    let action = args.first().cloned().unwrap_or_default();
    match action.as_str() {
        "list" => {
            require_count(args, 1, "mcp list")?;
            return Ok(McpManagementResult {
                action,
                message: format_mcp_server_list(settings_manager.get_global_mcp_servers().as_ref()),
                changed: false,
                server_change: None,
            });
        }
        "get" => {
            require_count(args, 2, "mcp get <name>")?;
            let name = validate_name(&args[1])?;
            let servers = settings_manager.get_global_mcp_servers();
            let config = servers.as_ref().and_then(|servers| servers.get(&name)).cloned();
            let Some(config) = config else {
                return Err(format!("MCP server \"{name}\" was not found."));
            };
            let config = parse_server_config(&config)?;
            return Ok(McpManagementResult {
                action,
                message: format_mcp_server(&name, &config),
                changed: false,
                server_change: None,
            });
        }
        "remove" => {
            require_count(args, 2, "mcp remove <name>")?;
            let name = validate_name(&args[1])?;
            let servers = settings_manager.get_global_mcp_servers();
            let config = servers.as_ref().and_then(|servers| servers.get(&name)).cloned();
            let Some(config) = config else {
                return Err(format!("MCP server \"{name}\" was not found."));
            };
            let config = parse_server_config(&config)?;
            if !settings_manager.remove_global_mcp_server(&name) {
                return Err(format!("MCP server \"{name}\" was not found."));
            }
            flush_global_settings(settings_manager).await?;
            drop_server_credentials(&name, auth_storage)?;
            return Ok(McpManagementResult {
                action,
                message: format!("Removed MCP server \"{name}\"."),
                changed: true,
                server_change: Some(McpServerChange {
                    name,
                    transport: config.transport().to_string(),
                    verb: "removed".to_string(),
                    uses_oauth: config.is_oauth_http(),
                }),
            });
        }
        "add" => {
            let parsed = parse_mcp_add_args(&args[1..])?;
            let servers = settings_manager.get_global_mcp_servers();
            let replaced = servers
                .as_ref()
                .map(|servers| servers.contains_key(&parsed.name))
                .unwrap_or(false);
            if replaced && !parsed.force {
                return Err(format!(
                    "MCP server \"{}\" already exists. Use --force to replace it.",
                    parsed.name
                ));
            }
            // Any add may repoint a name an authored skill resolves by (e.g. slack); a
            // stored token must never replay there. Verified drop first: every partial
            // failure lands on re-login, never on old-token-with-new-URL.
            drop_server_credentials(&parsed.name, auth_storage)?;
            settings_manager.set_global_mcp_server(
                &parsed.name,
                config_to_value(&parsed.config),
                parsed.force,
            );
            flush_global_settings(settings_manager).await?;
            return Ok(McpManagementResult {
                action,
                message: format!(
                    "{} MCP server \"{}\".",
                    if replaced { "Replaced" } else { "Added" },
                    parsed.name
                ),
                changed: true,
                server_change: Some(McpServerChange {
                    name: parsed.name,
                    transport: parsed.config.transport().to_string(),
                    verb: if replaced { "replaced" } else { "added" }.to_string(),
                    uses_oauth: parsed.config.is_oauth_http(),
                }),
            });
        }
        _ => Err("Usage: mcp <add|list|get|remove>.".to_string()),
    }
}

fn parse_server_config(value: &Value) -> Result<McpServerConfig, String> {
    serde_json::from_value(value.clone()).map_err(|error| error.to_string())
}

fn config_to_value(config: &McpServerConfig) -> Value {
    serde_json::to_value(config).unwrap_or(Value::Null)
}

impl McpServerConfig {
    pub fn transport(&self) -> &'static str {
        match self {
            McpServerConfig::Http(_) => "http",
            McpServerConfig::Stdio(_) => "stdio",
        }
    }

    /// `config.type === "http" && config.oauth === true`.
    pub fn is_oauth_http(&self) -> bool {
        match self {
            McpServerConfig::Http(config) => config.oauth == Some(true),
            McpServerConfig::Stdio(_) => false,
        }
    }
}

/// Result of `parseMcpAddArgs`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedMcpAddArgs {
    pub name: String,
    pub config: McpServerConfig,
    pub force: bool,
}

/// `parseMcpAddArgs`.
pub fn parse_mcp_add_args(args: &[String]) -> Result<ParsedMcpAddArgs, String> {
    let name = validate_name(args.first().map(String::as_str).unwrap_or(""))?;
    if get_catalog_entry(&name).is_some() {
        return Err(format!(
            "MCP server name \"{name}\" is reserved for a built-in integration."
        ));
    }
    let separator = args.iter().position(|arg| arg == "--");
    let option_args: &[String] = match separator {
        Some(index) => &args[1..index],
        None => &args[1.min(args.len())..],
    };
    let command_args: &[String] = match separator {
        Some(index) => &args[index + 1..],
        None => &[],
    };

    let mut url: Option<String> = None;
    let mut bearer_token_env_var: Option<String> = None;
    let mut oauth = false;
    let mut force = false;
    let mut cwd: Option<String> = None;
    // `Object.create(null)` - insertion order preserved via BTreeMap for stable output.
    let mut env: BTreeMap<String, Value> = BTreeMap::new();
    let mut seen_options: Vec<String> = Vec::new();

    let mut index = 0;
    while index < option_args.len() {
        let option = option_args[index].clone();
        if option != "--env" && seen_options.contains(&option) {
            return Err(format!("Duplicate MCP add option: {option}"));
        }
        seen_options.push(option.clone());
        if option == "--oauth" || option == "--force" {
            if option == "--oauth" {
                oauth = true;
            } else {
                force = true;
            }
            index += 1;
            continue;
        }
        if option != "--url" && option != "--bearer-token-env-var" && option != "--cwd" && option != "--env" {
            return Err(format!("Unknown MCP add option: {option}"));
        }
        index += 1;
        let value = option_args.get(index).cloned();
        let Some(value) = value else {
            return Err(format!("{option} requires a value."));
        };
        if value.is_empty() {
            return Err(format!("{option} requires a value."));
        }
        if option == "--url" {
            url = Some(value);
        } else if option == "--bearer-token-env-var" {
            bearer_token_env_var = Some(validate_env_name(&value, &option)?);
        } else if option == "--cwd" {
            cwd = Some(value);
        } else {
            let Some(equals) = value.find('=') else {
                return Err(
                    "--env must use CHILD=SOURCE, where both sides are environment variable names."
                        .to_string(),
                );
            };
            if equals == 0 || equals == value.len() - 1 {
                return Err(
                    "--env must use CHILD=SOURCE, where both sides are environment variable names."
                        .to_string(),
                );
            }
            let child = validate_env_name(&value[..equals], "--env child")?;
            let source = validate_env_name(&value[equals + 1..], "--env source")?;
            if env.contains_key(&child) {
                return Err(format!("Duplicate child environment variable: {child}"));
            }
            env.insert(child, serde_json::json!({ "env": source }));
        }
        index += 1;
    }

    if separator.is_some() {
        if url.is_some() || bearer_token_env_var.is_some() || oauth {
            return Err("Stdio MCP servers cannot use HTTP options.".to_string());
        }
        if command_args.is_empty() || command_args[0].trim().is_empty() {
            return Err("A command is required after --.".to_string());
        }
        if command_args.iter().any(|part| part.contains('\0')) {
            return Err("MCP command arguments cannot contain NUL.".to_string());
        }
        let env_map: Map<String, Value> = env.into_iter().collect();
        return Ok(ParsedMcpAddArgs {
            name,
            force,
            config: McpServerConfig::Stdio(StdioMcpServerConfig {
                command: command_args[0].clone(),
                args: if command_args.len() > 1 {
                    Some(command_args[1..].to_vec())
                } else {
                    None
                },
                cwd,
                env: if env_map.is_empty() { None } else { Some(env_map) },
                enabled: None,
                enabled_tools: None,
                disabled_tools: None,
                startup_timeout_ms: None,
                call_timeout_ms: None,
            }),
        });
    }

    if cwd.is_some() || !env.is_empty() {
        return Err("--cwd and --env require a stdio command after --.".to_string());
    }
    let Some(url) = url else {
        return Err("Use --url <url> for HTTP or -- <command> [args...] for stdio.".to_string());
    };
    if bearer_token_env_var.is_some() && oauth {
        return Err("--oauth and --bearer-token-env-var cannot be combined.".to_string());
    }
    Ok(ParsedMcpAddArgs {
        name,
        force,
        config: McpServerConfig::Http(HttpMcpServerConfig {
            url: validate_http_url(&url)?,
            headers: None,
            bearer_token_env_var,
            oauth: if oauth { Some(true) } else { None },
            enabled: None,
            enabled_tools: None,
            disabled_tools: None,
            startup_timeout_ms: None,
            call_timeout_ms: None,
        }),
    })
}

/// `formatMcpServerList`.
pub fn format_mcp_server_list(servers: Option<&Map<String, Value>>) -> String {
    let empty = Map::new();
    let servers = servers.unwrap_or(&empty);
    let mut entries: Vec<(&String, &Value)> = servers.iter().collect();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    if entries.is_empty() {
        return "No user-configured MCP servers.".to_string();
    }
    entries
        .into_iter()
        .filter_map(|(name, config)| {
            parse_server_config(config)
                .ok()
                .map(|config| format_mcp_server_summary(name, &config))
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// `formatMcpServer`.
pub fn format_mcp_server(name: &str, config: &McpServerConfig) -> String {
    format!("{name}: {}", config.transport())
}

fn format_mcp_server_summary(name: &str, config: &McpServerConfig) -> String {
    format_mcp_server(name, config)
}

fn validate_http_url(value: &str) -> Result<String, String> {
    let parsed = url::Url::parse(value).map_err(|_| format!("Invalid MCP URL: {value}"))?;
    let scheme_ok = parsed.scheme() == "http" || parsed.scheme() == "https";
    let has_host = parsed.host_str().map(|host| !host.is_empty()).unwrap_or(false);
    let has_credentials = !parsed.username().is_empty() || parsed.password().is_some();
    if !scheme_ok || !has_host || has_credentials {
        return Err("MCP URL must be an http(s) URL without embedded credentials.".to_string());
    }
    Ok(parsed.to_string())
}

async fn flush_global_settings(settings_manager: &mut SettingsManager) -> Result<(), String> {
    settings_manager.flush().await;
    let errors = settings_manager.drain_errors(Some("global"));
    if let Some(error) = errors.into_iter().next() {
        return Err(error.error.to_string());
    }
    Ok(())
}

fn drop_server_credentials(
    name: &str,
    auth_storage: Option<&mut dyn McpCredentialStore>,
) -> Result<(), String> {
    // A catalog-named mcp:<name> credential belongs to the authored built-in
    // integration; the generic runtime never serves catalog names.
    if get_catalog_entry(name).is_some() {
        return Ok(());
    }
    let Some(store) = auth_storage else {
        return Ok(());
    };
    match store.remove_verified(&format!("mcp:{name}")) {
        Ok(()) => Ok(()),
        Err(error) => Err(format!("Could not remove stored credentials for \"{name}\": {error}")),
    }
}

fn require_count(args: &[String], count: usize, usage: &str) -> Result<(), String> {
    if args.len() != count {
        return Err(format!("Usage: {usage}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_env_validation_matches_the_patterns() {
        assert_eq!(validate_name("slack").unwrap(), "slack");
        assert_eq!(validate_name("a_b-c9").unwrap(), "a_b-c9");
        assert!(validate_name("-bad").is_err());
        assert!(validate_name("bad name").is_err());
        assert!(validate_name(&"a".repeat(65)).is_err());
        assert_eq!(
            validate_name("bad name").unwrap_err(),
            "MCP server names must be 1-64 letters, numbers, underscores, or hyphens and start with a letter or number."
        );

        assert_eq!(validate_env_name("TOKEN", "--env child").unwrap(), "TOKEN");
        assert!(validate_env_name("1TOKEN", "--env child").is_err());
        assert_eq!(
            validate_env_name("1TOKEN", "--env child").unwrap_err(),
            "--env child requires an environment variable name."
        );
    }

    #[test]
    fn parses_http_add_with_oauth() {
        let args = vec![
            "my-server".to_string(),
            "--url".to_string(),
            "https://example.com/mcp".to_string(),
            "--oauth".to_string(),
        ];
        let parsed = parse_mcp_add_args(&args).unwrap();
        assert_eq!(parsed.name, "my-server");
        assert!(!parsed.force);
        match parsed.config {
            McpServerConfig::Http(config) => {
                assert_eq!(config.url, "https://example.com/mcp");
                assert_eq!(config.oauth, Some(true));
                assert!(config.bearer_token_env_var.is_none());
            }
            _ => panic!("expected http"),
        }
    }

    #[test]
    fn parses_stdio_add_with_env_and_cwd() {
        let args = vec![
            "local".to_string(),
            "--cwd".to_string(),
            "/srv".to_string(),
            "--env".to_string(),
            "CHILD=SOURCE".to_string(),
            "--".to_string(),
            "node".to_string(),
            "server.js".to_string(),
        ];
        let parsed = parse_mcp_add_args(&args).unwrap();
        match parsed.config {
            McpServerConfig::Stdio(config) => {
                assert_eq!(config.command, "node");
                assert_eq!(config.args, Some(vec!["server.js".to_string()]));
                assert_eq!(config.cwd.as_deref(), Some("/srv"));
                assert_eq!(
                    config.env.unwrap().get("CHILD"),
                    Some(&serde_json::json!({"env": "SOURCE"}))
                );
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn rejects_reserved_duplicate_and_invalid_options() {
        let reserved = parse_mcp_add_args(&["linear".to_string()]).unwrap_err();
        assert_eq!(
            reserved,
            "MCP server name \"linear\" is reserved for a built-in integration."
        );

        let duplicate = parse_mcp_add_args(&[
            "s".to_string(),
            "--url".to_string(),
            "https://a.example/mcp".to_string(),
            "--url".to_string(),
            "https://b.example/mcp".to_string(),
        ])
        .unwrap_err();
        assert_eq!(duplicate, "Duplicate MCP add option: --url");

        let unknown = parse_mcp_add_args(&["s".to_string(), "--nope".to_string()]).unwrap_err();
        assert_eq!(unknown, "Unknown MCP add option: --nope");

        let missing_value = parse_mcp_add_args(&["s".to_string(), "--url".to_string()]).unwrap_err();
        assert_eq!(missing_value, "--url requires a value.");

        let empty_value =
            parse_mcp_add_args(&["s".to_string(), "--url".to_string(), "".to_string()]).unwrap_err();
        assert_eq!(empty_value, "--url requires a value.");

        let bad_env = parse_mcp_add_args(&[
            "s".to_string(),
            "--env".to_string(),
            "NOEQUALS".to_string(),
            "--".to_string(),
            "cmd".to_string(),
        ])
        .unwrap_err();
        assert_eq!(
            bad_env,
            "--env must use CHILD=SOURCE, where both sides are environment variable names."
        );

        let dup_child = parse_mcp_add_args(&[
            "s".to_string(),
            "--env".to_string(),
            "A=B".to_string(),
            "--env".to_string(),
            "A=C".to_string(),
            "--".to_string(),
            "cmd".to_string(),
        ])
        .unwrap_err();
        assert_eq!(dup_child, "Duplicate child environment variable: A");
    }

    #[test]
    fn stdio_and_http_option_conflicts_are_reported() {
        let conflict = parse_mcp_add_args(&[
            "s".to_string(),
            "--url".to_string(),
            "https://a.example/mcp".to_string(),
            "--".to_string(),
            "cmd".to_string(),
        ])
        .unwrap_err();
        assert_eq!(conflict, "Stdio MCP servers cannot use HTTP options.");

        let missing_command = parse_mcp_add_args(&["s".to_string(), "--".to_string()]).unwrap_err();
        assert_eq!(missing_command, "A command is required after --.");

        let nul = parse_mcp_add_args(&[
            "s".to_string(),
            "--".to_string(),
            "cmd".to_string(),
            "a\0b".to_string(),
        ])
        .unwrap_err();
        assert_eq!(nul, "MCP command arguments cannot contain NUL.");

        let orphan_cwd = parse_mcp_add_args(&[
            "s".to_string(),
            "--cwd".to_string(),
            "/srv".to_string(),
        ])
        .unwrap_err();
        assert_eq!(orphan_cwd, "--cwd and --env require a stdio command after --.");

        let no_transport = parse_mcp_add_args(&["s".to_string()]).unwrap_err();
        assert_eq!(no_transport, "Use --url <url> for HTTP or -- <command> [args...] for stdio.");

        let combined = parse_mcp_add_args(&[
            "s".to_string(),
            "--url".to_string(),
            "https://a.example/mcp".to_string(),
            "--oauth".to_string(),
            "--bearer-token-env-var".to_string(),
            "TOKEN".to_string(),
        ])
        .unwrap_err();
        assert_eq!(combined, "--oauth and --bearer-token-env-var cannot be combined.");
    }

    #[test]
    fn http_url_validation_rejects_credentials_and_bad_schemes() {
        assert_eq!(
            validate_http_url("https://example.com/mcp").unwrap(),
            "https://example.com/mcp"
        );
        assert_eq!(
            validate_http_url("not a url").unwrap_err(),
            "Invalid MCP URL: not a url"
        );
        assert_eq!(
            validate_http_url("ftp://example.com/x").unwrap_err(),
            "MCP URL must be an http(s) URL without embedded credentials."
        );
        assert_eq!(
            validate_http_url("https://user:pass@example.com/x").unwrap_err(),
            "MCP URL must be an http(s) URL without embedded credentials."
        );
    }

    #[test]
    fn server_list_sorts_names_and_reports_the_empty_case() {
        assert_eq!(format_mcp_server_list(None), "No user-configured MCP servers.");
        let mut servers = Map::new();
        servers.insert(
            "zeta".to_string(),
            serde_json::json!({"type": "http", "url": "https://z.example/mcp"}),
        );
        servers.insert(
            "alpha".to_string(),
            serde_json::json!({"type": "stdio", "command": "node"}),
        );
        assert_eq!(
            format_mcp_server_list(Some(&servers)),
            "alpha: stdio\nzeta: http"
        );
        assert_eq!(
            format_mcp_server(
                "one",
                &McpServerConfig::Http(HttpMcpServerConfig {
                    url: "https://a.example/mcp".to_string(),
                    headers: None,
                    bearer_token_env_var: None,
                    oauth: None,
                    enabled: None,
                    enabled_tools: None,
                    disabled_tools: None,
                    startup_timeout_ms: None,
                    call_timeout_ms: None,
                })
            ),
            "one: http"
        );
    }
}
