//! Port of packages/coding-agent/src/core/logging.ts

use std::io::Write;
use std::path::Path;

use pi_ai::log::{set_log_sink, stringify_log_entry};
use serde_json::{Map, Value};

const AGENT_LOG_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// `getAgentLogPath()` from config.ts: `<agentDir>/logs/agent.jsonl`.
///
/// config.ts belongs to another slice; this minimal local definition keeps the
/// dependency explicit until `pi_coding_agent::config::get_agent_log_path`
/// exists. TODO(slice ca-root): re-export from crate::config.
fn get_agent_log_path() -> String {
    let agent_dir = std::env::var("PI_CODING_AGENT_DIR")
        .or_else(|_| std::env::var("PRIME_AGENT_CODING_AGENT_DIR"))
        .unwrap_or_else(|_| {
            let home = dirs::home_dir().unwrap_or_default();
            format!(
                "{}{}.prime/agent",
                home.to_string_lossy(),
                std::path::MAIN_SEPARATOR
            )
        });
    let sep = std::path::MAIN_SEPARATOR;
    format!("{agent_dir}{sep}logs{sep}agent.jsonl")
}

/// `appendRotatingLog()` from config.ts (single-generation rotation).
///
/// Best-effort: diagnostics must never throw into the caller. TODO(slice ca-root):
/// re-export from crate::config.
fn append_rotating_log(log_path: &str, message: &str, max_bytes: u64) {
    let path = Path::new(log_path);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Ok(metadata) = std::fs::metadata(path) {
        if metadata.len() > max_bytes {
            // Drop any prior .old first: renameSync fails on Windows if it exists.
            let old = format!("{log_path}.old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(path, &old);
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(format!("{message}\n").as_bytes());
    }
}

fn context() -> &'static std::sync::Mutex<Map<String, Value>> {
    static CONTEXT: std::sync::OnceLock<std::sync::Mutex<Map<String, Value>>> =
        std::sync::OnceLock::new();
    CONTEXT.get_or_init(|| std::sync::Mutex::new(Map::new()))
}

/// Merge late-bound fields (e.g. mode, sessionId) into every subsequent log entry.
pub fn set_log_context(fields: Map<String, Value>) {
    if let Ok(mut guard) = context().lock() {
        for (key, value) in fields {
            guard.insert(key, value);
        }
    }
}

fn current_context() -> Map<String, Value> {
    match context().lock() {
        Ok(guard) => guard.clone(),
        Err(_) => Map::new(),
    }
}

/// Route all structured logging (coding-agent and pi-ai) to the shared JSONL
/// log at ~/.prime/agent/logs/agent.jsonl. One master file, filterable by the
/// pid/context fields; writes are best-effort and size-bounded.
pub fn install_file_log_sink(fields: Option<Map<String, Value>>) {
    let mut next = Map::new();
    next.insert(
        "pid".to_string(),
        Value::Number(serde_json::Number::from(std::process::id())),
    );
    if let Some(fields) = fields {
        for (key, value) in fields {
            next.insert(key, value);
        }
    }
    if let Ok(mut guard) = context().lock() {
        *guard = next;
    }

    set_log_sink(Some(Box::new(|entry: &Value| {
        // `{ ...entry, ...context }`: context fields win over entry fields.
        let mut merged = match entry {
            Value::Object(map) => map.clone(),
            other => {
                let mut map = Map::new();
                map.insert("value".to_string(), other.clone());
                map
            }
        };
        for (key, value) in current_context() {
            merged.insert(key, value);
        }
        let line = stringify_log_entry(&Value::Object(merged));
        append_rotating_log(&get_agent_log_path(), &line, AGENT_LOG_MAX_BYTES);
    })));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotating_log_keeps_a_single_old_generation() {
        let dir = std::env::temp_dir().join(format!("prime-log-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.jsonl").to_string_lossy().to_string();
        append_rotating_log(&path, "first", 4);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\n");
        append_rotating_log(&path, "second", 4);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
        assert_eq!(
            std::fs::read_to_string(format!("{path}.old")).unwrap(),
            "first\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
