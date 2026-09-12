//! Port of packages/coding-agent/src/modes/daemon/daemon-supervisor-ownership.ts
//!
//! Durable supervisor ownership: a per-generation owner directory under the
//! registry, a shutdown-admission lease, and startup fences. The
//! `proper-lockfile` registry guard is ported as a lock directory with the same
//! stale/update windows, retry budget, and inode identity check.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::session_lease::get_process_start_id;
use crate::utils::atomic_file::{write_file_atomic_sync, WriteFileAtomicOptions};
use crate::utils::child_process::{is_process_alive, is_zombie_process, process_id_exists};
use crate::utils::daemon_socket_path::normalize_socket_path;

use super::daemon_socket::default_daemon_socket_dir;

pub const DAEMON_SUPERVISOR_REGISTRY_DIR_ENV: &str = "PRIME_AGENT_INTERNAL_DAEMON_SUPERVISOR_REGISTRY_DIR";

const OWNER_VERSION: u32 = 1;
const REGISTRY_LOCK_STALE_MS: u64 = 5000;
const REGISTRY_LOCK_UPDATE_MS: u64 = 1000;
const REGISTRY_LOCK_RETRIES: u32 = 500;
const REGISTRY_LOCK_RETRY_MS: u64 = 10;
const STARTUP_FENCE_POLL_MS: u64 = 250;
const SHUTDOWN_ADMISSION_FILE_NAME: &str = "shutdown-admission.json";
const SHUTDOWN_ADMISSION_LEASE_MS: i64 = 5000;
const SHUTDOWN_ADMISSION_REFRESH_MS: u64 = 1000;
const SHUTDOWN_ADMISSION_WAIT_MS: u64 = 50;
// The 250ms fence poll must not spawn `ps` per tick; existence stays
// kill(0)-checked every tick.
const OWNER_ZOMBIE_CONFIRM_INTERVAL_MS: i64 = 5000;

pub type DaemonSupervisorOwnerPhase = String;

pub const OWNER_PHASE_STARTING: &str = "starting";
pub const OWNER_PHASE_OWNER: &str = "owner";
pub const OWNER_PHASE_STOPPING: &str = "stopping";

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: i64,
    #[serde(rename = "processStartId", skip_serializing_if = "Option::is_none", default)]
    pub process_start_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonSupervisorOwnerRecord {
    pub version: u32,
    pub role: String,
    pub token: String,
    pub generation: String,
    pub pid: i64,
    #[serde(rename = "processStartId", skip_serializing_if = "Option::is_none", default)]
    pub process_start_id: Option<String>,
    #[serde(rename = "socketPath")]
    pub socket_path: String,
    #[serde(rename = "descriptorDir")]
    pub descriptor_dir: String,
    #[serde(rename = "agentDir")]
    pub agent_dir: String,
    #[serde(rename = "appVersion")]
    pub app_version: String,
    pub phase: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

impl DaemonSupervisorOwnerRecord {
    pub fn identity(&self) -> ProcessIdentity {
        ProcessIdentity {
            pid: self.pid,
            process_start_id: self.process_start_id.clone(),
        }
    }

    pub fn scope(&self) -> DaemonSupervisorOwnerScope {
        DaemonSupervisorOwnerScope {
            version: self.version,
            role: self.role.clone(),
            token: self.token.clone(),
            generation: self.generation.clone(),
            socket_path: self.socket_path.clone(),
            descriptor_dir: self.descriptor_dir.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonShutdownAdmissionRecord {
    pub version: u32,
    pub token: String,
    pub pid: i64,
    #[serde(rename = "processStartId", skip_serializing_if = "Option::is_none", default)]
    pub process_start_id: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
}

impl DaemonShutdownAdmissionRecord {
    pub fn identity(&self) -> ProcessIdentity {
        ProcessIdentity {
            pid: self.pid,
            process_start_id: self.process_start_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonSupervisorOwnerScope {
    pub version: u32,
    pub role: String,
    pub token: String,
    pub generation: String,
    #[serde(rename = "socketPath")]
    pub socket_path: String,
    #[serde(rename = "descriptorDir")]
    pub descriptor_dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStartupFenceRecord {
    pub version: u32,
    pub token: String,
    #[serde(rename = "ownerToken")]
    pub owner_token: String,
    pub pid: i64,
    #[serde(rename = "processStartId")]
    pub process_start_id: String,
    #[serde(rename = "socketPath")]
    pub socket_path: String,
    #[serde(rename = "supervisorGeneration")]
    pub supervisor_generation: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

impl DaemonStartupFenceRecord {
    pub fn identity(&self) -> ProcessIdentity {
        ProcessIdentity {
            pid: self.pid,
            process_start_id: Some(self.process_start_id.clone()),
        }
    }
}

/// `DaemonSupervisorHelloIdentity`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonSupervisorHelloIdentity {
    pub supervisor_generation: Option<String>,
    pub supervisor_owner_token: Option<String>,
    pub supervisor_pid: Option<i64>,
    pub supervisor_process_start_id: Option<String>,
    pub supervisor_socket_path: Option<String>,
}

impl DaemonSupervisorHelloIdentity {
    pub fn from_value(value: &Value) -> Self {
        let field = |key: &str| value.get(key);
        Self {
            supervisor_generation: field("supervisorGeneration")
                .and_then(Value::as_str)
                .map(str::to_string),
            supervisor_owner_token: field("supervisorOwnerToken")
                .and_then(Value::as_str)
                .map(str::to_string),
            supervisor_pid: field("supervisorPid").and_then(Value::as_i64),
            supervisor_process_start_id: field("supervisorProcessStartId")
                .and_then(Value::as_str)
                .map(str::to_string),
            supervisor_socket_path: field("supervisorSocketPath")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AcquireDaemonSupervisorOwnershipOptions {
    pub socket_path: String,
    pub descriptor_dir: String,
    pub agent_dir: String,
    pub generation: String,
    pub app_version: String,
    pub registry_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSupervisorAlreadyRunningError {
    pub owner: DaemonSupervisorOwnerRecord,
}

impl DaemonSupervisorAlreadyRunningError {
    pub const CODE: &'static str = "daemon_supervisor_already_running";

    pub fn message(&self) -> String {
        format!(
            "Daemon supervisor {} already owns {}",
            self.owner.generation, self.owner.socket_path
        )
    }
}

impl std::fmt::Display for DaemonSupervisorAlreadyRunningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for DaemonSupervisorAlreadyRunningError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSupervisorOwnershipLostError {
    pub generation: String,
    pub socket_path: Option<String>,
    pub registry_dir: Option<String>,
}

impl DaemonSupervisorOwnershipLostError {
    pub const CODE: &'static str = "supervisor_generation_stale";

    pub fn new(generation: &str, socket_path: Option<&str>, registry_dir: Option<&str>) -> Self {
        Self {
            generation: generation.to_string(),
            socket_path: socket_path.map(str::to_string),
            registry_dir: registry_dir.map(str::to_string),
        }
    }

    pub fn message(&self) -> String {
        let context: Vec<String> = [
            self.socket_path
                .as_ref()
                .map(|socket_path| format!("socket: {socket_path}")),
            self.registry_dir
                .as_ref()
                .map(|registry_dir| format!("registry: {registry_dir}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        format!(
            "Daemon supervisor generation {} no longer owns its registry entry (record on disk is missing or was replaced){}{}; restart the daemon to recover — sessions are preserved",
            self.generation,
            if context.is_empty() { "" } else { "; " },
            context.join("; ")
        )
    }
}

impl std::fmt::Display for DaemonSupervisorOwnershipLostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for DaemonSupervisorOwnershipLostError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonShutdownAdmissionError {
    pub message: String,
}

impl DaemonShutdownAdmissionError {
    pub const CODE: &'static str = "daemon_shutdown_in_progress";

    pub fn new(message: Option<&str>) -> Self {
        Self {
            message: message
                .unwrap_or("Daemon shutdown is in progress")
                .to_string(),
        }
    }
}

impl std::fmt::Display for DaemonShutdownAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DaemonShutdownAdmissionError {}

/// The registry guard failure surface (lock lost or guard directory stolen).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSupervisorRegistryGuardError {
    pub message: String,
}

impl std::fmt::Display for DaemonSupervisorRegistryGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DaemonSupervisorRegistryGuardError {}

/// Owns a lease-renew loop safely: the unref'd interval, single-flight refresh
/// dedup shared by timer-fired and direct calls, and lost-state fencing.
pub struct RenewableRegistryRecord {
    registry_dir: String,
    stopped: AtomicBool,
    lost: AtomicBool,
    refresh_task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    renew_under_guard: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    create_lost_error: Arc<dyn Fn() -> String + Send + Sync>,
}

impl RenewableRegistryRecord {
    pub fn new(
        registry_dir: &str,
        refresh_ms: u64,
        renew_under_guard: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
        create_lost_error: Arc<dyn Fn() -> String + Send + Sync>,
    ) -> Arc<Self> {
        let record = Arc::new(Self {
            registry_dir: registry_dir.to_string(),
            stopped: AtomicBool::new(false),
            lost: AtomicBool::new(false),
            refresh_task: StdMutex::new(None),
            renew_under_guard,
            create_lost_error,
        });
        let weak = Arc::downgrade(&record);
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(refresh_ms));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(record) = weak.upgrade() else {
                    return;
                };
                if record.stopped.load(Ordering::SeqCst) {
                    return;
                }
                let _ = record.assert_or_renew().await;
            }
        });
        *record.refresh_task.lock().expect("refresh task poisoned") = Some(handle);
        record
    }

    pub async fn assert_or_renew(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::SeqCst) || self.lost.load(Ordering::SeqCst) {
            return Err((self.create_lost_error)());
        }
        self.perform_renew().await
    }

    async fn perform_renew(&self) -> Result<(), String> {
        let result = with_daemon_supervisor_registry_guard(&self.registry_dir, {
            let renew = Arc::clone(&self.renew_under_guard);
            move || {
                // stop() may have completed while this call waited on the guard;
                // a stopped record must never be rewritten to disk.
                renew()
            }
        })
        .await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                self.lost.store(true, Ordering::SeqCst);
                if let Some(task) = self.refresh_task.lock().expect("refresh task poisoned").take() {
                    task.abort();
                }
                Err(error.message)
            }
        }
    }

    pub async fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(task) = self.refresh_task.lock().expect("refresh task poisoned").take() {
            task.abort();
        }
    }
}

pub struct DaemonSupervisorOwnership {
    pub record: StdMutex<DaemonSupervisorOwnerRecord>,
    registry_dir: String,
    owner_directory: String,
    released: AtomicBool,
}

impl DaemonSupervisorOwnership {
    pub fn new(record: DaemonSupervisorOwnerRecord, registry_dir: &str, owner_directory: &str) -> Self {
        Self {
            record: StdMutex::new(record),
            registry_dir: registry_dir.to_string(),
            owner_directory: owner_directory.to_string(),
            released: AtomicBool::new(false),
        }
    }

    pub fn snapshot(&self) -> DaemonSupervisorOwnerRecord {
        self.record.lock().expect("owner record poisoned").clone()
    }

    pub async fn assert_current(&self) -> Result<(), DaemonSupervisorOwnershipLostError> {
        if self.released.load(Ordering::SeqCst) {
            return Err(self.ownership_lost_error());
        }
        let record = self.snapshot();
        let current = read_owner_record(&self.owner_directory);
        if current
            .map(|current| same_owner_record(&current, &record))
            .unwrap_or(false)
        {
            return Ok(());
        }
        Err(self.ownership_lost_error())
    }

    fn ownership_lost_error(&self) -> DaemonSupervisorOwnershipLostError {
        let record = self.snapshot();
        DaemonSupervisorOwnershipLostError::new(
            &record.generation,
            Some(&record.socket_path),
            Some(&self.registry_dir),
        )
    }

    pub async fn update_phase(&self, phase: &str) -> Result<(), String> {
        if self.released.load(Ordering::SeqCst) {
            return Ok(());
        }
        let record = self.snapshot();
        let phase = phase.to_string();
        let updated = mutate_daemon_supervisor_owner(
            &record.generation,
            &record.token,
            Box::new(move |owner| {
                owner.phase = phase;
            }),
            Some(&self.registry_dir),
        )
        .await?;
        let Some(updated) = updated else {
            return Err(format!(
                "Daemon supervisor ownership was lost for {}",
                record.socket_path
            ));
        };
        {
            let mut record = self.record.lock().expect("owner record poisoned");
            record.phase = updated.phase.clone();
            record.updated_at = updated.updated_at.clone();
        }
        Ok(())
    }

    pub async fn release(&self) -> Result<(), String> {
        if self.released.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let token = self.snapshot().token;
        let owner_directory = self.owner_directory.clone();
        let mut released_directory: Option<String> = None;
        let result = with_daemon_supervisor_registry_guard(&self.registry_dir, {
            let owner_directory = owner_directory.clone();
            move || {
                let current = read_owner_record(&owner_directory);
                if current.map(|current| current.token != token).unwrap_or(true) {
                    return Ok(());
                }
                let renamed = format!("{owner_directory}.released-{}", uuid::Uuid::new_v4());
                std::fs::rename(&owner_directory, &renamed).map_err(|error| error.to_string())?;
                released_directory = Some(renamed);
                Ok(())
            }
        })
        .await;
        if let Some(directory) = released_directory {
            let _ = std::fs::remove_dir_all(directory);
        }
        result.map_err(|error| error.message)
    }
}

pub struct DaemonShutdownAdmission {
    record: StdMutex<DaemonShutdownAdmissionRecord>,
    registry_dir: String,
    renewal: Arc<RenewableRegistryRecord>,
    released: AtomicBool,
}

impl DaemonShutdownAdmission {
    pub fn new(record: DaemonShutdownAdmissionRecord, registry_dir: &str) -> Arc<Self> {
        let record_slot = Arc::new(StdMutex::new(record));
        let renewal = RenewableRegistryRecord::new(
            registry_dir,
            SHUTDOWN_ADMISSION_REFRESH_MS,
            {
                let registry_dir = registry_dir.to_string();
                let record_slot = Arc::clone(&record_slot);
                Arc::new(move || renew_shutdown_admission(&registry_dir, &record_slot))
            },
            Arc::new(|| "Daemon shutdown admission was lost".to_string()),
        );
        Arc::new(Self {
            record: record_slot,
            registry_dir: registry_dir.to_string(),
            renewal,
            released: AtomicBool::new(false),
        })
    }

    pub fn record(&self) -> DaemonShutdownAdmissionRecord {
        self.record.lock().expect("admission record poisoned").clone()
    }

    pub async fn assert_or_renew(&self) -> Result<(), DaemonShutdownAdmissionError> {
        if self.released.load(Ordering::SeqCst) {
            return Err(DaemonShutdownAdmissionError::new(Some(
                "Daemon shutdown admission was lost",
            )));
        }
        self.renewal
            .assert_or_renew()
            .await
            .map_err(|message| DaemonShutdownAdmissionError::new(Some(&message)))
    }

    pub async fn release(&self) -> Result<(), String> {
        if self.released.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.renewal.stop().await;
        let token = self.record().token;
        with_daemon_supervisor_registry_guard(&self.registry_dir, move || {
            let path = shutdown_admission_path(&self.registry_dir);
            let current = read_shutdown_admission(&path);
            if current.map(|current| current.token == token).unwrap_or(false) {
                let _ = std::fs::remove_file(&path);
            }
            Ok(())
        })
        .await
        .map_err(|error| error.message)
    }
}

/// The registry is durable authority state and must be global per user, so it
/// deliberately lives outside $TMPDIR and outside the per-invocation agent dir.
pub fn default_daemon_supervisor_registry_dir() -> String {
    default_daemon_supervisor_registry_dir_for(std::env::var(DAEMON_SUPERVISOR_REGISTRY_DIR_ENV).ok())
}

pub fn default_daemon_supervisor_registry_dir_for(environment_value: Option<String>) -> String {
    match environment_value {
        Some(value) => value,
        None => dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".prime")
            .join("supervisor-owners")
            .to_string_lossy()
            .to_string(),
    }
}

/// Pre-move registry location under $TMPDIR, consulted READ-ONLY while daemons
/// from before the ~/.prime move may still be running; gated off whenever the
/// registry is overridden.
pub fn legacy_daemon_supervisor_registry_dir() -> Option<String> {
    legacy_daemon_supervisor_registry_dir_for(std::env::var(DAEMON_SUPERVISOR_REGISTRY_DIR_ENV).ok())
}

pub fn legacy_daemon_supervisor_registry_dir_for(environment_value: Option<String>) -> Option<String> {
    if environment_value.is_some() {
        return None;
    }
    Some(
        Path::new(&default_daemon_socket_dir())
            .join("supervisor-owners")
            .to_string_lossy()
            .to_string(),
    )
}

/// Non-mutating legacy scan: never reclaims abandoned directories and runs
/// without the legacy guard.
pub fn read_legacy_owners_for_socket(
    legacy_registry_dir: &str,
    normalized_socket_path: &str,
) -> Vec<DaemonSupervisorOwnerRecord> {
    let Ok(entries) = std::fs::read_dir(legacy_registry_dir) else {
        return Vec::new();
    };
    let mut owners: Vec<DaemonSupervisorOwnerRecord> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".owner") {
            continue;
        }
        let directory = Path::new(legacy_registry_dir)
            .join(&name)
            .to_string_lossy()
            .to_string();
        if let Some(owner) = read_owner_record(&directory) {
            if owner.socket_path == normalized_socket_path {
                owners.push(owner);
            }
        }
    }
    owners
}

fn renew_shutdown_admission(
    registry_dir: &str,
    record: &Arc<StdMutex<DaemonShutdownAdmissionRecord>>,
) -> Result<(), String> {
    let path = shutdown_admission_path(registry_dir);
    let current = read_shutdown_admission(&path);
    let record_snapshot = record.lock().expect("admission record poisoned").clone();
    let valid = current
        .map(|current| {
            current.token == record_snapshot.token
                && current.pid == record_snapshot.pid
                && current.process_start_id == record_snapshot.process_start_id
                && parse_iso_ms(&current.expires_at) > now_ms()
                && matches_exact_process_identity(&record_snapshot.identity())
        })
        .unwrap_or(false);
    if !valid {
        return Err("Daemon shutdown admission was lost".to_string());
    }
    let now = now_ms();
    let mut updated = record_snapshot;
    updated.updated_at = iso_from_ms(now);
    updated.expires_at = iso_from_ms(now + SHUTDOWN_ADMISSION_LEASE_MS);
    write_json_atomically(&path, &serde_json::to_value(&updated).unwrap_or(Value::Null))?;
    *record.lock().expect("admission record poisoned") = updated;
    Ok(())
}

/// The `proper-lockfile` registry guard: a lock directory with the same
/// stale/update windows, retry budget, and inode identity check.
pub async fn with_daemon_supervisor_registry_guard<T, F>(
    registry_dir: &str,
    action: F,
) -> Result<T, DaemonSupervisorRegistryGuardError>
where
    F: FnOnce() -> Result<T, String>,
{
    std::fs::create_dir_all(registry_dir).map_err(|error| DaemonSupervisorRegistryGuardError {
        message: error.to_string(),
    })?;
    set_dir_mode(registry_dir, 0o700);
    let guard_path = Path::new(registry_dir).join(".guard").to_string_lossy().to_string();
    let deadline = Instant::now() + Duration::from_millis(REGISTRY_LOCK_RETRIES as u64 * REGISTRY_LOCK_RETRY_MS);
    let guard_ino = acquire_registry_guard(&guard_path, deadline).await?;
    let guard_stolen = |guard_ino: Option<u64>| -> bool {
        let Some(guard_ino) = guard_ino else {
            return false;
        };
        match directory_ino(&guard_path) {
            Some(current) => current != guard_ino,
            None => true,
        }
    };
    let assert_guard_held = |guard_ino: Option<u64>| -> Result<(), DaemonSupervisorRegistryGuardError> {
        if guard_stolen(guard_ino) {
            return Err(DaemonSupervisorRegistryGuardError {
                message: "Daemon supervisor registry guard was compromised: the guard lock changed hands"
                    .to_string(),
            });
        }
        Ok(())
    };
    assert_guard_held(guard_ino)?;
    let result = action();
    let assert_result = assert_guard_held(guard_ino);
    // A stolen-but-undetected guard is never released: that would delete the
    // successor's lock.
    if !guard_stolen(guard_ino) {
        let _ = std::fs::remove_dir(&guard_path);
    }
    assert_result?;
    result.map_err(|message| DaemonSupervisorRegistryGuardError { message })
}

async fn acquire_registry_guard(
    guard_path: &str,
    deadline: Instant,
) -> Result<Option<u64>, DaemonSupervisorRegistryGuardError> {
    loop {
        match std::fs::create_dir(guard_path) {
            Ok(()) => return Ok(directory_ino(guard_path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_stale(guard_path, REGISTRY_LOCK_STALE_MS) {
                    let _ = std::fs::remove_dir(guard_path);
                    continue;
                }
                if Instant::now() >= deadline {
                    return Err(DaemonSupervisorRegistryGuardError {
                        message: "Daemon supervisor registry guard could not be acquired".to_string(),
                    });
                }
                tokio::time::sleep(Duration::from_millis(REGISTRY_LOCK_RETRY_MS)).await;
            }
            Err(error) => {
                return Err(DaemonSupervisorRegistryGuardError {
                    message: error.to_string(),
                })
            }
        }
    }
}

fn lock_is_stale(path: &str, stale_ms: u64) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .map(|age| age.as_millis() as u64 > stale_ms)
        .unwrap_or(false)
}

#[cfg(unix)]
fn directory_ino(path: &str) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|metadata| metadata.ino())
}

#[cfg(not(unix))]
fn directory_ino(_path: &str) -> Option<u64> {
    None
}

#[cfg(unix)]
fn set_dir_mode(path: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_dir_mode(_path: &str, _mode: u32) {}

pub async fn mutate_daemon_supervisor_owner(
    generation: &str,
    expected_token: &str,
    mutation: Box<dyn FnOnce(&mut DaemonSupervisorOwnerRecord) + Send>,
    registry_dir: Option<&str>,
) -> Result<Option<DaemonSupervisorOwnerRecord>, String> {
    let registry_dir = registry_dir
        .map(str::to_string)
        .unwrap_or_else(default_daemon_supervisor_registry_dir);
    let generation = generation.to_string();
    let expected_token = expected_token.to_string();
    let mut mutation = Some(mutation);
    with_daemon_supervisor_registry_guard(&registry_dir, move || {
        let directory = owner_directory_path(&registry_dir, &generation)?;
        if !Path::new(&directory).exists() {
            return Ok(None);
        }
        let current = require_owner_record(&directory)?;
        if current.token != expected_token {
            return Ok(None);
        }
        let mut current = current;
        if let Some(mutation) = mutation.take() {
            mutation(&mut current);
        }
        current.updated_at = iso_from_ms(now_ms());
        if !is_daemon_supervisor_owner_record(&serde_json::to_value(&current).unwrap_or(Value::Null))
            || current.generation != generation
            || current.token != expected_token
        {
            return Err(format!("Invalid mutation for daemon supervisor owner {generation}"));
        }
        write_owner_record(&directory, &current)?;
        Ok(Some(current))
    })
    .await
    .map_err(|error| error.message)
}

pub async fn acquire_daemon_supervisor_ownership(
    options: AcquireDaemonSupervisorOwnershipOptions,
) -> Result<DaemonSupervisorOwnership, String> {
    let registry_dir = options
        .registry_dir
        .clone()
        .unwrap_or_else(default_daemon_supervisor_registry_dir);
    std::fs::create_dir_all(&registry_dir).map_err(|error| error.to_string())?;
    set_dir_mode(&registry_dir, 0o700);
    let token = uuid::Uuid::new_v4().to_string();
    let process_start_id = get_process_start_id(std::process::id() as i64);
    let now = iso_from_ms(now_ms());
    let record = DaemonSupervisorOwnerRecord {
        version: OWNER_VERSION,
        role: "supervisor".to_string(),
        token: token.clone(),
        generation: options.generation.clone(),
        pid: std::process::id() as i64,
        process_start_id,
        socket_path: normalize_socket_path(&options.socket_path, None),
        descriptor_dir: canonicalize_filesystem_path(&options.descriptor_dir),
        agent_dir: canonicalize_filesystem_path(&options.agent_dir),
        app_version: options.app_version.clone(),
        phase: OWNER_PHASE_STARTING.to_string(),
        created_at: now.clone(),
        updated_at: now,
    };
    let candidate_directory = Path::new(&registry_dir)
        .join(format!(".candidate-{}-{token}", std::process::id()))
        .to_string_lossy()
        .to_string();
    let owner_directory = owner_directory_path(&registry_dir, &options.generation)?;
    std::fs::create_dir_all(&candidate_directory).map_err(|error| error.to_string())?;
    set_dir_mode(&candidate_directory, 0o700);
    let mut stale_directories: Vec<String> = Vec::new();
    let mut guard_error: Option<String> = None;
    let guard_result = with_daemon_supervisor_registry_guard(&registry_dir, {
        let record = record.clone();
        let candidate_directory = candidate_directory.clone();
        let owner_directory = owner_directory.clone();
        let registry_dir = registry_dir.clone();
        let mut stale_directories = std::mem::take(&mut stale_directories);
        move || {
            let result = (|| -> Result<Vec<String>, String> {
                write_owner_scope(&candidate_directory, &record)?;
                write_owner_record(&candidate_directory, &record)?;
                if read_active_shutdown_admission(&registry_dir).is_some() {
                    return Err(DaemonShutdownAdmissionError::new(None).message);
                }
                let mut stale: Vec<String> = Vec::new();
                for directory in list_owner_directories(&registry_dir) {
                    let owner = read_owner_record_for_scope(&directory, &|scope| {
                        owner_conflicts(scope, &record.scope())
                    })?;
                    let Some(owner) = owner else {
                        continue;
                    };
                    if !owner_conflicts(&owner.scope(), &record.scope()) {
                        continue;
                    }
                    if is_process_identity_alive(&owner.identity()) {
                        return Err(DaemonSupervisorAlreadyRunningError { owner }.message());
                    }
                    let stale_directory = format!("{directory}.stale-{}", uuid::Uuid::new_v4());
                    std::fs::rename(&directory, &stale_directory).map_err(|error| error.to_string())?;
                    stale.push(stale_directory);
                }
                std::fs::rename(&candidate_directory, &owner_directory).map_err(|error| error.to_string())?;
                Ok(stale)
            })();
            match result {
                Ok(stale) => {
                    stale_directories.extend(stale);
                    Ok(())
                }
                Err(error) => {
                    guard_error = Some(error.clone());
                    Err(error)
                }
            }
        }
    })
    .await;
    if let Err(error) = guard_result {
        let _ = std::fs::remove_dir_all(&candidate_directory);
        for directory in stale_directories {
            let _ = std::fs::remove_dir_all(directory);
        }
        return Err(guard_error.unwrap_or(error.message));
    }
    for directory in stale_directories {
        let _ = std::fs::remove_dir_all(directory);
    }
    Ok(DaemonSupervisorOwnership::new(
        record,
        &registry_dir,
        &owner_directory,
    ))
}

/// Owner liveness with the zombie-confirmation cache (bounded by the confirm interval).
pub fn is_owner_process_alive(pid: i64) -> bool {
    static CONFIRMATIONS: once_cell::sync::Lazy<StdMutex<HashMap<i64, i64>>> =
        once_cell::sync::Lazy::new(|| StdMutex::new(HashMap::new()));
    if !process_id_exists(pid as i32) {
        CONFIRMATIONS.lock().expect("confirmations poisoned").remove(&pid);
        return false;
    }
    let now = now_ms();
    {
        let confirmations = CONFIRMATIONS.lock().expect("confirmations poisoned");
        if let Some(confirmed_at) = confirmations.get(&pid) {
            if now - confirmed_at < OWNER_ZOMBIE_CONFIRM_INTERVAL_MS {
                return true;
            }
        }
    }
    if is_zombie_process(pid as i32) {
        CONFIRMATIONS.lock().expect("confirmations poisoned").remove(&pid);
        return false;
    }
    let mut confirmations = CONFIRMATIONS.lock().expect("confirmations poisoned");
    // Expired entries belong to owners nothing asserts anymore; dropping them
    // keeps the cache bounded.
    confirmations.retain(|_, confirmed_at| now - *confirmed_at < OWNER_ZOMBIE_CONFIRM_INTERVAL_MS);
    confirmations.insert(pid, now);
    true
}

pub async fn assert_daemon_supervisor_owner_current(
    owner: &DaemonSupervisorOwnerRecord,
    validated_fingerprint: Option<&str>,
    registry_dir: Option<&str>,
    legacy_registry_dir: Option<&str>,
) -> Result<String, DaemonSupervisorOwnershipLostError> {
    let registry_dir = registry_dir
        .map(str::to_string)
        .unwrap_or_else(default_daemon_supervisor_registry_dir);
    let legacy_registry_dir = match (registry_dir.as_str(), legacy_registry_dir) {
        (_, Some(legacy)) => Some(legacy.to_string()),
        (_, None) => None,
    };
    let owner_directory = owner_directory_path(&registry_dir, &owner.generation)
        .map_err(|_| DaemonSupervisorOwnershipLostError::new(&owner.generation, Some(&owner.socket_path), Some(&registry_dir)))?;
    let current = read_owner_record(&owner_directory).or_else(|| {
        legacy_registry_dir.as_ref().and_then(|legacy| {
            owner_directory_path(legacy, &owner.generation)
                .ok()
                .and_then(|directory| read_owner_record(&directory))
        })
    });
    let lost = || {
        DaemonSupervisorOwnershipLostError::new(
            &owner.generation,
            Some(&owner.socket_path),
            Some(&registry_dir),
        )
    };
    let Some(current) = current else {
        return Err(lost());
    };
    if current.pid != owner.pid
        || current.process_start_id != owner.process_start_id
        || current.socket_path != normalize_socket_path(&owner.socket_path, None)
        || !is_owner_process_alive(current.pid)
    {
        return Err(lost());
    }
    let fingerprint = owner_record_fingerprint(&current);
    if validated_fingerprint != Some(fingerprint.as_str()) && !is_process_identity_alive(&current.identity()) {
        return Err(lost());
    }
    Ok(fingerprint)
}

pub async fn acquire_daemon_shutdown_admission() -> Result<Arc<DaemonShutdownAdmission>, String> {
    let registry_dir = default_daemon_supervisor_registry_dir();
    let process_start_id = get_process_start_id(std::process::id() as i64);
    loop {
        let mut acquired: Option<DaemonShutdownAdmissionRecord> = None;
        let result = with_daemon_supervisor_registry_guard(&registry_dir, {
            let registry_dir = registry_dir.clone();
            let process_start_id = process_start_id.clone();
            move || {
                if read_active_shutdown_admission(&registry_dir).is_some() {
                    return Ok(());
                }
                let now = now_ms();
                let record = DaemonShutdownAdmissionRecord {
                    version: OWNER_VERSION,
                    token: uuid::Uuid::new_v4().to_string(),
                    pid: std::process::id() as i64,
                    process_start_id,
                    created_at: iso_from_ms(now),
                    updated_at: iso_from_ms(now),
                    expires_at: iso_from_ms(now + SHUTDOWN_ADMISSION_LEASE_MS),
                };
                write_json_atomically(
                    &shutdown_admission_path(&registry_dir),
                    &serde_json::to_value(&record).unwrap_or(Value::Null),
                )?;
                acquired = Some(record);
                Ok(())
            }
        })
        .await;
        result.map_err(|error| error.message)?;
        if let Some(record) = acquired {
            return Ok(DaemonShutdownAdmission::new(record, &registry_dir));
        }
        tokio::time::sleep(Duration::from_millis(SHUTDOWN_ADMISSION_WAIT_MS)).await;
    }
}

pub async fn is_daemon_shutdown_admission_active() -> Result<bool, String> {
    let registry_dir = default_daemon_supervisor_registry_dir();
    with_daemon_supervisor_registry_guard(&registry_dir, {
        let registry_dir = registry_dir.clone();
        move || Ok(read_active_shutdown_admission(&registry_dir).is_some())
    })
    .await
    .map_err(|error| error.message)
}

pub async fn persist_daemon_startup_fence_from_owner(
    socket_path: &str,
    hello: &DaemonSupervisorHelloIdentity,
    registry_dir: Option<&str>,
    legacy_registry_dir: Option<&str>,
) -> Result<(), String> {
    let registry_dir = registry_dir
        .map(str::to_string)
        .unwrap_or_else(default_daemon_supervisor_registry_dir);
    std::fs::create_dir_all(&registry_dir).map_err(|error| error.to_string())?;
    set_dir_mode(&registry_dir, 0o700);
    let fence_directory = Path::new(&registry_dir)
        .join("startup-fences")
        .to_string_lossy()
        .to_string();
    std::fs::create_dir_all(&fence_directory).map_err(|error| error.to_string())?;
    set_dir_mode(&fence_directory, 0o700);
    let path = startup_fence_path(&fence_directory, socket_path);
    let normalized_socket_path = normalize_socket_path(socket_path, None);
    let hello = hello.clone();
    let legacy_registry_dir = legacy_registry_dir.map(str::to_string);
    let socket_path = socket_path.to_string();
    with_daemon_supervisor_registry_guard(&registry_dir, move || {
        let mut owners: Vec<DaemonSupervisorOwnerRecord> = Vec::new();
        for directory in list_owner_directories(&registry_dir) {
            if let Some(owner) =
                read_owner_record_for_scope(&directory, &|scope| scope.socket_path == normalized_socket_path)?
            {
                owners.push(owner);
            }
        }
        let mut matching: Vec<DaemonSupervisorOwnerRecord> = owners
            .into_iter()
            .filter(|owner| owner.socket_path == normalized_socket_path)
            .collect();
        if matching.is_empty() {
            if let Some(legacy_registry_dir) = &legacy_registry_dir {
                // Stale legacy leftovers are expected; keep only records matching
                // the identity the caller already holds.
                matching = read_legacy_owners_for_socket(legacy_registry_dir, &normalized_socket_path)
                    .into_iter()
                    .filter(|owner| {
                        Some(&owner.token) == hello.supervisor_owner_token.as_ref()
                            && Some(owner.pid) == hello.supervisor_pid
                    })
                    .collect();
            }
        }
        if matching.is_empty() {
            return Err(format!("Daemon supervisor owner does not match {socket_path}"));
        }
        if matching.len() > 1 {
            return Err(format!("Multiple daemon supervisor owners match {socket_path}"));
        }
        let owner = matching.remove(0);
        let hello_matches = hello.supervisor_pid == Some(owner.pid)
            && hello.supervisor_generation.as_deref() == Some(owner.generation.as_str())
            && hello.supervisor_owner_token.as_deref() == Some(owner.token.as_str())
            && hello
                .supervisor_socket_path
                .as_deref()
                .map(|path| normalize_socket_path(path, None) == owner.socket_path)
                .unwrap_or(false)
            && owner.process_start_id.is_some()
            && hello.supervisor_process_start_id == owner.process_start_id;
        if !hello_matches {
            return Err(format!(
                "Daemon supervisor hello does not match its durable owner for {socket_path}"
            ));
        }
        let observed_process_start_id = get_process_start_id(owner.pid);
        if observed_process_start_id != owner.process_start_id {
            return Err(format!(
                "Daemon supervisor process identity changed for {socket_path}"
            ));
        }
        let record = DaemonStartupFenceRecord {
            version: OWNER_VERSION,
            token: uuid::Uuid::new_v4().to_string(),
            owner_token: owner.token.clone(),
            pid: owner.pid,
            process_start_id: owner.process_start_id.clone().unwrap_or_default(),
            socket_path: owner.socket_path.clone(),
            supervisor_generation: owner.generation.clone(),
            created_at: iso_from_ms(now_ms()),
        };
        write_json_atomically(&path, &serde_json::to_value(&record).unwrap_or(Value::Null))
    })
    .await
    .map_err(|error| error.message)
}

pub async fn wait_for_daemon_startup_fence(
    socket_path: &str,
    timeout_ms: u64,
    registry_dir: Option<&str>,
) -> Result<(), String> {
    let registry_dir = registry_dir
        .map(str::to_string)
        .unwrap_or_else(default_daemon_supervisor_registry_dir);
    let path = startup_fence_path(
        &Path::new(&registry_dir)
            .join("startup-fences")
            .to_string_lossy()
            .to_string(),
        socket_path,
    );
    let deadline = now_ms() + timeout_ms as i64;
    loop {
        let fence = match read_startup_fence(&path) {
            Ok(fence) => fence,
            Err(error) => return Err(error),
        };
        let Some(fence) = fence else {
            return Ok(());
        };
        if fence.socket_path != normalize_socket_path(socket_path, None) {
            return Err(format!("Daemon startup fence does not match {socket_path}"));
        }
        if !is_process_identity_alive(&fence.identity()) {
            let registry_dir_for_guard = registry_dir.clone();
            let path_for_guard = path.clone();
            let token = fence.token.clone();
            let cleared = with_daemon_supervisor_registry_guard(&registry_dir_for_guard, move || {
                let current = read_startup_fence(&path_for_guard)?;
                let Some(current) = current else {
                    return Ok(true);
                };
                if current.token == token {
                    let _ = std::fs::remove_file(&path_for_guard);
                    return Ok(true);
                }
                Ok(false)
            })
            .await
            .map_err(|error| error.message)?;
            if cleared {
                return Ok(());
            }
            continue;
        }
        if now_ms() >= deadline {
            return Err(format!(
                "Timed out waiting for predecessor daemon process {} to exit",
                fence.pid
            ));
        }
        tokio::time::sleep(Duration::from_millis(STARTUP_FENCE_POLL_MS)).await;
    }
}

pub fn is_process_identity_alive(identity: &ProcessIdentity) -> bool {
    if !is_process_alive(identity.pid as i32) {
        return false;
    }
    let Some(expected) = &identity.process_start_id else {
        return true;
    };
    let observed = get_process_start_id(identity.pid);
    observed.is_none() || observed.as_deref() == Some(expected.as_str())
}

pub fn matches_exact_process_identity(identity: &ProcessIdentity) -> bool {
    if !is_process_alive(identity.pid as i32) {
        return false;
    }
    identity.process_start_id.is_none()
        || get_process_start_id(identity.pid).as_deref() == identity.process_start_id.as_deref()
}

pub fn canonicalize_filesystem_path(path: &str) -> String {
    let mut existing_ancestor = resolve_path(path);
    let mut missing_suffix: Vec<String> = Vec::new();
    loop {
        match std::fs::canonicalize(&existing_ancestor) {
            Ok(physical) => {
                let mut canonical = physical;
                for part in &missing_suffix {
                    canonical = canonical.join(part);
                }
                let canonical = canonical.to_string_lossy().to_string();
                return if crate::utils::pi_user_agent::process_platform() == "win32" {
                    canonical.to_lowercase()
                } else {
                    canonical
                };
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = Path::new(&existing_ancestor)
                    .parent()
                    .map(|parent| parent.to_string_lossy().to_string())
                    .unwrap_or_else(|| existing_ancestor.clone());
                if parent == existing_ancestor {
                    let unresolved = resolve_path(path);
                    return if crate::utils::pi_user_agent::process_platform() == "win32" {
                        unresolved.to_lowercase()
                    } else {
                        unresolved
                    };
                }
                missing_suffix.insert(
                    0,
                    Path::new(&existing_ancestor)
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default(),
                );
                existing_ancestor = parent;
            }
            Err(error) => return format!("{}: {error}", existing_ancestor),
        }
    }
}

fn resolve_path(path: &str) -> String {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        return candidate.to_string_lossy().to_string();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(candidate).to_string_lossy().to_string())
        .unwrap_or_else(|_| candidate.to_string_lossy().to_string())
}

fn owner_conflicts(left: &DaemonSupervisorOwnerScope, right: &DaemonSupervisorOwnerScope) -> bool {
    left.socket_path == right.socket_path || left.descriptor_dir == right.descriptor_dir
}

fn same_owner_record(left: &DaemonSupervisorOwnerRecord, right: &DaemonSupervisorOwnerRecord) -> bool {
    left.token == right.token
        && left.generation == right.generation
        && left.pid == right.pid
        && left.process_start_id == right.process_start_id
        && left.socket_path == right.socket_path
}

fn owner_record_fingerprint(record: &DaemonSupervisorOwnerRecord) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_string(record).unwrap_or_default().as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn list_owner_directories(registry_dir: &str) -> Vec<String> {
    std::fs::read_dir(registry_dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| name.ends_with(".owner"))
                .map(|name| {
                    Path::new(registry_dir)
                        .join(&name)
                        .to_string_lossy()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn owner_directory_path(registry_dir: &str, generation: &str) -> Result<String, String> {
    if !generation
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
    {
        return Err(format!("Invalid daemon supervisor generation: {generation}"));
    }
    Ok(Path::new(registry_dir)
        .join(format!("{generation}.owner"))
        .to_string_lossy()
        .to_string())
}

fn require_owner_record(directory: &str) -> Result<DaemonSupervisorOwnerRecord, String> {
    read_owner_record(directory).ok_or_else(|| format!("Invalid daemon supervisor owner record: {directory}"))
}

fn read_owner_record_for_scope(
    directory: &str,
    is_relevant: &dyn Fn(&DaemonSupervisorOwnerScope) -> bool,
) -> Result<Option<DaemonSupervisorOwnerRecord>, String> {
    if let Some(owner) = read_owner_record(directory) {
        return Ok(Some(owner));
    }
    let scope = read_owner_scope(directory);
    let entries: Vec<String> = if scope.is_none() {
        std::fs::read_dir(directory)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if scope.is_none()
        && !entries.iter().any(|name| name == "owner.json")
        && !entries.iter().any(|name| name == "scope.json")
    {
        let abandoned = format!("{directory}.abandoned-{}", uuid::Uuid::new_v4());
        let _ = std::fs::rename(directory, &abandoned);
        let _ = std::fs::remove_dir_all(&abandoned);
        return Ok(None);
    }
    match scope {
        Some(scope) if !is_relevant(&scope) => Ok(None),
        _ => Err(format!("Invalid daemon supervisor owner record: {directory}")),
    }
}

fn read_owner_record(directory: &str) -> Option<DaemonSupervisorOwnerRecord> {
    let contents = std::fs::read_to_string(Path::new(directory).join("owner.json")).ok()?;
    let value: Value = serde_json::from_str(&contents).ok()?;
    if !is_daemon_supervisor_owner_record(&value) {
        return None;
    }
    serde_json::from_value(value).ok()
}

pub fn is_daemon_supervisor_owner_record(value: &Value) -> bool {
    let Some(record) = value.as_object() else {
        return false;
    };
    if record.get("version").and_then(Value::as_u64) != Some(OWNER_VERSION as u64) {
        return false;
    }
    if record.get("role").and_then(Value::as_str) != Some("supervisor") {
        return false;
    }
    if record.get("token").and_then(Value::as_str).is_none() {
        return false;
    }
    if record.get("generation").and_then(Value::as_str).is_none() {
        return false;
    }
    match record.get("pid").and_then(Value::as_i64) {
        Some(pid) if pid > 0 => {}
        _ => return false,
    }
    if let Some(process_start_id) = record.get("processStartId") {
        if process_start_id.as_str().is_none() {
            return false;
        }
    }
    for key in ["socketPath", "descriptorDir", "agentDir", "appVersion"] {
        if record.get(key).and_then(Value::as_str).is_none() {
            return false;
        }
    }
    match record.get("phase").and_then(Value::as_str) {
        Some(OWNER_PHASE_STARTING) | Some(OWNER_PHASE_OWNER) | Some(OWNER_PHASE_STOPPING) => {}
        _ => return false,
    }
    record.get("createdAt").and_then(Value::as_str).is_some()
        && record.get("updatedAt").and_then(Value::as_str).is_some()
}

fn read_owner_scope(directory: &str) -> Option<DaemonSupervisorOwnerScope> {
    let contents = std::fs::read_to_string(Path::new(directory).join("scope.json")).ok()?;
    let value: Value = serde_json::from_str(&contents).ok()?;
    if !is_daemon_supervisor_owner_scope(&value) {
        return None;
    }
    let scope: DaemonSupervisorOwnerScope = serde_json::from_value(value).ok()?;
    let parent = Path::new(directory)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_default();
    let expected = owner_directory_path(&parent, &scope.generation).ok()?;
    if expected == directory {
        Some(scope)
    } else {
        None
    }
}

pub fn is_daemon_supervisor_owner_scope(value: &Value) -> bool {
    let Some(scope) = value.as_object() else {
        return false;
    };
    if scope.get("version").and_then(Value::as_u64) != Some(OWNER_VERSION as u64) {
        return false;
    }
    if scope.get("role").and_then(Value::as_str) != Some("supervisor") {
        return false;
    }
    for key in ["token", "generation", "socketPath", "descriptorDir"] {
        if scope.get(key).and_then(Value::as_str).is_none() {
            return false;
        }
    }
    true
}

fn write_owner_scope(directory: &str, owner: &DaemonSupervisorOwnerRecord) -> Result<(), String> {
    let scope = owner.scope();
    write_json_atomically(
        &Path::new(directory).join("scope.json").to_string_lossy().to_string(),
        &serde_json::to_value(&scope).unwrap_or(Value::Null),
    )
}

fn write_owner_record(directory: &str, record: &DaemonSupervisorOwnerRecord) -> Result<(), String> {
    write_json_atomically(
        &Path::new(directory).join("owner.json").to_string_lossy().to_string(),
        &serde_json::to_value(record).unwrap_or(Value::Null),
    )
}

pub fn read_startup_fence(path: &str) -> Result<Option<DaemonStartupFenceRecord>, String> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let value: Value = serde_json::from_str(&contents).map_err(|error| error.to_string())?;
    let Some(fence) = value.as_object() else {
        return Err(format!("Invalid daemon startup fence: {path}"));
    };
    let valid = fence.get("version").and_then(Value::as_u64) == Some(OWNER_VERSION as u64)
        && fence.get("token").and_then(Value::as_str).is_some()
        && fence.get("ownerToken").and_then(Value::as_str).is_some()
        && fence.get("pid").and_then(Value::as_i64).is_some_and(|pid| pid > 0)
        && fence.get("processStartId").and_then(Value::as_str).is_some()
        && fence.get("socketPath").and_then(Value::as_str).is_some()
        && fence.get("supervisorGeneration").and_then(Value::as_str).is_some()
        && fence.get("createdAt").and_then(Value::as_str).is_some();
    if !valid {
        return Err(format!("Invalid daemon startup fence: {path}"));
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn read_active_shutdown_admission(registry_dir: &str) -> Option<DaemonShutdownAdmissionRecord> {
    let path = shutdown_admission_path(registry_dir);
    let admission = read_shutdown_admission(&path)?;
    if parse_iso_ms(&admission.expires_at) > now_ms() && is_process_identity_alive(&admission.identity()) {
        return Some(admission);
    }
    let _ = std::fs::remove_file(&path);
    None
}

pub fn read_shutdown_admission(path: &str) -> Option<DaemonShutdownAdmissionRecord> {
    let contents = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&contents).ok()?;
    let admission = value.as_object()?;
    let valid = admission.get("version").and_then(Value::as_u64) == Some(OWNER_VERSION as u64)
        && admission.get("token").and_then(Value::as_str).is_some()
        && admission.get("pid").and_then(Value::as_i64).is_some_and(|pid| pid > 0)
        && admission
            .get("processStartId")
            .map(|value| value.is_null() || value.as_str().is_some())
            .unwrap_or(true)
        && admission.get("createdAt").and_then(Value::as_str).is_some()
        && admission.get("updatedAt").and_then(Value::as_str).is_some()
        && admission
            .get("expiresAt")
            .and_then(Value::as_str)
            .is_some_and(|expires_at| parse_iso_ms(expires_at).is_finite());
    if !valid {
        return None;
    }
    serde_json::from_value(value).ok()
}

fn write_json_atomically(path: &str, value: &Value) -> Result<(), String> {
    let contents = format!("{}\n", serde_json::to_string_pretty(value).unwrap_or_default());
    write_file_atomic_sync(
        path,
        &contents,
        WriteFileAtomicOptions {
            mode: Some(0o600),
            fsync: false,
            fsync_dir: false,
        },
    )
    .map_err(|error| error.to_string())
}

pub fn startup_fence_path(directory: &str, socket_path: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(normalize_socket_path(socket_path, None).as_bytes());
    let key: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Path::new(directory)
        .join(format!("{key}.json"))
        .to_string_lossy()
        .to_string()
}

fn shutdown_admission_path(registry_dir: &str) -> String {
    Path::new(registry_dir)
        .join(SHUTDOWN_ADMISSION_FILE_NAME)
        .to_string_lossy()
        .to_string()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn iso_from_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn parse_iso_ms(value: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| time.timestamp_millis() as f64)
        .unwrap_or(f64::NAN)
}
