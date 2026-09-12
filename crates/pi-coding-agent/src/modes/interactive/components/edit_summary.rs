//! Port of packages/coding-agent/src/modes/interactive/components/edit-summary.ts

use std::collections::HashMap;

use pi_tui::utils::{truncate_to_width, visible_width};

use crate::core::tools::edit_diff::generate_diff_string;
use crate::core::tools::path_utils::resolve_to_cwd;
use crate::modes::interactive::components::keybinding_hints::expand_collapse_hint;
use crate::modes::interactive::theme::theme::theme;
use crate::utils::paths::{canonicalize_path, format_path_relative_to_cwd_or_absolute};

/// `FileChangeSummary`
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileChangeSummary {
    pub path: String,
    pub added: i64,
    pub removed: i64,
}

/// Port of `countChangedLines`.
pub fn count_changed_lines(diff: &str) -> (i64, i64) {
    let mut added = 0i64;
    let mut removed = 0i64;
    for line in diff.split('\n') {
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

/// Port of `mergeFileChange`.
fn merge_file_change(target: &mut Vec<FileChangeSummary>, change: FileChangeSummary, cwd: &str) {
    if change.added == 0 && change.removed == 0 {
        return;
    }
    let key = canonicalize_path(&resolve_to_cwd(&change.path, cwd));
    if let Some(existing) = target.iter_mut().find(|entry| entry.path == key) {
        existing.added += change.added;
        existing.removed += change.removed;
        return;
    }
    target.push(FileChangeSummary {
        path: key,
        ..change
    });
}

/// Port of `getToolFileChanges`.
///
/// `result.details` is `unknown` in the TypeScript; the port reads the same
/// `durationMs`/`diffs` JSON through `serde_json::Value` so the shapes stay
/// exactly the wire shapes used by the tools.
pub fn get_tool_file_changes(
    tool_name: &str,
    args: &serde_json::Value,
    result_details: Option<&serde_json::Value>,
    is_error: bool,
    cwd: &str,
) -> Vec<FileChangeSummary> {
    let mut changes: Vec<FileChangeSummary> = Vec::new();
    if tool_name == "ipython" {
        let diffs = result_details
            .and_then(|details| details.get("diffs"))
            .and_then(|diffs| diffs.as_array())
            .cloned()
            .unwrap_or_default();
        for display in diffs {
            let Some(path) = display.get("path").and_then(|path| path.as_str()) else {
                continue;
            };
            let Some(old_str) = display.get("oldStr").and_then(|value| value.as_str()) else {
                continue;
            };
            let Some(new_str) = display.get("newStr").and_then(|value| value.as_str()) else {
                continue;
            };
            let start_line = display
                .get("startLine")
                .and_then(|value| value.as_f64())
                .unwrap_or(1.0);
            let (diff, _) = generate_diff_string(old_str, new_str, 4, start_line.max(0.0) as usize);
            let (added, removed) = count_changed_lines(&diff);
            merge_file_change(
                &mut changes,
                FileChangeSummary {
                    path: path.to_string(),
                    added,
                    removed,
                },
                cwd,
            );
        }
    } else if tool_name == "edit" && !is_error {
        let path = args
            .get("path")
            .and_then(|value| value.as_str())
            .or_else(|| args.get("file_path").and_then(|value| value.as_str()));
        let diff = result_details
            .and_then(|details| details.get("diff"))
            .and_then(|value| value.as_str());
        if let (Some(path), Some(diff)) = (path, diff) {
            let (added, removed) = count_changed_lines(diff);
            merge_file_change(
                &mut changes,
                FileChangeSummary {
                    path: path.to_string(),
                    added,
                    removed,
                },
                cwd,
            );
        }
    }
    changes
}

/// Port of `mergeTurnFileChanges`.
///
/// `message.content` is a `Vec<ContentBlock>` in the port, so the tool calls are
/// collected from the `ToolCall` variants in order.
pub fn merge_turn_file_changes(
    target: &mut Vec<FileChangeSummary>,
    message: &pi_ai::types::AssistantMessage,
    tool_results: &[pi_ai::types::ToolResultMessage],
    cwd: &str,
) {
    let calls: HashMap<String, pi_ai::types::ToolCall> = message
        .content
        .iter()
        .filter_map(|content| match content {
            pi_ai::types::ContentBlock::ToolCall(call) => Some((call.id.clone(), call.clone())),
            _ => None,
        })
        .collect();
    for result in tool_results {
        let Some(call) = calls.get(&result.tool_call_id) else {
            continue;
        };
        let args = serde_json::Value::Object(call.arguments.clone());
        for change in get_tool_file_changes(
            &call.name,
            &args,
            result.details.as_ref(),
            result.is_error,
            cwd,
        ) {
            merge_file_change(target, change, cwd);
        }
    }
}

/// Dim gutter that anchors every per-file change summary line.
const FILE_CHANGE_SUMMARY_PREFIX: &str = "    \u{2570}\u{2500} ";
/// Indent that aligns diff rows with the summary line's text column.
pub fn file_change_diff_indent() -> &'static str {
    FILE_CHANGE_DIFF_INDENT.as_str()
}

/// `" ".repeat(visibleWidth(FILE_CHANGE_SUMMARY_PREFIX))`
pub static FILE_CHANGE_DIFF_INDENT: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| " ".repeat(visible_width(FILE_CHANGE_SUMMARY_PREFIX)));

/// Port of `formatChangeCounts`.
fn format_change_counts(added: i64, removed: i64) -> String {
    format!(
        "{} {}",
        theme().fg("toolDiffAdded", &format!("+{added}")),
        theme().fg("toolDiffRemoved", &format!("-{removed}"))
    )
}

/// Port of `formatFileChangePath`.
fn format_file_change_path(path: &str, cwd: &str) -> String {
    let resolved_path = resolve_to_cwd(path, cwd);
    let lexical_path = format_path_relative_to_cwd_or_absolute(&resolved_path, cwd);
    if !is_absolute(&lexical_path) {
        return lexical_path;
    }
    format_path_relative_to_cwd_or_absolute(
        &canonicalize_path(&resolved_path),
        &canonicalize_path(cwd),
    )
}

/// `path.isAbsolute` from `node:path`.
fn is_absolute(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    let bytes: Vec<char> = path.chars().collect();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == ':'
        && (bytes[2] == '\\' || bytes[2] == '/')
}

/// One `    ╰─ <path> +N -M` row, truncated to width; the path renders relative
/// to cwd where possible and the hint renders only when `diffsExpanded` is defined.
pub fn format_file_change_summary_line(
    raw_path: &str,
    cwd: Option<&str>,
    added: i64,
    removed: i64,
    diffs_expanded: Option<bool>,
    width: f64,
) -> String {
    let prefix = theme().fg("dim", FILE_CHANGE_SUMMARY_PREFIX);
    let hint = match diffs_expanded {
        None => String::new(),
        Some(expanded) => format!(
            "{}{}",
            theme().fg("dim", " \u{b7} "),
            expand_collapse_hint("app.edits.expand", expanded)
        ),
    };
    // Size the path against the wider hint variant ("to collapse") so toggling
    // ctrl+j never re-truncates it - the summary line is a stable anchor.
    let widest_hint = match diffs_expanded {
        None => String::new(),
        Some(_) => format!(
            "{}{}",
            theme().fg("dim", " \u{b7} "),
            expand_collapse_hint("app.edits.expand", true)
        ),
    };
    let counts = format!(
        "{}{}",
        theme().fg("dim", " "),
        format_change_counts(added, removed)
    );
    let suffix = format!("{counts}{hint}");
    let safe_width = std::cmp::max(1, width.max(0.0).floor() as usize);
    let available = std::cmp::max(
        1,
        safe_width
            .saturating_sub(visible_width(&prefix))
            .saturating_sub(visible_width(&counts))
            .saturating_sub(visible_width(&widest_hint)),
    );
    let display_path = match cwd {
        None => raw_path.to_string(),
        Some(cwd) => format_file_change_path(raw_path, cwd),
    };
    let path = truncate_to_width(&display_path, available as f64, "\u{2026}", false);
    truncate_to_width(
        &format!("{prefix}{}{suffix}", theme().fg("muted", &path)),
        safe_width as f64,
        "",
        false,
    )
}

/// Port of `formatTotalChangeSummary`.
pub fn format_total_change_summary(changes: &[FileChangeSummary]) -> String {
    let mut added = 0i64;
    let mut removed = 0i64;
    for change in changes {
        added += change.added;
        removed += change.removed;
    }
    let files = format!(
        "{} file{} changed",
        changes.len(),
        if changes.len() == 1 { "" } else { "s" }
    );
    format!(
        "{}{}{}",
        theme().fg("muted", &files),
        theme().fg("dim", " | "),
        format_change_counts(added, removed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::interactive::theme::theme::init_theme;
    use serde_json::json;

    fn init() {
        init_theme(Some("prime"), false);
    }

    fn strip_ansi(text: &str) -> String {
        let mut result = String::new();
        let mut chars = text.chars().peekable();
        while let Some(character) = chars.next() {
            if character == '\u{1b}' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            } else {
                result.push(character);
            }
        }
        result
    }

    #[test]
    fn counts_added_and_removed_lines() {
        assert_eq!(count_changed_lines("+1 a\n-2 b\n 3 c\n---\n+++\n"), (3, 2));
        assert_eq!(count_changed_lines(""), (0, 0));
    }

    #[test]
    fn edit_tool_changes_merge_by_canonical_path() {
        init();
        let cwd = if cfg!(windows) { "C:\\work" } else { "/work" };
        let path = if cfg!(windows) {
            "C:\\work\\a.ts"
        } else {
            "/work/a.ts"
        };
        let args = json!({ "path": path });
        let details = json!({ "diff": "+1 added\n-2 removed\n" });
        let changes = get_tool_file_changes("edit", &args, Some(&details), false, cwd);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].added, 1);
        assert_eq!(changes[0].removed, 1);

        // A failed edit contributes nothing.
        assert!(get_tool_file_changes("edit", &args, Some(&details), true, cwd).is_empty());
    }

    #[test]
    fn file_path_argument_is_accepted_for_the_edit_tool() {
        init();
        let cwd = if cfg!(windows) { "C:\\work" } else { "/work" };
        let args =
            json!({ "file_path": if cfg!(windows) { "C:\\work\\b.ts" } else { "/work/b.ts" } });
        let details = json!({ "diff": "+1 x\n" });
        let changes = get_tool_file_changes("edit", &args, Some(&details), false, cwd);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].added, 1);
        assert_eq!(changes[0].removed, 0);
    }

    #[test]
    fn zero_change_edits_are_dropped() {
        init();
        let args = json!({ "path": "a.ts" });
        let details = json!({ "diff": " 1 unchanged\n" });
        assert!(get_tool_file_changes("edit", &args, Some(&details), false, ".").is_empty());
    }

    #[test]
    fn summary_line_hides_the_hint_when_expansion_is_undefined() {
        init();
        let line = format_file_change_summary_line("src/a.ts", None, 3, 2, None, 60.0);
        let plain = strip_ansi(&line);
        assert!(plain.contains("src/a.ts"));
        assert!(plain.ends_with("+3 -2"));
        assert!(!plain.contains("to expand"));
    }

    #[test]
    fn summary_line_carries_the_expand_hint() {
        init();
        let line = format_file_change_summary_line("src/a.ts", None, 3, 2, Some(false), 60.0);
        let plain = strip_ansi(&line);
        assert!(plain.contains("to expand"));
        assert!(plain.contains("+3 -2"));
    }

    #[test]
    fn summary_line_truncates_to_width() {
        init();
        let line =
            format_file_change_summary_line("src/a-very-long-file-name.ts", None, 3, 2, None, 24.0);
        assert_eq!(visible_width(&line), 24);
    }

    #[test]
    fn total_summary_pluralizes_files() {
        init();
        let one = vec![FileChangeSummary {
            path: "a".to_string(),
            added: 1,
            removed: 2,
        }];
        assert!(strip_ansi(&format_total_change_summary(&one)).starts_with("1 file changed"));
        let two = vec![
            one[0].clone(),
            FileChangeSummary {
                path: "b".to_string(),
                added: 3,
                removed: 4,
            },
        ];
        let summary = strip_ansi(&format_total_change_summary(&two));
        assert!(summary.starts_with("2 files changed"));
        assert!(summary.ends_with("+4 -6"));
    }

    #[test]
    fn diff_indent_matches_the_prefix_width() {
        assert_eq!(
            FILE_CHANGE_DIFF_INDENT.as_str(),
            " ".repeat(visible_width(FILE_CHANGE_SUMMARY_PREFIX))
        );
    }
}
