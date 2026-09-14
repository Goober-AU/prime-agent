//! Port of packages/coding-agent/src/utils/daemon-socket-path.ts

use std::path::{Path, PathBuf};

use super::pi_user_agent::process_platform;

/// Return the lexical socket identity without requiring the socket to exist.
pub fn normalize_socket_path(socket_path: &str, base_dir: Option<&str>) -> String {
    if process_platform() == "win32" {
        return socket_path.to_lowercase();
    }
    match base_dir {
        Some(base_dir) => lexical_resolve(&PathBuf::from(base_dir), socket_path),
        None => lexical_resolve(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")), socket_path),
    }
}

/// Node's `path.resolve`, lexically: absolute inputs win, otherwise join.
fn lexical_resolve(base: &Path, candidate: &str) -> String {
    let candidate_path = Path::new(candidate);
    let joined = if candidate_path.is_absolute() {
        candidate_path.to_path_buf()
    } else {
        base.join(candidate_path)
    };
    normalize_lexically(&joined)
}

/// Collapse `.` and `..` segments the way Node's `path.resolve` does.
fn normalize_lexically(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut prefix = String::new();
    for (index, component) in path.components().enumerate() {
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
                // Observed on this host: the drive-relative name "C:rwtest"
                // was created inside the working directory instead of the
                // absolute target the caller asked for.
                prefix.push('/');
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !parts.is_empty() && parts.last().map(|p| p != "..").unwrap_or(false) {
                    parts.pop();
                } else if !path.is_absolute() {
                    parts.push("..".to_string());
                }
            }
            Component::Normal(part) => {
                let _ = index;
                parts.push(part.to_string_lossy().to_string());
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_on_windows() {
        if process_platform() != "win32" {
            return;
        }
        assert_eq!(normalize_socket_path("C:\\Temp\\Pipe", None), "c:\\temp\\pipe");
    }

    #[test]
    fn resolves_against_the_base_directory() {
        if process_platform() == "win32" {
            return;
        }
        assert_eq!(normalize_socket_path("sock", Some("/tmp/base")), "/tmp/base/sock");
        assert_eq!(normalize_socket_path("/abs/sock", Some("/tmp/base")), "/abs/sock");
        assert_eq!(normalize_socket_path("../sock", Some("/tmp/base")), "/tmp/sock");
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
