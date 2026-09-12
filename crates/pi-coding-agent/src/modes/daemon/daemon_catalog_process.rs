//! Port of packages/coding-agent/src/modes/daemon/daemon-catalog-process.ts
//!
//! `SessionManager`, `deleteSessionFile` and the CLI launch spec belong to other
//! slices. The catalog talks to them through the `CatalogSessionBackend` trait,
//! and the child is launched with `tokio::process` using newline-delimited JSON
//! on stdin/stdout (the Node build used `child_process` IPC for the same
//! messages).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{oneshot, Mutex};

use super::daemon_session_list::{AgentStatus, SessionInfo, SessionState};
use crate::core::session_manager::{AgentTaskState, SessionStateStatus, SessionUsageSummary};
use crate::utils::atomic_file::realpath_if_present_sync;

pub const DAEMON_CATALOG_ROLE_ENV: &str = "PRIME_AGENT_INTERNAL_DAEMON_CATALOG";
const DAEMON_CATALOG_START_TIMEOUT_MS: u64 = 30_000;
const DAEMON_CATALOG_REQUEST_TIMEOUT_MS: u64 = 5 * 60 * 1000;

/// True when the catalog is running from a source checkout (`src/` entrypoint).
pub fn is_daemon_catalog_source_path(module_path: &str, package_dir: &str) -> bool {
    let prefix = format!("{}{}", join_path(package_dir, "src"), std::path::MAIN_SEPARATOR);
    module_path.starts_with(&prefix)
}

/// `getPackageDir()` from config.ts plus the two known entrypoints.
pub fn resolve_daemon_catalog_entrypoint(package_dir: &str, module_path: &str) -> Result<String, String> {
    let source_entrypoint = join_path(
        &join_path(&join_path(&join_path(package_dir, "src"), "modes"), "daemon"),
        "daemon-catalog-entry.ts",
    );
    let compiled_entrypoint = join_path(
        &join_path(&join_path(&join_path(package_dir, "dist"), "modes"), "daemon"),
        "daemon-catalog-entry.js",
    );
    let running_from_source = is_daemon_catalog_source_path(module_path, package_dir);
    let candidates = if running_from_source {
        [source_entrypoint, compiled_entrypoint]
    } else {
        [compiled_entrypoint, source_entrypoint]
    };
    for candidate in candidates {
        if Path::new(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err("Cannot locate the daemon catalog entrypoint".to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfoWire {
    pub path: String,
    pub id: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub state: Option<Value>,
    #[serde(rename = "parentSessionPath", skip_serializing_if = "Option::is_none", default)]
    pub parent_session_path: Option<String>,
    #[serde(rename = "rlmDepth", skip_serializing_if = "Option::is_none", default)]
    pub rlm_depth: Option<i64>,
    pub created: String,
    pub modified: String,
    #[serde(rename = "messageCount")]
    pub message_count: usize,
    #[serde(rename = "firstMessage")]
    pub first_message: String,
    #[serde(rename = "allMessagesText")]
    pub all_messages_text: String,
    #[serde(rename = "agentStatus", skip_serializing_if = "Option::is_none", default)]
    pub agent_status: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub usage: Option<Value>,
}

pub fn serialize_session_info(session: &SessionInfo) -> SessionInfoWire {
    SessionInfoWire {
        path: session.path.clone(),
        id: session.id.clone(),
        cwd: session.cwd.clone(),
        name: session.name.clone(),
        state: session
            .state
            .as_ref()
            .map(|state| serde_json::json!({ "status": state.status })),
        parent_session_path: session.parent_session_path.clone(),
        rlm_depth: Some(session.rlm_depth),
        created: iso_from_ms(session.created),
        modified: iso_from_ms(session.modified),
        message_count: session.message_count as usize,
        first_message: session.first_message.clone(),
        all_messages_text: session.all_messages_text.clone(),
        agent_status: session.agent_status.as_ref().map(|status| {
            serde_json::json!({
                "summary": status.summary,
                "taskState": status.task_state,
                "basedOnMessageCount": status.based_on_message_count,
            })
        }),
        usage: session.usage.as_ref().and_then(|usage| serde_json::to_value(usage).ok()),
    }
}

/// The wire carries the TS string for a session state; an unrecognized one is no state.
fn session_state_status(value: &str) -> Option<SessionStateStatus> {
    match value {
        "active" => Some(SessionStateStatus::Active),
        "archived" => Some(SessionStateStatus::Archived),
        "crash" => Some(SessionStateStatus::Crash),
        _ => None,
    }
}

/// The wire carries the TS string for a task verdict; an unrecognized one is no verdict.
fn agent_task_state(value: &str) -> Option<AgentTaskState> {
    match value {
        "needs_input" => Some(AgentTaskState::NeedsInput),
        "completed" => Some(AgentTaskState::Completed),
        _ => None,
    }
}

pub fn deserialize_session_info(session: &SessionInfoWire) -> SessionInfo {
    SessionInfo {
        path: session.path.clone(),
        id: session.id.clone(),
        cwd: session.cwd.clone(),
        name: session.name.clone(),
        state: session
            .state
            .as_ref()
            .and_then(|state| state.get("status").and_then(Value::as_str))
            .and_then(session_state_status)
            .map(|status| SessionState { status }),
        parent_session_path: session.parent_session_path.clone(),
        rlm_depth: session.rlm_depth.unwrap_or(0),
        created: parse_iso_ms(&session.created),
        modified: parse_iso_ms(&session.modified),
        message_count: session.message_count as i64,
        first_message: session.first_message.clone(),
        all_messages_text: session.all_messages_text.clone(),
        agent_status: session.agent_status.as_ref().map(|status| AgentStatus {
            summary: status
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            task_state: status
                .get("taskState")
                .and_then(Value::as_str)
                .and_then(agent_task_state),
            based_on_message_count: status
                .get("basedOnMessageCount")
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as i64,
        }),
        usage: session
            .usage
            .as_ref()
            .and_then(|usage| serde_json::from_value::<SessionUsageSummary>(usage.clone()).ok()),
    }
}

/// `resolveCatalogSessionMatch`: prefix-id or exact-name, ambiguous is an error.
pub fn resolve_catalog_session_match(
    sessions: &[SessionInfo],
    selector: &str,
) -> Result<Option<SessionInfo>, String> {
    let matches: Vec<&SessionInfo> = sessions
        .iter()
        .filter(|session| session.id.starts_with(selector) || session.name.as_deref() == Some(selector))
        .collect();
    if matches.len() > 1 {
        return Err(format!("Ambiguous session selector \"{selector}\""));
    }
    Ok(matches.first().map(|session| (*session).clone()))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum CatalogRequest {
    List {
        #[serde(rename = "type")]
        type_: String,
        id: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        cwd: Option<String>,
        #[serde(rename = "sessionDir", skip_serializing_if = "Option::is_none", default)]
        session_dir: Option<String>,
    },
    Resolve {
        #[serde(rename = "type")]
        type_: String,
        id: String,
        selector: String,
        cwd: String,
        #[serde(rename = "sessionDir", skip_serializing_if = "Option::is_none", default)]
        session_dir: Option<String>,
    },
    Rename {
        #[serde(rename = "type")]
        type_: String,
        id: String,
        #[serde(rename = "sessionPath")]
        session_path: String,
        name: String,
    },
    Delete {
        #[serde(rename = "type")]
        type_: String,
        id: String,
        #[serde(rename = "sessionPath")]
        session_path: String,
    },
    Archive {
        #[serde(rename = "type")]
        type_: String,
        id: String,
        #[serde(rename = "sessionPath")]
        session_path: String,
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    MarkInterrupted {
        #[serde(rename = "type")]
        type_: String,
        id: String,
        #[serde(rename = "sessionPath")]
        session_path: String,
        #[serde(rename = "activeSessionId")]
        active_session_id: String,
        operations: Vec<String>,
    },
    Shutdown {
        #[serde(rename = "type")]
        type_: String,
        id: String,
    },
}

impl CatalogRequest {
    pub fn id(&self) -> &str {
        match self {
            CatalogRequest::List { id, .. }
            | CatalogRequest::Resolve { id, .. }
            | CatalogRequest::Rename { id, .. }
            | CatalogRequest::Delete { id, .. }
            | CatalogRequest::Archive { id, .. }
            | CatalogRequest::MarkInterrupted { id, .. }
            | CatalogRequest::Shutdown { id, .. } => id,
        }
    }

    pub fn command(&self) -> &'static str {
        match self {
            CatalogRequest::List { .. } => "list",
            CatalogRequest::Resolve { .. } => "resolve",
            CatalogRequest::Rename { .. } => "rename",
            CatalogRequest::Delete { .. } => "delete",
            CatalogRequest::Archive { .. } => "archive",
            CatalogRequest::MarkInterrupted { .. } => "mark_interrupted",
            CatalogRequest::Shutdown { .. } => "shutdown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CatalogOutbound {
    Ready,
    Progress {
        id: String,
        loaded: u64,
        total: u64,
    },
    Session {
        id: String,
        session: SessionInfoWire,
    },
    Response {
        id: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        data: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        error: Option<String>,
    },
}

pub fn is_catalog_outbound(value: &Value) -> bool {
    let Some(candidate) = value.as_object() else {
        return false;
    };
    match candidate.get("type").and_then(Value::as_str) {
        Some("ready") => true,
        Some("progress") | Some("session") | Some("response") => {
            candidate.get("id").and_then(Value::as_str).is_some()
        }
        _ => false,
    }
}

pub fn is_catalog_request(value: &Value) -> bool {
    let Some(candidate) = value.as_object() else {
        return false;
    };
    if candidate.get("type").and_then(Value::as_str) != Some("request") {
        return false;
    }
    if candidate.get("id").and_then(Value::as_str).is_none() {
        return false;
    }
    matches!(
        candidate.get("command").and_then(Value::as_str),
        Some("list")
            | Some("resolve")
            | Some("rename")
            | Some("delete")
            | Some("archive")
            | Some("mark_interrupted")
            | Some("shutdown")
    )
}

pub fn is_daemon_catalog_process_from_env() -> bool {
    std::env::var(DAEMON_CATALOG_ROLE_ENV).map(|value| value == "1").unwrap_or(false)
}

/// The session-manager operations the catalog process performs.
pub trait CatalogSessionBackend: Send + Sync {
    fn list_boxed(
        &self,
        cwd: Option<String>,
        session_dir: Option<String>,
        on_progress: Arc<dyn Fn(u64, u64) + Send + Sync>,
        on_session: Arc<dyn Fn(SessionInfo) + Send + Sync>,
    ) -> BoxFuture<'static, Result<Vec<SessionInfo>, String>>;
    fn open_rename(&self, session_path: &str, name: &str) -> Result<(), String>;
    fn delete_boxed(&self, session_path: &str) -> BoxFuture<'static, Result<Value, String>>;
    fn read_session_info_boxed(&self, session_path: &str) -> BoxFuture<'static, Result<Option<SessionInfo>, String>>;
    fn append_session_state(&self, session_path: &str, status: &str) -> Result<(), String>;
    fn append_custom_message_entry(
        &self,
        session_path: &str,
        custom_type: &str,
        content: &str,
        metadata: Value,
    ) -> Result<(), String>;
}

/// The `runDaemonCatalogProcess` message loop over newline-delimited JSON.
pub async fn run_daemon_catalog_process(backend: Arc<dyn CatalogSessionBackend>) -> Result<(), String> {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();
    write_catalog_message(
        &mut stdout,
        &CatalogOutbound::Ready,
    )
    .await?;
    while let Ok(Some(line)) = lines.next_line().await {
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if !is_catalog_request(&value) {
            continue;
        }
        let Some(request) = parse_catalog_request(&value) else {
            continue;
        };
        let shutdown = matches!(request, CatalogRequest::Shutdown { .. });
        let message = handle_catalog_request(&request, &backend).await;
        write_catalog_message(&mut stdout, &message).await?;
        if shutdown {
            return Ok(());
        }
    }
    Ok(())
}

fn parse_catalog_request(value: &Value) -> Option<CatalogRequest> {
    let candidate = value.as_object()?;
    let id = candidate.get("id")?.as_str()?.to_string();
    let type_ = candidate.get("type")?.as_str()?.to_string();
    let field = |key: &str| candidate.get(key).and_then(Value::as_str).map(str::to_string);
    match candidate.get("command")?.as_str()? {
        "list" => Some(CatalogRequest::List {
            type_,
            id,
            cwd: field("cwd"),
            session_dir: field("sessionDir"),
        }),
        "resolve" => Some(CatalogRequest::Resolve {
            type_,
            id,
            selector: field("selector")?,
            cwd: field("cwd")?,
            session_dir: field("sessionDir"),
        }),
        "rename" => Some(CatalogRequest::Rename {
            type_,
            id,
            session_path: field("sessionPath")?,
            name: field("name")?,
        }),
        "delete" => Some(CatalogRequest::Delete {
            type_,
            id,
            session_path: field("sessionPath")?,
        }),
        "archive" => Some(CatalogRequest::Archive {
            type_,
            id,
            session_path: field("sessionPath")?,
            session_id: field("sessionId")?,
        }),
        "mark_interrupted" => Some(CatalogRequest::MarkInterrupted {
            type_,
            id,
            session_path: field("sessionPath")?,
            active_session_id: field("activeSessionId")?,
            operations: candidate
                .get("operations")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        }),
        "shutdown" => Some(CatalogRequest::Shutdown { type_, id }),
        _ => None,
    }
}

async fn handle_catalog_request(
    request: &CatalogRequest,
    backend: &Arc<dyn CatalogSessionBackend>,
) -> CatalogOutbound {
    let id = request.id().to_string();
    match request {
        CatalogRequest::List {
            cwd, session_dir, ..
        } => {
            let request_id = id.clone();
            let on_progress: Arc<dyn Fn(u64, u64) + Send + Sync> = {
                let request_id = request_id.clone();
                Arc::new(move |loaded, total| {
                    let message = CatalogOutbound::Progress {
                        id: request_id.clone(),
                        loaded,
                        total,
                    };
                    write_catalog_message_blocking(message);
                })
            };
            let on_session: Arc<dyn Fn(SessionInfo) + Send + Sync> = {
                let request_id = request_id.clone();
                Arc::new(move |session| {
                    let message = CatalogOutbound::Session {
                        id: request_id.clone(),
                        session: serialize_session_info(&session),
                    };
                    write_catalog_message_blocking(message);
                })
            };
            match backend
                .list_boxed(cwd.clone(), session_dir.clone(), on_progress, on_session)
                .await
            {
                Ok(sessions) => CatalogOutbound::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({
                        "sessions": sessions.iter().map(serialize_session_info).collect::<Vec<_>>(),
                    })),
                    error: None,
                },
                Err(error) => CatalogOutbound::Response {
                    id,
                    success: false,
                    data: None,
                    error: Some(error),
                },
            }
        }
        CatalogRequest::Resolve {
            selector,
            cwd,
            session_dir,
            ..
        } => {
            let local = backend
                .list_boxed(
                    Some(cwd.clone()),
                    session_dir.clone(),
                    Arc::new(|_, _| {}),
                    Arc::new(|_| {}),
                )
                .await;
            let local_match = match local {
                Ok(sessions) => match resolve_catalog_session_match(&sessions, selector) {
                    Ok(found) => found,
                    Err(error) => {
                        return CatalogOutbound::Response {
                            id,
                            success: false,
                            data: None,
                            error: Some(error),
                        }
                    }
                },
                Err(error) => {
                    return CatalogOutbound::Response {
                        id,
                        success: false,
                        data: None,
                        error: Some(error),
                    }
                }
            };
            if let Some(session) = local_match {
                return CatalogOutbound::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({ "sessionPath": session.path })),
                    error: None,
                };
            }
            let global = backend
                .list_boxed(
                    None,
                    session_dir.clone(),
                    Arc::new(|_, _| {}),
                    Arc::new(|_| {}),
                )
                .await;
            let global_match = match global {
                Ok(sessions) => match resolve_catalog_session_match(&sessions, selector) {
                    Ok(found) => found,
                    Err(error) => {
                        return CatalogOutbound::Response {
                            id,
                            success: false,
                            data: None,
                            error: Some(error),
                        }
                    }
                },
                Err(error) => {
                    return CatalogOutbound::Response {
                        id,
                        success: false,
                        data: None,
                        error: Some(error),
                    }
                }
            };
            match global_match {
                Some(session) => CatalogOutbound::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({ "sessionPath": session.path })),
                    error: None,
                },
                None => CatalogOutbound::Response {
                    id,
                    success: false,
                    data: None,
                    error: Some(format!("No session found matching '{selector}'")),
                },
            }
        }
        CatalogRequest::Rename {
            session_path, name, ..
        } => match backend.open_rename(session_path, name.trim()) {
            Ok(()) => CatalogOutbound::Response {
                id,
                success: true,
                data: None,
                error: None,
            },
            Err(error) => CatalogOutbound::Response {
                id,
                success: false,
                data: None,
                error: Some(error),
            },
        },
        CatalogRequest::Delete { session_path, .. } => match backend.delete_boxed(session_path).await {
            Ok(data) => CatalogOutbound::Response {
                id,
                success: true,
                data: Some(data),
                error: None,
            },
            Err(error) => CatalogOutbound::Response {
                id,
                success: false,
                data: None,
                error: Some(error),
            },
        },
        CatalogRequest::Archive {
            session_path,
            session_id,
            ..
        } => {
            let session = match backend.read_session_info_boxed(session_path).await {
                Ok(session) => session,
                Err(error) => {
                    return CatalogOutbound::Response {
                        id,
                        success: false,
                        data: None,
                        error: Some(error),
                    }
                }
            };
            match session {
                Some(session) if session.id == *session_id => {
                    let already_archived = session
                        .state
                        .as_ref()
                        .map(|state| state.status)
                        == Some(SessionStateStatus::Archived);
                    if !already_archived {
                        if let Err(error) = backend.append_session_state(session_path, "archived") {
                            return CatalogOutbound::Response {
                                id,
                                success: false,
                                data: None,
                                error: Some(error),
                            };
                        }
                    }
                    CatalogOutbound::Response {
                        id,
                        success: true,
                        data: Some(serde_json::json!({ "archived": true })),
                        error: None,
                    }
                }
                _ => CatalogOutbound::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({ "archived": false })),
                    error: None,
                },
            }
        }
        CatalogRequest::MarkInterrupted {
            session_path,
            active_session_id,
            operations,
            ..
        } => {
            let result = backend.append_custom_message_entry(
                session_path,
                "prime-agent.worker_recovery",
                WORKER_INTERRUPTED_CONTENT,
                serde_json::json!({
                    "activeSessionId": active_session_id,
                    "operations": operations,
                }),
            );
            match result {
                Ok(()) => CatalogOutbound::Response {
                    id,
                    success: true,
                    data: None,
                    error: None,
                },
                Err(error) => CatalogOutbound::Response {
                    id,
                    success: false,
                    data: None,
                    error: Some(error),
                },
            }
        }
        CatalogRequest::Shutdown { .. } => CatalogOutbound::Response {
            id,
            success: true,
            data: None,
            error: None,
        },
    }
}

pub const WORKER_INTERRUPTED_CONTENT: &str = "<prime_agent_worker_interrupted>\nThe isolated session worker stopped during in-flight work. The saved transcript was recovered, but uncertain model, tool, bash, or child-agent work was not replayed. Inspect external side effects before continuing.\n</prime_agent_worker_interrupted>";

async fn write_catalog_message(
    stdout: &mut tokio::io::Stdout,
    message: &CatalogOutbound,
) -> Result<(), String> {
    let line = serde_json::to_string(message).map_err(|error| error.to_string())?;
    stdout
        .write_all(format!("{line}\n").as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    stdout.flush().await.map_err(|error| error.to_string())
}

/// Progress/session callbacks fire from inside the backend call; the child
/// process writes them on the same stdout stream.
fn write_catalog_message_blocking(message: CatalogOutbound) {
    let Ok(line) = serde_json::to_string(&message) else {
        return;
    };
    use std::io::Write;
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(format!("{line}\n").as_bytes());
    let _ = stdout.flush();
}

struct PendingCatalogRequest {
    sender: oneshot::Sender<Result<Value, String>>,
    callbacks: Option<Arc<CatalogListCallbacks>>,
}

/// Supervisor-side client for the catalog child process.
pub struct DaemonCatalogClient {
    on_diagnostic: Arc<dyn Fn(&str) + Send + Sync>,
    child: Mutex<Option<tokio::process::Child>>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    pending: Mutex<HashMap<String, PendingCatalogRequest>>,
    request_counter: AtomicU64,
    ready: std::sync::atomic::AtomicBool,
    starting: Mutex<Option<Arc<tokio::sync::Mutex<()>>>>,
}

pub struct CatalogListCallbacks {
    pub on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    pub on_session: Option<Arc<dyn Fn(SessionInfo) + Send + Sync>>,
}

impl DaemonCatalogClient {
    pub fn new(on_diagnostic: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        Self {
            on_diagnostic,
            child: Mutex::new(None),
            stdin: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            request_counter: AtomicU64::new(0),
            ready: std::sync::atomic::AtomicBool::new(false),
            starting: Mutex::new(None),
        }
    }

    pub async fn start(self: &Arc<Self>, command: &str, args: Vec<String>, envs: Vec<(String, String)>) -> Result<(), String> {
        {
            let child = self.child.lock().await;
            if child.is_some() {
                return Ok(());
            }
        }
        self.spawn_catalog(command, args, envs).await
    }

    pub async fn list(
        self: &Arc<Self>,
        cwd: Option<&str>,
        session_dir: Option<&str>,
        callbacks: Option<CatalogListCallbacks>,
    ) -> Result<Vec<SessionInfo>, String> {
        let id = self.next_request_id();
        let mut request = serde_json::json!({
            "type": "request",
            "id": id,
            "command": "list",
        });
        if let Some(cwd) = cwd {
            request["cwd"] = Value::String(cwd.to_string());
        }
        if let Some(session_dir) = session_dir {
            request["sessionDir"] = Value::String(session_dir.to_string());
        }
        let data = self.request(request, callbacks).await?;
        let sessions = data
            .get("sessions")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| serde_json::from_value::<SessionInfoWire>(entry.clone()).ok())
                    .map(|wire| deserialize_session_info(&wire))
                    .collect::<Vec<SessionInfo>>()
            })
            .unwrap_or_default();
        Ok(sessions)
    }

    pub async fn rename(self: &Arc<Self>, session_path: &str, name: &str) -> Result<(), String> {
        let id = self.next_request_id();
        self.request(
            serde_json::json!({
                "type": "request",
                "id": id,
                "command": "rename",
                "sessionPath": session_path,
                "name": name,
            }),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn resolve(
        self: &Arc<Self>,
        selector: &str,
        cwd: &str,
        session_dir: Option<&str>,
    ) -> Result<String, String> {
        let id = self.next_request_id();
        let mut request = serde_json::json!({
            "type": "request",
            "id": id,
            "command": "resolve",
            "selector": selector,
            "cwd": cwd,
        });
        if let Some(session_dir) = session_dir {
            request["sessionDir"] = Value::String(session_dir.to_string());
        }
        let data = self.request(request, None).await?;
        data.get("sessionPath")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "Daemon catalog returned no session path".to_string())
    }

    pub async fn delete(self: &Arc<Self>, session_path: &str) -> Result<Value, String> {
        let id = self.next_request_id();
        self.request(
            serde_json::json!({
                "type": "request",
                "id": id,
                "command": "delete",
                "sessionPath": session_path,
            }),
            None,
        )
        .await
    }

    pub async fn archive(self: &Arc<Self>, session_path: &str, session_id: &str) -> Result<bool, String> {
        let id = self.next_request_id();
        let data = self
            .request(
                serde_json::json!({
                    "type": "request",
                    "id": id,
                    "command": "archive",
                    "sessionPath": session_path,
                    "sessionId": session_id,
                }),
                None,
            )
            .await?;
        Ok(data.get("archived").and_then(Value::as_bool).unwrap_or(false))
    }

    pub async fn mark_interrupted(
        self: &Arc<Self>,
        session_path: &str,
        active_session_id: &str,
        operations: &[String],
    ) -> Result<(), String> {
        let id = self.next_request_id();
        self.request(
            serde_json::json!({
                "type": "request",
                "id": id,
                "command": "mark_interrupted",
                "sessionPath": session_path,
                "activeSessionId": active_session_id,
                "operations": operations,
            }),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn stop(self: &Arc<Self>) {
        let has_child = self.child.lock().await.is_some();
        if !has_child {
            return;
        }
        let id = self.next_request_id();
        let _ = self
            .request(
                serde_json::json!({ "type": "request", "id": id, "command": "shutdown" }),
                None,
            )
            .await;
        self.child.lock().await.take();
        *self.stdin.lock().await = None;
    }

    fn next_request_id(&self) -> String {
        format!(
            "catalog_{}",
            self.request_counter.fetch_add(1, Ordering::SeqCst) + 1
        )
    }

    async fn spawn_catalog(
        self: &Arc<Self>,
        command: &str,
        args: Vec<String>,
        envs: Vec<(String, String)>,
    ) -> Result<(), String> {
        let mut child_command = tokio::process::Command::new(command);
        child_command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env(DAEMON_CATALOG_ROLE_ENV, "1");
        for (key, value) in envs {
            child_command.env(key, value);
        }
        let mut child = child_command
            .spawn()
            .map_err(|error| format!("Failed to start daemon catalog: {error}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        *self.stdin.lock().await = stdin;
        *self.child.lock().await = Some(child);
        let Some(stdout) = stdout else {
            return Err("Daemon catalog has no stdout".to_string());
        };

        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(client) = weak.upgrade() else {
                    return;
                };
                client.handle_message(&value).await;
            }
            if let Some(client) = weak.upgrade() {
                client.handle_close("Daemon catalog exited").await;
            }
        });

        // Wait for the `ready` greeting with the same 30s startup timeout.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(DAEMON_CATALOG_START_TIMEOUT_MS);
        loop {
            if tokio::time::Instant::now() >= deadline {
                self.handle_close("Timed out starting daemon catalog").await;
                return Err("Timed out starting daemon catalog".to_string());
            }
            if self.ready.load(Ordering::SeqCst) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn request(
        self: &Arc<Self>,
        request: Value,
        callbacks: Option<CatalogListCallbacks>,
    ) -> Result<Value, String> {
        let Some(id) = request.get("id").and_then(Value::as_str).map(str::to_string) else {
            return Err("Daemon catalog request has no id".to_string());
        };
        let (sender, receiver) = oneshot::channel::<Result<Value, String>>();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(
                id.clone(),
                PendingCatalogRequest {
                    sender,
                    callbacks: callbacks.map(Arc::new),
                },
            );
        }
        let line = serde_json::to_string(&request).map_err(|error| error.to_string())?;
        {
            let mut stdin = self.stdin.lock().await;
            let Some(stdin) = stdin.as_mut() else {
                self.pending.lock().await.remove(&id);
                return Err("Daemon catalog is not connected".to_string());
            };
            if let Err(error) = stdin.write_all(format!("{line}\n").as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(error.to_string());
            }
            let _ = stdin.flush().await;
        }
        match tokio::time::timeout(Duration::from_millis(DAEMON_CATALOG_REQUEST_TIMEOUT_MS), receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("Daemon catalog is not connected".to_string()),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                let command = request
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("request")
                    .to_string();
                Err(format!("Timed out waiting for daemon catalog {command}"))
            }
        }
    }

    async fn handle_message(self: &Arc<Self>, value: &Value) {
        if !is_catalog_outbound(value) {
            return;
        }
        let Some(candidate) = value.as_object() else {
            return;
        };
        match candidate.get("type").and_then(Value::as_str) {
            Some("ready") => {
                self.ready.store(true, Ordering::SeqCst);
                return;
            }
            Some("progress") => {
                let Some(id) = candidate.get("id").and_then(Value::as_str) else {
                    return;
                };
                let callbacks = self.callbacks_for(id).await;
                if let Some(callbacks) = callbacks {
                    if let Some(on_progress) = &callbacks.on_progress {
                        on_progress(
                            candidate.get("loaded").and_then(Value::as_u64).unwrap_or(0),
                            candidate.get("total").and_then(Value::as_u64).unwrap_or(0),
                        );
                    }
                }
                return;
            }
            Some("session") => {
                let Some(id) = candidate.get("id").and_then(Value::as_str) else {
                    return;
                };
                let callbacks = self.callbacks_for(id).await;
                if let Some(callbacks) = callbacks {
                    if let Some(on_session) = &callbacks.on_session {
                        if let Some(wire) = candidate
                            .get("session")
                            .cloned()
                            .and_then(|value| serde_json::from_value::<SessionInfoWire>(value).ok())
                        {
                            on_session(deserialize_session_info(&wire));
                        }
                    }
                }
                return;
            }
            Some("response") => {}
            _ => return,
        }
        let Some(id) = candidate.get("id").and_then(Value::as_str) else {
            return;
        };
        let pending = self.pending.lock().await.remove(id);
        let Some(pending) = pending else {
            return;
        };
        if candidate.get("success").and_then(Value::as_bool) == Some(true) {
            let _ = pending
                .sender
                .send(Ok(candidate.get("data").cloned().unwrap_or(Value::Null)));
        } else {
            let _ = pending.sender.send(Err(candidate
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()));
        }
    }

    async fn callbacks_for(&self, id: &str) -> Option<Arc<CatalogListCallbacks>> {
        let pending = self.pending.lock().await;
        pending.get(id).and_then(|entry| entry.callbacks.clone())
    }

    async fn handle_close(self: &Arc<Self>, message: &str) {
        if self.child.lock().await.take().is_none() {
            return;
        }
        *self.stdin.lock().await = None;
        (self.on_diagnostic)(message);
        let pending: Vec<PendingCatalogRequest> = {
            let mut pending = self.pending.lock().await;
            let keys: Vec<String> = pending.keys().cloned().collect();
            keys.into_iter().filter_map(|key| pending.remove(&key)).collect()
        };
        for entry in pending {
            let _ = entry.sender.send(Err(message.to_string()));
        }
    }
}

fn join_path(base: &str, name: &str) -> String {
    let base = base.trim_end_matches(['/', '\\']);
    format!("{base}/{name}")
}

fn iso_from_ms(ms: f64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn parse_iso_ms(value: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| time.timestamp_millis() as f64)
        .unwrap_or_else(|_| f64::NAN)
}

/// Realpath helper used when canonicalizing the package dir on Windows.
pub fn canonical_package_dir(package_dir: &str) -> String {
    realpath_if_present_sync(package_dir).unwrap_or_else(|_| PathBuf::from(package_dir).to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, name: Option<&str>) -> SessionInfo {
        SessionInfo {
            path: format!("/tmp/{id}.jsonl"),
            id: id.to_string(),
            cwd: "/tmp".to_string(),
            name: name.map(str::to_string),
            state: None,
            parent_session_path: None,
            rlm_depth: 0,
            created: 0.0,
            modified: 0.0,
            message_count: 1,
            first_message: String::new(),
            all_messages_text: String::new(),
            agent_status: None,
            usage: None,
        }
    }

    #[test]
    fn source_path_detection_uses_the_src_directory() {
        assert!(is_daemon_catalog_source_path(
            "/pkg/src/modes/daemon/daemon-catalog-entry.ts",
            "/pkg"
        ));
        assert!(!is_daemon_catalog_source_path(
            "/pkg/dist/modes/daemon/daemon-catalog-entry.js",
            "/pkg"
        ));
    }

    #[test]
    fn entrypoint_resolution_prefers_source_when_running_from_source() {
        let error = resolve_daemon_catalog_entrypoint("/definitely-missing-package", "/definitely-missing-package/src/x.ts")
            .expect_err("no entrypoint");
        assert_eq!(error, "Cannot locate the daemon catalog entrypoint");
    }

    #[test]
    fn session_info_round_trips_over_the_wire() {
        let original = SessionInfo {
            created: 0.0,
            modified: 5_000.0,
            state: Some(SessionState {
                status: SessionStateStatus::Archived,
            }),
            agent_status: Some(AgentStatus {
                summary: "done".to_string(),
                task_state: Some(AgentTaskState::Completed),
                based_on_message_count: 1,
            }),
            ..session("abc", Some("named"))
        };
        let wire = serialize_session_info(&original);
        assert_eq!(wire.created, "1970-01-01T00:00:00.000Z");
        assert_eq!(wire.modified, "1970-01-01T00:00:05.000Z");
        let restored = deserialize_session_info(&wire);
        assert_eq!(restored.path, original.path);
        assert_eq!(restored.modified, 5000.0);
        assert_eq!(
            restored.state.map(|state| state.status),
            Some(SessionStateStatus::Archived)
        );
        assert_eq!(
            restored.agent_status.and_then(|status| status.task_state),
            Some(AgentTaskState::Completed)
        );
    }

    #[test]
    fn catalog_session_match_prefers_prefix_and_rejects_ambiguity() {
        let sessions = vec![session("abc123", None), session("abc999", None), session("zzz", Some("named"))];
        assert_eq!(
            resolve_catalog_session_match(&sessions, "zzz")
                .expect("ok")
                .expect("found")
                .id,
            "zzz"
        );
        assert_eq!(
            resolve_catalog_session_match(&sessions, "named")
                .expect("ok")
                .expect("found")
                .id,
            "zzz"
        );
        assert_eq!(
            resolve_catalog_session_match(&sessions, "abc").expect_err("ambiguous"),
            "Ambiguous session selector \"abc\""
        );
        assert!(resolve_catalog_session_match(&sessions, "missing")
            .expect("ok")
            .is_none());
    }

    #[test]
    fn request_and_outbound_guards_match_the_typescript() {
        assert!(is_catalog_request(&serde_json::json!({
            "type": "request",
            "id": "1",
            "command": "list"
        })));
        assert!(!is_catalog_request(&serde_json::json!({
            "type": "request",
            "id": "1",
            "command": "other"
        })));
        assert!(!is_catalog_request(&serde_json::json!({ "type": "request", "command": "list" })));
        assert!(is_catalog_outbound(&serde_json::json!({ "type": "ready" })));
        assert!(is_catalog_outbound(&serde_json::json!({
            "type": "response",
            "id": "1",
            "success": true
        })));
        assert!(!is_catalog_outbound(&serde_json::json!({ "type": "response" })));
    }

    #[test]
    fn request_parsing_reads_every_command_shape() {
        let list = parse_catalog_request(&serde_json::json!({
            "type": "request",
            "id": "1",
            "command": "list",
            "cwd": "/tmp"
        }))
        .expect("parses");
        assert_eq!(list.command(), "list");
        assert_eq!(list.id(), "1");
        let interrupted = parse_catalog_request(&serde_json::json!({
            "type": "request",
            "id": "2",
            "command": "mark_interrupted",
            "sessionPath": "/tmp/s.jsonl",
            "activeSessionId": "a",
            "operations": ["prompt"]
        }))
        .expect("parses");
        match interrupted {
            CatalogRequest::MarkInterrupted { operations, .. } => assert_eq!(operations, vec!["prompt".to_string()]),
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[test]
    fn worker_interrupted_content_matches_the_typescript() {
        assert!(WORKER_INTERRUPTED_CONTENT.starts_with("<prime_agent_worker_interrupted>\n"));
        assert!(WORKER_INTERRUPTED_CONTENT.ends_with("</prime_agent_worker_interrupted>"));
    }
}
