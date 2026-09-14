//! Test-only helpers for the process-global environment.
//!
//! Port of `packages/ai/src`; this module exists only under `cfg(test)`.
//!
//! `cargo test` runs the test functions of one binary on parallel threads
//! inside a single process, and `std::env` is process-global. Two failure modes
//! follow, and a restore-on-drop journal alone only fixes the second:
//!
//! 1. Interleaving. Test A sets `OPENAI_API_KEY` (or `PI_CACHE_RETENTION`,
//!    `AZURE_OPENAI_*`, `AWS_*`, `GEMINI_API_KEY`, ...) and then reads; test B
//!    removes the same variable between A's set and A's read, so A sees the
//!    wrong value. Restoring on drop cannot help, because B's remove happens
//!    while A is still running. [`ScopedEnv`] therefore also holds
//!    [`env_lock`] for its whole lifetime, so only one env-mutating test can be
//!    inside its body at a time.
//! 2. Leakage. A test leaves its value behind for whatever runs next. `Drop`
//!    restores the previous value, including the previous *absence* (`None`).
//!
//! This is the same shape the sibling crate landed in
//! `pi-coding-agent/src/core/agent_traces.rs` (`ScopedEnv` + `env_lock`), and
//! the older `EnvRestore` variant in `pi-coding-agent/src/core/auth_storage.rs`
//! covers the leakage half only.
//!
//! The lock is one per process, not one per module: the variable names overlap
//! across modules (`PI_CACHE_RETENTION`, `AWS_*`, `OPENAI_API_KEY`, ...), so two
//! modules serialising independently would still interleave.
//!
//! `#[tokio::test]` here runs the body through `Builder::new_multi_thread()`
//! + `Runtime::block_on`, which drives the future on the calling thread and has
//! no `Send` bound, so a `std::sync::MutexGuard` may stay alive across an
//! `.await` in a test body without deadlocking or failing to compile. A guard
//! moved into a `tokio::spawn`ed task would not be allowed; none is.

use std::sync::{Mutex, MutexGuard, OnceLock};

/// One lock per process, shared by every test that mutates the environment.
pub fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Serializes env-mutating tests and restores the process environment when
/// dropped, including on panic.
///
/// Hold the value in a *named* local for the whole test body:
/// `let mut env = ScopedEnv::new();`, never `let _ = ScopedEnv::new();`
/// (that would drop it immediately and release the lock).
pub struct ScopedEnv {
    _lock: MutexGuard<'static, ()>,
    previous: Vec<(String, Option<String>)>,
}

impl ScopedEnv {
    /// Takes the process-wide environment lock and starts an empty journal.
    pub fn new() -> Self {
        // A panicking test poisons the lock; the next test must still run, so
        // recover the guard instead of failing the whole binary.
        let lock = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ScopedEnv {
            _lock: lock,
            previous: Vec::new(),
        }
    }

    /// Set `name`, remembering the value it had before the first write.
    pub fn set(&mut self, name: &str, value: impl AsRef<std::ffi::OsStr>) {
        self.remember(name);
        std::env::set_var(name, value);
    }

    /// Remove `name`, remembering the value it had before the first removal.
    pub fn remove(&mut self, name: &str) {
        self.remember(name);
        std::env::remove_var(name);
    }

    fn remember(&mut self, name: &str) {
        if self.previous.iter().any(|(saved, _)| saved == name) {
            return;
        }
        self.previous
            .push((name.to_string(), std::env::var(name).ok()));
    }
}

impl Default for ScopedEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        for (name, value) in self.previous.drain(..) {
            match value {
                Some(value) => std::env::set_var(&name, value),
                None => std::env::remove_var(&name),
            }
        }
    }
}
