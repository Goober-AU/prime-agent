//! Port of packages/coding-agent/src/modes/daemon/daemon-catalog-entry.ts
//!
//! The entry point is a module-level side effect in TypeScript: run the catalog
//! process and exit(1) with the same stderr line when it fails.

use super::daemon_catalog_process::run_daemon_catalog_process;

pub const DAEMON_CATALOG_FAILURE_PREFIX: &str = "Prime Agent daemon catalog failed: ";

/// Run the catalog process as the entry point does, reporting the failure text.
pub async fn run_daemon_catalog_entry() -> Result<(), String> {
    match run_daemon_catalog_process().await {
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
