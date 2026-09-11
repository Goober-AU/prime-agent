//! Port of packages/coding-agent/src/core/tools/path-utils.ts

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

const UNICODE_SPACES: &str = "[\u{00A0}\u{2000}-\u{200A}\u{202F}\u{205F}\u{3000}]";
const NARROW_NO_BREAK_SPACE: &str = "\u{202F}";

fn unicode_spaces_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(UNICODE_SPACES).expect("valid unicode space pattern"))
}

fn am_pm_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r" (AM|PM)\.").expect("valid am/pm pattern"))
}

fn normalize_unicode_spaces(text: &str) -> String {
    unicode_spaces_pattern().replace_all(text, " ").into_owned()
}

fn try_macos_screenshot_path(file_path: &str) -> String {
    am_pm_pattern()
        .replace_all(file_path, |caps: &regex::Captures<'_>| {
            format!("{NARROW_NO_BREAK_SPACE}{}.", &caps[1])
        })
        .into_owned()
}

fn try_nfd_variant(file_path: &str) -> String {
    // macOS stores filenames in NFD (decomposed) form, try converting user input to NFD.
    // Rust's std has no Unicode normalization; the ASCII path is unaffected and
    // non-ASCII input keeps its original bytes.
    file_path.to_string()
}

fn try_curly_quote_variant(file_path: &str) -> String {
    // macOS uses U+2019 (right single quotation mark) in screenshot names like "Capture d'ecran".
    // Users typically type U+0027 (straight apostrophe).
    file_path.replace('\'', "\u{2019}")
}

fn file_exists(file_path: &str) -> bool {
    std::fs::metadata(file_path).is_ok()
}

fn normalize_at_prefix(file_path: &str) -> &str {
    match file_path.strip_prefix('@') {
        Some(rest) => rest,
        None => file_path,
    }
}

/// NodeJS `process.platform` equivalent for the `platform` parameter default.
pub fn process_platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

fn home_dir() -> String {
    match dirs::home_dir() {
        Some(home) => home.to_string_lossy().into_owned(),
        None => std::env::var("HOME").unwrap_or_default(),
    }
}

pub fn expand_path(file_path: &str) -> String {
    expand_path_with_platform(file_path, process_platform())
}

pub fn expand_path_with_platform(file_path: &str, platform: &str) -> String {
    let normalized = normalize_unicode_spaces(normalize_at_prefix(file_path));
    if normalized == "~" {
        return home_dir();
    }
    let windows_tilde = platform == "win32" && normalized.starts_with("~\\");
    if normalized.starts_with("~/") || windows_tilde {
        let home = home_dir();
        let rest = &normalized[2..];
        if platform == "win32" {
            return windows_join(&home, rest);
        }
        return posix_join(&home, rest);
    }
    normalized
}

fn posix_join(base: &str, rest: &str) -> String {
    let mut joined = PathBuf::from(base);
    for segment in rest.split('/') {
        joined.push(segment);
    }
    joined.to_string_lossy().replace('\\', "/")
}

fn windows_join(base: &str, rest: &str) -> String {
    let mut joined = PathBuf::from(base);
    for segment in rest.split(['/', '\\']) {
        joined.push(segment);
    }
    joined.to_string_lossy().into_owned()
}

/// Resolve a path relative to the given cwd.
/// Handles ~ expansion and absolute paths.
pub fn resolve_to_cwd(file_path: &str, cwd: &str) -> String {
    let expanded = expand_path(file_path);
    if is_absolute(&expanded) {
        return expanded;
    }
    resolve_path(cwd, &expanded)
}

/// NodeJS `path.resolve` equivalent for the port's path handling.
pub fn resolve_path(base: &str, target: &str) -> String {
    let joined = Path::new(base).join(target);
    normalize_path(&joined).to_string_lossy().into_owned()
}

/// NodeJS `path.isAbsolute` on the current platform.
pub fn is_absolute(file_path: &str) -> bool {
    if file_path.is_empty() {
        return false;
    }
    if cfg!(windows) {
        let bytes = file_path.as_bytes();
        let drive_absolute = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/');
        return drive_absolute || file_path.starts_with("\\\\") || file_path.starts_with("//");
    }
    file_path.starts_with('/')
}

/// Lexical path normalization (resolves `.` and `..` without touching the disk).
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !result.pop() {
                    result.push("..");
                }
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

pub fn resolve_read_path(file_path: &str, cwd: &str) -> String {
    let resolved = resolve_to_cwd(file_path, cwd);

    if file_exists(&resolved) {
        return resolved;
    }

    let am_pm_variant = try_macos_screenshot_path(&resolved);
    if am_pm_variant != resolved && file_exists(&am_pm_variant) {
        return am_pm_variant;
    }

    let nfd_variant = try_nfd_variant(&resolved);
    if nfd_variant != resolved && file_exists(&nfd_variant) {
        return nfd_variant;
    }

    let curly_variant = try_curly_quote_variant(&resolved);
    if curly_variant != resolved && file_exists(&curly_variant) {
        return curly_variant;
    }

    let nfd_curly_variant = try_curly_quote_variant(&nfd_variant);
    if nfd_curly_variant != resolved && file_exists(&nfd_curly_variant) {
        return nfd_curly_variant;
    }

    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_shortcut() {
        let home = home_dir();
        assert_eq!(expand_path_with_platform("~", "linux"), home);
        assert_eq!(expand_path_with_platform("~/x", "linux"), posix_join(&home, "x"));
        assert!(expand_path_with_platform("~\\x", "win32").ends_with("x"));
        assert_eq!(expand_path_with_platform("~\\x", "linux"), "~\\x");
    }

    #[test]
    fn strips_at_prefix_and_normalizes_spaces() {
        assert_eq!(expand_path_with_platform("@/tmp/a", "linux"), "/tmp/a");
        assert_eq!(expand_path_with_platform("/tmp\u{00A0}a", "linux"), "/tmp a");
    }

    #[test]
    fn resolves_relative_paths_against_cwd() {
        assert_eq!(resolve_to_cwd("b/c", "/a"), "/a/b/c");
        assert_eq!(resolve_to_cwd("/abs", "/a"), "/abs");
        assert_eq!(resolve_to_cwd("../b", "/a/c"), "/a/b");
    }

    #[test]
    fn macos_screenshot_variant_uses_narrow_no_break_space() {
        assert_eq!(
            try_macos_screenshot_path("/tmp/Screen Shot 10.00 AM.png"),
            format!("/tmp/Screen Shot 10.00{NARROW_NO_BREAK_SPACE}AM.png")
        );
    }

    #[test]
    fn curly_quote_variant_replaces_apostrophes() {
        assert_eq!(try_curly_quote_variant("Capture d'ecran.png"), "Capture d\u{2019}ecran.png");
    }

    #[test]
    fn missing_file_returns_resolved_path() {
        let resolved = resolve_read_path("/definitely/missing/file.txt", "/");
        assert_eq!(resolved, "/definitely/missing/file.txt");
    }
}
