//! T02 — Windows bootstrap lock semantics (G-01, G-02, G-04).
//!
//! All lock scenarios go through the real bootstrap.rs lock path
//! (ensure_kernel_python -> acquire_bootstrap_lock); none of these tests call
//! the generic utils/dir_lock helper directly.
//! G-04 is POSIX-only: no POSIX runner is available in this lane, so the mode
//! question is reported as platform-not-tested rather than approximated here.

#![cfg(windows)]

#[path = "bootstrap_support/mod.rs"]
mod support;

use std::process::Command;
use std::time::{Duration, Instant};

use pi_coding_agent::core::kernel::bootstrap::{
    ensure_kernel_python, get_kernel_venv_dir, EnsureKernelPythonOptions,
};
use support::*;

const CHILD_TIMEOUT_MS: u64 = 90_000;

fn child_command(fixture: &Fixture) -> std::process::Command {
    let exe = std::env::current_exe().expect("current_exe");
    let mut command = Command::new(exe);
    command
        .args(["child_entry", "--exact", "--test-threads", "1", "--nocapture"])
        .env_clear();
    for (key, value) in fixture.child_env("bootstrap", &[]) {
        command.env(key, value);
    }
    command.current_dir(&fixture.root);
    command
}

fn run_child(mut command: std::process::Command, timeout_ms: u64) -> (i32, bool) {
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("spawn child test process");
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (-999, false);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    (status.code().unwrap_or(-1), status.success())
}

fn bootstrap_lock_path(fixture: &Fixture) -> std::path::PathBuf {
    let parent = fixture.venv.parent().expect("venv parent");
    let name = fixture
        .venv
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    parent.join(format!("{name}.bootstrap.lock"))
}

#[tokio::test(flavor = "multi_thread")]
async fn child_entry() {
    let Ok(role) = std::env::var("PARITY_CHILD_ROLE") else {
        return; // parent mode
    };
    assert_eq!(role, "bootstrap", "unexpected child role");
    let result = ensure_kernel_python(EnsureKernelPythonOptions::default()).await;
    let result_path = std::env::var("PARITY_CHILD_RESULT").expect("PARITY_CHILD_RESULT");
    let body = match result {
        Ok(python) => serde_json::json!({ "ok": true, "python": python }),
        Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
    };
    std::fs::write(&result_path, body.to_string()).expect("write child result");
}

/// G-01: a stale lock from a proved-dead bootstrap process must be reclaimed
/// through the real bootstrap.rs path within a bounded deadline.
#[tokio::test(flavor = "multi_thread")]
async fn stale_bootstrap_lock_recovers_on_ntfs() {
    let _env = env_lock();
    let _guard = EnvGuard::capture();
    let fixture = Fixture::new("t02-stale");
    std::fs::create_dir_all(fixture.root.join("tmp")).unwrap();
    fixture.apply_env();
    // Effective-root proof before touching state.
    let resolved = get_kernel_venv_dir().replace('/', "\\").to_lowercase();
    assert_eq!(
        resolved,
        fixture.venv.to_string_lossy().replace('/', "\\").to_lowercase(),
        "the lock test must run against the private fixture venv"
    );

    let dead_pid = spawn_and_reap_dead_pid();
    let lock_path = bootstrap_lock_path(&fixture);
    std::fs::write(&lock_path, format!("{dead_pid}\n")).expect("write stale lock");
    assert!(
        lock_path.exists(),
        "stale lock fixture must exist before the bootstrap runs"
    );

    let held = tokio::time::timeout(
        Duration::from_secs(30),
        ensure_kernel_python(EnsureKernelPythonOptions::default()),
    )
    .await;
    match held {
        Err(_) => panic!("G-01 reproduced: the stale lock was never reclaimed within 30s"),
        Ok(Ok(python)) => {
            assert!(
                python.contains("Scripts"),
                "bootstrap must resolve the managed venv python: {python}"
            );
            assert!(
                !lock_path.exists(),
                "the acquired lock must be released after the bootstrap finishes"
            );
        }
        Ok(Err(error)) => panic!("stale-lock recovery must acquire, got error: {error}"),
    }
}

/// G-01 negative controls: a LIVE holder is never stolen, and a lock that was
/// replaced with a live owner between judgements is never deleted.
#[tokio::test(flavor = "multi_thread")]
async fn live_and_replaced_locks_are_not_stolen() {
    let _env = env_lock();
    let _guard = EnvGuard::capture();
    let fixture = Fixture::new("t02-live");
    std::fs::create_dir_all(fixture.root.join("tmp")).unwrap();
    fixture.apply_env();
    let lock_path = bootstrap_lock_path(&fixture);

    // A live holder blocks inside its own bootstrap via the slow uv shim.
    let mut holder = child_command(&fixture)
        .env("PARITY_UV_SLEEP_MS", "6000")
        .spawn()
        .expect("spawn holder child");
    let holder_pid = holder.id();

    // Wait for the holder to publish its lock.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if lock_path.exists() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "holder never published its bootstrap lock"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let lock_content = std::fs::read_to_string(&lock_path).expect("read holder lock");
    assert!(
        lock_content.contains(&holder_pid.to_string()),
        "holder lock must carry the holder pid: {lock_content}"
    );

    // While the holder is alive, a second bootstrap must stay Held and must
    // not touch the holder's lock.
    let second = tokio::time::timeout(
        Duration::from_secs(4),
        ensure_kernel_python(EnsureKernelPythonOptions::default()),
    )
    .await;
    assert!(second.is_err(), "a live lock must not be acquired while held");
    let lock_after_attempt = std::fs::read_to_string(&lock_path).expect("lock still present");
    assert_eq!(
        lock_after_attempt, lock_content,
        "the live holder's lock content must be untouched"
    );

    // Replace the lock with a live CONTROL owner before the next attempt: the
    // judge must respect the new live owner, not delete the replacement.
    let _ = holder.kill();
    let _ = holder.wait();
    let mut control = Command::new("cmd.exe")
        .args(["/d", "/s", "/c", "ping", "-n", "30", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn control process");
    let control_pid = control.id();
    std::fs::write(&lock_path, format!("{control_pid}\n")).expect("write replacement lock");
    // A fresh child process must judge the replacement lock LIVE and stay
    // Held without deleting or rewriting it.
    let challenger = child_command(&fixture);
    let (challenger_code, challenger_ok) = run_child(challenger, 8_000);
    assert!(
        challenger_code == -999 && !challenger_ok,
        "a bootstrap against a live replacement lock must stay Held (watchdog),          got code={challenger_code} ok={challenger_ok}"
    );
    let replacement_content =
        std::fs::read_to_string(&lock_path).expect("replacement lock preserved");
    assert_eq!(
        replacement_content,
        format!("{control_pid}\n"),
        "the replacement lock must not be deleted by a second bootstrap attempt"
    );

    // Cleanup: the control is test-owned; stop it by its recorded identity.
    let _ = control.kill();
    let _ = control.wait();
}

/// G-02: a hard I/O failure (access-denied on the lock directory) must fail
/// fast with an actionable error instead of Held plus an endless 100ms retry.
#[tokio::test(flavor = "multi_thread")]
async fn lock_permission_error_fails_fast() {
    let _env = env_lock();
    let _guard = EnvGuard::capture();
    let fixture = Fixture::new("t02-acl");
    std::fs::create_dir_all(fixture.root.join("tmp")).unwrap();
    fixture.apply_env();
    // Deny file-creation on the venv parent (which is also the lock parent).
    let deny = Command::new("icacls")
        .args([
            fixture.root.to_string_lossy().to_string(),
            "/deny".to_string(),
            "*S-1-1-0:(OI)(CI)(WD)".to_string(),
        ])
        .output()
        .expect("icacls deny");
    assert!(deny.status.success(), "icacls deny must apply: {:?}", String::from_utf8_lossy(&deny.stdout));
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        ensure_kernel_python(EnsureKernelPythonOptions::default()),
    )
    .await;
    let restore = Command::new("icacls")
        .args([
            fixture.root.to_string_lossy().to_string(),
            "/remove:d".to_string(),
            "*S-1-1-0".to_string(),
        ])
        .output()
        .expect("icacls restore");
    assert!(restore.status.success(), "icacls restore must succeed");
    match outcome {
        Err(_) => panic!(
            "G-02 reproduced: the bootstrap spun on a permission error instead of failing fast"
        ),
        Ok(Err(error)) => {
            let message = error.to_string();
            assert!(
                !message.is_empty(),
                "the injected failure must produce an actionable error"
            );
        }
        Ok(Ok(python)) => panic!(
            "G-02: a denied lock directory must not yield success ({python})"
        ),
    }
}
