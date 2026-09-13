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
    OAuthAuthInfo, OAuthCredentials, OAuthLoginCallbacks, OAuthPrompt, OAuthProviderInterface,
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

/// JavaScript `Number.parseInt(string, 10)` (cli.ts:106).
///
/// It skips leading whitespace, accepts an optional `+`/`-` sign, then takes
/// the longest run of decimal digits and ignores the rest of the string
/// ("2x" -> 2, "1.9" -> 1, "" -> NaN). `None` is the NaN result.
fn js_parse_int_radix_10(text: &str) -> Option<f64> {
    let rest = text.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    let (negative, rest) = match rest.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, rest.strip_prefix('+').unwrap_or(rest)),
    };
    let digits = rest
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    let magnitude = digits
        .parse::<f64>()
        .unwrap_or_else(|_| panic!("digit run is a valid float"));
    Some(if negative { -magnitude } else { magnitude })
}

/// `parseInt(choice, 10) - 1` from cli.ts:106 for the interactive provider
/// selection.
///
/// The whole string is not required to be numeric: a numeric prefix is enough,
/// exactly like `parseInt`. `NaN - 1` keeps `NaN` in JavaScript, and the range
/// check at cli.ts:107-110 rejects it with "Invalid selection", so NaN maps to
/// `None` here.
fn selection_index(choice: &str) -> Option<i64> {
    match js_parse_int_radix_10(choice) {
        Some(value) if value.is_nan() => None,
        Some(value) if value.is_infinite() => None,
        Some(value) => Some(value as i64 - 1),
        None => None,
    }
}

fn prompt(question: &str) -> String {
    print!("{}", question);
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let stdin = std::io::stdin();
    let _ = stdin.lock().read_line(&mut line);
    line.trim_end_matches(['\r', '\n']).to_string()
}

/// The line printed by `main().catch` for a rejected login: cli.ts:130-133 is
/// `console.error("Error:", err.message); process.exit(1);`. The provider id is
/// not part of that message, so it must not appear here.
fn login_error_message(error: &str) -> String {
    format!("Error: {}", error)
}

/// `login(providerId)`.
pub async fn login(provider_id: &str) -> i32 {
    let Some(provider) = get_oauth_provider(provider_id) else {
        eprintln!("Unknown provider: {}", provider_id);
        return 1;
    };

    let credentials = match run_provider_login(&provider).await {
        Ok(credentials) => credentials,
        Err(error) => {
            eprintln!("{}", login_error_message(&error));
            return 1;
        }
    };

    let mut auth = load_auth();
    let mut entry = Map::new();
    entry.insert("type".to_string(), Value::String("oauth".to_string()));
    if let Ok(Value::Object(credentials_map)) = serde_json::to_value(&credentials) {
        for (key, value) in credentials_map {
            entry.insert(key, value);
        }
    }
    auth.insert(provider_id.to_string(), Value::Object(entry));
    // cli.ts:24-26 - `saveAuth` calls `writeFileSync`, which throws on failure
    // (read-only directory, disk full). That throw reaches `main().catch`
    // (cli.ts:130-133), so the CLI prints `Error: <message>` and exits 1
    // without ever printing the "Credentials saved" line.
    if let Err(error) = save_auth(&auth) {
        eprintln!("Error: {}", error);
        return 1;
    }

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
            // cli.ts:106 - `const index = parseInt(choice, 10) - 1;`
            let Some(index) = selection_index(&choice) else {
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
        return login(provider.as_str()).await;
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
    use crate::utils::oauth::{register_oauth_provider, reset_oauth_providers};

    /// Serializes the tests that change the process working directory, so a
    /// guard can be held across await points without making the test future
    /// non-`Send`.
    fn cwd_lock() -> CwdGuard {
        static HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        while HELD
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        CwdGuard(&HELD)
    }

    /// Holds the process-wide cwd lock until the creating test drops it.
    struct CwdGuard(&'static std::sync::atomic::AtomicBool);

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

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

    /// cli.ts:24-26 + 130-133 - the save path must report the real write error.
    #[test]
    fn save_auth_reports_write_failures() {
        let _guard = cwd_lock();
        let original = std::env::current_dir().unwrap();
        let temp = std::env::temp_dir().join(format!("pi-ai-cli-save-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(temp.join(AUTH_FILE)).unwrap();
        std::env::set_current_dir(&temp).unwrap();

        let mut auth = AuthStore::new();
        let mut entry = Map::new();
        entry.insert("type".to_string(), Value::String("oauth".to_string()));
        auth.insert("test-oauth".to_string(), Value::Object(entry));
        let error = save_auth(&auth).expect_err("writing auth.json over a directory must fail");
        assert!(
            login_error_message(&error.to_string()).starts_with("Error: "),
            "cli.ts:130-133 prints `Error: <message>`"
        );
        // The message is what `main().catch` prints as `Error: <message>`, so it
        // has to be non-empty; the concrete kind differs per OS (Windows
        // returns PermissionDenied, POSIX returns IsADirectory).
        assert!(
            !error.to_string().is_empty(),
            "a failed save needs a printable message, got kind {:?}",
            error.kind()
        );

        std::env::set_current_dir(&original).unwrap();
        let _ = std::fs::remove_dir_all(&temp);
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

    fn test_provider(
        login: impl Fn(OAuthLoginCallbacks) -> crate::types::BoxFuture<Result<OAuthCredentials, String>>
            + Send
            + Sync
            + 'static,
    ) -> OAuthProviderInterface {
        OAuthProviderInterface {
            id: "test-oauth".to_string(),
            name: "Test OAuth".to_string(),
            login: std::sync::Arc::new(login),
            uses_callback_server: None,
            refresh_token: std::sync::Arc::new(|credentials| Box::pin(async move { Ok(credentials) })),
            get_api_key: std::sync::Arc::new(|_| "key".to_string()),
            modify_models: None,
        }
    }

    /// Runs `body` in a fresh scratch directory and restores the process cwd
    /// afterwards. `std::env::set_current_dir` is process-wide, so the same
    /// thread must not run another cwd-sensitive test concurrently.
    async fn with_scratch_dir<F, Fut, T>(_guard: CwdGuard, body: F) -> T
    where
        F: FnOnce(std::path::PathBuf) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let original = std::env::current_dir().unwrap();
        let temp = std::env::temp_dir().join(format!(
            "pi-ai-cli-entry-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        std::env::set_current_dir(&temp).unwrap();
        let result = body(temp.clone()).await;
        std::env::set_current_dir(&original).unwrap();
        let _ = std::fs::remove_dir_all(&temp);
        result
    }

    /// cli.ts:24-26 + 130-133 - `writeFileSync` throws, `main().catch` prints
    /// `Error: <message>` and exits 1, so a failed save must never report
    /// success.
    #[tokio::test]
    async fn failed_auth_save_exits_non_zero_and_reports_the_error() {
        reset_oauth_providers();
        register_oauth_provider(test_provider(|_| {
            Box::pin(async {
                Ok(OAuthCredentials {
                    refresh: "r".to_string(),
                    access: "a".to_string(),
                    expires: 123.0,
                    extra: Map::new(),
                })
            })
        }));

        let directory_instead_of_file = with_scratch_dir(cwd_lock(), |temp| async move {
            // `auth.json` cannot be a file if its path is an existing directory:
            // the write fails the same way a read-only directory would.
            std::fs::create_dir(temp.join(AUTH_FILE)).unwrap();
            login("test-oauth").await
        })
        .await;

        reset_oauth_providers();
        assert_eq!(
            directory_instead_of_file, 1,
            "a failed save_auth must exit non-zero, not report saved credentials"
        );
    }

    /// cli.ts:130-133 - the login rejection surfaces as `Error: <err.message>`.
    #[tokio::test]
    async fn failed_login_reports_the_error_message_not_the_provider_id() {
        reset_oauth_providers();
        register_oauth_provider(test_provider(|_| {
            Box::pin(async { Err("provider said no".to_string()) })
        }));
        assert_eq!(login("test-oauth").await, 1);

        // The message the callback hands back is the one that must be printed,
        // so assert on the exact text the login path forwards to stderr.
        let message = match run_provider_login(&OAuthProviderInterface {
            login: std::sync::Arc::new(|_| {
                Box::pin(async { Err("provider said no".to_string()) })
            }),
            ..test_provider(|_| Box::pin(async { Ok(OAuthCredentials::default()) }))
        })
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("login must reject"),
        };
        assert_eq!(message, "provider said no");
        // cli.ts:131 - the printed line is `Error: provider said no`, and it
        // must not name the provider id instead.
        let printed = login_error_message(&message);
        assert_eq!(printed, "Error: provider said no");
        assert!(!printed.contains("test-oauth"));
        reset_oauth_providers();
    }

    /// cli.ts:106 - `parseInt(choice, 10) - 1`, verified against node v24.
    #[test]
    fn selection_index_port_matches_parse_int() {
        assert_eq!(selection_index("2x"), Some(1));
        assert_eq!(selection_index("1"), Some(0));
        assert_eq!(selection_index("  2  "), Some(1));
        assert_eq!(selection_index("1.9"), Some(0));
        assert_eq!(selection_index("+2"), Some(1));
        assert_eq!(selection_index("0"), Some(-1));
        assert_eq!(selection_index("3"), Some(2));
        assert_eq!(selection_index("-1"), Some(-2));
        // `parseInt("abc")` is NaN, which the cli.ts:107 range check rejects.
        assert_eq!(selection_index("abc"), None);
        assert_eq!(selection_index(""), None);
        assert_eq!(selection_index("0x10"), Some(-1), "radix 10 stops after 0");
        // Rust's `str::parse` rejected "2x"; the prefix parse accepts it.
        assert_ne!("2x".parse::<i64>().ok(), selection_index("2x"));
    }

    #[test]
    fn missing_auth_file_loads_empty() {
        let _guard = cwd_lock();
        let original = std::env::current_dir().unwrap();
        let temp = std::env::temp_dir().join(format!("pi-ai-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        std::env::set_current_dir(&temp).unwrap();
        assert!(load_auth().is_empty());
        assert!(!std::path::Path::new(AUTH_FILE).exists());
        std::env::set_current_dir(original).unwrap();
    }
}
