//! Port of packages/coding-agent/src/utils/paths.ts

use std::path::{Component, Path, PathBuf};

fn separator() -> char {
    if cfg!(windows) {
        '\\'
    } else {
        '/'
    }
}

/// Resolve a path to its canonical (real) form, following symlinks.
/// Falls back to the raw path if resolution fails (e.g. the target does
/// not exist yet), so that callers never crash on missing filesystem
/// entries.
pub fn canonicalize_path(path: &str) -> String {
    match std::fs::canonicalize(path) {
        Ok(real) => real.to_string_lossy().to_string(),
        Err(_) => path.to_string(),
    }
}

/// Returns true if the value is NOT a package source (npm:, git:, etc.)
/// or a URL protocol. Bare names and relative paths without ./ prefix
/// are considered local.
pub fn is_local_path(value: &str) -> bool {
    let trimmed = value.trim();
    // Known non-local prefixes
    const NON_LOCAL_PREFIXES: [&str; 6] = ["npm:", "git:", "github:", "http:", "https:", "ssh:"];
    !NON_LOCAL_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

fn resolve_path(value: &str) -> String {
    let path = Path::new(value);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        if value.is_empty() {
            cwd
        } else {
            cwd.join(path)
        }
    };
    normalize_lexically(&absolute)
}

fn resolve_against_cwd(file_path: &str, cwd: &str) -> String {
    if Path::new(file_path).is_absolute() {
        resolve_path(file_path)
    } else {
        resolve_path(&join_paths(cwd, file_path))
    }
}

fn join_paths(base: &str, part: &str) -> String {
    let base = base.trim_end_matches(['/', '\\']);
    format!("{}{}{}", base, separator(), part)
}

/// Lexical normalization, mirroring Node's `path.resolve` (no filesystem access).
fn normalize_lexically(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut prefix = String::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix_component) => {
                prefix.push_str(&prefix_component.as_os_str().to_string_lossy());
            }
            Component::RootDir => {
                prefix.push(separator());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !parts.is_empty() {
                    parts.pop();
                }
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
        }
    }
    let joined = parts.join(&separator().to_string());
    if prefix.is_empty() {
        joined
    } else {
        format!("{}{}", prefix, joined)
    }
}

/// Node's `path.relative`: lexical, platform separator, `..` segments for parents.
fn relative_path(from: &str, to: &str) -> String {
    let from_parts = split_parts(from);
    let to_parts = split_parts(to);

    let mut common = 0;
    while common < from_parts.len() && common < to_parts.len() && from_parts[common] == to_parts[common] {
        common += 1;
    }

    let mut result: Vec<String> = Vec::new();
    for _ in common..from_parts.len() {
        result.push("..".to_string());
    }
    for part in &to_parts[common..] {
        result.push(part.clone());
    }
    result.join(&separator().to_string())
}

fn split_parts(value: &str) -> Vec<String> {
    value
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect()
}

/// Node's `path.resolve` for an absolute-or-relative path, without filesystem access.
/// Used by the agents-view port (`resolve(canonicalizePath(path))`).
pub fn resolve_absolute(value: &str) -> String {
    resolve_path(value)
}

/// Node's `path.resolve` for an already-absolute path (same lexical result).
pub fn normalize_absolute(path: &Path) -> String {
    normalize_lexically(path)
}

/// Node's `path.basename`.
pub fn basename(value: &str) -> String {
    let trimmed = value.trim_end_matches(['/', '\\']);
    match trimmed.rsplit(['/', '\\']).next() {
        Some(part) if !part.is_empty() => part.to_string(),
        _ => {
            if value == "/" || value == "\\" {
                value.to_string()
            } else {
                String::new()
            }
        }
    }
}

pub fn get_cwd_relative_path(file_path: &str, cwd: &str) -> Option<String> {
    let resolved_cwd = resolve_path(cwd);
    let resolved_path = resolve_against_cwd(file_path, &resolved_cwd);
    let relative = relative_path(&resolved_cwd, &resolved_path);
    let is_inside_cwd = relative.is_empty()
        || (relative != ".."
            && !relative.starts_with(&format!("..{}", separator()))
            && !Path::new(&relative).is_absolute());

    if is_inside_cwd {
        Some(if relative.is_empty() {
            ".".to_string()
        } else {
            relative
        })
    } else {
        None
    }
}

pub fn format_path_relative_to_cwd_or_absolute(file_path: &str, cwd: &str) -> String {
    let absolute_path = resolve_against_cwd(file_path, cwd);
    let chosen = get_cwd_relative_path(&absolute_path, cwd).unwrap_or(absolute_path);
    chosen.replace(separator(), "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_path_detection_matches_the_typescript() {
        assert!(is_local_path("./local"));
        assert!(is_local_path("bare-name"));
        assert!(!is_local_path("npm:foo"));
        assert!(!is_local_path("git:github.com/user/repo"));
        assert!(!is_local_path("https://example.com/x"));
        assert!(!is_local_path("ssh://git@example.com/x"));
        assert!(is_local_path("  ./padded  "));
    }

    #[test]
    fn relative_paths_inside_cwd() {
        let cwd = if cfg!(windows) { "C:\\work\\project" } else { "/work/project" };
        let inside = if cfg!(windows) { "C:\\work\\project\\src\\a.ts" } else { "/work/project/src/a.ts" };
        assert_eq!(get_cwd_relative_path(inside, cwd).unwrap(), format!("src{}a.ts", separator()));
        assert_eq!(get_cwd_relative_path(cwd, cwd).unwrap(), ".");

        let outside = if cfg!(windows) { "C:\\other\\b.ts" } else { "/other/b.ts" };
        assert!(get_cwd_relative_path(outside, cwd).is_none());
    }

    #[test]
    fn formats_with_forward_slashes() {
        let cwd = if cfg!(windows) { "C:\\work\\project" } else { "/work/project" };
        let inside = if cfg!(windows) { "C:\\work\\project\\src\\a.ts" } else { "/work/project/src/a.ts" };
        assert_eq!(format_path_relative_to_cwd_or_absolute(inside, cwd), "src/a.ts");
    }

    #[test]
    fn canonicalize_falls_back_to_the_raw_path() {
        assert_eq!(canonicalize_path("does-not-exist-xyz"), "does-not-exist-xyz");
    }
}
