//! Port of packages/coding-agent/src/core/tools/edit-diff.ts
//!
//! Shared diff computation utilities for the edit tool.
//! Used by both edit.rs (for execution) and tool-execution.ts (for preview rendering).

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::path_utils::resolve_to_cwd;

/// TypeScript `"\r\n" | "\n"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Crlf,
    Lf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Crlf => "\r\n",
            LineEnding::Lf => "\n",
        }
    }
}

pub fn detect_line_ending(content: &str) -> LineEnding {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    let (Some(lf_idx), Some(crlf_idx)) = (lf_idx, crlf_idx) else {
        return LineEnding::Lf;
    };
    if crlf_idx < lf_idx {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    }
}

pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn restore_line_endings(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Crlf => text.replace('\n', "\r\n"),
        LineEnding::Lf => text.to_string(),
    }
}

fn smart_single_quote_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"[\u{2018}\u{2019}\u{201A}\u{201B}]").expect("valid quote pattern"))
}

fn smart_double_quote_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"[\u{201C}\u{201D}\u{201E}\u{201F}]").expect("valid quote pattern"))
}

fn unicode_dash_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"[\u{2010}\u{2011}\u{2012}\u{2013}\u{2014}\u{2015}\u{2212}]").expect("valid dash pattern")
    })
}

fn unicode_space_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"[\u{00A0}\u{2002}-\u{200A}\u{202F}\u{205F}\u{3000}]").expect("valid space pattern")
    })
}

/// Normalize text for fuzzy matching. Applies progressive transformations:
/// - Strip trailing whitespace from each line
/// - Normalize smart quotes to ASCII equivalents
/// - Normalize Unicode dashes/hyphens to ASCII hyphen
/// - Normalize special Unicode spaces to regular space
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    // TypeScript applies `String.prototype.normalize("NFKC")` first. Rust's std
    // has no Unicode normalisation and the workspace has no ICU crate, so the
    // remaining transformations run on the original text; ASCII input is
    // unaffected because NFKC is an identity mapping there.
    let trimmed: String = text
        .split('\n')
        .map(|line| line.trim_end())
        .collect::<Vec<&str>>()
        .join("\n");
    let trimmed = smart_single_quote_pattern().replace_all(&trimmed, "'").into_owned();
    let trimmed = smart_double_quote_pattern().replace_all(&trimmed, "\"").into_owned();
    let trimmed = unicode_dash_pattern().replace_all(&trimmed, "-").into_owned();
    unicode_space_pattern().replace_all(&trimmed, " ").into_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FuzzyMatchResult {
    /// Whether a match was found
    pub found: bool,
    /// The index where the match starts (in the content that should be used for replacement)
    pub index: i64,
    /// Length of the matched text
    pub match_length: usize,
    /// Whether fuzzy matching was used (false = exact match)
    pub used_fuzzy_match: bool,
    /// The content to use for replacement operations.
    /// When exact match: original content. When fuzzy match: normalized content.
    pub content_for_replacement: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edit {
    #[serde(rename = "oldText")]
    pub old_text: String,
    #[serde(rename = "newText")]
    pub new_text: String,
}

struct MatchedEdit {
    edit_index: usize,
    match_index: i64,
    match_length: usize,
    new_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedEditsResult {
    pub base_content: String,
    pub new_content: String,
}

/// Find oldText in content, trying exact match first, then fuzzy match.
/// When fuzzy matching is used, the returned contentForReplacement is the
/// fuzzy-normalized version of the content (trailing whitespace stripped,
/// Unicode quotes/dashes normalized to ASCII).
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(exact_index) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index: char_index(content, exact_index) as i64,
            match_length: old_text.chars().count(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    }

    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    let fuzzy_index = fuzzy_content.find(&fuzzy_old_text);

    let Some(fuzzy_index) = fuzzy_index else {
        return FuzzyMatchResult {
            found: false,
            index: -1,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    };

    // When fuzzy matching, we work in the normalized space for replacement.
    // This means the output will have normalized whitespace/quotes/dashes,
    // which is acceptable since we're fixing minor formatting differences anyway.
    FuzzyMatchResult {
        found: true,
        index: char_index(&fuzzy_content, fuzzy_index) as i64,
        match_length: fuzzy_old_text.chars().count(),
        used_fuzzy_match: true,
        content_for_replacement: fuzzy_content,
    }
}

/// JavaScript string indices are UTF-16 code units; the port keeps the same
/// index space so `substring`-style slicing stays compatible for BMP text.
fn char_index(text: &str, byte_index: usize) -> usize {
    text[..byte_index].encode_utf16().count()
}

fn slice_by_index(text: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let end = end.min(units.len());
    let start = start.min(end);
    String::from_utf16_lossy(&units[start..end])
}

/// Strip UTF-8 BOM if present, return both the BOM (if any) and the text without it.
pub struct StripBomResult {
    pub bom: String,
    pub text: String,
}

pub fn strip_bom(content: &str) -> StripBomResult {
    match content.strip_prefix('\u{FEFF}') {
        Some(rest) => StripBomResult {
            bom: "\u{FEFF}".to_string(),
            text: rest.to_string(),
        },
        None => StripBomResult {
            bom: String::new(),
            text: content.to_string(),
        },
    }
}

fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    if fuzzy_old_text.is_empty() {
        return fuzzy_content.chars().count() + 1;
    }
    fuzzy_content.matches(&fuzzy_old_text).count()
}

fn get_not_found_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        return format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        );
    }
    format!(
        "Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines."
    )
}

fn get_duplicate_error(path: &str, edit_index: usize, total_edits: usize, occurrences: usize) -> String {
    if total_edits == 1 {
        return format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        );
    }
    format!(
        "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
    )
}

fn get_empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        return format!("oldText must not be empty in {path}.");
    }
    format!("edits[{edit_index}].oldText must not be empty in {path}.")
}

fn get_no_change_error(path: &str, total_edits: usize) -> String {
    if total_edits == 1 {
        return format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        );
    }
    format!("No changes made to {path}. The replacements produced identical content.")
}

/// Apply one or more exact-text replacements to LF-normalized content.
///
/// All edits are matched against the same original content. Replacements are
/// then applied in reverse order so offsets remain stable. If any edit needs
/// fuzzy matching, the operation runs in fuzzy-normalized content space to
/// preserve current single-edit behavior.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, String> {
    let normalized_edits: Vec<Edit> = edits
        .iter()
        .map(|edit| Edit {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect();

    for (index, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(get_empty_old_text_error(path, index, normalized_edits.len()));
        }
    }

    let initial_matches: Vec<FuzzyMatchResult> = normalized_edits
        .iter()
        .map(|edit| fuzzy_find_text(normalized_content, &edit.old_text))
        .collect();
    let base_content = if initial_matches.iter().any(|found| found.used_fuzzy_match) {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_string()
    };

    let mut matched_edits: Vec<MatchedEdit> = Vec::new();
    for (index, edit) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&base_content, &edit.old_text);
        if !match_result.found {
            return Err(get_not_found_error(path, index, normalized_edits.len()));
        }

        let occurrences = count_occurrences(&base_content, &edit.old_text);
        if occurrences > 1 {
            return Err(get_duplicate_error(path, index, normalized_edits.len(), occurrences));
        }

        matched_edits.push(MatchedEdit {
            edit_index: index,
            match_index: match_result.index,
            match_length: match_result.match_length,
            new_text: edit.new_text.clone(),
        });
    }

    matched_edits.sort_by_key(|edit| edit.match_index);
    for index in 1..matched_edits.len() {
        let previous = &matched_edits[index - 1];
        let current = &matched_edits[index];
        if previous.match_index + previous.match_length as i64 > current.match_index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index
            ));
        }
    }

    let mut new_content = base_content.clone();
    let mut index = matched_edits.len();
    while index > 0 {
        index -= 1;
        let edit = &matched_edits[index];
        let start = edit.match_index as usize;
        let end = start + edit.match_length;
        let head = slice_by_index(&new_content, 0, start);
        let tail = slice_by_index(&new_content, end, usize::MAX);
        new_content = format!("{head}{}{tail}", edit.new_text);
    }

    if base_content == new_content {
        return Err(get_no_change_error(path, normalized_edits.len()));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

// ---------------------------------------------------------------------------
// Line diff (port of the `diff` package's diffLines, LCS over lines)
// ---------------------------------------------------------------------------

struct DiffPart {
    value: String,
    added: bool,
    removed: bool,
}

/// Port of `Diff.diffLines(oldContent, newContent)` for the edit-tool input shape.
///
/// The `diff` package compares lines (keeping their trailing newline) with an
/// LCS; the parts below are emitted in the same order and with the same
/// `added`/`removed` flags the renderer relies on.
fn diff_lines(old_content: &str, new_content: &str) -> Vec<DiffPart> {
    let old_lines = split_keeping_newlines(old_content);
    let new_lines = split_keeping_newlines(new_content);

    let n = old_lines.len();
    let m = new_lines.len();
    // lcs[i][j] = length of the longest common subsequence of old_lines[i..] and new_lines[j..]
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old_lines[i] == new_lines[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                std::cmp::max(lcs[i + 1][j], lcs[i][j + 1])
            };
        }
    }

    let mut parts: Vec<DiffPart> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old_lines[i] == new_lines[j] {
            push_part(&mut parts, &old_lines[i], false, false);
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            push_part(&mut parts, &old_lines[i], false, true);
            i += 1;
        } else {
            push_part(&mut parts, &new_lines[j], true, false);
            j += 1;
        }
    }
    while i < n {
        push_part(&mut parts, &old_lines[i], false, true);
        i += 1;
    }
    while j < m {
        push_part(&mut parts, &new_lines[j], true, false);
        j += 1;
    }
    parts
}

/// JavaScript `splitLines` semantics: the trailing newline stays with its line
/// and a trailing newline does not create an extra empty line.
fn split_keeping_newlines(content: &str) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for character in content.chars() {
        current.push(character);
        if character == '\n' {
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn push_part(parts: &mut Vec<DiffPart>, value: &str, added: bool, removed: bool) {
    if let Some(last) = parts.last_mut() {
        if last.added == added && last.removed == removed {
            last.value.push_str(value);
            return;
        }
    }
    parts.push(DiffPart {
        value: value.to_string(),
        added,
        removed,
    });
}

/// Generate a unified diff string with line numbers and context.
/// Returns both the diff string and the first changed line number (in the new file).
pub fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
    start_line: usize,
) -> (String, Option<usize>) {
    let parts = diff_lines(old_content, new_content);
    let mut output: Vec<String> = Vec::new();

    let old_lines: Vec<&str> = old_content.split('\n').collect();
    let new_lines: Vec<&str> = new_content.split('\n').collect();
    let max_line_num = start_line - 1 + std::cmp::max(old_lines.len(), new_lines.len());
    let line_num_width = format!("{max_line_num}").chars().count();

    let mut old_line_num = start_line;
    let mut new_line_num = start_line;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for i in 0..parts.len() {
        let part = &parts[i];
        let mut raw: Vec<&str> = part.value.split('\n').collect();
        if raw.last() == Some(&"") {
            raw.pop();
        }

        if part.added || part.removed {
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_num);
            }

            for line in &raw {
                if part.added {
                    let line_num = pad_start(&format!("{new_line_num}"), line_num_width, ' ');
                    output.push(format!("+{line_num} {line}"));
                    new_line_num += 1;
                } else {
                    let line_num = pad_start(&format!("{old_line_num}"), line_num_width, ' ');
                    output.push(format!("-{line_num} {line}"));
                    old_line_num += 1;
                }
            }
            last_was_change = true;
        } else {
            let next_part_is_change =
                i < parts.len() - 1 && (parts[i + 1].added || parts[i + 1].removed);
            let has_leading_change = last_was_change;
            let has_trailing_change = next_part_is_change;

            if has_leading_change && has_trailing_change {
                if raw.len() <= context_lines * 2 {
                    for line in &raw {
                        let line_num = pad_start(&format!("{old_line_num}"), line_num_width, ' ');
                        output.push(format!(" {line_num} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    let leading_lines = &raw[..context_lines];
                    let trailing_lines = &raw[raw.len() - context_lines..];
                    let skipped_lines = raw.len() - leading_lines.len() - trailing_lines.len();

                    for line in leading_lines {
                        let line_num = pad_start(&format!("{old_line_num}"), line_num_width, ' ');
                        output.push(format!(" {line_num} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }

                    output.push(format!(" {} ...", pad_start("", line_num_width, ' ')));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;

                    for line in trailing_lines {
                        let line_num = pad_start(&format!("{old_line_num}"), line_num_width, ' ');
                        output.push(format!(" {line_num} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                }
            } else if has_leading_change {
                let shown_lines = &raw[..std::cmp::min(context_lines, raw.len())];
                let skipped_lines = raw.len() - shown_lines.len();

                for line in shown_lines {
                    let line_num = pad_start(&format!("{old_line_num}"), line_num_width, ' ');
                    output.push(format!(" {line_num} {line}"));
                    old_line_num += 1;
                    new_line_num += 1;
                }

                if skipped_lines > 0 {
                    output.push(format!(" {} ...", pad_start("", line_num_width, ' ')));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                }
            } else if has_trailing_change {
                let skipped_lines = raw.len().saturating_sub(context_lines);
                if skipped_lines > 0 {
                    output.push(format!(" {} ...", pad_start("", line_num_width, ' ')));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                }

                for line in &raw[skipped_lines..] {
                    let line_num = pad_start(&format!("{old_line_num}"), line_num_width, ' ');
                    output.push(format!(" {line_num} {line}"));
                    old_line_num += 1;
                    new_line_num += 1;
                }
            } else {
                old_line_num += raw.len();
                new_line_num += raw.len();
            }

            last_was_change = false;
        }
    }

    (output.join("\n"), first_changed_line)
}

fn pad_start(text: &str, width: usize, fill: char) -> String {
    let length = text.chars().count();
    if length >= width {
        return text.to_string();
    }
    let mut padded = String::with_capacity(width);
    for _ in 0..(width - length) {
        padded.push(fill);
    }
    padded.push_str(text);
    padded
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditDiffResult {
    pub diff: String,
    #[serde(rename = "firstChangedLine")]
    pub first_changed_line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditDiffError {
    pub error: String,
}

/// TypeScript `EditDiffResult | EditDiffError`.
#[derive(Debug, Clone, PartialEq)]
pub enum EditDiffOutcome {
    Result(EditDiffResult),
    Error(EditDiffError),
}

/// Compute the diff for one or more edit operations without applying them.
/// Used for preview rendering in the TUI before the tool executes.
pub async fn compute_edits_diff(path: &str, edits: &[Edit], cwd: &str) -> EditDiffOutcome {
    let absolute_path = resolve_to_cwd(path, cwd);

    let metadata = tokio::fs::metadata(&absolute_path).await;
    if let Err(error) = metadata {
        let error_message = format!("Error code: {}", io_error_code(&error));
        return EditDiffOutcome::Error(EditDiffError {
            error: format!("Could not edit file: {path}. {error_message}."),
        });
    }

    let raw_content = match tokio::fs::read_to_string(&absolute_path).await {
        Ok(content) => content,
        Err(error) => {
            return EditDiffOutcome::Error(EditDiffError {
                error: error.to_string(),
            })
        }
    };

    let StripBomResult { text: content, .. } = strip_bom(&raw_content);
    let normalized_content = normalize_to_lf(&content);
    match apply_edits_to_normalized_content(&normalized_content, edits, path) {
        Ok(AppliedEditsResult {
            base_content,
            new_content,
        }) => {
            let (diff, first_changed_line) = generate_diff_string(&base_content, &new_content, 4, 1);
            EditDiffOutcome::Result(EditDiffResult {
                diff,
                first_changed_line,
            })
        }
        Err(error) => EditDiffOutcome::Error(EditDiffError { error }),
    }
}

/// NodeJS error `code` property equivalent for the messages above.
fn io_error_code(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => format!("E{code}"),
        None => format!("{:?}", error.kind()),
    }
}

/// Compute the diff for a single edit operation without applying it.
/// Kept as a convenience wrapper for single-edit callers.
pub async fn compute_edit_diff(path: &str, old_text: &str, new_text: &str, cwd: &str) -> EditDiffOutcome {
    compute_edits_diff(
        path,
        &[Edit {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        }],
        cwd,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_line_ending_matches_typescript() {
        assert_eq!(detect_line_ending("a\r\nb"), LineEnding::Crlf);
        assert_eq!(detect_line_ending("a\nb\r\n"), LineEnding::Lf);
        assert_eq!(detect_line_ending("no newline"), LineEnding::Lf);
    }

    #[test]
    fn normalize_and_restore_line_endings_round_trip() {
        assert_eq!(normalize_to_lf("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(restore_line_endings("a\nb", LineEnding::Crlf), "a\r\nb");
        assert_eq!(restore_line_endings("a\nb", LineEnding::Lf), "a\nb");
    }

    #[test]
    fn fuzzy_find_text_prefers_exact_match() {
        let result = fuzzy_find_text("hello world", "world");
        assert!(result.found);
        assert_eq!(result.index, 6);
        assert_eq!(result.match_length, 5);
        assert!(!result.used_fuzzy_match);
        assert_eq!(result.content_for_replacement, "hello world");
    }

    #[test]
    fn fuzzy_find_text_normalizes_trailing_whitespace_and_quotes() {
        let result = fuzzy_find_text("a = \u{2018}x\u{2019}   \nb", "a = 'x'\nb");
        assert!(result.found);
        assert!(result.used_fuzzy_match);
        assert_eq!(result.content_for_replacement, "a = 'x'\nb");
    }

    #[test]
    fn fuzzy_find_text_reports_missing_text() {
        let result = fuzzy_find_text("abc", "zzz");
        assert!(!result.found);
        assert_eq!(result.index, -1);
        assert_eq!(result.match_length, 0);
        assert!(!result.used_fuzzy_match);
        assert_eq!(result.content_for_replacement, "abc");
    }

    #[test]
    fn strip_bom_returns_bom_and_text() {
        let stripped = strip_bom("\u{FEFF}hello");
        assert_eq!(stripped.bom, "\u{FEFF}");
        assert_eq!(stripped.text, "hello");
        let plain = strip_bom("hello");
        assert_eq!(plain.bom, "");
        assert_eq!(plain.text, "hello");
    }

    #[test]
    fn apply_edits_replaces_single_block() {
        let edits = vec![Edit {
            old_text: "b".to_string(),
            new_text: "B".to_string(),
        }];
        let applied = apply_edits_to_normalized_content("a\nb\nc", &edits, "f.txt").expect("applied");
        assert_eq!(applied.base_content, "a\nb\nc");
        assert_eq!(applied.new_content, "a\nB\nc");
    }

    #[test]
    fn apply_edits_applies_multiple_disjoint_edits_in_reverse_order() {
        let edits = vec![
            Edit {
                old_text: "a".to_string(),
                new_text: "AA".to_string(),
            },
            Edit {
                old_text: "c".to_string(),
                new_text: "CC".to_string(),
            },
        ];
        let applied = apply_edits_to_normalized_content("a b c", &edits, "f.txt").expect("applied");
        assert_eq!(applied.new_content, "AA b CC");
    }

    #[test]
    fn apply_edits_rejects_empty_old_text() {
        let edits = vec![Edit {
            old_text: String::new(),
            new_text: "x".to_string(),
        }];
        let error = apply_edits_to_normalized_content("abc", &edits, "f.txt").expect_err("must reject");
        assert_eq!(error, "oldText must not be empty in f.txt.");
    }

    #[test]
    fn apply_edits_rejects_missing_text() {
        let edits = vec![Edit {
            old_text: "zzz".to_string(),
            new_text: "x".to_string(),
        }];
        let error = apply_edits_to_normalized_content("abc", &edits, "f.txt").expect_err("must reject");
        assert_eq!(
            error,
            "Could not find the exact text in f.txt. The old text must match exactly including all whitespace and newlines."
        );
    }

    #[test]
    fn apply_edits_rejects_duplicate_text() {
        let edits = vec![Edit {
            old_text: "a".to_string(),
            new_text: "b".to_string(),
        }];
        let error = apply_edits_to_normalized_content("a a", &edits, "f.txt").expect_err("must reject");
        assert_eq!(
            error,
            "Found 2 occurrences of the text in f.txt. The text must be unique. Please provide more context to make it unique."
        );
    }

    #[test]
    fn apply_edits_rejects_overlapping_edits() {
        let edits = vec![
            Edit {
                old_text: "abc".to_string(),
                new_text: "x".to_string(),
            },
            Edit {
                old_text: "bc".to_string(),
                new_text: "y".to_string(),
            },
        ];
        let error = apply_edits_to_normalized_content("abcdef", &edits, "f.txt").expect_err("must reject");
        assert_eq!(
            error,
            "edits[0] and edits[1] overlap in f.txt. Merge them into one edit or target disjoint regions."
        );
    }

    #[test]
    fn apply_edits_rejects_no_change() {
        let edits = vec![Edit {
            old_text: "a".to_string(),
            new_text: "a".to_string(),
        }];
        let error = apply_edits_to_normalized_content("abc", &edits, "f.txt").expect_err("must reject");
        assert_eq!(
            error,
            "No changes made to f.txt. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        );
    }

    #[test]
    fn apply_edits_reports_multi_edit_messages() {
        let edits = vec![
            Edit {
                old_text: "a".to_string(),
                new_text: "A".to_string(),
            },
            Edit {
                old_text: "zzz".to_string(),
                new_text: "Z".to_string(),
            },
        ];
        let error = apply_edits_to_normalized_content("a b", &edits, "f.txt").expect_err("must reject");
        assert_eq!(
            error,
            "Could not find edits[1] in f.txt. The oldText must match exactly including all whitespace and newlines."
        );
    }

    #[test]
    fn generate_diff_string_marks_added_and_removed_lines() {
        let (diff, first_changed_line) = generate_diff_string("a\nb\nc", "a\nB\nc", 4, 1);
        assert_eq!(diff, " 1 a\n-2 b\n+2 B\n 3 c");
        assert_eq!(first_changed_line, Some(2));
    }

    #[test]
    fn generate_diff_string_collapses_unchanged_gaps() {
        let old: String = (1..=12).map(|index| format!("line{index}\n")).collect();
        let mut new = old.clone();
        new = new.replace("line1\n", "line1 changed\n");
        new = new.replace("line12\n", "line12 changed\n");
        let (diff, _) = generate_diff_string(&old, &new, 1, 1);
        assert!(diff.contains(" ..."), "expected elision marker in:\n{diff}");
        assert_eq!(diff, "- 1 line1\n+ 1 line1 changed\n  2 line2\n    ...\n 11 line11\n-12 line12\n+12 line12 changed");
    }

    #[test]
    fn generate_diff_string_uses_start_line_offset() {
        let (diff, first_changed_line) = generate_diff_string("a", "b", 4, 10);
        assert_eq!(diff, "-10 a\n+10 b");
        assert_eq!(first_changed_line, Some(10));
    }

    #[tokio::test]
    async fn compute_edits_diff_reports_missing_file() {
        let outcome = compute_edits_diff(
            "missing-file-for-edit-diff.txt",
            &[Edit {
                old_text: "a".to_string(),
                new_text: "b".to_string(),
            }],
            std::env::temp_dir().to_string_lossy().as_ref(),
        )
        .await;
        match outcome {
            EditDiffOutcome::Error(error) => {
                assert!(error.error.starts_with("Could not edit file: missing-file-for-edit-diff.txt."));
            }
            EditDiffOutcome::Result(_) => panic!("expected error outcome"),
        }
    }

    #[tokio::test]
    async fn compute_edits_diff_returns_diff_for_existing_file() {
        let dir = std::env::temp_dir().join(format!("pi-edit-diff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("target.txt");
        std::fs::write(&file, "a\nb\nc\n").expect("write");
        let outcome = compute_edit_diff(
            "target.txt",
            "b",
            "B",
            dir.to_string_lossy().as_ref(),
        )
        .await;
        let _ = std::fs::remove_dir_all(&dir);
        match outcome {
            EditDiffOutcome::Result(result) => {
                assert_eq!(result.diff, " 1 a\n-2 b\n+2 B\n 3 c");
                assert_eq!(result.first_changed_line, Some(2));
            }
            EditDiffOutcome::Error(error) => panic!("unexpected error: {}", error.error),
        }
    }
}
