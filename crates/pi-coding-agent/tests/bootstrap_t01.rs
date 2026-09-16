//! T01 — managed Python bootstrap reuse and validation (G-03, G-07, G-13, G-28, G-29, G-31).
//!
//! Owner: bootstrap. Fixture roots: work/state-roots/bootstrap/<case>/.
//! The fixture "python" (compiled no-op exe) and fixture "uv" (recording exe
//! shim) replace EXTERNAL tool I/O only; every code path under test is the
//! real production bootstrap in crates/pi-coding-agent.

#![cfg(windows)]

#[path = "bootstrap_support/mod.rs"]
mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use pi_coding_agent::core::kernel::bootstrap::{
    ensure_kernel_python, get_kernel_venv_dir, resolve_runtime_identity, EnsureKernelPythonOptions,
};
use pi_coding_agent::core::kernel::shared::KernelPythonSkill;
use pi_coding_agent::postinstall::run_postinstall;
use support::*;

const CHILD_TIMEOUT_MS: u64 = 90_000;

fn child_command(fixture: &Fixture, role: &str, extra: &[(&str, String)]) -> std::process::Command {
    let exe = std::env::current_exe().expect("current_exe");
    let mut command = std::process::Command::new(exe);
    command
        .args(["child_entry", "--exact", "--test-threads", "1", "--nocapture"])
        .env_clear();
    for (key, value) in fixture.child_env(role, extra) {
        command.env(key, value);
    }
    command.current_dir(&fixture.root);
    command
}

#[derive(Debug)]
struct ChildRun {
    exit_code: i32,
    ok: bool,
    python: Option<String>,
    error: Option<String>,
    stderr: String,
}

fn run_child(
    mut command: std::process::Command,
    timeout_ms: u64,
    result_path: &Path,
) -> ChildRun {
    use std::io::Read;
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("spawn child test process");
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let (status, stderr_text) = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                let mut stderr_text = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut stderr_text);
                }
                break (status, stderr_text);
            }
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ChildRun {
                        exit_code: -999,
                        ok: false,
                        python: None,
                        error: Some("child watchdog timeout".to_string()),
                        stderr: String::new(),
                    };
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let (ok, python, error) = match std::fs::read_to_string(result_path) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => (
                value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
                value.get("python").and_then(|v| v.as_str()).map(str::to_string),
                value.get("error").and_then(|v| v.as_str()).map(str::to_string),
            ),
            Err(error) => (false, None, Some(format!("result parse: {error}"))),
        },
        Err(_) => (false, None, Some("child wrote no result file".to_string())),
    };
    ChildRun {
        exit_code: status.code().unwrap_or(-1),
        ok,
        python,
        error,
        stderr: stderr_text,
    }
}

fn venv_python_creation_time(fixture: &Fixture) -> Option<u128> {
    std::fs::metadata(fixture.venv.join("Scripts").join("python.exe"))
        .ok()
        .and_then(|meta| {
            meta.created()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_micros())
        })
}

/// V00-lite: prove the effective kernel-venv root is the private fixture
/// before any bootstrap runs.
fn assert_effective_venv_root(fixture: &Fixture) {
    let resolved = get_kernel_venv_dir();
    let expected = fixture.venv.to_string_lossy().replace('/', "\\");
    let resolved_back = resolved.replace('/', "\\");
    assert_eq!(
        resolved_back.to_lowercase(),
        expected.to_lowercase(),
        "effective venv root must be the private fixture, got {resolved}"
    );
    assert!(
        fixture.venv.starts_with(&fixture.root),
        "fixture venv must live under the private fixture root"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn child_entry() {
    let Ok(role) = std::env::var("PARITY_CHILD_ROLE") else {
        return; // parent mode: the harness runs the real tests below
    };
    if role == "postinstall" {
        let code = run_postinstall().await;
        let result_path = std::env::var("PARITY_CHILD_RESULT").expect("PARITY_CHILD_RESULT");
        let body = serde_json::json!({ "ok": code == 0, "code": code });
        std::fs::write(&result_path, body.to_string()).expect("write child result");
        return;
    }
    assert_eq!(role, "bootstrap", "unexpected child role");
    let options = match std::env::var("PARITY_CHILD_SKILLS") {
        Ok(text) => {
            let values: Vec<serde_json::Value> = serde_json::from_str(&text).expect("skills json");
            let skills = values
                .into_iter()
                .map(|value| KernelPythonSkill {
                    import_name: value["import_name"].as_str().expect("import_name").to_string(),
                    package_path: value["package_path"].as_str().expect("package_path").to_string(),
                    pyproject_path: value["pyproject_path"].as_str().expect("pyproject_path").to_string(),
                    name: value["name"].as_str().unwrap_or_default().to_string(),
                })
                .collect();
            ensure_kernel_python(EnsureKernelPythonOptions {
                python_skills: Some(skills),
                on_progress: None,
            })
            .await
        }
        Err(_) => ensure_kernel_python(EnsureKernelPythonOptions::default()).await,
    };
    let result_path = std::env::var("PARITY_CHILD_RESULT").expect("PARITY_CHILD_RESULT");
    let body = match options {
        Ok(python) => serde_json::json!({ "ok": true, "python": python }),
        Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
    };
    std::fs::write(&result_path, body.to_string()).expect("write child result");
}

/// G-29: a second fresh process must REUSE the managed venv instead of
/// rebuilding it. Baseline (defect) expectation: the marker round-trip is
/// broken, so the second process tears the venv down and bootstraps again.
#[tokio::test(flavor = "multi_thread")]
async fn second_process_bootstrap_reuses_venv() {
    let _env = env_lock();
    let _guard = EnvGuard::capture();
    let fixture = Fixture::new("t01-reuse");
    std::fs::create_dir_all(fixture.root.join("tmp")).unwrap();
    fixture.apply_env();
    assert_effective_venv_root(&fixture);

    let first = run_child(
        child_command(&fixture, "bootstrap", &[]),
        CHILD_TIMEOUT_MS,
        &fixture.root.join("child-result.json"),
    );
    let first_log = fixture.uv_lines();
    assert!(
        first.exit_code == 0 && first.ok,
        "first bootstrap child must succeed: {first:?}\nlog={first_log:?}"
    );
    assert!(
        first_log.iter().any(|line| line.starts_with("python install")),
        "first bootstrap must invoke uv python install: {first_log:?}"
    );
    assert!(
        first_log.iter().any(|line| line.starts_with("venv ")),
        "first bootstrap must invoke uv venv: {first_log:?}"
    );
    assert!(
        first_log.iter().any(|line| line.starts_with("pip install")),
        "first bootstrap must invoke uv pip install: {first_log:?}"
    );
    let marker_after_first = std::fs::read(&fixture.marker).expect("marker written");
    let python_time_first = venv_python_creation_time(&fixture).expect("python exe");

    // Second fresh process: PRIME_AGENT_KERNEL_PYTHON stays UNSET.
    let second = run_child(
        child_command(&fixture, "bootstrap", &[]),
        CHILD_TIMEOUT_MS,
        &fixture.root.join("child-result.json"),
    );
    assert!(
        second.exit_code == 0 && second.ok,
        "second bootstrap child must succeed: {second:?}"
    );
    assert_eq!(
        second.python.as_deref(),
        first.python.as_deref(),
        "both processes must resolve the same managed venv python"
    );
    let log_after_second = fixture.uv_lines();
    assert_eq!(
        log_after_second.len(),
        first_log.len(),
        "second process must not invoke uv again (rebuild detected): {log_after_second:?}"
    );
    let marker_after_second = std::fs::read(&fixture.marker).expect("marker preserved");
    assert_eq!(
        marker_after_first, marker_after_second,
        "marker must be unchanged when the venv is reused"
    );
    let python_time_second = venv_python_creation_time(&fixture).expect("python exe");
    assert_eq!(
        python_time_first, python_time_second,
        "venv python must not be recreated by the second process"
    );
}

/// G-07 integration: the sync pass must resolve duplicate project names like
/// the TypeScript reference (LAST duplicate wins). Observable through the real
/// uv pip arguments: the FAILED first duplicate must not leak into the
/// dependent skill's pip invocation. Baseline (first-wins) leaks it; the TS
/// contract resolves the surviving duplicate instead.
#[tokio::test(flavor = "multi_thread")]
async fn duplicate_skill_dependency_resolves_last_in_sync() {
    let _env = env_lock();
    let _guard = EnvGuard::capture();
    let fixture = Fixture::new("t01-dup-sync");
    std::fs::create_dir_all(fixture.root.join("tmp")).unwrap();
    fixture.apply_env();
    assert_effective_venv_root(&fixture);
    let alpha = fixture.root.join("s1").join("alpha");
    let beta = fixture.root.join("s2").join("beta");
    let gamma = fixture.root.join("s3").join("gamma");
    for (dir, body) in [
        (&alpha, "[project]\nname = \"parity-dup-name\"\n"),
        (&beta, "[project]\nname = \"parity-dup-name\"\n"),
        (
            &gamma,
            "[project]\nname = \"parity-gamma\"\ndependencies = [\"parity-dup-name\"]\n",
        ),
    ] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("pyproject.toml"), body).unwrap();
    }
    let skill = |import_name: &str, dir: &Path| KernelPythonSkill {
        import_name: import_name.to_string(),
        package_path: dir.to_string_lossy().into_owned(),
        pyproject_path: dir.join("pyproject.toml").to_string_lossy().into_owned(),
        name: String::new(),
    };
    let skills = vec![
        skill("parity_alpha", &alpha),
        skill("parity_beta", &beta),
        skill("parity_gamma", &gamma),
    ];
    std::env::set_var(
        "PARITY_UV_FAIL_PATH",
        alpha.to_string_lossy().to_string(),
    );
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        ensure_kernel_python(EnsureKernelPythonOptions {
            python_skills: Some(skills),
            on_progress: None,
        }),
    )
    .await;
    match outcome {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            panic!("one failed skill is a warning, not fatal: {error}")
        }
        Err(_) => panic!("bootstrap hung (watchdog)"),
    }
    let alpha_path = alpha.to_string_lossy().to_string();
    let lines = fixture.uv_lines();
    let leaked: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains(&alpha_path))
        .collect();
    assert_eq!(
        leaked.len(),
        1,
        "G-07: the FIRST duplicate leaked into the dependent's pip args \
         (baseline first-wins): {lines:?}"
    );
}

/// G-03 + G-31: identity-hash failures must surface as errors (not silently
/// fall back to the registry identity), and a failed venv teardown must fail
/// the bootstrap (not silently reuse partially rebuilt state).
#[tokio::test(flavor = "multi_thread")]
async fn bootstrap_errors_remain_errors() {
    let _env = env_lock();
    let _guard = EnvGuard::capture();

    // --- G-03: unreadable runtime source -> TS throws; Rust must surface it.
    let fixture = Fixture::new("t01-errors");
    std::fs::create_dir_all(fixture.root.join("tmp")).unwrap();
    let pkg_root = fixture.root.join("pkg");
    let runtime_dir = pkg_root.join("dist").join("prime-agent-runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    std::fs::write(runtime_dir.join("pyproject.toml"), "[project]\nname = \"prime-agent-runtime\"\n").unwrap();
    // src/rlm is a FILE, not a directory: read_dir must fail -> hash error.
    std::fs::write(runtime_dir.join("src"), "not a directory").unwrap();
    std::env::set_var("PI_PACKAGE_DIR", pkg_root.to_string_lossy().to_string());
    let (ok_identity, err_identity) = resolve_runtime_identity().await.probe();
    assert!(
        err_identity.is_some(),
        "runtime identity must surface a hash failure, got registry fallback: {ok_identity:?}"
    );
    // Negative control: a readable runtime source hashes fine.
    std::fs::remove_file(runtime_dir.join("src")).unwrap();
    std::fs::create_dir_all(runtime_dir.join("src").join("rlm")).unwrap();
    std::fs::write(
        runtime_dir.join("src").join("rlm").join("x.py"),
        "def placeholder():\n    return None\n",
    )
    .unwrap();
    let (ok_identity, err_identity) = resolve_runtime_identity().await.probe();
    assert!(
        err_identity.is_none(),
        "readable runtime source must hash: {err_identity:?}"
    );
    assert!(
        ok_identity.as_deref().is_some_and(|value| value.starts_with("sha256:")),
        "readable runtime source must hash, got {ok_identity:?}"
    );
    // Registry-install case (no source dir) stays a valid registry identity.
    std::env::remove_var("PI_PACKAGE_DIR");
    let (ok_identity, err_identity) = resolve_runtime_identity().await.probe();
    assert!(
        err_identity.is_none() && ok_identity.is_some(),
        "registry fallback without a source dir is legitimate: {ok_identity:?} / {err_identity:?}"
    );
    std::env::remove_var("PI_PACKAGE_DIR");
    std::env::remove_var("PRIME_AGENT_KERNEL_VENV");

    // --- G-31: a teardown failure must surface, not silently rebuild over it.
    let fixture = Fixture::new("t01-teardown");
    std::fs::create_dir_all(fixture.root.join("tmp")).unwrap();
    fixture.apply_env();
    assert_effective_venv_root(&fixture);
    let first = run_child(
        child_command(&fixture, "bootstrap", &[]),
        CHILD_TIMEOUT_MS,
        &fixture.root.join("child-result.json"),
    );
    assert!(first.exit_code == 0 && first.ok, "preparation bootstrap must succeed: {first:?}");
    // Corrupt the marker so the next ensure call takes the rebuild path.
    std::fs::write(&fixture.marker, "{\"schema\":9,\"runtime\":\"sha256:0000000000000000000000000000000000000000000000000000000000000000\",\"snapshot\":\"dill\",\"extraUvArgs\":[],\"pythonSkills\":[]}\n").unwrap();
    // Hold the venv directory open with no sharing so remove_dir_all fails.
    let hold = hold_directory(&fixture.venv);
    let held = tokio::time::timeout(
        Duration::from_secs(60),
        ensure_kernel_python(EnsureKernelPythonOptions::default()),
    )
    .await;
    drop(hold);
    match held {
        Err(_elapsed) => panic!("rebuild with a held venv must finish or fail, not hang"),
        Ok(Ok(python)) => {
            assert!(
                false,
                "G-31: teardown failure was swallowed; ensure_kernel_python reported success ({python})"
            );
        }
        Ok(Err(error)) => {
            let message = error.to_string();
            assert!(
                message.contains("Failed to set up the Python kernel runtime"),
                "G-31: expected the formatted bootstrap failure, got: {message}"
            );
        }
    }
}

/// G-28 parity oracle, both halves of the TS contract
/// (packages/coding-agent/src/postinstall.ts):
/// 1. tools unavailable (offline + empty PI_BIN_DIR): `ensureTool` resolves
///    undefined (its download-failure path catches internally), so the catch
///    does NOT fire, ensureKernelPython STILL runs, exit 0, NO skipped
///    message.
/// 2. kernel setup FAILING: the catch fires, the skipped message prints, and
///    the hook still exits 0 (TS identical).
#[tokio::test(flavor = "multi_thread")]
async fn bootstrap_edge_contracts_postinstall() {
    let _env = env_lock();
    let _guard = EnvGuard::capture();

    // --- Scenario 1: tools unavailable, kernel bootstrap healthy.
    let fixture = Fixture::new("t01-postinstall");
    std::fs::create_dir_all(fixture.root.join("tmp")).unwrap();
    let empty_bin = fixture.root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    fixture.apply_env();
    assert_effective_venv_root(&fixture);
    let run = run_child(
        child_command(
            &fixture,
            "postinstall",
            &[
                ("PRIME_AGENT_BOOTSTRAP_KERNEL_ON_INSTALL", "1".to_string()),
                ("PRIME_AGENT_BOOTSTRAP_TOOLS_ON_INSTALL", "1".to_string()),
                ("PI_OFFLINE", "1".to_string()),
                ("PI_BIN_DIR", empty_bin.to_string_lossy().into_owned()),
            ],
        ),
        CHILD_TIMEOUT_MS,
        &fixture.root.join("child-result.json"),
    );
    assert!(
        run.exit_code == 0 && run.ok,
        "postinstall hook never fails the install: {run:?}"
    );
    assert!(
        !run.stderr.contains("prime-agent: postinstall setup skipped"),
        "TS contract: a tools failure must NOT print the skipped message; stderr={}",
        run.stderr
    );
    assert!(
        fixture.marker.exists(),
        "TS contract: kernel bootstrap still runs when the tools are unavailable"
    );
    assert!(
        fixture.uv_lines().iter().any(|line| line.starts_with("venv ")),
        "kernel venv was bootstrapped through the real ensure path"
    );

    // --- Scenario 2: kernel setup fails -> skipped message, exit 0.
    let fixture = Fixture::new("t01-postinstall-skip");
    std::fs::create_dir_all(fixture.root.join("tmp")).unwrap();
    fixture.apply_env();
    let bad_python = fixture.root.join("missing").join("python.exe");
    let run = run_child(
        child_command(
            &fixture,
            "postinstall",
            &[
                ("PRIME_AGENT_BOOTSTRAP_KERNEL_ON_INSTALL", "1".to_string()),
                ("PRIME_AGENT_KERNEL_PYTHON", bad_python_path(&fixture).to_string_lossy().into_owned()),
            ],
        ),
        CHILD_TIMEOUT_MS,
        &fixture.root.join("child-result.json"),
    );
    assert!(
        run.exit_code == 0 && run.ok,
        "the skipped path still exits 0: {run:?}"
    );
    assert!(
        run.stderr.contains("prime-agent: postinstall setup skipped"),
        "TS contract: a failing kernel setup must print the skipped message; stderr={}",
        run.stderr
    );
    let _ = empty_bin;
}

fn bad_python_path(fixture: &Fixture) -> std::path::PathBuf {
    fixture.root.join("missing").join("python.exe")
}

/// Bridges the pre-fix (`String` registry fallback) and post-fix
/// (`Result<String, KernelError>`) identity signatures so the SAME test
/// compiles against both trees and fails with evidence on the baseline.
trait IdentityProbe {
    fn probe(self) -> (Option<String>, Option<String>);
}

impl IdentityProbe for String {
    fn probe(self) -> (Option<String>, Option<String>) {
        (Some(self), None)
    }
}

impl IdentityProbe
    for Result<String, pi_coding_agent::core::kernel::shared::KernelError>
{
    fn probe(self) -> (Option<String>, Option<String>) {
        match self {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error.to_string())),
        }
    }
}
