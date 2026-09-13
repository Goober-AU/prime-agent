//! Port of packages/coding-agent/src/core/exec.ts
//!
//! Shared command execution utilities for extensions and custom tools.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::utils::child_process::{
    signal_process_group_if_held, signal_process_group_or_process, spawn_hidden,
    wait_for_child_process, Signal, SpawnOptions,
};

/// Options for executing shell commands.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// AbortSignal to cancel the command
    pub signal: Option<CancellationToken>,
    /// Timeout in milliseconds
    pub timeout: Option<f64>,
    /// Working directory
    pub cwd: Option<String>,
    /// Extra env vars merged over the parent process env for this command.
    /// A key with an undefined value is unset in the child.
    pub env: Option<BTreeMap<String, Option<String>>>,
}

/// Result of executing a shell command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i64,
    pub killed: bool,
}

/// `mergeExecEnv(env)`: `undefined` when no extra env was passed, otherwise the
/// parent env with the overrides applied (a `None` value deletes the key).
fn merge_exec_env(env: Option<&BTreeMap<String, Option<String>>>) -> Option<Vec<(String, String)>> {
    let env = env?;
    let mut merged: BTreeMap<String, String> = std::env::vars().collect();
    for (key, value) in env {
        match value {
            None => {
                merged.remove(key);
            }
            Some(value) => {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    Some(merged.into_iter().collect())
}

/// Execute a shell command and return stdout/stderr/code.
/// Supports timeout and abort signal.
pub async fn exec_command(
    command: &str,
    args: &[String],
    cwd: &str,
    options: Option<ExecOptions>,
) -> ExecResult {
    let options = options.unwrap_or_default();
    let spawn_options = SpawnOptions {
        cwd: Some(cwd.to_string()),
        env: merge_exec_env(options.env.as_ref()),
        shell: false,
        capture_stdout: true,
        capture_stderr: true,
        ..Default::default()
    };

    let handle = match spawn_hidden(command, args, spawn_options) {
        Ok(handle) => handle,
        Err(_) => {
            return ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                code: 1,
                killed: false,
            }
        }
    };
    let mut child = handle.child;

    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    let stdout_task = stdout_handle.map(|mut stdout| {
        tokio::spawn(async move {
            let mut buffer = Vec::new();
            let _ = stdout.read_to_end(&mut buffer).await;
            buffer
        })
    });
    let stderr_task = stderr_handle.map(|mut stderr| {
        tokio::spawn(async move {
            let mut buffer = Vec::new();
            let _ = stderr.read_to_end(&mut buffer).await;
            buffer
        })
    });

    let killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // `proc.exitCode`/`proc.signalCode` (exec.ts:81): the delayed SIGKILL is only
    // sent while the child has not exited. Set as soon as the wait observes
    // termination, so a reused PID can never be signalled.
    let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let child_id = child.id();

    // killProcess(): SIGTERM now, SIGKILL after 5s if the child is still alive.
    let kill_process = {
        let killed = Arc::clone(&killed);
        let exited = Arc::clone(&exited);
        move || {
            if killed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            if let Some(pid) = child_id {
                signal_process_group_or_process(pid as i32, Signal::Term);
            }
            let killed_inner = Arc::clone(&killed);
            let exited_inner = Arc::clone(&exited);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(5000)).await;
                if killed_inner.load(std::sync::atomic::Ordering::SeqCst) {
                    if let Some(pid) = child_id {
                        force_kill_still_live_child(pid, &exited_inner);
                    }
                }
            });
        }
    };

    if let Some(signal) = options.signal.clone() {
        if signal.is_cancelled() {
            kill_process();
        } else {
            let kill_process = kill_process.clone();
            tokio::spawn(async move {
                signal.cancelled().await;
                kill_process();
            });
        }
    }

    let timeout_task = match options.timeout {
        Some(timeout) if timeout > 0.0 => {
            let kill_process = kill_process.clone();
            Some(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(timeout as u64)).await;
                kill_process();
            }))
        }
        _ => None,
    };

    // Wait for process termination without hanging on inherited stdio handles
    // held open by detached descendants.
    let code = wait_for_child_process(child).await.unwrap_or(Some(1));
    // The child is gone: any later force kill must be withheld.
    exited.store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some(task) = timeout_task {
        task.abort();
    }

    let stdout = match stdout_task {
        Some(task) => String::from_utf8_lossy(&task.await.unwrap_or_default()).to_string(),
        None => String::new(),
    };
    let stderr = match stderr_task {
        Some(task) => String::from_utf8_lossy(&task.await.unwrap_or_default()).to_string(),
        None => String::new(),
    };

    ExecResult {
        stdout,
        stderr,
        code: code.unwrap_or(0) as i64,
        killed: killed.load(std::sync::atomic::Ordering::SeqCst),
    }
}

/// exec.ts:81: `proc.exitCode === null && proc.signalCode === null` — the delayed
/// SIGKILL only runs while the child has NOT exited. A child that already exited
/// can have its pid (and process group) reused, so signalling it blindly could
/// kill an unrelated process. `signal_process_group_if_held` is the existing
/// owner of the "only signal while provably still the target" guard
/// (child-process.ts:162-168). Returns whether a signal was sent.
fn force_kill_still_live_child(pid: u32, exited: &std::sync::atomic::AtomicBool) -> bool {
    if exited.load(std::sync::atomic::Ordering::SeqCst) {
        return false;
    }
    signal_process_group_if_held(pid as i32, Signal::Kill)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_command() -> (String, Vec<String>) {
        if cfg!(windows) {
            (
                "cmd".to_string(),
                vec!["/c".to_string(), "echo hi".to_string()],
            )
        } else {
            ("sh".to_string(), vec!["-c".to_string(), "echo hi".to_string()])
        }
    }

    #[tokio::test]
    async fn captures_stdout_and_exit_code() {
        let (command, args) = echo_command();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let result = exec_command(&command, &args, &cwd, None).await;
        assert_eq!(result.code, 0);
        assert!(!result.killed);
        assert!(result.stdout.contains("hi"));
    }

    #[tokio::test]
    async fn reports_the_failing_exit_code() {
        let (command, args) = if cfg!(windows) {
            (
                "cmd".to_string(),
                vec!["/c".to_string(), "exit 7".to_string()],
            )
        } else {
            ("sh".to_string(), vec!["-c".to_string(), "exit 7".to_string()])
        };
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let result = exec_command(&command, &args, &cwd, None).await;
        assert_eq!(result.code, 7);
    }

    #[tokio::test]
    async fn kills_on_an_already_aborted_signal() {
        let (command, args) = if cfg!(windows) {
            (
                "cmd".to_string(),
                vec!["/c".to_string(), "ping -n 10 127.0.0.1 >NUL".to_string()],
            )
        } else {
            ("sh".to_string(), vec!["-c".to_string(), "sleep 10".to_string()])
        };
        let token = CancellationToken::new();
        token.cancel();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let result = exec_command(
            &command,
            &args,
            &cwd,
            Some(ExecOptions {
                signal: Some(token),
                ..Default::default()
            }),
        )
        .await;
        assert!(result.killed);
    }

    /// exec.ts:79-84 — the delayed force kill runs only while the child has not
    /// exited (`proc.exitCode === null && proc.signalCode === null`). Without
    /// that liveness guard the reused pid/process group of an exited child could
    /// be signalled and an unrelated process killed.
    #[tokio::test]
    async fn a_delayed_force_kill_never_reaches_an_exited_child() {
        let (command, args) = if cfg!(windows) {
            (
                "cmd".to_string(),
                vec!["/c".to_string(), "ping -n 30 127.0.0.1 >NUL".to_string()],
            )
        } else {
            (
                "sh".to_string(),
                vec!["-c".to_string(), "sleep 30".to_string()],
            )
        };
        let mut handle = spawn_hidden(
            &command,
            &args,
            SpawnOptions {
                capture_stdout: true,
                capture_stderr: true,
                ..Default::default()
            },
        )
        .expect("spawn stand-in for a reused pid");
        let pid = handle.child.id().expect("child pid");

        // The child has exited: the force kill must be withheld.
        let exited = std::sync::atomic::AtomicBool::new(true);
        assert!(
            !force_kill_still_live_child(pid, &exited),
            "a delayed force kill reached a child that had already exited (pid {pid})"
        );
        // The stand-in is untouched, which is the observable symptom of a kill
        // that PID reuse would have aimed at an unrelated process.
        assert!(
            crate::utils::child_process::is_process_alive(pid as i32),
            "an exited child was force-killed anyway"
        );

        // While the child is still live the delayed kill is still delivered.
        let running = std::sync::atomic::AtomicBool::new(false);
        assert!(
            force_kill_still_live_child(pid, &running),
            "a delayed force kill was withheld from a live child (pid {pid})"
        );

        signal_process_group_or_process(pid as i32, Signal::Kill);
        let _ = wait_for_child_process(handle.child).await;
    }

    #[test]
    fn merge_exec_env_is_none_without_overrides() {
        assert!(merge_exec_env(None).is_none());
    }

    #[test]
    fn merge_exec_env_applies_and_unsets_keys() {
        let mut env = BTreeMap::new();
        env.insert("PI_EXEC_TEST".to_string(), Some("1".to_string()));
        env.insert("PATH".to_string(), None);
        let merged = merge_exec_env(Some(&env)).expect("merged env");
        assert!(merged.iter().any(|(key, value)| key == "PI_EXEC_TEST" && value == "1"));
        assert!(!merged.iter().any(|(key, _)| key == "PATH"));
    }
}
