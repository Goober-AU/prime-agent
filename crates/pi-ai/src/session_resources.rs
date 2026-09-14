//! Port of packages/ai/src/session-resources.ts

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};

pub type SessionResourceCleanup = Arc<dyn Fn(Option<&str>) + Send + Sync>;

fn session_resource_cleanups() -> &'static Mutex<BTreeSet<usize>> {
    static SLOT: OnceLock<Mutex<BTreeSet<usize>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// The TypeScript `Set<SessionResourceCleanup>`; Rust callbacks are not
/// comparable, so the registry keys them by a generated id.
fn cleanup_registry() -> &'static Mutex<Vec<(usize, SessionResourceCleanup)>> {
    static SLOT: OnceLock<Mutex<Vec<(usize, SessionResourceCleanup)>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(Vec::new()))
}

fn next_cleanup_id() -> usize {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// `registerSessionResourceCleanup(cleanup)` - returns the unregister function.
pub fn register_session_resource_cleanup(
    cleanup: SessionResourceCleanup,
) -> Box<dyn Fn() + Send + Sync> {
    let id = next_cleanup_id();
    {
        let mut registry = cleanup_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.push((id, cleanup));
        session_resource_cleanups()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id);
    }
    Box::new(move || {
        let mut registry = cleanup_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.retain(|(cleanup_id, _)| *cleanup_id != id);
        session_resource_cleanups()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id);
    })
}

/// `cleanupSessionResources(sessionId?)`.
///
/// The TypeScript throws `AggregateError(errors, "Failed to cleanup session resources")`;
/// Rust has no AggregateError, so the collected messages are joined with newlines.
pub fn cleanup_session_resources(session_id: Option<&str>) -> Result<(), String> {
    let cleanups: Vec<SessionResourceCleanup> = {
        let registry = cleanup_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry
            .iter()
            .map(|(_, cleanup)| cleanup.clone())
            .collect()
    };
    let mut errors: Vec<String> = Vec::new();
    for cleanup in cleanups {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cleanup(session_id)));
        if let Err(panic) = result {
            let message = panic
                .downcast_ref::<&str>()
                .map(|message| (*message).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            errors.push(message);
        }
    }
    if !errors.is_empty() {
        return Err(format!(
            "Failed to cleanup session resources: {}",
            errors.join("\n")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    struct RegisteredCleanup(Box<dyn Fn() + Send + Sync>);
    impl Drop for RegisteredCleanup {
        fn drop(&mut self) {
            (self.0)();
        }
    }

    #[test]
    fn cleanups_run_in_registration_order_and_unregister() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let first = calls.clone();
        let unregister_first = RegisteredCleanup(register_session_resource_cleanup(Arc::new(
            move |session_id| {
                first
                    .lock()
                    .unwrap()
                    .push(format!("first:{:?}", session_id));
            },
        )));
        let second = calls.clone();
        let _second = RegisteredCleanup(register_session_resource_cleanup(Arc::new(
            move |_session_id| {
                second.lock().unwrap().push("second".to_string());
            },
        )));

        (unregister_first.0)();
        cleanup_session_resources(Some("session-1")).unwrap();

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded, vec!["second".to_string()]);
    }

    #[test]
    fn failing_cleanup_is_reported() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let counter = Arc::new(AtomicUsize::new(0));
        let seen = counter.clone();
        let _failing = RegisteredCleanup(register_session_resource_cleanup(Arc::new(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            panic!("cleanup failed");
        })));
        let error = cleanup_session_resources(None).unwrap_err();
        assert!(error.starts_with("Failed to cleanup session resources:"));
        assert!(error.contains("cleanup failed"));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
