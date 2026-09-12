//! Port of packages/coding-agent/src/modes/daemon/rlm-subagent-display.ts

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::utils::atomic_file::{write_file_atomic_sync, WriteFileAtomicOptions};

const RLM_SUBAGENT_DISPLAY_FILE: &str = "rlm-subagent.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RlmSubagentModel {
    pub provider: String,
    #[serde(rename = "modelId")]
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RlmSubagentDisplayEntry {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(rename = "childId")]
    pub child_id: String,
    #[serde(rename = "sessionName")]
    pub session_name: String,
    #[serde(rename = "sessionDir")]
    pub session_dir: String,
    #[serde(rename = "sessionFile")]
    pub session_file: String,
    #[serde(rename = "rlmMaxDepth", skip_serializing_if = "Option::is_none", default)]
    pub rlm_max_depth: Option<i64>,
    #[serde(rename = "rlmParentNodeId", skip_serializing_if = "Option::is_none", default)]
    pub rlm_parent_node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prompt: Option<String>,
    #[serde(rename = "spawnCode", skip_serializing_if = "Option::is_none", default)]
    pub spawn_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model: Option<RlmSubagentModel>,
    pub status: String,
    #[serde(rename = "createdAt")]
    pub created_at: f64,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

impl RlmSubagentDisplayEntry {
    pub fn new(child_id: &str, session_name: &str, session_dir: &str, session_file: &str, status: &str) -> Self {
        Self {
            type_: "rlm_subagent".to_string(),
            child_id: child_id.to_string(),
            session_name: session_name.to_string(),
            session_dir: session_dir.to_string(),
            session_file: session_file.to_string(),
            rlm_max_depth: None,
            rlm_parent_node_id: None,
            prompt: None,
            spawn_code: None,
            model: None,
            status: status.to_string(),
            created_at: 0.0,
            updated_at: String::new(),
        }
    }
}

pub fn rlm_subagent_display_path(session_dir: &str) -> String {
    Path::new(session_dir)
        .join(RLM_SUBAGENT_DISPLAY_FILE)
        .to_string_lossy()
        .to_string()
}

pub fn is_rlm_subagent_display_entry(value: &serde_json::Value) -> bool {
    let Some(entry) = value.as_object() else {
        return false;
    };
    if entry.get("type").and_then(serde_json::Value::as_str) != Some("rlm_subagent") {
        return false;
    }
    if entry.get("childId").and_then(serde_json::Value::as_str).is_none() {
        return false;
    }
    if entry
        .get("sessionName")
        .and_then(serde_json::Value::as_str)
        .is_none()
    {
        return false;
    }
    if entry.get("sessionDir").and_then(serde_json::Value::as_str).is_none() {
        return false;
    }
    if entry
        .get("sessionFile")
        .and_then(serde_json::Value::as_str)
        .is_none()
    {
        return false;
    }
    match entry.get("status").and_then(serde_json::Value::as_str) {
        Some("running") | Some("completed") | Some("deleted") => {}
        _ => return false,
    }
    if let Some(depth) = entry.get("rlmMaxDepth") {
        let Some(depth) = depth.as_i64() else {
            return false;
        };
        if depth < 0 {
            return false;
        }
    }
    entry.get("createdAt").and_then(serde_json::Value::as_f64).is_some()
}

fn read_rlm_subagent_display_entry_sync(session_dir: &str) -> Result<Option<RlmSubagentDisplayEntry>, std::io::Error> {
    let contents = match std::fs::read_to_string(rlm_subagent_display_path(session_dir)) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return Ok(None);
    };
    if !is_rlm_subagent_display_entry(&parsed) {
        return Ok(None);
    }
    Ok(serde_json::from_value(parsed).ok())
}

/// Returns false when a deletion tombstone already exists for the child.
pub fn write_rlm_subagent_display_entry(entry: &RlmSubagentDisplayEntry) -> Result<bool, std::io::Error> {
    let path = rlm_subagent_display_path(&entry.session_dir);
    if entry.status != "deleted" {
        let existing = read_rlm_subagent_display_entry_sync(&entry.session_dir)?;
        if existing.map(|existing| existing.status == "deleted").unwrap_or(false) {
            return Ok(false);
        }
    }
    std::fs::create_dir_all(&entry.session_dir)?;
    let contents = format!(
        "{}\n",
        serde_json::to_string(entry).unwrap_or_else(|_| "null".to_string())
    );
    write_file_atomic_sync(
        &path,
        &contents,
        WriteFileAtomicOptions {
            mode: Some(0o600),
            fsync: true,
            fsync_dir: false,
            before_rename: None,
        },
    )?;
    Ok(true)
}

pub async fn read_rlm_subagent_display_entry(
    session_dir: &str,
    on_read_error: Option<&(dyn Fn() + Send + Sync)>,
) -> Option<RlmSubagentDisplayEntry> {
    let contents = match tokio::fs::read_to_string(rlm_subagent_display_path(session_dir)).await {
        Ok(contents) => contents,
        Err(_) => {
            if let Some(on_read_error) = on_read_error {
                on_read_error();
            }
            return None;
        }
    };
    let parsed = serde_json::from_str::<serde_json::Value>(&contents).ok()?;
    if !is_rlm_subagent_display_entry(&parsed) {
        return None;
    }
    serde_json::from_value(parsed).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(status: &str, dir: &str) -> RlmSubagentDisplayEntry {
        RlmSubagentDisplayEntry {
            created_at: 1.0,
            ..RlmSubagentDisplayEntry::new("child-1", "child", dir, "/tmp/child.jsonl", status)
        }
    }

    #[test]
    fn the_display_path_is_the_child_session_dir() {
        assert!(rlm_subagent_display_path("/tmp/child").ends_with("rlm-subagent.json"));
    }

    #[test]
    fn the_entry_guard_matches_the_typescript() {
        let mut value = serde_json::json!({
            "type": "rlm_subagent",
            "childId": "c",
            "sessionName": "n",
            "sessionDir": "/tmp",
            "sessionFile": "/tmp/s.jsonl",
            "status": "running",
            "createdAt": 1
        });
        assert!(is_rlm_subagent_display_entry(&value));
        value["rlmMaxDepth"] = serde_json::json!(-1);
        assert!(!is_rlm_subagent_display_entry(&value));
        value["rlmMaxDepth"] = serde_json::json!(2);
        assert!(is_rlm_subagent_display_entry(&value));
        value["status"] = serde_json::json!("other");
        assert!(!is_rlm_subagent_display_entry(&value));
    }

    #[tokio::test]
    async fn writes_are_atomic_and_deletion_tombstones_win() {
        let root = std::env::temp_dir().join(format!("rlm-display-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.to_string_lossy().to_string();
        assert!(write_rlm_subagent_display_entry(&entry("running", &dir)).expect("writes"));
        let stored = read_rlm_subagent_display_entry(&dir, None)
            .await
            .expect("stored");
        assert_eq!(stored.status, "running");
        assert_eq!(stored.child_id, "child-1");
        assert!(write_rlm_subagent_display_entry(&entry("deleted", &dir)).expect("writes"));
        assert!(!write_rlm_subagent_display_entry(&entry("completed", &dir)).expect("refuses"));
        let stored = read_rlm_subagent_display_entry(&dir, None)
            .await
            .expect("stored");
        assert_eq!(stored.status, "deleted");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn unreadable_files_call_the_read_error_callback() {
        let root = std::env::temp_dir().join(format!("rlm-display-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let called = std::sync::atomic::AtomicUsize::new(0);
        let result = read_rlm_subagent_display_entry(&root.to_string_lossy(), Some(&|| {
            called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }))
        .await;
        assert!(result.is_none());
        assert_eq!(called.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(read_rlm_subagent_display_entry_sync(&root.to_string_lossy())
            .expect("tolerant")
            .is_none());
    }
}
