//! Existing supervisor RPCs used by worker-side agent messaging.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::core::agent_messages::{
    AgentSessionMessageAgentSummary, AgentSessionMessageReceipt,
    DELIVERY_STATUS_DELIVERED, DELIVERY_STATUS_QUEUED,
};
use crate::modes::daemon::daemon_client::DaemonClient;
use crate::modes::daemon::daemon_errors::deserialize_daemon_error;

pub(super) async fn list_peers(
    socket_path: &str,
    worker_token: &str,
) -> Vec<AgentSessionMessageAgentSummary> {
    let client = Arc::new(DaemonClient::new(socket_path));
    let result = async {
        client.connect(1_000).await.map_err(|error| error.to_string())?;
        client.wait_for_hello(1_000).await.map_err(|error| error.to_string())?;
        // request() checks the existing schema-revision compatibility gate.
        let response = client.request(
            json!({"type": "list_agent_peers", "workerToken": worker_token})
                .as_object().expect("command object").clone(),
            Some(5_000),
            Default::default(),
        ).await.map_err(|error| error.to_string())?;
        if !response.success {
            return Err(deserialize_daemon_error(&response).to_string());
        }
        serde_json::from_value::<Vec<AgentSessionMessageAgentSummary>>(
            response.data.and_then(|data| data.get("peers").cloned()).unwrap_or(Value::Null),
        ).map_err(|error| error.to_string())
    }.await;
    client.close().await;
    // Discovery is optional; an older or temporarily unavailable supervisor must
    // not prevent local child messaging or session startup.
    result.unwrap_or_default()
}

pub(super) async fn send_message(
    socket_path: &str,
    from_active_session_id: &str,
    target_selector: &str,
    message: &str,
    shutting_down: &AtomicBool,
) -> Result<AgentSessionMessageReceipt, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut last_error = format!("Unknown active session: {target_selector}");
    let client = loop {
        if shutting_down.load(Ordering::SeqCst) {
            return Err("Daemon is shutting down".to_string());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(last_error);
        }
        let candidate = Arc::new(DaemonClient::new(socket_path));
        let connected = async {
            candidate.connect(1_000).await?;
            candidate.wait_for_hello(1_000).await?;
            Ok::<_, crate::modes::daemon::daemon_client::DaemonClientError>(())
        }.await;
        match connected {
            Ok(()) => break candidate,
            Err(error) => {
                last_error = error.to_string();
                candidate.close().await;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    // Retry connections only. A sent message is never replayed after a lost
    // response: it may already have been accepted by the other worker.
    let response = client.request(
        json!({
            "type": "send_message",
            "fromActiveSessionId": from_active_session_id,
            "targetActiveSessionId": target_selector,
            "message": message,
            "agentOrigin": true,
        }).as_object().expect("command object").clone(),
        Some(30_000),
        Default::default(),
    ).await;
    client.close().await;
    let response = response.map_err(|error| error.to_string())?;
    if !response.success {
        return Err(deserialize_daemon_error(&response).to_string());
    }
    let receipt: AgentSessionMessageReceipt = serde_json::from_value(
        response.data.unwrap_or(Value::Null),
    ).map_err(|_| "Supervisor returned an invalid agent-message receipt".to_string())?;
    if !matches!(receipt.delivery_status.as_str(), DELIVERY_STATUS_DELIVERED | DELIVERY_STATUS_QUEUED) {
        return Err("Supervisor returned an invalid agent-message receipt".to_string());
    }
    Ok(receipt)
}

#[cfg(test)]
mod messaging_safety_tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
    use crate::modes::daemon::daemon_protocol::{daemon_protocol_info, DAEMON_SCHEMA_REVISION};

    struct ScriptedSupervisor {
        socket: String,
        requests: Arc<std::sync::Mutex<Vec<Value>>>,
        task: tokio::task::JoinHandle<()>,
        _root: tempfile::TempDir,
    }

    impl Drop for ScriptedSupervisor {
        fn drop(&mut self) { self.task.abort(); }
    }

    async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
        socket: S,
        revision: u32,
        requests: Arc<std::sync::Mutex<Vec<Value>>>,
        answer: Option<Value>,
    ) {
        let (reader, mut writer) = tokio::io::split(socket);
        let mut hello = json!({"type":"daemon_hello", "protocol":daemon_protocol_info(), "schemaRevision":revision, "serverCapabilities":[]}).to_string();
        hello.push('\n');
        writer.write_all(hello.as_bytes()).await.unwrap();
        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            if line.trim().is_empty() { continue; }
            let envelope: Value = serde_json::from_str(&line).unwrap();
            let command = envelope.get("command").cloned().unwrap_or_else(|| envelope.clone());
            requests.lock().unwrap().push(command.clone());
            let Some(mut response) = answer.clone() else { return; };
            response["type"] = json!("response");
            response["id"] = envelope["id"].clone();
            response["command"] = command["type"].clone();
            let mut line = response.to_string();
            line.push('\n');
            writer.write_all(line.as_bytes()).await.unwrap();
        }
    }

    impl ScriptedSupervisor {
        fn new(revision: u32, answer: Option<Value>) -> Self {
            let root = tempfile::Builder::new().prefix("optimus-message-transport-").tempdir().unwrap();
            let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
            let captured = Arc::clone(&requests);
            #[cfg(windows)]
            let (socket, task) = {
                let socket = format!(r"\\.\pipe\optimus-message-transport-{}", uuid::Uuid::new_v4());
                let server = tokio::net::windows::named_pipe::ServerOptions::new().first_pipe_instance(true).create(&socket).unwrap();
                let task = tokio::spawn(async move {
                    server.connect().await.unwrap();
                    serve(server, revision, captured, answer).await;
                });
                (socket, task)
            };
            #[cfg(unix)]
            let (socket, task) = {
                let socket = root.path().join("supervisor.sock").to_string_lossy().into_owned();
                let server = tokio::net::UnixListener::bind(&socket).unwrap();
                let task = tokio::spawn(async move {
                    let (stream, _) = server.accept().await.unwrap();
                    serve(stream, revision, captured, answer).await;
                });
                (socket, task)
            };
            Self { socket, requests, task, _root: root }
        }
    }

    fn receipt(status: &str) -> Value {
        json!({"success":true,"data":{
            "id":"message-one", "source":"agent_message", "message":"hello peer",
            "target":{"activeSessionId":"target-active","sessionId":"target-session"},
            "from":{"activeSessionId":"source-active","sessionId":"source-session"},
            "deliveryStatus":status,
        }})
    }

    #[tokio::test]
    async fn messaging_safety_peer_discovery_uses_worker_token() {
        let peer = json!({"activeSessionId":"peer-active","sessionId":"peer-session","cwd":"fixture", "isStreaming":false, "unfinishedActionCount":0});
        let server = ScriptedSupervisor::new(DAEMON_SCHEMA_REVISION, Some(json!({"success":true,"data":{"peers":[peer]}})));
        let peers = list_peers(&server.socket, "private-worker-token").await;
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].active_session_id, "peer-active");
        assert_eq!(server.requests.lock().unwrap()[0], json!({"id":"daemon_1","type":"list_agent_peers","workerToken":"private-worker-token"}));
    }

    #[tokio::test]
    async fn messaging_safety_old_supervisor_discovery_degrades_without_request() {
        let server = ScriptedSupervisor::new(22, None);
        assert!(list_peers(&server.socket, "private-worker-token").await.is_empty());
        assert!(server.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn messaging_safety_remote_receipt_preserves_delivered_and_queued() {
        for status in [DELIVERY_STATUS_DELIVERED, DELIVERY_STATUS_QUEUED] {
            let server = ScriptedSupervisor::new(DAEMON_SCHEMA_REVISION, Some(receipt(status)));
            let result = send_message(&server.socket, "source-active", "target-active", "hello peer", &AtomicBool::new(false)).await.unwrap();
            assert_eq!(result.delivery_status, status);
            assert_eq!(result.from.unwrap().active_session_id.as_deref(), Some("source-active"));
            let requests = server.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0], json!({"id":"daemon_1","type":"send_message","fromActiveSessionId":"source-active","targetActiveSessionId":"target-active","message":"hello peer","agentOrigin":true}));
        }
    }

    #[tokio::test]
    async fn messaging_safety_rejected_admission_and_invalid_receipt_stay_errors() {
        for (response, expected) in [
            (json!({"success":false,"error":"Agent message was not accepted"}), "Agent message was not accepted"),
            (json!({"success":true,"data":{}}), "Supervisor returned an invalid agent-message receipt"),
            (receipt("rejected"), "Supervisor returned an invalid agent-message receipt"),
        ] {
            let server = ScriptedSupervisor::new(DAEMON_SCHEMA_REVISION, Some(response));
            let error = send_message(&server.socket, "source-active", "target-active", "hello peer", &AtomicBool::new(false)).await.unwrap_err();
            assert_eq!(error, expected);
            assert_eq!(server.requests.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn messaging_safety_lost_response_is_never_replayed() {
        let server = ScriptedSupervisor::new(DAEMON_SCHEMA_REVISION, None);
        let result = tokio::time::timeout(Duration::from_secs(5), send_message(&server.socket, "source-active", "target-active", "hello peer", &AtomicBool::new(false))).await.unwrap();
        assert!(result.is_err());
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn messaging_safety_shutdown_cancels_before_connecting() {
        let server = ScriptedSupervisor::new(DAEMON_SCHEMA_REVISION, Some(receipt(DELIVERY_STATUS_DELIVERED)));
        assert_eq!(send_message(&server.socket, "source-active", "target-active", "hello peer", &AtomicBool::new(true)).await.unwrap_err(), "Daemon is shutting down");
        assert!(server.requests.lock().unwrap().is_empty());
    }
}
