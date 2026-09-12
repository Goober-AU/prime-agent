//! Port of packages/coding-agent/src/modes/daemon/saved-session-info.ts

use serde_json::Value;

use super::daemon_protocol::DaemonSavedSessionInfo;
use super::daemon_session_list::{AgentStatusRecord, SessionInfo, SessionState};

/// `SessionInfo` on the wire: dates become ISO strings.
pub fn serialize_saved_session_info(session: &SessionInfo) -> DaemonSavedSessionInfo {
    DaemonSavedSessionInfo {
        path: session.path.clone(),
        id: session.id.clone(),
        cwd: session.cwd.clone(),
        name: session.name.clone(),
        state: session
            .state
            .as_ref()
            .map(|state| serde_json::json!({ "status": state.status })),
        parent_session_path: session.parent_session_path.clone(),
        rlm_depth: session.rlm_depth,
        created: iso_from_ms(session.created_ms),
        modified: iso_from_ms(session.modified_ms),
        message_count: session.message_count as i64,
        first_message: session.first_message.clone(),
        all_messages_text: session.all_messages_text.clone(),
        agent_status: session.agent_status.as_ref().map(|status| {
            serde_json::json!({
                "summary": status.summary,
                "taskState": status.task_state,
                "basedOnMessageCount": status.based_on_message_count,
            })
        }),
        usage: session.usage.clone(),
    }
}

/// The client-facing saved-session shape (`AgentConnectionSavedSessionInfo`).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentConnectionSavedSessionInfo {
    pub path: String,
    pub id: String,
    pub cwd: String,
    pub name: Option<String>,
    pub state: Option<SessionState>,
    pub parent_session_path: Option<String>,
    pub rlm_depth: Option<i64>,
    pub created: f64,
    pub modified: f64,
    pub message_count: usize,
    pub first_message: String,
    pub all_messages_text: String,
    pub agent_status: Option<AgentStatusRecord>,
    pub usage: Option<Value>,
}

pub fn deserialize_saved_session_info(session: &DaemonSavedSessionInfo) -> AgentConnectionSavedSessionInfo {
    AgentConnectionSavedSessionInfo {
        path: session.path.clone(),
        id: session.id.clone(),
        cwd: session.cwd.clone(),
        name: session.name.clone(),
        state: session.state.as_ref().map(|state| SessionState {
            status: state.get("status").and_then(Value::as_str).map(str::to_string),
        }),
        parent_session_path: session.parent_session_path.clone(),
        rlm_depth: session.rlm_depth,
        created: parse_iso_ms(&session.created),
        modified: parse_iso_ms(&session.modified),
        message_count: session.message_count.max(0) as usize,
        first_message: session.first_message.clone(),
        all_messages_text: session.all_messages_text.clone(),
        agent_status: session.agent_status.as_ref().map(|status| AgentStatusRecord {
            summary: status
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            task_state: status
                .get("taskState")
                .and_then(Value::as_str)
                .map(str::to_string),
            based_on_message_count: status
                .get("basedOnMessageCount")
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as usize,
        }),
        usage: session.usage.clone(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionInfo {
        SessionInfo {
            path: "/tmp/s.jsonl".to_string(),
            id: "abc".to_string(),
            cwd: "/tmp".to_string(),
            name: Some("named".to_string()),
            state: Some(SessionState {
                status: Some("archived".to_string()),
            }),
            parent_session_path: Some("/tmp/p.jsonl".to_string()),
            rlm_depth: Some(1),
            created_ms: 0.0,
            modified_ms: 1_000.0,
            message_count: 2,
            first_message: "hello".to_string(),
            all_messages_text: "hello world".to_string(),
            agent_status: Some(AgentStatusRecord {
                summary: "done".to_string(),
                task_state: Some("completed".to_string()),
                based_on_message_count: 2,
            }),
            usage: None,
        }
    }

    #[test]
    fn round_trips_through_the_wire_shape() {
        let original = session();
        let wire = serialize_saved_session_info(&original);
        assert_eq!(wire.created, "1970-01-01T00:00:00.000Z");
        assert_eq!(wire.modified, "1970-01-01T00:00:01.000Z");
        let restored = deserialize_saved_session_info(&wire);
        assert_eq!(restored.path, original.path);
        assert_eq!(restored.id, original.id);
        assert_eq!(restored.created, 0.0);
        assert_eq!(restored.modified, 1000.0);
        assert_eq!(restored.message_count, 2);
        assert_eq!(
            restored.state.and_then(|state| state.status).as_deref(),
            Some("archived")
        );
        assert_eq!(
            restored.agent_status.and_then(|status| status.task_state).as_deref(),
            Some("completed")
        );
    }

    #[test]
    fn absent_fields_stay_absent() {
        let mut bare = session();
        bare.name = None;
        bare.state = None;
        bare.parent_session_path = None;
        bare.rlm_depth = None;
        bare.agent_status = None;
        let wire = serialize_saved_session_info(&bare);
        assert!(wire.name.is_none());
        assert!(wire.state.is_none());
        assert!(wire.parent_session_path.is_none());
        assert!(wire.agent_status.is_none());
        let restored = deserialize_saved_session_info(&wire);
        assert!(restored.name.is_none());
        assert!(restored.state.is_none());
    }
}
