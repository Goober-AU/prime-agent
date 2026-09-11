//! Port of packages/coding-agent/src/modes/daemon/heartbeat-catalog.ts

use super::daemon_client::protocol::{DaemonCommand, DaemonResponse};
use super::daemon_client::{DaemonClientError, DaemonClientResult};
use super::daemon_errors::{deserialize_daemon_error, DaemonErrorResponse};
use super::daemon_client::protocol::is_unknown_daemon_command_error;

/// One heartbeat row as the agents view consumes it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentConnectionHeartbeat {
    #[serde(flatten, default)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// The transport surface `listDaemonHeartbeats` needs (DaemonTransportClient).
pub trait HeartbeatCatalogClient: Send + Sync {
    fn hello_present(&self) -> bool;
    fn wait_for_hello_boxed(&self, timeout_ms: u64) -> futures::future::BoxFuture<'static, DaemonClientResult<()>>;
    fn supports_server_capability(&self, capability: &str) -> bool;
    fn request_boxed(
        &self,
        command: DaemonCommand,
        timeout_ms: Option<u64>,
    ) -> futures::future::BoxFuture<'static, DaemonClientResult<DaemonResponse>>;
}

pub async fn list_daemon_heartbeats(
    client: &dyn HeartbeatCatalogClient,
    active_session_id: Option<&str>,
) -> DaemonClientResult<Vec<AgentConnectionHeartbeat>> {
    if !client.hello_present() {
        client.wait_for_hello_boxed(3000).await?;
    }
    if !client.supports_server_capability("heartbeat_catalog") {
        return Ok(Vec::new());
    }
    let mut command = DaemonCommand::new("heartbeats_list");
    if let Some(active_session_id) = active_session_id {
        command.body.insert(
            "activeSessionId".to_string(),
            serde_json::Value::String(active_session_id.to_string()),
        );
    }
    let response = client.request_boxed(command, None).await?;
    if !response.success {
        return Err(daemon_error_from_response(&response));
    }
    let heartbeats = response
        .data
        .as_ref()
        .and_then(|data| data.get("heartbeats"))
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| AgentConnectionHeartbeat {
                    fields: entry.as_object().cloned().unwrap_or_default(),
                })
                .collect::<Vec<AgentConnectionHeartbeat>>()
        })
        .unwrap_or_default();
    Ok(heartbeats)
}

fn daemon_error_from_response(response: &DaemonResponse) -> DaemonClientError {
    let error = deserialize_daemon_error(&DaemonErrorResponse {
        error: response.error.clone().unwrap_or_default(),
        error_info: response.error_info.clone(),
    });
    DaemonClientError::Message(error.message())
}

/// True when the daemon rejected `heartbeats_list` because its build predates it.
pub fn is_unknown_heartbeats_command(error: &DaemonClientError) -> bool {
    is_unknown_daemon_command_error(&error.message(), "heartbeats_list")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct FakeClient {
        hello: bool,
        capability: bool,
        response: Mutex<Option<serde_json::Value>>,
        commands: Arc<Mutex<Vec<DaemonCommand>>>,
    }

    impl HeartbeatCatalogClient for FakeClient {
        fn hello_present(&self) -> bool {
            self.hello
        }

        fn wait_for_hello_boxed(
            &self,
            _timeout_ms: u64,
        ) -> futures::future::BoxFuture<'static, DaemonClientResult<()>> {
            Box::pin(async move { Ok(()) })
        }

        fn supports_server_capability(&self, capability: &str) -> bool {
            capability == "heartbeat_catalog" && self.capability
        }

        fn request_boxed(
            &self,
            command: DaemonCommand,
            _timeout_ms: Option<u64>,
        ) -> futures::future::BoxFuture<'static, DaemonClientResult<DaemonResponse>> {
            let commands = Arc::clone(&self.commands);
            let response = self.response.lock().expect("response poisoned").clone();
            Box::pin(async move {
                commands.lock().expect("commands poisoned").push(command);
                let value = response.expect("fake response");
                DaemonResponse::from_value(&value)
                    .ok_or_else(|| DaemonClientError::Message("invalid response".to_string()))
            })
        }
    }

    fn client(capability: bool, response: serde_json::Value) -> FakeClient {
        FakeClient {
            hello: true,
            capability,
            response: Mutex::new(Some(response)),
            commands: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[tokio::test]
    async fn returns_empty_without_the_capability() {
        let client = client(false, serde_json::json!({}));
        let heartbeats = list_daemon_heartbeats(&client, None).await.expect("ok");
        assert!(heartbeats.is_empty());
        assert!(client.commands.lock().expect("commands").is_empty());
    }

    #[tokio::test]
    async fn returns_the_heartbeats_and_forwards_the_session_id() {
        let client = client(
            true,
            serde_json::json!({
                "type": "response",
                "command": "heartbeats_list",
                "success": true,
                "data": { "heartbeats": [{ "id": "h1" }] }
            }),
        );
        let heartbeats = list_daemon_heartbeats(&client, Some("active-1")).await.expect("ok");
        assert_eq!(heartbeats.len(), 1);
        assert_eq!(heartbeats[0].fields.get("id").and_then(|v| v.as_str()), Some("h1"));
        let commands = client.commands.lock().expect("commands");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].type_, "heartbeats_list");
        assert_eq!(commands[0].string_field("activeSessionId"), Some("active-1"));
    }

    #[tokio::test]
    async fn a_failed_response_becomes_a_daemon_error() {
        let client = client(
            true,
            serde_json::json!({
                "type": "response",
                "command": "heartbeats_list",
                "success": false,
                "error": "Unknown daemon command: heartbeats_list"
            }),
        );
        let error = list_daemon_heartbeats(&client, None).await.expect_err("fails");
        assert!(is_unknown_heartbeats_command(&error));
    }
}
