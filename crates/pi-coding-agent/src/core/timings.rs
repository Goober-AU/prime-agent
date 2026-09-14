//! Port of packages/coding-agent/src/core/timings.ts
//!
//! Central timing instrumentation for startup profiling.
//! Enable with PI_TIMING=1 environment variable.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use pi_ai::log::get_logger;
use serde_json::json;

fn enabled() -> bool {
    std::env::var("PI_TIMING").map(|value| value == "1").unwrap_or(false)
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

#[derive(Debug, Clone)]
struct TimingEntry {
    label: String,
    ms: f64,
}

struct TimingsState {
    timings: Vec<TimingEntry>,
    last_time: f64,
}

fn state() -> &'static Mutex<TimingsState> {
    static STATE: std::sync::OnceLock<Mutex<TimingsState>> = std::sync::OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(TimingsState {
            timings: Vec::new(),
            last_time: now_ms(),
        })
    })
}

pub fn reset_timings() {
    if !enabled() {
        return;
    }
    if let Ok(mut guard) = state().lock() {
        guard.timings.clear();
        guard.last_time = now_ms();
    }
}

pub fn time(label: &str) {
    if !enabled() {
        return;
    }
    let now = now_ms();
    if let Ok(mut guard) = state().lock() {
        let last_time = guard.last_time;
        guard.timings.push(TimingEntry {
            label: label.to_string(),
            ms: now - last_time,
        });
        guard.last_time = now;
    }
}

pub fn print_timings() {
    if !enabled() {
        return;
    }
    let (timings, total_ms) = match state().lock() {
        Ok(guard) => {
            if guard.timings.is_empty() {
                return;
            }
            let total: f64 = guard.timings.iter().map(|entry| entry.ms).sum();
            (guard.timings.clone(), total)
        }
        Err(_) => return,
    };

    let log = get_logger("coding-agent.timings");
    let mut fields = serde_json::Map::new();
    fields.insert(
        "timings".to_string(),
        json!(timings
            .iter()
            .map(|entry| json!({ "label": entry.label, "ms": entry.ms }))
            .collect::<Vec<_>>()),
    );
    fields.insert("totalMs".to_string(), json!(total_ms));
    log.debug("startup timings", Some(fields));
    eprintln!("\n--- Startup Timings ---");
    for entry in &timings {
        eprintln!("  {}: {}ms", entry.label, entry.ms);
    }
    eprintln!("  TOTAL: {}ms", total_ms);
    eprintln!("------------------------\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_instrumentation_records_nothing() {
        std::env::remove_var("PI_TIMING");
        reset_timings();
        time("boot");
        let guard = state().lock().unwrap();
        assert!(guard.timings.is_empty());
    }
}
