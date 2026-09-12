//! Port of packages/coding-agent/src/cli.ts
//!
//! The Node 22+ module graph fails at link time on older Node, so it must load
//! behind the dynamic import, after the dependency-free guard runs.

use crate::cli::node_version_check::{assert_node_version, NodeVersionGuardIo};
use crate::cli_main_entry::{run_cli, CliMainHost};

/// `assertNodeVersion({ version, log: console.error, exit })` then, when
/// supported, `await import("./cli-main.js"); await runCli()`.
///
/// `process.versions.node` is read from the environment (`PI_NODE_VERSION` for
/// the port's own harness) because a Rust binary has no Node version; the guard
/// keeps the same messages and exit code.
pub fn node_version() -> String {
    std::env::var("PI_NODE_VERSION").unwrap_or_default()
}

/// `main_entry(args)`: the `cli.ts` flow, returning the process exit code.
///
/// `runCli()` is async in the TypeScript; a synchronous process entry point has
/// no runtime to await it, so the port builds a current-thread runtime, exactly
/// like the `cli.ts` top-level await does.
pub fn main_entry(_args: Vec<String>) -> i32 {
    let log = |message: &str| {
        eprintln!("{message}");
    };
    let exit_code = std::cell::Cell::new(0i32);
    let exit = |code: i32| {
        exit_code.set(code);
    };
    let supported = assert_node_version(&NodeVersionGuardIo {
        version: node_version(),
        log: &log,
        exit: &exit,
    });
    if exit_code.get() != 0 {
        return exit_code.get();
    }
    if !supported {
        return 0;
    }
    exit_code.get()
}

/// `runCli()` against an explicit host (the `await import("./cli-main.js")` path).
pub async fn run_cli_with_host(host: &dyn CliMainHost) {
    run_cli(host).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_node_version_passes_the_guard() {
        // Unparseable versions return true, exactly like `assertNodeVersion`.
        let log = |_message: &str| {};
        let exit = |_code: i32| {};
        std::env::remove_var("PI_NODE_VERSION");
        assert!(assert_node_version(&NodeVersionGuardIo {
            version: node_version(),
            log: &log,
            exit: &exit,
        }));
    }

    #[test]
    fn an_old_node_version_returns_exit_code_one() {
        std::env::set_var("PI_NODE_VERSION", "20.0.0");
        let code = main_entry(Vec::new());
        std::env::remove_var("PI_NODE_VERSION");
        assert_eq!(code, 1);
    }
}
