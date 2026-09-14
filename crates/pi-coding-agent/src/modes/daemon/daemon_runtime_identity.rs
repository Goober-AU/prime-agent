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
                // `Component::Prefix("C:")` followed by `Component::RootDir`
                // is the single drive root "C:\". Pushing the root separator
                // unconditionally keeps that root; the earlier `if prefix
                // .is_empty()` test dropped it and produced "C:Users/...",
                // a drive-relative path, so every caller failed with
                // os error 3 (and "/tmp/registry" became "C:tmp/registry").
                prefix.push('/');
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
    let executable = std::env::current_exe()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    let lookup = |key: &str| std::env::var(key).ok();
    // The native executable is also its entry point. argv[1] is a CLI flag.
    get_daemon_runtime_identity(&lookup, Some(executable.clone()), &executable)
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
    /// `Component::Prefix("C:")` followed by `Component::RootDir` is the single
    /// drive root "C:\". Returns the path once the host has produced that shape
    /// (None on a POSIX host, where "C:\..." has no Prefix component at all), so
    /// the assertions below cannot silently pass on a path that never reaches the
    /// RootDir arm.
    fn drive_absolute_input(label: &str, raw: &str) -> Option<PathBuf> {
        use std::path::Component;
        let path = Path::new(raw);
        if !matches!(path.components().next(), Some(Component::Prefix(_))) {
            return None;
        }
        assert!(
            path.components().any(|component| component == Component::RootDir),
            "{raw:?} has a drive prefix but no RootDir component ({label})"
        );
        Some(path.to_path_buf())
    }

    /// The defect this pins: the old `if prefix.is_empty()` guard skipped the
    /// root separator because `Prefix("C:")` had already filled `prefix`, so a
    /// drive-absolute path collapsed to a DRIVE-RELATIVE one - "C:Users/x/registry"
    /// instead of "C:/Users/x/registry", and a drive-rooted "/tmp/x" became
    /// "C:tmp/x". Every downstream create_dir_all / open / lockfile then failed
    /// with os error 3 ("The system cannot find the path specified").
    #[test]
    fn drive_roots_survive_the_lexical_collapse() {
        // Checked on every host: a rooted path keeps its root separator.
        let rooted = normalize_lexically(Path::new("/tmp/x"));
        assert!(rooted.starts_with('/'), "root separator dropped: {rooted}");

        let registry = match drive_absolute_input("registry", "C:\\Users\\x\\registry") {
            Some(path) => path,
            None => return,
        };
        let normalised = normalize_lexically(&registry);
        assert!(normalised.starts_with("C:/"), "drive root dropped: {normalised}");
        assert_eq!(normalised, "C:/Users/x/registry");

        // `Path::join` keeps the drive when it appends the rooted POSIX spelling,
        // which is exactly how a caller's resolve("/tmp/x") reaches this function.
        let joined = drive_absolute_input("joined /tmp/x", "C:\\base")
            .expect("C:\\base is drive-absolute")
            .join("/tmp/x");
        let normalised = normalize_lexically(&joined);
        assert!(!normalised.starts_with("C:tmp"), "/tmp/x became drive-relative: {normalised}");
        assert_eq!(normalised, "C:/tmp/x");

        let dotted = drive_absolute_input("parent collapse", "C:\\Users\\x\\..\\y")
            .expect("C:\\Users\\x\\..\\y is drive-absolute");
        assert_eq!(normalize_lexically(&dotted), "C:/Users/y");

        // Only the RootDir arm changed: a drive-RELATIVE input has no RootDir, so
        // it must still come back rootless instead of gaining a "C:/" root.
        let relative = Path::new("C:registry");
        if !relative.components().any(|component| component == std::path::Component::RootDir) {
            assert_eq!(normalize_lexically(relative), "C:registry");
        }
    }
}
