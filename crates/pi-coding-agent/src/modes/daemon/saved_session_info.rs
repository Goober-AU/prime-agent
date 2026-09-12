//! Port of packages/coding-agent/src/modes/daemon/saved-session-info.ts

use super::daemon_protocol::DaemonSavedSessionInfo;

/// The client-facing saved-session shape, re-exported where the TypeScript
/// module imports it (`agent-connection/types.ts`).
pub use crate::modes::agent_connection::types::AgentConnectionSavedSessionInfo;

/// `serializeSavedSessionInfo(session: SessionInfo): DaemonSavedSessionInfo`.
pub fn serialize_saved_session_info(session: &crate::core::session_manager::SessionInfo) -> DaemonSavedSessionInfo {
    DaemonSavedSessionInfo {
        path: session.path.clone(),
        id: session.id.clone(),
        cwd: session.cwd.clone(),
        name: session.name.clone(),
        state: session.state.as_ref().and_then(project),
        parent_session_path: session.parent_session_path.clone(),
        rlm_depth: Some(session.rlm_depth as f64),
        created: iso_from_ms(session.created),
        modified: iso_from_ms(session.modified),
        message_count: session.message_count as f64,
        first_message: session.first_message.clone(),
        all_messages_text: session.all_messages_text.clone(),
        agent_status: session.agent_status.as_ref().and_then(project),
        usage: session.usage.as_ref().and_then(project),
    }
}

/// `session.state` / `session.agentStatus` / `session.usage` are copied straight
/// across in the TypeScript; the port moves them through their JSON shape
/// because the core session types and the wire types are separate structs.
fn project<T: serde::Serialize, U: serde::de::DeserializeOwned>(value: &T) -> Option<U> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
}

/// `deserializeSavedSessionInfo(session): AgentConnectionSavedSessionInfo`.
pub fn deserialize_saved_session_info(session: &DaemonSavedSessionInfo) -> AgentConnectionSavedSessionInfo {
    AgentConnectionSavedSessionInfo {
        path: session.path.clone(),
        id: session.id.clone(),
        cwd: session.cwd.clone(),
        name: session.name.clone(),
        state: session.state.clone(),
        parent_session_path: session.parent_session_path.clone(),
        rlm_depth: session.rlm_depth,
        created: parse_iso_ms(&session.created),
        modified: parse_iso_ms(&session.modified),
        message_count: session.message_count,
        first_message: session.first_message.clone(),
        all_messages_text: session.all_messages_text.clone(),
        agent_status: session.agent_status.clone(),
        usage: session.usage.as_ref().and_then(project),
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
    use crate::core::session_manager::{AgentStatus, AgentTaskState, SessionInfo, SessionState, SessionStateStatus};

    fn session() -> SessionInfo {
        SessionInfo {
            path: "/tmp/s.jsonl".to_string(),
            id: "abc".to_string(),
            cwd: "/tmp".to_string(),
            name: Some("named".to_string()),
            state: Some(SessionState { status: SessionStateStatus::Archived }),
            parent_session_path: Some("/tmp/p.jsonl".to_string()),
            rlm_depth: 1,
            created: 0.0,
            modified: 1_000.0,
            message_count: 2,
            first_message: "hello".to_string(),
            all_messages_text: "hello world".to_string(),
            agent_status: Some(AgentStatus {
                summary: "done".to_string(),
                task_state: Some(AgentTaskState::Completed),
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
        assert_eq!(restored.message_count, 2.0);
        assert_eq!(
            restored.state.map(|state| state.status).as_deref(),
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
