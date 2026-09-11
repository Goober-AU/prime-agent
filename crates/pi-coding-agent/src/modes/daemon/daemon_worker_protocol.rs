//! Port of packages/coding-agent/src/modes/daemon/daemon-worker-protocol.ts

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::agent_roster::WorkerRosterEntry;
use super::daemon_client::protocol::{DaemonCommand, DaemonHello};

pub const SESSION_LEASE_OWNER_ID_ENV: &str = "PRIME_AGENT_INTERNAL_SESSION_LEASE_OWNER_ID";
pub const SESSION_LEASES_ENABLED_ENV: &str = "PRIME_AGENT_INTERNAL_SESSION_LEASES";

pub const DAEMON_WORKER_ROLE_ENV: &str = "PRIME_AGENT_INTERNAL_DAEMON_WORKER";
pub const DAEMON_WORKER_TOKEN_ENV: &str = "PRIME_AGENT_INTERNAL_DAEMON_WORKER_TOKEN";
pub const DAEMON_WORKER_INSTANCE_ID_ENV: &str = "PRIME_AGENT_INTERNAL_DAEMON_WORKER_INSTANCE_ID";
pub const DAEMON_WORKER_ACTIVE_SESSION_ID_ENV: &str = "PRIME_AGENT_INTERNAL_DAEMON_WORKER_ACTIVE_SESSION_ID";
pub const DAEMON_WORKER_SUPERVISOR_SOCKET_ENV: &str = "PRIME_AGENT_INTERNAL_DAEMON_SUPERVISOR_SOCKET";
pub const DAEMON_WORKER_RECOVERY_JOURNAL_ENV: &str = "PRIME_AGENT_INTERNAL_DAEMON_WORKER_RECOVERY_JOURNAL";
pub const DAEMON_WORKER_STARTUP_GATE_FD_ENV: &str = "PRIME_AGENT_INTERNAL_DAEMON_WORKER_STARTUP_GATE_FD";
pub const DAEMON_WORKER_STARTUP_GATE_COMMIT: &str = "start\n";

/// Advertised by new workers in the worker_auth response; absent on legacy workers.
pub const DAEMON_WORKER_ROSTER_CAPABILITY: &str = "agent_roster";

/// Advertised in the worker_auth response by workers that accept peer transport grants.
pub const DAEMON_WORKER_PEER_TRANSPORT_CAPABILITY: &str = "peer_transport";

/// Idle keepalive cadence for worker->supervisor roster frames.
pub const ROSTER_HEARTBEAT_INTERVAL_MS: u64 = 15_000;

pub type DaemonWorkerLifecycle = &'static str;

pub const DAEMON_WORKER_LIFECYCLE_STARTING: &str = "starting";
pub const DAEMON_WORKER_LIFECYCLE_READY: &str = "ready";
pub const DAEMON_WORKER_LIFECYCLE_RECOVERING: &str = "recovering";
pub const DAEMON_WORKER_LIFECYCLE_STOPPING: &str = "stopping";
pub const DAEMON_WORKER_LIFECYCLE_FAILED: &str = "failed";

/// Worker->supervisor roster frames live outside the client-facing DaemonOutbound schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonWorkerRosterOutbound {
    RosterDelta {
        entries: Vec<WorkerRosterEntry>,
        #[serde(rename = "removedAgentIds", skip_serializing_if = "Option::is_none", default)]
        removed_agent_ids: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        snapshot: Option<bool>,
    },
    RosterHeartbeat,
}

/// The private-frame routing header used on the worker socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DaemonWorkerFrameHeader {
    Command {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "commandType")]
        command_type: String,
    },
    Outbound {
        #[serde(rename = "requestId", skip_serializing_if = "Option::is_none", default)]
        request_id: Option<String>,
        #[serde(rename = "outboundType")]
        outbound_type: String,
        #[serde(rename = "activeSessionId", skip_serializing_if = "Option::is_none", default)]
        active_session_id: Option<String>,
        #[serde(rename = "snapshotId", skip_serializing_if = "Option::is_none", default)]
        snapshot_id: Option<String>,
        #[serde(rename = "sessionEventType", skip_serializing_if = "Option::is_none", default)]
        session_event_type: Option<String>,
        #[serde(rename = "payloadEncoding", skip_serializing_if = "Option::is_none", default)]
        payload_encoding: Option<String>,
        #[serde(rename = "snapshotPurpose", skip_serializing_if = "Option::is_none", default)]
        snapshot_purpose: Option<String>,
    },
}

pub fn is_daemon_worker_frame_header(value: &Value) -> bool {
    let Some(candidate) = value.as_object() else {
        return false;
    };
    match candidate.get("kind").and_then(Value::as_str) {
        Some("command") => {
            candidate.get("requestId").and_then(Value::as_str).is_some()
                && candidate.get("commandType").and_then(Value::as_str).is_some()
        }
        Some("outbound") => {
            candidate.get("outboundType").and_then(Value::as_str).is_some()
                && candidate
                    .get("requestId")
                    .is_none_or(|value| value.as_str().is_some())
                && candidate
                    .get("activeSessionId")
                    .is_none_or(|value| value.as_str().is_some())
                && candidate
                    .get("snapshotId")
                    .is_none_or(|value| value.as_str().is_some())
                && candidate
                    .get("sessionEventType")
                    .is_none_or(|value| value.as_str().is_some())
                && candidate.get("snapshotPurpose").is_none_or(|value| {
                    matches!(value.as_str(), Some("attach") | Some("replacement") | Some("catchup"))
                })
                && candidate.get("payloadEncoding").is_none_or(|value| {
                    matches!(value.as_str(), Some("jsonl") | Some("assistant-delta"))
                })
        }
        _ => false,
    }
}

/// A single-use, worker-memory-only admission for one direct peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonWorkerPeerGrant {
    #[serde(rename = "grantId")]
    pub grant_id: String,
    pub token: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
    pub purpose: String,
    #[serde(rename = "workerInstanceId")]
    pub worker_instance_id: String,
    #[serde(rename = "activeSessionId")]
    pub active_session_id: String,
    #[serde(rename = "issuerGeneration")]
    pub issuer_generation: String,
}

/// The ticket the supervisor hands a client for one direct worker connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonPeerTransportTicket {
    pub purpose: String,
    #[serde(rename = "socketPath")]
    pub socket_path: String,
    #[serde(rename = "socketIdentity")]
    pub socket_identity: DaemonSocketIdentity,
    #[serde(rename = "workerInstanceId")]
    pub worker_instance_id: String,
    #[serde(rename = "activeSessionId")]
    pub active_session_id: String,
    #[serde(rename = "grantId")]
    pub grant_id: String,
    pub token: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonSocketIdentity {
    pub dev: u64,
    pub ino: u64,
}

impl DaemonPeerTransportTicket {
    pub fn from_value(value: &Value) -> Option<Self> {
        let candidate = value.as_object()?;
        if candidate.get("purpose").and_then(Value::as_str) != Some("session_client") {
            return None;
        }
        let socket_identity = candidate.get("socketIdentity")?.as_object()?;
        if socket_identity.get("dev").and_then(Value::as_u64).is_none()
            || socket_identity.get("ino").and_then(Value::as_u64).is_none()
        {
            return None;
        }
        serde_json::from_value(value.clone()).ok()
    }
}

/// The durable create command written before a worker spawns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DurableDaemonCreateCommand {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(rename = "sessionPath", skip_serializing_if = "Option::is_none", default)]
    pub session_path: Option<String>,
    #[serde(rename = "noSession", skip_serializing_if = "Option::is_none", default)]
    pub no_session: Option<bool>,
    /// Version-1 descriptors on disk carried the full create config here; it is
    /// read (never re-written) so a v1 record can be upgraded without losing
    /// the session dir / telemetry flag it encodes.
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

pub fn durable_daemon_create_command(command: &DaemonCommand) -> DurableDaemonCreateCommand {
    DurableDaemonCreateCommand {
        type_: "create".to_string(),
        session_path: command.string_field("sessionPath").map(str::to_string),
        no_session: command.field("noSession").and_then(Value::as_bool),
        extra: Map::new(),
    }
}

/// Read a v1 durable create command (it carried the whole `config` object).
pub fn durable_create_command_from_value(value: &Value) -> Option<DurableDaemonCreateCommand> {
    let candidate = value.as_object()?;
    if candidate.get("type").and_then(Value::as_str) != Some("create") {
        return None;
    }
    let mut extra = Map::new();
    if let Some(config) = candidate.get("config") {
        extra.insert("config".to_string(), config.clone());
    }
    Some(DurableDaemonCreateCommand {
        type_: "create".to_string(),
        session_path: candidate.get("sessionPath").and_then(Value::as_str).map(str::to_string),
        no_session: candidate.get("noSession").and_then(Value::as_bool),
        extra,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonWorkerDescriptor {
    pub version: u32,
    #[serde(rename = "workerId")]
    pub worker_id: String,
    pub pid: i32,
    #[serde(rename = "processStartId", skip_serializing_if = "Option::is_none", default)]
    pub process_start_id: Option<String>,
    #[serde(rename = "socketPath")]
    pub socket_path: String,
    #[serde(rename = "recoveryJournalPath")]
    pub recovery_journal_path: String,
    #[serde(rename = "orphanProcessJournalPath", skip_serializing_if = "Option::is_none", default)]
    pub orphan_process_journal_path: Option<String>,
    #[serde(rename = "supervisorSocketPath")]
    pub supervisor_socket_path: String,
    #[serde(rename = "authenticationToken")]
    pub authentication_token: String,
    #[serde(rename = "workerInstanceId", skip_serializing_if = "Option::is_none", default)]
    pub worker_instance_id: Option<String>,
    #[serde(rename = "rootActiveSessionId")]
    pub root_active_session_id: String,
    /// Stable protocol client that owns this worker. Omitted for resident sessions.
    #[serde(rename = "ownerClientId", skip_serializing_if = "Option::is_none", default)]
    pub owner_client_id: Option<String>,
    #[serde(rename = "rootSessionId", skip_serializing_if = "Option::is_none", default)]
    pub root_session_id: Option<String>,
    #[serde(rename = "sessionFile", skip_serializing_if = "Option::is_none", default)]
    pub session_file: Option<String>,
    #[serde(rename = "sessionDir", skip_serializing_if = "Option::is_none", default)]
    pub session_dir: Option<String>,
    #[serde(rename = "telemetryDisabled", skip_serializing_if = "Option::is_none", default)]
    pub telemetry_disabled: Option<bool>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    pub lifecycle: String,
    #[serde(rename = "createCommand")]
    pub create_command: DurableDaemonCreateCommand,
    #[serde(rename = "consecutiveFailures")]
    pub consecutive_failures: u32,
    /// Durable intent written before root termination so replacement supervisors never recover it.
    #[serde(rename = "stopRequestedAt", skip_serializing_if = "Option::is_none", default)]
    pub stop_requested_at: Option<String>,
    /// Complete the root's archived lifecycle state after its process has stopped.
    #[serde(rename = "archiveOnStop", skip_serializing_if = "Option::is_none", default)]
    pub archive_on_stop: Option<bool>,
    #[serde(rename = "lastFailureAt", skip_serializing_if = "Option::is_none", default)]
    pub last_failure_at: Option<String>,
    #[serde(rename = "lastError", skip_serializing_if = "Option::is_none", default)]
    pub last_error: Option<String>,
}

pub fn durable_daemon_worker_descriptor(descriptor: &DaemonWorkerDescriptor) -> DaemonWorkerDescriptor {
    let version_one_config = if descriptor.version == 1 {
        descriptor
            .create_command
            .extra
            .get("config")
            .and_then(Value::as_object)
            .cloned()
    } else {
        None
    };
    let session_dir = descriptor.session_dir.clone().or_else(|| {
        version_one_config
            .as_ref()
            .and_then(|config| config.get("sessionDir"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let telemetry_disabled = descriptor.telemetry_disabled == Some(true)
        || version_one_config
            .as_ref()
            .and_then(|config| config.get("telemetryDisabled"))
            .and_then(Value::as_bool)
            == Some(true);
    DaemonWorkerDescriptor {
        version: 2,
        worker_id: descriptor.worker_id.clone(),
        pid: descriptor.pid,
        process_start_id: descriptor.process_start_id.clone(),
        socket_path: descriptor.socket_path.clone(),
        recovery_journal_path: descriptor.recovery_journal_path.clone(),
        orphan_process_journal_path: descriptor.orphan_process_journal_path.clone(),
        supervisor_socket_path: descriptor.supervisor_socket_path.clone(),
        authentication_token: descriptor.authentication_token.clone(),
        worker_instance_id: descriptor.worker_instance_id.clone(),
        root_active_session_id: descriptor.root_active_session_id.clone(),
        owner_client_id: descriptor.owner_client_id.clone(),
        root_session_id: descriptor.root_session_id.clone(),
        session_file: descriptor.session_file.clone(),
        session_dir,
        telemetry_disabled: telemetry_disabled.then_some(true),
        created_at: descriptor.created_at.clone(),
        updated_at: descriptor.updated_at.clone(),
        lifecycle: descriptor.lifecycle.clone(),
        create_command: DurableDaemonCreateCommand {
            type_: "create".to_string(),
            session_path: descriptor.create_command.session_path.clone(),
            no_session: descriptor.create_command.no_session,
            extra: Map::new(),
        },
        consecutive_failures: descriptor.consecutive_failures,
        stop_requested_at: descriptor.stop_requested_at.clone(),
        archive_on_stop: descriptor.archive_on_stop,
        last_failure_at: descriptor.last_failure_at.clone(),
        last_error: (descriptor.lifecycle == "failed")
            .then(|| "Waiting for a client with fresh runtime context".to_string()),
    }
}

pub fn is_daemon_worker_process(environment: &HashMap<String, String>) -> bool {
    environment.get(DAEMON_WORKER_ROLE_ENV).map(String::as_str) == Some("1")
}

pub fn is_daemon_worker_process_from_env() -> bool {
    std::env::var(DAEMON_WORKER_ROLE_ENV).map(|value| value == "1").unwrap_or(false)
}

/// Block on the worker startup gate pipe; a cancelled gate aborts the worker.
#[cfg(unix)]
pub fn wait_for_daemon_worker_startup_gate(environment: &mut HashMap<String, String>) -> Result<(), String> {
    let Some(raw_fd) = environment.remove(DAEMON_WORKER_STARTUP_GATE_FD_ENV) else {
        return Ok(());
    };
    let fd: i32 = raw_fd
        .parse()
        .map_err(|_| "Daemon session worker has an invalid startup gate".to_string())?;
    if fd < 3 {
        return Err("Daemon session worker has an invalid startup gate".to_string());
    }
    use std::os::fd::FromRawFd;
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut marker = String::new();
    let read_result = std::io::Read::read_to_string(&mut file, &mut marker);
    drop(file);
    read_result.map_err(|error| error.to_string())?;
    if marker != DAEMON_WORKER_STARTUP_GATE_COMMIT {
        return Err("Daemon session worker startup was cancelled".to_string());
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn wait_for_daemon_worker_startup_gate(environment: &mut HashMap<String, String>) -> Result<(), String> {
    let Some(_raw_fd) = environment.remove(DAEMON_WORKER_STARTUP_GATE_FD_ENV) else {
        return Ok(());
    };
    Err("Daemon session worker startup gate is unavailable on this platform".to_string())
}

pub fn daemon_worker_instance_id(environment: &HashMap<String, String>) -> Option<String> {
    environment
        .get(DAEMON_WORKER_INSTANCE_ID_ENV)
        .filter(|value| !value.is_empty())
        .cloned()
}

pub fn require_daemon_worker_authentication_token(environment: &HashMap<String, String>) -> Result<String, String> {
    environment
        .get(DAEMON_WORKER_TOKEN_ENV)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| "Daemon session worker is missing its authentication token".to_string())
}

/// The worker command bodies this slice sends; unknown commands keep their JSON body.
pub fn worker_command(type_: &str, fields: &[(&str, Value)]) -> DaemonCommand {
    let mut command = DaemonCommand::new(type_);
    for (key, value) in fields {
        command.body.insert((*key).to_string(), value.clone());
    }
    command
}

pub fn worker_command_body(command: &DaemonCommand) -> Map<String, Value> {
    command.body_without_id()
}

pub fn hello_from_value(value: &Value) -> Option<DaemonHello> {
    DaemonHello::from_value(value)
}

/// Path helpers shared with the supervisor's descriptor layout.
pub fn join_path(base: &str, name: &str) -> String {
    let base = base.trim_end_matches(['/', '\\']);
    format!("{base}/{name}")
}

pub fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_header_validation_matches_the_typescript() {
        assert!(is_daemon_worker_frame_header(&serde_json::json!({
            "kind": "command",
            "requestId": "worker_1",
            "commandType": "worker_auth"
        })));
        assert!(!is_daemon_worker_frame_header(&serde_json::json!({
            "kind": "command",
            "requestId": 1,
            "commandType": "worker_auth"
        })));
        assert!(is_daemon_worker_frame_header(&serde_json::json!({
            "kind": "outbound",
            "outboundType": "daemon_hello",
            "payloadEncoding": "jsonl"
        })));
        assert!(!is_daemon_worker_frame_header(&serde_json::json!({
            "kind": "outbound",
            "outboundType": "daemon_hello",
            "payloadEncoding": "protobuf"
        })));
        assert!(!is_daemon_worker_frame_header(&serde_json::json!({ "kind": "other" })));
    }

    #[test]
    fn durable_create_command_keeps_only_the_persisted_fields() {
        let mut command = DaemonCommand::new("create");
        command.body.insert("sessionPath".to_string(), Value::String("/tmp/s.jsonl".to_string()));
        command.body.insert("config".to_string(), serde_json::json!({ "cwd": "/tmp" }));
        let durable = durable_daemon_create_command(&command);
        assert_eq!(durable.type_, "create");
        assert_eq!(durable.session_path.as_deref(), Some("/tmp/s.jsonl"));
        assert!(durable.no_session.is_none());
    }

    #[test]
    fn worker_environment_helpers() {
        let mut environment = HashMap::new();
        assert!(!is_daemon_worker_process(&environment));
        environment.insert(DAEMON_WORKER_ROLE_ENV.to_string(), "1".to_string());
        assert!(is_daemon_worker_process(&environment));
        environment.insert(DAEMON_WORKER_INSTANCE_ID_ENV.to_string(), "".to_string());
        assert!(daemon_worker_instance_id(&environment).is_none());
        environment.insert(DAEMON_WORKER_INSTANCE_ID_ENV.to_string(), "abc".to_string());
        assert_eq!(daemon_worker_instance_id(&environment).as_deref(), Some("abc"));
        assert_eq!(
            require_daemon_worker_authentication_token(&environment),
            Err("Daemon session worker is missing its authentication token".to_string())
        );
        environment.insert(DAEMON_WORKER_TOKEN_ENV.to_string(), "token".to_string());
        assert_eq!(require_daemon_worker_authentication_token(&environment).unwrap(), "token");
    }

    #[test]
    fn startup_gate_rejects_an_invalid_fd() {
        let mut environment = HashMap::new();
        environment.insert(DAEMON_WORKER_STARTUP_GATE_FD_ENV.to_string(), "nope".to_string());
        assert_eq!(
            wait_for_daemon_worker_startup_gate(&mut environment),
            Err("Daemon session worker has an invalid startup gate".to_string())
        );
        assert!(!environment.contains_key(DAEMON_WORKER_STARTUP_GATE_FD_ENV));
    }

    #[test]
    fn peer_ticket_parsing_requires_identity_and_purpose() {
        let value = serde_json::json!({
            "purpose": "session_client",
            "socketPath": "/tmp/w.sock",
            "socketIdentity": { "dev": 1, "ino": 2 },
            "workerInstanceId": "w1",
            "activeSessionId": "a1",
            "grantId": "g1",
            "token": "t",
            "expiresAt": "2030-01-01T00:00:00.000Z"
        });
        assert!(DaemonPeerTransportTicket::from_value(&value).is_some());
        let mut broken = value.clone();
        broken["socketIdentity"] = serde_json::json!({ "dev": 1 });
        assert!(DaemonPeerTransportTicket::from_value(&broken).is_none());
        let mut wrong_purpose = value;
        wrong_purpose["purpose"] = Value::String("supervisor".to_string());
        assert!(DaemonPeerTransportTicket::from_value(&wrong_purpose).is_none());
    }
}
