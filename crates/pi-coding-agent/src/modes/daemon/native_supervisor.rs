//! Native transport/process adapter for daemon-supervisor.ts.
//!
//! Each resident root runs in a separately authenticated worker process. Public
//! clients share the supervisor connection, never the worker's secret or socket.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;
use crate::core::agent_session_config::{AgentSessionRuntimeConfig, durable_agent_session_runtime_config, merge_agent_session_runtime_config};
use crate::core::session_lease::get_process_start_id;
use crate::utils::atomic_file::{write_file_atomic_sync, remove_file_durably, RemoveFileDurablyOptions, WriteFileAtomicOptions};
use super::super::active_session_state::create_active_session_id;
use super::super::command_recovery_journal::{CommandRecoveryJournal, CommandJournalBeginResult};
use super::super::compact_session_stream::{CompactAssistantStreamReconstructor, CompactAssistantDelta};
use super::super::daemon_catalog_process::{DaemonCatalogClient, DAEMON_CATALOG_ROLE_ENV};
use super::super::daemon_client::DaemonClientRequestOptions;
use super::super::daemon_protocol::{self, DaemonResponse};
use super::super::daemon_socket::*;
use super::super::daemon_supervisor_ownership::*;
use super::super::daemon_worker_client::{DaemonWorkerClient, PrivateFrame};
use super::super::daemon_worker_protocol::*;
use super::super::saved_session_info::serialize_saved_session_info;

const REQUEST_TIMEOUT: u64 = 24 * 60 * 60 * 1000;
const MAX_PUBLIC_LINE: usize = super::super::daemon_client::DAEMON_MAX_LINE_LENGTH;

struct PublicClient {
    connection_id: String,
    id: Mutex<String>,
    protocol_id: Mutex<Option<String>>,
    subscriptions: Mutex<HashSet<String>>,
    supports_extension_ui: std::sync::atomic::AtomicBool,
    pause_epoch: std::sync::atomic::AtomicU64,
    output: mpsc::Sender<Vec<u8>>,
    stopped: CancellationToken,
}
impl PublicClient {
    fn write(&self, value: &Value) {
        if let Ok(mut bytes) = serde_json::to_vec(value) {
            bytes.push(b'\n');
            if self.output.try_send(bytes).is_err() { self.stopped.cancel(); }
        }
    }
    fn identity(&self) -> String { self.protocol_id.lock().unwrap().clone().unwrap_or_else(|| self.id.lock().unwrap().clone()) }
}
struct Worker {
    descriptor: Mutex<DaemonWorkerDescriptor>,
    client: Arc<DaemonWorkerClient>,
    summaries: Mutex<Vec<Value>>,
    connection: AsyncMutex<()>,
    stream: Mutex<CompactAssistantStreamReconstructor>,
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
    });
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
    for worker in workers { worker.client.close().await; }
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
        client.authenticate_worker(&descriptor.authentication_token, &claim, super::WORKER_CONNECT_TIMEOUT_MS).await.map_err(|error| error.to_string())?;
        Ok(())
    }
    fn install_worker(self: &Arc<Self>, descriptor: DaemonWorkerDescriptor, client: Arc<DaemonWorkerClient>) -> Arc<Worker> {
        let worker = Arc::new(Worker { descriptor: Mutex::new(descriptor), client, summaries: Mutex::new(vec![]), connection: AsyncMutex::new(()), stream: Mutex::new(CompactAssistantStreamReconstructor::new()) });
        let weak = Arc::downgrade(self);
        let weak_worker = Arc::downgrade(&worker);
        let _unsubscribe = worker.client.on_frame(Arc::new(move |frame| {
            if let (Some(supervisor), Some(worker)) = (weak.upgrade(), weak_worker.upgrade()) { supervisor.forward_frame(&worker, frame); }
        }));
        self.workers.lock().unwrap().insert(worker.descriptor.lock().unwrap().worker_id.clone(), worker.clone());
        worker
    }
    fn forward_frame(&self, worker: &Worker, frame: &PrivateFrame) {
        let kind = frame.header.get("outboundType").and_then(Value::as_str).unwrap_or("");
        if matches!(kind, "daemon_hello" | "response" | "roster_delta" | "roster_heartbeat") { return; }
        let Ok(mut value) = serde_json::from_slice::<Value>(&frame.payload) else { return; };
        if frame.header.get("payloadEncoding").and_then(Value::as_str) == Some("assistant-delta") {
            let Ok(delta) = serde_json::from_value::<CompactAssistantDelta>(value) else { return; };
            let Some(reconstructed) = worker.stream.lock().unwrap().reconstruct(&delta) else { return; };
            value = reconstructed;
        } else { worker.stream.lock().unwrap().observe(&value); }
        if let Some(summary) = value.get("state").filter(|value| value.get("sessionId").is_some()).cloned() { value["state"] = self.public_summary(worker, summary); }
        let active = frame.header.get("activeSessionId").and_then(Value::as_str).or_else(|| value.get("activeSessionId").and_then(Value::as_str));
        let owner = worker.descriptor.lock().unwrap().owner_client_id.clone();
        for client in self.clients.lock().unwrap().values() {
            if owner.as_ref().is_some_and(|owner| owner != &client.identity()) { continue; }
            if active.is_some_and(|id| client.subscriptions.lock().unwrap().contains(id)) { client.write(&value); }
        }
    }
    async fn refresh(&self, worker: &Arc<Worker>) -> Result<Vec<Value>, String> {
        if !worker.client.is_connected() {
            let _connection = worker.connection.lock().await;
            if !worker.client.is_connected() {
                let descriptor = worker.descriptor.lock().unwrap().clone();
                if !matches_exact_process_identity(&ProcessIdentity { pid: descriptor.pid as i64, process_start_id: descriptor.process_start_id.clone() }) {
                    return Err(format!("Session worker {} exited; retry_worker is required", descriptor.worker_id));
                }
                self.authenticate(&worker.client, &descriptor).await?;
                self.subscribe(worker, &descriptor.root_active_session_id).await?;
            }
        }
        let response = worker.client.request_worker(command("list"), REQUEST_TIMEOUT).await.map_err(|error| error.to_string())?;
        let sessions = response_data(response)?.get("sessions").and_then(Value::as_array).cloned().ok_or("Session worker returned an invalid list response")?;
        if !sessions.iter().all(|session| session.get("id").and_then(Value::as_str).is_some() && session.get("sessionId").and_then(Value::as_str).is_some()) {
            return Err("Session worker returned an invalid list response".to_string());
        }
        *worker.summaries.lock().unwrap() = sessions.clone();
        Ok(sessions)
    }
    async fn subscribe(&self, worker: &Arc<Worker>, active: &str) -> Result<(), String> {
        let mut body = command("worker_subscribe");
        body.insert("activeSessionId".into(), json!(active));
        // Full snapshots avoid a private chunk cache at the public boundary.
        let supports_ui = self.clients.lock().unwrap().values().any(|client| client.subscriptions.lock().unwrap().contains(active) && client.supports_extension_ui.load(std::sync::atomic::Ordering::SeqCst));
        let capabilities = if supports_ui { vec!["attach_snapshot", "event_sequence", "extension_ui"] } else { vec!["attach_snapshot", "event_sequence"] };
        body.insert("capabilities".into(), json!(capabilities));
        body.insert("supportsExtensionUi".into(), json!(supports_ui));
        response_data(worker.client.request_worker(body, REQUEST_TIMEOUT).await.map_err(|error| error.to_string())?).map(|_| ())
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
            self.authenticate(&client, &descriptor).await?;
            let root = descriptor.root_active_session_id.clone();
            let worker = self.install_worker(descriptor, client);
            self.refresh(&worker).await?;
            self.subscribe(&worker, &root).await?;
        }
        Ok(())
    }
    async fn create(self: &Arc<Self>, public: &PublicClient, body: &Map<String, Value>) -> Result<Value, String> {
        let _opening = self.opening.lock().await;
        let owner = public.identity();
        if let Some(path) = body.get("sessionPath").and_then(Value::as_str) {
            let workers: Vec<_> = self.workers.lock().unwrap().values().cloned().collect();
            for worker in workers {
                if !visible(&worker, &owner) { continue; }
                if let Some(summary) = self.refresh(&worker).await?.into_iter().find(|summary| summary.get("sessionFile").and_then(Value::as_str) == Some(path)) { return Ok(summary); }
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
            commit_gate(gate).await?;
            let client = Arc::new(DaemonWorkerClient::new(&socket));
            self.authenticate(&client, &descriptor).await?;
            let mut forwarded = body.clone(); forwarded.remove("id"); forwarded.remove("launchEnv"); forwarded.remove("lifecycle");
            forwarded.insert("config".into(), serde_json::to_value(config).map_err(|error| error.to_string())?);
            let summary = response_data(client.request_worker(forwarded, REQUEST_TIMEOUT).await.map_err(|error| error.to_string())?)?;
            if summary.get("activeSessionId").or_else(|| summary.get("id")).and_then(Value::as_str) != Some(&root) { return Err("Session worker did not preserve its assigned active session id".into()); }
            descriptor.root_session_id = summary.get("sessionId").and_then(Value::as_str).map(str::to_string);
            descriptor.session_file = summary.get("sessionFile").and_then(Value::as_str).map(str::to_string);
            descriptor.lifecycle = "ready".into();
            self.persist_worker(&descriptor)?;
            let worker = self.install_worker(descriptor.clone(), client);
            self.subscribe(&worker, &root).await?;
            self.refresh(&worker).await?;
            Ok(self.public_summary(&worker, summary))
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
    async fn find(&self, identity: &str, requested: &str) -> Result<(Arc<Worker>, String), String> {
        let workers: Vec<_> = self.workers.lock().unwrap().values().cloned().collect();
        let mut matches = Vec::new();
        for worker in workers {
            if !visible(&worker, identity) { continue; }
            for summary in self.refresh(&worker).await? {
                let active = summary.get("activeSessionId").or_else(|| summary.get("id")).and_then(Value::as_str).unwrap_or("");
                if [Some(active), summary.get("sessionId").and_then(Value::as_str), summary.get("sessionFile").and_then(Value::as_str)].into_iter().flatten().any(|value| value == requested) {
                    matches.push((worker.clone(), active.to_string()));
                }
            }
        }
        match matches.len() { 1 => Ok(matches.remove(0)), 0 => Err(format!("Unknown active session: {requested}")), _ => Err(format!("Ambiguous active session: {requested}")) }
    }
    async fn release_client_pauses(&self, public: &PublicClient, active: Option<&str>) -> Result<(), String> {
        public.pause_epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let entries: Vec<_> = self.pauses.lock().unwrap().iter().filter(|(_, pause)| pause.connection_id == public.connection_id && active.map_or(true, |active| active == pause.active || active == pause.requested)).map(|(id, pause)| (id.clone(), pause.clone())).collect();
        for (id, pause) in entries {
            let mut body = command("release_session_input_pause");
            body.insert("activeSessionId".into(), json!(pause.active)); body.insert("pauseId".into(), json!(id));
            response_data(pause.worker.client.request_worker(body, 5_000).await.map_err(|error| error.to_string())?)?;
            self.pauses.lock().unwrap().remove(&id);
        }
        Ok(())
    }
    fn public_summary(&self, worker: &Worker, mut summary: Value) -> Value {
        let active = summary.get("activeSessionId").or_else(|| summary.get("id")).and_then(Value::as_str).unwrap_or("");
        let attached = self.clients.lock().unwrap().values().filter(|client| client.subscriptions.lock().unwrap().contains(active)).count();
        if let Some(summary) = summary.as_object_mut() {
            let descriptor = worker.descriptor.lock().unwrap();
            summary.insert("attachedClients".into(), json!(attached));
            summary.insert("workerState".into(), json!(descriptor.lifecycle));
            summary.insert("workerPid".into(), json!(descriptor.pid));
        }
        summary
    }
    async fn stop_worker(&self, worker: &Arc<Worker>, archive: bool) -> Result<(), String> {
        self.ownership.assert_current().await.map_err(|error| error.to_string())?;
        let descriptor = {
            let mut descriptor = worker.descriptor.lock().unwrap();
            descriptor.lifecycle = "stopping".into();
            descriptor.stop_requested_at = Some(chrono::Utc::now().to_rfc3339());
            descriptor.archive_on_stop = Some(archive);
            self.persist_worker(&descriptor)?;
            descriptor.clone()
        };
        let kind = if archive { "worker_archive_and_shutdown" } else { "shutdown" };
        response_data(worker.client.request_worker(command(kind), 30_000).await.map_err(|error| error.to_string())?)?;
        let identity = ProcessIdentity { pid: descriptor.pid as i64, process_start_id: descriptor.process_start_id.clone() };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while matches_exact_process_identity(&identity) {
            if tokio::time::Instant::now() >= deadline { return Err(format!("Session worker {} is still stopping", descriptor.worker_id)); }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        worker.client.close().await;
        self.workers.lock().unwrap().remove(&descriptor.worker_id);
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
                let workers: Vec<_> = self.workers.lock().unwrap().values().cloned().collect();
                let mut sessions = vec![];
                for worker in workers { if visible(&worker, &public.identity()) { sessions.extend(self.refresh(&worker).await?.into_iter().map(|summary| self.public_summary(&worker, summary))); } }
                return Ok(success(Some(json!({"sessions":sessions}))));
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
            "list_saved_sessions" => {
                let sessions = self.catalog.list(body.get("cwd").and_then(Value::as_str), self.config.session_dir.as_deref(), None).await?;
                return Ok(success(Some(json!({"sessions":sessions.iter().map(serialize_saved_session_info).collect::<Vec<_>>()}))));
            }
            "shutdown" => {
                let workers: Vec<_> = self.workers.lock().unwrap().values().cloned().collect();
                for worker in workers { self.stop_worker(&worker, false).await?; }
                public.write(&json!(DaemonResponse::success(id.as_deref(), &kind, None)));
                self.stopped.cancel(); return Ok(None);
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
                let mut response = pause.worker.client.request_worker(body, REQUEST_TIMEOUT).await.map_err(|error| error.to_string())?;
                if response.success { self.pauses.lock().unwrap().remove(&pause_id); }
                response.id = id; return Ok(Some(response));
            }
            "get_direct_worker_transport" | "prepare_update_restart" | "restart" | "roster_subscribe" | "roster_unsubscribe" => return Err(format!("Daemon supervisor command is not implemented: {kind}")),
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
            self.stop_worker(&worker, true).await?;
            return Ok(success(None));
        }
        if kind == "kill" && worker.descriptor.lock().unwrap().root_active_session_id == active {
            self.stop_worker(&worker, true).await?;
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
            body.insert("capabilities".into(), json!(["attach_snapshot", "event_sequence"]));
            if let Some(client_id) = body.get("clientId").and_then(Value::as_str) { *public.id.lock().unwrap() = client_id.to_string(); }
            body.remove("clientId");
            public.subscriptions.lock().unwrap().insert(active.clone());
        }
        let mut response = worker.client.request(body, REQUEST_TIMEOUT, DaemonClientRequestOptions::default()).await.map_err(|error| error.to_string())?;
        response.id = id; response.command = kind.clone();
        if response.success {
            if let Some(data) = response.data.as_mut() {
                if data.get("sessionId").is_some() { *data = self.public_summary(&worker, data.clone()); }
                if let Some(summary) = data.get("state").cloned() { data["state"] = self.public_summary(&worker, summary); }
                if let Some(summary) = data.get("snapshot").and_then(|snapshot| snapshot.get("summary")).cloned() { data["snapshot"]["summary"] = self.public_summary(&worker, summary); }
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
        let owner = public.identity();
        let supervisor = self.clone();
        let public = public.clone();
        tokio::spawn(async move {
            if let Err(error) = supervisor.release_client_pauses(&public, None).await { eprintln!("Disconnected client pause cleanup failed: {error}"); }
            tokio::time::sleep(Duration::from_secs(30)).await;
            if supervisor.clients.lock().unwrap().values().any(|client| client.identity() == owner) { return; }
            let workers: Vec<_> = supervisor.workers.lock().unwrap().values().filter(|worker| worker.descriptor.lock().unwrap().owner_client_id.as_deref() == Some(&owner)).cloned().collect();
            for worker in workers {
                if let Err(error) = supervisor.stop_worker(&worker, true).await { eprintln!("Owned worker cleanup failed: {error}"); }
            }
        });
    }
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
/// The TypeScript ADDS to the default list. The port must not subtract from it: every
/// capability is a promise the supervisor keeps, and the client gates real commands on it.
/// Dropping `client_owned_sessions` in particular made every `--print` run fail with
/// "The running Prime Agent daemon does not support client_owned_sessions" (main.ts:1095-1098),
/// even though the supervisor implements the whole owned-session lifecycle
/// (`create` with `lifecycle: "client_owned"`, `promote_owned_session`, `complete_owned_session`).
///
/// Capabilities this supervisor genuinely does not serve yet are deliberately NOT advertised,
/// so callers fall back instead of hanging. Each one is a known gap:
///   - `agent_roster`         - roster_subscribe is still rejected (see the command dispatch).
///   - `direct_peer_transport` - `get_direct_worker_transport` is still rejected; clients then
///                              keep the supervisor link and use plain jsonl.
///   - `slim_attach`, `chunked_snapshot`, `history_ranges` - snapshot transfer modes the
///     supervisor still serves as full snapshots.
///   - `heartbeat_catalog`, `authoritative_child_roster`, `owned_session_recovery_context` -
///     roster/heartbeat and recovery-context paths that are not wired to workers yet.
fn server_capabilities() -> Vec<String> {
    daemon_protocol::DAEMON_DEFAULT_SERVER_CAPABILITIES
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
        .collect()
}
fn descriptor_key(socket: &str) -> String { format!("{:x}", Sha256::digest(socket.as_bytes()))[..12].to_string() }
fn worker_socket(supervisor: &str, worker: &str) -> String {
    let key = descriptor_key(supervisor);
    #[cfg(windows)] { format!(r"\\.\pipe\prime-agent-worker-{key}-{}", &worker[..worker.len().min(12)]) }
    #[cfg(unix)] { Path::new(&default_daemon_socket_dir()).join(format!("worker-{key}-{}.sock", &worker[..worker.len().min(12)])).to_string_lossy().into_owned() }
}
fn visible(worker: &Worker, client: &str) -> bool { worker.descriptor.lock().unwrap().owner_client_id.as_deref().map_or(true, |owner| owner == client) }
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
        let public = Arc::new(PublicClient { connection_id: identity.clone(), id: Mutex::new(identity.clone()), protocol_id: Mutex::new(None), subscriptions: Mutex::new(HashSet::new()), supports_extension_ui: std::sync::atomic::AtomicBool::new(false), pause_epoch: std::sync::atomic::AtomicU64::new(0), output: sender, stopped: CancellationToken::new() });
        supervisor.clients.lock().unwrap().insert(identity, public.clone());
        public.write(&supervisor.hello(&public));
        let cancelled = public.stopped.clone();
        let writer = tokio::spawn(async move {
            loop {
                tokio::select! { biased;
                    bytes = receiver.recv() => match bytes { Some(bytes) => { if output.write_all(&bytes).await.is_err() { break; } }, None => break },
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
