//! Shared fixture helpers for the bootstrap parity suites (T01/T02).
//!
//! Isolation (V00): every writable artifact lives under
//! a unique temporary directory — never .prime or the real profile.
//! The fixture "python" is a compiled no-op executable and the fixture "uv"
//! is a recording batch shim: both stand-ins replace external tool I/O only;
//! all bootstrap logic under test is the real `pi_coding_agent` production code.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};

fn tools_dir() -> PathBuf {
    std::env::var_os("PARITY_BOOTSTRAP_TOOLS_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .expect("set PARITY_BOOTSTRAP_TOOLS_DIR after running tests/bootstrap_support/build-fixtures.cmd")
}

pub static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// All env-mutating tests serialize on this lock (and suites run with
/// --test-threads=1 on top).
pub fn env_lock() -> MutexGuard<'static, ()> {
    // A panicking test legitimately leaves the mutex poisoned in a failing
    // baseline; recover the guard so later tests still produce evidence.
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Environment keys the bootstrap path reads; captured and restored per test.
pub const GUARDED_KEYS: &[&str] = &[
    "PRIME_AGENT_CODING_AGENT_DIR",
    "PRIME_AGENT_KERNEL_VENV",
    "PRIME_AGENT_KERNEL_PYTHON",
    "PRIME_AGENT_INSTALL_UV",
    "PRIME_AGENT_BOOTSTRAP_KERNEL_ON_INSTALL",
    "PRIME_AGENT_BOOTSTRAP_TOOLS_ON_INSTALL",
    "PRIME_AGENT_MAX_CONCURRENT_KERNEL_BOOTS",
    "PI_BIN_DIR",
    "PI_OFFLINE",
    "PI_PACKAGE_DIR",
    "PARITY_UV_LOG",
    "PARITY_NOOP_PYTHON",
    "PARITY_UV_SLEEP_MS",
    "PARITY_UV_FAIL_PATH",
    "PARITY_CHILD_ROLE",
    "PATH",
];

pub struct EnvGuard {
    saved: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    pub fn capture() -> Self {
        let saved = GUARDED_KEYS
            .iter()
            .map(|key| {
                let value = std::env::var(key).ok();
                (key.to_string(), value)
            })
            .collect();
        EnvGuard { saved }
    }

    pub fn restore(mut self) {
        self.restore_now();
    }

    fn restore_now(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        self.restore_now();
    }
}

pub struct Fixture {
    _temp_root: tempfile::TempDir,
    pub case: String,
    pub root: PathBuf,
    pub venv: PathBuf,
    pub uv_dir: PathBuf,
    pub uv_log: PathBuf,
    pub noop_python: PathBuf,
    pub marker: PathBuf,
}

impl Fixture {
    /// Creates the isolated case tree and writes the recording uv shim.
    /// The no-op python executable is compiled once during lane setup into
    /// PARITY_BOOTSTRAP_TOOLS_DIR and copied per case.
    pub fn new(case: &str) -> Fixture {
        let temp_root = tempfile::Builder::new().prefix(case).tempdir().expect("case root");
        let root = temp_root.path().to_path_buf();
        let uv_dir = root.join("bin");
        std::fs::create_dir_all(&uv_dir).unwrap();
        let noop_python = root.join("python-fixture");
        std::fs::create_dir_all(&noop_python).unwrap();
        let noop_python = noop_python.join("noop_python.exe");
        std::fs::copy(
            tools_dir().join("noop_python.exe"),
            &noop_python,
        )
        .expect("compiled noop python shim must exist under tools/");
        let uv_log = root.join("uv-log.txt");
        let venv = root.join("venv");
        let marker = venv.join(".bootstrap-version");
        let fixture = Fixture {
            _temp_root: temp_root,
            case: case.to_string(),
            root,
            venv,
            uv_dir,
            uv_log,
            noop_python,
            marker,
        };
        std::fs::copy(
            tools_dir().join("uvshim.exe"),
            fixture.uv_exe_path(),
        )
        .expect("compiled uvshim.exe must exist under tools/");
        fixture
    }

    pub fn uv_exe_path(&self) -> PathBuf {
        self.uv_dir.join("uv.exe")
    }

    /// The process environment every in-process bootstrap call needs.
    pub fn apply_env(&self) {
        std::env::set_var("PRIME_AGENT_CODING_AGENT_DIR", self.root.join("agent"));
        std::env::set_var("PATH", format!("{};{}", self.uv_dir.display(), minimal_path()));
        std::env::set_var("PARITY_UV_LOG", self.uv_log.to_string_lossy().to_string());
        std::env::set_var(
            "PARITY_NOOP_PYTHON",
            self.noop_python.to_string_lossy().to_string(),
        );
        std::env::set_var(
            "PRIME_AGENT_KERNEL_VENV",
            self.venv.to_string_lossy().to_string(),
        );
        std::env::remove_var("PRIME_AGENT_KERNEL_PYTHON");
        std::env::remove_var("PRIME_AGENT_INSTALL_UV");
    }

    /// The environment handed to a fresh child process (no inherited parity vars).
    pub fn child_env(&self, role: &str, extra: &[(&str, String)]) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = vec![
            ("PRIME_AGENT_CODING_AGENT_DIR".into(), self.root.join("agent").to_string_lossy().into_owned()),
            ("SystemRoot".into(), "C:\\Windows".into()),
            ("TEMP".into(), self.root.join("tmp").to_string_lossy().to_string()),
            ("TMP".into(), self.root.join("tmp").to_string_lossy().to_string()),
            (
                "PATH".into(),
                format!("{};{}", self.uv_dir.display(), minimal_path()),
            ),
            (
                "PARITY_UV_LOG".into(),
                self.uv_log.to_string_lossy().to_string(),
            ),
            (
                "PARITY_NOOP_PYTHON".into(),
                self.noop_python.to_string_lossy().to_string(),
            ),
            (
                "PRIME_AGENT_KERNEL_VENV".into(),
                self.venv.to_string_lossy().to_string(),
            ),
            ("PARITY_CHILD_ROLE".into(), role.to_string()),
            (
                "PARITY_CHILD_RESULT".into(),
                self.root.join("child-result.json").to_string_lossy().to_string(),
            ),
        ];
        for (key, value) in extra {
            env.push((key.to_string(), value.clone()));
        }
        env
    }

    pub fn uv_lines(&self) -> Vec<String> {
        match std::fs::read_to_string(&self.uv_log) {
            Ok(text) => text
                .lines()
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

pub fn minimal_path() -> String {
    "C:\\Windows\\System32;C:\\Windows;C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\".to_string()
}

/// A positive integer pid that is PROVED dead via tasklist before use.
pub fn spawn_and_reap_dead_pid() -> u32 {
    let mut child = Command::new("cmd.exe")
        .args(["/d", "/s", "/c", "exit", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn probe child");
    let pid = child.id();
    let _ = child.wait();
    assert_dead_pid(pid);
    pid
}

pub fn assert_dead_pid(pid: u32) {
    // tasklist with a PID filter prints an INFO line instead of a CSV row when
    // no process matches; a row contains the quoted pid.
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .expect("tasklist");
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        !text.contains(&format!("\"{pid}\"")),
        "pid {pid} unexpectedly alive; cannot prove staleness"
    );
}

/// Holds a directory handle open with NO sharing so that any delete/rename of
/// the directory fails (Windows-only fixture primitive for G-31).
pub struct HeldDirectory {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

pub fn hold_directory(path: &Path) -> HeldDirectory {
    use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, GENERIC_READ};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_NONE, OPEN_EXISTING,
    };
    let wide: Vec<u16> = path
        .to_string_lossy()
        .replace('/', "\\")
        .encode_utf16()
        .chain([0])
        .collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_NONE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    HeldDirectory { handle }
}

impl Drop for HeldDirectory {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}
