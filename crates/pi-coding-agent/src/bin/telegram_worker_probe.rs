
//! Validation-only launcher for the Telegram native-worker environment check (H-06).
//!
//! This binary exists so the T12 fixture can observe the environment of the real
//! native worker child. It is NOT part of the shipped application: nothing in
//! `src/` references it, and it is only used by
//! `tests/telegram_native_worker_env.rs`.
//!
//! Behaviour (approved by the coordinator, 2026-09-16):
//!   1. Dump this process environment to `$PARITY_TELEGRAM_ENV_DUMP` (token-like
//!      values redacted) plus its own pid.
//!   2. Optionally neutralise the private store connection (`enabled: false`) so
//!      the worker never contacts a live Telegram service.
//!   3. Continue into the real native worker entry
//!      (`worker::telegram_worker_main`) with the same argv the production
//!      `--internal-telegram-worker` dispatch uses.

use serde_json::{json, Map, Value};

fn redact(name: &str, value: &str) -> String {
    let upper = name.to_ascii_uppercase();
    let secret = ["TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "APIKEY", "API_KEY", "AUTHORIZATION"]
        .iter()
        .any(|needle| upper.contains(needle));
    if secret {
        "[redacted]".to_string()
    } else {
        value.to_string()
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dump_path = std::env::var("PARITY_TELEGRAM_ENV_DUMP")
        .expect("PARITY_TELEGRAM_ENV_DUMP must point at the private state root");
    let mut map = Map::new();
    for (name, value) in std::env::vars() {
        map.insert(name.clone(), Value::String(redact(&name, &value)));
    }
    map.insert("__probe_pid".to_string(), json!(std::process::id()));
    map.insert(
        "__probe_argv".to_string(),
        Value::String(args.join(" ")),
    );
    let body = serde_json::to_string_pretty(&Value::Object(map)).unwrap_or_default();
    if let Err(error) = std::fs::write(&dump_path, body) {
        eprintln!("probe: cannot write environment dump: {error}");
        std::process::exit(2);
    }

    if std::env::var("PARITY_TELEGRAM_DISABLE_SETTINGS").as_deref() == Ok("1") {
        if let Some(agent_dir) = args.get(2) {
            let store = pi_coding_agent::modes::telegram::store::TelegramStore::new(agent_dir);
            if let Ok(Some(mut settings)) = store.settings() {
                settings.enabled = false;
                if let Ok(value) = serde_json::to_value(&settings) {
                    let _ = store.write("connection.json", &value);
                }
            }
        }
    }

    // Continue into the real native worker entry, exactly as
    // `bin/optimus_rust.rs` does for `--internal-telegram-worker`.
    let result = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| error.to_string())
        .and_then(|runtime| {
            runtime.block_on(pi_coding_agent::modes::telegram::worker::telegram_worker_main(&args))
        });
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
