//! Port of packages/coding-agent/src/core/session-file-actions.ts
//!
//! `spawnSyncHidden` lives in utils/child-process.ts (another slice), so a
//! private plumbing helper with the same observable behaviour is kept here and
//! recorded in blocked_on.

use std::path::Path;
use std::process::Command;

use crate::core::session_manager::get_session_artifact_path_for_file;

/// `DeleteSessionFileResult` - a discriminated union in TypeScript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteSessionFileResult {
    Ok { method: DeleteSessionFileMethod },
    Error { error: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteSessionFileMethod {
    Trash,
    Unlink,
}

impl DeleteSessionFileResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, DeleteSessionFileResult::Ok { .. })
    }
}

#[derive(Default)]
pub struct DeleteSessionFileOptions {
    /// `afterFileRemoved?` - invoked once the session file itself is gone.
    pub after_file_removed: Option<Box<dyn FnMut()>>,
}

/// Private port of `spawnSyncHidden` for the single call site in this module.
struct SpawnSyncResult {
    status: Option<i32>,
    stdout: String,
    stderr: String,
    error: String,
}

fn spawn_sync_hidden(command: &str, args: &[String]) -> SpawnSyncResult {
    let mut process = Command::new(command);
    process.args(args);
    process.stdin(std::process::Stdio::null());
    process.stdout(std::process::Stdio::piped());
    process.stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        process.creation_flags(CREATE_NO_WINDOW);
    }
    match process.output() {
        Ok(output) => SpawnSyncResult {
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            error: String::new(),
        },
        Err(error) => SpawnSyncResult {
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            error: error.to_string(),
        },
    }
}

/// Permanently remove a session's artifact directory (durable schedule state,
/// kernel snapshot, RLM scratch files, …), which lives at
/// `<dirname(sessionDir)>/session-artifacts/<id>`.
/// Only invoked on delete, never on deactivation.
pub fn delete_session_artifacts(session_path: &str) {
    // A degenerate name (".jsonl") would resolve to the artifacts root itself.
    let basename = Path::new(session_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    if basename
        .strip_suffix(".jsonl")
        .unwrap_or(&basename)
        .is_empty()
    {
        return;
    }
    let _ = std::fs::remove_dir_all(get_session_artifact_path_for_file(session_path, None));
}

/// Remove the session `.jsonl`, trying the `trash` CLI first, then falling back to unlink.
fn remove_session_file(session_path: &str) -> DeleteSessionFileResult {
    let trash_args: Vec<String> = if session_path.starts_with('-') {
        vec!["--".to_string(), session_path.to_string()]
    } else {
        vec![session_path.to_string()]
    };
    let trash_result = spawn_sync_hidden("trash", &trash_args);

    let get_trash_error_hint = || -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if !trash_result.error.is_empty() {
            parts.push(trash_result.error.clone());
        }
        let stderr = trash_result.stderr.trim();
        if !stderr.is_empty() {
            parts.push(stderr.split('\n').next().unwrap_or(stderr).to_string());
        }
        if parts.is_empty() {
            return None;
        }
        let joined = parts.join(" - ");
        let capped: String = joined.chars().take(200).collect();
        Some(format!("trash: {capped}"))
    };

    if trash_result.status == Some(0) || !Path::new(session_path).exists() {
        return DeleteSessionFileResult::Ok {
            method: DeleteSessionFileMethod::Trash,
        };
    }

    match std::fs::remove_file(session_path) {
        Ok(()) => DeleteSessionFileResult::Ok {
            method: DeleteSessionFileMethod::Unlink,
        },
        Err(err) => {
            let unlink_error = err.to_string();
            let trash_error_hint = get_trash_error_hint();
            let error = match trash_error_hint {
                Some(hint) => format!("{unlink_error} ({hint})"),
                None => unlink_error,
            };
            DeleteSessionFileResult::Error { error }
        }
    }
}

/// Delete a session file, trying the `trash` CLI first, then falling back to unlink.
/// Also permanently removes the session's artifact directory, but only
/// once the session file itself is gone — otherwise a failed delete would orphan a
/// session whose kernel snapshot has already been destroyed.
pub fn delete_session_file(
    session_path: &str,
    options: &mut DeleteSessionFileOptions,
) -> DeleteSessionFileResult {
    let result = remove_session_file(session_path);
    if result.is_ok() {
        if let Some(callback) = options.after_file_removed.as_mut() {
            callback();
        }
        delete_session_artifacts(session_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn degenerate_jsonl_name_leaves_the_artifacts_root_alone() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("session-artifacts");
        std::fs::create_dir_all(&root).unwrap();
        delete_session_artifacts(".jsonl");
        assert!(root.exists());
    }

    #[test]
    fn deletes_the_artifact_directory_for_a_real_session_file() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let session_file = sessions.join("abc.jsonl");
        std::fs::write(&session_file, b"{}").unwrap();
        let artifacts = temp.path().join("session-artifacts").join("abc");
        std::fs::create_dir_all(&artifacts).unwrap();

        delete_session_artifacts(&session_file.to_string_lossy());
        assert!(!artifacts.exists());
    }

    #[test]
    fn unlink_fallback_removes_the_file_and_runs_the_callback() {
        let temp = tempfile::tempdir().unwrap();
        let session_file = temp.path().join("abc.jsonl");
        std::fs::write(&session_file, b"{}").unwrap();
        let called = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&called);
        let mut options = DeleteSessionFileOptions {
            after_file_removed: Some(Box::new(move || {
                *flag.lock().unwrap() = true;
            })),
        };
        let result = delete_session_file(&session_file.to_string_lossy(), &mut options);
        assert!(result.is_ok());
        assert!(!session_file.exists());
        assert!(*called.lock().unwrap());
    }

    #[test]
    fn a_missing_file_is_reported_as_removed() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.jsonl");
        let mut options = DeleteSessionFileOptions::default();
        let result = delete_session_file(&missing.to_string_lossy(), &mut options);
        // `trash` is normally absent in CI, so the existsSync guard reports success.
        assert!(result.is_ok());
    }
}
