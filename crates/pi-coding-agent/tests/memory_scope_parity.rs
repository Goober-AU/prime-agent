//! T13 — memory scope and persistence (validation suite, owner: memoryscope).
//!
//! Finding under test: H-12 — the memory extension does not resolve its source path
//! the same way the TypeScript reference does. Reference: `memory/service.ts:170-187`
//! (`relative(this.store.project.root, resolve(path))`, `pathToFileURL(path).href`).
//! Rust under test: `crates/pi-coding-agent/src/core/memory/{service,project}.rs`,
//! reached through the real `memory.request` host handler built by
//! `core/tools/ipython.rs::create_memory_host_handlers`.
//!
//! The reference semantics were established by EXECUTING Node's win32 `path` module
//! (see `work/logs/memoryscope/ts-source-oracle.out.json`,
//! `ts-oracle-plain.out.json`, `ts-oracle-git.out.json`,
//! `ts-path-semantics.out.json`, `ts-relative-rule.out.json`): `path.resolve` is
//! lexical and uses the process cwd, `path.relative` compares the root prefix
//! case-insensitively on win32, and `path.resolve` drops dot segments and a trailing
//! separator. `reference_project_path` below reproduces those measured rules.
//!
//! Isolation (V00, `work/CONVENTIONS.md`): every case writes only under
//! `work/state-roots/memoryscope/<case>/` (`PARITY_MEMORY_STATE_ROOT`), with synthetic
//! fixtures. No production pipe, port, profile, session or credential is touched.
//!
//! These are focused controls for the reviewed memory integration (TEST-SPEC T13):
//! no new store, migration or external service is introduced.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_coding_agent::core::kernel::shared::HostRequestHandlers;
use pi_coding_agent::core::memory::evidence::{hash, MemorySource};
use pi_coding_agent::core::memory::project::project_identity;
use pi_coding_agent::core::memory::search::{freshness, MemoryFreshness};
use pi_coding_agent::core::memory::service::{create_memory_host_handlers, MemoryService};
use pi_coding_agent::core::memory::store::MemoryStore;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// V00 isolation helpers
// ---------------------------------------------------------------------------

/// `work/state-roots/memoryscope/<case>` — mandatory; never the default temp dir.
fn case_root(case: &str) -> PathBuf {
    let base = std::env::var("PARITY_MEMORY_STATE_ROOT")
        .expect("PARITY_MEMORY_STATE_ROOT must point at work/state-roots/memoryscope (V00)");
    let path = Path::new(&base).join(case);
    if path.exists() {
        std::fs::remove_dir_all(&path).expect("clean the private case root");
    }
    std::fs::create_dir_all(&path).expect("create the private case root");
    path
}

fn assert_private(root: &Path) {
    let base = std::env::var("PARITY_MEMORY_STATE_ROOT").unwrap();
    let base = Path::new(&base).canonicalize().unwrap();
    let canonical = root.canonicalize().unwrap();
    assert!(
        canonical.starts_with(&base),
        "V00: writable state escaped the private root: {}",
        canonical.display()
    );
    let text = canonical.to_string_lossy().to_lowercase();
    assert!(
        !text.contains(".prime") && !text.contains("optimus-rust-test-20260915"),
        "V00: no private state may live under a production root: {}",
        canonical.display()
    );
}

// ---------------------------------------------------------------------------
// Reference oracle (measured Node win32 semantics)
// ---------------------------------------------------------------------------

/// `path.resolve(path)`: absolute input is normalized lexically; a relative input is
/// resolved against the process cwd (measured: `resolve("src/./nested/../foo.txt")`
/// == `<cwd>/src/foo.txt`).
fn reference_resolve(path: &str) -> String {
    let separators = |value: &str| value.replace('/', "\\");
    let normalized = separators(path);
    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' || normalized.starts_with("\\\\") {
        lexical_normalize(&normalized)
    } else {
        let cwd = std::env::current_dir().unwrap().to_string_lossy().to_string();
        lexical_normalize(&separators(&format!("{}\\{}", cwd.trim_end_matches('\\'), path)))
    }
}

fn lexical_normalize(path: &str) -> String {
    let (prefix, rest) = if path.len() >= 2 && path.as_bytes()[1] == b':' {
        (&path[..2], &path[2..])
    } else if let Some(stripped) = path.strip_prefix("\\\\") {
        let mut pieces = stripped.splitn(3, '\\');
        let server = pieces.next().unwrap_or_default();
        let share = pieces.next().unwrap_or_default();
        let tail = pieces.next().unwrap_or_default();
        return format!(
            "\\\\{}\\{}\\{}",
            server,
            share,
            normalize_segments(tail)
        );
    } else {
        ("", path)
    };
    let joined = normalize_segments(rest);
    format!("{prefix}\\{joined}")
}

fn normalize_segments(rest: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in rest.split(['\\', '/']) {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("\\")
}

fn segments(value: &str) -> Vec<(String, bool)> {
    // (segment, is_root_prefix_marker)
    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut rest = value;
    if let Some(stripped) = rest.strip_prefix("\\\\") {
        let mut pieces = stripped.splitn(3, '\\');
        let server = pieces.next().unwrap_or_default();
        let share = pieces.next().unwrap_or_default();
        parts.push((format!("\\\\{server}"), true));
        parts.push((share.to_string(), true));
        rest = pieces.next().unwrap_or_default();
    } else if rest.len() >= 2 && rest.as_bytes()[1] == b':' {
        let drive = rest[..2].to_lowercase();
        parts.push((drive, true));
        rest = &rest[2..];
    }
    for segment in rest.split(['\\', '/']) {
        if !segment.is_empty() {
            parts.push((segment.to_string(), false));
        }
    }
    parts
}

/// Node win32 `path.relative(from, to)`, measured: the common prefix is compared
/// case-insensitively; when the roots differ the absolute target is returned.
fn reference_relative(from: &str, to: &str) -> String {
    let from_parts = segments(from);
    let to_parts = segments(to);
    let root_matches = match (from_parts.first(), to_parts.first()) {
        (Some((a, _)), Some((b, _))) => a.to_lowercase() == b.to_lowercase(),
        _ => false,
    };
    if !root_matches {
        return to.to_string();
    }
    let mut common = 0;
    while common < from_parts.len()
        && common < to_parts.len()
        && from_parts[common].0.to_lowercase() == to_parts[common].0.to_lowercase()
    {
        common += 1;
    }
    let mut result: Vec<String> = Vec::new();
    for _ in common..from_parts.len() {
        result.push("..".to_string());
    }
    result.extend(to_parts[common..].iter().map(|(value, _)| value.clone()));
    result.join("\\")
}

/// `service.ts:175-179`: `projectPath` is present only when the resolved path is a
/// non-empty, non-`..`, non-absolute relative path inside the project root.
fn reference_project_path(root: &Path, path: &str) -> Option<String> {
    let root = root.to_string_lossy().to_string();
    let resolved = reference_resolve(path);
    let relative = reference_relative(&root, &resolved);
    if relative.is_empty() || relative.starts_with("..") || Path::new(&relative).is_absolute() {
        None
    } else {
        Some(relative.replace('\\', "/"))
    }
}

/// `pathToFileURL(resolve(path)).href` for the same input, normalized for comparison.
fn reference_file_url(root: &Path, path: &str) -> String {
    let _ = root;
    let resolved = reference_resolve(path).replace('\\', "/");
    format!("file:///{}", resolved.trim_start_matches('/'))
}

fn same_windows_path(left: &str, right: &str) -> bool {
    left.replace('\\', "/").trim_end_matches('/').to_lowercase()
        == right.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    root: PathBuf,
    project: PathBuf,
    agent_dir: PathBuf,
}

impl Fixture {
    /// `<case>/project` is the project root; `<case>/agent` is the private agent dir.
    /// `git_backed` chooses which reference root formula applies (`git rev-parse
    /// --show-toplevel` vs `fs.realpathSync(cwd)`), matching `project.ts:41`.
    fn new(case: &str, git_backed: bool) -> Fixture {
        let root = case_root(case);
        assert_private(&root);
        let project = root.join("project");
        let agent_dir = root.join("agent");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(agent_dir.join("sessions")).unwrap();
        if git_backed {
            let status = std::process::Command::new("git")
                .arg("init")
                .arg("-q")
                .arg(&project)
                .status()
                .expect("git init");
            assert!(status.success(), "git init must succeed for the fixture");
        }
        Fixture {
            root,
            project,
            agent_dir,
        }
    }

    /// The reference root form: `git rev-parse --show-toplevel` (always a plain path)
    /// or `realpathSync(cwd)`. `std::fs::canonicalize` may add a verbatim device
    /// prefix that Node never returns, so the fixtures stay git-backed where the
    /// reference root form matters.
    fn reference_root(&self) -> String {
        let output = std::process::Command::new("git")
            .args(["-C", &self.project_str(), "rev-parse", "--show-toplevel"])
            .output();
        match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => self.project_str(),
        }
    }

    fn project_str(&self) -> String {
        self.project.to_string_lossy().to_string()
    }

    fn agent_str(&self) -> String {
        self.agent_dir.to_string_lossy().to_string()
    }

    fn write(&self, relative: &str, body: &str) -> PathBuf {
        let path = self.project.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, body).unwrap();
        path
    }

    fn session_artifact_dir(&self) -> String {
        let dir = self.root.join("session-artifacts").join("sess-h12");
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().to_string()
    }

    fn service(&self) -> MemoryService {
        MemoryService::new(&self.project_str(), &self.agent_str(), None).expect("memory service")
    }

    fn store(&self) -> MemoryStore {
        let identity =
            project_identity(&self.project_str(), &self.agent_str(), None).expect("identity");
        MemoryStore::new(&self.agent_str(), identity).expect("memory store")
    }

    /// The real production host-request map (`ipython.rs:774` builds the same one).
    fn host(&self, session_artifact_dir: Option<String>) -> Arc<HostRequestHandlers> {
        Arc::new(create_memory_host_handlers(
            self.project_str(),
            Some(self.agent_str()),
            session_artifact_dir,
            None,
        ))
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// One `memory.request` action through the real host handler.
async fn request(handlers: &HostRequestHandlers, payload: Value) -> Result<Value, String> {
    let handler = handlers.get("memory.request").expect("memory.request handler");
    match handler(payload).await {
        Ok(value) => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
        Err(error) => Err(error.to_string()),
    }
}

fn apply_entry(handlers: &HostRequestHandlers, event: &str, revision: i64, id: &str, title: &str, content: &str) -> Result<Value, String> {
    block_on(request(
        handlers,
        json!({
            "action": "apply",
            "eventId": event,
            "revision": revision,
            "proposal": {
                "summary": id,
                "rationale": "T13 synthetic fixture",
                "expectedOutcome": "correct project scope",
                "edits": [{"action": "create", "kind": "memory", "id": id, "title": title, "content": content}]
            }
        }),
    ))
}

/// Run `action` with the process cwd temporarily set to `cwd` (the production shape:
/// the CLI applies `--cwd` with `process.chdir(cwd)`, `main.ts:1230-1233`).
fn with_cwd<T>(cwd: &Path, body: impl FnOnce() -> T) -> T {
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(cwd).expect("enter the fixture directory");
    let result = body();
    std::env::set_current_dir(&previous).ok();
    result
}

fn source_action(handlers: &HostRequestHandlers, path: &str) -> Result<Value, String> {
    block_on(request(handlers, json!({"action": "source", "path": path})))
}

// ---------------------------------------------------------------------------
// H-12 — source path resolution
// ---------------------------------------------------------------------------

/// H-12: `source` must resolve its input the way the reference does, so every
/// variant of one in-project resource (relative, absolute, separators, dot
/// segments, case, trailing separator) reports the same non-null `projectPath`.
#[test]
fn h12_source_path_variants_resolve_to_one_project_scope() {
    for git_backed in [false, true] {
        let case = if git_backed {
            "h12-source-variants-git"
        } else {
            "h12-source-variants-plain"
        };
        let fixture = Fixture::new(case, git_backed);
        let file = fixture.write("src/foo.txt", "source content\n");
        let handlers = fixture.host(None);
        let repo = fixture.project_str();

        // (name, input, check_uri). `check_uri` is false for the trailing-separator
        // variant only: the reference builds the URI from the RAW path
        // (`pathToFileURL(path)`, service.ts:183), which keeps a trailing separator,
        // so that one cosmetic difference is not asserted here.
        let variants: Vec<(&str, String, bool)> = vec![
            ("relative_plain", "src/foo.txt".to_string(), true),
            ("relative_dot_segments", "src/./nested/../foo.txt".to_string(), true),
            ("relative_separator_and_case", "SRC\\Foo.TXT".to_string(), true),
            ("relative_forward_slashes", "./src/foo.txt".to_string(), true),
            ("absolute_backslashes", file.to_string_lossy().to_string(), true),
            (
                "absolute_forward_slashes",
                file.to_string_lossy().replace('\\', "/"),
                true,
            ),
            (
                "absolute_dot_segments",
                format!("{repo}\\src\\.\\nested\\..\\foo.txt"),
                true,
            ),
            (
                "absolute_mixed_case",
                format!("{}\\SRC\\FOO.TXT", repo.to_uppercase()),
                true,
            ),
            (
                "absolute_trailing_separator",
                format!("{repo}\\src\\foo.txt\\"),
                false,
            ),
        ];

        let mut observed: Vec<(String, Option<String>)> = Vec::new();
        let reference_root = PathBuf::from(fixture.reference_root());
        let mut failures: Vec<String> = Vec::new();
        with_cwd(&fixture.project, || {
            for (name, value, check_uri) in &variants {
                let expected = reference_project_path(&reference_root, value);
                assert!(
                    expected.is_some(),
                    "fixture variant {name} must be inside the project"
                );
                match source_action(&handlers, value) {
                    Ok(payload) => {
                        let actual = payload
                            .get("projectPath")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        observed.push(((*name).to_string(), actual.clone()));
                        if actual != expected {
                            failures.push(format!(
                                "[{}] {name}: projectPath={actual:?}, reference={expected:?}",
                                if git_backed { "git" } else { "plain" }
                            ));
                        }
                        if !check_uri {
                            continue;
                        }
                        let uri = payload.get("uri").and_then(Value::as_str).unwrap_or_default();
                        let expected_uri = reference_file_url(&reference_root, value);
                        if !same_windows_path(uri, &expected_uri) {
                            failures.push(format!(
                                "[{}] {name}: uri={uri:?}, reference={expected_uri:?}",
                                if git_backed { "git" } else { "plain" }
                            ));
                        }
                    }
                    Err(error) => {
                        observed.push(((*name).to_string(), None));
                        failures.push(format!(
                            "[{}] {name}: error {error}",
                            if git_backed { "git" } else { "plain" }
                        ));
                    }
                }
            }
        });

        eprintln!(
            "H-12 observed projectPath ({} project root): {observed:?}",
            if git_backed { "git-backed" } else { "non-git" }
        );
        assert!(
            failures.is_empty(),
            "source path resolution differs from the reference contract:\n{}",
            failures.join("\n")
        );

        // Every in-project variant names one resource identity.
        let identities: std::collections::BTreeSet<String> = observed
            .iter()
            .filter_map(|(_, value)| value.clone())
            .map(|value| value.rsplit('/').next().unwrap_or_default().to_lowercase())
            .collect();
        assert_eq!(
            identities.len(),
            1,
            "all variants describe the same file, got {identities:?}"
        );
        assert_eq!(
            observed
                .iter()
                .filter(|(_, value)| value.is_none())
                .count(),
            0
        );
    }
}

/// H-12: the reference resolves `source.path` against the PROCESS cwd
/// (`service.ts:175` `resolve(path)`), not against the project root. A relative path
/// that exists in both places must read the process-cwd file, and the project scope
/// must then be reported as absent.
#[test]
fn h12_relative_source_uses_process_cwd() {
    let fixture = Fixture::new("h12-source-process-cwd", true);
    fixture.write("src/foo.txt", "project content\n");
    let elsewhere = fixture.root.join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("src")).unwrap();
    std::fs::write(elsewhere.join("src").join("foo.txt"), "elsewhere content\n").unwrap();
    let handlers = fixture.host(None);

    let reference_root = PathBuf::from(fixture.reference_root());
    let (payload, expected_project_path, expected_uri) = with_cwd(&elsewhere, || {
        let expected_project_path = reference_project_path(&reference_root, "src/foo.txt");
        let expected_uri = reference_file_url(&reference_root, "src/foo.txt");
        let payload = source_action(&handlers, "src/foo.txt")
            .expect("source must resolve a relative path against the process cwd");
        (payload, expected_project_path, expected_uri)
    });
    eprintln!("H-12 process-cwd source payload: {payload}");
    assert!(
        expected_project_path.is_none(),
        "the cwd-based file is outside the project root"
    );
    assert_eq!(
        payload.get("projectPath").cloned().unwrap_or(Value::Null),
        Value::Null,
        "a file outside the project root has no projectPath (reference: {:?})",
        reference_relative(&reference_root.to_string_lossy(), &reference_resolve("src/foo.txt"))
    );
    assert_eq!(
        payload.get("sha256").and_then(Value::as_str),
        Some(hash("elsewhere content\n").as_str()),
        "the action must read the process-cwd file, not the project file"
    );
    let uri = payload.get("uri").and_then(Value::as_str).unwrap_or_default();
    assert!(
        same_windows_path(uri, &expected_uri),
        "uri={uri:?} must be the resolved absolute file URL ({expected_uri:?})"
    );
}

/// H-12 negative control: a traversal and a distinct project never report this
/// project scope, while the source stays usable (absolute `file:` URI + digest).
#[test]
fn h12_source_paths_outside_the_project_never_leak_scope() {
    let fixture = Fixture::new("h12-source-outside-scope", true);
    fixture.write("src/foo.txt", "inside\n");
    let outside = fixture.root.join("outside.txt");
    std::fs::write(&outside, "outside\n").unwrap();
    let other = fixture.root.join("other-project");
    std::fs::create_dir_all(&other).unwrap();
    let other_file = other.join("foo.txt");
    std::fs::write(&other_file, "other\n").unwrap();
    let handlers = fixture.host(None);

    let cases: Vec<(&str, String)> = vec![
        ("relative_traversal", "../outside.txt".to_string()),
        ("relative_traversal_deep", "src/../../outside.txt".to_string()),
        ("absolute_outside", outside.to_string_lossy().to_string()),
        ("other_project", other_file.to_string_lossy().to_string()),
    ];
    let mut failures: Vec<String> = Vec::new();
    with_cwd(&fixture.project, || {
        for (name, value) in &cases {
            assert!(
                reference_project_path(&PathBuf::from(fixture.reference_root()), value).is_none(),
                "fixture case {name} must be outside the project"
            );
            match source_action(&handlers, value) {
                Ok(payload) => {
                    let actual = payload.get("projectPath").cloned().unwrap_or(Value::Null);
                    if actual != Value::Null {
                        failures.push(format!("{name}: projectPath={actual} (expected null)"));
                    }
                    let uri = payload.get("uri").and_then(Value::as_str).unwrap_or_default();
                    if !uri.starts_with("file:") {
                        failures.push(format!("{name}: uri={uri:?} is not a file URL"));
                    }
                }
                Err(error) => failures.push(format!("{name}: error {error}")),
            }
        }
    });
    assert!(failures.is_empty(), "outside-project leak: {failures:#?}");
}

/// H-12 end-to-end: a memory created from a relative in-project source must stay in
/// the model context. `recallMemory` skips `missing`/`stale` sources
/// (`search.ts:117-121`), so a wrong `projectPath` silently drops the note.
#[test]
fn h12_relative_source_memory_survives_recall_freshness() {
    let fixture = Fixture::new("h12-relative-source-recall", true);
    fixture.write("src/foo.txt", "source content\n");
    let handlers = fixture.host(None);

    let source = with_cwd(&fixture.project, || source_action(&handlers, "src/foo.txt"))
        .expect("source payload");
    eprintln!("H-12 relative source payload: {source}");
    assert_eq!(
        source.get("projectPath").and_then(Value::as_str),
        Some("src/foo.txt"),
        "a relative in-project source must record its project path"
    );

    let status = block_on(request(&handlers, json!({"action": "status"}))).expect("status");
    let revision = status.get("revision").and_then(Value::as_i64).unwrap_or(0);
    block_on(request(
        &handlers,
        json!({
            "action": "apply",
            "eventId": "h12_relative_source",
            "revision": revision,
            "sources": [source],
            "proposal": {
                "summary": "h12",
                "rationale": "T13 synthetic fixture",
                "expectedOutcome": "correct project scope",
                "edits": [{"action": "create", "kind": "memory", "id": "h12_note",
                           "title": "Source note", "content": "Remember the relative source"}]
            }
        }),
    ))
    .expect("apply with the file source");

    let recalled = fixture.service().recall("relative source");
    eprintln!(
        "H-12 recall after relative source: ids={:?} chars={} text={:?}",
        recalled.ids, recalled.chars, recalled.text
    );
    assert!(
        recalled.text.contains("Remember the relative source"),
        "the memory must remain recallable through the existing recall path"
    );
}

/// H-12 durability: the recorded `projectPath` must still resolve after a reopen, so
/// the source verifies as `current` and not `unknown`/`missing`.
#[test]
fn h12_recorded_project_path_stays_fresh_after_reopen() {
    for git_backed in [false, true] {
        let fixture = Fixture::new(
            if git_backed {
                "h12-project-path-reopen-git"
            } else {
                "h12-project-path-reopen-plain"
            },
            git_backed,
        );
        fixture.write("src/foo.txt", "source content\n");
        let handlers = fixture.host(None);
        let source = with_cwd(&fixture.project, || source_action(&handlers, "src/foo.txt"))
            .expect("source payload");

        let memory_source: MemorySource =
            serde_json::from_value(source.clone()).expect("source shape");
        let store = fixture.store();
        let fresh = freshness(std::slice::from_ref(&memory_source), Some(&store.project.root));
        eprintln!(
            "H-12 freshness ({} root, projectPath={:?}): {fresh:?}",
            if git_backed { "git" } else { "plain" },
            memory_source.project_path
        );
        assert_eq!(
            fresh,
            MemoryFreshness::Current,
            "an in-project source must verify as current against the project root"
        );
        // Documented residual (NOT part of the H-12 fix): on a non-git root the port
        // stores the `std::fs::canonicalize` verbatim form as the project root, while
        // the reference uses `fs.realpathSync` (a plain path). `project.rs:realpath_sync`
        // is left unchanged here because the root also keys the project alias registry
        // (`path:...`); changing it is a migration question for the integrator. The
        // observable contract above (`projectPath` + freshness) still holds for both
        // root forms, so this is recorded as evidence, not asserted as parity.
        eprintln!(
            "H-12 root form ({} root): verbatim={} root={:?}",
            if git_backed { "git" } else { "plain" },
            store.project.root.starts_with("\\\\?\\"),
            store.project.root
        );
        if !git_backed {
            assert!(
                store.project.root.starts_with("\\\\?\\"),
                "documented residual: a non-git root keeps the canonical verbatim form, got {:?}",
                store.project.root
            );
        } else {
            assert!(
                !store.project.root.starts_with("\\\\?\\"),
                "a git-backed root is the plain `rev-parse` form, got {:?}",
                store.project.root
            );
        }
    }
}

// ---------------------------------------------------------------------------
// T13 scope controls
// ---------------------------------------------------------------------------

/// Project vs session scope: a project entry is visible without a session artifact
/// directory, and a local harness entry is visible only with one (`service.ts:48-52`).
#[test]
fn t13_project_and_session_scope_visibility() {
    let fixture = Fixture::new("t13-project-session-scope", true);
    let session_dir = fixture.session_artifact_dir();
    let project_only = fixture.host(None);
    apply_entry(&project_only, "t13_project_entry", 0, "t13_project", "Project fact", "Project scoped fact")
        .expect("project entry");

    // A session-scope entry is the local harness state under the artifact directory.
    let local_dir = Path::new(&session_dir).join("harness");
    std::fs::create_dir_all(&local_dir).unwrap();
    std::fs::write(
        local_dir.join("harness_state.json"),
        serde_json::to_string_pretty(&json!({
            "schema": 1.0,
            "entries": {"memory": {"t13_session": {
                "id": "t13_session",
                "kind": "memory",
                "title": "Session fact",
                "content": "Session scoped fact",
                "path": "session",
                "reference": {},
                "arguments": {},
                "metadata": {"projectReusable": true},
                "source": "refinement",
                "created_at": "2026-09-16T00:00:00.000Z",
                "updated_at": "2026-09-16T00:00:00.000Z",
                "version": 1
            }}},
            "refinements": []
        }))
        .unwrap(),
    )
    .unwrap();

    let with_session = fixture.host(Some(session_dir));
    let scopes = |value: &Value| -> Vec<(String, String)> {
        value
            .as_array()
            .map(|hits| {
                hits.iter()
                    .map(|hit| {
                        (
                            hit.get("scope").and_then(Value::as_str).unwrap_or("").to_string(),
                            hit.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let with: Value = block_on(request(&with_session, json!({"action": "search", "query": "scoped fact"})))
        .expect("search with a session scope");
    let without: Value = block_on(request(&project_only, json!({"action": "search", "query": "scoped fact"})))
        .expect("search without a session scope");
    eprintln!("T13 scopes with a session dir: {:?}", scopes(&with));
    eprintln!("T13 scopes without a session dir: {:?}", scopes(&without));

    assert!(
        scopes(&with).iter().any(|(scope, title)| scope == "project" && title == "Project fact"),
        "project entries stay visible in every scope"
    );
    assert!(
        scopes(&with).iter().any(|(scope, title)| scope == "session" && title == "Session fact"),
        "a session artifact directory exposes local harness entries"
    );
    assert!(
        !scopes(&without).iter().any(|(scope, _)| scope == "session"),
        "no session artifact directory means no session scope"
    );
    assert_eq!(
        scopes(&without).iter().filter(|(scope, _)| scope == "project").count(),
        1,
        "the project scope keeps exactly its own entry"
    );
}

/// Distinct projects never share scope.
#[test]
fn t13_distinct_projects_do_not_leak_scope() {
    let fixture = Fixture::new("t13-distinct-projects", true);
    let second = fixture.root.join("project-two");
    std::fs::create_dir_all(second.join("src")).unwrap();
    let second_str = second.to_string_lossy().to_string();

    let first = project_identity(&fixture.project_str(), &fixture.agent_str(), None).expect("first");
    let other = project_identity(&second_str, &fixture.agent_str(), None).expect("second");
    assert_ne!(first.id, other.id, "distinct roots get distinct identities");

    let handlers_a = fixture.host(None);
    let handlers_b = Arc::new(create_memory_host_handlers(
        second_str.clone(),
        Some(fixture.agent_str()),
        None,
        None,
    ));
    apply_entry(&handlers_a, "t13_alpha", 0, "t13_alpha", "Alpha secret", "Alpha-only project fact")
        .expect("apply in project A");

    let hits_b: Value = block_on(request(&handlers_b, json!({"action": "search", "query": "alpha-only project fact"})))
        .expect("search in project B");
    eprintln!("T13 project B hits: {hits_b}");
    assert_eq!(
        hits_b.as_array().map(Vec::len),
        Some(0),
        "project B must not see project A memory"
    );
    let hits_a: Value = block_on(request(&handlers_a, json!({"action": "search", "query": "alpha-only project fact"})))
        .expect("search in project A");
    assert!(
        !hits_a.as_array().unwrap_or(&Vec::new()).is_empty(),
        "project A keeps its own memory"
    );
}

/// Provenance: the persisted entry keeps its cited sources, project ID and status.
#[test]
fn t13_entries_keep_provenance_and_project_id() {
    let fixture = Fixture::new("t13-provenance", true);
    let file = fixture.write("src/evidence.txt", "evidence body\n");
    let handlers = fixture.host(None);
    let source = source_action(&handlers, &file.to_string_lossy()).expect("source payload");

    let status = block_on(request(&handlers, json!({"action": "status"}))).expect("status");
    let revision = status.get("revision").and_then(Value::as_i64).unwrap_or(0);
    block_on(request(
        &handlers,
        json!({
            "action": "apply",
            "eventId": "t13_provenance",
            "revision": revision,
            "sources": [source.clone()],
            "proposal": {
                "summary": "t13",
                "rationale": "T13 synthetic fixture",
                "expectedOutcome": "provenance",
                "edits": [{"action": "create", "kind": "memory", "id": "t13_prov",
                           "title": "Provenance", "content": "Fact with a cited source"}]
            }
        }),
    ))
    .expect("apply with a source");

    let store = fixture.store();
    let document = store.read().expect("reopen document");
    let entry = document.entries["memory"].get("t13_prov").expect("persisted entry");
    assert_eq!(
        entry.metadata.get("projectId").and_then(Value::as_str),
        Some(document.memory.project_id.as_str())
    );
    assert_eq!(entry.metadata.get("status").and_then(Value::as_str), Some("current"));
    let sources = entry
        .metadata
        .get("sources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    eprintln!("T13 stored sources: {sources:?}");
    assert_eq!(sources.len(), 1, "the cited source survives serialization");
    assert_eq!(
        sources[0].get("sha256").and_then(Value::as_str),
        source.get("sha256").and_then(Value::as_str)
    );
    assert_eq!(sources[0].get("origin").and_then(Value::as_str), Some("file"));
}

/// Reopen persistence through the real host API.
#[test]
fn t13_memory_survives_reopen_through_the_host_api() {
    let fixture = Fixture::new("t13-reopen-persistence", true);
    let handlers = fixture.host(None);
    apply_entry(&handlers, "t13_reopen", 0, "t13_reopen", "Durable", "Survives a reopen")
        .expect("apply");

    let reopened = fixture.host(None);
    let read: Value = block_on(request(&reopened, json!({"action": "read", "id": "project:memory:t13_reopen"})))
        .expect("read after reopen");
    assert_eq!(
        read["entry"]["title"].as_str(),
        Some("Durable"),
        "read after reopen: {read}"
    );
    assert_eq!(
        read["entry"]["content"].as_str(),
        Some("Survives a reopen")
    );
    assert_eq!(read["scope"].as_str(), Some("project"));
    let history: Value = block_on(request(&reopened, json!({"action": "history"}))).expect("history");
    assert_eq!(history.as_array().map(Vec::len), Some(1));
    let status: Value = block_on(request(&reopened, json!({"action": "status"}))).expect("status");
    assert_eq!(status.get("revision").and_then(Value::as_i64), Some(1));
    assert!(
        same_windows_path(
            status["project"]["root"].as_str().unwrap_or_default(),
            &fixture.reference_root()
        ),
        "reopen resolves the same project root as the reference formula: {status}"
    );
}

/// Learning disabled blocks automatic writes but keeps explicit correction working.
#[test]
fn t13_learning_disabled_blocks_automatic_not_explicit_writes() {
    let fixture = Fixture::new("t13-learning-disabled", true);
    let handlers = fixture.host(None);
    apply_entry(&handlers, "t13_seed", 0, "t13_seed", "Seed", "Seed content").expect("seed");

    let settings = block_on(request(
        &handlers,
        json!({"action": "configure", "settings": {"learning": false}}),
    ))
    .expect("disable learning");
    assert_eq!(settings.get("learning").and_then(Value::as_bool), Some(false));

    let automatic = block_on(request(
        &handlers,
        json!({
            "action": "apply",
            "eventId": "t13_auto",
            "revision": 1,
            "automatic": true,
            "proposal": {
                "summary": "auto",
                "rationale": "T13 synthetic fixture",
                "expectedOutcome": "refused",
                "edits": [{"action": "create", "kind": "memory", "id": "t13_auto",
                           "title": "Automatic", "content": "Should be refused"}]
            }
        }),
    ));
    eprintln!("T13 automatic apply with learning off: {automatic:?}");
    assert!(automatic.is_err(), "automatic learning must be refused while paused");

    let explicit = block_on(request(
        &handlers,
        json!({
            "action": "apply",
            "eventId": "t13_explicit",
            "revision": 1,
            "proposal": {
                "summary": "explicit",
                "rationale": "T13 synthetic fixture",
                "expectedOutcome": "allowed",
                "edits": [{"action": "create", "kind": "memory", "id": "t13_explicit",
                           "title": "Explicit", "content": "Explicit write allowed"}]
            }
        }),
    ));
    assert!(explicit.is_ok(), "explicit writes keep working: {explicit:?}");

    let document = fixture.store().read().unwrap();
    assert!(!document.entries["memory"].contains_key("t13_auto"));
    assert!(document.entries["memory"].contains_key("t13_explicit"));
    assert_eq!(document.memory.revision, 2);
}

/// Correction, backup/restore and rollback controls preserve state.
#[test]
fn t13_correction_and_rollback_controls_preserve_state() {
    let fixture = Fixture::new("t13-rollback", true);
    let handlers = fixture.host(None);
    apply_entry(&handlers, "t13_original", 0, "t13_original", "Original", "Original content")
        .expect("original");

    block_on(request(
        &handlers,
        json!({
            "action": "apply",
            "eventId": "t13_correction",
            "revision": 1,
            "proposal": {
                "summary": "correct",
                "rationale": "T13 synthetic fixture",
                "expectedOutcome": "corrected content",
                "edits": [{"action": "update", "kind": "memory", "id": "t13_original",
                           "title": "Original", "content": "Corrected content"}]
            }
        }),
    ))
    .expect("correction");
    let corrected = block_on(request(
        &handlers,
        json!({"action": "read", "id": "project:memory:t13_original"}),
    ))
    .expect("read corrected");
    assert_eq!(corrected["entry"]["content"].as_str(), Some("Corrected content"));

    let backup = block_on(request(&handlers, json!({"action": "backup"}))).expect("backup");
    apply_entry(&handlers, "t13_third", 2, "t13_third", "Third", "Third content").expect("third");
    let restored = block_on(request(
        &handlers,
        json!({"action": "restore", "id": backup.get("id").and_then(Value::as_str).unwrap()}),
    ))
    .expect("restore");
    // `store.ts` `restore`: `restored.memory.revision = current.revision + 1`.
    assert_eq!(
        restored.get("revision").and_then(Value::as_i64),
        Some(4),
        "restore advances the revision past the current one"
    );
    let document = fixture.store().read().unwrap();
    assert!(document.entries["memory"].contains_key("t13_original"));
    assert!(
        !document.entries["memory"].contains_key("t13_third"),
        "restoring the backup removes the later entry"
    );

    let rolled = block_on(request(
        &handlers,
        json!({"action": "rollback", "id": "t13_correction", "revision": 4}),
    ))
    .expect("rollback the correction");
    eprintln!("T13 rollback result: {rolled}");
    // `AppliedRefinementEdit` flattens the edit fields beside `applied`/`before`/`after`.
    assert_eq!(
        rolled["appliedEdits"][0]["action"].as_str(),
        Some("update"),
        "the correction is replayed as its inverse"
    );
    assert_eq!(rolled["appliedEdits"][0]["applied"].as_bool(), Some(true));
    // The inverse edit replays the pre-correction content as its `after` state.
    assert_eq!(
        rolled["appliedEdits"][0]["after"]["content"].as_str(),
        Some("Original content")
    );
    assert_eq!(
        rolled["appliedEdits"][0]["before"]["content"].as_str(),
        Some("Corrected content")
    );
    assert_eq!(
        rolled["id"].as_str(),
        Some("rollback_t13_correction_4"),
        "the rollback event is recorded under its documented id"
    );
    let after = fixture.store().read().unwrap();
    assert_eq!(
        after.entries["memory"]["t13_original"].content, "Original content",
        "rollback restores the corrected entry"
    );
    assert_eq!(after.memory.revision, 5);

    // A stale rollback is refused rather than silently applied.
    let stale = block_on(request(
        &handlers,
        json!({"action": "rollback", "id": "t13_correction", "revision": 0}),
    ));
    eprintln!("T13 stale rollback: {stale:?}");
    assert!(stale.is_err(), "a stale revision must be refused");
}

/// One existing memory job / sharing API path bounds the declared gap: unknown or
/// unconfigured targets fail truthfully, so no new store is introduced here.
#[test]
fn t13_sharing_and_job_paths_keep_their_declared_contract() {
    let fixture = Fixture::new("t13-sharing-jobs", true);
    let handlers = fixture.host(None);
    apply_entry(&handlers, "t13_shared_seed", 0, "t13_shared_seed", "Shared seed", "Shareable content")
        .expect("seed");

    let unconfigured = block_on(request(
        &handlers,
        json!({"action": "share", "ids": ["t13_shared_seed"]}),
    ));
    eprintln!("T13 share while unconfigured: {unconfigured:?}");
    assert!(unconfigured.is_err(), "sharing requires configuration first");

    let missing = block_on(request(&handlers, json!({"action": "share", "ids": ["absent"]})));
    assert!(missing.is_err(), "an unknown memory cannot be shared");

    let job = block_on(request(
        &handlers,
        json!({"action": "import_prepare",
               "path": fixture.project.join("missing.jsonl").to_string_lossy().to_string()}),
    ));
    eprintln!("T13 import_prepare on a missing file: {job:?}");
    assert!(job.is_err(), "a missing import source must be reported");
}
