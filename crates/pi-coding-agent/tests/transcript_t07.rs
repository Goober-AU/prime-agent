//! T07 — directories, saved-chat catalog and file identity (owner: transcript).
//! Spec: TEST-SPEC.md §T07. Findings B-02, B-04, B-07, B-09, B-10, B-13, G-25.
//!
//! Layer: unit/integration through real production entry points
//! (get_default_session_dir, SessionManager create/list/list_all,
//! find_most_recent_session_for_cwd, session_lease acquire, artifact path
//! info). Env-var cases run in SEPARATE child processes (fresh env caches).
//! All writable state under OPTIMUS_PARITY_STATE_ROOT (V00 isolation).
//!
//! Source-derived oracles (per TEST-SPEC "Common proof standard"):
//! - agent/session dir contract = TS config.ts getAgentDir/getSessionsDir:
//!   PRIME_AGENT_CODING_AGENT_DIR (prefix derived from piConfig "prime-agent"),
//!   PRIME_AGENT_SESSION_DIR ?? PRIME_AGENT_CODING_AGENT_SESSION_DIR, tilde
//!   expanded, default <home>/.prime/agent. PI_* literals are NOT read.
//! - realpath contract = TS utils/atomic-file.ts realpathIfPresentSync:270-297:
//!   plain (non-verbatim) paths; non-ENOENT canonicalize errors are loud.
//! - cwd normalization contract = TS normalizeCwd via path.resolve (collapses
//!   dot segments, dot-dot and trailing separators).
//! - artifact logical path = TS snapshot.ts createArtifactPathInfo with
//!   path.relative (case-insensitive on win32).
//! - saved-roots catalog contract = TS daemon-mode.ts:5909-5918: every
//!   rlmDepth == 0 session is a root; the ?? fallback is dead code.

use std::path::{Path, PathBuf};
use std::process::Command;

use pi_coding_agent::core::session_lease::{
    acquire_session_lease, canonical_session_path, AcquireSessionLeaseError,
    SESSION_LEASES_ENABLED_ENV,
};
use pi_coding_agent::core::session_manager::{
    find_most_recent_session_for_cwd, get_default_session_dir, SessionListCallbacks, SessionManager,
};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// V00 harness
// ---------------------------------------------------------------------------

fn state_root() -> PathBuf {
    static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let root = std::env::var_os("OPTIMUS_PARITY_STATE_ROOT")
        .map(|raw| {
            assert!(!raw.is_empty(), "OPTIMUS_PARITY_STATE_ROOT must not be empty");
            std::path::absolute(raw).expect("absolute state root")
        })
        .unwrap_or_else(|| {
            ROOT.get_or_init(|| tempfile::Builder::new().prefix("optimus-transcript-t07-").tempdir().expect("private state root"))
                .path().to_path_buf()
        });
    let lowered = root.to_string_lossy().to_lowercase();
    assert!(!lowered.contains(".prime"), "state root must not touch .prime");
    std::fs::create_dir_all(&root).expect("create state root");
    assert!(!std::fs::canonicalize(&root).expect("canonical state root").to_string_lossy().to_lowercase().contains(".prime"), "state root must not resolve into .prime");
    root
}

fn case_dir(tag: &str) -> PathBuf {
    let dir = state_root()
        .join(tag)
        .join(format!("{}-{}", std::process::id(), unique_tag()));
    std::fs::create_dir_all(&dir).expect("create case dir");
    dir
}

fn unique_tag() -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static TAG_SEQ: AtomicUsize = AtomicUsize::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        "{}-{}",
        nanos,
        TAG_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

fn write_jsonl(path: &Path, entries: &[Value]) {
    let mut body = String::new();
    for entry in entries {
        body.push_str(&serde_json::to_string(entry).unwrap());
        body.push('\n');
    }
    std::fs::write(path, body).expect("write fixture");
}

fn same_path_loose(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

fn sha256_hex(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn session_header(id: &str, timestamp: &str, cwd: &str, extra: Value) -> Value {
    let mut header = json!({
        "type": "session",
        "version": 3,
        "id": id,
        "timestamp": timestamp,
        "cwd": cwd,
        "rlmDepth": 0,
    });
    if let Some(map) = header.as_object_mut() {
        if let Some(map_extra) = extra.as_object() {
            for (key, value) in map_extra {
                if !value.is_null() {
                    map.insert(key.clone(), value.clone());
                }
            }
        }
    }
    header
}

fn user_message_entry(id: &str, parent: Value, timestamp: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "parentId": parent,
        "timestamp": timestamp,
        "message": {"role": "user", "content": text, "timestamp": 0},
    })
}

// ---------------------------------------------------------------------------
// agent_dir_precedence_matrix (B-02) — separate subprocess per case
// ---------------------------------------------------------------------------

struct EnvCase {
    name: &'static str,
    env: Vec<(&'static str, String)>,
    expected_sessions_dir: PathBuf,
    /// "full": assert the resolved dir equals the expectation AND exercise
    /// create/list/continue agreement (allowed only when the resolved dir is
    /// a private fixture path). "shape": assert the default-brand shape only
    /// and never create sessions.
    mode: &'static str,
}

#[test]
fn agent_dir_precedence_matrix() {
    let dir = case_dir("t07-agent-dir");
    let fixture_home = dir.join("fixture-home");
    std::fs::create_dir_all(&fixture_home).unwrap();
    let agent_a = dir.join("agentA");
    let agent_b = dir.join("agentB");
    let agent_x = dir.join("agentX");
    let session_s = dir.join("sessionS");
    let session_l = dir.join("sessionL");
    let home_agent_sessions = fixture_home.join(".prime").join("agent").join("sessions");

    let home_str = fixture_home.to_string_lossy().to_string();
    let cases = vec![
        EnvCase {
            name: "unset",
            env: vec![],
            expected_sessions_dir: home_agent_sessions.clone(),
            mode: "shape",
        },
        EnvCase {
            name: "prime_only",
            env: vec![("PRIME_AGENT_CODING_AGENT_DIR", agent_a.to_string_lossy().to_string())],
            expected_sessions_dir: agent_a.join("sessions"),
            mode: "full",
        },
        EnvCase {
            name: "prime_session_dir",
            env: vec![
                ("PRIME_AGENT_CODING_AGENT_DIR", agent_a.to_string_lossy().to_string()),
                ("PRIME_AGENT_SESSION_DIR", session_s.to_string_lossy().to_string()),
            ],
            expected_sessions_dir: session_s.clone(),
            mode: "full",
        },
        // PI_* literals are NOT part of the TS contract: with no PRIME var the
        // default directory must be used, never PI_CODING_AGENT_DIR.
        EnvCase {
            name: "pi_only",
            env: vec![("PI_CODING_AGENT_DIR", agent_b.to_string_lossy().to_string())],
            expected_sessions_dir: home_agent_sessions.clone(),
            mode: "shape",
        },
        EnvCase {
            name: "both_equal",
            env: vec![
                ("PRIME_AGENT_CODING_AGENT_DIR", agent_x.to_string_lossy().to_string()),
                ("PI_CODING_AGENT_DIR", agent_x.to_string_lossy().to_string()),
            ],
            expected_sessions_dir: agent_x.join("sessions"),
            mode: "full",
        },
        EnvCase {
            name: "conflicting",
            env: vec![
                ("PRIME_AGENT_CODING_AGENT_DIR", agent_a.to_string_lossy().to_string()),
                ("PI_CODING_AGENT_DIR", agent_b.to_string_lossy().to_string()),
            ],
            expected_sessions_dir: agent_a.join("sessions"),
            mode: "full",
        },
        EnvCase {
            name: "explicit_session_dir",
            env: vec![
                ("PRIME_AGENT_CODING_AGENT_DIR", agent_b.to_string_lossy().to_string()),
                ("PRIME_AGENT_SESSION_DIR", session_s.to_string_lossy().to_string()),
            ],
            expected_sessions_dir: session_s.clone(),
            mode: "full",
        },
        EnvCase {
            name: "tilde",
            env: vec![
                ("PRIME_AGENT_CODING_AGENT_DIR", "~/parity-agent".to_string()),
                // Windows must use USERPROFILE, not this conflicting HOME.
                ("HOME", if cfg!(windows) { dir.join("ignored-home").to_string_lossy().into_owned() } else { home_str.clone() }),
            ],
            expected_sessions_dir: fixture_home.join("parity-agent").join("sessions"),
            mode: "full",
        },
        EnvCase {
            name: "legacy_session_alias",
            env: vec![
                ("PRIME_AGENT_CODING_AGENT_SESSION_DIR", session_l.to_string_lossy().to_string()),
            ],
            expected_sessions_dir: session_l.clone(),
            mode: "full",
        },
    ];

    // Child cwd: a scratch dir inside the state root so any baseline
    // non-expanded literal-tilde fallback stays inside V00 bounds.
    let child_cwd = dir.join("child-cwd");
    std::fs::create_dir_all(&child_cwd).unwrap();

    let mut failures: Vec<String> = Vec::new();
    for case in &cases {
        let output = Command::new(std::env::current_exe().expect("test exe"))
            .current_dir(&child_cwd)
            .arg("--exact")
            .arg("parity_env_child")
            .arg("--nocapture")
            .env_clear()
            .env("SystemRoot", std::env::var("SystemRoot").unwrap_or_default())
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("TEMP", std::env::var("TEMP").unwrap_or_default())
            .env("TMP", std::env::var("TMP").unwrap_or_default())
            .env("HOME", &home_str)
            .env("USERPROFILE", &home_str)
            .env("OPTIMUS_T07_CHILD_CASE", case.name)
            .env("OPTIMUS_T07_CHILD_MODE", case.mode)
            .env("OPTIMUS_T07_FIXTURE_ROOT", &dir)
            .env(
                "OPTIMUS_T07_EXPECTED_SESSIONS",
                case.expected_sessions_dir.to_string_lossy().as_ref(),
            )
            .envs(case.env.iter().map(|(key, value)| (*key, value.as_str())))
            .output()
            .unwrap_or_else(|error| panic!("spawn child for {}: {error}", case.name));
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            failures.push(format!(
                "B-02 env case {} failed (rc={:?})\nstdout:\n{}\nstderr:\n{}",
                case.name,
                output.status.code(),
                stdout,
                stderr
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "B-02 evidence (baseline deviations from the TS PRIME_AGENT_* contract):\n{}",
        failures.join("\n")
    )
}

/// Child-side runner: one env case per process so the package.json/env caches
/// and the process environment cannot leak between cases.
#[test]
fn parity_env_child() {
    let case = match std::env::var("OPTIMUS_T07_CHILD_CASE") {
        Ok(case) => case,
        Err(_) => return, // not a child run
    };
    if case == "home-fallback" {
        // Pure path lookup only: never create anything in the real profile.
        let expected_home = dirs::home_dir().expect("OS home fallback");
        assert_eq!(
            Path::new(&pi_coding_agent::config::expand_tilde_path("~", None)),
            expected_home.as_path(),
            "missing or empty USERPROFILE must preserve the OS home fallback"
        );
        return;
    }
    let mode = std::env::var("OPTIMUS_T07_CHILD_MODE").unwrap_or_else(|_| "full".to_string());
    let expected = PathBuf::from(std::env::var("OPTIMUS_T07_EXPECTED_SESSIONS").expect("expected"));
    let cwd = "C:\\";

    // Resolve without creating directories before checking fixture containment.
    // get_default_session_dir() creates its target and must only run afterward.
    let resolved = pi_coding_agent::config::get_sessions_dir(None);

    if mode == "shape" {
        // Assert the default brand shape without creating any directories.
        let lowered = resolved.to_lowercase();
        let normalized = lowered.replace('/', "\\");
        assert!(
            normalized.ends_with(".prime\\agent\\sessions"),
            "default sessions dir must keep the .prime agent brand (case {case}), got {resolved}"
        );
        return;
    }

    assert_eq!(
        Path::new(&resolved),
        Path::new(&expected),
        "resolved session dir must follow the TS PRIME_AGENT_* contract (case {case}): got {resolved}, want {}",
        expected.display()
    );
    let fixture_root = PathBuf::from(std::env::var_os("OPTIMUS_T07_FIXTURE_ROOT").expect("private fixture root"));
    assert!(
        Path::new(&resolved).starts_with(&fixture_root),
        "resolved dir must be private before creating sessions (case {case}): {resolved}"
    );
    assert_eq!(get_default_session_dir(cwd, None), resolved);

    // create/list/continue agreement: the created transcript must land in the
    // same resolved directory and be found by the resume scan.
    let mut manager = SessionManager::create(cwd, None).expect("create session");
    let file = manager.get_session_file().expect("session file");
    let parent = Path::new(&file).parent().expect("session file parent");
    assert!(
        same_path_loose(parent, Path::new(&resolved)),
        "created file must live in the resolved sessions dir (case {case}): {file}"
    );
    let user = pi_agent_core::types::AgentMessage::from(pi_ai::types::UserMessage::new(
        pi_ai::types::UserContent::Text("history".to_string()),
        1,
    ));
    manager.append_message(user).expect("append user");
    let assistant = pi_agent_core::types::AgentMessage::from(
        pi_ai::types::AssistantMessage {
            timestamp: 2,
            ..Default::default()
        },
    );
    manager.append_message(assistant).expect("append assistant");
    manager.flush_now();
    let found = find_most_recent_session_for_cwd(&resolved, cwd);
    assert!(
        found.is_some(),
        "resume scan must find the created session (case {case})"
    );

    // V00: the exercised dir must be the private fixture path.
    assert!(
        Path::new(&resolved).starts_with(&fixture_root),
        "resolved dir must be private (case {case}): {resolved}"
    );
}

#[cfg(windows)]
#[test]
fn home_fallback_without_userprofile_stays_available() {
    for empty in [false, true] {
        let mut command = Command::new(std::env::current_exe().expect("test exe"));
        command
            .arg("--exact")
            .arg("parity_env_child")
            .arg("--nocapture")
            .env("OPTIMUS_T07_CHILD_CASE", "home-fallback")
            .env_remove("USERPROFILE");
        if empty {
            command.env("USERPROFILE", "");
        }
        let output = command.output().expect("spawn fallback child");
        assert!(
            output.status.success(),
            "OS fallback failed (empty={empty}): {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

// ---------------------------------------------------------------------------
// windows_paths_resolve_to_one_identity (B-09 + B-13 + G-25)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn windows_paths_resolve_to_one_identity() {
    let dir = case_dir("t07-paths");
    let mut failures: Vec<String> = Vec::new();

    // --- Part 1 (B-09): canonical session path / lease key identity ---
    {
        let case_dir = dir.join("lease-identity");
        std::fs::create_dir_all(&case_dir).unwrap();
        let file = case_dir.join("Session.File.jsonl");
        write_jsonl(
            file.as_path(),
            &[json!({
                "type": "session", "version": 3, "id": "lease-1",
                "timestamp": "2026-09-16T00:00:00.000Z",
                "cwd": case_dir.to_string_lossy(), "rlmDepth": 0,
            })],
        );
        let file_str = file.to_string_lossy().to_string();

        // Contract: the canonical session path is the TS-interchangeable plain
        // realpath (no \\?\ verbatim prefix) and the lease key derives from it.
        let canonical = canonical_session_path(&file_str);
        if canonical.starts_with("\\\\?\\") {
            failures.push(format!(
                "B-09: canonical session path must be plain (TS realpath), got {canonical}"
            ));
        }
        if !Path::new(&canonical).exists() {
            failures.push("B-09: canonical path must point at the same existing file".to_string());
        }
        assert!(
            Path::new(&canonical).exists(),
            "canonical path must point at the same existing file"
        );

        let agent_dir = case_dir.join("leases-agent");
        let lease_env = vec![(SESSION_LEASES_ENABLED_ENV.to_string(), "1".to_string())];
        let expected_key = format!("{}.lock", sha256_hex(&canonical));

        // First acquisition of the file (still held below).
        let first = acquire_session_lease(
            Some(&file_str),
            agent_dir.to_string_lossy().as_ref(),
            Some(lease_env.as_slice()),
        )
        .expect("first lease must not error")
        .expect("leases enabled");
        assert_eq!(
            first.session_path, canonical,
            "held lease must record the canonical (TS-realpath) session path"
        );

        // The lease lives under sha256(<plain canonical>).lock — checked
        // against the real on-disk directory (non-circular: sha256 computed
        // independently here).
        let lease_root = agent_dir.join("session-leases");
        let mut lock_names = std::fs::read_dir(&lease_root)
            .expect("lease root")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        lock_names.sort();
        if lock_names != vec![expected_key.clone()] {
            failures.push(format!(
                "B-09: lease key must be sha256 of the plain TS-realpath form (got {lock_names:?}, want {expected_key})"
            ));
        }

        // All textual variants of the same file must canonicalize to ONE
        // identity (drive case, separators, verbatim prefix, dot, dot-dot).
        let file_str = file.to_string_lossy().to_string();
        let variants: Vec<String> = vec![
            file_str.clone(),
            file_str.replace('\\', "/"),
            format!("\\\\?\\{}", file_str.replace('/', "\\")),
            format!("{}{}", file_str[..1].to_ascii_lowercase(), &file_str[1..]),
            case_dir.join(".").join("Session.File.jsonl").to_string_lossy().to_string(),
            case_dir
                .join("sub..")
                .join("..")
                .join("Session.File.jsonl")
                .to_string_lossy()
                .to_string(),
        ];
        for (index, variant) in variants.iter().enumerate() {
            if canonical_session_path(variant) != canonical {
                failures.push(format!(
                    "B-09: path variant {index} must resolve to one identity: {variant}"
                ));
            }
        }

        // One identity: a second lease on the SAME file via the verbatim
        // prefix variant must be rejected while held.
        let second = acquire_session_lease(
            Some(&variants[2]),
            agent_dir.to_string_lossy().as_ref(),
            Some(lease_env.as_slice()),
        );
        if !matches!(second, Err(AcquireSessionLeaseError::AlreadyActive(_))) {
            failures.push(format!(
                "B-09: second lease on the same file via a path variant must be rejected, got {:?}",
                second.map(|lease| lease.map(|lease| lease.session_path))
            ));
        }

        // Distinct files must remain distinct.
        let other = case_dir.join("Other.Session.jsonl");
        write_jsonl(
            other.as_path(),
            &[json!({
                "type": "session", "version": 3, "id": "lease-2",
                "timestamp": "2026-09-16T00:00:00.000Z",
                "cwd": case_dir.to_string_lossy(), "rlmDepth": 0,
            })],
        );
        let other_canonical = canonical_session_path(other.to_string_lossy().as_ref());
        assert_ne!(canonical, other_canonical, "distinct files keep distinct identities");
        let third = acquire_session_lease(
            Some(other.to_string_lossy().as_ref()),
            agent_dir.to_string_lossy().as_ref(),
            Some(lease_env.as_slice()),
        );
        if !matches!(third, Ok(Some(_))) {
            let third_error = third.as_ref().err().map(|e| e.to_string());
            failures.push(format!(
                "B-09: a different file must acquire its own lease, got {:?}",
                third_error
            ));
        }
        drop(third);
        drop(first);
    }

    // --- Part 2 (B-13): cwd comparisons collapse dot segments and separators ---
    {
        let case_dir = dir.join("cwd-normalization");
        std::fs::create_dir_all(&case_dir).unwrap();
        let sessions = case_dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        // Header cwd recorded with a dot-dot segment and a trailing separator;
        // path.resolve collapses it back to the case dir (the segment "inner"
        // is lexical only and need not exist on disk).
        let raw_cwd = format!("{}\\inner\\..\\", case_dir.to_string_lossy());
        let file = sessions.join("b.jsonl");
        write_jsonl(
            file.as_path(),
            &[
                session_header("s-b13", "2026-09-16T00:00:00.000Z", &raw_cwd, json!({})),
                user_message_entry("m1", Value::Null, "2026-09-16T00:00:00.000Z", "hello"),
                json!({
                    "type": "message", "id": "m2", "parentId": "m1",
                    "timestamp": "2026-09-16T00:00:01.000Z",
                    "message": {"role": "assistant", "content": [], "timestamp": 1},
                }),
            ],
        );

        // TS path.resolve collapses the dot-dot and trailing separator, so the
        // plain cwd must resolve to one identity with the recorded variant.
        let found = find_most_recent_session_for_cwd(
            sessions.to_string_lossy().as_ref(),
            case_dir.to_string_lossy().as_ref(),
        );
        if found.as_deref() != Some(file.to_string_lossy().as_ref()) {
            failures.push(format!(
                "B-13: recorded '..'/trailing-sep cwd must match the plain cwd, got {found:?}"
            ));
        }

        // list(cwd) agrees with continue: the recorded variant cwd must match
        // the plain query cwd through the same normalization contract.
        let listed = SessionManager::list(
            case_dir.to_string_lossy().as_ref(),
            Some(sessions.to_string_lossy().as_ref()),
            None,
        )
        .await;
        let listed_paths: Vec<String> = listed.iter().map(|info| info.path.clone()).collect();
        if listed_paths != vec![file.to_string_lossy().to_string()] {
            failures.push(format!(
                "B-13: list(cwd) must match the recorded '..'/trailing-sep cwd, got {listed_paths:?}"
            ));
        }
    }

    // --- Part 3 (G-25): artifact logical path is case-insensitive on Windows ---
    {
        let (logical, relative) =
            pi_coding_agent::modes::agent_connection::snapshot::create_artifact_path_info(
                "c:/work/sub/a.md",
                "C:/Work",
            );
        if logical != "sub/a.md" {
            failures.push(format!(
                "G-25: artifact logical path must be cwd-relative (case-insensitive on win32), got {logical}"
            ));
        }
        if relative.as_deref() != Some("sub/a.md") {
            failures.push(format!("G-25: relative artifact path must survive, got {relative:?}"));
        }

        // Control: same-case input stays relative.
        let (logical2, relative2) =
            pi_coding_agent::modes::agent_connection::snapshot::create_artifact_path_info(
                "C:/Work/sub/b.md",
                "C:/Work",
            );
        if logical2 != "sub/b.md" || relative2.as_deref() != Some("sub/b.md") {
            failures.push(format!(
                "G-25: same-case control must stay relative, got {logical2}/{relative2:?}"
            ));
        }

        // Distinct files must remain distinct.
        if logical == logical2 {
            failures.push("G-25: distinct files must remain distinct".to_string());
        }
    }

    // --- Part 4 (B-09 residual): canonicalization failures are loud, never a
    // silent "nonexistent" fallback (TS realpathIfPresentSync throws on any
    // non-ENOENT error) ---
    {
        let denied_parent = dir.join("denied-parent");
        std::fs::create_dir_all(&denied_parent).unwrap();
        let denied_file = denied_parent.join("s.jsonl");
        write_jsonl(
            denied_file.as_path(),
            &[json!({
                "type": "session", "version": 3, "id": "lease-3",
                "timestamp": "2026-09-16T00:00:00.000Z",
                "cwd": denied_parent.to_string_lossy(), "rlmDepth": 0,
            })],
        );
        let denied_str = denied_file.to_string_lossy().to_string();

        // Strip all inherited ACEs from the parent: an empty DACL denies
        // canonicalization of the subtree for everyone (including this
        // process). Best effort only — the sub-case asserts only when the
        // deny is actually in effect (non-ENOENT canonicalize error).
        let deny = Command::new("icacls")
            .arg(denied_parent.to_string_lossy().as_ref())
            .arg("/inheritance:r")
            .output();
        let blocked = match std::fs::canonicalize(&denied_file) {
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
            Ok(_) => false,
        };
        if let Ok(out) = &deny {
            if !out.status.success() {
                eprintln!("access-denied sub-case skipped: icacls deny failed");
            }
        }
        if blocked && deny.as_ref().map(|out| out.status.success()).unwrap_or(false) {
            let agent_dir = dir.join("denied-leases-agent");
            let lease_env = vec![(SESSION_LEASES_ENABLED_ENV.to_string(), "1".to_string())];
            let outcome = acquire_session_lease(
                Some(&denied_str),
                agent_dir.to_string_lossy().as_ref(),
                Some(lease_env.as_slice()),
            );
            if !outcome.is_err() {
                failures.push(format!(
                    "B-09: lease acquisition must be loud when canonicalization fails with non-ENOENT (TS realpathIfPresentSync throws); got {:?}",
                    outcome.map(|lease| lease.map(|lease| lease.session_path))
                ));
            }
        } else {
            eprintln!("access-denied sub-case skipped: deny did not block canonicalization");
        }
        // Best-effort restore so later cleanup can remove the case dir.
        let _ = Command::new("icacls")
            .arg(denied_parent.to_string_lossy().as_ref())
            .arg("/reset")
            .output();
        let _ = denied_str;
    }
    assert!(
        failures.is_empty(),
        "B-09/B-13/G-25 evidence (baseline deviations from the TS identity contracts):\n{}",
        failures.join("\n")
    )
}

// ---------------------------------------------------------------------------
// catalog_detects_atomic_replacement (B-04)
// ---------------------------------------------------------------------------

/// TS caches unchanged identity/size/mtime and resumes append-only growth.
/// A real atomic replacement must invalidate that cache even at the same
/// size and mtime; an in-place rewrite preserving all three is outside the
/// append-or-rename writer contract (TS session-manager.ts:1396-1399).
#[tokio::test]
async fn catalog_detects_atomic_replacement() {
    let dir = case_dir("t07-replacement");
    let mut failures: Vec<String> = Vec::new();
    let sessions = dir.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let cwd = dir.to_string_lossy().to_string();
    let file = sessions.join("r.jsonl");

    // Helper: suffix-preserving same-length content with distinct first
    // message text (suffix keeps any trailing partial-line handling stable).
    let suffix = "\"timestamp\":1}}\n";
    let make_body = |first_text: &str| -> String {
        let mut body = String::new();
        body.push_str(
            &serde_json::to_string(&session_header(
                "s-b04",
                "2026-09-16T00:00:00.000Z",
                &cwd,
                json!({}),
            ))
            .unwrap(),
        );
        body.push('\n');
        body.push_str(
            &serde_json::to_string(&json!({
                "type": "message", "id": "m1", "parentId": null,
                "timestamp": "2026-09-16T00:00:00.000Z",
                "message": {"role": "user", "content": first_text, "timestamp": 0},
            }))
            .unwrap(),
        );
        body.push('\n');
        // Pad so both variants share the exact byte length AND suffix.
        while body.len() + suffix.len() < 4096 {
            body.push_str(
                &serde_json::to_string(&json!({
                    "type": "message", "id": "pad", "parentId": null,
                    "timestamp": "2026-09-16T00:00:00.000Z",
                    "message": {"role": "user", "content": "pad", "timestamp": 0},
                }))
                .unwrap(),
            );
            body.push('\n');
        }
        body.push_str(suffix);
        body
    };

    let body_a = make_body("FIRST-CONTENT-");
    std::fs::write(&file, &body_a).expect("write A");
    let path_str = file.to_string_lossy().to_string();
    let initial = pi_coding_agent::core::session_manager::read_session_info(&path_str)
        .await
        .expect("initial scan");
    assert_eq!(initial.first_message, "FIRST-CONTENT-", "initial content must be read");
    // Capture A's mtime AFTER the first scan: the scan-store cached exactly
    // this mtime, so only the replacement's distinct identity invalidates it.
    let mtime_a = std::fs::metadata(&file).expect("metadata A").modified().expect("mtime A");

    // Atomic replacement: same byte length, same suffix, different content.
    let body_b = make_body("SECOND-CONTENT");
    assert_eq!(body_b.len(), body_a.len(), "replacement must be byte-length equal");
    assert!(body_b.ends_with(suffix) && body_a.ends_with(suffix));
    let replacement = sessions.join("r-replacement.jsonl");
    std::fs::write(&replacement, &body_b).expect("write replacement B");
    {
        let handle = std::fs::OpenOptions::new()
            .write(true)
            .open(&replacement)
            .expect("open B for mtime restore");
        handle.set_modified(mtime_a).expect("restore mtime");
    }
    std::fs::rename(&replacement, &file).expect("atomically replace A with B");

    let rescan = pi_coding_agent::core::session_manager::read_session_info(&path_str)
        .await
        .expect("rescan");
    if rescan.first_message != "SECOND-CONTENT" {
        failures.push(format!(
            "B-04: a different file identity must invalidate same-size, same-mtime cached content, got {:?}",
            rescan.first_message
        ));
    }

    // Control: an append (size grows) is always detected and the header/first
    // message survive the resume path unchanged.
    let mut grown = body_b.clone();
    grown.push_str(
        &serde_json::to_string(&json!({
            "type": "message", "id": "m9", "parentId": null,
            "timestamp": "2026-09-16T00:00:02.000Z",
            "message": {"role": "user", "content": "GROWN", "timestamp": 2},
        }))
        .unwrap(),
    );
    grown.push('\n');
    std::fs::write(&file, &grown).expect("write grown");
    let grown_scan = pi_coding_agent::core::session_manager::read_session_info(&path_str)
        .await
        .expect("grown scan");
    if grown_scan.first_message != "SECOND-CONTENT" {
        failures.push(format!(
            "append control must keep the first message stable, got {:?}",
            grown_scan.first_message
        ));
    }
    assert!(
        failures.is_empty(),
        "B-04 evidence (baseline deviations from the TS catalog refresh contract):\n{}",
        failures.join("\n")
    )
}

/// An unchanged Windows catalog entry must reuse the cached snapshot without
/// reopening transcript data. Denying data reads while allowing metadata
/// attributes is a deterministic cache-hit oracle, not a timing threshold.
#[cfg(windows)]
#[tokio::test]
async fn catalog_reuses_unchanged_windows_file_without_rereading() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_WRITE};

    let dir = case_dir("t07-catalog-cache");
    let file = dir.join("cached.jsonl");
    write_jsonl(
        &file,
        &[
            session_header("cache-1", "2026-09-16T00:00:00.000Z", &dir.to_string_lossy(), json!({})),
            user_message_entry("m1", Value::Null, "2026-09-16T00:00:00.000Z", "cached content"),
        ],
    );
    let path = file.to_string_lossy().into_owned();
    let first = pi_coding_agent::core::session_manager::read_session_info(&path)
        .await.expect("initial catalog scan");
    let hold = std::fs::OpenOptions::new()
        .write(true)
        .share_mode(FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(&file).expect("deny transcript data reads");
    assert!(std::fs::File::open(&file).is_err(), "fixture must actually deny data reads");

    let cached = pi_coding_agent::core::session_manager::read_session_info(&path).await;
    drop(hold);
    let cached = cached.expect("unchanged catalog entry must not reread transcript data");
    assert_eq!(cached.first_message, first.first_message);
    assert_eq!(cached.message_count, first.message_count);
}

// ---------------------------------------------------------------------------
// roots_children_and_forks_remain_visible_correctly (B-07)
// ---------------------------------------------------------------------------

/// Saved-list API (SessionManager::list_all) is the catalog's input: every
/// rlmDepth == 0 session is a root — including depth-zero forks that carry
/// parentSessionPath (the TS `?? this.leafId` fallback is dead code). Nested
/// RLM children (rlmDepth != 0) belong to the parent view, never to the roots
/// list. The daemon catalog filter itself is exercised through the seam when
/// the repair lands (candidate lane).
#[tokio::test]
async fn roots_children_and_forks_remain_visible_correctly() {
    let dir = case_dir("t07-roots");
    let sessions = dir.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let cwd = dir.to_string_lossy().to_string();

    let root_main = sessions.join("root-main.jsonl");
    write_jsonl(
        root_main.as_path(),
        &[
            session_header("root-1", "2026-09-16T00:00:00.000Z", &cwd, json!({})),
            user_message_entry("r1", Value::Null, "2026-09-16T00:00:00.000Z", "main root"),
        ],
    );

    // Depth-zero fork: a fork OF a main session, still rlmDepth 0.
    let fork = sessions.join("fork-depth0.jsonl");
    write_jsonl(
        fork.as_path(),
        &[
            session_header(
                "fork-1",
                "2026-09-16T00:00:01.000Z",
                &cwd,
                json!({"parentSession": root_main.to_string_lossy()}),
            ),
            user_message_entry("f1", Value::Null, "2026-09-16T00:00:01.000Z", "depth0 fork"),
        ],
    );

    // Nested RLM child (depth 1) of the fork.
    let child = sessions.join("nested-child.jsonl");
    write_jsonl(
        child.as_path(),
        &[
            session_header(
                "child-1",
                "2026-09-16T00:00:02.000Z",
                &cwd,
                json!({"rlmDepth": 1, "parentSession": fork.to_string_lossy()}),
            ),
            user_message_entry("c1", Value::Null, "2026-09-16T00:00:02.000Z", "nested child"),
        ],
    );

    // Passivated child: depth 1, parent linkage retained.
    let passivated = sessions.join("passivated-child.jsonl");
    write_jsonl(
        passivated.as_path(),
        &[
            session_header(
                "child-2",
                "2026-09-16T00:00:03.000Z",
                &cwd,
                json!({"rlmDepth": 1, "parentSession": fork.to_string_lossy()}),
            ),
            user_message_entry("c2", Value::Null, "2026-09-16T00:00:03.000Z", "passivated child"),
        ],
    );

    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = seen.clone();
    let callbacks = SessionListCallbacks {
        on_progress: None,
        on_session: Some(Box::new(move |info: &pi_coding_agent::core::session_manager::SessionInfo| {
            sink.lock().unwrap().push(info.path.clone());
        })),
    };
    let all = SessionManager::list_all(Some(&callbacks), Some(sessions.to_string_lossy().as_ref())).await;
    let mut expected = vec![
        root_main.to_string_lossy().to_string(),
        fork.to_string_lossy().to_string(),
        child.to_string_lossy().to_string(),
        passivated.to_string_lossy().to_string(),
    ];
    expected.sort();
    let mut seen_sorted = seen.lock().unwrap().clone();
    seen_sorted.sort();
    assert_eq!(
        seen_sorted, expected,
        "the saved-list API must surface roots, depth-zero forks and nested children (comprehensive input)"
    );

    // Premise for the catalog defect: the depth-zero fork is indistinguishable
    // from a plain root in the saved list — its rlm_depth is 0 and it carries a
    // parentSessionPath. Per the TS catalog contract it MUST stay visible as a
    // root; the daemon-side filter is pinned through the seam on the candidate.
    let fork_info = all
        .iter()
        .find(|info| info.path == fork.to_string_lossy())
        .expect("fork present in list_all");
    assert_eq!(fork_info.rlm_depth, 0, "fork fixture must carry rlmDepth 0");
    assert_eq!(
        fork_info.parent_session_path.as_deref(),
        Some(root_main.to_string_lossy().as_ref()),
        "fork fixture must carry its parentSessionPath"
    );
    let child_info = all
        .iter()
        .find(|info| info.path == child.to_string_lossy())
        .expect("nested child present in list_all");
    assert_eq!(child_info.rlm_depth, 1, "nested child fixture must carry rlmDepth 1");

    // Browser/catalog rule (TS contract): roots = every rlmDepth == 0 session.
    let ts_roots: Vec<String> = all
        .iter()
        .filter(|info| info.rlm_depth == 0)
        .map(|info| info.path.clone())
        .collect();
    assert_eq!(
        ts_roots.len(),
        2,
        "exactly the main root and the depth-zero fork are roots"
    );

    // The extracted catalog classifier must keep the fork and reject children.
    let mut got = saved_roots_classifier_via_seam(&all);
    got.sort();
    let mut want = expected_root_paths(&root_main, &fork);
    want.sort();
    assert_eq!(
        got, want,
        "catalog classifier must follow the TS rlmDepth==0 root rule"
    );
}

fn expected_root_paths(root_main: &Path, fork: &Path) -> Vec<String> {
    vec![root_main.to_string_lossy().to_string(), fork.to_string_lossy().to_string()]
}

fn saved_roots_classifier_via_seam(
    sessions: &[pi_coding_agent::core::session_manager::SessionInfo],
) -> Vec<String> {
    let active_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    sessions
        .iter()
        .filter(|info| {
            pi_coding_agent::modes::daemon::daemon_mode::saved_roots_classifier(
                info.rlm_depth,
                &active_paths,
                &info.path,
            )
        })
        .map(|info| info.path.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// scoped_events_match_scoped_result (B-10)
// ---------------------------------------------------------------------------

/// SessionManager::list(cwd, ...) filters its RESULT to the cwd, and the TS
/// contract routes the SAME subset through onSession events. The baseline
/// forwards the callbacks unfiltered, so off-cwd sessions leak into the event
/// stream. list_all stays comprehensive (control).
#[tokio::test]
async fn scoped_events_match_scoped_result() {
    let dir = case_dir("t07-scoped-events");
    let mut failures: Vec<String> = Vec::new();
    let sessions = dir.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let cwd_a = dir.join("project-a");
    let cwd_b = dir.join("project-b");
    std::fs::create_dir_all(&cwd_a).unwrap();
    std::fs::create_dir_all(&cwd_b).unwrap();
    let cwd_a_str = cwd_a.to_string_lossy().to_string();
    let cwd_b_str = cwd_b.to_string_lossy().to_string();

    let file_a = sessions.join("a.jsonl");
    write_jsonl(
        file_a.as_path(),
        &[
            session_header("sc-a", "2026-09-16T00:00:00.000Z", &cwd_a_str, json!({})),
            user_message_entry("a1", Value::Null, "2026-09-16T00:00:00.000Z", "in cwd A"),
        ],
    );
    let file_b = sessions.join("b.jsonl");
    write_jsonl(
        file_b.as_path(),
        &[
            session_header("sc-b", "2026-09-16T00:00:01.000Z", &cwd_b_str, json!({})),
            user_message_entry("b1", Value::Null, "2026-09-16T00:00:01.000Z", "in cwd B"),
        ],
    );

    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = seen.clone();
    let callbacks = SessionListCallbacks {
        on_progress: None,
        on_session: Some(Box::new(move |info: &pi_coding_agent::core::session_manager::SessionInfo| {
            sink.lock().unwrap().push(info.path.clone());
        })),
    };

    let result = SessionManager::list(
        &cwd_a_str,
        Some(sessions.to_string_lossy().as_ref()),
        Some(callbacks),
    )
    .await;

    // Result scope: exactly the cwd-A session.
    let result_paths: Vec<String> = result.iter().map(|info| info.path.clone()).collect();
    assert_eq!(
        result_paths,
        vec![file_a.to_string_lossy().to_string()],
        "scoped result must contain only the cwd-A session"
    );

    // Event scope: the SAME subset — no off-cwd leak (baseline leaks file_b).
    let event_paths = seen.lock().unwrap().clone();
    if event_paths != result_paths {
        failures.push(format!(
            "B-10: onSession events must carry the same scope as the result, got {event_paths:?} vs {result_paths:?}"
        ));
    }

    // Control: all-scope listing stays comprehensive.
    let seen_all = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let sink_all = seen_all.clone();
    let callbacks_all = SessionListCallbacks {
        on_progress: None,
        on_session: Some(Box::new(move |_info| {
            *sink_all.lock().unwrap() += 1;
        })),
    };
    let everything =
        SessionManager::list_all(Some(&callbacks_all), Some(sessions.to_string_lossy().as_ref())).await;
    if everything.len() != 2 || *seen_all.lock().unwrap() != 2 {
        failures.push(format!(
            "list_all control must remain comprehensive, got {} items / {} events",
            everything.len(), *seen_all.lock().unwrap()
        ));
    }
    assert!(
        failures.is_empty(),
        "B-10 evidence (baseline deviations from the TS event-scope contract):\n{}",
        failures.join("\n")
    )
}
