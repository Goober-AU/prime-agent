//! Port of packages/coding-agent/src/modes/daemon/saved-session-catalog.ts

use std::sync::Arc;

use serde_json::{Map, Value};

use super::daemon_client::{DaemonClientError, DaemonClientResult, DaemonCommandBody};
use super::daemon_errors::deserialize_daemon_error;
use super::daemon_protocol::{DaemonResponse, DaemonSavedSessionInfo};
use super::saved_session_info::{deserialize_saved_session_info, AgentConnectionSavedSessionInfo};

/// `{ activeSessionId } | { cwd, sessionDir? }` from the TypeScript union.
#[derive(Debug, Clone, PartialEq)]
pub enum DaemonSavedSessionCatalogContext {
    ActiveSessionId { active_session_id: String },
    Cwd { cwd: String, session_dir: Option<String> },
}

/// `AgentConnectionSavedSessionScope` (agent-connection/types.ts).
pub type AgentConnectionSavedSessionScope = String;

/// The client-facing callbacks for a streamed session list.
#[derive(Clone)]
pub struct AgentConnectionSessionListCallbacks {
    pub on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    pub on_session: Option<Arc<dyn Fn(AgentConnectionSavedSessionInfo) + Send + Sync>>,
}

/// The transport surface the catalog helpers need.
pub trait SavedSessionCatalogClient: Send + Sync {
    fn request_boxed(
        &self,
        command: DaemonCommandBody,
        timeout_ms: Option<u64>,
        on_progress: Option<Arc<dyn Fn(&Value) + Send + Sync>>,
    ) -> futures::future::BoxFuture<'static, DaemonClientResult<DaemonResponse>>;
}

pub async fn list_daemon_saved_sessions(
    client: &dyn SavedSessionCatalogClient,
    context: &DaemonSavedSessionCatalogContext,
    scope: &str,
    callbacks: Option<AgentConnectionSessionListCallbacks>,
) -> DaemonClientResult<Vec<AgentConnectionSavedSessionInfo>> {
    let mut command: DaemonCommandBody = Map::from_iter([(
        "type".to_string(),
        Value::String("list_saved_sessions".to_string()),
    )]);
    match context {
        DaemonSavedSessionCatalogContext::ActiveSessionId { active_session_id } => {
            command.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
        }
        DaemonSavedSessionCatalogContext::Cwd { cwd, session_dir } => {
            command.insert("cwd".to_string(), Value::String(cwd.clone()));
            if let Some(session_dir) = session_dir {
                command.insert("sessionDir".to_string(), Value::String(session_dir.clone()));
            }
        }
    }
    command.insert("scope".to_string(), Value::String(scope.to_string()));

    let progress: Option<Arc<dyn Fn(&Value) + Send + Sync>> = callbacks.as_ref().map(|callbacks| {
        let callbacks = callbacks.clone();
        Arc::new(move |update: &Value| {
            let Some(candidate) = update.as_object() else {
                return;
            };
            if candidate.get("type").and_then(Value::as_str) == Some("session_list_progress") {
                if let Some(on_progress) = &callbacks.on_progress {
                    on_progress(
                        candidate.get("loaded").and_then(Value::as_u64).unwrap_or(0),
                        candidate.get("total").and_then(Value::as_u64).unwrap_or(0),
                    );
                }
            } else if let Some(on_session) = &callbacks.on_session {
                if let Some(session) = candidate.get("session").cloned() {
                    if let Ok(session) = serde_json::from_value::<DaemonSavedSessionInfo>(session)
                    {
                        on_session(deserialize_saved_session_info(&session));
                    }
                }
            }
        }) as Arc<dyn Fn(&Value) + Send + Sync>
    });

    let response = client.request_boxed(command, Some(30000), progress).await?;
    if !response.success {
        return Err(daemon_error_from_response(&response));
    }
    let sessions = response
        .data
        .as_ref()
        .and_then(|data| data.get("sessions"))
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| serde_json::from_value::<DaemonSavedSessionInfo>(entry.clone()).ok())
                .map(|wire| deserialize_saved_session_info(&wire))
                .collect::<Vec<AgentConnectionSavedSessionInfo>>()
        })
        .unwrap_or_default();
    Ok(sessions)
}

pub async fn rename_daemon_saved_session(
    client: &dyn SavedSessionCatalogClient,
    context: &DaemonSavedSessionCatalogContext,
    session_path: &str,
    name: &str,
) -> DaemonClientResult<()> {
    let mut command: DaemonCommandBody = Map::from_iter([(
        "type".to_string(),
        Value::String("rename_saved_session".to_string()),
    )]);
    if let DaemonSavedSessionCatalogContext::ActiveSessionId { active_session_id } = context {
        command.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
    }
    command.insert("sessionPath".to_string(), Value::String(session_path.to_string()));
    command.insert("name".to_string(), Value::String(name.to_string()));
    let response = client.request_boxed(command, None, None).await?;
    if !response.success {
        return Err(daemon_error_from_response(&response));
    }
    Ok(())
}

pub async fn delete_daemon_saved_session(
    client: &dyn SavedSessionCatalogClient,
    context: &DaemonSavedSessionCatalogContext,
    session_path: &str,
) -> DaemonClientResult<Value> {
    let mut command: DaemonCommandBody = Map::from_iter([(
        "type".to_string(),
        Value::String("delete_saved_session".to_string()),
    )]);
    if let DaemonSavedSessionCatalogContext::ActiveSessionId { active_session_id } = context {
        command.insert("activeSessionId".to_string(), Value::String(active_session_id.clone()));
    }
    command.insert("sessionPath".to_string(), Value::String(session_path.to_string()));
    let response = client.request_boxed(command, None, None).await?;
    if !response.success {
        return Err(daemon_error_from_response(&response));
    }
    Ok(response.data.unwrap_or(Value::Null))
}

fn daemon_error_from_response(response: &DaemonResponse) -> DaemonClientError {
    // `throw deserializeDaemonError(response)`: the parser reads the whole response.
    let error = deserialize_daemon_error(response);
    DaemonClientError::Message(error.message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeClient {
        response: Mutex<serde_json::Value>,
        commands: Mutex<Vec<DaemonCommandBody>>,
    }

    impl SavedSessionCatalogClient for FakeClient {
        fn request_boxed(
            &self,
            command: DaemonCommandBody,
            _timeout_ms: Option<u64>,
            _on_progress: Option<Arc<dyn Fn(&Value) + Send + Sync>>,
        ) -> futures::future::BoxFuture<'static, DaemonClientResult<DaemonResponse>> {
            self.commands.lock().expect("commands poisoned").push(command);
            let response = self.response.lock().expect("response poisoned").clone();
            Box::pin(async move {
                DaemonResponse::from_value(&response)
                    .ok_or_else(|| DaemonClientError::Message("invalid response".to_string()))
            })
        }
    }

    /// A command body field, as the JSON object the client writes.
    fn field<'a>(command: &'a DaemonCommandBody, key: &str) -> Option<&'a str> {
        command.get(key).and_then(Value::as_str)
    }

    fn client(response: serde_json::Value) -> FakeClient {
        FakeClient {
            response: Mutex::new(response),
            commands: Mutex::new(Vec::new()),
        }
    }

    fn saved_session_json() -> serde_json::Value {
        serde_json::json!({
            "path": "/tmp/s.jsonl",
            "id": "abc",
            "cwd": "/tmp",
            "created": "1970-01-01T00:00:00.000Z",
            "modified": "1970-01-01T00:00:01.000Z",
            "messageCount": 1,
            "firstMessage": "hello",
            "allMessagesText": "hello"
        })
    }

    #[tokio::test]
    async fn list_sends_the_session_context_and_returns_rows() {
        let client = client(serde_json::json!({
            "type": "response",
            "command": "list_saved_sessions",
            "success": true,
            "data": { "sessions": [saved_session_json()] }
        }));
        let sessions = list_daemon_saved_sessions(
            &client,
            &DaemonSavedSessionCatalogContext::ActiveSessionId {
                active_session_id: "active-1".to_string(),
            },
            "all",
            None,
        )
        .await
        .expect("ok");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "abc");
        let commands = client.commands.lock().expect("commands poisoned");
        assert_eq!(field(&commands[0], "activeSessionId"), Some("active-1"));
        assert_eq!(field(&commands[0], "scope"), Some("all"));
        assert!(commands[0].get("cwd").is_none());
    }

    #[tokio::test]
    async fn list_sends_the_cwd_context_without_an_active_session() {
        let client = client(serde_json::json!({
            "type": "response",
            "command": "list_saved_sessions",
            "success": true,
            "data": { "sessions": [] }
        }));
        let sessions = list_daemon_saved_sessions(
            &client,
            &DaemonSavedSessionCatalogContext::Cwd {
                cwd: "/tmp".to_string(),
                session_dir: Some("/tmp/sessions".to_string()),
            },
            "cwd",
            None,
        )
        .await
        .expect("ok");
        assert!(sessions.is_empty());
        let commands = client.commands.lock().expect("commands poisoned");
        assert_eq!(field(&commands[0], "cwd"), Some("/tmp"));
        assert_eq!(field(&commands[0], "sessionDir"), Some("/tmp/sessions"));
        assert!(commands[0].get("activeSessionId").is_none());
    }

    #[tokio::test]
    async fn a_failed_response_is_a_daemon_error() {
        let client = client(serde_json::json!({
            "type": "response",
            "command": "list_saved_sessions",
            "success": false,
            "error": "boom"
        }));
        let error = list_daemon_saved_sessions(
            &client,
            &DaemonSavedSessionCatalogContext::Cwd {
                cwd: "/tmp".to_string(),
                session_dir: None,
            },
            "cwd",
            None,
        )
        .await
        .expect_err("fails");
        assert_eq!(error.message(), "boom");
    }

    #[tokio::test]
    async fn rename_and_delete_send_their_commands() {
        let client = client(serde_json::json!({
            "type": "response",
            "command": "rename_saved_session",
            "success": true
        }));
        rename_daemon_saved_session(
            &client,
            &DaemonSavedSessionCatalogContext::Cwd {
                cwd: "/tmp".to_string(),
                session_dir: None,
            },
            "/tmp/s.jsonl",
            "new name",
        )
        .await
        .expect("ok");
        let commands = client.commands.lock().expect("commands poisoned");
        assert_eq!(field(&commands[0], "type"), Some("rename_saved_session"));
        assert_eq!(field(&commands[0], "name"), Some("new name"));
        assert!(commands[0].get("activeSessionId").is_none());
    }
}
