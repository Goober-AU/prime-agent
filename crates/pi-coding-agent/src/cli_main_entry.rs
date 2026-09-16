//! Port of packages/coding-agent/src/cli-main.ts

use crate::cli::daemon_launch::maybe_start_daemon_early;
use crate::config::APP_NAME;
use crate::modes::daemon::daemon_catalog_process::is_daemon_catalog_process_from_env;

fn can_start_frontend_daemon(owned_worker: bool, catalog: bool) -> bool {
    !owned_worker && !catalog
}

/// `enableCompileCache?.()`: the port is a compiled binary, so there is no cache
/// to enable; the TypeScript swallows a read-only cache dir the same way.
fn enable_compile_cache() {}

/// The `owned-session-worker.ts` surface this entry point drives.
///
/// `cli/owned-session-worker.ts` belongs to another slice (ca-cli-b) and is not
/// landed, so the four calls are a seam with the same order and the same
/// meanings as the TypeScript.
pub trait OwnedSessionWorkerEntry {
    /// `installOwnedSessionWorkerOwnerWatch()`.
    fn install_owner_watch(&self);
    /// `closeOwnedSessionWorkerOwnerWatch()`.
    fn close_owner_watch(&self);
    /// `isOwnedSessionWorkerProcess()`.
    fn is_owned_session_worker_process(&self) -> bool;
    /// `maybeRunOwnedSessionWorkerFrontend(args)`.
    fn maybe_run_frontend(
        &self,
        args: Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>;
}

/// The `main.ts` surface this entry point drives.
///
/// `main.ts` is this slice's own `main_entry.rs`; the seam keeps the entry point
/// testable without touching process globals.
pub trait CliMainHost: OwnedSessionWorkerEntry {
    /// `main(process.argv.slice(2))`.
    fn run_main(
        &self,
        args: Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
    /// `process.argv` (the full argv, including the executable and the script).
    fn argv(&self) -> Vec<String>;
    /// `process.title = APP_NAME`.
    fn set_process_title(&self, title: &str);
    /// `process.env.PI_CODING_AGENT = "true"`.
    fn set_env(&self, key: &str, value: &str);
    /// `process.emitWarning = () => {}`.
    fn silence_emit_warning(&self);
    /// `undici`'s global dispatcher with `bodyTimeout: 0, headersTimeout: 0`.
    ///
    /// The port's HTTP client has no global undici dispatcher; provider HTTP
    /// deadlines come from `retry.provider.timeoutMs`, so this is a no-op with
    /// the same observable result (no global 300s abort).
    fn install_global_dispatcher(&self);
}

/// `runCli()`.
pub async fn run_cli(host: &dyn CliMainHost) -> Result<(), String> {
    enable_compile_cache();

    host.set_process_title(APP_NAME);
    host.set_env("PI_CODING_AGENT", "true");
    host.silence_emit_warning();

    host.install_owner_watch();

    let argv = host.argv();
    let args: Vec<String> = argv.iter().skip(2).cloned().collect();
    let handled_by_owned_worker = host.maybe_run_frontend(args.clone()).await;
    if !handled_by_owned_worker {
        if can_start_frontend_daemon(host.is_owned_session_worker_process(), is_daemon_catalog_process_from_env()) {
            // Boot a cold daemon concurrently with this process's heavy imports.
            maybe_start_daemon_early(&args);
        }
        host.install_global_dispatcher();

        let result = host.run_main(args).await;
        host.close_owner_watch();
        result?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn catalog_and_workers_never_start_a_frontend_daemon() {
        assert!(can_start_frontend_daemon(false, false));
        assert!(!can_start_frontend_daemon(false, true));
        assert!(!can_start_frontend_daemon(true, false));
        assert!(!can_start_frontend_daemon(true, true));
    }

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
    }

    impl OwnedSessionWorkerEntry for Recorder {
        fn install_owner_watch(&self) {
            self.calls.lock().unwrap().push("install".to_string());
        }
        fn close_owner_watch(&self) {
            self.calls.lock().unwrap().push("close".to_string());
        }
        fn is_owned_session_worker_process(&self) -> bool {
            false
        }
        fn maybe_run_frontend(
            &self,
            _args: Vec<String>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> {
            self.calls.lock().unwrap().push("frontend".to_string());
            Box::pin(async { false })
        }
    }

    impl CliMainHost for Recorder {
        fn run_main(
            &self,
            _args: Vec<String>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        {
            self.calls.lock().unwrap().push("main".to_string());
            Box::pin(async { Ok(()) })
        }
        fn argv(&self) -> Vec<String> {
            vec![
                "node".to_string(),
                "cli.js".to_string(),
                "--version".to_string(),
            ]
        }
        fn set_process_title(&self, title: &str) {
            self.calls.lock().unwrap().push(format!("title:{title}"));
        }
        fn set_env(&self, key: &str, value: &str) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("env:{key}={value}"));
        }
        fn silence_emit_warning(&self) {
            self.calls.lock().unwrap().push("silence".to_string());
        }
        fn install_global_dispatcher(&self) {
            self.calls.lock().unwrap().push("dispatcher".to_string());
        }
    }

    #[test]
    fn run_cli_sets_identity_then_runs_main() {
        let recorder = Recorder::default();
        futures::executor::block_on(run_cli(&recorder)).expect("CLI startup");
        let calls = recorder.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                "title:prime-agent".to_string(),
                "env:PI_CODING_AGENT=true".to_string(),
                "silence".to_string(),
                "install".to_string(),
                "frontend".to_string(),
                "dispatcher".to_string(),
                "main".to_string(),
                "close".to_string(),
            ]
        );
    }
}
