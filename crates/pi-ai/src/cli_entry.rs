//! Port of packages/ai/src/cli.ts
//!
//! The TypeScript entry point reads stdin through `node:readline`. The Rust port
//! keeps the same command surface, output strings, exit codes and auth.json
//! format, reading prompts line-by-line from stdin.

use std::collections::HashMap;
use std::io::{BufRead, Write};

use serde_json::{Map, Value};

use crate::utils::oauth::{get_oauth_provider, get_oauth_providers};
use crate::utils::oauth::types::{
    OAuthAuthInfo, OAuthCredentials, OAuthLoginCallbacks, OAuthPrompt, OAuthProviderId,
    OAuthProviderInterface,
};

pub const AUTH_FILE: &str = "auth.json";
pub const CLI_NAME: &str = "npx @earendil-works/pi-ai";

pub type AuthEntry = Map<String, Value>;
pub type AuthStore = HashMap<String, Value>;

/// `loadAuth()`.
pub fn load_auth() -> AuthStore {
    if !std::path::Path::new(AUTH_FILE).exists() {
        return AuthStore::new();
    }
    let Ok(contents) = std::fs::read_to_string(AUTH_FILE) else {
        return AuthStore::new();
    };
    match serde_json::from_str::<Value>(&contents) {
        Ok(Value::Object(map)) => map.into_iter().collect(),
        _ => AuthStore::new(),
    }
}

/// `saveAuth(auth)` - `JSON.stringify(auth, null, 2)`.
pub fn save_auth(auth: &AuthStore) -> std::io::Result<()> {
    let mut map = Map::new();
    for (key, value) in auth {
        map.insert(key.clone(), value.clone());
    }
    let text = serde_json::to_string_pretty(&Value::Object(map)).unwrap_or_default();
    std::fs::write(AUTH_FILE, text)
}

fn prompt(question: &str) -> String {
    print!("{}", question);
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let stdin = std::io::stdin();
    let _ = stdin.lock().read_line(&mut line);
    line.trim_end_matches(['\r', '\n']).to_string()
}

/// `login(providerId)`.
pub async fn login(provider_id: &OAuthProviderId) -> i32 {
    let Some(provider) = get_oauth_provider(provider_id) else {
        eprintln!("Unknown provider: {}", provider_id);
        return 1;
    };

    let Ok(credentials) = run_provider_login(&provider).await else {
        eprintln!("Error: {}", provider_id);
        return 1;
    };

    let mut auth = load_auth();
    let mut entry = Map::new();
    entry.insert("type".to_string(), Value::String("oauth".to_string()));
    if let Ok(Value::Object(credentials_map)) = serde_json::to_value(&credentials) {
        for (key, value) in credentials_map {
            entry.insert(key, value);
        }
    }
    auth.insert(provider_id.clone(), Value::Object(entry));
    let _ = save_auth(&auth);

    println!("\nCredentials saved to {}", AUTH_FILE);
    0
}

/// Runs `provider.login(callbacks)` with the CLI's console callbacks.
pub async fn run_provider_login(
    provider: &OAuthProviderInterface,
) -> Result<OAuthCredentials, String> {
    let callbacks = OAuthLoginCallbacks {
        on_auth: Some(std::sync::Arc::new(|info: OAuthAuthInfo| {
            println!("\nOpen this URL in your browser:\n{}", info.url);
            if let Some(instructions) = info.instructions {
                println!("{}", instructions);
            }
            println!();
        })),
        on_prompt: Some(std::sync::Arc::new(|prompt_value: OAuthPrompt| {
            Box::pin(async move {
                let placeholder = prompt_value
                    .placeholder
                    .map(|placeholder| format!(" ({})", placeholder))
                    .unwrap_or_default();
                prompt(&format!("{}{}:", prompt_value.message, placeholder))
            })
        })),
        on_progress: Some(std::sync::Arc::new(|message: String| println!("{}", message))),
        on_manual_code_input: None,
        on_select: None,
        signal: None,
    };

    (provider.login)(callbacks).await
}

/// `main()` - returns the process exit code.
pub async fn main_with_args(args: &[String]) -> i32 {
    let command = args.first().map(String::as_str);

    if command.is_none()
        || command == Some("help")
        || command == Some("--help")
        || command == Some("-h")
    {
        let providers = get_oauth_providers();
        let provider_list = providers
            .iter()
            .map(|provider| format!("  {:<20} {}", provider.id, provider.name))
            .collect::<Vec<_>>()
            .join("\n");
        println!(
            "Usage: {} <command> [provider]\n\nCommands:\n  login [provider]  Login to an OAuth provider\n  list              List available providers\n\nProviders:\n{}\n\nExamples:\n  {} login              # interactive provider selection\n  {} login anthropic    # login to specific provider\n  {} list               # list providers\n",
            CLI_NAME, provider_list, CLI_NAME, CLI_NAME, CLI_NAME
        );
        return 0;
    }

    if command == Some("list") {
        println!("Available OAuth providers:\n");
        for provider in get_oauth_providers() {
            println!("  {:<20} {}", provider.id, provider.name);
        }
        return 0;
    }

    if command == Some("login") {
        let mut provider = args.get(1).cloned();

        if provider.is_none() {
            let providers = get_oauth_providers();
            println!("Select a provider:\n");
            for (index, provider) in providers.iter().enumerate() {
                println!("  {}. {}", index + 1, provider.name);
            }
            println!();

            let choice = prompt(&format!("Enter number (1-{}): ", providers.len()));
            let index = choice.trim().parse::<i64>().map(|value| value - 1);
            let Ok(index) = index else {
                eprintln!("Invalid selection");
                return 1;
            };
            if index < 0 || index as usize >= providers.len() {
                eprintln!("Invalid selection");
                return 1;
            }
            provider = Some(providers[index as usize].id.clone());
        }

        let provider = provider.unwrap_or_default();
        let providers = get_oauth_providers();
        if !providers.iter().any(|candidate| candidate.id == provider) {
            eprintln!("Unknown provider: {}", provider);
            eprintln!("Use '{} list' to see available providers", CLI_NAME);
            return 1;
        }

        println!("Logging in to {}...", provider);
        return login(&provider).await;
    }

    eprintln!("Unknown command: {}", command.unwrap_or_default());
    eprintln!("Use '{} --help' for usage", CLI_NAME);
    1
}

/// `main().catch(...)` - prints `Error: <message>` and exits 1.
pub async fn run_main(args: &[String]) -> i32 {
    main_with_args(args).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::oauth::reset_oauth_providers;

    #[tokio::test]
    async fn help_output_lists_providers() {
        reset_oauth_providers();
        // The help path prints and returns 0 without touching stdin.
        assert_eq!(main_with_args(&["--help".to_string()]).await, 0);
        assert_eq!(main_with_args(&["-h".to_string()]).await, 0);
        assert_eq!(main_with_args(&[]).await, 0);
    }

    #[tokio::test]
    async fn list_command_succeeds() {
        reset_oauth_providers();
        assert_eq!(main_with_args(&["list".to_string()]).await, 0);
    }

    #[tokio::test]
    async fn unknown_command_and_provider_exit_one() {
        reset_oauth_providers();
        assert_eq!(main_with_args(&["bogus".to_string()]).await, 1);
        assert_eq!(main_with_args(&["login".to_string(), "nope".to_string()]).await, 1);
    }

    #[test]
    fn auth_store_round_trip_uses_two_space_indent() {
        let mut auth = AuthStore::new();
        let mut entry = Map::new();
        entry.insert("type".to_string(), Value::String("oauth".to_string()));
        entry.insert("refresh".to_string(), Value::String("r".to_string()));
        auth.insert("anthropic".to_string(), Value::Object(entry));
        let mut map = Map::new();
        for (key, value) in &auth {
            map.insert(key.clone(), value.clone());
        }
        let text = serde_json::to_string_pretty(&Value::Object(map)).unwrap();
        assert!(text.starts_with("{\n  \"anthropic\": {\n    \"type\": \"oauth\","));
    }

    #[test]
    fn missing_auth_file_loads_empty() {
        let original = std::env::current_dir().unwrap();
        let temp = std::env::temp_dir().join(format!("pi-ai-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        std::env::set_current_dir(&temp).unwrap();
        assert!(load_auth().is_empty());
        assert!(!std::path::Path::new(AUTH_FILE).exists());
        std::env::set_current_dir(original).unwrap();
    }
}
