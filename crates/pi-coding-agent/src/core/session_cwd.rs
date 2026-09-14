//! Port of packages/coding-agent/src/core/session-cwd.ts

/// `interface SessionCwdSource { getCwd(); getSessionFile(); }`
pub trait SessionCwdSource {
    fn get_cwd(&self) -> String;
    fn get_session_file(&self) -> Option<String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCwdIssue {
    /// `sessionFile?` - absent when the manager has no session file.
    pub session_file: Option<String>,
    pub session_cwd: String,
    pub fallback_cwd: String,
}

pub fn get_missing_session_cwd_issue(
    session_manager: &dyn SessionCwdSource,
    fallback_cwd: &str,
) -> Option<SessionCwdIssue> {
    let session_file = session_manager.get_session_file()?;

    let session_cwd = session_manager.get_cwd();
    if session_cwd.is_empty() || std::path::Path::new(&session_cwd).exists() {
        return None;
    }

    Some(SessionCwdIssue {
        session_file: Some(session_file),
        session_cwd,
        fallback_cwd: fallback_cwd.to_string(),
    })
}

pub fn format_missing_session_cwd_error(issue: &SessionCwdIssue) -> String {
    let session_file = match &issue.session_file {
        Some(session_file) => format!("\nSession file: {session_file}"),
        None => String::new(),
    };
    format!(
        "Stored session working directory does not exist: {}{}\nCurrent working directory: {}",
        issue.session_cwd, session_file, issue.fallback_cwd
    )
}

pub fn format_missing_session_cwd_prompt(issue: &SessionCwdIssue) -> String {
    format!(
        "cwd from session file does not exist\n{}\n\ncontinue in current cwd\n{}",
        issue.session_cwd, issue.fallback_cwd
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingSessionCwdError {
    pub issue: SessionCwdIssue,
}

impl MissingSessionCwdError {
    pub fn new(issue: SessionCwdIssue) -> Self {
        Self { issue }
    }
}

impl std::fmt::Display for MissingSessionCwdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format_missing_session_cwd_error(&self.issue))
    }
}

impl std::error::Error for MissingSessionCwdError {}

pub fn assert_session_cwd_exists(
    session_manager: &dyn SessionCwdSource,
    fallback_cwd: &str,
) -> Result<(), MissingSessionCwdError> {
    match get_missing_session_cwd_issue(session_manager, fallback_cwd) {
        Some(issue) => Err(MissingSessionCwdError::new(issue)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Source {
        cwd: String,
        session_file: Option<String>,
    }

    impl SessionCwdSource for Source {
        fn get_cwd(&self) -> String {
            self.cwd.clone()
        }
        fn get_session_file(&self) -> Option<String> {
            self.session_file.clone()
        }
    }

    #[test]
    fn no_issue_without_a_session_file() {
        let source = Source {
            cwd: "/definitely/missing/cwd".to_string(),
            session_file: None,
        };
        assert!(get_missing_session_cwd_issue(&source, "/tmp").is_none());
    }

    #[test]
    fn reports_a_missing_cwd_and_formats_both_messages() {
        let source = Source {
            cwd: "/definitely/missing/cwd".to_string(),
            session_file: Some("/tmp/s.jsonl".to_string()),
        };
        let issue = get_missing_session_cwd_issue(&source, "/tmp").unwrap();
        assert_eq!(
            format_missing_session_cwd_error(&issue),
            "Stored session working directory does not exist: /definitely/missing/cwd\nSession file: /tmp/s.jsonl\nCurrent working directory: /tmp"
        );
        assert_eq!(
            format_missing_session_cwd_prompt(&issue),
            "cwd from session file does not exist\n/definitely/missing/cwd\n\ncontinue in current cwd\n/tmp"
        );
        assert!(assert_session_cwd_exists(&source, "/tmp").is_err());
    }

    #[test]
    fn an_existing_cwd_is_not_an_issue() {
        let cwd = std::env::temp_dir();
        let source = Source {
            cwd: cwd.to_string_lossy().to_string(),
            session_file: Some("/tmp/s.jsonl".to_string()),
        };
        assert!(get_missing_session_cwd_issue(&source, "/tmp").is_none());
        assert!(assert_session_cwd_exists(&source, "/tmp").is_ok());
    }
}
