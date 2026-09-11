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
                if prefix.is_empty() {
                    prefix.push('/');
                }
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
}
