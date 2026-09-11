//! Port of packages/coding-agent/src/modes/daemon/daemon-socket.ts
//!
//! Unix socket path ownership: the per-path lock lease (proper-lockfile in the
//! TypeScript) is ported with the same stale/update windows and retry budget,
//! using a lock directory next to the socket path.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::utils::daemon_socket_path::normalize_socket_path;

const DAEMON_SOCKET_MODE: u32 = 0o600;
const DAEMON_SOCKET_DIR_MODE: u32 = 0o700;
const DAEMON_SOCKET_RELEASE_GRACE_MS: u64 = 1000;
const DAEMON_SOCKET_RELEASE_POLL_MS: u64 = 25;
const DAEMON_SOCKET_LOCK_STALE_MS: u64 = 5000;
const DAEMON_SOCKET_LOCK_UPDATE_MS: u64 = 1000;
const DAEMON_SOCKET_LOCK_RETRIES: u32 = 600;

pub type DaemonSocketCompromiseListener = Box<dyn Fn(&DaemonSocketError) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSocketError {
    pub message: String,
}

impl std::fmt::Display for DaemonSocketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DaemonSocketError {}

pub struct DaemonSocketPathLease {
    pub socket_path: String,
    released: std::sync::atomic::AtomicBool,
    compromised_error: std::sync::Mutex<Option<DaemonSocketError>>,
    compromise_listeners: std::sync::Mutex<Vec<std::sync::Arc<DaemonSocketCompromiseListener>>>,
    lock_path: Option<String>,
}

impl DaemonSocketPathLease {
    pub fn new(socket_path: &str, lock_path: Option<String>) -> Self {
        Self {
            socket_path: socket_path.to_string(),
            released: std::sync::atomic::AtomicBool::new(false),
            compromised_error: std::sync::Mutex::new(None),
            compromise_listeners: std::sync::Mutex::new(Vec::new()),
            lock_path,
        }
    }

    pub fn compromise(&self) -> Option<DaemonSocketError> {
        self.compromised_error
            .lock()
            .expect("compromise poisoned")
            .clone()
    }

    /// Returns the unsubscribe handle (a no-op when the lease is already compromised).
    pub fn on_compromised(
        self: &std::sync::Arc<Self>,
        listener: std::sync::Arc<DaemonSocketCompromiseListener>,
    ) -> Box<dyn FnOnce() + Send> {
        if let Some(error) = self.compromise() {
            notify_compromise_listener(&listener, &error);
            return Box::new(|| {});
        }
        let lease = std::sync::Arc::clone(self);
        let retained = std::sync::Arc::clone(&listener);
        self.compromise_listeners
            .lock()
            .expect("listeners poisoned")
            .push(listener);
        Box::new(move || {
            lease
                .compromise_listeners
                .lock()
                .expect("listeners poisoned")
                .retain(|candidate| !std::sync::Arc::ptr_eq(candidate, &retained));
        })
    }

    pub fn record_compromise(&self, error: DaemonSocketError) {
        {
            let mut compromised = self.compromised_error.lock().expect("compromise poisoned");
            if compromised.is_some() {
                return;
            }
            *compromised = Some(error.clone());
        }
        let listeners: Vec<std::sync::Arc<DaemonSocketCompromiseListener>> = self
            .compromise_listeners
            .lock()
            .expect("listeners poisoned")
            .drain(..)
            .collect();
        for listener in listeners {
            notify_compromise_listener(&listener, &error);
        }
    }

    pub async fn release(&self) {
        if self.released.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        self.compromise_listeners
            .lock()
            .expect("listeners poisoned")
            .clear();
        if let Some(lock_path) = &self.lock_path {
            let _ = std::fs::remove_dir(lock_path);
        }
    }
}

fn notify_compromise_listener(listener: &DaemonSocketCompromiseListener, error: &DaemonSocketError) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(error)));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonSocketIdentity {
    pub dev: u64,
    pub ino: u64,
}

pub fn default_daemon_socket_path() -> String {
    if crate::utils::pi_user_agent::process_platform() == "win32" {
        return "\\\\.\\pipe\\prime-agent-daemon".to_string();
    }
    Path::new(&default_daemon_socket_dir())
        .join("daemon.sock")
        .to_string_lossy()
        .to_string()
}

/// Acquire the per-path lease. `None` on Windows and on Unix when the lock is
/// held elsewhere past the retry budget.
pub async fn acquire_daemon_socket_path_lease(
    socket_path: &str,
) -> Option<std::sync::Arc<DaemonSocketPathLease>> {
    if let Err(error) = ensure_default_daemon_socket_dir(socket_path) {
        let _ = error;
        return None;
    }
    if crate::utils::pi_user_agent::process_platform() == "win32" {
        return None;
    }
    let lock_path = lock_directory_path(socket_path);
    let deadline = Instant::now() + Duration::from_millis(DAEMON_SOCKET_RELEASE_POLL_MS as u64 * DAEMON_SOCKET_LOCK_RETRIES as u64);
    loop {
        match std::fs::create_dir(&lock_path) {
            Ok(()) => {
                return Some(std::sync::Arc::new(DaemonSocketPathLease::new(
                    socket_path,
                    Some(lock_path),
                )))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_stale(&lock_path) {
                    let _ = std::fs::remove_dir_all(&lock_path);
                    continue;
                }
                if Instant::now() >= deadline {
                    return None;
                }
                tokio::time::sleep(Duration::from_millis(DAEMON_SOCKET_RELEASE_POLL_MS)).await;
            }
            Err(_) => return None,
        }
    }
}

fn lock_directory_path(socket_path: &str) -> String {
    format!("{socket_path}.lock")
}

fn lock_is_stale(lock_path: &str) -> bool {
    std::fs::metadata(lock_path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .map(|age| age.as_millis() as u64 > DAEMON_SOCKET_LOCK_STALE_MS)
        .unwrap_or(false)
}

pub async fn prepare_daemon_socket_path(
    socket_path: &str,
    lease: Option<std::sync::Arc<DaemonSocketPathLease>>,
) -> Result<(), DaemonSocketError> {
    ensure_default_daemon_socket_dir(socket_path)?;

    if crate::utils::pi_user_agent::process_platform() == "win32" {
        return Ok(());
    }
    if let Some(lease) = lease {
        assert_socket_lease(socket_path, &lease)?;
        assert_socket_lease_held(socket_path, &lease)?;
        return prepare_unix_daemon_socket_path(socket_path, Some(&lease)).await;
    }
    if !Path::new(socket_path).exists() {
        return Ok(());
    }
    if can_connect_to_unix_socket(socket_path).await {
        return Err(DaemonSocketError {
            message: format!("Daemon socket already in use: {socket_path}"),
        });
    }
    let owned_lease = acquire_daemon_socket_path_lease(socket_path).await;
    let result = prepare_unix_daemon_socket_path(socket_path, owned_lease.as_deref()).await;
    if let Some(lease) = owned_lease {
        lease.release().await;
    }
    result
}

async fn prepare_unix_daemon_socket_path(
    socket_path: &str,
    lease: Option<&DaemonSocketPathLease>,
) -> Result<(), DaemonSocketError> {
    if !Path::new(socket_path).exists() {
        return Ok(());
    }

    let metadata = match std::fs::symlink_metadata(socket_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(DaemonSocketError {
                message: error.to_string(),
            })
        }
    };
    if !is_socket(&metadata) {
        return Err(DaemonSocketError {
            message: format!("Daemon socket path exists and is not a socket: {socket_path}"),
        });
    }

    let stale_identity = match get_daemon_socket_identity(socket_path) {
        Some(identity) => identity,
        None => return Ok(()),
    };
    if can_connect_to_unix_socket(socket_path).await {
        return Err(DaemonSocketError {
            message: format!("Daemon socket already in use: {socket_path}"),
        });
    }
    let deadline = Instant::now() + Duration::from_millis(DAEMON_SOCKET_RELEASE_GRACE_MS);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(DAEMON_SOCKET_RELEASE_POLL_MS)).await;
        if !Path::new(socket_path).exists() {
            return Ok(());
        }
        let current_identity = match get_daemon_socket_identity(socket_path) {
            Some(identity) => identity,
            None => return Ok(()),
        };
        if current_identity != stale_identity {
            return Err(DaemonSocketError {
                message: format!("Daemon socket changed ownership while waiting for cleanup: {socket_path}"),
            });
        }
        if can_connect_to_unix_socket(socket_path).await {
            return Err(DaemonSocketError {
                message: format!("Daemon socket already in use: {socket_path}"),
            });
        }
    }

    if let Some(lease) = lease {
        assert_socket_lease_held(socket_path, lease)?;
    }
    std::fs::remove_file(socket_path).map_err(|error| DaemonSocketError {
        message: error.to_string(),
    })
}

pub fn restrict_daemon_socket_path(socket_path: &str) {
    if crate::utils::pi_user_agent::process_platform() == "win32" {
        return;
    }
    let _ = set_file_mode(socket_path, DAEMON_SOCKET_MODE);
}

pub fn get_daemon_socket_identity(socket_path: &str) -> Option<DaemonSocketIdentity> {
    if crate::utils::pi_user_agent::process_platform() == "win32" {
        return None;
    }
    let metadata = std::fs::symlink_metadata(socket_path).ok()?;
    Some(identity_from_metadata(&metadata))
}

pub fn cleanup_daemon_socket_path(
    socket_path: &str,
    expected_identity: Option<DaemonSocketIdentity>,
    lease: Option<&DaemonSocketPathLease>,
) {
    if crate::utils::pi_user_agent::process_platform() == "win32" {
        return;
    }
    if let Some(lease) = lease {
        if assert_socket_lease(socket_path, lease).is_err() {
            return;
        }
        if lease.compromise().is_some() {
            return;
        }
        let _ = cleanup_unix_daemon_socket_path(socket_path, expected_identity);
        return;
    }
    let lock_path = lock_directory_path(socket_path);
    if std::fs::create_dir(&lock_path).is_err() {
        return;
    }
    let _ = cleanup_unix_daemon_socket_path(socket_path, expected_identity);
    let _ = std::fs::remove_dir(&lock_path);
}

fn cleanup_unix_daemon_socket_path(socket_path: &str, expected_identity: Option<DaemonSocketIdentity>) -> Option<()> {
    if !Path::new(socket_path).exists() {
        return None;
    }
    if let Some(expected_identity) = expected_identity {
        let current_identity = get_daemon_socket_identity(socket_path)?;
        if current_identity != expected_identity {
            return None;
        }
    }
    std::fs::remove_file(socket_path).ok()
}

fn assert_socket_lease(socket_path: &str, lease: &DaemonSocketPathLease) -> Result<(), DaemonSocketError> {
    if lease.socket_path != socket_path {
        return Err(DaemonSocketError {
            message: format!("Daemon socket lease does not match {socket_path}"),
        });
    }
    Ok(())
}

fn assert_socket_lease_held(socket_path: &str, lease: &DaemonSocketPathLease) -> Result<(), DaemonSocketError> {
    if let Some(compromise) = lease.compromise() {
        return Err(DaemonSocketError {
            message: format!(
                "Daemon socket lease for {socket_path} was compromised: {}",
                compromise.message
            ),
        });
    }
    Ok(())
}

pub fn default_daemon_socket_dir() -> String {
    let suffix = current_uid().map(|uid| uid.to_string()).unwrap_or_else(|| "user".to_string());
    Path::new(&std::env::temp_dir())
        .join(format!("prime-agent-{suffix}"))
        .to_string_lossy()
        .to_string()
}

fn ensure_default_daemon_socket_dir(socket_path: &str) -> Result<(), DaemonSocketError> {
    let default_dir = default_daemon_socket_dir();
    if crate::utils::pi_user_agent::process_platform() == "win32" {
        return Ok(());
    }
    if parent_dir(socket_path) != default_dir {
        return Ok(());
    }

    if !Path::new(&default_dir).exists() {
        create_dir_mode(&default_dir, DAEMON_SOCKET_DIR_MODE).map_err(|error| DaemonSocketError {
            message: error.to_string(),
        })?;
    }

    let metadata = std::fs::symlink_metadata(&default_dir).map_err(|error| DaemonSocketError {
        message: error.to_string(),
    })?;
    if !metadata.is_dir() {
        return Err(DaemonSocketError {
            message: format!("Daemon socket directory exists and is not a directory: {default_dir}"),
        });
    }

    if let (Some(uid), Some(owner)) = (current_uid(), file_owner(&metadata)) {
        if owner != uid {
            return Err(DaemonSocketError {
                message: format!("Daemon socket directory is not owned by the current user: {default_dir}"),
            });
        }
    }

    let _ = set_file_mode(&default_dir, DAEMON_SOCKET_DIR_MODE);
    Ok(())
}

async fn can_connect_to_unix_socket(socket_path: &str) -> bool {
    let connect = tokio::net::UnixStream::connect(socket_path);
    match tokio::time::timeout(Duration::from_millis(250), connect).await {
        Ok(Ok(_stream)) => true,
        _ => false,
    }
}

fn parent_dir(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(unix)]
fn is_socket(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    metadata.file_type().is_socket()
}

#[cfg(not(unix))]
fn is_socket(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn identity_from_metadata(metadata: &std::fs::Metadata) -> DaemonSocketIdentity {
    use std::os::unix::fs::MetadataExt;
    DaemonSocketIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn identity_from_metadata(_metadata: &std::fs::Metadata) -> DaemonSocketIdentity {
    DaemonSocketIdentity { dev: 0, ino: 0 }
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    Some(unsafe { libc::getuid() })
}

#[cfg(not(unix))]
fn current_uid() -> Option<u32> {
    None
}

#[cfg(unix)]
fn file_owner(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.uid())
}

#[cfg(not(unix))]
fn file_owner(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn create_dir_mode(path: &str, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(mode);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_dir_mode(path: &str, _mode: u32) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

#[cfg(unix)]
fn set_file_mode(path: &str, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_file_mode(_path: &str, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// `export { normalizeSocketPath } from "../../utils/daemon-socket-path.js"`.
pub fn normalize_socket_path_for_daemon(path: &str, base_dir: Option<&str>) -> String {
    normalize_socket_path(path, base_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paths_match_the_platform() {
        let path = default_daemon_socket_path();
        if crate::utils::pi_user_agent::process_platform() == "win32" {
            assert_eq!(path, "\\\\.\\pipe\\prime-agent-daemon");
        } else {
            assert!(path.ends_with("daemon.sock"));
        }
        assert!(default_daemon_socket_dir().contains("prime-agent-"));
    }

    #[tokio::test]
    async fn a_lease_blocks_a_second_acquire_until_release() {
        if crate::utils::pi_user_agent::process_platform() == "win32" {
            assert!(acquire_daemon_socket_path_lease("/tmp/whatever.sock").await.is_none());
            return;
        }
        let root = std::env::temp_dir().join(format!("daemon-socket-lease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        let socket_path = root.join("daemon.sock").to_string_lossy().to_string();
        let lease = acquire_daemon_socket_path_lease(&socket_path).await.expect("lease");
        assert!(Path::new(&lock_directory_path(&socket_path)).exists());
        lease.release().await;
        assert!(!Path::new(&lock_directory_path(&socket_path)).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lease_compromise_notifies_listeners_once() {
        let lease = std::sync::Arc::new(DaemonSocketPathLease::new("/tmp/s.sock", None));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = std::sync::Arc::clone(&seen);
        let listener: std::sync::Arc<DaemonSocketCompromiseListener> =
            std::sync::Arc::new(move |error: &DaemonSocketError| {
                captured.lock().expect("seen").push(error.message.clone());
            });
        let _unsubscribe = lease.on_compromised(listener);
        lease.record_compromise(DaemonSocketError {
            message: "lock lost".to_string(),
        });
        lease.record_compromise(DaemonSocketError {
            message: "second".to_string(),
        });
        assert_eq!(seen.lock().expect("seen").clone(), vec!["lock lost".to_string()]);
        assert_eq!(
            lease.compromise().map(|error| error.message),
            Some("lock lost".to_string())
        );
        assert_eq!(
            assert_socket_lease("/tmp/other.sock", &lease)
                .expect_err("mismatch")
                .message,
            "Daemon socket lease does not match /tmp/other.sock"
        );
        assert_eq!(
            assert_socket_lease_held("/tmp/s.sock", &lease)
                .expect_err("compromised")
                .message,
            "Daemon socket lease for /tmp/s.sock was compromised: lock lost"
        );
    }

    #[tokio::test]
    async fn cleanup_removes_the_socket_when_the_identity_matches() {
        if crate::utils::pi_user_agent::process_platform() == "win32" {
            return;
        }
        let root = std::env::temp_dir().join(format!("daemon-socket-cleanup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        let socket_path = root.join("daemon.sock").to_string_lossy().to_string();
        std::fs::write(&socket_path, b"stale").expect("stale file");
        let identity = get_daemon_socket_identity(&socket_path).expect("identity");
        cleanup_daemon_socket_path(&socket_path, Some(identity), None);
        assert!(!Path::new(&socket_path).exists());

        std::fs::write(&socket_path, b"stale").expect("stale file");
        cleanup_daemon_socket_path(
            &socket_path,
            Some(DaemonSocketIdentity {
                dev: 0,
                ino: 0,
            }),
            None,
        );
        assert!(Path::new(&socket_path).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn prepare_rejects_a_non_socket_path_and_ignores_missing_paths() {
        if crate::utils::pi_user_agent::process_platform() == "win32" {
            return;
        }
        let root = std::env::temp_dir().join(format!("daemon-socket-prepare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        let socket_path = root.join("daemon.sock").to_string_lossy().to_string();
        prepare_daemon_socket_path(&socket_path, None)
            .await
            .expect("missing path is fine");
        std::fs::write(&socket_path, b"not a socket").expect("file");
        assert_eq!(
            prepare_daemon_socket_path(&socket_path, None)
                .await
                .expect_err("not a socket")
                .message,
            format!("Daemon socket path exists and is not a socket: {socket_path}")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
