//! Port of packages/coding-agent/src/core/session-import-errors.ts

/// `SessionImportFileNotFoundError extends Error`.
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
}

impl std::fmt::Display for SessionImportFileNotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "File not found: {}", self.file_path)
    }
}

impl std::error::Error for SessionImportFileNotFoundError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_matches_typescript() {
        let error = SessionImportFileNotFoundError::new("/tmp/a.jsonl");
        assert_eq!(error.to_string(), "File not found: /tmp/a.jsonl");
        assert_eq!(error.file_path, "/tmp/a.jsonl");
    }
}
