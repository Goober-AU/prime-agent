//! Port of packages/coding-agent/src/modes/daemon/daemon-errors.ts

use serde::{Deserialize, Serialize};

/// Local minimal copy of `core/session-cwd.ts`'s `SessionCwdIssue` (that module
/// belongs to another slice). Wire field names match the TypeScript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCwdIssue {
    #[serde(rename = "sessionFile", skip_serializing_if = "Option::is_none", default)]
    pub session_file: Option<String>,
    #[serde(rename = "sessionCwd")]
    pub session_cwd: String,
    #[serde(rename = "fallbackCwd")]
    pub fallback_cwd: String,
}

/// A known session (a persisted descriptor names it) that cannot be routed to yet; retryable, unlike "Unknown active session".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSessionRecoveringError {
    pub active_session_id: String,
}

impl DaemonSessionRecoveringError {
    pub const CODE: &'static str = "session_recovering";

    pub fn new(active_session_id: impl Into<String>) -> Self {
        Self {
            active_session_id: active_session_id.into(),
        }
    }

    pub fn message(&self) -> String {
        format!(
            "Active session {} is recovering; retry shortly",
            self.active_session_id
        )
    }
}

impl std::fmt::Display for DaemonSessionRecoveringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for DaemonSessionRecoveringError {}

/// Local mirror of the core `MissingSessionCwdError`; the core module is owned by
/// another slice, so the daemon keeps its own copy until that type is shared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingSessionCwdError {
    pub issue: SessionCwdIssue,
}

impl MissingSessionCwdError {
    pub fn new(issue: SessionCwdIssue) -> Self {
        Self { issue }
    }

    pub fn message(&self) -> String {
        match &self.issue.session_file {
            Some(session_file) => format!(
                "Session {} was created in {} which no longer exists",
                session_file, self.issue.session_cwd
            ),
            None => format!(
                "Session working directory {} no longer exists",
                self.issue.session_cwd
            ),
        }
    }
}

impl std::fmt::Display for MissingSessionCwdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for MissingSessionCwdError {}

/// Local mirror of the core `SessionImportFileNotFoundError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionImportFileNotFoundError {
    pub file_path: String,
}

impl SessionImportFileNotFoundError {
    pub fn new(file_path: impl Into<String>) -> Self {
        Self {
            file_path: file_path.into(),
        }
    }

    pub fn message(&self) -> String {
        format!("Session file not found: {}", self.file_path)
    }
}

impl std::fmt::Display for SessionImportFileNotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for SessionImportFileNotFoundError {}

/// Local mirror of the core `SessionAlreadyActiveError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAlreadyActiveError {
    pub session_path: String,
    pub active_session_id: Option<String>,
}

impl SessionAlreadyActiveError {
    pub fn new(session_path: impl Into<String>, active_session_id: Option<String>) -> Self {
        Self {
            session_path: session_path.into(),
            active_session_id,
        }
    }

    pub fn message(&self) -> String {
        match &self.active_session_id {
            Some(id) => format!("Session {} is already active as {}", self.session_path, id),
            None => format!("Session {} is already active", self.session_path),
        }
    }
}

impl std::fmt::Display for SessionAlreadyActiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for SessionAlreadyActiveError {}

/// Typed daemon failure carried on a failed response, discriminated by `code`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum DaemonErrorInfo {
    MissingSessionCwd {
        issue: SessionCwdIssue,
    },
    SessionImportFileNotFound {
        #[serde(rename = "filePath")]
        file_path: String,
    },
    SessionAlreadyActive {
        #[serde(rename = "sessionPath")]
        session_path: String,
        #[serde(rename = "activeSessionId", skip_serializing_if = "Option::is_none", default)]
        active_session_id: Option<String>,
    },
    SessionRecovering {
        #[serde(rename = "activeSessionId")]
        active_session_id: String,
    },
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

    pub fn message(&self) -> String {
        match self {
            DaemonErrorInfo::MissingSessionCwd { issue } => MissingSessionCwdError::new(issue.clone()).message(),
            DaemonErrorInfo::SessionImportFileNotFound { file_path } => {
                SessionImportFileNotFoundError::new(file_path.clone()).message()
            }
            DaemonErrorInfo::SessionAlreadyActive {
                session_path,
                active_session_id,
            } => SessionAlreadyActiveError::new(session_path.clone(), active_session_id.clone()).message(),
            DaemonErrorInfo::SessionRecovering { active_session_id } => {
                DaemonSessionRecoveringError::new(active_session_id.clone()).message()
            }
            DaemonErrorInfo::CommandResultUncertain {
                client_id,
                command_id,
            } => format!(
                "Command {} from client {} has an uncertain result",
                command_id, client_id
            ),
        }
    }
}

/// Known daemon errors that carry structured wire info. Anything else serializes to `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonError {
    MissingSessionCwd(MissingSessionCwdError),
    SessionImportFileNotFound(SessionImportFileNotFoundError),
    SessionAlreadyActive(SessionAlreadyActiveError),
    SessionRecovering(DaemonSessionRecoveringError),
    /// Untyped failures (`new Error(message)` in TypeScript).
    Message(String),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonError::MissingSessionCwd(error) => f.write_str(&error.message()),
            DaemonError::SessionImportFileNotFound(error) => f.write_str(&error.message()),
            DaemonError::SessionAlreadyActive(error) => f.write_str(&error.message()),
            DaemonError::SessionRecovering(error) => f.write_str(&error.message()),
            DaemonError::Message(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DaemonError {}

impl DaemonError {
    pub fn message(&self) -> String {
        self.to_string()
    }
}

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

/// Wraps untyped create failures so the CLI prints one line instead of rethrowing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSessionCreateError {
    pub message: String,
}

impl DaemonSessionCreateError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DaemonSessionCreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DaemonSessionCreateError {}

/// The `{ success: false }` arm of a daemon response, as far as error mapping needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonErrorResponse {
    pub error: String,
    pub error_info: Option<DaemonErrorInfo>,
}

/// Wraps untyped create failures so the CLI prints one line instead of rethrowing.
pub fn deserialize_daemon_create_error(response: &DaemonErrorResponse) -> DaemonError {
    let error = deserialize_daemon_error(response);
    if response.error_info.is_some() {
        return error;
    }
    DaemonError::Message(DaemonSessionCreateError::new(error.message()).message)
}

pub fn deserialize_daemon_error(response: &DaemonErrorResponse) -> DaemonError {
    match &response.error_info {
        Some(DaemonErrorInfo::MissingSessionCwd { issue }) => {
            DaemonError::MissingSessionCwd(MissingSessionCwdError::new(issue.clone()))
        }
        Some(DaemonErrorInfo::SessionImportFileNotFound { file_path }) => DaemonError::SessionImportFileNotFound(
            SessionImportFileNotFoundError::new(file_path.clone()),
        ),
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
        Some(DaemonErrorInfo::CommandResultUncertain { .. }) | None => DaemonError::Message(response.error.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwd_issue() -> SessionCwdIssue {
        SessionCwdIssue {
            session_file: Some("C:/sessions/a.jsonl".to_string()),
            session_cwd: "C:/gone".to_string(),
            fallback_cwd: "C:/now".to_string(),
        }
    }

    #[test]
    fn serializes_known_error_codes() {
        let recovering = serialize_daemon_error(&DaemonError::SessionRecovering(DaemonSessionRecoveringError::new(
            "s1",
        )))
        .expect("known");
        assert_eq!(recovering.code(), "session_recovering");
        let value = serde_json::to_value(&recovering).unwrap();
        assert_eq!(value["code"], "session_recovering");
        assert_eq!(value["activeSessionId"], "s1");

        let cwd = serialize_daemon_error(&DaemonError::MissingSessionCwd(MissingSessionCwdError::new(cwd_issue())))
            .expect("known");
        let value = serde_json::to_value(&cwd).unwrap();
        assert_eq!(value["code"], "missing_session_cwd");
        assert_eq!(value["issue"]["sessionCwd"], "C:/gone");

        let plain = serialize_daemon_error(&DaemonError::Message("boom".to_string()));
        assert!(plain.is_none());
    }

    #[test]
    fn round_trips_already_active_with_absent_session_id() {
        let info = DaemonErrorInfo::SessionAlreadyActive {
            session_path: "C:/sessions/a.jsonl".to_string(),
            active_session_id: None,
        };
        let value = serde_json::to_value(&info).unwrap();
        assert!(value.get("activeSessionId").is_none());
        let parsed: DaemonErrorInfo = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, info);
    }

    #[test]
    fn create_error_wraps_untyped_failures_only() {
        let typed = DaemonErrorResponse {
            error: "Active session s1 is recovering; retry shortly".to_string(),
            error_info: Some(DaemonErrorInfo::SessionRecovering {
                active_session_id: "s1".to_string(),
            }),
        };
        match deserialize_daemon_create_error(&typed) {
            DaemonError::SessionRecovering(inner) => assert_eq!(inner.active_session_id, "s1"),
            other => panic!("unexpected {other:?}"),
        }

        let untyped = DaemonErrorResponse {
            error: "create failed".to_string(),
            error_info: None,
        };
        match deserialize_daemon_create_error(&untyped) {
            DaemonError::Message(message) => assert_eq!(message, "create failed"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn recovering_error_message_matches_typescript() {
        let error = DaemonSessionRecoveringError::new("abc");
        assert_eq!(
            error.message(),
            "Active session abc is recovering; retry shortly"
        );
    }
}
