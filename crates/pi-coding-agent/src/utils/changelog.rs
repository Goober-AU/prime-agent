//! Port of packages/coding-agent/src/utils/changelog.ts

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogEntry {
    pub major: i64,
    pub minor: i64,
    pub patch: i64,
    pub content: String,
}

/// Parse changelog entries from CHANGELOG.md
/// Scans for ## lines and collects content until next ## or EOF
pub fn parse_changelog(changelog_path: &str) -> Vec<ChangelogEntry> {
    if !Path::new(changelog_path).exists() {
        return Vec::new();
    }

    let content = match std::fs::read_to_string(changelog_path) {
        Ok(content) => content,
        Err(error) => {
            // The TypeScript catches everything and warns; keep the same text.
            eprintln!("Warning: Could not parse changelog: {}", error);
            return Vec::new();
        }
    };

    let lines: Vec<&str> = content.split('\n').collect();
    let mut entries: Vec<ChangelogEntry> = Vec::new();

    let mut current_lines: Vec<String> = Vec::new();
    let mut current_version: Option<(i64, i64, i64)> = None;

    for line in lines {
        // Check if this is a version header (## [x.y.z] ...)
        if line.starts_with("## ") {
            // Save previous entry if exists
            if let Some((major, minor, patch)) = current_version {
                if !current_lines.is_empty() {
                    entries.push(ChangelogEntry {
                        major,
                        minor,
                        patch,
                        content: current_lines.join("\n").trim().to_string(),
                    });
                }
            }

            // Try to parse version from this line
            match parse_version_header(line) {
                Some(version) => {
                    current_version = Some(version);
                    current_lines = vec![line.to_string()];
                }
                None => {
                    // Reset if we can't parse version
                    current_version = None;
                    current_lines = Vec::new();
                }
            }
        } else if current_version.is_some() {
            // Collect lines for current version
            current_lines.push(line.to_string());
        }
    }

    // Save last entry
    if let Some((major, minor, patch)) = current_version {
        if !current_lines.is_empty() {
            entries.push(ChangelogEntry {
                major,
                minor,
                patch,
                content: current_lines.join("\n").trim().to_string(),
            });
        }
    }

    entries
}

/// `/##\s+\[?(\d+)\.(\d+)\.(\d+)\]?/` applied to a `## ` line.
fn parse_version_header(line: &str) -> Option<(i64, i64, i64)> {
    let rest = line.strip_prefix("##")?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('[').unwrap_or(rest);
    let mut numbers: Vec<i64> = Vec::new();
    let mut current = String::new();
    for character in rest.chars() {
        if character.is_ascii_digit() {
            current.push(character);
        } else if character == '.' && !current.is_empty() {
            numbers.push(current.parse().ok()?);
            current.clear();
        } else {
            break;
        }
    }
    if current.is_empty() {
        return None;
    }
    numbers.push(current.parse().ok()?);
    if numbers.len() < 3 {
        return None;
    }
    Some((numbers[0], numbers[1], numbers[2]))
}

/// Compare versions. Returns: -1 if v1 < v2, 0 if v1 === v2, 1 if v1 > v2
pub fn compare_versions(v1: &ChangelogEntry, v2: &ChangelogEntry) -> i64 {
    if v1.major != v2.major {
        return v1.major - v2.major;
    }
    if v1.minor != v2.minor {
        return v1.minor - v2.minor;
    }
    v1.patch - v2.patch
}

/// Get entries newer than lastVersion
pub fn get_new_entries(entries: &[ChangelogEntry], last_version: &str) -> Vec<ChangelogEntry> {
    // Parse lastVersion
    let parts: Vec<i64> = last_version
        .split('.')
        .map(|part| part.trim().parse::<f64>().map(|value| value as i64).unwrap_or(0))
        .collect();
    let last = ChangelogEntry {
        major: parts.first().copied().unwrap_or(0),
        minor: parts.get(1).copied().unwrap_or(0),
        patch: parts.get(2).copied().unwrap_or(0),
        content: String::new(),
    };

    entries
        .iter()
        .filter(|entry| compare_versions(entry, &last) > 0)
        .cloned()
        .collect()
}

/// Re-export of `getChangelogPath` from config.ts for convenience.
///
/// `config.ts` belongs to another slice; this minimal local definition keeps the
/// dependency explicit until `pi_coding_agent::config::get_changelog_path` exists.
pub fn get_changelog_path() -> String {
    let package_dir = std::env::var("PI_PACKAGE_DIR").unwrap_or_else(|_| {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .to_string_lossy()
            .to_string()
    });
    let trimmed = package_dir.trim_end_matches(['/', '\\']);
    let separator = if cfg!(windows) { "\\" } else { "/" };
    format!("{}{}CHANGELOG.md", trimmed, separator)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
## [0.9.3] - 2026-09-11

### Fixed
- Fixed a thing

## [0.9.2]

### Added
- Added another thing

## Unreleased

### Changed
- Something else
";

    fn temp_changelog(contents: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CHANGELOG.md");
        std::fs::write(&path, contents).unwrap();
        std::mem::forget(dir);
        path.to_string_lossy().to_string()
    }

    #[test]
    fn parses_versioned_sections_and_ignores_unparseable_headers() {
        let path = temp_changelog(SAMPLE);
        let entries = parse_changelog(&path);
        assert_eq!(entries.len(), 2);
        assert_eq!((entries[0].major, entries[0].minor, entries[0].patch), (0, 9, 3));
        assert_eq!((entries[1].major, entries[1].minor, entries[1].patch), (0, 9, 2));
        assert!(entries[0].content.starts_with("## [0.9.3] - 2026-09-11"));
        assert!(entries[0].content.ends_with("- Fixed a thing"));
        // The unparseable "## Unreleased" header resets collection.
        assert!(!entries[1].content.contains("Something else"));
    }

    #[test]
    fn missing_files_produce_no_entries() {
        assert!(parse_changelog("does-not-exist-changelog.md").is_empty());
    }

    #[test]
    fn compares_versions() {
        let v = |major, minor, patch| ChangelogEntry {
            major,
            minor,
            patch,
            content: String::new(),
        };
        assert!(compare_versions(&v(0, 70, 6), &v(0, 70, 5)) > 0);
        assert_eq!(compare_versions(&v(0, 70, 5), &v(0, 70, 5)), 0);
        assert!(compare_versions(&v(0, 70, 4), &v(0, 70, 5)) < 0);
    }

    #[test]
    fn filters_entries_newer_than_the_last_version() {
        let path = temp_changelog(SAMPLE);
        let entries = parse_changelog(&path);
        let newer = get_new_entries(&entries, "0.9.2");
        assert_eq!(newer.len(), 1);
        assert_eq!(newer[0].patch, 3);
        assert_eq!(get_new_entries(&entries, "0.9.3").len(), 0);
        assert_eq!(get_new_entries(&entries, "garbage").len(), 2);
    }
}
