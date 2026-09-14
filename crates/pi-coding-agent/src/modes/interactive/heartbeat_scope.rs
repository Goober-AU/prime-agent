//! Port of packages/coding-agent/src/modes/interactive/heartbeat-scope.ts

use std::collections::HashSet;

use super::interactive_mode_services::{AgentConnectionHeartbeat, AgentConnectionRlmChildAgentSnapshot};

/// `HeartbeatSessionIdentity`
#[derive(Debug, Clone, Default)]
pub struct HeartbeatSessionIdentity {
    pub active_session_id: Option<String>,
    pub session_id: String,
}

/// Port of `scopeHeartbeatsToSession`.
pub fn scope_heartbeats_to_session(
    heartbeats: &[AgentConnectionHeartbeat],
    session: Option<&HeartbeatSessionIdentity>,
    children: &[AgentConnectionRlmChildAgentSnapshot],
) -> Vec<AgentConnectionHeartbeat> {
    let Some(session) = session else {
        return Vec::new();
    };

    let mut active_session_ids: HashSet<&str> = HashSet::new();
    if let Some(active) = session.active_session_id.as_deref() {
        active_session_ids.insert(active);
    }
    for child in children {
        if let Some(active) = child.active_session_id.as_deref() {
            active_session_ids.insert(active);
        }
    }

    heartbeats
        .iter()
        .filter(|heartbeat| {
            heartbeat.job.active_session_id == session.session_id
                || active_session_ids.contains(heartbeat.job.active_session_id.as_str())
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::interactive::interactive_mode_services::AgentCronJob;

    fn heartbeat(id: &str, job_session: &str, active_session: &str) -> AgentConnectionHeartbeat {
        AgentConnectionHeartbeat {
            job: AgentCronJob {
                id: id.to_string(),
                active_session_id: active_session.to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn no_session_scopes_to_empty() {
        let heartbeats = vec![heartbeat("h1", "s1", "a1")];
        assert!(scope_heartbeats_to_session(&heartbeats, None, &[]).is_empty());
    }

    #[test]
    fn keeps_session_and_child_active_session_heartbeats() {
        let heartbeats = vec![
            heartbeat("own", "s1", "a1"),
            heartbeat("child", "s2", "child-active"),
            heartbeat("foreign", "s3", "other"),
        ];
        let session = HeartbeatSessionIdentity { active_session_id: Some("a1".into()), session_id: "s1".into() };
        let child = AgentConnectionRlmChildAgentSnapshot {
            active_session_id: Some("child-active".into()),
            ..Default::default()
        };
        let scoped = scope_heartbeats_to_session(&heartbeats, Some(&session), &[child]);
        let ids: Vec<&str> = scoped.iter().map(|heartbeat| heartbeat.job.id.as_str()).collect();
        assert_eq!(ids, vec!["own", "child"]);
    }
}
