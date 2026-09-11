//! Port of packages/coding-agent/src/modes/daemon/daemon-client-env.ts

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;

use once_cell::sync::Lazy;

use super::daemon_client::protocol::DAEMON_CLIENT_ENV_KEYS;

pub type EnvMap = HashMap<String, String>;

/// Re-filter client-sent env to the allowlist; the socket peer is untrusted.
pub fn filter_client_env(env: Option<&EnvMap>) -> Option<EnvMap> {
    let env = env?;
    let mut filtered: EnvMap = HashMap::new();
    for key in DAEMON_CLIENT_ENV_KEYS {
        if let Some(value) = env.get(key) {
            filtered.insert(key.to_string(), value.clone());
        }
    }
    if filtered.is_empty() {
        None
    } else {
        Some(filtered)
    }
}

// The daemon's own allowlisted env, captured at startup before any env window
// can mutate process.env.
static BASE_CLIENT_ENV: Lazy<HashMap<String, Option<String>>> = Lazy::new(|| {
    let mut base: HashMap<String, Option<String>> = HashMap::new();
    for key in DAEMON_CLIENT_ENV_KEYS {
        base.insert(key.to_string(), std::env::var(key).ok());
    }
    base
});

/// Exec env for a session's subprocesses: pins every allowlisted key to the
/// session's value (unset when the client didn't send it), or to the daemon's
/// startup value for env-less sessions.
pub fn exec_env_for_session(client_env: Option<&EnvMap>) -> HashMap<String, Option<String>> {
    let mut env: HashMap<String, Option<String>> = HashMap::new();
    for key in DAEMON_CLIENT_ENV_KEYS {
        let value = match client_env {
            Some(client_env) => client_env.get(key).cloned(),
            None => BASE_CLIENT_ENV.get(key).cloned().flatten(),
        };
        env.insert(key.to_string(), value);
    }
    env
}

// Shared/exclusive lock: env windows are exclusive (they mutate process.env),
// env-less loads are shared.
static ENV_LOCK: Lazy<tokio::sync::Mutex<()>> = Lazy::new(|| tokio::sync::Mutex::new(()));
// Shared loads must not observe an env window's mutation; this counter keeps the
// exclusive path waiting for the shared loads that were already running.
static ACTIVE_SHARED: Lazy<Mutex<usize>> = Lazy::new(|| Mutex::new(0));
static SHARED_IDLE: Lazy<tokio::sync::Notify> = Lazy::new(tokio::sync::Notify::new);

async fn wait_for_shared_loads_to_finish() {
    loop {
        if *ACTIVE_SHARED.lock().expect("client env shared counter poisoned") == 0 {
            return;
        }
        SHARED_IDLE.notified().await;
    }
}

/// Run `fn_` with the client's env applied to the process environment, restoring afterwards.
pub async fn with_client_env<T, F, Fut>(env: Option<&EnvMap>, fn_: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let Some(env) = env else {
        // Shared load: it must not start inside an env window, and it must not
        // capture another session's identity.
        let guard = ENV_LOCK.lock().await;
        drop(guard);
        {
            let mut active = ACTIVE_SHARED.lock().expect("client env shared counter poisoned");
            *active += 1;
        }
        let result = fn_().await;
        {
            let mut active = ACTIVE_SHARED.lock().expect("client env shared counter poisoned");
            *active -= 1;
        }
        SHARED_IDLE.notify_waiters();
        return result;
    };

    let guard = ENV_LOCK.lock().await;
    wait_for_shared_loads_to_finish().await;

    let mut previous: Vec<(String, Option<String>)> = Vec::new();
    // Pin the full allowlist (unsetting keys the client didn't send) so a
    // partially-forwarded env can't mix with the daemon's ambient values.
    for key in DAEMON_CLIENT_ENV_KEYS {
        previous.push((key.to_string(), std::env::var(key).ok()));
        match env.get(key) {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    let result = fn_().await;
    for (key, value) in previous {
        match value {
            Some(value) => std::env::set_var(&key, value),
            None => std::env::remove_var(&key),
        }
    }
    drop(guard);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_to_the_allowlist() {
        let mut env = EnvMap::new();
        env.insert("HERDR_PANE_ID".to_string(), "pane-1".to_string());
        env.insert("SOMETHING_ELSE".to_string(), "nope".to_string());
        let filtered = filter_client_env(Some(&env)).expect("allowlisted key survives");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered.get("HERDR_PANE_ID").map(String::as_str), Some("pane-1"));
        assert!(filter_client_env(None).is_none());
        assert!(filter_client_env(Some(&EnvMap::new())).is_none());
    }

    #[test]
    fn exec_env_pins_every_allowlisted_key() {
        let env = exec_env_for_session(None);
        assert_eq!(env.len(), DAEMON_CLIENT_ENV_KEYS.len());
    }

    #[tokio::test]
    async fn client_env_window_is_restored() {
        let mut env = EnvMap::new();
        env.insert("HERDR_PANE_ID".to_string(), "pane-window".to_string());
        let seen = with_client_env(Some(&env), || async { std::env::var("HERDR_PANE_ID").ok() }).await;
        assert_eq!(seen.as_deref(), Some("pane-window"));
        assert!(std::env::var("HERDR_PANE_ID").is_err());
    }
}
