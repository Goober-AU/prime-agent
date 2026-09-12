//! Port of packages/coding-agent/src/core/resolve-config-value.ts
//!
//! Resolve configuration values that may be shell commands, environment
//! variables, or literals. Used by auth-storage.ts and model-registry.ts.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::utils::child_process::{exec_sync_hidden, spawn_sync_hidden, SpawnOptions};
use crate::utils::shell::get_shell_config;

/// `const commandResultCache = new Map<string, string | undefined>()`.
///
/// `undefined` is a cached value in the TypeScript (a `has()` hit), so the Rust
/// map stores `Option<String>` and membership is the cache hit.
static COMMAND_RESULT_CACHE: std::sync::LazyLock<Mutex<HashMap<String, Option<String>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// The Node default timeout used by both shell executions.
const CONFIG_VALUE_TIMEOUT_MS: u64 = 10_000;

/// `resolveConfigValue(config)`.
///
/// - If it starts with "!", executes the rest as a shell command and uses
///   stdout (cached)
/// - Otherwise checks the environment variable first, then treats it as a
///   literal (not cached)
pub fn resolve_config_value(config: &str) -> Option<String> {
    if config.starts_with('!') {
        return execute_command(config);
    }
    resolve_env_or_literal(config)
}

/// `resolveEnvOrLiteral(config)`.
///
/// Unset env var: fall back to the literal string. Set-but-empty: missing
/// credential, never the var name.
fn resolve_env_or_literal(config: &str) -> Option<String> {
    match std::env::var(config) {
        Ok(value) => {
            if value.is_empty() {
                None
            } else {
                Some(value)
            }
        }
        Err(_) => Some(config.to_string()),
    }
}

struct ConfiguredShellResult {
    executed: bool,
    value: Option<String>,
}

/// `executeWithConfiguredShell(command)`.
fn execute_with_configured_shell(command: &str) -> ConfiguredShellResult {
    let config = match get_shell_config(None) {
        Ok(config) => config,
        Err(_) => {
            return ConfiguredShellResult {
                executed: false,
                value: None,
            }
        }
    };
    let mut args: Vec<String> = config.args.clone();
    args.push(command.to_string());
    let result = spawn_sync_hidden(
        &config.shell,
        &args,
        SpawnOptions {
            capture_stdout: true,
            ..Default::default()
        },
    );
    match result {
        Err(error) => {
            // `error.code === "ENOENT"` means the shell could not be found, so
            // the fallback shell is tried instead; any other error is "executed".
            if error.kind() == std::io::ErrorKind::NotFound {
                ConfiguredShellResult {
                    executed: false,
                    value: None,
                }
            } else {
                ConfiguredShellResult {
                    executed: true,
                    value: None,
                }
            }
        }
        Ok(result) => {
            if result.status.code() != Some(0) {
                return ConfiguredShellResult {
                    executed: true,
                    value: None,
                };
            }
            let value = String::from_utf8_lossy(&result.stdout).trim().to_string();
            ConfiguredShellResult {
                executed: true,
                value: if value.is_empty() { None } else { Some(value) },
            }
        }
    }
}

/// `executeWithDefaultShell(command)`.
fn execute_with_default_shell(command: &str) -> Option<String> {
    let _timeout_ms = CONFIG_VALUE_TIMEOUT_MS;
    match exec_sync_hidden(
        command,
        SpawnOptions {
            capture_stdout: true,
            ..Default::default()
        },
    ) {
        Ok(output) => {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if value.is_empty() {
                None
            } else {
                Some(value)
            }
        }
        Err(_) => None,
    }
}

/// `executeCommandUncached(commandConfig)`.
fn execute_command_uncached(command_config: &str) -> Option<String> {
    let command = &command_config[1..];
    if process_platform_is_win32() {
        let configured_result = execute_with_configured_shell(command);
        if configured_result.executed {
            configured_result.value
        } else {
            execute_with_default_shell(command)
        }
    } else {
        execute_with_default_shell(command)
    }
}

/// `process.platform === "win32"`.
fn process_platform_is_win32() -> bool {
    cfg!(windows)
}

/// `executeCommand(commandConfig)`.
fn execute_command(command_config: &str) -> Option<String> {
    {
        let cache = COMMAND_RESULT_CACHE.lock().expect("command cache poisoned");
        if let Some(cached) = cache.get(command_config) {
            return cached.clone();
        }
    }
    let result = execute_command_uncached(command_config);
    COMMAND_RESULT_CACHE
        .lock()
        .expect("command cache poisoned")
        .insert(command_config.to_string(), result.clone());
    result
}

/// `resolveConfigValueUncached(config)`.
pub fn resolve_config_value_uncached(config: &str) -> Option<String> {
    if config.starts_with('!') {
        return execute_command_uncached(config);
    }
    resolve_env_or_literal(config)
}

/// `resolveConfigValueOrThrow(config, description)`.
pub fn resolve_config_value_or_throw(config: &str, description: &str) -> Result<String, String> {
    if let Some(resolved_value) = resolve_config_value_uncached(config) {
        return Ok(resolved_value);
    }
    if config.starts_with('!') {
        return Err(format!(
            "Failed to resolve {description} from shell command: {}",
            &config[1..]
        ));
    }
    Err(format!("Failed to resolve {description}"))
}

/// `resolveHeaders(headers)`.
pub fn resolve_headers(headers: Option<&indexmap::IndexMap<String, String>>) -> Option<indexmap::IndexMap<String, String>> {
    let headers = headers?;
    let mut resolved: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
    for (key, value) in headers {
        if let Some(resolved_value) = resolve_config_value(value) {
            resolved.insert(key.clone(), resolved_value);
        }
    }
    if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    }
}

/// `resolveHeadersOrThrow(headers, description)`.
pub fn resolve_headers_or_throw(
    headers: Option<&indexmap::IndexMap<String, String>>,
    description: &str,
) -> Result<Option<indexmap::IndexMap<String, String>>, String> {
    let Some(headers) = headers else {
        return Ok(None);
    };
    let mut resolved: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
    for (key, value) in headers {
        resolved.insert(
            key.clone(),
            resolve_config_value_or_throw(value, &format!("{description} header \"{key}\""))?,
        );
    }
    if resolved.is_empty() {
        Ok(None)
    } else {
        Ok(Some(resolved))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VAR: &str = "PRIME_AGENT_TEST_CREDENTIAL_VAR";

    #[test]
    fn env_var_value_is_used_when_set() {
        std::env::set_var(VAR, "secret-value");
        assert_eq!(resolve_config_value(VAR).as_deref(), Some("secret-value"));
        assert_eq!(resolve_config_value_uncached(VAR).as_deref(), Some("secret-value"));
        std::env::remove_var(VAR);
    }

    #[test]
    fn unset_env_var_falls_back_to_the_literal() {
        std::env::remove_var(VAR);
        assert_eq!(resolve_config_value(VAR).as_deref(), Some(VAR));
        assert_eq!(resolve_config_value("sk-literal-key").as_deref(), Some("sk-literal-key"));
    }

    #[test]
    fn set_but_empty_env_var_is_a_missing_credential() {
        std::env::set_var(VAR, "");
        assert_eq!(resolve_config_value(VAR), None);
        assert_eq!(resolve_config_value_uncached(VAR), None);
        assert_eq!(
            resolve_config_value_or_throw(VAR, "test credential").unwrap_err(),
            "Failed to resolve test credential"
        );
        std::env::remove_var(VAR);
    }

    #[test]
    fn shell_command_failures_report_the_command_text() {
        let error = resolve_config_value_or_throw("!definitely-not-a-real-binary-xyz", "test credential")
            .unwrap_err();
        assert!(error.starts_with("Failed to resolve test credential"));
    }

    #[test]
    fn resolve_headers_drops_unresolvable_values_and_missing_input() {
        std::env::remove_var(VAR);
        assert!(resolve_headers(None).is_none());
        let mut headers: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
        headers.insert("x-literal".to_string(), "value".to_string());
        let resolved = resolve_headers(Some(&headers)).unwrap();
        assert_eq!(resolved.get("x-literal").map(String::as_str), Some("value"));

        let mut empty_var_headers: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
        empty_var_headers.insert("x-empty".to_string(), VAR.to_string());
        std::env::set_var(VAR, "");
        assert!(resolve_headers(Some(&empty_var_headers)).is_none());
        std::env::remove_var(VAR);
    }

    #[test]
    fn resolve_headers_or_throw_names_the_header() {
        std::env::set_var(VAR, "secret");
        let mut headers: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
        headers.insert("authorization".to_string(), VAR.to_string());
        let resolved = resolve_headers_or_throw(Some(&headers), "provider").unwrap().unwrap();
        assert_eq!(resolved.get("authorization").map(String::as_str), Some("secret"));
        std::env::remove_var(VAR);

        std::env::set_var(VAR, "");
        let error = resolve_headers_or_throw(Some(&headers), "provider").unwrap_err();
        assert_eq!(error, "Failed to resolve provider header \"authorization\"");
        std::env::remove_var(VAR);
    }

    #[test]
    fn resolve_headers_or_throw_returns_none_without_headers() {
        assert!(resolve_headers_or_throw(None, "provider").unwrap().is_none());
    }
}
