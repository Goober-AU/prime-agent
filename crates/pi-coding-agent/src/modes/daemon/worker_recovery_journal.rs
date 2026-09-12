//! Port of packages/coding-agent/src/modes/daemon/worker-recovery-journal.ts

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerRecoveryRecord {
    pub version: u32,
    #[serde(rename = "activeSessionId")]
    pub active_session_id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "sessionFile", skip_serializing_if = "Option::is_none", default)]
    pub session_file: Option<String>,
    pub busy: bool,
    pub operation: String,
    #[serde(rename = "recordedAt")]
    pub recorded_at: String,
}

/// Input for `record`: the version and timestamp are written by the journal.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerRecoveryRecordInput {
    pub active_session_id: String,
    pub session_id: String,
    pub session_file: Option<String>,
    pub busy: bool,
    pub operation: String,
}

fn parse_records(path: &str) -> HashMap<String, WorkerRecoveryRecord> {
    let mut latest: HashMap<String, WorkerRecoveryRecord> = HashMap::new();
    let Ok(contents) = std::fs::read_to_string(path) else {
        return latest;
    };
    for line in contents.split('\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<WorkerRecoveryRecord>(line) else {
            continue;
        };
        if record.version == 1 && !record.active_session_id.is_empty() && !record.session_id.is_empty() {
            latest.insert(record.active_session_id.clone(), record);
        }
    }
    latest
}

/// Append-only worker recovery journal: the latest record per active session.
pub struct WorkerRecoveryJournal {
    path: String,
    latest: HashMap<String, WorkerRecoveryRecord>,
}

impl WorkerRecoveryJournal {
    pub fn new(path: &str) -> Self {
        if let Some(parent) = Path::new(path).parent() {
            let _ = create_private_dir(&parent.to_string_lossy());
        }
        let latest = parse_records(path);
        Self {
            path: path.to_string(),
            latest,
        }
    }

    pub fn record(&mut self, input: WorkerRecoveryRecordInput) {
        let previous = self.latest.get(&input.active_session_id);
        if previous.is_some_and(|previous| {
            previous.busy == input.busy
                && previous.operation == input.operation
                && previous.session_file == input.session_file
        }) {
            return;
        }
        let record = WorkerRecoveryRecord {
            version: 1,
            active_session_id: input.active_session_id.clone(),
            session_id: input.session_id,
            session_file: input.session_file,
            busy: input.busy,
            operation: input.operation,
            recorded_at: now_iso(),
        };
        self.append(&record);
        self.latest.insert(record.active_session_id.clone(), record);
        if self.latest.values().all(|entry| !entry.busy) {
            self.compact();
        }
    }

    pub fn get_latest(&self) -> Vec<WorkerRecoveryRecord> {
        self.latest.values().cloned().collect()
    }

    pub fn read_latest(path: &str) -> Vec<WorkerRecoveryRecord> {
        parse_records(path).values().cloned().collect()
    }

    fn append(&self, record: &WorkerRecoveryRecord) {
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };
        let line = serde_json::to_string(record).unwrap_or_default();
        let _ = file.write_all(format!("{line}\n").as_bytes());
        let _ = file.sync_all();
        set_private_mode(&self.path);
    }

    fn compact(&self) {
        let temp_path = format!("{}.{}.tmp", self.path, std::process::id());
        let payload: String = self
            .latest
            .values()
            .map(|record| serde_json::to_string(record).unwrap_or_default())
            .collect::<Vec<String>>()
            .join("\n");
        let payload = format!("{payload}\n");
        if std::fs::write(&temp_path, payload).is_err() {
            return;
        }
        set_private_mode(&temp_path);
        let _ = std::fs::rename(&temp_path, &self.path);
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn create_private_dir(path: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn set_private_mode(path: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// True when a parsed JSON line is a usable worker recovery record.
pub fn is_worker_recovery_record(value: &Value) -> bool {
    let Some(candidate) = value.as_object() else {
        return false;
    };
    candidate.get("version").and_then(Value::as_u64) == Some(1)
        && candidate.get("activeSessionId").and_then(Value::as_str).is_some()
        && candidate.get("sessionId").and_then(Value::as_str).is_some()
        && candidate.get("busy").and_then(Value::as_bool).is_some()
        && candidate.get("operation").and_then(Value::as_str).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("worker-recovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join(name).to_string_lossy().to_string()
    }

    #[test]
    fn records_are_appended_and_deduplicated() {
        let path = temp_path("journal.jsonl");
        let mut journal = WorkerRecoveryJournal::new(&path);
        let input = WorkerRecoveryRecordInput {
            active_session_id: "active-1".to_string(),
            session_id: "session-1".to_string(),
            session_file: Some("/tmp/s.jsonl".to_string()),
            busy: true,
            operation: "prompt".to_string(),
        };
        journal.record(input.clone());
        journal.record(input.clone());
        let latest = journal.get_latest();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].operation, "prompt");

        let lines = std::fs::read_to_string(&path).expect("journal contents");
        assert_eq!(lines.lines().count(), 1);

        // A non-busy record compacts the journal down to the latest entries.
        journal.record(WorkerRecoveryRecordInput {
            busy: false,
            operation: "idle".to_string(),
            ..input
        });
        let reloaded = WorkerRecoveryJournal::read_latest(&path);
        assert_eq!(reloaded.len(), 1);
        assert!(!reloaded[0].busy);
        assert_eq!(reloaded[0].operation, "idle");
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let path = temp_path("journal.jsonl");
        std::fs::write(
            &path,
            "not json\n{\"version\":2,\"activeSessionId\":\"a\",\"sessionId\":\"b\",\"busy\":true,\"operation\":\"x\",\"recordedAt\":\"now\"}\n",
        )
        .expect("seed");
        assert!(WorkerRecoveryJournal::read_latest(&path).is_empty());
    }

    #[test]
    fn record_shape_validation() {
        assert!(is_worker_recovery_record(&serde_json::json!({
            "version": 1,
            "activeSessionId": "a",
            "sessionId": "b",
            "busy": true,
            "operation": "prompt"
        })));
        assert!(!is_worker_recovery_record(&serde_json::json!({
            "version": 1,
            "activeSessionId": "a"
        })));
    }
}
