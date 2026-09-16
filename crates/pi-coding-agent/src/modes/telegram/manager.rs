//! Port of packages/coding-agent/src/modes/telegram/manager.ts

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::modes::telegram::store::{is_record, TelegramStore};
use crate::utils::child_process::{spawn_hidden, SpawnOptions};
use crate::utils::dir_lock::{file_identity, open_lock, stat_identity, StatIdentity};

/// `TELEGRAM_WORKER_LOCK_OPTIONS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelegramLockOptions {
    pub realpath: bool,
    pub stale: u64,
    pub update: u64,
}

/// `{ realpath: false, stale: 10_000, update: 2000 }`.
pub const TELEGRAM_WORKER_LOCK_OPTIONS: TelegramLockOptions = TelegramLockOptions {
    realpath: false,
    stale: 10_000,
    update: 2000,
};

/// `withTelegramManagement` retry policy: `{ retries: 50, minTimeout: 100, maxTimeout: 100, factor: 1 }`.
pub const TELEGRAM_MANAGEMENT_RETRIES: u32 = 50;
pub const TELEGRAM_MANAGEMENT_RETRY_MS: u64 = 100;

/// One `proper-lockfile` lock, held through a `.lock` directory next to the target.
///
/// blocked_on: `proper-lockfile` is a Node dependency; the port implements the
/// same protocol locally (lock directory, stale takeover, mtime heartbeat).
#[derive(Debug)]
pub struct TelegramFileLock {
    lock_path: String,
    identity: StatIdentity,
    _pinned: Arc<std::fs::File>,
    /// `locks[file]`: the in-process registry entry. `None` means the lock was
    /// released or compromised, and a later `release()` must not touch the path
    /// (`unlock()` fails with `ENOTACQUIRED` when the registry entry is gone).
    owned: Arc<std::sync::atomic::AtomicBool>,
    /// `lock.mtime`: the mtime this holder last wrote, compared on every heartbeat.
    mtime: Arc<std::sync::Mutex<Option<SystemTime>>>,
    heartbeat: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl TelegramFileLock {
    /// `lockfile.lock(path, options)`.
    pub async fn acquire(
        target: &str,
        options: TelegramLockOptions,
        retries: Option<u32>,
        on_compromised: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<Self, TelegramLockError> {
        let lock_path = format!("{target}.lock");
        let mut attempts = 0u32;
        loop {
            match std::fs::create_dir(&lock_path) {
                Ok(()) => {
                    let pinned = Arc::new(open_lock(Path::new(&lock_path))
                        .map_err(|error| TelegramLockError::Io(error.to_string()))?);
                    let identity = file_identity(&pinned)
                        .ok_or_else(|| TelegramLockError::Io("Lock identity unavailable".into()))?;
                    let owned = Arc::new(std::sync::atomic::AtomicBool::new(true));
                    let mtime = Arc::new(std::sync::Mutex::new(lock_mtime(&lock_path).ok()));
                    // `update: 2000` with the default `onCompromised`: the heartbeat
                    // always runs, with or without a caller callback. A callback is the
                    // only thing that replaces the default (which throws).
                    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
                    let heartbeat = {
                        let lock_path = lock_path.clone();
                        let flag = alive.clone();
                        let owned = owned.clone();
                        let mtime = mtime.clone();
                        let on_compromised = on_compromised.clone();
                        let pinned = pinned.clone();
                        tokio::spawn(async move {
                            let _pinned = pinned;
                            loop {
                                tokio::time::sleep(std::time::Duration::from_millis(options.update)).await;
                                let mut stamp = mtime.lock().unwrap();
                                if !flag.load(std::sync::atomic::Ordering::SeqCst) {
                                    return;
                                }
                                if !owned.load(std::sync::atomic::Ordering::SeqCst) {
                                    return;
                                }
                                // Windows can reuse a removed directory's timestamp.
                                if stat_identity(Path::new(&lock_path)) != Some(identity) {
                                    drop(stamp);
                                    compromise(&owned, &flag, on_compromised.as_deref());
                                    return;
                                }
                                // `updateLock`: ENOENT, or an mtime that is no longer
                                // ours, means the lock was reclaimed.
                                match lock_mtime(&lock_path) {
                                    Ok(current) => {
                                        let ours = *stamp;
                                        if ours.is_some_and(|stamp| stamp != current) {
                                            drop(stamp);
                                            compromise(&owned, &flag, on_compromised.as_deref());
                                            return;
                                        }
                                    }
                                    Err(_) => {
                                        drop(stamp);
                                        compromise(&owned, &flag, on_compromised.as_deref());
                                        return;
                                    }
                                }
                                match touch(&lock_path) {
                                    Ok(()) => {
                                        *stamp = lock_mtime(&lock_path).ok();
                                    }
                                    Err(_) => {
                                        drop(stamp);
                                        compromise(&owned, &flag, on_compromised.as_deref());
                                        return;
                                    }
                                }
                            }
                        });
                        alive
                    };
                    return Ok(Self {
                        lock_path,
                        identity,
                        _pinned: pinned,
                        owned,
                        mtime,
                        heartbeat: Some(heartbeat),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale(&lock_path, options.stale) {
                        let _ = std::fs::remove_dir(&lock_path);
                        continue;
                    }
                    attempts += 1;
                    if let Some(retries) = retries {
                        if attempts > retries {
                            return Err(TelegramLockError::Elocked);
                        }
                    } else {
                        return Err(TelegramLockError::Elocked);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(TELEGRAM_MANAGEMENT_RETRY_MS)).await;
                }
                Err(error) => return Err(TelegramLockError::Io(error.to_string())),
            }
        }
    }

    /// The holder detected a compromise: the directory is gone or its mtime moved.
    ///
    /// Returns `true` when a callback was registered, mirroring `onCompromised`.
    pub fn was_compromised(&self) -> bool {
        !self.owned.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// `release()`.
    ///
    /// `unlock()` refuses to remove the path when the in-process registry entry is
    /// gone (`ENOTACQUIRED`), so a compromised holder leaves the new owner's lock
    /// directory in place.
    pub fn release(&self) {
        let stamp = self.mtime.lock().unwrap();
        if let Some(heartbeat) = &self.heartbeat {
            heartbeat.store(false, std::sync::atomic::Ordering::SeqCst);
        }
        // Compromised or already released: `unlock()` fails with `ENOTACQUIRED`.
        if self.owned.swap(false, std::sync::atomic::Ordering::SeqCst) == false {
            return;
        }
        if stat_identity(Path::new(&self.lock_path)) != Some(self.identity)
            || lock_mtime(&self.lock_path).ok() != *stamp
        {
            return;
        }
        let _ = std::fs::remove_dir(&self.lock_path);
    }
}

impl Drop for TelegramFileLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// `setLockAsCompromised`: drop the registry entry, stop the heartbeat, notify.
fn compromise(
    owned: &Arc<std::sync::atomic::AtomicBool>,
    alive: &Arc<std::sync::atomic::AtomicBool>,
    on_compromised: Option<&(dyn Fn() + Send + Sync)>,
) {
    owned.store(false, std::sync::atomic::Ordering::SeqCst);
    alive.store(false, std::sync::atomic::Ordering::SeqCst);
    if let Some(on_compromised) = on_compromised {
        on_compromised();
    }
}

fn lock_mtime(path: &str) -> std::io::Result<SystemTime> {
    std::fs::metadata(path)?.modified()
}

/// `lockfile.check(path, options)`.
pub fn telegram_lock_held(target: &str, options: TelegramLockOptions) -> bool {
    let lock_path = format!("{target}.lock");
    if !Path::new(&lock_path).exists() {
        return false;
    }
    if is_stale(&lock_path, options.stale) {
        let _ = std::fs::remove_dir(&lock_path);
        return false;
    }
    true
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TelegramLockError {
    /// `ELOCKED`: another holder owns the lock.
    #[error("lock is already held")]
    Elocked,
    #[error("{0}")]
    Io(String),
}

fn touch(path: &str) -> std::io::Result<()> {
    // `update: 2000` refreshes the lock mtime so a live holder is never stale.
    let directory = std::fs::read_dir(path)?;
    drop(directory);
    let stamp = SystemTime::now();
    filetime_now(path, stamp)
}

fn filetime_now(path: &str, stamp: SystemTime) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES};
        options.access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    #[cfg(not(windows))]
    options.read(true);
    options.open(path)?.set_times(std::fs::FileTimes::new().set_modified(stamp))
}

fn is_stale(lock_path: &str, stale_ms: u64) -> bool {
    let Ok(metadata) = std::fs::metadata(lock_path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(elapsed) = SystemTime::now().duration_since(modified) else {
        return false;
    };
    elapsed.as_millis() as u64 > stale_ms
}

/// `telegramWorkerRunning(store)`.
pub async fn telegram_worker_running(store: &TelegramStore) -> bool {
    telegram_lock_held(&store.path("worker"), TELEGRAM_WORKER_LOCK_OPTIONS)
}

/// `withTelegramManagement(store, action)`.
pub async fn with_telegram_management<T, F>(store: &TelegramStore, action: F) -> Result<T, TelegramLockError>
where
    F: FnOnce() -> pi_ai::types::BoxFuture<Result<T, String>>,
{
    std::fs::create_dir_all(&store.directory).map_err(|error| TelegramLockError::Io(error.to_string()))?;
    let lock = TelegramFileLock::acquire(
        &store.path("management"),
        TELEGRAM_WORKER_LOCK_OPTIONS,
        Some(TELEGRAM_MANAGEMENT_RETRIES),
        None,
    )
    .await?;
    let result = action().await;
    lock.release();
    result.map_err(TelegramLockError::Io)
}

/// `stopTelegramWorker(store)`.
pub async fn stop_telegram_worker(store: &TelegramStore) -> Result<(), String> {
    if !telegram_worker_running(store).await {
        return Ok(());
    }
    let status = store.status()?.ok_or_else(|| "Telegram is starting. Retry in a few seconds.".to_string())?;
    store.write("stop.json", &serde_json::json!({ "instanceId": status.instance_id }))?;
    for _ in 0..150 {
        if !telegram_worker_running(store).await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err("Telegram has not stopped yet. Retry /telegram pause in a few seconds.".to_string())
}

/// The launch facts `startTelegramWorker` reads from the installation.
///
/// blocked_on: `config.ts` (`getPackageDir`, `isBunBinary`) and
/// `cli/subprocess-launch.ts` (`createCliSubprocessEnv`) belong to other slices.
pub struct TelegramWorkerLaunch {
    pub is_bun_binary: bool,
    /// `import.meta.url.endsWith(".ts")`.
    pub from_source: bool,
    pub package_dir: String,
    pub exec_path: String,
    pub exec_argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// `startTelegramWorker(store, launch)`.
pub async fn start_telegram_worker(store: &TelegramStore, launch: &TelegramWorkerLaunch) -> Result<(), String> {
    let Some(settings) = store.settings()? else {
        return Ok(());
    };
    if !settings.enabled {
        return Ok(());
    }
    if telegram_worker_running(store).await {
        return Ok(());
    }
    let env: Vec<(String, String)> = launch
        .env
        .iter()
        .filter(|(name, _)| {
            !name.starts_with("PRIME_AGENT_INTERNAL_")
                && !name.starts_with("PRIME_AGENT_SESSION_LEASE")
                && name != "PRIME_AGENT_ORPHAN_PROCESS_JOURNAL"
                && name != "PRIME_AGENT_INTERACTIVE_SELF_UPDATE"
        })
        .cloned()
        .collect();
    let _ = std::fs::remove_file(store.path("stop.json"));

    let args = native_worker_args(&store.agent_dir);
    let mut handle = spawn_hidden(
        &launch.exec_path,
        &args,
        SpawnOptions {
            cwd: Some(settings.cwd.clone()),
            env: Some(env),
            // `createCliSubprocessEnv()` builds the worker's whole environment; the
            // prohibited `PRIME_AGENT_INTERNAL_*` names must be absent, not merely
            // out-shadowed by the additive `envs()` merge.
            replace_env: true,
            detached: true,
            shell: false,
            capture_stdout: false,
            capture_stderr: false,
            stdin_piped: false,
        },
    )
    .map_err(|_| "Telegram worker could not start. Check /telegram status.".to_string())?;
    let child_pid = handle.child.id().map(|pid| pid as f64);

    for _ in 0..150 {
        if let Some(status) = store.status()? {
            if Some(status.pid) == child_pid && status.phase == "error" {
                return Err(status
                    .error
                    .unwrap_or_else(|| "Telegram could not start.".to_string()));
            }
        }
        if telegram_worker_running(store).await {
            return Ok(());
        }
        if let Ok(Some(exit)) = handle.child.try_wait() {
            if exit.code() != Some(0) {
                return Err("Telegram worker could not start. Check /telegram status.".to_string());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err("Telegram worker startup timed out. Check /telegram status.".to_string())
}

fn native_worker_args(agent_dir: &str) -> Vec<String> {
    vec!["--internal-telegram-worker".into(), agent_dir.into()]
}

/// `telegramStopRequested(store, instanceId)`.
pub fn telegram_stop_requested(store: &TelegramStore, instance_id: &str) -> Result<bool, String> {
    let request: Option<Value> = store.read("stop.json")?;
    Ok(match request {
        Some(request) => is_record(&request)
            && request.get("instanceId").and_then(Value::as_str) == Some(instance_id),
        None => false,
    })
}

/// `describeTelegram(store)`.
pub async fn describe_telegram(store: &TelegramStore) -> Result<String, String> {
    let Some(settings) = store.settings()? else {
        return Ok("Telegram is not connected. Use /telegram setup.".to_string());
    };
    let status = store.status()?;
    let running = telegram_worker_running(store).await;
    let mut lines = vec![
        format!("Bot: @{}", settings.bot_username),
        format!(
            "Connection: {}",
            if running {
                status
                    .as_ref()
                    .map(|status| status.phase.clone())
                    .unwrap_or_else(|| "starting".to_string())
            } else {
                "stopped".to_string()
            }
        ),
        format!("Enabled: {}", if settings.enabled { "yes" } else { "no" }),
        format!(
            "Paired account: {}",
            settings
                .paired_user_id
                .map(|id| format_telegram_id(id))
                .unwrap_or_else(|| "not paired".to_string())
        ),
        format!("Session: {}", settings.session_id),
        format!("Directory: {}", settings.cwd),
    ];
    if let Some(status) = &status {
        if let Some(error) = &status.error {
            lines.push(format!("Last error: {error}"));
        }
    }
    lines.push(
        "Use /telegram here to connect this session, /telegram pause to stop, or /telegram disconnect to remove the connection."
            .to_string(),
    );
    Ok(lines.join("\n"))
}

/// JavaScript number formatting for an id (`${value}`).
fn format_telegram_id(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// Milliseconds since the epoch, matching `Date.now()`.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_launch_uses_native_entry_not_a_typescript_file() {
        assert_eq!(native_worker_args("C:/separate profile/agent"),
            ["--internal-telegram-worker", "C:/separate profile/agent"]);
    }

    #[test]
    fn heartbeat_really_refreshes_the_lock_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        filetime_now(path, UNIX_EPOCH + std::time::Duration::from_secs(100)).unwrap();
        assert!(is_stale(path, TELEGRAM_WORKER_LOCK_OPTIONS.stale));
        touch(path).unwrap();
        assert!(!is_stale(path, TELEGRAM_WORKER_LOCK_OPTIONS.stale));
        assert!(touch(dir.path().join("missing").to_str().unwrap()).is_err());
    }

    #[test]
    fn lock_options_match_the_typescript() {
        assert!(!TELEGRAM_WORKER_LOCK_OPTIONS.realpath);
        assert_eq!(TELEGRAM_WORKER_LOCK_OPTIONS.stale, 10_000);
        assert_eq!(TELEGRAM_WORKER_LOCK_OPTIONS.update, 2000);
    }

    #[tokio::test]
    async fn a_second_holder_is_rejected_and_release_frees_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("worker").to_string_lossy().to_string();
        let first = TelegramFileLock::acquire(&target, TELEGRAM_WORKER_LOCK_OPTIONS, None, None)
            .await
            .unwrap();
        assert!(telegram_lock_held(&target, TELEGRAM_WORKER_LOCK_OPTIONS));
        assert_eq!(
            TelegramFileLock::acquire(&target, TELEGRAM_WORKER_LOCK_OPTIONS, None, None)
                .await
                .unwrap_err(),
            TelegramLockError::Elocked
        );
        first.release();
        assert!(!telegram_lock_held(&target, TELEGRAM_WORKER_LOCK_OPTIONS));
    }

    #[tokio::test]
    async fn replacement_with_identical_mtime_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("management").to_string_lossy().into_owned();
        let path = format!("{target}.lock");
        let options = TelegramLockOptions { update: 10, ..TELEGRAM_WORKER_LOCK_OPTIONS };
        let lock = TelegramFileLock::acquire(&target, options, None, None).await.unwrap();
        let original = lock_mtime(&path).unwrap();
        std::fs::remove_dir(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        filetime_now(&path, original).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !lock.was_compromised() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }).await.expect("replacement identity must be detected even with equal timestamps");
        lock.release();
        assert!(Path::new(&path).exists());
    }

    #[tokio::test]
    async fn release_preserves_replacement_before_first_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("management").to_string_lossy().into_owned();
        let path = format!("{target}.lock");
        let lock = TelegramFileLock::acquire(&target, TELEGRAM_WORKER_LOCK_OPTIONS, None, None).await.unwrap();
        let original = lock_mtime(&path).unwrap();
        std::fs::remove_dir(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        filetime_now(&path, original).unwrap();
        lock.release();
        assert!(Path::new(&path).exists());
    }

    #[tokio::test]
    async fn dropped_management_lock_stops_renewal_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("management").to_string_lossy().into_owned();
        let lock = TelegramFileLock::acquire(&target, TELEGRAM_WORKER_LOCK_OPTIONS, None, None).await.unwrap();
        drop(lock);
        assert!(!Path::new(&format!("{target}.lock")).exists());
    }

    #[test]
    fn stop_requests_are_matched_by_instance_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = TelegramStore::new(dir.path().to_str().unwrap());
        assert!(!telegram_stop_requested(&store, "a").unwrap());
        store
            .write("stop.json", &serde_json::json!({"instanceId": "a"}))
            .unwrap();
        assert!(telegram_stop_requested(&store, "a").unwrap());
        assert!(!telegram_stop_requested(&store, "b").unwrap());
    }

    #[tokio::test]
    async fn describe_without_settings_points_at_setup() {
        let dir = tempfile::tempdir().unwrap();
        let store = TelegramStore::new(dir.path().to_str().unwrap());
        assert_eq!(
            describe_telegram(&store).await.unwrap(),
            "Telegram is not connected. Use /telegram setup."
        );
    }

    #[tokio::test]
    async fn describe_reports_the_connection_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = TelegramStore::new(dir.path().to_str().unwrap());
        store
            .write(
                "connection.json",
                &serde_json::json!({
                    "version": 1,
                    "enabled": true,
                    "botToken": "123456789:AAAAAAAAAAAAAAAAAAAA",
                    "botId": 5,
                    "botUsername": "prime_bot",
                    "daemonSocket": "/tmp/socket",
                    "cwd": "/work",
                    "sessionId": "s1",
                }),
            )
            .unwrap();
        let description = describe_telegram(&store).await.unwrap();
        assert!(description.starts_with("Bot: @prime_bot\nConnection: stopped\nEnabled: yes\nPaired account: not paired\nSession: s1\nDirectory: /work\n"));
        assert!(description.ends_with("Use /telegram here to connect this session, /telegram pause to stop, or /telegram disconnect to remove the connection."));
    }

    #[tokio::test]
    async fn stopping_without_a_worker_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let store = TelegramStore::new(dir.path().to_str().unwrap());
        assert!(stop_telegram_worker(&store).await.is_ok());
    }

    #[test]
    fn ids_print_like_javascript_numbers() {
        assert_eq!(format_telegram_id(5.0), "5");
        assert_eq!(format_telegram_id(5.5), "5.5");
    }
}
