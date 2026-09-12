//! Port of packages/coding-agent/src/modes/daemon/daemon-runtime-identity.ts

use std::path::{Path, PathBuf};

use crate::modes::daemon::daemon_protocol::DaemonRuntimeIdentity;

pub const PRIME_AGENT_BUILD_ID_ENV: &str = "PRIME_AGENT_BUILD_ID";
pub const PRIME_AGENT_LAUNCHER_PATH_ENV: &str = "PRIME_AGENT_LAUNCHER_PATH";

/// `declare const __PI_BUILD_ID__` - a bundler define, so it is absent (None)
/// unless the build supplied it, exactly like `typeof __PI_BUILD_ID__ === "undefined"`.
fn bundled_build_id() -> Option<String> {
    option_env!("PI_BUILD_ID").map(str::to_string)
}

/// Node's `path.resolve`, lexically (absolute input wins, otherwise CWD-relative).
/// `utils/paths.ts` has no `resolve` equivalent, so this stays private plumbing.
fn resolve_path(path: &str) -> String {
    let candidate = Path::new(path);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(candidate)
    };
    normalize_lexically(&joined)
}

/// Collapse `.` and `..` segments the way Node's `path.resolve` does.
fn normalize_lexically(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut prefix = String::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::Prefix(prefix_component) => {
                prefix.push_str(&prefix_component.as_os_str().to_string_lossy());
            }
            Component::RootDir => {
                if prefix.is_empty() {
                    prefix.push('/');
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !parts.is_empty() && parts.last().map(|part| part != "..").unwrap_or(false) {
                    parts.pop();
                } else if !path.is_absolute() {
                    parts.push("..".to_string());
                }
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
        }
    }
    if prefix.is_empty() {
        parts.join("/")
    } else if prefix == "/" {
        format!("/{}", parts.join("/"))
    } else {
        format!("{}{}", prefix, parts.join("/"))
    }
}

/// Environment lookup with Node's `undefined`-for-missing semantics.
pub type DaemonRuntimeEnvironment<'a> = &'a dyn Fn(&str) -> Option<String>;

/// `getDaemonRuntimeIdentity(environment = process.env)`.
///
/// `entrypoint` is `process.argv[1]` and `executable` is `process.execPath`;
/// both are passed in so the identity stays testable without touching the host.
pub fn get_daemon_runtime_identity(
    environment: DaemonRuntimeEnvironment<'_>,
    entrypoint: Option<String>,
    executable: &str,
) -> DaemonRuntimeIdentity {
    let launcher = environment(PRIME_AGENT_LAUNCHER_PATH_ENV);
    DaemonRuntimeIdentity {
        // `??` keeps an empty env value; only `undefined` falls through.
        build_id: environment(PRIME_AGENT_BUILD_ID_ENV)
            .or_else(bundled_build_id)
            .unwrap_or_else(|| format!("release-{}", crate::config::VERSION)),
        executable_path: resolve_path(executable),
        // `...(entrypoint ? { entrypointPath } : {})` - falsy strings are dropped.
        entrypoint_path: entrypoint.filter(|value| !value.is_empty()).map(|value| resolve_path(&value)),
        launcher_path: launcher.filter(|value| !value.is_empty()).map(|value| resolve_path(&value)),
    }
}

/// `getDaemonRuntimeIdentity()` with the live `process.env`, `process.argv` and
/// `process.execPath` equivalents.
pub fn get_daemon_runtime_identity_from_process() -> DaemonRuntimeIdentity {
    let argv: Vec<String> = std::env::args().collect();
    let executable = std::env::current_exe()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    let lookup = |key: &str| std::env::var(key).ok();
    get_daemon_runtime_identity(&lookup, argv.get(1).cloned(), &executable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_id_precedence_matches_the_typescript() {
        // Explicit env wins, empty string included (`??` only skips undefined).
        let explicit = |key: &str| (key == PRIME_AGENT_BUILD_ID_ENV).then(|| "build-1".to_string());
        let identity = get_daemon_runtime_identity(&explicit, None, "/usr/bin/prime-agent");
        assert_eq!(identity.build_id, "build-1");

        let empty = |key: &str| (key == PRIME_AGENT_BUILD_ID_ENV).then(String::new);
        let identity = get_daemon_runtime_identity(&empty, None, "/usr/bin/prime-agent");
        assert_eq!(identity.build_id, "");

        // No build define in this build, so the release fallback is used.
        let none = |_key: &str| None;
        let identity = get_daemon_runtime_identity(&none, None, "/usr/bin/prime-agent");
        assert_eq!(identity.build_id, format!("release-{}", crate::config::VERSION));
    }

    #[test]
    fn optional_paths_are_omitted_when_absent_or_empty() {
        let none = |_key: &str| None;
        let identity = get_daemon_runtime_identity(&none, None, "prime-agent");
        assert!(identity.entrypoint_path.is_none());
        assert!(identity.launcher_path.is_none());

        let empty = |key: &str| (key == PRIME_AGENT_LAUNCHER_PATH_ENV).then(String::new);
        let identity = get_daemon_runtime_identity(&empty, Some(String::new()), "prime-agent");
        assert!(identity.entrypoint_path.is_none());
        assert!(identity.launcher_path.is_none());

        let launcher = |key: &str| (key == PRIME_AGENT_LAUNCHER_PATH_ENV).then(|| "bin/launcher".to_string());
        let identity = get_daemon_runtime_identity(&launcher, Some("bin/cli.js".to_string()), "/exe");
        assert!(identity.launcher_path.unwrap().ends_with("bin/launcher"));
        assert!(identity.entrypoint_path.unwrap().ends_with("bin/cli.js"));
        assert!(identity.executable_path.ends_with("exe"));
    }
}
