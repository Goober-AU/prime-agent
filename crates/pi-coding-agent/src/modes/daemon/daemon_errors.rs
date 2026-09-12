//! Port of packages/coding-agent/src/modes/daemon/daemon-errors.ts

use serde::{Deserialize, Serialize};

use crate::core::session_cwd::{format_missing_session_cwd_error, MissingSessionCwdError, SessionCwdIssue};
use crate::core::session_import_errors::SessionImportFileNotFoundError;
use crate::core::session_lease::SessionAlreadyActiveError;
use crate::modes::daemon::daemon_protocol::DaemonResponse;

/// A known session (a persisted descriptor names it) that cannot be routed to yet; retryable, unlike "Unknown active session".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSessionRecoveringError {
    pub active_session_id: String,
}

impl DaemonSessionRecoveringError {
    pub const CODE: &'static str = "session_recovering";

    pub fn new(active_session_id: impl Into<String>) -> Self {
        Self { active_session_id: active_session_id.into() }
    }

    pub fn message(&self) -> String {
        format!("Active session {} is recovering; retry shortly", self.active_session_id)
    }
}

impl std::fmt::Display for DaemonSessionRecoveringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for DaemonSessionRecoveringError {}

/// `DaemonErrorInfo` - the structured `errorInfo` carried on a failed response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code")]
pub enum DaemonErrorInfo {
    #[serde(rename = "missing_session_cwd")]
    MissingSessionCwd { issue: SessionCwdIssue },
    #[serde(rename = "session_import_file_not_found")]
    SessionImportFileNotFound {
        #[serde(rename = "filePath")]
        file_path: String,
    },
    #[serde(rename = "session_already_active")]
    SessionAlreadyActive {
        #[serde(rename = "sessionPath")]
        session_path: String,
        #[serde(rename = "activeSessionId", skip_serializing_if = "Option::is_none", default)]
        active_session_id: Option<String>,
    },
    #[serde(rename = "session_recovering")]
    SessionRecovering {
        #[serde(rename = "activeSessionId")]
        active_session_id: String,
    },
    #[serde(rename = "command_result_uncertain")]
    CommandResultUncertain {
        #[serde(rename = "clientId")]
        client_id: String,
        #[serde(rename = "commandId")]
        command_id: String,
    },
}

impl DaemonErrorInfo {
    pub fn code(&self) -> &'static str {
        match self {
            DaemonErrorInfo::MissingSessionCwd { .. } => "missing_session_cwd",
            DaemonErrorInfo::SessionImportFileNotFound { .. } => "session_import_file_not_found",
            DaemonErrorInfo::SessionAlreadyActive { .. } => "session_already_active",
            DaemonErrorInfo::SessionRecovering { .. } => "session_recovering",
            DaemonErrorInfo::CommandResultUncertain { .. } => "command_result_uncertain",
        }
    }
}

/// The daemon's known error shapes plus the untyped fallback. The TypeScript
/// branches on `instanceof`, so the variant set is the class set it checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonError {
    MissingSessionCwd(MissingSessionCwdError),
    SessionImportFileNotFound(SessionImportFileNotFoundError),
    SessionAlreadyActive(SessionAlreadyActiveError),
    SessionRecovering(DaemonSessionRecoveringError),
    /// `DaemonSessionCreateError` / any other `Error`.
    Message(String),
}

impl DaemonError {
    pub fn message(&self) -> String {
        match self {
            DaemonError::MissingSessionCwd(error) => format_missing_session_cwd_error(&error.issue),
            DaemonError::SessionImportFileNotFound(error) => error.to_string(),
            DaemonError::SessionAlreadyActive(error) => error.to_string(),
            DaemonError::SessionRecovering(error) => error.message(),
            DaemonError::Message(message) => message.clone(),
        }
    }
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for DaemonError {}

pub fn serialize_daemon_error(error: &DaemonError) -> Option<DaemonErrorInfo> {
    match error {
        DaemonError::MissingSessionCwd(inner) => Some(DaemonErrorInfo::MissingSessionCwd {
            issue: inner.issue.clone(),
        }),
        DaemonError::SessionImportFileNotFound(inner) => Some(DaemonErrorInfo::SessionImportFileNotFound {
            file_path: inner.file_path.clone(),
        }),
        DaemonError::SessionAlreadyActive(inner) => Some(DaemonErrorInfo::SessionAlreadyActive {
            session_path: inner.session_path.clone(),
            active_session_id: inner.active_session_id.clone(),
        }),
        DaemonError::SessionRecovering(inner) => Some(DaemonErrorInfo::SessionRecovering {
            active_session_id: inner.active_session_id.clone(),
        }),
        DaemonError::Message(_) => None,
    }
}

/// `DaemonSessionCreateError extends Error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSessionCreateError {
    pub message: String,
}

impl DaemonSessionCreateError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl std::fmt::Display for DaemonSessionCreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DaemonSessionCreateError {}

/// Wraps untyped create failures so the CLI prints one line instead of rethrowing.
pub fn deserialize_daemon_create_error(response: &DaemonResponse) -> DaemonError {
    let error = deserialize_daemon_error(response);
    if response.error_info.is_some() {
        return error;
    }
    DaemonError::Message(DaemonSessionCreateError::new(error.message()).message)
}

pub fn deserialize_daemon_error(response: &DaemonResponse) -> DaemonError {
    match &response.error_info {
        Some(DaemonErrorInfo::MissingSessionCwd { issue }) => {
            DaemonError::MissingSessionCwd(MissingSessionCwdError::new(issue.clone()))
        }
        Some(DaemonErrorInfo::SessionImportFileNotFound { file_path }) => {
            DaemonError::SessionImportFileNotFound(SessionImportFileNotFoundError::new(file_path.clone()))
        }
        Some(DaemonErrorInfo::SessionAlreadyActive {
            session_path,
            active_session_id,
        }) => DaemonError::SessionAlreadyActive(SessionAlreadyActiveError::new(
            session_path.clone(),
            active_session_id.clone(),
        )),
        Some(DaemonErrorInfo::SessionRecovering { active_session_id }) => {
            DaemonError::SessionRecovering(DaemonSessionRecoveringError::new(active_session_id.clone()))
        }
        // `command_result_uncertain` has no dedicated class, so it stays a plain error.
        Some(DaemonErrorInfo::CommandResultUncertain { .. }) | None => {
            DaemonError::Message(response.error.clone().unwrap_or_default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(error: &str, error_info: Option<DaemonErrorInfo>) -> DaemonResponse {
        DaemonResponse {
            id: None,
            type_: "response".to_string(),
            command: "create".to_string(),
            success: false,
            data: None,
            error: Some(error.to_string()),
            error_info,
        }
    }

    #[test]
    fn wraps_generic_create_failures_so_the_cli_boundary_prints_one_line() {
        let error = deserialize_daemon_create_error(&response(
            "Failed to spawn session worker: spawn node EMFILE",
            None,
        ));
        assert_eq!(error.message(), "Failed to spawn session worker: spawn node EMFILE");
        assert!(matches!(error, DaemonError::Message(_)));
    }

    #[test]
    fn preserves_typed_daemon_errors_for_their_dedicated_boundaries() {
        let error = deserialize_daemon_create_error(&response(
            "session already active",
            Some(DaemonErrorInfo::SessionAlreadyActive {
                session_path: "/tmp/session.jsonl".to_string(),
                active_session_id: None,
            }),
        ));
        match error {
            DaemonError::SessionAlreadyActive(inner) => {
                assert_eq!(inner.session_path, "/tmp/session.jsonl");
                assert_eq!(inner.active_session_id, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn serializes_for_old_clients_and_deserializes_for_new_clients() {
        let error = DaemonError::SessionRecovering(DaemonSessionRecoveringError::new("active-gap"));
        let error_info = serialize_daemon_error(&error).expect("known");
        assert_eq!(
            serde_json::to_value(&error_info).unwrap(),
            serde_json::json!({ "code": "session_recovering", "activeSessionId": "active-gap" })
        );
        // Old client / new daemon: errorInfo is ignored, the message carries the state.
        assert_eq!(error.message(), "Active session active-gap is recovering; retry shortly");

        let round_tripped = deserialize_daemon_error(&response(&error.message(), Some(error_info)));
        match round_tripped {
            DaemonError::SessionRecovering(inner) => assert_eq!(inner.active_session_id, "active-gap"),
            other => panic!("unexpected {other:?}"),
        }

        let legacy = deserialize_daemon_error(&response("Unknown active session: active-gap", None));
        assert!(!matches!(legacy, DaemonError::SessionRecovering(_)));
    }

    #[test]
    fn command_result_uncertain_without_a_class_stays_a_plain_error() {
        let error = deserialize_daemon_error(&response(
            "uncertain",
            Some(DaemonErrorInfo::CommandResultUncertain {
                client_id: "client-1".to_string(),
                command_id: "cmd-1".to_string(),
            }),
        ));
        assert_eq!(error.message(), "uncertain");
    }
}
