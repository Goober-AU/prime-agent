//! Port of packages/coding-agent/src/core/extensions/builtin/telegram.ts

use std::sync::Arc;

use crate::core::extensions::types::{
    AutocompleteItem, ExtensionCommandContext, ExtensionApi, ExtensionFactory, ExtensionHandler,
    RegisterCommandOptions,
};
use crate::modes::daemon::daemon_socket::default_daemon_socket_path;
use crate::modes::daemon::daemon_worker_protocol::DAEMON_WORKER_SUPERVISOR_SOCKET_ENV;
use crate::modes::telegram::api::{TelegramApi, TelegramFetcher, TelegramHttpResponse, TELEGRAM_DEFAULT_BASE_URL};
use crate::modes::telegram::manager::{
    describe_telegram, now_ms, start_telegram_worker, stop_telegram_worker, with_telegram_management,
    TelegramWorkerLaunch,
};
use crate::modes::telegram::store::{
    create_pairing, valid_bot_token, TelegramConnectionSettings, TelegramStore,
};

/// `TELEGRAM_SETUP_INSTRUCTIONS`.
pub const TELEGRAM_SETUP_INSTRUCTIONS: &str = "1. Open https://t.me/BotFather in Telegram (the official @BotFather).\n2. Send /newbot, choose a display name, and choose a username ending in bot.\n3. Copy its bot token and paste it into the next Prime input dialog.\n4. Open the pairing link Prime provides and press Start in your bot's private chat.\nThe paired Telegram account can control your Prime sessions and tools. Keep the token and pairing link private.";

/// `sessionBinding(ctx)` - the `Pick<TelegramConnectionSettings, "cwd" |
/// "sessionId" | "sessionFile" | "daemonSocket">` slice.
pub fn session_binding(ctx: &Arc<dyn ExtensionCommandContext>) -> TelegramSessionBinding {
    let session_manager = ctx.session_manager();
    TelegramSessionBinding {
        cwd: ctx.cwd(),
        session_id: session_manager.get_session_id(),
        session_file: session_manager.get_session_file(),
        daemon_socket: std::env::var(DAEMON_WORKER_SUPERVISOR_SOCKET_ENV)
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(default_daemon_socket_path),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TelegramSessionBinding {
    pub cwd: String,
    pub session_id: String,
    pub session_file: Option<String>,
    pub daemon_socket: String,
}

/// The `/telegram` menu labels mapped to their actions.
const MENU_LABELS: [(&str, &str); 7] = [
    ("Setup with BotFather", "setup"),
    ("Connection status", "status"),
    ("Pair account", "pair"),
    ("Connect this session", "here"),
    ("Restart", "restart"),
    ("Pause", "pause"),
    ("Disconnect", "disconnect"),
];

const ACTIONS: [&str; 7] = ["setup", "status", "pair", "here", "restart", "pause", "disconnect"];

/// The `startTelegramWorker(store)` launch facts the TypeScript reads from
/// `config.ts` (`getPackageDir`, `isBunBinary`) and `cli/subprocess-launch.ts`
/// (`createCliSubprocessEnv`) at spawn time.
fn telegram_worker_launch() -> TelegramWorkerLaunch {
    let package_dir = crate::config::get_package_dir();
    let from_source = std::path::Path::new(&package_dir).join("src").exists();
    let entrypoint = std::path::Path::new(&package_dir)
        .join(if from_source { "src" } else { "dist" })
        .join("modes")
        .join("telegram")
        .join(if from_source { "worker.ts" } else { "worker.js" })
        .to_string_lossy()
        .to_string();
    let source: crate::cli::subprocess_launch::ProcessEnv = std::env::vars().collect();
    let exec_argv: Vec<String> = Vec::new();
    let env = crate::cli::subprocess_launch::create_cli_subprocess_env(&source, Some(&entrypoint), &exec_argv);
    TelegramWorkerLaunch {
        is_bun_binary: crate::config::is_bun_binary(),
        from_source,
        package_dir,
        exec_path: crate::cli::subprocess_launch::current_exec_path(),
        exec_argv,
        env: env.into_iter().collect(),
    }
}

/// `fetch` - the default fetcher `TelegramApi` uses.
struct ReqwestTelegramFetcher;

impl TelegramFetcher for ReqwestTelegramFetcher {
    fn fetch(
        &self,
        url: String,
        body: String,
        timeout_ms: u64,
    ) -> pi_ai::types::BoxFuture<Result<TelegramHttpResponse, String>> {
        Box::pin(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(timeout_ms))
                .build()
                .map_err(|error| error.to_string())?;
            let response = client
                .post(&url)
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16() as f64;
            let ok = response.status().is_success();
            let body = response.bytes().await.map_err(|error| error.to_string())?.to_vec();
            Ok(TelegramHttpResponse { ok, status, body })
        })
    }
}

fn notify(ctx: &Arc<dyn ExtensionCommandContext>, message: String, kind: &str) {
    ctx.ui().notify(message, Some(kind.to_string()));
}

fn ui_select(
    ctx: &Arc<dyn ExtensionCommandContext>,
    title: &str,
    options: Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
    ctx.ui().select(title.to_string(), options, None)
}

fn ui_input(
    ctx: &Arc<dyn ExtensionCommandContext>,
    title: &str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
    ctx.ui().input(title.to_string(), None, None)
}

/// `createTelegramExtension(agentDir)`.
pub fn create_telegram_extension(agent_dir: String) -> ExtensionFactory {
    Arc::new(move |pi: Arc<dyn ExtensionApi>| {
        let agent_dir = agent_dir.clone();
        Box::pin(async move {
            create_telegram_extension_impl(pi, agent_dir);
            Ok(())
        })
    })
}

fn create_telegram_extension_impl(pi: Arc<dyn ExtensionApi>, agent_dir: String) {
    let store = Arc::new(TelegramStore::new(&agent_dir));

    let get_argument_completions: Arc<
        dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<AutocompleteItem>>> + Send>>
            + Send
            + Sync,
    > = Arc::new(|prefix: String| {
        Box::pin(async move {
            Some(
                ACTIONS
                    .iter()
                    .filter(|value| value.starts_with(&prefix))
                    .map(|value| AutocompleteItem {
                        value: (*value).to_string(),
                        label: (*value).to_string(),
                        description: None,
                        argument_hint: None,
                        source_tag: None,
                        takes_argument: None,
                    })
                    .collect::<Vec<AutocompleteItem>>(),
            )
        })
    });

    let handler = {
        let store = store.clone();
        Arc::new(
            move |args: String, ctx: Arc<dyn ExtensionCommandContext>| -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<(), String>> + Send>,
            > {
                let store = store.clone();
                Box::pin(async move { run_telegram_command(&store, &ctx, args).await })
            },
        ) as Arc<
            dyn Fn(
                    String,
                    Arc<dyn ExtensionCommandContext>,
                ) -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<(), String>> + Send>,
                > + Send
                + Sync,
        >
    };

    pi.register_command(
        "telegram".to_string(),
        RegisterCommandOptions {
            description: Some(
                "Connect Telegram to Prime, with BotFather setup and private pairing".to_string(),
            ),
            get_argument_completions: Some(get_argument_completions),
            handler: Some(handler),
        },
    );

    let session_start_handler: ExtensionHandler = {
        let store = store.clone();
        Arc::new(move |_event, ctx| {
            let store = store.clone();
            Box::pin(async move {
                let enabled = matches!(store.settings(), Ok(Some(settings)) if settings.enabled);
                if !enabled {
                    return None;
                }
                let store_for_management = store.clone();
                let result = with_telegram_management(&store, move || {
                    let store = store_for_management.clone();
                    Box::pin(async move { start_telegram_worker(&store, &telegram_worker_launch()).await })
                })
                .await;
                if result.is_err() && ctx.has_ui() {
                    ctx.ui().notify(
                        "Telegram is unavailable. Use /telegram status to inspect the connection.".to_string(),
                        Some("warning".to_string()),
                    );
                }
                None
            })
        })
    };
    pi.on("session_start", session_start_handler);
}

/// The `/telegram` command handler body.
async fn run_telegram_command(
    store: &Arc<TelegramStore>,
    ctx: &Arc<dyn ExtensionCommandContext>,
    args: String,
) -> Result<(), String> {
    if !ctx.has_ui() {
        return Err("Run /telegram in the Prime terminal to manage the connection.".to_string());
    }
    let mut action = args.trim().to_string();
    if action.is_empty() {
        let choice = ui_select(
            ctx,
            "Telegram \u{2014} connect to Prime",
            MENU_LABELS
                .iter()
                .map(|(label, _)| (*label).to_string())
                .collect::<Vec<String>>(),
        )
        .await;
        let Some(choice) = choice else {
            return Ok(());
        };
        action = MENU_LABELS
            .iter()
            .find(|(label, _)| *label == choice)
            .map(|(_, action)| (*action).to_string())
            .unwrap_or_default();
    }
    if action.is_empty() {
        return Ok(());
    }
    if action == "status" {
        let described = describe_telegram(store).await?;
        notify(ctx, described, "info");
        return Ok(());
    }
    if !ACTIONS.contains(&action.as_str()) {
        notify(
            ctx,
            "Usage: /telegram [setup|status|pair|here|restart|pause|disconnect]. Paste bot tokens only into the setup dialog.".to_string(),
            "error",
        );
        return Ok(());
    }

    let store_for_management = store.clone();
    let ctx_for_management = ctx.clone();
    with_telegram_management(store, move || {
        let store = store_for_management.clone();
        let ctx = ctx_for_management.clone();
        let action = action.clone();
        Box::pin(async move { run_telegram_action(&store, &ctx, &action).await })
    })
    .await
    .map_err(|error| error.to_string())
}

/// Everything `withTelegramManagement(store, async () => { ... })` runs.
async fn run_telegram_action(
    store: &Arc<TelegramStore>,
    ctx: &Arc<dyn ExtensionCommandContext>,
    action: &str,
) -> Result<(), String> {
    stop_telegram_worker(store).await?;
    let mut prepared: Option<TelegramConnectionSettings> = None;

    if action == "setup" {
        let outcome = async {
            notify(ctx, TELEGRAM_SETUP_INSTRUCTIONS.to_string(), "info");
            let token = ui_input(ctx, "Telegram bot token from @BotFather (not saved to chat history)")
                .await
                .map(|value| value.trim().to_string());
            let Some(token) = token else {
                return Ok(());
            };
            if token.is_empty() {
                return Ok(());
            }
            if !valid_bot_token(&token) {
                notify(
                    ctx,
                    "That does not look like a BotFather token. Run /telegram setup again.".to_string(),
                    "error",
                );
                return Ok(());
            }
            let api = TelegramApi::new(&token, TELEGRAM_DEFAULT_BASE_URL, Arc::new(ReqwestTelegramFetcher))?;
            let bot = api.identify(None).await.map_err(|error| error.to_string())?;
            api.require_polling(None).await.map_err(|error| error.to_string())?;
            let binding = session_binding(ctx);
            prepared = Some(TelegramConnectionSettings {
                version: 1.0,
                enabled: true,
                bot_token: token,
                bot_id: bot.id,
                bot_username: bot.username,
                daemon_socket: binding.daemon_socket,
                cwd: binding.cwd,
                session_id: binding.session_id,
                session_file: binding.session_file,
                paired_user_id: None,
                pairing: None,
            });
            Ok::<(), String>(())
        }
        .await;
        // `finally`: a failed or cancelled setup leaves the worker stopped.
        if prepared.is_none()
            && start_telegram_worker(store, &telegram_worker_launch())
                .await
                .is_err()
        {
            notify(
                ctx,
                "Telegram remains stopped. Use /telegram restart to reconnect.".to_string(),
                "warning",
            );
        }
        outcome?;
    }

    if action == "disconnect" {
        for name in ["connection.json", "state.json", "worker.json", "stop.json"] {
            let _ = std::fs::remove_file(store.path(name));
        }
        notify(
            ctx,
            "Telegram disconnected and its local credentials removed.".to_string(),
            "info",
        );
        return Ok(());
    }

    let mut settings = prepared.or(store.settings()?);
    let Some(settings_ref) = settings.as_mut() else {
        notify(ctx, "Use /telegram setup to connect a bot first.".to_string(), "info");
        return Ok(());
    };

    let mut pairing_link: Option<String> = None;
    if action == "setup" || action == "pair" {
        if settings_ref.paired_user_id.is_some() {
            start_telegram_worker(store, &telegram_worker_launch()).await?;
            notify(
                ctx,
                "An account is already paired. Disconnect and set up again to replace it.".to_string(),
                "info",
            );
            return Ok(());
        }
        let (code, pairing) = create_pairing(now_ms());
        settings_ref.pairing = Some(pairing);
        pairing_link = Some(format!("https://t.me/{}?start={}", settings_ref.bot_username, code));
    }
    if action == "setup" {
        let _ = std::fs::remove_file(store.path("state.json"));
    }
    if action == "here" {
        let binding = session_binding(ctx);
        settings_ref.cwd = binding.cwd;
        settings_ref.session_id = binding.session_id;
        settings_ref.session_file = binding.session_file;
        settings_ref.daemon_socket = binding.daemon_socket;
    }
    settings_ref.enabled = action != "pause";
    let value = serde_json::to_value(&settings_ref).map_err(|error| error.to_string())?;
    store.write("connection.json", &value)?;
    if settings_ref.enabled {
        start_telegram_worker(store, &telegram_worker_launch()).await?;
    }
    let message = match pairing_link {
        Some(link) => format!(
            "Open this one-time link in Telegram and press Start (expires in 10 minutes):\n{link}\nUse /telegram pair for a fresh link."
        ),
        None => describe_telegram(store).await?,
    };
    notify(ctx, message, "info");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::telegram::store::TelegramPairing;

    #[test]
    fn setup_instructions_are_one_newline_joined_block() {
        assert!(TELEGRAM_SETUP_INSTRUCTIONS.starts_with("1. Open https://t.me/BotFather"));
        assert_eq!(TELEGRAM_SETUP_INSTRUCTIONS.lines().count(), 5);
    }

    #[test]
    fn menu_labels_map_to_actions() {
        let mapped = MENU_LABELS
            .iter()
            .find(|(label, _)| *label == "Connect this session")
            .map(|(_, action)| *action);
        assert_eq!(mapped, Some("here"));
        assert_eq!(MENU_LABELS.len(), ACTIONS.len());
        assert!(ACTIONS.contains(&"disconnect"));
    }

    #[test]
    fn the_pairing_link_uses_the_bot_username_and_code() {
        let pairing = TelegramPairing {
            hash: "a".repeat(64),
            expires_at: 1.0,
        };
        let settings = TelegramConnectionSettings {
            version: 1.0,
            enabled: true,
            bot_token: "123456789:abcdefghijklmnopqrst".to_string(),
            bot_id: 1.0,
            bot_username: "prime_bot".to_string(),
            daemon_socket: "pipe".to_string(),
            cwd: "/tmp".to_string(),
            session_id: "id".to_string(),
            session_file: None,
            paired_user_id: None,
            pairing: Some(pairing),
        };
        let link = format!("https://t.me/{}?start={}", settings.bot_username, "code");
        assert_eq!(link, "https://t.me/prime_bot?start=code");
    }
}
