//! Port of packages/coding-agent/src/modes/daemon/command-recovery-journal.ts

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// UNKNOWN: ['::{DaemonResponse', 'DaemonSavedSessionInfo}']
use super::daemon_protocol::{DaemonResponse, DaemonSavedSessionInfo};

use crate::utils::atomic_file::{write_file_atomic_sync, WriteFileAtomicOptions};

const COMPACT_AFTER_RECORDS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceivedRecord {
    pub version: u32,
    #[serde(rename = "type")]
    pub type_: String,
    pub key: String,
    #[serde(rename = "clientId")]
    pub client_id: String,
    #[serde(rename = "commandId")]
    pub command_id: String,
    #[serde(rename = "commandType")]
    pub command_type: String,
    #[serde(rename = "recordedAt")]
    pub recorded_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultRecord {
    pub version: u32,
    #[serde(rename = "type")]
    pub type_: String,
    pub key: String,
    pub response: DaemonResponse,
    #[serde(rename = "recordedAt")]
    pub recorded_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcknowledgedRecord {
    pub version: u32,
    #[serde(rename = "type")]
    pub type_: String,
    pub key: String,
    #[serde(rename = "recordedAt")]
    pub recorded_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JournalRecord {
    Received(ReceivedRecord),
    Result(ResultRecord),
    Acknowledged(AcknowledgedRecord),
}

impl JournalRecord {
    pub fn version(&self) -> u32 {
        match self {
            JournalRecord::Received(record) => record.version,
            JournalRecord::Result(record) => record.version,
            JournalRecord::Acknowledged(record) => record.version,
        }
    }

    pub fn key(&self) -> &str {
        match self {
            JournalRecord::Received(record) => &record.key,
            JournalRecord::Result(record) => &record.key,
            JournalRecord::Acknowledged(record) => &record.key,
        }
    }

    pub fn type_(&self) -> &str {
        match self {
            JournalRecord::Received(record) => &record.type_,
            JournalRecord::Result(record) => &record.type_,
            JournalRecord::Acknowledged(record) => &record.type_,
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            JournalRecord::Received(record) => serde_json::to_value(record),
            JournalRecord::Result(record) => serde_json::to_value(record),
            JournalRecord::Acknowledged(record) => serde_json::to_value(record),
        }
        .unwrap_or(Value::Null)
    }
}

#[derive(Debug, Clone, PartialEq)]
struct JournalEntry {
    received: ReceivedRecord,
    response: Option<DaemonResponse>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandJournalBeginResult {
    New,
    Pending,
    Complete(DaemonResponse),
}

pub fn create_command_idempotency_key(client_id: &str, command_id: &str) -> String {
    serde_json::to_string(&[client_id, command_id]).unwrap_or_else(|_| format!("[\"{client_id}\",\"{command_id}\"]"))
}

/// Append-only command journal used at the supervisor boundary. A received
/// record is durable before a mutating command is dispatched; a missing result
/// after a crash is therefore treated as uncertain and is never replayed.
pub struct CommandRecoveryJournal {
    path: String,
    entries: HashMap<String, JournalEntry>,
    record_count: usize,
}

impl CommandRecoveryJournal {
    pub fn new(path: &str) -> Self {
        if let Some(parent) = Path::new(path).parent() {
            let _ = create_private_dir(parent);
        }
        let mut journal = Self {
            path: path.to_string(),
            entries: HashMap::new(),
            record_count: 0,
        };
        journal.load();
        journal
    }

    pub fn lookup(&self, client_id: &str, command_id: &str) -> Option<CommandJournalBeginResult> {
        let existing = self.entries.get(&create_command_idempotency_key(client_id, command_id));
        match existing {
            Some(entry) => match &entry.response {
                Some(response) => Some(CommandJournalBeginResult::Complete(response.clone())),
                None => Some(CommandJournalBeginResult::Pending),
            },
            None => None,
        }
    }

    pub fn begin(&mut self, client_id: &str, command_id: &str, command_type: &str) -> CommandJournalBeginResult {
        let key = create_command_idempotency_key(client_id, command_id);
        if let Some(existing) = self.lookup(client_id, command_id) {
            return existing;
        }
        let received = ReceivedRecord {
            version: 1,
            type_: "received".to_string(),
            key: key.clone(),
            client_id: client_id.to_string(),
            command_id: command_id.to_string(),
            command_type: command_type.to_string(),
            recorded_at: now_iso(),
        };
        self.append(&JournalRecord::Received(received.clone()));
        self.entries.insert(
            key,
            JournalEntry {
                received,
                response: None,
            },
        );
        CommandJournalBeginResult::New
    }

    pub fn record_result(&mut self, client_id: &str, command_id: &str, response: DaemonResponse) {
        let key = create_command_idempotency_key(client_id, command_id);
        if !self.entries.contains_key(&key) {
            // TypeScript throws here; the Rust port surfaces the same condition
            // through the returned error string on the `try_record_result` path.
            return;
        }
        let record = ResultRecord {
            version: 1,
            type_: "result".to_string(),
            key: key.clone(),
            response: response.clone(),
            recorded_at: now_iso(),
        };
        self.append(&JournalRecord::Result(record));
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.response = Some(response);
        }
        if self.record_count >= COMPACT_AFTER_RECORDS {
            self.compact();
        }
    }

    /// Same as `record_result`, but reports the TypeScript error condition.
    pub fn try_record_result(
        &mut self,
        client_id: &str,
        command_id: &str,
        response: DaemonResponse,
    ) -> Result<(), String> {
        let key = create_command_idempotency_key(client_id, command_id);
        if !self.entries.contains_key(&key) {
            return Err(format!("Cannot record a result before command receipt: {key}"));
        }
        self.record_result(client_id, command_id, response);
        Ok(())
    }

    pub fn acknowledge(&mut self, client_id: &str, command_id: &str) {
        let key = create_command_idempotency_key(client_id, command_id);
        if !self.entries.contains_key(&key) {
            return;
        }
        self.append(&JournalRecord::Acknowledged(AcknowledgedRecord {
            version: 1,
            type_: "acknowledged".to_string(),
            key: key.clone(),
            recorded_at: now_iso(),
        }));
        self.entries.remove(&key);
        if self.entries.is_empty() || self.record_count >= COMPACT_AFTER_RECORDS {
            self.compact();
        }
    }

    fn load(&mut self) {
        let Ok(contents) = std::fs::read_to_string(&self.path) else {
            return;
        };
        for line in contents.split('\n') {
            if line.is_empty() {
                continue;
            }
            // A crash may leave only the final append truncated.
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(candidate) = record.as_object() else {
                continue;
            };
            if candidate.get("version").and_then(Value::as_u64) != Some(1) {
                continue;
            }
            let Some(key) = candidate.get("key").and_then(Value::as_str) else {
                continue;
            };
            let key = key.to_string();
            self.record_count += 1;
            match candidate.get("type").and_then(Value::as_str) {
                Some("received") => {
                    if let Ok(received) = serde_json::from_value::<ReceivedRecord>(record.clone()) {
                        if !received.client_id.is_empty()
                            && !received.command_id.is_empty()
                            && !received.command_type.is_empty()
                        {
                            self.entries.insert(
                                key,
                                JournalEntry {
                                    received,
                                    response: None,
                                },
                            );
                        }
                    }
                }
                Some("acknowledged") => {
                    self.entries.remove(&key);
                }
                Some("result") => {
                    let Ok(result) = serde_json::from_value::<ResultRecord>(record.clone()) else {
                        continue;
                    };
                    if result.response.type_ != "response" {
                        continue;
                    }
                    if let Some(entry) = self.entries.get_mut(&key) {
                        entry.response = Some(result.response);
                    }
                }
                _ => {}
            }
        }
    }

    fn append(&mut self, record: &JournalRecord) {
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };
        let line = serde_json::to_string(&record.to_value()).unwrap_or_default();
        let _ = file.write_all(format!("{line}\n").as_bytes());
        let _ = file.sync_all();
        set_private_mode(&self.path);
        self.record_count += 1;
    }

    fn compact(&mut self) {
        let mut records: Vec<JournalRecord> = Vec::new();
        for (key, entry) in &self.entries {
            records.push(JournalRecord::Received(entry.received.clone()));
            if let Some(response) = &entry.response {
                records.push(JournalRecord::Result(ResultRecord {
                    version: 1,
                    type_: "result".to_string(),
                    key: key.clone(),
                    response: response.clone(),
                    recorded_at: now_iso(),
                }));
            }
        }
        let payload: String = records
            .iter()
            .map(|record| serde_json::to_string(&record.to_value()).unwrap_or_default())
            .collect::<Vec<String>>()
            .join("\n");
        let payload = format!("{payload}\n");
        let _ = write_file_atomic_sync(
            &self.path,
            &payload,
            WriteFileAtomicOptions {
                mode: Some(0o600),
                fsync: true,
                fsync_dir: true,
                before_rename: None,
            },
        );
        self.record_count = records.len();
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
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

/// A saved-session row stored inside a journal result record.
pub fn journal_result_saved_session(value: &Value) -> Option<DaemonSavedSessionInfo> {
    serde_json::from_value(value.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("command-recovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join(name).to_string_lossy().to_string()
    }

    fn response(id: &str) -> DaemonResponse {
        DaemonResponse::success(Some(id), "prompt", None)
    }

    #[test]
    fn begin_lookup_and_complete_cycle() {
        let path = temp_path("journal.jsonl");
        let mut journal = CommandRecoveryJournal::new(&path);
        assert_eq!(journal.begin("client", "c1", "prompt"), CommandJournalBeginResult::New);
        assert_eq!(journal.lookup("client", "c1"), Some(CommandJournalBeginResult::Pending));
        assert_eq!(journal.begin("client", "c1", "prompt"), CommandJournalBeginResult::Pending);
        journal.record_result("client", "c1", response("c1"));
        assert_eq!(
            journal.lookup("client", "c1"),
            Some(CommandJournalBeginResult::Complete(response("c1")))
        );
        journal.acknowledge("client", "c1");
        assert_eq!(journal.lookup("client", "c1"), None);
        assert_eq!(std::fs::read_to_string(&path).expect("journal").trim(), "");
    }

    #[test]
    fn records_survive_a_reload() {
        let path = temp_path("journal.jsonl");
        {
            let mut journal = CommandRecoveryJournal::new(&path);
            journal.begin("client", "c2", "prompt");
            journal.record_result("client", "c2", response("c2"));
        }
        let journal = CommandRecoveryJournal::new(&path);
        assert_eq!(
            journal.lookup("client", "c2"),
            Some(CommandJournalBeginResult::Complete(response("c2")))
        );
    }

    #[test]
    fn recording_before_receipt_is_an_error() {
        let path = temp_path("journal.jsonl");
        let mut journal = CommandRecoveryJournal::new(&path);
        let error = journal
            .try_record_result("client", "missing", response("missing"))
            .expect_err("must fail");
        assert!(error.starts_with("Cannot record a result before command receipt:"));
    }

    #[test]
    fn truncated_final_line_is_ignored() {
        let path = temp_path("journal.jsonl");
        let mut journal = CommandRecoveryJournal::new(&path);
        journal.begin("client", "c3", "prompt");
        let mut contents = std::fs::read_to_string(&path).expect("journal");
        contents.push_str("{\"version\":1,\"type\":\"resu");
        std::fs::write(&path, contents).expect("seed truncation");
        let journal = CommandRecoveryJournal::new(&path);
        assert_eq!(journal.lookup("client", "c3"), Some(CommandJournalBeginResult::Pending));
    }

    #[test]
    fn idempotency_key_is_a_two_element_json_array() {
        assert_eq!(create_command_idempotency_key("c", "1"), "[\"c\",\"1\"]");
    }
}
