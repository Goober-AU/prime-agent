//! Port of packages/coding-agent/src/modes/interactive/resume-hint.ts

use std::path::Path;

use crate::config::app_name;

/// `ResumeHintStats` (`Pick<SessionStats, "sessionId" | "sessionFile" | "userMessages">`)
#[derive(Debug, Clone, Default)]
pub struct ResumeHintStats {
    pub session_id: String,
    pub session_file: Option<String>,
    pub user_messages: f64,
}

/// Omit ephemeral and unflushed empty sessions because neither can be resumed.
pub fn format_resume_hint(stats: Option<&ResumeHintStats>) -> Option<String> {
    let stats = stats?;
    let session_file = stats.session_file.as_deref()?;
    if stats.user_messages == 0.0 {
        return None;
    }
    // Persistence is lazy: nothing is written until the first assistant message
    // arrives, so exiting before then leaves no file to resume from.
    if !Path::new(session_file).exists() {
        return None;
    }
    Some(format!(
        "\u{1b}[2mResume this session with: {} --resume {}\u{1b}[22m",
        app_name(),
        stats.session_id
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_stats_and_missing_file_are_omitted() {
        assert_eq!(format_resume_hint(None), None);
        assert_eq!(
            format_resume_hint(Some(&ResumeHintStats {
                session_id: "s".into(),
                session_file: None,
                user_messages: 3.0
            })),
            None
        );
        assert_eq!(
            format_resume_hint(Some(&ResumeHintStats {
                session_id: "s".into(),
                session_file: Some("does-not-exist.jsonl".into()),
                user_messages: 3.0
            })),
            None
        );
    }

    #[test]
    fn empty_sessions_are_omitted_even_when_the_file_exists() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "{}").expect("write");
        assert_eq!(
            format_resume_hint(Some(&ResumeHintStats {
                session_id: "abc".into(),
                session_file: Some(path.to_string_lossy().to_string()),
                user_messages: 0.0
            })),
            None
        );
    }

    #[test]
    fn resumable_session_gets_the_dim_hint() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "{}").expect("write");
        let hint = format_resume_hint(Some(&ResumeHintStats {
            session_id: "abc".into(),
            session_file: Some(path.to_string_lossy().to_string()),
            user_messages: 2.0,
        }))
        .expect("hint");
        assert!(hint.contains("Resume this session with:"));
        assert!(hint.contains("--resume abc"));
    }
}
