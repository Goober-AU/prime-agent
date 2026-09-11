//! Port of packages/ai/src/env-api-keys.ts
//!
//! The TypeScript lazily dynamic-imports `node:fs`, `node:os` and `node:path`
//! ("NEVER convert to top-level runtime imports - breaks browser/Vite builds").
//! Rust links those modules at build time, so the lazy handles are plain
//! functions; the observable behaviour (env-var precedence, the Vertex ADC
//! existence check, the Bun `/proc/self/environ` fallback) is unchanged.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::types::KnownProvider;

/// Fallback for https://github.com/oven-sh/bun/issues/27802
/// Bun compiled binaries have an empty `process.env` inside sandbox
/// environments on Linux. We can recover the env from `/proc/self/environ`.
///
/// `None` means "not a Bun runtime / not triggered"; `Some(map)` is the cache.
fn proc_env_cache() -> &'static Mutex<Option<Option<HashMap<String, String>>>> {
    static CACHE: Mutex<Option<Option<HashMap<String, String>>>> = Mutex::new(None);
    &CACHE
}

/// `typeof process !== "undefined" && (process.versions?.node || process.versions?.bun)`
fn is_node_like() -> bool {
    true
}

fn is_bun() -> bool {
    std::env::var_os("BUN_INSTALL").is_some() || std::env::var_os("BUN_RUNTIME").is_some()
}

fn get_proc_env(key: &str) -> Option<String> {
    if !is_bun() {
        return None;
    }

    // If process.env already has entries, the bug is not triggered.
    if !std::env::vars().next().is_none() {
        return None;
    }

    let mut cache = proc_env_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.is_none() {
        let mut entries: HashMap<String, String> = HashMap::new();
        if let Ok(data) = std::fs::read_to_string("/proc/self/environ") {
            for entry in data.split('\0') {
                if let Some(index) = entry.find('=') {
                    if index > 0 {
                        entries.insert(entry[..index].to_string(), entry[index + 1..].to_string());
                    }
                }
            }
        }
        // /proc/self/environ may not be readable.
        *cache = Some(Some(entries));
    }
    cache
        .as_ref()
        .and_then(|cached| cached.as_ref())
        .and_then(|entries| entries.get(key).cloned())
}

fn cached_vertex_adc_credentials_exists() -> &'static Mutex<Option<bool>> {
    static CACHE: Mutex<Option<bool>> = Mutex::new(None);
    &CACHE
}

fn has_vertex_adc_credentials() -> bool {
    let mut cache = cached_vertex_adc_credentials_exists()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(value) = *cache {
        return value;
    }

    // If node modules haven't loaded yet (async import race at startup), return
    // false WITHOUT caching so the next call retries once they're ready. Only
    // cache false permanently in a browser environment where fs is never
    // available. Rust always has fs, so `is_node_like()` is true.
    if !is_node_like() {
        *cache = Some(false);
        return false;
    }

    let gac_path = std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
        .ok()
        .or_else(|| get_proc_env("GOOGLE_APPLICATION_CREDENTIALS"));
    let exists = match gac_path {
        Some(path) => std::path::Path::new(&path).exists(),
        None => home_dir()
            .map(|home| {
                home.join(".config")
                    .join("gcloud")
                    .join("application_default_credentials.json")
                    .exists()
            })
            .unwrap_or(false),
    };
    *cache = Some(exists);
    exists
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

pub const ENV_GITHUB_COPILOT: [&str; 3] = ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"];
/// ANTHROPIC_OAUTH_TOKEN takes precedence over ANTHROPIC_API_KEY
pub const ENV_ANTHROPIC: [&str; 2] = ["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"];

fn get_api_key_env_vars(provider: &str) -> Vec<&'static str> {
    if provider == "github-copilot" {
        return ENV_GITHUB_COPILOT.to_vec();
    }
    if provider == "anthropic" {
        return ENV_ANTHROPIC.to_vec();
    }

    let env_var: Option<&'static str> = match provider {
        "openai" => Some("OPENAI_API_KEY"),
        "azure-openai-responses" => Some("AZURE_OPENAI_API_KEY"),
        "prime-inference" => Some("PRIME_API_KEY"),
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "google" => Some("GEMINI_API_KEY"),
        "google-vertex" => Some("GOOGLE_CLOUD_API_KEY"),
        "groq" => Some("GROQ_API_KEY"),
        "cerebras" => Some("CEREBRAS_API_KEY"),
        "xai" => Some("XAI_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "vercel-ai-gateway" => Some("AI_GATEWAY_API_KEY"),
        "zai" => Some("ZAI_API_KEY"),
        "mistral" => Some("MISTRAL_API_KEY"),
        "minimax" => Some("MINIMAX_API_KEY"),
        "minimax-cn" => Some("MINIMAX_CN_API_KEY"),
        "moonshotai" => Some("MOONSHOT_API_KEY"),
        "moonshotai-cn" => Some("MOONSHOT_API_KEY"),
        "huggingface" => Some("HF_TOKEN"),
        "fireworks" => Some("FIREWORKS_API_KEY"),
        "opencode" => Some("OPENCODE_API_KEY"),
        "opencode-go" => Some("OPENCODE_API_KEY"),
        "kimi-coding" => Some("KIMI_API_KEY"),
        "cloudflare-workers-ai" => Some("CLOUDFLARE_API_KEY"),
        "cloudflare-ai-gateway" => Some("CLOUDFLARE_API_KEY"),
        "xiaomi" => Some("XIAOMI_API_KEY"),
        "xiaomi-token-plan-cn" => Some("XIAOMI_TOKEN_PLAN_CN_API_KEY"),
        "xiaomi-token-plan-ams" => Some("XIAOMI_TOKEN_PLAN_AMS_API_KEY"),
        "xiaomi-token-plan-sgp" => Some("XIAOMI_TOKEN_PLAN_SGP_API_KEY"),
        _ => None,
    };

    match env_var {
        Some(env_var) => vec![env_var],
        None => Vec::new(),
    }
}

/// Find configured environment variables that can provide an API key for a provider.
///
/// This only reports actual API key variables. It intentionally excludes ambient
/// credential sources such as AWS profiles, AWS IAM credentials, and Google
/// Application Default Credentials.
pub fn find_env_keys(provider: &str) -> Option<Vec<String>> {
    let env_vars = get_api_key_env_vars(provider);
    if env_vars.is_empty() {
        return None;
    }
    let found: Vec<String> = env_vars
        .iter()
        .filter(|env_var| {
            std::env::var(env_var).map(|value| !value.is_empty()).unwrap_or(false)
                || get_proc_env(env_var).map(|value| !value.is_empty()).unwrap_or(false)
        })
        .map(|env_var| (*env_var).to_string())
        .collect();
    if found.is_empty() {
        None
    } else {
        Some(found)
    }
}

/// Get API key for provider from known environment variables, e.g. OPENAI_API_KEY.
///
/// Will not return API keys for providers that require OAuth tokens.
pub fn get_env_api_key(provider: &KnownProvider) -> Option<String> {
    if let Some(env_keys) = find_env_keys(provider) {
        if let Some(first) = env_keys.first() {
            if let Ok(value) = std::env::var(first) {
                if !value.is_empty() {
                    return Some(value);
                }
            }
            if let Some(value) = get_proc_env(first) {
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
    }

    if provider == "google-vertex" {
        let has_credentials = has_vertex_adc_credentials();
        let has_project = [
            "GOOGLE_CLOUD_PROJECT",
            "GCLOUD_PROJECT",
        ]
        .iter()
        .any(|key| {
            std::env::var(key).map(|value| !value.is_empty()).unwrap_or(false)
                || get_proc_env(key).map(|value| !value.is_empty()).unwrap_or(false)
        });
        let has_location = ["GOOGLE_CLOUD_LOCATION"].iter().any(|key| {
            std::env::var(key).map(|value| !value.is_empty()).unwrap_or(false)
                || get_proc_env(key).map(|value| !value.is_empty()).unwrap_or(false)
        });

        if has_credentials && has_project && has_location {
            return Some("<authenticated>".to_string());
        }
    }

    if provider == "amazon-bedrock" {
        let has_aws_env = |key: &str| {
            std::env::var(key).map(|value| !value.is_empty()).unwrap_or(false)
                || get_proc_env(key).map(|value| !value.is_empty()).unwrap_or(false)
        };
        if has_aws_env("AWS_PROFILE")
            || (has_aws_env("AWS_ACCESS_KEY_ID") && has_aws_env("AWS_SECRET_ACCESS_KEY"))
            || has_aws_env("AWS_BEARER_TOKEN_BEDROCK")
            || has_aws_env("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
            || has_aws_env("AWS_CONTAINER_CREDENTIALS_FULL_URI")
            || has_aws_env("AWS_WEB_IDENTITY_TOKEN_FILE")
        {
            return Some("<authenticated>".to_string());
        }
    }

    None
}

pub fn get_prime_team_id() -> Option<String> {
    let from_env = std::env::var("PRIME_TEAM_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| get_proc_env("PRIME_TEAM_ID").filter(|value| !value.trim().is_empty()));
    if let Some(from_env) = from_env {
        return Some(from_env.trim().to_string());
    }

    let home = home_dir()?;
    let config_path = home.join(".prime").join("config.json");
    if !config_path.exists() {
        return None;
    }
    let Ok(contents) = std::fs::read_to_string(&config_path) else {
        return None;
    };
    // Treat unreadable or malformed config as no configured team.
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return None;
    };
    let team_id = parsed.as_object().and_then(|object| object.get("team_id"))?;
    let team_id = team_id.as_str()?;
    if team_id.trim().is_empty() {
        return None;
    }
    Some(team_id.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_table_matches_typescript() {
        assert_eq!(get_api_key_env_vars("openai"), vec!["OPENAI_API_KEY"]);
        assert_eq!(get_api_key_env_vars("google"), vec!["GEMINI_API_KEY"]);
        assert_eq!(
            get_api_key_env_vars("anthropic"),
            vec!["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]
        );
        assert_eq!(
            get_api_key_env_vars("github-copilot"),
            vec!["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]
        );
        assert_eq!(get_api_key_env_vars("moonshotai-cn"), vec!["MOONSHOT_API_KEY"]);
        assert!(get_api_key_env_vars("amazon-bedrock").is_empty());
        assert!(get_api_key_env_vars("unknown-provider").is_empty());
    }

    #[test]
    fn anthropic_oauth_token_takes_precedence() {
        std::env::set_var("ANTHROPIC_OAUTH_TOKEN", "oauth-token");
        std::env::set_var("ANTHROPIC_API_KEY", "api-key");
        let keys = find_env_keys("anthropic").unwrap();
        assert_eq!(keys, vec!["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]);
        assert_eq!(get_env_api_key(&"anthropic".to_string()).as_deref(), Some("oauth-token"));

        std::env::remove_var("ANTHROPIC_OAUTH_TOKEN");
        assert_eq!(get_env_api_key(&"anthropic".to_string()).as_deref(), Some("api-key"));
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert_eq!(find_env_keys("anthropic"), None);
    }

    #[test]
    fn unknown_provider_has_no_env_key() {
        assert_eq!(find_env_keys("nope"), None);
        assert_eq!(get_env_api_key(&"nope".to_string()), None);
    }

    #[test]
    fn bedrock_credentials_report_authenticated() {
        std::env::remove_var("AWS_PROFILE");
        std::env::remove_var("AWS_ACCESS_KEY_ID");
        std::env::remove_var("AWS_SECRET_ACCESS_KEY");
        std::env::remove_var("AWS_BEARER_TOKEN_BEDROCK");
        assert_eq!(get_env_api_key(&"amazon-bedrock".to_string()), None);

        std::env::set_var("AWS_PROFILE", "default");
        assert_eq!(
            get_env_api_key(&"amazon-bedrock".to_string()).as_deref(),
            Some("<authenticated>")
        );
        std::env::remove_var("AWS_PROFILE");
    }
}
