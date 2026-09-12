//! Port of packages/coding-agent/src/modes/daemon/daemon-catalog-entry.ts
//!
//! The entry point is a module-level side effect in TypeScript: run the catalog
//! process and exit(1) with the same stderr line when it fails.

use std::sync::Arc;

use super::daemon_catalog_process::{run_daemon_catalog_process, CatalogSessionBackend};

pub const DAEMON_CATALOG_FAILURE_PREFIX: &str = "Prime Agent daemon catalog failed: ";

/// Run the catalog process as the entry point does, reporting the failure text.
///
/// The TypeScript entry calls `runDaemonCatalogProcess()` with no argument
/// because it reaches the `SessionManager` statics directly. The port routes
/// those calls through `CatalogSessionBackend`, so the backend is forwarded from
/// the caller that owns the session manager.
// REPAIR CURSOR: `run_daemon_catalog_entry` has no caller in this crate, and
// `CatalogSessionBackend` has no implementor anywhere yet. Wiring it needs a
// `SessionManager`-backed implementor owned by the session slice (or the daemon
// host), plus a call from `main_entry`'s catalog-process branch. Cross-slice
// change; not made here (files outside this pack).
pub async fn run_daemon_catalog_entry(backend: Arc<dyn CatalogSessionBackend>) -> Result<(), String> {
    match run_daemon_catalog_process(backend).await {
        Ok(()) => Ok(()),
        Err(message) => {
            eprintln!("{DAEMON_CATALOG_FAILURE_PREFIX}{message}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_prefix_matches_the_typescript() {
        assert_eq!(
            DAEMON_CATALOG_FAILURE_PREFIX,
            "Prime Agent daemon catalog failed: "
        );
    }
}
