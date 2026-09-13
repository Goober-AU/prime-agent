//! Native transport/process adapter for daemon-supervisor.ts.
//!
//! Each resident root runs in a separately authenticated worker process. Public
//! clients share the supervisor connection, never the worker's secret or socket.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;
use crate::core::agent_session_config::{AgentSessionRuntimeConfig, durable_agent_session_runtime_config, merge_agent_session_runtime_config};
use crate::core::session_lease::{canonical_session_path, get_process_start_id};
use crate::utils::child_process::is_process_alive;
use crate::core::session_resolver::looks_like_session_path;
use crate::utils::atomic_file::{write_file_atomic_sync, remove_file_durably, RemoveFileDurablyOptions, WriteFileAtomicOptions};
use crate::utils::child_process::{signal_process_group_or_process, Signal};
use super::super::active_session_state::create_active_session_id;
use super::super::command_recovery_journal::{CommandRecoveryJournal, CommandJournalBeginResult};
use super::super::compact_session_stream::{CompactAssistantStreamReconstructor, CompactAssistantDelta};
use super::super::daemon_catalog_process::{DaemonCatalogClient, DAEMON_CATALOG_ROLE_ENV};
use super::super::agent_roster::{
    agent_roster_entry_to_value, classify_session_roster_status, is_session_summary_busy, AgentRosterStatus,
    passivated_worker_roster_entry, roster_agent_id_for_entry,
    worker_roster_entry_from_summary, AgentRoster, AgentRosterEntry, AgentRosterMutation,
    RegisteredHeartbeatFlags, RosterEntryMarks, RosterSessionSummary, RosterSummaryView, WorkerRosterEntry,
};
use super::super::daemon_client::DaemonClientRequestOptions;
use super::super::daemon_errors::DaemonSessionRecoveringError;
use super::super::daemon_protocol::{self, DaemonResponse};
use super::super::daemon_session_id::matches_session_id_suffix;
use super::super::daemon_session_list::{summary_for_inactive_session, SessionSummary};
use crate::core::session_manager::SessionInfo;
use crate::core::agent_messages::AgentFamilyCatalogEntry;
use super::super::rlm_ledger::{create_rlm_ledger_registry_seed_source, RlmLedgerEdge, RlmSpawnLedger};
use super::super::daemon_socket::*;
use super::super::daemon_supervisor_ownership::*;
use super::super::daemon_worker_client::{DaemonWorkerClient, PrivateFrame};
use super::super::daemon_worker_protocol::*;
use super::super::saved_session_info::serialize_saved_session_info;

const REQUEST_TIMEOUT: u64 = 24 * 60 * 60 * 1000;
const MAX_PUBLIC_LINE: usize = super::super::daemon_client::DAEMON_MAX_LINE_LENGTH;

/// `ROSTER_WATCHDOG_INTERVAL_MS` / `ROSTER_STALE_AFTER_MS` (daemon-supervisor.ts:194-195).
const ROSTER_WATCHDOG_INTERVAL_MS: u64 = 15_000;
const ROSTER_STALE_AFTER_MS: u64 = 3 * ROSTER_HEARTBEAT_INTERVAL_MS;

/// `SCHEDULED_WAKE_RETRY_MS` / `SCHEDULED_WAKE_MAX_TIMEOUT_MS` / `SCHEDULED_WAKE_CLIENT_ID`
/// (daemon-supervisor.ts:198-199 and daemon_supervisor.rs:140-142, which owns the values).
const SCHEDULED_WAKE_RETRY_MS: f64 = 60_000.0;
const SCHEDULED_WAKE_MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
const SCHEDULED_WAKE_CLIENT_ID: &str = "scheduled-wake";

/// `const WORKER_RETRY_DELAYS_MS = [250, 1000, 5000] as const` (daemon-supervisor.ts:210).
/// daemon_supervisor.rs also declares this ladder, but keeps it private; the close-time
/// recovery ladder in this file needs its own copy.
const WORKER_RETRY_DELAYS_MS: [u64; 3] = [250, 1000, 5000];

struct PublicClient {
    connection_id: String,
    id: Mutex<String>,
    protocol_id: Mutex<Option<String>>,
    subscriptions: Mutex<HashSet<String>>,
    supports_extension_ui: AtomicBool,
    pause_epoch: AtomicU64,
    output: mpsc::Sender<Vec<u8>>,
    stopped: CancellationToken,
    /// `client.rosterSubscribed` / `client.rosterResyncPending`.
    roster_subscribed: AtomicBool,
    roster_resync_pending: AtomicBool,
    /// `client.backpressured`: a resync is deferred while the writer is behind.
    backpressured: AtomicBool,
}
impl PublicClient {
    fn write(&self, value: &Value) -> bool {
        let Ok(mut bytes) = serde_json::to_vec(value) else { return false; };
        bytes.push(b'\n');
        match self.output.try_send(bytes) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => { self.backpressured.store(true, Ordering::SeqCst); false }
            Err(mpsc::error::TrySendError::Closed(_)) => { self.stopped.cancel(); false }
        }
    }
    fn identity(&self) -> String { self.protocol_id.lock().unwrap().clone().unwrap_or_else(|| self.id.lock().unwrap().clone()) }
}
struct Worker {
    descriptor: Mutex<DaemonWorkerDescriptor>,
    /// `ResidentWorker.client`: replaced by a reconnection, so it is not `Arc`-pinned.
    client: Mutex<Option<Arc<DaemonWorkerClient>>>,
    /// Bumped per applied roster frame; a summaries pull that straddles one must not gap-fill.
    roster_epoch: AtomicU64,
    /// `worker.rosterStale`: the watchdog marks rows whose worker stopped talking.
    roster_stale: AtomicBool,
    last_frame_at: Mutex<Option<u64>>,
    /// In-flight reconnection; an allowed frame source alongside `client`.
    pending_client: Mutex<Option<Arc<DaemonWorkerClient>>>,
    connection: AsyncMutex<()>,
    stream: Mutex<CompactAssistantStreamReconstructor>,
    /// `worker.recovery`: one recovery ladder per worker (daemon-supervisor.ts:3912, 4226-4228).
    recovery: AtomicBool,
    /// `worker.deferredRecovery` / `worker.deferredRecoveryRounds` (daemon-supervisor.ts:3960-3966).
    deferred_recovery: AtomicBool,
    deferred_recovery_rounds: AtomicU64,
}
#[derive(Clone)]
struct InputPause { connection_id: String, worker: Arc<Worker>, active: String, requested: String }
struct Supervisor {
    socket_path: String,
    descriptor_dir: PathBuf,
    config: AgentSessionRuntimeConfig,
    ownership: DaemonSupervisorOwnership,
    workers: Mutex<HashMap<String, Arc<Worker>>>,
    clients: Mutex<HashMap<String, Arc<PublicClient>>>,
    opening: AsyncMutex<()>,
    pauses: Mutex<HashMap<String, InputPause>>,
    journal: Mutex<CommandRecoveryJournal>,
    catalog: Arc<DaemonCatalogClient>,
    stopped: CancellationToken,
    /// `this.rosterStore`: the one supervisor-owned roster, lazily created.
    roster: Mutex<Option<Arc<Mutex<AgentRoster>>>>,
    /// `pendingRosterChanged` / `pendingRosterRemoved` / `publishedRosterIds`.
    pending_roster_changed: Mutex<HashSet<String>>,
    pending_roster_removed: Mutex<HashSet<String>>,
    published_roster_ids: Mutex<HashSet<String>>,
    roster_push_scheduled: AtomicBool,
    /// `this.rlmSpawnLedgerInstance`, a process-wide OnceLock so every clone shares one.
    ledger: Arc<tokio::sync::OnceCell<Arc<RlmSpawnLedger>>>,
    /// `this.scheduledWakeTimer` / `this.scheduledWakeRecompute` / `scheduledWakeRecomputeQueued`
    /// (daemon-supervisor.ts:775-778, 942-1055).
    scheduled_wake_timer: Mutex<Option<tokio::task::JoinHandle<()>>>,
    scheduled_wake_recompute: AtomicBool,
    scheduled_wake_recompute_queued: AtomicBool,
    /// `this.scheduledWakeFailures`: the failure floor per passive root.
    scheduled_wake_failures: Mutex<HashMap<String, f64>>,
}

pub(crate) async fn run_daemon_supervisor_mode(socket_path: Option<String>, mut config: AgentSessionRuntimeConfig) -> Result<(), String> {
    let socket_path = normalize_socket_path_for_daemon(&socket_path.unwrap_or_else(default_daemon_socket_path), None);
    let agent_dir = config.agent_dir.clone().ok_or("Daemon supervisor config is missing agentDir")?;
    let descriptor_dir = Path::new(&agent_dir).join("daemon-workers").join(descriptor_key(&socket_path));
    if let Ok(bytes) = std::fs::read(descriptor_dir.join("supervisor-config")) {
        let persisted: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if persisted.get("version").and_then(Value::as_u64) == Some(1) && persisted.get("socketPath").and_then(Value::as_str) == Some(&socket_path) {
            let durable = serde_json::from_value::<crate::core::agent_session_config::DurableAgentSessionRuntimeConfig>(persisted.get("defaultSessionConfig").cloned().unwrap_or(Value::Null)).map_err(|error| error.to_string())?;
            let previous: AgentSessionRuntimeConfig = serde_json::from_value(serde_json::to_value(durable).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
            config = merge_agent_session_runtime_config(&config, Some(&previous));
        }
    }
    let lease = acquire_daemon_socket_path_lease(&socket_path).await;
    #[cfg(unix)]
    if lease.is_none() { return Err(format!("Could not acquire daemon socket lease: {socket_path}")); }
    let ownership = match async {
        wait_for_daemon_startup_fence(&socket_path, 120_000, None).await?;
        acquire_daemon_supervisor_ownership(AcquireDaemonSupervisorOwnershipOptions {
            socket_path: socket_path.clone(), descriptor_dir: descriptor_dir.to_string_lossy().into_owned(),
            agent_dir, generation: uuid::Uuid::new_v4().to_string(), app_version: crate::config::VERSION.to_string(), registry_dir: None,
        }).await
    }.await {
        Ok(owner) => owner,
        Err(error) => { if let Some(lease) = lease { lease.release().await; } return Err(error); }
    };
    let journal = match CommandRecoveryJournal::new(&descriptor_dir.join("command-journal.jsonl").to_string_lossy()) {
        Ok(journal) => journal,
        Err(error) => { let _ = ownership.release().await; if let Some(lease) = lease { lease.release().await; } return Err(error); }
    };
    let supervisor = Arc::new(Supervisor {
        socket_path: socket_path.clone(), journal: Mutex::new(journal),
        descriptor_dir, config, ownership, workers: Mutex::new(HashMap::new()), clients: Mutex::new(HashMap::new()), opening: AsyncMutex::new(()), pauses: Mutex::new(HashMap::new()),
        catalog: Arc::new(DaemonCatalogClient::new(Arc::new(|message| eprintln!("Daemon catalog: {message}")))), stopped: CancellationToken::new(),
        roster: Mutex::new(None), pending_roster_changed: Mutex::new(HashSet::new()), pending_roster_removed: Mutex::new(HashSet::new()),
        published_roster_ids: Mutex::new(HashSet::new()), roster_push_scheduled: AtomicBool::new(false),
        ledger: Arc::new(tokio::sync::OnceCell::new()),
        scheduled_wake_timer: Mutex::new(None), scheduled_wake_recompute: AtomicBool::new(false),
        scheduled_wake_recompute_queued: AtomicBool::new(false), scheduled_wake_failures: Mutex::new(HashMap::new()),
    });
    // The TS store is installed lazily by `roster()`; the port installs it here
    // because its mutation sink needs a `Weak` to the finished `Arc`.
    supervisor.init_roster();
    let mut identity = None;
    let result = async {
        prepare_daemon_socket_path(&socket_path, lease.clone()).await.map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&supervisor.descriptor_dir).map_err(|error| error.to_string())?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&supervisor.descriptor_dir, std::fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())?;
        }
        persist_json(&supervisor.descriptor_dir.join("supervisor-config"), &json!({
            "version": 1, "socketPath": socket_path, "defaultSessionConfig": durable_agent_session_runtime_config(&supervisor.config),
        }))?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut environment: Vec<(String, String)> = std::env::vars().collect();
        environment.retain(|(key, _)| key != DAEMON_WORKER_ROLE_ENV && key != DAEMON_WORKER_TOKEN_ENV);
        environment.push((DAEMON_CATALOG_ROLE_ENV.to_string(), "1".to_string()));
        supervisor.catalog.start(&executable.to_string_lossy(), vec![], environment).await?;
        supervisor.adopt_workers().await?;
        supervisor.seed_roster_ledger().await;
        supervisor.start_roster_watchdog();
        // `this.scheduleScheduledSessionWakeRecompute()` on startup (daemon-supervisor.ts:883).
        supervisor.schedule_scheduled_session_wake_recompute();
        #[cfg(unix)]
        let listener = tokio::net::UnixListener::bind(&socket_path).map_err(|error| error.to_string())?;
        #[cfg(windows)]
        let mut listener = tokio::net::windows::named_pipe::ServerOptions::new().first_pipe_instance(true).create(&socket_path).map_err(|error| error.to_string())?;
        identity = get_daemon_socket_identity(&socket_path);
        restrict_daemon_socket_path(&socket_path);
        supervisor.ownership.update_phase("owner").await?;
        supervisor.register_signals();
        eprintln!("Prime Agent daemon supervisor {} listening on {}", supervisor.ownership.snapshot().generation, socket_path);
        loop {
            #[cfg(unix)] {
                let accepted = tokio::select! { _ = supervisor.stopped.cancelled() => break, result = listener.accept() => result };
                let (stream, _) = accepted.map_err(|error| error.to_string())?;
                spawn_connection(supervisor.clone(), stream);
            }
            #[cfg(windows)] {
                tokio::select! { _ = supervisor.stopped.cancelled() => break, result = listener.connect() => result.map_err(|error| error.to_string())? };
                let next = tokio::net::windows::named_pipe::ServerOptions::new().create(&socket_path).map_err(|error| error.to_string())?;
                spawn_connection(supervisor.clone(), listener);
                listener = next;
            }
        }
        Ok(())
    }.await;
    supervisor.stopped.cancel();
    for client in supervisor.clients.lock().unwrap().values() { client.stopped.cancel(); }
    let workers: Vec<_> = supervisor.workers.lock().unwrap().values().cloned().collect();
    for worker in workers {
        let client = { worker.client.lock().unwrap().clone() };
        if let Some(client) = client { client.close().await; }
    }
    supervisor.catalog.stop().await;
    if identity.is_some() { cleanup_daemon_socket_path(&socket_path, identity, lease.as_deref()); }
    let release = supervisor.ownership.release().await;
    if let Some(lease) = lease { lease.release().await; }
    result.and(release)
}

impl Supervisor {
    fn register_signals(self: &Arc<Self>) {
        #[cfg(unix)]
        for kind in [tokio::signal::unix::SignalKind::interrupt(), tokio::signal::unix::SignalKind::terminate(), tokio::signal::unix::SignalKind::hangup()] {
            if let Ok(mut signal) = tokio::signal::unix::signal(kind) {
                let stopped = self.stopped.clone();
                tokio::spawn(async move { tokio::select! { _ = stopped.cancelled() => {}, _ = signal.recv() => stopped.cancel() } });
            }
        }
        #[cfg(windows)] {
            let stopped = self.stopped.clone();
            tokio::spawn(async move { tokio::select! { _ = stopped.cancelled() => {}, _ = tokio::signal::ctrl_c() => stopped.cancel() } });
        }
    }
    fn hello(&self, client: &PublicClient) -> Value {
        let owner = self.ownership.snapshot();
        json!({"type":"daemon_hello", "socketPath":self.socket_path,
            "protocol":daemon_protocol::daemon_protocol_info(), "schemaId":daemon_protocol::DAEMON_SCHEMA_ID,
            "schemaRevision":daemon_protocol::DAEMON_SCHEMA_REVISION, "appVersion":crate::config::VERSION,
            "runtime":super::super::daemon_runtime_identity::get_daemon_runtime_identity_from_process(),
            "supervisorGeneration":owner.generation, "supervisorOwnerToken":owner.token, "supervisorPid":owner.pid,
            "supervisorProcessStartId":owner.process_start_id, "supervisorSocketPath":owner.socket_path,
            "clientId":client.identity(), "serverCapabilities":server_capabilities()})
    }
    fn persist_worker(&self, descriptor: &DaemonWorkerDescriptor) -> Result<(), String> {
        persist_json(&self.descriptor_dir.join(format!("{}.json", descriptor.worker_id)), &serde_json::to_value(durable_daemon_worker_descriptor(descriptor)).map_err(|error| error.to_string())?)
    }
    async fn authenticate(&self, client: &Arc<DaemonWorkerClient>, descriptor: &DaemonWorkerDescriptor) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(super::WORKER_CONNECT_TIMEOUT_MS);
        loop {
            self.ownership.assert_current().await.map_err(|error| error.to_string())?;
            match client.connect(super::WORKER_CONNECT_PROBE_MS).await {
                Ok(()) => break,
                Err(error) if tokio::time::Instant::now() >= deadline => return Err(error.to_string()),
                Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
        client.wait_for_hello(super::WORKER_CONNECT_TIMEOUT_MS).await.map_err(|error| error.to_string())?;
        let owner = self.ownership.snapshot();
        let mut claim = vec![("supervisorGeneration", json!(owner.generation)), ("supervisorPid", json!(owner.pid)), ("supervisorSocketPath", json!(owner.socket_path))];
        if let Some(start) = owner.process_start_id { claim.push(("supervisorProcessStartId", json!(start))); }
        let response = client.authenticate_worker(&descriptor.authentication_token, &claim, super::WORKER_CONNECT_TIMEOUT_MS).await.map_err(|error| error.to_string())?;
        // `workerAuthAdvertisesRoster`: a worker that predates the roster protocol
        // must be restarted, never silently treated as roster-resident.
        if !worker_auth_advertises_roster(response.data.as_ref()) {
            return Err(format!("Session worker {} predates the roster protocol and must be restarted", descriptor.worker_id));
        }
        Ok(())
    }
    /// `installWorker(worker)` plus `connectWorker`'s listeners, which are
    /// registered BEFORE authentication so the worker's first roster snapshot
    /// cannot race them (daemon-supervisor.ts:3610).
    fn install_worker(self: &Arc<Self>, descriptor: DaemonWorkerDescriptor, client: Arc<DaemonWorkerClient>) -> Arc<Worker> {
        let worker = Arc::new(Worker {
            descriptor: Mutex::new(descriptor), client: Mutex::new(Some(Arc::clone(&client))), roster_epoch: AtomicU64::new(0),
            roster_stale: AtomicBool::new(false), last_frame_at: Mutex::new(None), pending_client: Mutex::new(None),
            connection: AsyncMutex::new(()), stream: Mutex::new(CompactAssistantStreamReconstructor::new()),
            recovery: AtomicBool::new(false), deferred_recovery: AtomicBool::new(false), deferred_recovery_rounds: AtomicU64::new(0),
        });
        self.attach_worker_listeners(&worker, &client);
        self.workers.lock().unwrap().insert(worker.descriptor.lock().unwrap().worker_id.clone(), Arc::clone(&worker));
        worker
    }
    fn attach_worker_listeners(self: &Arc<Self>, worker: &Arc<Worker>, client: &Arc<DaemonWorkerClient>) {
        let weak = Arc::downgrade(self);
        let weak_worker = Arc::downgrade(worker);
        let weak_source = Arc::downgrade(client);
        let _unsubscribe = client.on_frame(Arc::new(move |frame| {
            if let (Some(supervisor), Some(worker), Some(source)) = (weak.upgrade(), weak_worker.upgrade(), weak_source.upgrade()) {
                supervisor.handle_worker_frame(&worker, frame, Some(&source));
            }
        }));
        let weak = Arc::downgrade(self);
        let weak_worker = Arc::downgrade(worker);
        let weak_source = Arc::downgrade(client);
        let _close = client.on_close(Arc::new(move |error| {
            if let (Some(supervisor), Some(worker), Some(source)) = (weak.upgrade(), weak_worker.upgrade(), weak_source.upgrade()) {
                let message = error.message();
                tokio::spawn(async move { supervisor.handle_worker_close(&worker, &source, &message).await; });
            }
        }));
        let _ = (_unsubscribe, _close);
    }
    /// `handleWorkerFrame(worker, frame, source)`.
    fn handle_worker_frame(self: &Arc<Self>, worker: &Arc<Worker>, frame: &PrivateFrame, source: Option<&Arc<DaemonWorkerClient>>) {
        if frame.header.get("kind").and_then(Value::as_str) != Some("outbound") { return; }
        if let Some(source) = source {
            let current = worker.client.lock().unwrap().as_ref().map(Arc::as_ptr);
            let pending = worker.pending_client.lock().unwrap().as_ref().map(Arc::as_ptr);
            if current != Some(Arc::as_ptr(source)) && pending != Some(Arc::as_ptr(source)) { return; }
        }
        *worker.last_frame_at.lock().unwrap() = Some(supervisor_now_ms());
        self.clear_roster_staleness(worker);
        match frame.header.get("outboundType").and_then(Value::as_str).unwrap_or("") {
            "roster_delta" => { self.consume_worker_roster_delta(worker, &frame.payload); return; }
            "roster_heartbeat" => return,
            // daemon-supervisor.ts:5897-5901. TS also records `worker.heartbeatSnapshotStale`
            // and calls `broadcastHeartbeatsChanged()` (7109-7114), which fans out to every
            // client. Both are withheld here on purpose: this supervisor never advertises
            // `heartbeat_catalog` (see `server_capabilities`), and
            // `DAEMON_OUTBOUND_COMPATIBILITY` gates `heartbeats_changed` on exactly that
            // capability (daemon-protocol.ts:1258), so a client must not act on the frame. The
            // frame is consumed rather than forwarded: letting it fall through `forward_frame`
            // would feed a non-event payload into the assistant-stream reconstructor.
            "heartbeats_changed" => return,
            _ => {}
        }
        self.forward_frame(worker, frame);
    }
    /// `handleWorkerClose(worker, client, error)` (daemon-supervisor.ts:3835-3888).
    ///
    /// A dead connection makes the worker's own rows non-live, and then the close
    /// itself drives recovery: mark `recovering` (3872), persist the descriptor
    /// transition with the close error (3884-3887) and hand off to the retry
    /// ladder. Without that hand-off every session of a crashed worker stays stuck
    /// on the typed "is recovering; retry shortly" error forever, because
    /// `recovery_command` was reachable only from `adopt_workers` and `retry_worker`.
    async fn handle_worker_close(self: &Arc<Self>, worker: &Arc<Worker>, client: &Arc<DaemonWorkerClient>, error: &str) {
        {
            let mut current = worker.client.lock().unwrap();
            if current.as_ref().map(Arc::as_ptr) != Some(Arc::as_ptr(client)) { return; }
            *current = None;
        }
        if self.stopped.is_cancelled() { return; }
        self.mark_worker_roster_entries(worker, Some(DAEMON_WORKER_LIFECYCLE_RECOVERING));
        // `isWorkerRecoveryEligible` (3881, 3923-3925): no concurrent ladder may be
        // admitted for the same worker.
        if !self.is_worker_recovery_eligible(worker) { return; }
        {
            let mut descriptor = worker.descriptor.lock().unwrap();
            descriptor.lifecycle = DAEMON_WORKER_LIFECYCLE_RECOVERING.into();
            descriptor.last_error = Some(error.to_string());
            let persisted = descriptor.clone();
            drop(descriptor);
            if let Err(persist_error) = self.persist_worker(&persisted) { eprintln!("Could not persist worker descriptor {}: {persist_error}", persisted.worker_id); }
        }
        // `persistAndRecoverWorker(worker, recoveryTransition, false)` (3887): the
        // ladder is admitted (and its flag published) before the await returns.
        let _ = self.persist_and_recover_worker(worker, false).await;
    }
    /// `isWorkerRecoveryCandidate(worker)` (daemon-supervisor.ts:3949-3957).
    fn is_worker_recovery_candidate(&self, worker: &Arc<Worker>) -> bool {
        let descriptor = worker.descriptor.lock().unwrap();
        !self.stopped.is_cancelled()
            && descriptor.lifecycle != DAEMON_WORKER_LIFECYCLE_STOPPING
            && descriptor.stop_requested_at.is_none()
            && self.workers.lock().unwrap().get(&descriptor.worker_id).is_some_and(|resident| Arc::ptr_eq(resident, worker))
            && worker.client.lock().unwrap().is_none()
    }
    /// `isWorkerRecoveryEligible(worker)` (daemon-supervisor.ts:3923-3925).
    fn is_worker_recovery_eligible(&self, worker: &Arc<Worker>) -> bool {
        self.is_worker_recovery_candidate(worker) && !worker.recovery.load(Ordering::SeqCst)
    }
    /// `persistAndRecoverWorker(worker, transition, waitForRecovery)` (daemon-supervisor.ts:3890-3921).
    /// The ladder flag is published before any await, so concurrent touches join it.
    async fn persist_and_recover_worker(self: &Arc<Self>, worker: &Arc<Worker>, wait_for_recovery: bool) -> Result<(), String> {
        // Joining an admitted ladder (`await worker.recovery`, daemon-supervisor.ts:3913-3914)
        // needs a shared in-flight handle; the flag below is the admission gate, so a second
        // caller returns instead of starting a competing ladder.
        if worker.recovery.swap(true, Ordering::SeqCst) {
            let _ = wait_for_recovery;
            return Ok(());
        }
        let result = self.recover_worker(worker).await;
        worker.recovery.store(false, Ordering::SeqCst);
        result
    }
    /// `recoverWorker(worker)` (daemon-supervisor.ts:4222-4361): the close-time
    /// retry ladder over `WORKER_RETRY_DELAYS_MS = [250, 1000, 5000]` (:210).
    async fn recover_worker(self: &Arc<Self>, worker: &Arc<Worker>) -> Result<(), String> {
        if self.is_worker_recovery_cancelled(worker) { return Ok(()); }
        if self.ownership.assert_current().await.is_err() { return Ok(()); }
        let descriptor_worker_id = worker.descriptor.lock().unwrap().worker_id.clone();
        // `let keepProbingLiveWorker = false` (daemon-supervisor.ts:4237). TS re-initialises it at
        // the top of every delay round (:4240); here every path that does not return sets it, so
        // only the last round's verdict reaches the post-loop decision, exactly as in TS.
        let mut keep_probing_live_worker = false;
        for retry_delay in WORKER_RETRY_DELAYS_MS {
            tokio::time::sleep(Duration::from_millis(retry_delay)).await;
            if self.is_worker_recovery_cancelled(worker) { return Ok(()); }
            if self.ownership.assert_current().await.is_err() { return Ok(()); }
            let descriptor = worker.descriptor.lock().unwrap().clone();
            let identity = ProcessIdentity { pid: descriptor.pid as i64, process_start_id: descriptor.process_start_id.clone() };
            // `processIdentity(pid, processStartId)` (daemon-supervisor.ts:4246, 4287-4292): the
            // three verdicts are distinguished so "unknown" keeps probing and "replaced" does not.
            let verdict = process_identity_verdict(&identity);
            let identity_compatible = verdict != ProcessIdentityVerdict::Gone && matches_exact_process_identity(&identity);
            if identity_compatible {
                match self.reconnect_worker(worker).await {
                    Ok(()) => {
                        if self.is_worker_recovery_cancelled(worker) { return Ok(()); }
                        {
                            let mut current = worker.descriptor.lock().unwrap();
                            current.lifecycle = DAEMON_WORKER_LIFECYCLE_READY.into();
                            current.consecutive_failures = 0;
                            let ready = current.clone();
                            drop(current);
                            if let Err(error) = self.persist_worker(&ready) { eprintln!("Could not persist worker descriptor {}: {error}", ready.worker_id); }
                        }
                        worker.deferred_recovery_rounds.store(0, Ordering::SeqCst);
                        return Ok(());
                    }
                    Err(error) => {
                        // daemon-supervisor.ts:4279-4284: a worker that keeps the same
                        // durable identity may still be load-slow, so keep probing it
                        // instead of replacing live work after a timeout.
                        keep_probing_live_worker = true;
                        {
                            let mut current = worker.descriptor.lock().unwrap();
                            current.consecutive_failures += 1;
                            current.last_failure_at = Some(chrono::Utc::now().to_rfc3339());
                            current.last_error = Some(error.clone());
                            let failed = current.clone();
                            drop(current);
                            if let Err(persist_error) = self.persist_worker(&failed) { eprintln!("Could not persist worker descriptor {}: {persist_error}", failed.worker_id); }
                        }
                    }
                }
                continue;
            }
            // daemon-supervisor.ts:4230-4236: a client-owned worker whose process is gone and
            // that has no stored launch environment waits for its owner, never for a relaunch.
            if verdict == ProcessIdentityVerdict::Gone && descriptor.owner_client_id.is_some() {
                let mut current = worker.descriptor.lock().unwrap();
                current.lifecycle = DAEMON_WORKER_LIFECYCLE_FAILED.into();
                current.last_error = Some("Waiting for the owning client to reconnect".to_string());
                let failed = current.clone();
                drop(current);
                if let Err(error) = self.persist_worker(&failed) { eprintln!("Could not persist worker descriptor {}: {error}", failed.worker_id); }
                return Err("Waiting for the owning client to reconnect".to_string());
            }
            // daemon-supervisor.ts:4287-4292: a live pid whose identity cannot be verified
            // ("unknown") keeps probing instead of being replaced.
            if verdict == ProcessIdentityVerdict::Unknown {
                keep_probing_live_worker = true;
                continue;
            }
            // daemon-supervisor.ts:4293-4302: the identity is gone and no client-owned launch
            // context is stored here, so the worker parks failed with the TS message. Recovery
            // from there is the shipped reclaim path (`create` / `retry_worker`), never a
            // silent relaunch from the close handler.
            //
            // blocked_on: the TS line above relaunches only a client-owned worker that carries
            // BOTH `transientCreateCommand` and `launchEnv`. Neither exists in
            // `DaemonWorkerDescriptor`, and the capability that would advertise that flow
            // (`owned_session_recovery_context`, daemon-supervisor.ts:1370-1375) is deliberately
            // unadvertised by this supervisor, so the launch half stays unported on purpose.
            {
                let mut current = worker.descriptor.lock().unwrap();
                current.lifecycle = DAEMON_WORKER_LIFECYCLE_FAILED.into();
                current.last_error = Some("Waiting for a client with fresh runtime context".to_string());
                let failed = current.clone();
                drop(current);
                if let Err(error) = self.persist_worker(&failed) { eprintln!("Could not persist worker descriptor {}: {error}", failed.worker_id); }
            }
            self.mark_worker_roster_entries(worker, Some(DAEMON_WORKER_LIFECYCLE_FAILED));
            return Ok(());
        }
        if self.is_worker_recovery_cancelled(worker) { return Ok(()); }
        if keep_probing_live_worker {
            // daemon-supervisor.ts:4329-4343 -> `deferWorkerRecovery` (3959-3981): stay in
            // `recovering` and re-probe every `DEFERRED_RECOVERY_RECHECK_MS`, parking failed
            // only after `MAX_DEFERRED_RECOVERY_ROUNDS` so a silent worker is never killed.
            {
                let mut current = worker.descriptor.lock().unwrap();
                current.lifecycle = DAEMON_WORKER_LIFECYCLE_RECOVERING.into();
                let recovering = current.clone();
                drop(current);
                if let Err(error) = self.persist_worker(&recovering) { eprintln!("Could not persist worker descriptor {}: {error}", recovering.worker_id); }
            }
            let rounds = worker.deferred_recovery_rounds.fetch_add(1, Ordering::SeqCst) + 1;
            if rounds > MAX_DEFERRED_RECOVERY_ROUNDS {
                let mut current = worker.descriptor.lock().unwrap();
                current.lifecycle = DAEMON_WORKER_LIFECYCLE_FAILED.into();
                current.last_error = Some(format!("Live session worker did not answer recovery probes for {MAX_DEFERRED_RECOVERY_ROUNDS} rounds"));
                let failed = current.clone();
                drop(current);
                if let Err(error) = self.persist_worker(&failed) { eprintln!("Could not persist worker descriptor {}: {error}", failed.worker_id); }
                self.mark_worker_roster_entries(worker, Some(DAEMON_WORKER_LIFECYCLE_FAILED));
                eprintln!("Worker {} is unresponsive; parked failed after {MAX_DEFERRED_RECOVERY_ROUNDS} probe rounds", descriptor_worker_id);
                return Ok(());
            }
            self.schedule_deferred_worker_recovery(worker);
            return Ok(());
        }
        // daemon-supervisor.ts:4351-4358: the worker's process is gone and no client
        // launch context exists, so it parks failed; `create` / `retry_worker` reclaim it.
        {
            let mut current = worker.descriptor.lock().unwrap();
            current.lifecycle = DAEMON_WORKER_LIFECYCLE_FAILED.into();
            let failed = current.clone();
            drop(current);
            if let Err(error) = self.persist_worker(&failed) { eprintln!("Could not persist worker descriptor {}: {error}", failed.worker_id); }
        }
        self.mark_worker_roster_entries(worker, Some(DAEMON_WORKER_LIFECYCLE_FAILED));
        eprintln!("Worker {descriptor_worker_id} failed after three recovery attempts");
        Ok(())
    }
    /// `resumeDeferredWorkerRecovery(worker, error)` (daemon-supervisor.ts:3983-4020): probe
    /// again after `DEFERRED_RECOVERY_RECHECK_MS` while the worker stays a candidate.
    fn schedule_deferred_worker_recovery(self: &Arc<Self>, worker: &Arc<Worker>) {
        if worker.deferred_recovery.swap(true, Ordering::SeqCst) { return; }
        let supervisor = Arc::clone(self);
        let worker = Arc::clone(worker);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(DEFERRED_RECOVERY_RECHECK_MS)).await;
            worker.deferred_recovery.store(false, Ordering::SeqCst);
            if !supervisor.is_worker_recovery_candidate(&worker) { return; }
            if !supervisor.is_worker_recovery_eligible(&worker) { return; }
            if supervisor.ownership.assert_current().await.is_err() { return; }
            let _ = supervisor.persist_and_recover_worker(&worker, false).await;
        });
    }
    /// `isWorkerRecoveryCancelled(worker)` (daemon-supervisor.ts:4365-4372).
    fn is_worker_recovery_cancelled(&self, worker: &Arc<Worker>) -> bool {
        if self.stopped.is_cancelled() { return true; }
        let descriptor = worker.descriptor.lock().unwrap();
        descriptor.lifecycle == DAEMON_WORKER_LIFECYCLE_STOPPING
            || descriptor.stop_requested_at.is_some()
            || !self.workers.lock().unwrap().get(&descriptor.worker_id).is_some_and(|resident| Arc::ptr_eq(resident, worker))
    }
    /// `connectWorker(worker, WORKER_CONNECT_TIMEOUT_MS)` + `subscribeWorker` +
    /// `refreshWorkerSummaries(worker, true)` (daemon-supervisor.ts:4252-4254).
    async fn reconnect_worker(self: &Arc<Self>, worker: &Arc<Worker>) -> Result<(), String> {
        let descriptor = worker.descriptor.lock().unwrap().clone();
        let client = Arc::new(DaemonWorkerClient::new(&descriptor.socket_path));
        self.attach_worker_listeners(worker, &client);
        *worker.pending_client.lock().unwrap() = Some(Arc::clone(&client));
        let result = self.authenticate(&client, &descriptor).await;
        *worker.pending_client.lock().unwrap() = None;
        if let Err(error) = result { client.close_now(); return Err(error); }
        *worker.client.lock().unwrap() = Some(Arc::clone(&client));
        self.subscribe(worker, &descriptor.root_active_session_id).await?;
        self.refresh(worker).await?;
        Ok(())
    }
    fn forward_frame(&self, worker: &Worker, frame: &PrivateFrame) {
        let kind = frame.header.get("outboundType").and_then(Value::as_str).unwrap_or("");
        if matches!(kind, "daemon_hello" | "response") { return; }
        let Ok(mut value) = serde_json::from_slice::<Value>(&frame.payload) else { return; };
        if frame.header.get("payloadEncoding").and_then(Value::as_str) == Some("assistant-delta") {
            let Ok(delta) = serde_json::from_value::<CompactAssistantDelta>(value) else { return; };
            let Some(reconstructed) = worker.stream.lock().unwrap().reconstruct(&delta) else { return; };
            value = reconstructed;
        } else { worker.stream.lock().unwrap().observe(&value); }
        if let Some(summary) = value.get("state").filter(|value| value.get("sessionId").is_some()).and_then(|value| serde_json::from_value::<RosterSessionSummary>(value.clone()).ok()) {
            value["state"] = serde_json::to_value(self.public_summary(worker, session_summary_from_roster_row(&summary, None, None, None))).unwrap_or(Value::Null);
        }
        let active = frame.header.get("activeSessionId").and_then(Value::as_str).or_else(|| value.get("activeSessionId").and_then(Value::as_str));
        let owner = worker.descriptor.lock().unwrap().owner_client_id.clone();
        for client in self.clients.lock().unwrap().values() {
            if owner.as_ref().is_some_and(|owner| owner != &client.identity()) { continue; }
            if active.is_some_and(|id| client.subscriptions.lock().unwrap().contains(id)) { client.write(&value); }
        }
    }
    /// `refreshWorkerSummaries(worker, recovery, fillGaps)`.
    async fn refresh(self: &Arc<Self>, worker: &Arc<Worker>) -> Result<Vec<Value>, String> {
        let client = self.connected_client(worker).await?;
        let response = client.request_worker(command("list"), REQUEST_TIMEOUT).await.map_err(|error| error.to_string())?;
        let sessions = session_summaries_from_response(response)?;
        for summary in &sessions {
            self.write_roster_entry(worker_roster_entry_from_summary(summary), Some(worker), None);
        }
        Ok(sessions.iter().map(|summary| serde_json::to_value(summary).unwrap_or(Value::Null)).collect())
    }
    /// `this.requireWorkerClient(worker)`: reconnects a worker whose connection
    /// died, after proving the pid is still this worker's own process.
    async fn connected_client(self: &Arc<Self>, worker: &Arc<Worker>) -> Result<Arc<DaemonWorkerClient>, String> {
        if let Some(client) = worker.client.lock().unwrap().as_ref().cloned() {
            if client.is_connected() { return Ok(client); }
        }
        let _connection = worker.connection.lock().await;
        if let Some(client) = worker.client.lock().unwrap().as_ref().cloned() {
            if client.is_connected() { return Ok(client); }
        }
        let descriptor = worker.descriptor.lock().unwrap().clone();
        if !matches_exact_process_identity(&ProcessIdentity { pid: descriptor.pid as i64, process_start_id: descriptor.process_start_id.clone() }) {
            return Err(format!("Session worker {} exited; retry_worker is required", descriptor.worker_id));
        }
        let client = Arc::new(DaemonWorkerClient::new(&descriptor.socket_path));
        self.attach_worker_listeners(worker, &client);
        *worker.pending_client.lock().unwrap() = Some(Arc::clone(&client));
        let result = self.authenticate(&client, &descriptor).await;
        *worker.pending_client.lock().unwrap() = None;
        match result {
            Ok(()) => {
                *worker.client.lock().unwrap() = Some(Arc::clone(&client));
                self.subscribe(worker, &descriptor.root_active_session_id).await?;
                Ok(client)
            }
            Err(error) => { client.close_now(); Err(error) }
        }
    }
    async fn subscribe(&self, worker: &Arc<Worker>, active: &str) -> Result<(), String> {
        let client = { worker.client.lock().unwrap().as_ref().cloned().ok_or("Session worker is not connected")? };
        let mut body = command("worker_subscribe");
        body.insert("activeSessionId".into(), json!(active));
        // Full snapshots avoid a private chunk cache at the public boundary.
        let supports_ui = self.clients.lock().unwrap().values().any(|client| client.subscriptions.lock().unwrap().contains(active) && client.supports_extension_ui.load(Ordering::SeqCst));
        let capabilities = if supports_ui { vec!["attach_snapshot", "event_sequence", "extension_ui"] } else { vec!["attach_snapshot", "event_sequence"] };
        body.insert("capabilities".into(), json!(capabilities));
        body.insert("supportsExtensionUi".into(), json!(supports_ui));
        response_data(client.request_worker(body, REQUEST_TIMEOUT).await.map_err(|error| error.to_string())?).map(|_| ())
    }
    /// `consumeWorkerRosterDelta(worker, payload, source)`.
    fn consume_worker_roster_delta(self: &Arc<Self>, worker: &Arc<Worker>, payload: &[u8]) {
        let Ok(value) = serde_json::from_slice::<Value>(payload) else { return; };
        let outbound = match serde_json::from_value::<DaemonWorkerRosterOutbound>(value) {
            Ok(outbound) => outbound,
            Err(_) => return,
        };
        let DaemonWorkerRosterOutbound::RosterDelta { entries, removed_agent_ids, snapshot } = outbound else {
            // `roster_heartbeat` reaches the same consumer; it carries no rows.
            worker.roster_epoch.fetch_add(1, Ordering::SeqCst);
            return;
        };
        worker.roster_epoch.fetch_add(1, Ordering::SeqCst);
        if snapshot == Some(true) { self.apply_worker_roster_snapshot(worker, entries, removed_agent_ids); }
        else { self.apply_worker_roster_delta(worker, entries, removed_agent_ids); }
    }
    /// `applyWorkerRosterDelta`.
    fn apply_worker_roster_delta(self: &Arc<Self>, worker: &Arc<Worker>, entries: Vec<WorkerRosterEntry>, removed_agent_ids: Option<Vec<String>>) {
        for entry in entries {
            self.write_roster_entry(entry.clone(), Some(worker), None);
            self.sync_root_descriptor_from_roster_entry(worker, &entry);
        }
        for agent_id in removed_agent_ids.unwrap_or_default() {
            self.roster().lock().unwrap().delete(&agent_id);
        }
    }
    /// `applyWorkerRosterSnapshot`: the sweep of rows this worker did not claim,
    /// then the snapshot's own rows.
    ///
    /// blocked_on: the TS body is async and reads the spawn ledger twice (once to
    /// decide `edgesFailed` and once to reseed this worker's family). This port
    /// applies frames synchronously from the worker listener, so the ledger read
    /// and the reseed are not on the frame path; the boot seed
    /// (`seed_roster_ledger`) and `list(all=true)` supply those rows instead.
    /// The `edgesFailed` repair pull (`scheduleRosterRepairPull`) is therefore
    /// unreachable here.
    fn apply_worker_roster_snapshot(self: &Arc<Self>, worker: &Arc<Worker>, entries: Vec<WorkerRosterEntry>, removed_agent_ids: Option<Vec<String>>) {
        let sent: HashSet<String> = entries.iter().map(|entry| entry.agent_id.clone()).collect();
        let removed: HashSet<String> = removed_agent_ids.clone().unwrap_or_default().into_iter().collect();
        let unclaimed: Vec<AgentRosterEntry> = self.worker_roster_entries(worker)
            .into_iter().filter(|entry| !sent.contains(&entry.agent_id)).collect();
        // Unreadable edges skip the absentee sweep: it cannot tell registry children
        // from stale rows. The ledger read is async, so the sweep is conservative here.
        for entry in &unclaimed {
            self.roster().lock().unwrap().delete(&entry.agent_id);
        }
        for entry in entries {
            self.write_roster_entry(entry.clone(), Some(worker), None);
            self.sync_root_descriptor_from_roster_entry(worker, &entry);
        }
        for agent_id in removed { self.roster().lock().unwrap().delete(&agent_id); }
    }
    /// `handleList(client, command)`: live roster rows, then the saved-session
    /// catalog merge and the spawn ledger's dead families when `all` is set.
    async fn handle_list(&self, public: &PublicClient, body: &Map<String, Value>) -> Result<Value, String> {
        let include_client_owned = body.get("includeClientOwned").and_then(Value::as_bool) == Some(true);
        let all = body.get("all").and_then(Value::as_bool) == Some(true);
        let workers: HashMap<String, Arc<Worker>> = self.workers.lock().unwrap().clone();
        let mut active: Vec<SessionSummary> = Vec::new();
        let mut active_by_file: HashMap<String, SessionSummary> = HashMap::new();
        let mut busy_client_owned_session_count: i64 = 0;
        for entry in self.roster().lock().unwrap().values() {
            if entry.queued_child == Some(true) { continue; }
            let Some(worker) = entry.worker_id.as_ref().and_then(|worker_id| workers.get(worker_id)).cloned() else { continue; };
            let summary = self.public_summary(&worker, summary_from_entry(&entry));
            if self.is_visible_worker(&worker) {
                if let Some(file) = summary.session_file.as_ref() { active_by_file.insert(canonical_session_path(file), summary.clone()); }
                active.push(summary);
                continue;
            }
            if let Some(file) = summary.session_file.as_ref() { active_by_file.insert(canonical_session_path(file), summary.clone()); }
            // Busy counts come from the row's REAL state, never a synthetic flag.
            if is_session_summary_busy(summary.is_session_active, summary.has_running_rlm_children) { busy_client_owned_session_count += 1; }
            if include_client_owned && visible(&worker, &public.identity()) { active.push(summary); }
        }
        let mut data = json!({ "sessions": active });
        if include_client_owned { data["busyClientOwnedSessionCount"] = json!(busy_client_owned_session_count); }
        if !all { return Ok(data); }
        let session_dir = body.get("sessionDir").and_then(Value::as_str).map(str::to_string).or_else(|| self.config.session_dir.clone());
        let cwd = body.get("cwd").and_then(Value::as_str).map(resolve_path);
        let scanned = self.catalog.list(cwd.as_deref(), session_dir.as_deref(), None).await?;
        let mut merged: Vec<SessionSummary> = Vec::new();
        let mut served_rows: HashSet<String> = HashSet::new();
        for summary in &active { served_rows.insert(summary.served_row_key()); }
        let mut merged_active_files: HashSet<String> = HashSet::new();
        let mut scanned_files: HashSet<String> = HashSet::new();
        for info in &scanned {
            let file = canonical_session_path(&info.path);
            scanned_files.insert(file.clone());
            let worker_row = active_by_file.get(&file);
            // The on-disk scan is public: an unserved (client-owned) worker row hides its live metadata only.
            match worker_row.filter(|row| served_rows.contains(&row.served_row_key())) {
                Some(row) => { merged.push(row.clone()); merged_active_files.insert(file); }
                None => merged.push(summary_for_inactive_session(info, false, false)),
            }
        }
        let spawn_edges = match self.rlm_spawn_ledger().await { Ok(ledger) => ledger.live_edges().await, Err(error) => { eprintln!("Could not list spawn-ledger sessions: {error}"); Vec::new() } };
        let spawn_parents: HashMap<String, String> = spawn_edges.iter()
            .map(|edge| (canonical_session_path(&edge.child), edge.parent.clone())).collect();
        let offline_rows: Vec<AgentRosterEntry> = self.roster().lock().unwrap().values().into_iter()
            .filter(|entry| {
                if entry.queued_child == Some(true) || entry.summary.active_session_id.is_some() { return false; }
                if entry.worker_id.as_ref().is_some_and(|worker_id| workers.contains_key(worker_id)) { return false; }
                let Some(file) = entry.summary.session_file.as_ref().map(|file| canonical_session_path(file)) else { return false; };
                !scanned_files.contains(&file) && !active_by_file.contains_key(&file)
            })
            .collect();
        for entry in offline_rows {
            let hydrated = hydrate_seeded_roster_entry(entry, self).await;
            let summary = summary_from_entry(&hydrated);
            if cwd.as_ref().is_some_and(|cwd| resolve_path(&summary.cwd) != *cwd) { continue; }
            if !matches_list_session_dir(&summary, session_dir.as_deref(), &spawn_parents) { continue; }
            merged.push(summary);
        }
        let mut unseeded_files: HashSet<String> = HashSet::new();
        for edge in &spawn_edges {
            let child_path = canonical_session_path(&edge.child);
            if scanned_files.contains(&child_path) || active_by_file.contains_key(&child_path) || unseeded_files.contains(&child_path) { continue; }
            if self.roster().lock().unwrap().has_session_file(&child_path) { continue; }
            let entry = roster_entry_for_spawn_ledger_edge(edge);
            if self.roster().lock().unwrap().has(&entry.agent_id) { continue; }
            unseeded_files.insert(child_path);
            // Hydrated one at a time, like the boot seed: a large dead-family ledger must not fan
            // out into one concurrent transcript read per child.
            let hydrated = hydrated_seed_entry(&entry).await;
            // The same classification a roster write would have applied: these rows read "inactive".
            let status = classify_session_roster_status(
                &RosterSummaryView { active_session_id: hydrated.summary.active_session_id.clone(), activity: Some(hydrated.summary.activity.clone()), is_session_active: Some(hydrated.summary.is_session_active) },
                hydrated.queued_child == Some(true),
            );
            let summary = session_summary_from_roster_row(&hydrated.summary, Some(status), None, None);
            if cwd.as_ref().is_some_and(|cwd| resolve_path(&summary.cwd) != *cwd) { continue; }
            if !matches_list_session_dir(&summary, session_dir.as_deref(), &spawn_parents) { continue; }
            merged.push(summary);
        }
        for summary in active {
            let file = summary.session_file.as_ref().map(|file| canonical_session_path(file));
            if file.is_some_and(|file| merged_active_files.contains(&file)) { continue; }
            merged.push(summary);
        }
        data["sessions"] = json!(merged);
        Ok(data)
    }
    /// `hydrateSeededEntry(entry)`: refresh a seeded row's cwd from its transcript.
    /// `syncRootDescriptorFromRosterEntry(worker, entry)`.
    fn sync_root_descriptor_from_roster_entry(&self, worker: &Arc<Worker>, entry: &WorkerRosterEntry) {
        let summary = &entry.summary;
        if summary.active_session_id.as_deref() != Some(worker.descriptor.lock().unwrap().root_active_session_id.as_str()) { return; }
        {
            let mut descriptor = worker.descriptor.lock().unwrap();
            if descriptor.root_session_id == Some(summary.session_id.clone()) && descriptor.session_file == summary.session_file { return; }
            descriptor.root_session_id = Some(summary.session_id.clone());
            descriptor.session_file = summary.session_file.clone();
            descriptor.create_command = DurableDaemonCreateCommand {
                type_: "create".into(),
                session_path: summary.session_file.clone(),
                no_session: descriptor.create_command.no_session,
                extra: Map::new(),
            };
        }
        let descriptor = worker.descriptor.lock().unwrap().clone();
        if let Err(error) = self.persist_worker(&descriptor) { eprintln!("Could not persist worker descriptor {}: {error}", descriptor.worker_id); }
    }
    /// `markWorkerRosterEntries(worker, statusLabel)`.
    fn mark_worker_roster_entries(&self, worker: &Arc<Worker>, status_label: Option<&str>) {
        for entry in self.worker_roster_entries(worker) {
            if entry.queued_child != Some(true) && entry.summary.active_session_id.is_none() { continue; }
            self.roster().lock().unwrap().amend(&entry.agent_id, RosterEntryMarks { status_label: Some(status_label.map(str::to_string)), last_heard_from_at: None });
        }
    }
    /// `flipWorkerRosterEntriesInactive(worker)`: an owned worker's rows are
    /// ephemeral and die with the registration; a resident worker's rows are
    /// passivated so a scheduled wake can still find them.
    fn flip_worker_roster_entries_inactive(&self, worker: &Arc<Worker>) {
        let ephemeral = worker.descriptor.lock().unwrap().owner_client_id.is_some();
        for entry in self.worker_roster_entries(worker) {
            if ephemeral || entry.queued_child == Some(true) {
                self.roster().lock().unwrap().delete(&entry.agent_id);
                continue;
            }
            let registrations = RegisteredHeartbeatFlags {
                has_registered_heartbeat: entry.summary.has_registered_heartbeat == Some(true),
                has_registered_cron_job: entry.summary.has_registered_cron_job == Some(true),
            };
            let passivated = passivated_worker_roster_entry(
                &WorkerRosterEntry { agent_id: entry.agent_id.clone(), queued_child: entry.queued_child, seeded_cwd: entry.seeded_cwd, summary: entry.summary.clone() },
                Some(registrations),
            );
            self.write_roster_entry(passivated, None, None);
        }
    }
    /// `clearRosterStaleness(worker)`: a live frame clears the mark at once, not
    /// at the next watchdog tick, so `lastHeardFromAt` never outlives the report.
    fn clear_roster_staleness(&self, worker: &Arc<Worker>) {
        if !worker.roster_stale.swap(false, Ordering::SeqCst) { return; }
        for entry in self.worker_roster_entries(worker) {
            self.roster().lock().unwrap().amend(&entry.agent_id, RosterEntryMarks { status_label: None, last_heard_from_at: Some(None) });
        }
    }
    /// `sweepRosterStaleness(now)`.
    fn sweep_roster_staleness(&self) {
        let workers: Vec<Arc<Worker>> = self.workers.lock().unwrap().values().cloned().collect();
        for worker in workers {
            let Some(last_frame_at) = *worker.last_frame_at.lock().unwrap() else { continue; };
            if worker.client.lock().unwrap().is_none() { continue; }
            if supervisor_now_ms().saturating_sub(last_frame_at) > ROSTER_STALE_AFTER_MS {
                let last_heard_from_at = iso_from_ms(last_frame_at as f64);
                for entry in self.worker_roster_entries(&worker) {
                    if entry.last_heard_from_at.as_deref() != Some(last_heard_from_at.as_str()) {
                        self.roster().lock().unwrap().amend(&entry.agent_id, RosterEntryMarks { status_label: None, last_heard_from_at: Some(Some(last_heard_from_at.clone())) });
                    }
                }
                worker.roster_stale.store(true, Ordering::SeqCst);
            } else {
                self.clear_roster_staleness(&worker);
            }
        }
    }
    /// `scheduleScheduledSessionWakeRecompute()` (daemon-supervisor.ts:942-957): one recompute
    /// at a time, with a queued re-run so a change during a recompute is never lost.
    fn schedule_scheduled_session_wake_recompute(self: &Arc<Self>) {
        if self.stopped.is_cancelled() { return; }
        if self.scheduled_wake_recompute.swap(true, Ordering::SeqCst) {
            self.scheduled_wake_recompute_queued.store(true, Ordering::SeqCst);
            return;
        }
        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            let mut requeue = true;
            while requeue {
                requeue = false;
                if let Err(error) = supervisor.recompute_scheduled_session_wake().await {
                    eprintln!("Scheduled-session wake recompute failed: {error}");
                }
                supervisor.scheduled_wake_recompute.store(false, Ordering::SeqCst);
                if supervisor.scheduled_wake_recompute_queued.swap(false, Ordering::SeqCst) { requeue = true; }
            }
        });
    }
    /// `recomputeScheduledSessionWake()` (daemon-supervisor.ts:1028-1056): arm a timer for the
    /// earliest due passive scheduled job, with the failure floor that keeps an overdue job
    /// off a hot retry loop.
    async fn recompute_scheduled_session_wake(self: &Arc<Self>) -> Result<(), String> {
        if self.stopped.is_cancelled() { return Ok(()); }
        let candidates = self.collect_passive_scheduled_jobs(false).await;
        if self.stopped.is_cancelled() { return Ok(()); }
        {
            let mut timer = self.scheduled_wake_timer.lock().unwrap();
            if let Some(handle) = timer.take() { handle.abort(); }
        }
        let now = supervisor_now_ms() as f64;
        let candidate_roots: HashSet<String> = candidates.iter().map(|(root, _)| canonical_session_path(root)).collect();
        {
            let mut failures = self.scheduled_wake_failures.lock().unwrap();
            failures.retain(|root, _| candidate_roots.contains(root));
        }
        let mut wake_times: Vec<f64> = Vec::new();
        for (root_session_file, job) in &candidates {
            if job.get("status").and_then(Value::as_str) != Some(crate::core::cron_jobs::STATUS_ACTIVE) { continue; }
            let Some(next_run_at) = job.get("nextRunAt").and_then(Value::as_str) else { continue; };
            let Some(run_at) = iso_to_ms(next_run_at) else { continue; };
            let failed_at = self.scheduled_wake_failures.lock().unwrap().get(&canonical_session_path(root_session_file)).copied();
            wake_times.push(match failed_at { Some(failed_at) => run_at.max(failed_at + SCHEDULED_WAKE_RETRY_MS), None => run_at });
        }
        if wake_times.is_empty() { return Ok(()); }
        let earliest = wake_times.iter().cloned().fold(f64::INFINITY, f64::min);
        let delay = (earliest - now).max(0.0).min(SCHEDULED_WAKE_MAX_TIMEOUT_MS);
        let supervisor = Arc::clone(self);
        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = supervisor.stopped.cancelled() => {}
                _ = tokio::time::sleep(Duration::from_millis(delay as u64)) => {
                    if let Err(error) = supervisor.wake_due_scheduled_sessions().await {
                        eprintln!("Scheduled-session wake failed: {error}");
                    }
                }
            }
        });
        // The TS unrefs this timer; aborting it on shutdown matches that (see `start_roster_watchdog`).
        let replaced = self.scheduled_wake_timer.lock().unwrap().replace(handle);
        if let Some(replaced) = replaced { replaced.abort(); }
        Ok(())
    }
    /// `wakeDueScheduledSessions(now)` (daemon-supervisor.ts:1058-1086): create-or-reuse a
    /// worker for every passive root whose active scheduled job is due, then re-arm.
    async fn wake_due_scheduled_sessions(self: &Arc<Self>) -> Result<(), String> {
        self.scheduled_wake_timer.lock().unwrap().take();
        if self.stopped.is_cancelled() { return Ok(()); }
        let now = supervisor_now_ms() as f64;
        let mut due: Vec<(String, String)> = Vec::new();
        {
            let mut seen: HashSet<String> = HashSet::new();
            for (root_session_file, job) in self.collect_passive_scheduled_jobs(false).await {
                if job.get("status").and_then(Value::as_str) != Some(crate::core::cron_jobs::STATUS_ACTIVE) { continue; }
                let Some(run_at) = job.get("nextRunAt").and_then(Value::as_str).and_then(iso_to_ms) else { continue; };
                if run_at > now { continue; }
                let key = canonical_session_path(&root_session_file);
                if seen.insert(key) { due.push((canonical_session_path(&root_session_file), root_session_file)); }
            }
        }
        for (root_key, session_path) in due {
            if self.stopped.is_cancelled() { return Ok(()); }
            let body = json!({"type":"create", "sessionPath": session_path}).as_object().cloned().unwrap_or_default();
            // The supervisor only wakes non-resident trees; firing and delivery stay worker-owned
            // (daemon-supervisor.ts:941), so the wake uses the `SCHEDULED_WAKE_CLIENT_ID` owner.
            match self.create_for_owner(SCHEDULED_WAKE_CLIENT_ID.to_string(), &body).await {
                Ok(_) => { self.scheduled_wake_failures.lock().unwrap().remove(&root_key); eprintln!("Woke session worker for a due scheduled job: {session_path}"); }
                Err(error) => { self.scheduled_wake_failures.lock().unwrap().insert(root_key, supervisor_now_ms() as f64); eprintln!("Scheduled wake failed for {session_path}: {error}"); }
            }
        }
        self.schedule_scheduled_session_wake_recompute();
        Ok(())
    }
    /// The roster watchdog (`setInterval(() => this.sweepRosterStaleness(), ROSTER_WATCHDOG_INTERVAL_MS)`).
    fn start_roster_watchdog(self: &Arc<Self>) {
        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(ROSTER_WATCHDOG_INTERVAL_MS));
            loop {
                tokio::select! { _ = supervisor.stopped.cancelled() => break, _ = interval.tick() => supervisor.sweep_roster_staleness() }
            }
        });
    }
    /// `seedRosterLedger()`: registered workers' families become roster rows.
    async fn seed_roster_ledger(self: &Arc<Self>) {
        let roots: HashSet<String> = self.workers.lock().unwrap().values()
            .filter_map(|worker| {
                let descriptor = worker.descriptor.lock().unwrap();
                descriptor.session_file.clone().or_else(|| descriptor.create_command.session_path.clone())
            })
            .map(|root| canonical_session_path(&root))
            .collect();
        if roots.is_empty() { return; }
        let ledger = match self.rlm_spawn_ledger().await {
            Ok(ledger) => ledger,
            Err(error) => { eprintln!("Could not seed the agent roster from the spawn ledger: {error}"); return; }
        };
        let edges = ledger.live_edges().await;
        // The parent map is owned, so the walk does not borrow `edges` across the loop.
        let parent_by_child = roster_parent_by_child(&edges);
        for edge in &edges {
            if !roster_path_descends_from(&parent_by_child, &canonical_session_path(&edge.parent), &roots) { continue; }
            let entry = roster_entry_for_spawn_ledger_edge(&edge);
            if self.roster().lock().unwrap().has(&entry.agent_id) { continue; }
            if self.roster().lock().unwrap().has_session_file(&canonical_session_path(&edge.child)) { continue; }
            let hydrated = hydrated_seed_entry(&entry).await;
            self.roster().lock().unwrap().write(hydrated, None, None);
        }
    }
    async fn adopt_workers(self: &Arc<Self>) -> Result<(), String> {
        for entry in std::fs::read_dir(&self.descriptor_dir).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") { continue; }
            let descriptor: DaemonWorkerDescriptor = match std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()) { Some(value) => value, None => continue };
            if normalize_socket_path_for_daemon(&descriptor.supervisor_socket_path, None) != self.socket_path || descriptor.stop_requested_at.is_some() { continue; }
            if !matches_exact_process_identity(&ProcessIdentity { pid: descriptor.pid as i64, process_start_id: descriptor.process_start_id.clone() }) {
                if descriptor.owner_client_id.is_none() && descriptor.session_file.is_some() {
                    let body = recovery_command(&descriptor)?;
                    self.launch_worker(&body, String::new(), Some(descriptor)).await?;
                }
                continue;
            }
            let client = Arc::new(DaemonWorkerClient::new(&descriptor.socket_path));
            let root = descriptor.root_active_session_id.clone();
            // Listeners are installed before authentication so the roster snapshot
            // the worker flushes right after auth cannot race them.
            let worker = self.install_worker(descriptor, client);
            let client = { worker.client.lock().unwrap().clone().ok_or("Session worker is not connected")? };
            let descriptor = { worker.descriptor.lock().unwrap().clone() };
            self.authenticate(&client, &descriptor).await?;
            self.refresh(&worker).await?;
            self.subscribe(&worker, &root).await?;
        }
        Ok(())
    }
    async fn create(self: &Arc<Self>, public: &PublicClient, body: &Map<String, Value>) -> Result<Value, String> {
        self.create_for_owner(public.identity(), body).await
    }
    /// `createOrReuseWorker(clientId, command)` (daemon-supervisor.ts:3033): the owner is a
    /// client identity string, so the scheduled wake can reuse the same path with
    /// `SCHEDULED_WAKE_CLIENT_ID` (:1075) instead of a live client connection.
    async fn create_for_owner(self: &Arc<Self>, owner: String, body: &Map<String, Value>) -> Result<Value, String> {
        let _opening = self.opening.lock().await;
        if let Some(path) = body.get("sessionPath").and_then(Value::as_str) {
            let workers: Vec<_> = self.workers.lock().unwrap().values().cloned().collect();
            for worker in workers {
                if !visible(&worker, &owner) { continue; }
                // The refresh republishes the worker's rows, so the summary that
                // answers a reuse is the roster's, matched on the canonical path.
                self.refresh(&worker).await?;
                if let Some(summary) = self.find_summary_in_worker(&worker, path) {
                    return Ok(serde_json::to_value(self.public_summary(&worker, summary)).unwrap_or(Value::Null));
                }
            }
        }
        self.launch_worker(body, owner, None).await
    }
    async fn launch_worker(self: &Arc<Self>, body: &Map<String, Value>, owner: String, existing: Option<DaemonWorkerDescriptor>) -> Result<Value, String> {
        self.ownership.assert_current().await.map_err(|error| error.to_string())?;
        let override_config = body.get("config").map(|value| serde_json::from_value::<AgentSessionRuntimeConfig>(value.clone())).transpose().map_err(|error| error.to_string())?;
        let config = merge_agent_session_runtime_config(&self.config, override_config.as_ref());
        let worker_id = existing.as_ref().map(|descriptor| descriptor.worker_id.clone()).unwrap_or_else(|| create_active_session_id(None));
        let root = existing.as_ref().map(|descriptor| descriptor.root_active_session_id.clone()).unwrap_or_else(|| create_active_session_id(None));
        let socket = existing.as_ref().map(|descriptor| descriptor.socket_path.clone()).unwrap_or_else(|| worker_socket(&self.socket_path, &worker_id));
        let mut token_bytes = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut token_bytes);
        use base64::Engine;
        let token = existing.as_ref().map(|descriptor| descriptor.authentication_token.clone()).unwrap_or_else(|| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token_bytes));
        let instance = uuid::Uuid::new_v4().to_string();
        let recovery = self.descriptor_dir.join(format!("{worker_id}.recovery.jsonl")).to_string_lossy().into_owned();
        let orphan = self.descriptor_dir.join(format!("{worker_id}.orphans.jsonl")).to_string_lossy().into_owned();
        let mut environment: HashMap<String, String> = std::env::vars().collect();
        if let Some(launch) = body.get("launchEnv").and_then(Value::as_object) {
            for (key, value) in launch { if let Some(value) = value.as_str() { environment.insert(key.clone(), value.to_string()); } }
        }
        for (key, value) in [(DAEMON_WORKER_ROLE_ENV, "1".to_string()), (DAEMON_WORKER_TOKEN_ENV, token.clone()),
            (DAEMON_WORKER_INSTANCE_ID_ENV, instance.clone()), (DAEMON_WORKER_ACTIVE_SESSION_ID_ENV, root.clone()),
            (DAEMON_WORKER_SUPERVISOR_SOCKET_ENV, self.socket_path.clone()), (DAEMON_WORKER_RECOVERY_JOURNAL_ENV, recovery.clone()),
            (SESSION_LEASES_ENABLED_ENV, "1".to_string()), (SESSION_LEASE_OWNER_ID_ENV, root.clone()),
            (crate::core::orphan_process_journal::ORPHAN_PROCESS_JOURNAL_ENV, orphan.clone())] { environment.insert(key.to_string(), value); }
        environment.remove("RLM_DEPTH"); environment.remove(DAEMON_CATALOG_ROLE_ENV);
        let (mut child, gate) = spawn_worker_process(&socket, config.cwd.as_deref(), environment)?;
        let pid = child.id().ok_or("Failed to obtain daemon worker pid")?;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut descriptor = DaemonWorkerDescriptor {
            version: 2, worker_id, pid: pid as i32, process_start_id: get_process_start_id(pid as i64), socket_path: socket.clone(),
            recovery_journal_path: recovery, orphan_process_journal_path: Some(orphan), supervisor_socket_path: self.socket_path.clone(),
            authentication_token: token, worker_instance_id: Some(instance), root_active_session_id: root.clone(),
            owner_client_id: existing.as_ref().and_then(|descriptor| descriptor.owner_client_id.clone()).or_else(|| (body.get("lifecycle").and_then(Value::as_str) == Some("client_owned")).then_some(owner)),
            root_session_id: None, session_file: None, session_dir: config.session_dir.clone(), telemetry_disabled: config.telemetry_disabled,
            created_at: existing.as_ref().map(|descriptor| descriptor.created_at.clone()).unwrap_or_else(|| now.clone()), updated_at: now, lifecycle: "starting".into(),
            create_command: DurableDaemonCreateCommand { type_: "create".into(), session_path: body.get("sessionPath").and_then(Value::as_str).map(str::to_string), no_session: body.get("noSession").and_then(Value::as_bool), extra: Map::new() },
            consecutive_failures: 0, stop_requested_at: None, archive_on_stop: None, last_failure_at: None, last_error: None,
        };
        let result: Result<Value, String> = async {
            self.persist_worker(&descriptor)?;
            let client = Arc::new(DaemonWorkerClient::new(&socket));
            // Listeners before authentication: the worker flushes its roster
            // snapshot immediately after auth succeeds.
            let worker = self.install_worker(descriptor.clone(), client);
            commit_gate(gate).await?;
            let client = { worker.client.lock().unwrap().clone().ok_or("Session worker is not connected")? };
            self.authenticate(&client, &descriptor).await?;
            let mut forwarded = body.clone(); forwarded.remove("id"); forwarded.remove("launchEnv"); forwarded.remove("lifecycle");
            forwarded.insert("config".into(), serde_json::to_value(config).map_err(|error| error.to_string())?);
            let response = client.request_worker(forwarded, REQUEST_TIMEOUT).await.map_err(|error| error.to_string())?;
            let summary = response_data(response)?;
            if summary.get("activeSessionId").or_else(|| summary.get("id")).and_then(Value::as_str) != Some(&root) { return Err("Session worker did not preserve its assigned active session id".into()); }
            {
                let mut current = worker.descriptor.lock().unwrap();
                current.root_session_id = summary.get("sessionId").and_then(Value::as_str).map(str::to_string);
                current.session_file = summary.get("sessionFile").and_then(Value::as_str).map(str::to_string);
                current.lifecycle = DAEMON_WORKER_LIFECYCLE_READY.into();
                self.persist_worker(&current)?;
            }
            let summary_row = worker_roster_entry_from_value(&summary).ok_or("Session worker returned an invalid create response")?;
            self.write_roster_entry(summary_row, Some(&worker), None);
            self.subscribe(&worker, &root).await?;
            self.refresh(&worker).await?;
            let entry = self.roster().lock().unwrap().by_active_session_id(&root);
            let Some(entry) = entry else { return Err("Session worker started without a root session".into()); };
            Ok(serde_json::to_value(self.public_summary(&worker, summary_from_entry(&entry))).unwrap_or(Value::Null))
        }.await;
        if let Err(error) = &result {
            let _ = child.kill().await;
            self.workers.lock().unwrap().remove(&descriptor.worker_id);
            descriptor.lifecycle = "failed".into(); descriptor.last_error = Some(error.clone()); descriptor.consecutive_failures += 1;
            let _ = self.persist_worker(&descriptor);
        }
        tokio::spawn(async move { let _ = child.wait().await; });
        result
    }
    /// `matchWorkers(selector, includeWorker)` over the roster, then
    /// `collectedPassiveScheduledJob { rootSessionFile, job, info }` (daemon-supervisor.ts:962).
    ///
    /// `collectPassiveScheduledJobs(includeInactive)` (daemon-supervisor.ts:960-1026): the
    /// durable scheduled-job truth for sessions with no resident worker, so a wake timer and
    /// `cron_list` can see jobs of passivated trees. The ledger family supplies the sessions
    /// and their parents; a session is "uncovered" when no worker holds its path.
    ///
    /// blocked_on: the TS prologue (964-974) first settles `collectEphemeralCancelIntents()`
    /// (:1388-1411) through `cancelScheduledJobsForSessionTree` (:6998) and
    /// `deleteWorkerDescriptor` (:1603) and excludes those roots. Neither helper is ported: the
    /// owned-descriptor sweeper belongs to the descriptor-write ownership that
    /// `daemon_supervisor.rs` and `daemon_mode.rs` own, so a stale owned descriptor is filtered
    /// here only by the `AgentRoster`/worker lookup, not deleted.
    async fn collect_passive_scheduled_jobs(self: &Arc<Self>, include_inactive: bool) -> Vec<(String, Value)> {
        let ledger = match self.rlm_spawn_ledger().await { Ok(ledger) => ledger, Err(_) => return Vec::new() };
        let infos = ledger.family().await;
        let info_by_path: HashMap<String, SessionInfo> = infos.iter()
            .map(|info| (canonical_session_path(&info.path), info.clone()))
            .collect();
        let mut results: Vec<(String, Value)> = Vec::new();
        for info in &infos {
            if info.state.as_ref().is_some_and(|state| state.status != crate::core::session_manager::SessionStateStatus::Active) { continue; }
            let artifact_dir = crate::core::session_manager::get_session_artifact_path_for_file(&resolve_path(&info.path), Some(&info.id));
            if !Path::new(&artifact_dir).join(crate::core::cron_jobs::SESSION_SCHEDULED_JOBS_FILENAME).exists() { continue; }
            let store = crate::core::cron_jobs::AgentCronJobStore::for_session_artifacts();
            if !store.register_session_artifact(&info.id, &artifact_dir) { continue; }
            // TS wraps this in `try/catch` and skips unreadable jobs (1009-1014); the ported
            // `AgentCronJobStore::list()` returns just the jobs and has no failure channel, so
            // there is nothing to catch here.
            let jobs = store.list();
            for job in jobs {
                if !include_inactive && job.status != crate::core::cron_jobs::STATUS_ACTIVE && job.status != crate::core::cron_jobs::STATUS_PAUSED { continue; }
                // `uncoveredRootFor(info)`: walk up while no worker holds the current path.
                let root = {
                    let mut cursor = info.clone();
                    let mut visited: HashSet<String> = HashSet::from([canonical_session_path(&cursor.path)]);
                    loop {
                        if self.find_worker_by_session_file(&cursor.path, None).is_some() { break; }
                        let Some(parent_path) = cursor.parent_session_path.clone() else { break; };
                        let Some(parent) = info_by_path.get(&canonical_session_path(&parent_path)) else { break; };
                        if !visited.insert(canonical_session_path(&parent.path)) { break; }
                        cursor = parent.clone();
                    }
                    cursor
                };
                let current = root;
                results.push((current.path.clone(), serde_json::to_value(&job).unwrap_or(Value::Null)));
            }
        }
        results
    }
    /// `findWorkerBySessionFile(sessionFile, exclude)` (daemon-supervisor.ts:5316-5340).
    /// Conflicting resident paths are reported as an error, exactly like the TS throw.
    fn find_worker_by_session_file(&self, session_file: &str, exclude: Option<&Arc<Worker>>) -> Option<Arc<Worker>> {
        let target = canonical_session_path(session_file);
        let target_entry_worker = self.roster().lock().unwrap().by_session_file(&target).and_then(|entry| entry.worker_id);
        let mut matches: Vec<Arc<Worker>> = Vec::new();
        for worker in self.workers.lock().unwrap().values() {
            if exclude.is_some_and(|exclude| Arc::ptr_eq(exclude, worker)) { continue; }
            let descriptor = worker.descriptor.lock().unwrap();
            let summary_match = target_entry_worker.as_deref() == Some(descriptor.worker_id.as_str());
            let descriptor_path = descriptor.session_file.as_ref().map(|file| canonical_session_path(file));
            let configured_path = descriptor.create_command.session_path.as_ref().map(|file| canonical_session_path(file));
            if !summary_match && descriptor_path.as_deref() != Some(target.as_str()) && configured_path.as_deref() != Some(target.as_str()) { continue; }
            if descriptor_path.is_some() && configured_path.is_some() && descriptor_path != configured_path {
                return None;
            }
            matches.push(Arc::clone(worker));
        }
        if matches.len() > 1 { return None; }
        matches.into_iter().next()
    }
    /// `cancelPassiveScheduledJob`: `AgentCronJobStore.forSessionArtifacts().registerSessionArtifact(...).cancel(id)`
    /// (daemon-supervisor.ts:2618-2630).
    async fn cancel_passive_scheduled_job(self: &Arc<Self>, job_id: &str) -> Option<Value> {
        let jobs = self.collect_passive_scheduled_jobs(true).await;
        let job = jobs.into_iter().map(|(_, job)| job).find(|job| job.get("id").and_then(Value::as_str) == Some(job_id))?;
        let session_file = job.get("sessionFile").and_then(Value::as_str)?.to_string();
        let session_id = job.get("sessionId").and_then(Value::as_str)?.to_string();
        let store = crate::core::cron_jobs::AgentCronJobStore::for_session_artifacts();
        store.register_session_artifact(&session_id, &crate::core::session_manager::get_session_artifact_path_for_file(&resolve_path(&session_file), Some(&session_id)));
        store.cancel(job_id, supervisor_now_ms() as f64).and_then(|job| serde_json::to_value(job).ok())
    }
    /// `isLiveWorker(worker)` (daemon-supervisor.ts:5053-5055).
    fn is_live_worker(&self, worker: &Arc<Worker>) -> bool {
        self.is_visible_worker(worker) && worker.descriptor.lock().unwrap().stop_requested_at.is_none()
    }
    /// `findWorker`'s recovery fallback and its typed recovering error.
    async fn find(self: &Arc<Self>, identity: &str, requested: &str) -> Result<(Arc<Worker>, String), String> {
        let mut matches = self.match_workers(requested, Some(&|worker: &Arc<Worker>| visible(worker, identity)));
        if matches.is_empty() {
            let workers: Vec<Arc<Worker>> = self.workers.lock().unwrap().values().cloned().collect();
            for worker in workers { let _ = self.refresh(&worker).await; }
            matches = self.match_workers(requested, Some(&|worker: &Arc<Worker>| visible(worker, identity)));
        }
        match matches.len() {
            1 => return Ok(matches.remove(0)),
            0 => {}
            _ => return Err(format!("Ambiguous active session \"{requested}\"")),
        }
        // Descriptors are the durable half of addressability: an unhydrated root is
        // recovering, not unknown; failed workers stay unknown so clients take the
        // create fallback, which reclaims or retries them.
        let recovering: Vec<Arc<Worker>> = self.workers.lock().unwrap().values()
            .filter(|worker| visible(worker, identity))
            .filter(|worker| {
                let descriptor = worker.descriptor.lock().unwrap();
                descriptor.lifecycle != DAEMON_WORKER_LIFECYCLE_FAILED
                    && descriptor.stop_requested_at.is_none()
                    && worker.client.lock().unwrap().is_none()
                    && (descriptor.root_active_session_id == requested || descriptor.root_session_id.as_deref() == Some(requested))
            })
            .cloned().collect();
        let recovering = if recovering.is_empty() {
            self.workers.lock().unwrap().values().filter(|worker| visible(worker, identity)).filter(|worker| {
                let descriptor = worker.descriptor.lock().unwrap();
                descriptor.lifecycle != DAEMON_WORKER_LIFECYCLE_FAILED
                    && descriptor.stop_requested_at.is_none()
                    && worker.client.lock().unwrap().is_none()
                    && (matches_session_id_suffix(&descriptor.root_active_session_id, requested)
                        || descriptor.root_session_id.as_deref().is_some_and(|root| matches_session_id_suffix(root, requested)))
            }).cloned().collect()
        } else { recovering };
        if let [worker] = recovering.as_slice() {
            return Err(DaemonSessionRecoveringError::new(worker.descriptor.lock().unwrap().root_active_session_id.clone()).message());
        }
        Err(format!("Unknown active session: {requested}"))
    }
    /// `familyCatalogEntry(summary)` (daemon-supervisor.ts:5148-5161).
    fn family_catalog_entry(&self, summary: &SessionSummary) -> AgentFamilyCatalogEntry {
        let depth = summary.rlm_depth.unwrap_or(if summary.parent_session_path.is_some() { 1 } else { 0 });
        let status = summary.roster_status.unwrap_or_else(|| classify_session_roster_status(
            &RosterSummaryView {
                active_session_id: summary.active_session_id.clone(),
                activity: Some(summary.activity.clone()),
                is_session_active: Some(summary.is_session_active),
            },
            false,
        ));
        AgentFamilyCatalogEntry {
            id: summary.session_id.clone(),
            name: summary.session_name.clone(),
            depth: depth as f64,
            status: status.as_str().to_string(),
            replied_since_task: None,
            parent_session_id: (depth > 0).then(|| summary.parent_session_id.clone()).flatten(),
            parent_session_path: (depth > 0).then(|| summary.parent_session_path.as_ref().map(|path| canonical_session_path(path))).flatten(),
            session_path: summary.session_file.as_ref().map(|file| canonical_session_path(file)),
        }
    }
    /// The roster summary for a resolved active session id, as the a2a send path needs it
    /// (`target.summary`, daemon-supervisor.ts:2746-2753).
    fn summary_for_active(&self, worker: &Arc<Worker>, active: &str) -> Option<SessionSummary> {
        let entry = self.roster().lock().unwrap().by_active_session_id(active)?;
        Some(self.public_summary(worker, summary_from_entry(&entry)))
    }
    /// `matchWorkers(selector, includeWorker)`: exact ids win, hex suffixes second.
    fn match_workers(&self, selector: &str, include_worker: Option<&dyn Fn(&Arc<Worker>) -> bool>) -> Vec<(Arc<Worker>, String)> {
        let mut exact: Vec<(Arc<Worker>, String)> = Vec::new();
        let mut suffix: Vec<(Arc<Worker>, String)> = Vec::new();
        for entry in self.roster().lock().unwrap().values() {
            if entry.queued_child == Some(true) { continue; }
            let worker = entry.worker_id.as_ref().and_then(|worker_id| self.workers.lock().unwrap().get(worker_id).cloned());
            let Some(worker) = worker else { continue; };
            if include_worker.is_some_and(|include| !include(&worker)) { continue; }
            let summary = self.public_summary(&worker, summary_from_entry(&entry));
            let active_session_id = summary.active_session_id.clone().unwrap_or_else(|| summary.id.clone());
            if active_session_id == selector || summary.session_id == selector || summary.session_name.as_deref() == Some(selector) {
                exact.push((Arc::clone(&worker), active_session_id));
            } else if matches_session_id_suffix(&active_session_id, selector) || matches_session_id_suffix(&summary.session_id, selector) {
                suffix.push((Arc::clone(&worker), active_session_id));
            }
        }
        if exact.is_empty() { suffix } else { exact }
    }
    /// `findSummaryInWorker(worker, selector)`.
    fn find_summary_in_worker(&self, worker: &Arc<Worker>, selector: &str) -> Option<SessionSummary> {
        let path_selector = looks_like_session_path(selector).then(|| canonical_session_path(selector));
        let summaries: Vec<SessionSummary> = self.worker_roster_entries(worker).into_iter()
            .filter(|entry| entry.queued_child != Some(true))
            .map(|entry| self.public_summary(worker, summary_from_entry(&entry)))
            .collect();
        let exact = summaries.iter().find(|summary| {
            let active_session_id = summary.active_session_id.clone().unwrap_or_else(|| summary.id.clone());
            active_session_id == selector
                || summary.session_id == selector
                || summary.session_name.as_deref() == Some(selector)
                || (path_selector.is_some() && summary.session_file.as_ref().map(|file| canonical_session_path(file)) == path_selector)
        });
        if let Some(exact) = exact { return Some(exact.clone()); }
        summaries.into_iter().find(|summary| {
            let active_session_id = summary.active_session_id.clone().unwrap_or_else(|| summary.id.clone());
            matches_session_id_suffix(&active_session_id, selector) || matches_session_id_suffix(&summary.session_id, selector)
        })
    }
    async fn release_client_pauses(&self, public: &PublicClient, active: Option<&str>) -> Result<(), String> {
        public.pause_epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let entries: Vec<_> = self.pauses.lock().unwrap().iter().filter(|(_, pause)| pause.connection_id == public.connection_id && active.map_or(true, |active| active == pause.active || active == pause.requested)).map(|(id, pause)| (id.clone(), pause.clone())).collect();
        for (id, pause) in entries {
            let mut body = command("release_session_input_pause");
            body.insert("activeSessionId".into(), json!(pause.active)); body.insert("pauseId".into(), json!(id));
            let client = { pause.worker.client.lock().unwrap().clone().ok_or("Session worker is not connected")? };
            response_data(client.request_worker(body, 5_000).await.map_err(|error| error.to_string())?)?;
            self.pauses.lock().unwrap().remove(&id);
        }
        Ok(())
    }
        /// `publicSummary(worker, summary)`: attached clients plus the worker's
    /// effective lifecycle, which never reports a disconnected worker as ready.
    fn public_summary(&self, worker: &Worker, summary: SessionSummary) -> SessionSummary {
        let active = summary.active_session_id.clone().unwrap_or_else(|| summary.id.clone());
        let mut summary = summary;
        summary.attached_clients = self.attached_client_count(&summary, &active);
        summary.worker_state = Some(self.effective_worker_state(worker));
        summary.worker_pid = Some(worker.descriptor.lock().unwrap().pid as i64);
        summary
    }
    /// `attachedClientCount(summary, activeSessionId)`.
    fn attached_client_count(&self, summary: &SessionSummary, active_session_id: &str) -> i64 {
        let direct = summary.direct_attached_clients.unwrap_or(0);
        let supervisor = self.clients.lock().unwrap().values()
            .filter(|client| client.subscriptions.lock().unwrap().contains(active_session_id))
            .count() as i64;
        direct + supervisor
    }
    /// `effectiveWorkerState(worker)`.
    fn effective_worker_state(&self, worker: &Worker) -> String {
        let descriptor = worker.descriptor.lock().unwrap().clone();
        if descriptor.stop_requested_at.is_some() { return DAEMON_WORKER_LIFECYCLE_STOPPING.to_string(); }
        let connected = worker.client.lock().unwrap().is_some();
        if descriptor.lifecycle == DAEMON_WORKER_LIFECYCLE_READY && !connected { return DAEMON_WORKER_LIFECYCLE_RECOVERING.to_string(); }
        descriptor.lifecycle
    }
    /// `this.roster()`: the one store per supervisor, created at startup so its
    /// mutation sink can hold a real `Weak` to this supervisor.
    fn roster(&self) -> Arc<Mutex<AgentRoster>> {
        self.roster.lock().unwrap().clone().expect("supervisor roster is created at startup")
    }
    fn init_roster(self: &Arc<Self>) {
        let mut slot = self.roster.lock().unwrap();
        if slot.is_some() { return; }
        let weak = Arc::downgrade(self);
        *slot = Some(Arc::new(Mutex::new(AgentRoster::new(
            Box::new(canonical_session_path),
            Box::new(move |mutation| {
                if let Some(supervisor) = weak.upgrade() { supervisor.on_roster_mutation(mutation); }
            }),
        ))));
    }
    /// `onRosterMutation(mutation)`.
    fn on_roster_mutation(self: &Arc<Self>, mutation: AgentRosterMutation) {
        match mutation {
            AgentRosterMutation::Delete { agent_id } => {
                self.pending_roster_changed.lock().unwrap().remove(&agent_id);
                self.pending_roster_removed.lock().unwrap().insert(agent_id);
            }
            AgentRosterMutation::Write { agent_id } => {
                self.pending_roster_removed.lock().unwrap().remove(&agent_id);
                self.pending_roster_changed.lock().unwrap().insert(agent_id);
            }
        }
        self.schedule_roster_push();
    }
    /// `scheduleRosterPush()`.
    fn schedule_roster_push(self: &Arc<Self>) {
        if self.roster_push_scheduled.swap(true, Ordering::SeqCst) { return; }
        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            supervisor.roster_push_scheduled.store(false, Ordering::SeqCst);
            if supervisor.stopped.is_cancelled() { return; }
            supervisor.flush_roster_updates();
        });
    }
    /// `flushRosterUpdates()`.
    fn flush_roster_updates(&self) {
        let mut changed: Vec<AgentRosterEntry> = Vec::new();
        let mut removed: Vec<String> = Vec::new();
        let removed_ids: Vec<String> = self.pending_roster_removed.lock().unwrap().iter().cloned().collect();
        for agent_id in removed_ids {
            if self.published_roster_ids.lock().unwrap().remove(&agent_id) { removed.push(agent_id); }
        }
        let changed_ids: Vec<String> = self.pending_roster_changed.lock().unwrap().iter().cloned().collect();
        for agent_id in changed_ids {
            let entry = self.roster().lock().unwrap().get(&agent_id);
            let Some(entry) = entry else { continue; };
            if self.is_roster_entry_visible_to_clients(&entry) {
                changed.push(entry);
                self.published_roster_ids.lock().unwrap().insert(agent_id);
            } else if self.published_roster_ids.lock().unwrap().remove(&agent_id) {
                removed.push(agent_id);
            }
        }
        self.pending_roster_changed.lock().unwrap().clear();
        self.pending_roster_removed.lock().unwrap().clear();
        if changed.is_empty() && removed.is_empty() { return; }
        for client in self.clients.lock().unwrap().values() {
            if !client.roster_subscribed.load(Ordering::SeqCst) { continue; }
            if client.backpressured.load(Ordering::SeqCst) {
                client.roster_resync_pending.store(true, Ordering::SeqCst);
                continue;
            }
            client.write(&roster_update_value(&changed, &removed, None));
        }
    }
    /// `rosterEntriesForClient()`.
    fn roster_entries_for_client(&self) -> Vec<AgentRosterEntry> {
        let entries: Vec<AgentRosterEntry> = self.roster().lock().unwrap().values()
            .into_iter().filter(|entry| self.is_roster_entry_visible_to_clients(entry)).collect();
        for entry in &entries { self.published_roster_ids.lock().unwrap().insert(entry.agent_id.clone()); }
        entries
    }
    /// `isRosterEntryVisibleToClients(entry)`.
    fn is_roster_entry_visible_to_clients(&self, entry: &AgentRosterEntry) -> bool {
        let worker = entry.worker_id.as_ref().and_then(|worker_id| self.workers.lock().unwrap().get(worker_id).cloned());
        worker.is_none_or(|worker| self.is_visible_worker(&worker))
    }
    /// `isVisibleWorker(worker)`: client-owned workers are private to their owner.
    fn is_visible_worker(&self, worker: &Worker) -> bool {
        worker.descriptor.lock().unwrap().owner_client_id.is_none()
    }
    /// `workerRosterEntries(worker)`.
    fn worker_roster_entries(&self, worker: &Worker) -> Vec<AgentRosterEntry> {
        let worker_id = worker.descriptor.lock().unwrap().worker_id.clone();
        self.roster().lock().unwrap().entries_for_worker(&worker_id)
    }
    /// `writeRosterEntry(entry, worker?, statusLabel?)`.
    fn write_roster_entry(
        &self,
        entry: WorkerRosterEntry,
        worker: Option<&Arc<Worker>>,
        status_label: Option<&str>,
    ) -> AgentRosterEntry {
        let previous_direct = self.roster().lock().unwrap().get(&entry.agent_id)
            .and_then(|existing| existing.summary.direct_attached_clients).unwrap_or(0);
        let worker_id = worker.map(|worker| worker.descriptor.lock().unwrap().worker_id.clone());
        let stored = self.roster().lock().unwrap().write(entry.clone(), worker_id.as_deref(), status_label);
        // Direct peers attach and detach on the worker socket, so their last detach
        // arrives here as roster truth instead of through a supervisor-socket close.
        //
        // blocked_on: the TS follow-up for that case is
        // `evictEmptySessionOnLastDetach`, which needs the idle-eviction fence and
        // the passivate chain (daemon_mode.rs owns both). The row transition itself
        // is applied; only the eviction is deferred.
        let _ = (previous_direct, worker, &stored);
        stored
    }
    /// `rlmSpawnLedger()`: one shared instance per supervisor.
    async fn rlm_spawn_ledger(&self) -> Result<Arc<RlmSpawnLedger>, String> {
        let agent_dir = self.config.agent_dir.clone().ok_or("Daemon supervisor config is missing agentDir")?;
        let session_dir = self.config.session_dir.clone().unwrap_or_else(|| crate::config::get_sessions_dir(Some(&agent_dir)));
        self.ledger.get_or_try_init(|| async {
            Ok(Arc::new(RlmSpawnLedger::new(
                &agent_dir,
                &session_dir,
                Some(create_rlm_ledger_registry_seed_source()),
                Some(Arc::new(|message: &str| eprintln!("{message}"))),
            )))
        }).await.cloned()
    }
    /// `stopWorker(worker, removeDescriptor, force, archiveSession)` (daemon-supervisor.ts:6689).
    /// Only the `shutdown` command's `force` flag reaches here (`forceWorkers`, 2419/7326); every
    /// other caller keeps the non-forcing default of the port.
    async fn stop_worker(&self, worker: &Arc<Worker>, archive: bool, force: bool) -> Result<(), String> {
        self.ownership.assert_current().await.map_err(|error| error.to_string())?;
        let descriptor = {
            let mut descriptor = worker.descriptor.lock().unwrap();
            descriptor.lifecycle = DAEMON_WORKER_LIFECYCLE_STOPPING.into();
            descriptor.stop_requested_at = Some(chrono::Utc::now().to_rfc3339());
            descriptor.archive_on_stop = Some(archive);
            self.persist_worker(&descriptor)?;
            descriptor.clone()
        };
        let kind = if archive { "worker_archive_and_shutdown" } else { "shutdown" };
        let client = { worker.client.lock().unwrap().clone() };
        if let Some(client) = client {
            response_data(client.request_worker(command(kind), 30_000).await.map_err(|error| error.to_string())?)?;
        }
        let identity = ProcessIdentity { pid: descriptor.pid as i64, process_start_id: descriptor.process_start_id.clone() };
        // `const gracefulDeadline = Date.now() + (force ? 500 : process.platform === "win32" ? 10_000 : 2000)`
        // (daemon-supervisor.ts:6824); this port only serves Windows workers.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(if force { 500 } else { 10_000 });
        let mut alive = matches_exact_process_identity(&identity);
        let mut sigkill_sent = false;
        while alive {
            if tokio::time::Instant::now() >= deadline {
                // daemon-supervisor.ts:6830-6843: `force` escalates to SIGKILL, then waits 1s
                // before declaring the stop a timeout.
                if force && matches_exact_process_identity(&identity) {
                    signal_process_group_or_process(descriptor.pid, Signal::Kill);
                    sigkill_sent = true;
                    let force_deadline = tokio::time::Instant::now() + Duration::from_millis(1000);
                    while matches_exact_process_identity(&identity) && tokio::time::Instant::now() < force_deadline {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                }
                if matches_exact_process_identity(&identity) {
                    // `throw new WorkerStopTimeoutError(...)` (daemon-supervisor.ts:6850-6852).
                    return Err(format!("Session worker {} did not stop{}", descriptor.worker_id, if sigkill_sent { " after SIGKILL" } else { "" }));
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
            alive = matches_exact_process_identity(&identity);
        }
        let client = { worker.client.lock().unwrap().take() };
        if let Some(client) = client { client.close().await; }
        self.workers.lock().unwrap().remove(&descriptor.worker_id);
        // The registration is gone, so its rows stop being live: owned rows die
        // with it, resident rows are passivated.
        self.flip_worker_roster_entries_inactive(worker);
        remove_file_durably(&self.descriptor_dir.join(format!("{}.json", descriptor.worker_id)).to_string_lossy(), RemoveFileDurablyOptions { fsync_dir: true, platform: None }).await.map_err(|error| error.to_string())?;
        Ok(())
    }
    async fn dispatch(self: &Arc<Self>, public: &Arc<PublicClient>, mut body: Map<String, Value>) -> Result<Option<DaemonResponse>, String> {
        let id = body.get("id").and_then(Value::as_str).map(str::to_string);
        let kind = body.get("type").and_then(Value::as_str).ok_or("Daemon command requires a type")?.to_string();
        let success = |data| Some(DaemonResponse::success(id.as_deref(), &kind, data));
        match kind.as_str() {
            "ack_result" => { if let Some(command_id) = body.get("commandId").and_then(Value::as_str) { self.journal.lock().unwrap().acknowledge(&public.identity(), command_id)?; } return Ok(None); }
            "create" => return Ok(success(Some(self.create(public, &body).await?))),
            "list" => {
                let data = self.handle_list(public, &body).await?;
                return Ok(success(Some(data)));
            }
            "roster_subscribe" => {
                public.roster_subscribed.store(true, Ordering::SeqCst);
                let roster: Vec<Value> = self.roster_entries_for_client().iter().map(agent_roster_entry_to_value).collect();
                return Ok(success(Some(json!({"roster": roster}))));
            }
            "roster_unsubscribe" => {
                public.roster_subscribed.store(false, Ordering::SeqCst);
                public.roster_resync_pending.store(false, Ordering::SeqCst);
                return Ok(success(None));
            }
            "retry_worker" => {
                let requested = body.get("activeSessionId").and_then(Value::as_str).ok_or("retry_worker requires activeSessionId")?;
                let _opening = self.opening.lock().await;
                let worker = self.workers.lock().unwrap().values().find(|worker| {
                    let descriptor = worker.descriptor.lock().unwrap();
                    descriptor.root_active_session_id == requested || descriptor.root_session_id.as_deref() == Some(requested)
                }).cloned();
                let descriptor = if let Some(worker) = worker {
                    let descriptor = worker.descriptor.lock().unwrap().clone();
                    if matches_exact_process_identity(&ProcessIdentity { pid: descriptor.pid as i64, process_start_id: descriptor.process_start_id.clone() }) {
                        let summary = self.refresh(&worker).await?.into_iter().find(|summary| summary.get("id").and_then(Value::as_str) == Some(&descriptor.root_active_session_id));
                        return Ok(success(summary));
                    }
                    descriptor
                } else {
                    let mut descriptor = None;
                    for entry in std::fs::read_dir(&self.descriptor_dir).map_err(|error| error.to_string())? {
                        let path = entry.map_err(|error| error.to_string())?.path();
                        if path.extension().and_then(|value| value.to_str()) != Some("json") { continue; }
                        if let Some(candidate) = std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<DaemonWorkerDescriptor>(&bytes).ok()) {
                            if candidate.root_active_session_id == requested || candidate.root_session_id.as_deref() == Some(requested) { descriptor = Some(candidate); break; }
                        }
                    }
                    descriptor.ok_or_else(|| format!("Unknown active session: {requested}"))?
                };
                if descriptor.stop_requested_at.is_some() { return Err("Session worker is stopping".into()); }
                if descriptor.owner_client_id.as_deref().is_some_and(|owner| owner != public.identity()) { return Err(format!("Unknown active session: {requested}")); }
                if descriptor.owner_client_id.is_some() { return Err("Client-owned session recovery requires the owning client environment".into()); }
                let recovery = recovery_command(&descriptor)?;
                return Ok(success(Some(self.launch_worker(&recovery, public.identity(), Some(descriptor)).await?)));
            }
            // `case "agent_messages_status"` (daemon-supervisor.ts:2425-2435): without an
            // `activeSessionId` the id-less form is served, not rejected. The port is a
            // supervisor-only slice with no per-worker pause state, so the id-less answer is
            // the TS "no live worker" fallback `{ paused: false, limits: {} }` (2432), and the
            // id-less pause/resume fan-out (2436-2448) is refused explicitly because its
            // durable counterpart (`agent_messages_paused`) lives in daemon_mode.rs.
            "agent_messages_status" if body.get("activeSessionId").and_then(Value::as_str).is_none() => {
                return Ok(success(Some(json!({"paused": false, "limits": {}}))));
            }
            "agent_messages_pause" | "agent_messages_resume" if body.get("activeSessionId").and_then(Value::as_str).is_none() => {
                return Err(format!("Daemon supervisor command requires activeSessionId in this port: {kind}"));
            }
            // `case "cron_list"` (daemon-supervisor.ts:2450-2480): the id-less form aggregates
            // the live workers' jobs and then adds every passive scheduled job
            // (`collectPassiveScheduledJobs`, 2476-2478).
            "cron_list" if body.get("activeSessionId").and_then(Value::as_str).is_none() => {
                let mut jobs: Vec<Value> = Vec::new();
                let mut seen: HashSet<String> = HashSet::new();
                let workers: Vec<Arc<Worker>> = self.workers.lock().unwrap().values().cloned().collect();
                for worker in workers {
                    if !self.is_live_worker(&worker) { continue; }
                    if worker.descriptor.lock().unwrap().lifecycle != DAEMON_WORKER_LIFECYCLE_READY { continue; }
                    let Ok(client) = self.connected_client(&worker).await else { continue; };
                    let Ok(response) = client.request_worker(command("cron_list"), 5_000).await else { continue; };
                    for job in cron_jobs_from_response(&response) {
                        let job_id = job.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                        if seen.insert(job_id) { jobs.push(job); }
                    }
                }
                let include_inactive = body.get("includeInactive").and_then(Value::as_bool) == Some(true);
                for (_, passive) in self.collect_passive_scheduled_jobs(include_inactive).await {
                    let job_id = passive.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                    if seen.insert(job_id) { jobs.push(passive); }
                }
                jobs.sort_by(|left, right| compare_cron_jobs_by_next_run(left, right));
                return Ok(success(Some(json!({"jobs": jobs}))));
            }
            // `case "heartbeats_list"` (daemon-supervisor.ts:2481-2535) with no
            // `activeSessionId` needs per-worker heartbeat snapshots, which this supervisor
            // slice does not cache; the passive half alone would silently drop live heartbeats.
            "heartbeats_list" if body.get("activeSessionId").and_then(Value::as_str).is_none() => {
                return Err("Daemon supervisor command requires activeSessionId in this port: heartbeats_list".to_string());
            }
            // `case "cron_cancel"` (daemon-supervisor.ts:2591-2632): the id-less form searches
            // the live workers for the job, then the passive scheduled stores, then throws.
            "cron_cancel" if body.get("activeSessionId").and_then(Value::as_str).is_none() => {
                let job_id = body.get("jobId").and_then(Value::as_str).ok_or("cron_cancel requires jobId")?.to_string();
                let workers: Vec<Arc<Worker>> = self.workers.lock().unwrap().values().cloned().collect();
                for worker in workers {
                    if !self.is_live_worker(&worker) { continue; }
                    if worker.descriptor.lock().unwrap().lifecycle != DAEMON_WORKER_LIFECYCLE_READY { continue; }
                    let Ok(client) = self.connected_client(&worker).await else { continue; };
                    let mut list = command("cron_list");
                    list.insert("includeInactive".into(), json!(true));
                    let Ok(response) = client.request_worker(list, 5_000).await else { continue; };
                    if !cron_jobs_from_response(&response).iter().any(|job| job.get("id").and_then(Value::as_str) == Some(job_id.as_str())) { continue; }
                    let mut forwarded = body.clone();
                    forwarded.insert("type".into(), json!("cron_cancel"));
                    let mut response = client.request_worker(forwarded, REQUEST_TIMEOUT).await.map_err(|error| error.to_string())?;
                    response.id = id; response.command = kind.clone();
                    return Ok(Some(response));
                }
                if let Some(job) = self.cancel_passive_scheduled_job(&job_id).await {
                    // `broadcastHeartbeatsChanged()` (2627) re-arms the wake timer (:7110).
                    self.schedule_scheduled_session_wake_recompute();
                    return Ok(success(Some(json!({"job": job}))));
                }
                return Err(format!("No cron job found: {job_id}"));
            }
            // `if (command.type === "send_message")` (daemon-supervisor.ts:2707-2779): the
            // a2a path is reachable without an `activeSessionId`. The target is resolved by
            // the ordinary worker lookup; when that fails with an unknown active session the
            // session path is resolved through the catalog and a worker is woken on demand
            // (:2741-2745). `fromActiveSessionId` is optional and only adds the `agentOrigin`
            // family check (:2733-2740, 2754-2756), which is only enforceable when the
            // sender already has a resident worker.
            "send_message" if body.get("activeSessionId").and_then(Value::as_str).is_none() => {
                let target_selector = body.get("targetActiveSessionId").and_then(Value::as_str).ok_or("send_message requires targetActiveSessionId")?.to_string();
                let source = match body.get("fromActiveSessionId").and_then(Value::as_str) {
                    Some(from) => Some(self.find(&public.identity(), from).await.map_err(|error| error.to_string())?),
                    None => None,
                };
                let target = match self.find(&public.identity(), &target_selector).await {
                    Ok(found) => found,
                    Err(error) if error.starts_with("Unknown active session:") => {
                        let cwd = source.as_ref().and_then(|(worker, _)| worker.descriptor.lock().unwrap().create_command.session_path.clone())
                            .or_else(|| self.config.cwd.clone())
                            .unwrap_or_else(|| std::env::current_dir().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default());
                        let session_dir = source.as_ref().and_then(|(worker, _)| worker.descriptor.lock().unwrap().session_dir.clone()).or_else(|| self.config.session_dir.clone());
                        let session_path = self.catalog.resolve(&target_selector, &cwd, session_dir.as_deref()).await.map_err(|catalog_error| {
                            // `Ambiguous session selector` is preserved so a2a senders can tell
                            // it apart from the original lookup failure (:2725-2732).
                            if catalog_error.starts_with("Ambiguous session selector") { catalog_error } else { error.clone() }
                        })?;
                        let create = json!({"type":"create", "sessionPath": session_path, "continueRecent": false});
                        self.create_for_owner(public.identity(), create.as_object().expect("create body is an object")).await?;
                        // `findSummaryInWorker(worker, sessionPath) ?? sessionSummaryFromRosterEntry(root)`
                        // (:2746-2750): the created root is now addressable, so the ordinary
                        // lookup yields the target the wake was for.
                        self.find(&public.identity(), &target_selector).await?
                    }
                    Err(error) => return Err(error),
                };
                let (target_worker, target_active) = target;
                let target_summary = self.summary_for_active(&target_worker, &target_active);
                if let (Some((source_worker, source_active)), true) = (source.as_ref(), body.get("agentOrigin").and_then(Value::as_bool) == Some(true)) {
                    // `assertAgentFamilyReach` on the resolved pair (:2754-2756).
                    if let (Some(source_summary), Some(target_summary)) = (self.summary_for_active(source_worker, source_active), target_summary.as_ref()) {
                        crate::core::agent_messages::assert_agent_family_reach(&self.family_catalog_entry(&source_summary), &self.family_catalog_entry(target_summary))?;
                    }
                }
                let client = self.connected_client(&target_worker).await?;
                let mut forwarded = body.clone();
                forwarded.insert("type".into(), json!("send_message"));
                forwarded.insert("targetActiveSessionId".into(), json!(target_active));
                let mut response = client.request_worker(forwarded, REQUEST_TIMEOUT).await.map_err(|error| error.to_string())?;
                response.id = id; response.command = kind.clone();
                return Ok(Some(response));
            }
            "list_saved_sessions" => {
                let sessions = self.catalog.list(body.get("cwd").and_then(Value::as_str), self.config.session_dir.as_deref(), None).await?;
                return Ok(success(Some(json!({"sessions":sessions.iter().map(serialize_saved_session_info).collect::<Vec<_>>()}))));
            }
            // `case "shutdown"` (daemon-supervisor.ts:2418-2420): the success reply is returned
            // immediately through `setImmediate`, the `daemon_closing` broadcast goes out BEFORE
            // any worker stop (7314-7318), `force` (2419) is honoured by the stop, and a worker
            // stop timeout is swallowed (7327-7334) instead of aborting the shutdown.
            "shutdown" => {
                let force = body.get("force").and_then(Value::as_bool) == Some(true);
                let reply = DaemonResponse::success(id.as_deref(), &kind, None);
                // `setImmediate(() => void this.shutdown(...))` + `return success(...)`
                // (daemon-supervisor.ts:2418-2420): the reply is written before the deferred
                // shutdown body runs, and that body writes `daemon_closing` to every client
                // before it stops any worker (7314-7318).
                public.write(&json!(reply));
                for client in self.clients.lock().unwrap().values() { client.write(&json!({"type":"daemon_closing","reason":"shutdown"})); }
                let supervisor = Arc::clone(self);
                tokio::spawn(async move {
                    let workers: Vec<_> = supervisor.workers.lock().unwrap().values().cloned().collect();
                    for worker in workers {
                        // `stopWorker(worker, true, forceWorkers, true)` in a `Promise.all` whose
                        // `WorkerStopTimeoutError` is logged, not rethrown (7324-7334).
                        if let Err(error) = supervisor.stop_worker(&worker, false, force).await {
                            eprintln!("Worker {} remains tombstoned for recovery after shutdown: {error}", worker.descriptor.lock().unwrap().worker_id);
                        }
                    }
                    supervisor.stopped.cancel();
                });
                return Ok(None);
            }
            "detach" => {
                let active = body.get("activeSessionId").and_then(Value::as_str);
                let targets = if let Some(active) = active { vec![active.to_string()] } else { public.subscriptions.lock().unwrap().iter().cloned().collect() };
                for active in targets { public.subscriptions.lock().unwrap().remove(&active); public.write(&json!({"type":"session_detached","activeSessionId":active})); }
                self.release_client_pauses(public, active).await?;
                return Ok(success(None));
            }
            "release_session_input_pause" => {
                let pause_id = body.get("pauseId").and_then(Value::as_str).ok_or("Session input pause requires pauseId")?.to_string();
                let pause = self.pauses.lock().unwrap().get(&pause_id).cloned();
                let Some(pause) = pause else { return Ok(success(None)); };
                if pause.connection_id != public.connection_id { return Err(format!("Session input pause is owned by another client: {pause_id}")); }
                let requested = body.get("activeSessionId").and_then(Value::as_str).ok_or("Session input pause requires activeSessionId")?;
                if requested != pause.active && requested != pause.requested { return Err(format!("Session input pause belongs to another session: {pause_id}")); }
                body.insert("activeSessionId".into(), json!(pause.active));
                let client = { pause.worker.client.lock().unwrap().clone().ok_or("Session worker is not connected")? };
                let mut response = client.request_worker(body, REQUEST_TIMEOUT).await.map_err(|error| error.to_string())?;
                if response.success { self.pauses.lock().unwrap().remove(&pause_id); }
                response.id = id; return Ok(Some(response));
            }
            // `get_direct_worker_transport` (daemon-supervisor.ts:2159-2165) is refused because
            // `issuePeerTransport` (5082-5146) is not ported: it needs the worker's
            // `worker_register_peer_transport` grant (5120) answered by the worker side
            // (daemon_mode.rs:9201-9231), `worker.peerTransportCapable` from the
            // `peer_transport` capability in the worker_auth response (3629,
            // worker_auth_advertises_roster's sibling `workerAuthAdvertisesPeerTransport` at
            // 612-616), and a `get_daemon_socket_identity` read of the live worker socket
            // (5105-5113). Advertising `direct_peer_transport` without them would hand clients
            // a ticket this supervisor cannot mint, so the capability stays unadvertised and
            // the rejection stays explicit. `prepare_update_restart` / `restart` need the
            // update-restart transaction (2421-2423, 2415-2417).
            "get_direct_worker_transport" | "prepare_update_restart" | "restart" => return Err(format!("Daemon supervisor command is not implemented: {kind}")),
            _ => {}
        }
        let previous_active = body.get("activeSessionId").and_then(Value::as_str).map(str::to_string);
        let requested = if kind == "reattach" { body.get("targetActiveSessionId") } else { body.get("activeSessionId") }.and_then(Value::as_str).ok_or("Daemon command requires activeSessionId")?.to_string();
        let (worker, active) = self.find(&public.identity(), &requested).await?;
        body.insert("activeSessionId".into(), json!(active));
        if matches!(kind.as_str(), "complete_owned_session" | "promote_owned_session") {
            if worker.descriptor.lock().unwrap().owner_client_id.as_deref() != Some(&public.identity()) { return Err("Session is not owned by this client".into()); }
            if kind == "promote_owned_session" {
                { let mut descriptor = worker.descriptor.lock().unwrap(); descriptor.owner_client_id = None; self.persist_worker(&descriptor)?; }
                let summary = self.refresh(&worker).await?.into_iter().find(|summary| summary.get("activeSessionId").or_else(|| summary.get("id")).and_then(Value::as_str) == Some(&active));
                return Ok(success(summary));
            }
            self.stop_worker(&worker, true, false).await?;
            return Ok(success(None));
        }
        if kind == "kill" && worker.descriptor.lock().unwrap().root_active_session_id == active {
            self.stop_worker(&worker, true, false).await?;
            return Ok(success(None));
        }
        let pause_epoch = public.pause_epoch.load(std::sync::atomic::Ordering::SeqCst);
        if kind == "acquire_session_input_pause" {
            let lease_key = body.get("leaseKey").and_then(Value::as_str).ok_or("Session input pause requires leaseKey")?;
            body.insert("leaseKey".into(), json!(serde_json::to_string(&[public.connection_id.as_str(), public.identity().as_str(), lease_key]).map_err(|error| error.to_string())?));
        }
        if matches!(kind.as_str(), "prompt" | "prompt_and_wait" | "cancel_prompt_admission") {
            if let Some(admission) = body.get("admissionId").and_then(Value::as_str) {
                body.insert("admissionId".into(), json!(format!("supervisor-admission:{}:{admission}", public.connection_id)));
            }
        }
        let attaching = matches!(kind.as_str(), "attach" | "reattach");
        if attaching {
            let supports_ui = body.get("supportsExtensionUi").and_then(Value::as_bool) == Some(true) || body.get("capabilities").and_then(Value::as_array).is_some_and(|capabilities| capabilities.iter().any(|value| value.as_str() == Some("extension_ui")));
            public.supports_extension_ui.store(supports_ui, std::sync::atomic::Ordering::SeqCst);
            body.insert("type".into(), json!("attach"));
            // daemon-supervisor.ts:5451-5458 derives the forwarded list from the attaching
            // client's own capabilities (`normalizeCapabilities`, 704-713) and ALWAYS sends
            // `supportsExtensionUi: false`; the worker-side UI decision is made by the
            // supervisor, not by the worker: TS re-asks the worker through `subscribeWorker`
            // with the real flag (3660-3669).
            body.insert("supportsExtensionUi".into(), json!(false));
            // `slim_attach` / `chunked_snapshot` / `history_ranges` stay out of the list
            // although TS:5451-5457 can add them: this port pins full snapshots (see
            // `subscribe`, "Full snapshots avoid a private chunk cache at the public
            // boundary") and reads `response.data.snapshot` directly, so a slim or chunked
            // reply would be a snapshot the supervisor cannot consume.
            body.insert("capabilities".into(), json!(["attach_snapshot", "event_sequence"]));
            if let Some(client_id) = body.get("clientId").and_then(Value::as_str) { *public.id.lock().unwrap() = client_id.to_string(); }
            body.remove("clientId");
            public.subscriptions.lock().unwrap().insert(active.clone());
        }
        let client = self.connected_client(&worker).await?;
        let mut response = client.request(body, REQUEST_TIMEOUT, DaemonClientRequestOptions::default()).await.map_err(|error| error.to_string())?;
        response.id = id; response.command = kind.clone();
        if response.success {
            if let Some(data) = response.data.as_mut() {
                if data.get("sessionId").is_some() {
                    if let Some(summary) = serde_json::from_value::<RosterSessionSummary>(data.clone()).ok() {
                        *data = serde_json::to_value(self.public_summary(&worker, session_summary_from_roster_row(&summary, None, None, None))).unwrap_or(Value::Null);
                    }
                }
                if let Some(summary) = data.get("state").cloned().and_then(|value| serde_json::from_value::<RosterSessionSummary>(value).ok()) {
                    data["state"] = serde_json::to_value(self.public_summary(&worker, session_summary_from_roster_row(&summary, None, None, None))).unwrap_or(Value::Null);
                }
                if let Some(summary) = data.get("snapshot").and_then(|snapshot| snapshot.get("summary")).cloned().and_then(|value| serde_json::from_value::<RosterSessionSummary>(value).ok()) {
                    data["snapshot"]["summary"] = serde_json::to_value(self.public_summary(&worker, session_summary_from_roster_row(&summary, None, None, None))).unwrap_or(Value::Null);
                }
            }
        }
        if kind == "acquire_session_input_pause" && response.success {
            let pause_id = response.data.as_ref().and_then(|data| data.get("pauseId")).and_then(Value::as_str).ok_or("Worker returned an invalid session input pause id")?.to_string();
            let pause = InputPause { connection_id: public.connection_id.clone(), worker: worker.clone(), active: active.clone(), requested: requested.clone() };
            self.pauses.lock().unwrap().insert(pause_id, pause);
            if public.stopped.is_cancelled() || public.pause_epoch.load(std::sync::atomic::Ordering::SeqCst) != pause_epoch {
                self.release_client_pauses(public, Some(&active)).await?;
                return Err("Session input pause acquisition was invalidated before completion".into());
            }
        }
        if attaching {
            if !response.success { public.subscriptions.lock().unwrap().remove(&active); }
            else {
                if let Some(data) = response.data.as_mut().and_then(Value::as_object_mut) {
                    if let Some(client) = data.get_mut("client").and_then(Value::as_object_mut) { client.insert("id".into(), json!(public.id.lock().unwrap().clone())); }
                }
                if let Some(snapshot) = response.data.as_ref().and_then(|data| data.get("snapshot")) {
                    let message = snapshot.get("summary").and_then(|summary| summary.get("streamingMessage")).filter(|value| value.get("role").and_then(Value::as_str) == Some("assistant"))
                        .or_else(|| snapshot.get("messages").and_then(Value::as_array).and_then(|messages| messages.iter().rev().find(|value| value.get("role").and_then(Value::as_str) == Some("assistant"))));
                    if let Some(message) = message.and_then(|value| serde_json::from_value(value.clone()).ok()) { worker.stream.lock().unwrap().seed(&active, message); }
                }
                self.subscribe(&worker, &active).await?;
                if kind == "reattach" { if let Some(previous) = previous_active.filter(|previous| previous != &active) { public.subscriptions.lock().unwrap().remove(&previous); public.write(&json!({"type":"session_detached","activeSessionId":previous})); } }
            }
        }
        Ok(Some(response))
    }
    async fn handle_line(self: Arc<Self>, public: Arc<PublicClient>, line: Vec<u8>) {
        let parsed = parse_command(&line);
        let (body, envelope_client) = match parsed {
            Ok(parsed) => parsed,
            Err(error) => { public.write(&json!(DaemonResponse::failure(None, "parse", &error, None))); return; }
        };
        let id = body.get("id").and_then(Value::as_str).map(str::to_string);
        let kind = body.get("type").and_then(Value::as_str).unwrap_or("dispatch").to_string();
        if let Some(identity) = envelope_client.as_ref().filter(|identity| !identity.is_empty()) { *public.protocol_id.lock().unwrap() = Some(identity.clone()); }
        let journal_identity = envelope_client.map(|identity| if identity.is_empty() { public.identity() } else { identity }).filter(|_| daemon_protocol::is_daemon_mutating_command(&kind) && kind != "ack_result").zip(id.clone());
        if let Err(error) = self.ownership.assert_current().await {
            public.write(&json!(DaemonResponse::failure(id.as_deref(), &kind, &error.to_string(), None))); return;
        }
        if let Some((client, id)) = &journal_identity {
            match self.journal.lock().unwrap().begin(client, id, &kind) {
                Ok(CommandJournalBeginResult::Complete(response)) => { public.write(&json!(response)); return; }
                Ok(CommandJournalBeginResult::Pending) => { public.write(&json!({"type":"response", "command":kind, "id":id, "success":false, "error":"The previous command result is uncertain and was not replayed", "errorInfo":{"code":"command_result_uncertain","clientId":client,"commandId":id}})); return; }
                Ok(CommandJournalBeginResult::New) => {},
                Err(error) => { public.write(&json!(DaemonResponse::failure(id.as_str().into(), &kind, &error, None))); return; }
            }
        }
        let response = match self.dispatch(&public, body).await { Ok(response) => response, Err(error) => Some(DaemonResponse::failure(id.as_deref(), &kind, &error, None)) };
        if let Some(response) = response {
            if let Some((client, id)) = journal_identity {
                let durable = match self.ownership.assert_current().await {
                    Ok(()) => self.journal.lock().unwrap().record_result(&client, &id, response.clone()),
                    Err(error) => Err(error.to_string()),
                };
                if let Err(error) = durable { public.write(&json!(DaemonResponse::failure(Some(&id), &kind, &error, None))); return; }
            }
            public.write(&json!(response));
        }
    }
    fn disconnected(self: &Arc<Self>, public: &Arc<PublicClient>) {
        self.clients.lock().unwrap().remove(&public.connection_id);
        public.roster_subscribed.store(false, Ordering::SeqCst);
        public.roster_resync_pending.store(false, Ordering::SeqCst);
        let owner = public.identity();
        let supervisor = self.clone();
        let public = public.clone();
        tokio::spawn(async move {
            if let Err(error) = supervisor.release_client_pauses(&public, None).await { eprintln!("Disconnected client pause cleanup failed: {error}"); }
            tokio::time::sleep(Duration::from_secs(30)).await;
            if supervisor.clients.lock().unwrap().values().any(|client| client.identity() == owner) { return; }
            let workers: Vec<_> = supervisor.workers.lock().unwrap().values().filter(|worker| worker.descriptor.lock().unwrap().owner_client_id.as_deref() == Some(&owner)).cloned().collect();
            for worker in workers {
                if let Err(error) = supervisor.stop_worker(&worker, true, false).await { eprintln!("Owned worker cleanup failed: {error}"); }
            }
        });
    }
}

/// The TS `servedRows` set holds summary object identities; the port has no
/// such identity, so a row's own JSON is its key (`activeByFile` rows are the
/// same objects, so equal JSON means the same served row).
trait ServedRowKey {
    fn served_row_key(&self) -> String;
}

impl ServedRowKey for SessionSummary {
    fn served_row_key(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// `sessionSummaryFromRosterEntry(entry)`: the slim roster row widened to the
/// daemon `SessionSummary`. `statusLabel`/`lastHeardFromAt` are written only when
/// the row carries them, exactly like the TypeScript's conditional spreads.
fn session_summary_from_roster_row(
    summary: &RosterSessionSummary,
    status: Option<AgentRosterStatus>,
    status_label: Option<&str>,
    last_heard_from_at: Option<&str>,
) -> SessionSummary {
    let mut value = serde_json::to_value(summary).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.insert("sessionActions".into(), json!({"queuedCount": 0, "steering": [], "followUps": []}));
        if let Some(status) = status { object.insert("rosterStatus".into(), json!(status.as_str())); }
        if let Some(label) = status_label { object.insert("statusLabel".into(), json!(label)); }
        if let Some(last_heard_from_at) = last_heard_from_at { object.insert("lastHeardFromAt".into(), json!(last_heard_from_at)); }
    }
    serde_json::from_value(value).unwrap_or_default()
}

/// `sessionSummaryFromRosterEntry(entry)` for an entry that already has its
/// ledger marks; `workerId` is not part of the summary shape.
fn summary_from_entry(entry: &AgentRosterEntry) -> SessionSummary {
    session_summary_from_roster_row(&entry.summary, Some(entry.status), entry.status_label.as_deref(), entry.last_heard_from_at.as_deref())
}

fn recovery_command(descriptor: &DaemonWorkerDescriptor) -> Result<Map<String, Value>, String> {
    let session_file = descriptor.session_file.as_ref().or(descriptor.create_command.session_path.as_ref()).ok_or("Worker has no saved session to recover")?;
    let mut command = command("create");
    command.insert("sessionPath".into(), json!(session_file));
    command.insert("config".into(), json!({"sessionDir":descriptor.session_dir, "telemetryDisabled":descriptor.telemetry_disabled}));
    Ok(command)
}
/// `SUPERVISOR_SERVER_CAPABILITIES` (daemon-supervisor.ts:196-200):
/// `[...DAEMON_DEFAULT_SERVER_CAPABILITIES, "agent_roster", "direct_peer_transport"]`.
///
/// The TypeScript ADDS to the default list. Two additions are served here, the third is not:
///   - `agent_roster`         - served (advertised below); roster rows, subscriptions and
///                              public `roster_update` pushes are implemented.
///   - `client_owned_sessions`- served via `DAEMON_DEFAULT_SERVER_CAPABILITIES` itself; the
///                              owned-session lifecycle (`create` with `lifecycle:
///                              "client_owned"`, `promote_owned_session`,
///                              `complete_owned_session`) is implemented. Dropping it made every
///                              `--print` run fail with "The running Prime Agent daemon does not
///                              support client_owned_sessions" (main.ts:1095-1098).
///   - `direct_peer_transport`- NOT served. `dispatch` rejects
///                              `get_direct_worker_transport`, and TS pairs that command with
///                              `issuePeerTransport` (daemon-supervisor.ts:2159-2165, 5082-5146).
///                              Advertising it would promise a ticket this port never issues, so
///                              callers keep the supervisor link and plain jsonl instead.
///
/// Withheld capabilities. Each one is a promise the supervisor would have to keep, so it is
/// deliberately NOT advertised and callers fall back instead of hanging. This filter is the
/// honest divergence from TS 196-200; the functions that would have to be implemented here are:
///   - `slim_attach`, `chunked_snapshot` - `subscribe` pins full snapshots
///     ("Full snapshots avoid a private chunk cache at the public boundary"), and the
///     attach path reads `response.data.snapshot` directly. TS instead streams through
///     `SnapshotTranscriptCache` (`streamSnapshot`, daemon-supervisor.ts:5697-5776) and replies
///     from `createStreamedAttachResult` (5681-5695). Required: that cache plus the
///     `session_snapshot_begin/chunk/end/failed` writers.
///   - `history_ranges` - needs the supervisor-side `get_history_range` routing and the
///     `history` field on the cached snapshot (TS gates the snapshot on
///     `wantsHistoryRanges`, daemon-supervisor.ts:5424-5436); the worker already serves the
///     command (daemon_mode.rs `get_history_range`), the supervisor does not route it.
///   - `heartbeat_catalog` - needs the worker-wide `heartbeats_list` fan-out with
///     `worker.heartbeatSnapshot` / `heartbeatSnapshotStale` caching
///     (daemon-supervisor.ts:2481-2535) and `broadcastHeartbeatsChanged` (7109-7114).
///   - `authoritative_child_roster` - needs supervisor routing for `get_rlm_children`
///     (`daemon_command_compatibility` gates it at revision 17); the port only forwards
///     commands that carry `activeSessionId`.
///   - `owned_session_recovery_context` - needs `command.recoveryConfig` handling in the
///     owned-worker attach branch (daemon-supervisor.ts:5385-5396).
fn server_capabilities() -> Vec<String> {
    let mut capabilities: Vec<String> = daemon_protocol::DAEMON_DEFAULT_SERVER_CAPABILITIES
        .iter()
        .map(super::super::daemon_client::capability_name)
        .filter(|capability| {
            !matches!(
                capability.as_str(),
                "slim_attach"
                    | "chunked_snapshot"
                    | "history_ranges"
                    | "heartbeat_catalog"
                    | "authoritative_child_roster"
                    | "owned_session_recovery_context"
            )
        })
        .collect();
    // `agent_roster` is advertised only because `roster_subscribe` /
    // `roster_unsubscribe` are now served (`handle_list` merges the catalog for
    // `all`, and busy counts come from the roster rows' real busy state).
    capabilities.push("agent_roster".to_string());
    capabilities
}
/// `cronJobsFromResponse(response)` (daemon-supervisor.ts:640-646).
fn cron_jobs_from_response(response: &DaemonResponse) -> Vec<Value> {
    if !response.success { return Vec::new(); }
    response.data.as_ref().and_then(|data| data.get("jobs")).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// `sortCronJobs(jobs)` (daemon-supervisor.ts:656-669): ascending `nextRunAt`, with
/// jobs that have no `nextRunAt` last.
fn compare_cron_jobs_by_next_run(left: &Value, right: &Value) -> std::cmp::Ordering {
    let next_run = |job: &Value| job.get("nextRunAt").and_then(Value::as_str).map(str::to_string);
    match (next_run(left), next_run(right)) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(left), Some(right)) => left.cmp(&right),
    }
}

/// `DEFERRED_RECOVERY_RECHECK_MS` / `MAX_DEFERRED_RECOVERY_ROUNDS`
/// (daemon-supervisor.ts:211-213, values shared with daemon_supervisor.rs:121-123).
const DEFERRED_RECOVERY_RECHECK_MS: u64 = 5000;
const MAX_DEFERRED_RECOVERY_ROUNDS: u64 = 10;

/// The three verdicts of `processIdentity(pid, processStartId)`
/// (daemon-supervisor.ts:4246, 4287-4292): `Current`, `Unknown` (alive but not provably ours)
/// and `Gone`/`Replaced`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProcessIdentityVerdict { Current, Unknown, Gone }

fn process_identity_verdict(identity: &ProcessIdentity) -> ProcessIdentityVerdict {
    if !is_process_alive(identity.pid as i32) { return ProcessIdentityVerdict::Gone; }
    if identity.process_start_id.is_none() { return ProcessIdentityVerdict::Unknown; }
    if get_process_start_id(identity.pid).as_deref() == identity.process_start_id.as_deref() { ProcessIdentityVerdict::Current } else { ProcessIdentityVerdict::Gone }
}

/// `Date.parse(value)`, `undefined` when unparseable (the TS guards with `Number.isFinite`).
fn iso_to_ms(value: &str) -> Option<f64> {
    let millis = crate::core::cron_jobs::parse_iso_date(value);
    millis.is_finite().then_some(millis)
}

fn descriptor_key(socket: &str) -> String { format!("{:x}", Sha256::digest(socket.as_bytes()))[..12].to_string() }
fn worker_socket(supervisor: &str, worker: &str) -> String {
    let key = descriptor_key(supervisor);
    #[cfg(windows)] { format!(r"\\.\pipe\prime-agent-worker-{key}-{}", &worker[..worker.len().min(12)]) }
    #[cfg(unix)] { Path::new(&default_daemon_socket_dir()).join(format!("worker-{key}-{}.sock", &worker[..worker.len().min(12)])).to_string_lossy().into_owned() }
}
fn visible(worker: &Worker, client: &str) -> bool { worker.descriptor.lock().unwrap().owner_client_id.as_deref().map_or(true, |owner| owner == client) }

/// The public `roster_update` outbound (`{ type, changed, removed?, resync? }`).
fn roster_update_value(changed: &[AgentRosterEntry], removed: &[String], resync: Option<bool>) -> Value {
    let mut value = json!({
        "type": "roster_update",
        "changed": changed.iter().map(agent_roster_entry_to_value).collect::<Vec<_>>(),
    });
    if let Some(object) = value.as_object_mut() {
        if !removed.is_empty() { object.insert("removed".into(), json!(removed)); }
        if let Some(resync) = resync { object.insert("resync".into(), json!(resync)); }
    }
    value
}

/// `handleWorkerFrame`'s `worker.lastFrameAt = Date.now()`.
fn supervisor_now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_millis() as u64).unwrap_or(0)
}

/// `new Date(ms).toISOString()` with the daemon's millisecond precision.
fn iso_from_ms(ms: f64) -> String {
    let millis = ms as i64;
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// `workerAuthAdvertisesRoster(data)`.
fn worker_auth_advertises_roster(data: Option<&Value>) -> bool {
    data.and_then(|value| value.get("capabilities")).and_then(Value::as_array)
        .is_some_and(|capabilities| capabilities.iter().any(|value| value.as_str() == Some(DAEMON_WORKER_ROSTER_CAPABILITY)))
}

/// `rosterParentByChild(edges)` plus `rosterFamilyDescendsFrom`'s walk:
/// membership at any step of the parent chain, never just the ultimate root.
fn roster_parent_by_child(edges: &[RlmLedgerEdge]) -> HashMap<String, String> {
    edges.iter()
        .map(|edge| (canonical_session_path(&edge.child), canonical_session_path(&edge.parent)))
        .collect()
}

fn roster_path_descends_from(parent_by_child: &HashMap<String, String>, path: &str, roots: &HashSet<String>) -> bool {
    let mut visited: HashSet<String> = HashSet::new();
    let mut current = path.to_string();
    while !visited.contains(&current) {
        if roots.contains(&current) { return true; }
        visited.insert(current.clone());
        match parent_by_child.get(&current) {
            Some(parent) => current = parent.clone(),
            None => return false,
        }
    }
    false
}

/// `rosterEntryForSpawnLedgerEdge(edge)`.
fn roster_entry_for_spawn_ledger_edge(edge: &RlmLedgerEdge) -> WorkerRosterEntry {
    let persisted_session_id = Path::new(&edge.child).file_stem().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
    let summary = RosterSessionSummary {
        id: persisted_session_id.clone(),
        lifecycle: "live".to_string(),
        activity: "idle".to_string(),
        is_session_active: false,
        runtime_kind: Some("subagent".to_string()),
        rlm_depth: Some(edge.depth),
        session_id: persisted_session_id,
        session_file: Some(edge.child.clone()),
        session_name: (!edge.name.is_empty()).then(|| edge.name.clone()),
        cwd: Path::new(&edge.child).parent().map(|parent| parent.to_string_lossy().to_string()).unwrap_or_default(),
        is_streaming: false,
        is_compacting: false,
        attached_clients: 0,
        message_count: 0,
        parent_session_path: Some(edge.parent.clone()),
        rlm_child_id: Some(edge.child_id.clone()),
        ..RosterSessionSummary::default()
    };
    WorkerRosterEntry { agent_id: roster_agent_id_for_entry(&summary), queued_child: None, seeded_cwd: None, summary }
}

/// `hydrateSeededEntry(entry)`: only a row still marked `seededCwd` is re-read,
/// and only while it is still the store's current row.
async fn hydrate_seeded_roster_entry(entry: AgentRosterEntry, supervisor: &Supervisor) -> AgentRosterEntry {
    if entry.seeded_cwd != Some(true) || entry.summary.session_file.is_none() { return entry; }
    let worker_entry = WorkerRosterEntry { agent_id: entry.agent_id.clone(), queued_child: entry.queued_child, seeded_cwd: entry.seeded_cwd, summary: entry.summary.clone() };
    let hydrated = hydrated_seed_entry(&worker_entry).await;
    if hydrated.seeded_cwd == Some(true) { return entry; }
    let current = supervisor.roster().lock().unwrap().get(&entry.agent_id);
    match current {
        Some(current) if current.summary != entry.summary || current.status != entry.status => current,
        None => entry,
        Some(_) => supervisor.write_roster_entry(hydrated, None, entry.status_label.as_deref()),
    }
}

/// `hydratedSeedEntry(entry)`: fill `cwd` from the saved transcript when it exists.
async fn hydrated_seed_entry(entry: &WorkerRosterEntry) -> WorkerRosterEntry {
    let Some(session_file) = entry.summary.session_file.clone() else { return with_seeded_cwd(entry); };
    let Some(info) = crate::core::session_manager::read_session_info(&session_file).await else { return with_seeded_cwd(entry); };
    let mut hydrated = entry.clone();
    hydrated.seeded_cwd = None;
    hydrated.summary.cwd = info.cwd;
    hydrated
}

fn with_seeded_cwd(entry: &WorkerRosterEntry) -> WorkerRosterEntry {
    let mut seeded = entry.clone();
    seeded.seeded_cwd = Some(true);
    seeded
}

/// `matchesListSessionDir(summary, sessionDir, spawnParents)`.
fn matches_list_session_dir(summary: &SessionSummary, session_dir: Option<&str>, spawn_parents: &HashMap<String, String>) -> bool {
    let Some(session_dir) = session_dir else { return true; };
    let Some(session_file) = summary.session_file.as_ref() else { return false; };
    let mut file = resolve_path(session_file);
    let mut parent_session_path = summary.parent_session_path.clone();
    let mut visited: HashSet<String> = HashSet::new();
    while let Some(parent) = parent_session_path {
        let canonical = canonical_session_path(&parent);
        if visited.contains(&canonical) { break; }
        visited.insert(canonical.clone());
        file = resolve_path(&parent);
        parent_session_path = spawn_parents.get(&canonical).cloned();
    }
    Path::new(&file).parent().map(|parent| parent.to_string_lossy().to_string()).unwrap_or_default()
        == resolve_path(session_dir)
}

fn resolve_path(path: &str) -> String {
    let candidate = Path::new(path);
    if candidate.is_absolute() { candidate.to_string_lossy().to_string() }
    else {
        std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
            .join(candidate).to_string_lossy().to_string()
    }
}

/// `isSessionSummary(value)`: the daemon's summary guard for worker list payloads.
fn is_session_summary_value(value: &Value) -> bool {
    matches!(
        (value.get("id").and_then(Value::as_str), value.get("sessionId").and_then(Value::as_str), value.get("cwd").and_then(Value::as_str)),
        (Some(_), Some(_), Some(_))
    )
}

/// `sessionSummariesFromResponse(response)`: the worker `list` payload, validated.
fn session_summaries_from_response(response: DaemonResponse) -> Result<Vec<RosterSessionSummary>, String> {
    let data = response_data(response)?;
    let sessions = data.get("sessions").and_then(Value::as_array).ok_or("Session worker returned an invalid list response")?;
    if !sessions.iter().all(is_session_summary_value) { return Err("Session worker returned an invalid list response".to_string()); }
    Ok(sessions.iter().filter_map(|value| serde_json::from_value::<RosterSessionSummary>(value.clone()).ok()).collect())
}

/// `workerRosterEntryFromSummary` over the worker's wire summary.
fn worker_roster_entry_from_value(summary: &Value) -> Option<WorkerRosterEntry> {
    let summary: RosterSessionSummary = serde_json::from_value(summary.clone()).ok()?;
    Some(worker_roster_entry_from_summary(&summary))
}
fn command(kind: &str) -> Map<String, Value> { json!({"type":kind}).as_object().unwrap().clone() }
fn response_data(response: DaemonResponse) -> Result<Value, String> { if response.success { Ok(response.data.unwrap_or(Value::Null)) } else { Err(response.error.unwrap_or_else(|| "Session worker request failed".to_string())) } }
fn persist_json(path: &Path, value: &Value) -> Result<(), String> {
    write_file_atomic_sync(&path.to_string_lossy(), &serde_json::to_string(value).map_err(|error| error.to_string())?, WriteFileAtomicOptions { mode: Some(0o600), fsync: true, fsync_dir: true, ..Default::default() }).map_err(|error| error.to_string())
}
fn parse_command(line: &[u8]) -> Result<(Map<String, Value>, Option<String>), String> {
    let value: Value = serde_json::from_slice(line).map_err(|error| error.to_string())?;
    let object = value.as_object().ok_or("Invalid daemon command")?;
    if object.get("type").and_then(Value::as_str) == Some("command") {
        if !daemon_protocol::is_daemon_command_envelope(&value) { return Err("Invalid daemon command envelope".into()); }
        let mut command = object.get("command").and_then(Value::as_object).ok_or("Invalid daemon command envelope")?.clone();
        command.insert("id".into(), object.get("id").cloned().ok_or("Daemon command requires id")?);
        Ok((command, Some(object.get("clientId").and_then(Value::as_str).unwrap_or("").to_string())))
    } else { Ok((object.clone(), None)) }
}
fn spawn_connection<S>(supervisor: Arc<Supervisor>, stream: S) where S: AsyncRead + AsyncWrite + Unpin + Send + 'static {
    tokio::spawn(async move {
        let (mut input, mut output) = tokio::io::split(stream);
        let (sender, mut receiver) = mpsc::channel::<Vec<u8>>(1024);
        let identity = create_active_session_id(None);
        let public = Arc::new(PublicClient {
            connection_id: identity.clone(), id: Mutex::new(identity.clone()), protocol_id: Mutex::new(None),
            subscriptions: Mutex::new(HashSet::new()), supports_extension_ui: AtomicBool::new(false), pause_epoch: AtomicU64::new(0),
            output: sender, stopped: CancellationToken::new(), roster_subscribed: AtomicBool::new(false),
            roster_resync_pending: AtomicBool::new(false), backpressured: AtomicBool::new(false),
        });
        supervisor.clients.lock().unwrap().insert(identity, public.clone());
        public.write(&supervisor.hello(&public));
        let cancelled = public.stopped.clone();
        let writer_public = Arc::clone(&public);
        let writer_supervisor = Arc::clone(&supervisor);
        let writer = tokio::spawn(async move {
            loop {
                tokio::select! { biased;
                    bytes = receiver.recv() => match bytes {
                        Some(bytes) => {
                            if output.write_all(&bytes).await.is_err() { break; }
                            // The socket drained: a resync deferred under backpressure
                            // is written once, covering the whole loss gap.
                            if writer_public.backpressured.swap(false, Ordering::SeqCst)
                                && writer_public.roster_subscribed.load(Ordering::SeqCst)
                                && writer_public.roster_resync_pending.swap(false, Ordering::SeqCst)
                            {
                                let changed = writer_supervisor.roster_entries_for_client();
                                writer_public.write(&roster_update_value(&changed, &[], Some(true)));
                            }
                        },
                        None => break,
                    },
                    _ = cancelled.cancelled() => break,
                }
            }
            cancelled.cancel(); let _ = output.shutdown().await;
        });
        let mut pending = Vec::new(); let mut buffer = [0u8; 8192];
        loop {
            let read = tokio::select! { _ = supervisor.stopped.cancelled() => break, _ = public.stopped.cancelled() => break, read = input.read(&mut buffer) => read };
            let size = match read { Ok(0) | Err(_) => break, Ok(size) => size };
            for &byte in &buffer[..size] {
                if byte == b'\n' {
                    if !pending.is_empty() { tokio::spawn(supervisor.clone().handle_line(public.clone(), std::mem::take(&mut pending))); }
                } else { pending.push(byte); if pending.len() > MAX_PUBLIC_LINE { public.stopped.cancel(); break; } }
            }
        }
        public.stopped.cancel(); let _ = writer.await;
        supervisor.disconnected(&public);
    });
}

#[cfg(unix)]
fn spawn_worker_process(socket: &str, cwd: Option<&str>, mut environment: HashMap<String, String>) -> Result<(tokio::process::Child, std::fs::File), String> {
    use std::os::fd::{AsRawFd, OwnedFd};
    // std creates close-on-exec handles, so concurrent process launches cannot
    // inherit the parent gate and keep the worker waiting for EOF.
    let (read, write) = std::os::unix::net::UnixStream::pair().map_err(|error| error.to_string())?;
    let read = std::fs::File::from(OwnedFd::from(read));
    let write = std::fs::File::from(OwnedFd::from(write));
    environment.insert(DAEMON_WORKER_STARTUP_GATE_FD_ENV.to_string(), "3".to_string());
    let mut process = tokio::process::Command::new(std::env::current_exe().map_err(|error| error.to_string())?);
    process.args(["--mode", "daemon", "--daemon-socket", socket]).env_clear().envs(environment)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::inherit());
    if let Some(cwd) = cwd { process.current_dir(cwd); }
    let fd = read.as_raw_fd();
    unsafe { process.pre_exec(move || {
        if libc::setsid() == -1 { return Err(std::io::Error::last_os_error()); }
        if fd != 3 && libc::dup2(fd, 3) == -1 { return Err(std::io::Error::last_os_error()); }
        if libc::fcntl(3, libc::F_SETFD, 0) == -1 { return Err(std::io::Error::last_os_error()); }
        Ok(())
    }); }
    let child = process.spawn().map_err(|error| error.to_string())?;
    drop(read);
    Ok((child, write))
}
#[cfg(windows)]
fn spawn_worker_process(_socket: &str, _cwd: Option<&str>, _environment: HashMap<String, String>) -> Result<(tokio::process::Child, tokio::process::ChildStdin), String> {
    let mut environment = _environment;
    environment.insert(DAEMON_WORKER_STARTUP_GATE_FD_ENV.to_string(), "stdin".to_string());
    let mut process = tokio::process::Command::new(std::env::current_exe().map_err(|error| error.to_string())?);
    process.args(["--mode", "daemon", "--daemon-socket", _socket]).env_clear().envs(environment)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::inherit());
    if let Some(cwd) = _cwd { process.current_dir(cwd); }
    process.creation_flags(0x00000200 | 0x08000000);
    let mut child = process.spawn().map_err(|error| error.to_string())?;
    let gate = child.stdin.take().ok_or("Failed to create daemon worker startup gate")?;
    Ok((child, gate))
}
#[cfg(unix)]
async fn commit_gate(mut gate: std::fs::File) -> Result<(), String> { std::io::Write::write_all(&mut gate, DAEMON_WORKER_STARTUP_GATE_COMMIT.as_bytes()).map_err(|error| error.to_string()) }
#[cfg(windows)]
async fn commit_gate(mut gate: tokio::process::ChildStdin) -> Result<(), String> {
    gate.write_all(DAEMON_WORKER_STARTUP_GATE_COMMIT.as_bytes()).await.map_err(|error| error.to_string())?;
    gate.shutdown().await.map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptor_and_socket_paths_match_the_typescript_hash_contract() {
        assert_eq!(descriptor_key("/tmp/daemon.sock"), format!("{:x}", Sha256::digest(b"/tmp/daemon.sock"))[..12]);
        assert!(worker_socket("/tmp/daemon.sock", "abcdef1234567890").contains("abcdef123456"));
    }
    #[test]
    fn command_envelopes_preserve_public_request_and_client_identity() {
        let wire = json!({"type":"command", "id":"request-1", "clientId":"client-1", "protocol":daemon_protocol::daemon_protocol_info(), "command":{"type":"list"}});
        let (body, client) = parse_command(&serde_json::to_vec(&wire).unwrap()).unwrap();
        assert_eq!(body["id"], "request-1"); assert_eq!(client.as_deref(), Some("client-1"));
    }
}
