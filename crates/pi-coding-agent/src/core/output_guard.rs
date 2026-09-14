//! Port of packages/coding-agent/src/core/output-guard.ts
//!
//! The TypeScript swaps `process.stdout.write` for a function that forwards to
//! stderr, keeping the raw stdout writer for explicit protocol output. Rust has
//! no writable global stdout handle, so the takeover is represented by a
//! process-global flag: [`is_stdout_taken_over`] gates the same call sites and
//! [`write_raw_stdout`] stays the only path that writes to real stdout.

use std::io::Write;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdoutState {
    Free,
    TakenOver,
}

static STATE: Mutex<StdoutState> = Mutex::new(StdoutState::Free);

pub fn take_over_stdout() {
    if let Ok(mut guard) = STATE.lock() {
        if *guard == StdoutState::TakenOver {
            return;
        }
        *guard = StdoutState::TakenOver;
    }
}

pub fn restore_stdout() {
    if let Ok(mut guard) = STATE.lock() {
        *guard = StdoutState::Free;
    }
}

pub fn is_stdout_taken_over() -> bool {
    match STATE.lock() {
        Ok(guard) => *guard == StdoutState::TakenOver,
        Err(_) => false,
    }
}

pub fn write_raw_stdout(text: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(text.as_bytes());
    let _ = lock.flush();
}

/// Writes to the guarded stdout: while the takeover is active, output goes to
/// stderr (the TypeScript replacement writer), otherwise to real stdout.
pub fn write_stdout(text: &str) {
    if is_stdout_taken_over() {
        let stderr = std::io::stderr();
        let mut lock = stderr.lock();
        let _ = lock.write_all(text.as_bytes());
        let _ = lock.flush();
        return;
    }
    write_raw_stdout(text);
}

pub async fn flush_raw_stdout() {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takeover_is_idempotent_and_reversible() {
        restore_stdout();
        assert!(!is_stdout_taken_over());
        take_over_stdout();
        take_over_stdout();
        assert!(is_stdout_taken_over());
        restore_stdout();
        restore_stdout();
        assert!(!is_stdout_taken_over());
    }

    #[tokio::test]
    async fn flush_completes_while_taken_over() {
        take_over_stdout();
        flush_raw_stdout().await;
        restore_stdout();
        assert!(!is_stdout_taken_over());
    }
}
