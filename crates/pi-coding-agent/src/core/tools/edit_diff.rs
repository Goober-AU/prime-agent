//! Port of packages/coding-agent/src/core/tools/edit-diff.ts
//!
//! Shared diff computation utilities for the edit tool.
//! Used by both edit.rs (for execution) and tool-execution.ts (for preview rendering).

use std::collections::HashMap;
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
            // TS `matchLength: oldText.length` (edit-diff.ts:91) counts UTF-16 code
            // units, the same index space `String.prototype.substring` uses at
            // edit-diff.ts:236-239; `chars().count()` would be code points and land
            // the replacement end offset inside a surrogate pair for non-BMP text.
            match_length: utf16_len(old_text),
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
        // TS `matchLength: fuzzyOldText.length` (edit-diff.ts:117), UTF-16 code units.
        match_length: utf16_len(&fuzzy_old_text),
        used_fuzzy_match: true,
        content_for_replacement: fuzzy_content,
    }
}

/// JavaScript string indices are UTF-16 code units; the port keeps the same
/// index space so `substring`-style slicing stays compatible.
fn char_index(text: &str, byte_index: usize) -> usize {
    text[..byte_index].encode_utf16().count()
}

/// JavaScript `str.length`: the number of UTF-16 code units, not code points.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
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
        // TS `fuzzyContent.split("").length - 1` (edit-diff.ts:131) counts UTF-16
        // code units; code points would differ for non-BMP content.
        return utf16_len(&fuzzy_content);
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
// Line diff (port of the `diff` package's diffLines: Myers O(ND))
// ---------------------------------------------------------------------------

struct DiffPart {
    value: String,
    added: bool,
    removed: bool,
}

/// One run of tokens in the edit script: the port of the `component` objects
/// that jsdiff's `Diff.addToPath` / `Diff.extractCommon` build
/// (`diff@9.0.0` libesm/diff/base.js).
///
/// jsdiff links components with `previousComponent` object references; the port
/// stores an index into [`DiffGraph::components`] instead, so a 20k-line edit
/// script cannot blow the stack when the chain is dropped.
struct DiffComponent {
    count: usize,
    added: bool,
    removed: bool,
    previous: Option<usize>,
}

/// The component arena plus one search path of the Myers edit graph
/// (`bestPath[diagonalPath]` in jsdiff holds `{ oldPos, lastComponent }`).
struct DiffGraph {
    components: Vec<DiffComponent>,
}

/// jsdiff `{ oldPos, lastComponent }`.
#[derive(Clone, Copy)]
struct EditPath {
    old_pos: isize,
    last_component: Option<usize>,
}

impl DiffGraph {
    fn new() -> Self {
        DiffGraph {
            components: Vec::new(),
        }
    }

    fn push_component(
        &mut self,
        count: usize,
        added: bool,
        removed: bool,
        previous: Option<usize>,
    ) -> usize {
        self.components.push(DiffComponent {
            count,
            added,
            removed,
            previous,
        });
        self.components.len() - 1
    }

    /// jsdiff `Diff.addToPath` (base.js): extend `path` by one added or removed
    /// token, merging with the previous component when the change type repeats.
    fn add_to_path(
        &mut self,
        path: EditPath,
        added: bool,
        removed: bool,
        old_pos_inc: isize,
    ) -> EditPath {
        if let Some(last) = path.last_component.map(|index| &self.components[index]) {
            if last.added == added && last.removed == removed {
                let count = last.count + 1;
                let previous = last.previous;
                return EditPath {
                    old_pos: path.old_pos + old_pos_inc,
                    last_component: Some(self.push_component(count, added, removed, previous)),
                };
            }
        }
        EditPath {
            old_pos: path.old_pos + old_pos_inc,
            last_component: Some(self.push_component(1, added, removed, path.last_component)),
        }
    }

    /// jsdiff `Diff.extractCommon` (base.js): consume the run of equal tokens
    /// that follows `base_path` on `diagonal_path` and return the new `newPos`.
    fn extract_common(
        &mut self,
        base_path: &mut EditPath,
        new_tokens: &[&str],
        old_tokens: &[&str],
        diagonal_path: isize,
    ) -> isize {
        let new_len = new_tokens.len() as isize;
        let old_len = old_tokens.len() as isize;
        let mut old_pos = base_path.old_pos;
        let mut new_pos = old_pos - diagonal_path;
        let mut common_count: usize = 0;
        while new_pos + 1 < new_len
            && old_pos + 1 < old_len
            && old_tokens[(old_pos + 1) as usize] == new_tokens[(new_pos + 1) as usize]
        {
            new_pos += 1;
            old_pos += 1;
            common_count += 1;
        }
        if common_count > 0 {
            base_path.last_component =
                Some(self.push_component(common_count, false, false, base_path.last_component));
        }
        base_path.old_pos = old_pos;
        new_pos
    }

    /// jsdiff `Diff.diffWithOptionsObj` (base.js) for the default option set the
    /// edit tool uses: Myers O(ND) with jsdiff's edge-tracking optimization,
    /// O(D) live paths instead of an O(n*m) DP matrix, and jsdiff's exact
    /// tie-breaking (`!canRemove || (canAdd && removePath.oldPos < addPath.oldPos)`)
    /// so the part order matches `Diff.diffLines(oldContent, newContent)`
    /// (edit-diff.ts:259).
    ///
    /// Returns the component index of the edit-script head, or `None` for the
    /// empty diff (jsdiff's `buildValues(undefined, ...)`).
    fn diff_components(&mut self, old_tokens: &[&str], new_tokens: &[&str]) -> Option<usize> {
        let new_len = new_tokens.len() as isize;
        let old_len = old_tokens.len() as isize;
        let max_edit_length = new_len + old_len;

        let mut best_path: HashMap<isize, EditPath> = HashMap::new();
        best_path.insert(
            0,
            EditPath {
                old_pos: -1,
                last_component: None,
            },
        );

        // Seed editLength = 0: the content starts with the same values.
        let mut new_pos = {
            let mut seed = *best_path.get(&0).expect("seed path exists");
            let new_pos = self.extract_common(&mut seed, new_tokens, old_tokens, 0);
            best_path.insert(0, seed);
            new_pos
        };
        {
            let seed = best_path.get(&0).expect("seed path exists");
            if seed.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                return seed.last_component;
            }
        }

        // `-Infinity` / `Infinity` in jsdiff: half the range keeps `± 1` in bounds.
        let mut min_diagonal_to_consider = isize::MIN / 2;
        let mut max_diagonal_to_consider = isize::MAX / 2;

        let mut edit_length: isize = 1;
        while edit_length <= max_edit_length {
            let mut diagonal_path = std::cmp::max(min_diagonal_to_consider, -edit_length);
            let last_diagonal = std::cmp::min(max_diagonal_to_consider, edit_length);
            while diagonal_path <= last_diagonal {
                // Read both neighbours before clearing the one that is consumed.
                let remove_path = best_path.get(&(diagonal_path - 1)).copied();
                let add_path = best_path.get(&(diagonal_path + 1)).copied();
                if remove_path.is_some() {
                    best_path.remove(&(diagonal_path - 1));
                }

                let mut can_add = false;
                if let Some(add_path) = add_path {
                    let add_path_new_pos = add_path.old_pos - diagonal_path;
                    can_add = add_path_new_pos >= 0 && add_path_new_pos < new_len;
                }
                let can_remove = remove_path.is_some_and(|path| path.old_pos + 1 < old_len);

                if !can_add && !can_remove {
                    best_path.remove(&diagonal_path);
                    diagonal_path += 2;
                    continue;
                }

                // Pick the branch whose position in the old text is farthest
                // from the origin, exactly like jsdiff's comparison order.
                let prefer_add = !can_remove
                    || (can_add
                        && remove_path.expect("canRemove implies a path").old_pos
                            < add_path.expect("canAdd implies a path").old_pos);
                let mut base_path = if prefer_add {
                    self.add_to_path(add_path.expect("canAdd implies a path"), true, false, 0)
                } else {
                    self.add_to_path(
                        remove_path.expect("canRemove implies a path"),
                        false,
                        true,
                        1,
                    )
                };

                new_pos =
                    self.extract_common(&mut base_path, new_tokens, old_tokens, diagonal_path);
                if base_path.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                    return base_path.last_component;
                }

                let reached_old_end = base_path.old_pos + 1 >= old_len;
                let reached_new_end = new_pos + 1 >= new_len;
                best_path.insert(diagonal_path, base_path);
                // Past the edge of the edit graph no farther diagonal can help.
                if reached_old_end {
                    max_diagonal_to_consider =
                        std::cmp::min(max_diagonal_to_consider, diagonal_path - 1);
                }
                if reached_new_end {
                    min_diagonal_to_consider =
                        std::cmp::max(min_diagonal_to_consider, diagonal_path + 1);
                }
                diagonal_path += 2;
            }
            edit_length += 1;
        }
        None
    }
}

/// jsdiff `Diff.buildValues` (base.js): walk the component chain from the head
/// back to the tail, reverse it, and materialize each component's token run.
/// `LineDiff.useLongestToken` is false, so the unchanged/added components always
/// take the new-text branch.
fn build_diff_parts(
    graph: &DiffGraph,
    last_component: Option<usize>,
    old_tokens: &[&str],
    new_tokens: &[&str],
) -> Vec<DiffPart> {
    let mut components: Vec<&DiffComponent> = Vec::new();
    let mut next = last_component;
    while let Some(index) = next {
        let component = &graph.components[index];
        next = component.previous;
        components.push(component);
    }
    components.reverse();

    let mut parts: Vec<DiffPart> = Vec::new();
    let mut new_pos = 0usize;
    let mut old_pos = 0usize;
    for component in components {
        let value = if !component.removed {
            let value = new_tokens[new_pos..new_pos + component.count].concat();
            new_pos += component.count;
            if !component.added {
                old_pos += component.count;
            }
            value
        } else {
            let value = old_tokens[old_pos..old_pos + component.count].concat();
            old_pos += component.count;
            value
        };
        push_part(&mut parts, &value, component.added, component.removed);
    }
    parts
}

/// Port of `Diff.diffLines(oldContent, newContent)` (edit-diff.ts:259) for the
/// edit-tool input shape: jsdiff's `tokenize` (lines keep their trailing
/// newline) plus `removeEmpty`, then the Myers edit script. The parts are
/// emitted in jsdiff's order with the `added`/`removed` flags the renderer
/// relies on.
fn diff_lines(old_content: &str, new_content: &str) -> Vec<DiffPart> {
    let old_lines = split_keeping_newlines(old_content);
    let new_lines = split_keeping_newlines(new_content);
    let old_tokens: Vec<&str> = old_lines
        .iter()
        .map(String::as_str)
        .filter(|line| !line.is_empty())
        .collect();
    let new_tokens: Vec<&str> = new_lines
        .iter()
        .map(String::as_str)
        .filter(|line| !line.is_empty())
        .collect();

    let mut graph = DiffGraph::new();
    let last_component = graph.diff_components(&old_tokens, &new_tokens);
    build_diff_parts(&graph, last_component, &old_tokens, &new_tokens)
}

/// JavaScript `splitLines` semantics: the trailing newline stays with its line
/// and a trailing newline does not create an extra empty line. Matches jsdiff's
/// `tokenize` (`value.split(/(\n|\r\n)/)` with the separators merged back).
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
    fn fuzzy_find_text_counts_match_length_in_utf16_units() {
        // TS `matchLength: oldText.length` (edit-diff.ts:91) is the UTF-16 length:
        // "a\u{1F600}" is 3 code units, not 2 code points.
        let result = fuzzy_find_text("a\u{1F600}b", "a\u{1F600}");
        assert!(result.found);
        assert!(!result.used_fuzzy_match);
        assert_eq!(result.index, 0);
        assert_eq!(result.match_length, 3);
    }

    #[test]
    fn apply_edits_replaces_non_bmp_text_without_corruption() {
        // TS edit-diff.ts:236-239 slices with `substring(matchIndex, matchIndex +
        // matchLength)` in UTF-16 units, so an edit whose oldText ends with an
        // astral character replaces exactly those units and leaves the rest of the
        // file untouched. A code-point matchLength lands inside the surrogate pair
        // and writes U+FFFD instead of the tail of the file.
        let edits = vec![Edit {
            old_text: "a\u{1F600}".to_string(),
            new_text: "X".to_string(),
        }];
        let applied = apply_edits_to_normalized_content("a\u{1F600}b", &edits, "f.txt").expect("applied");
        assert_eq!(applied.base_content, "a\u{1F600}b");
        assert_eq!(applied.new_content, "Xb");
        assert!(!applied.new_content.contains('\u{FFFD}'));
    }

    #[test]
    fn apply_edits_detects_overlap_after_non_bmp_text() {
        // The overlap check (TS edit-diff.ts:226) compares
        // `previous.matchIndex + previous.matchLength` in UTF-16 units. With a
        // code-point matchLength the first edit spans 3 units instead of 4, so
        // `3 > 3` is false and the overlapping second edit is applied silently.
        let edits = vec![
            Edit {
                old_text: "\u{1F600}ab".to_string(),
                new_text: "x".to_string(),
            },
            Edit {
                old_text: "b".to_string(),
                new_text: "y".to_string(),
            },
        ];
        let error = apply_edits_to_normalized_content("\u{1F600}ab", &edits, "f.txt")
            .expect_err("must reject");
        assert_eq!(
            error,
            "edits[0] and edits[1] overlap in f.txt. Merge them into one edit or target disjoint regions."
        );
    }

    #[test]
    fn generate_diff_string_matches_jsdiff_part_order() {
        // `Diff.diffLines` (jsdiff ^9.0.0, edit-diff.ts:259) runs Myers O(ND) with
        // its own tie-breaking, so this edit script puts the added line first; the
        // previous LCS DP emitted "-1 a / +1 b /  2 a" instead.
        let (diff, first_changed_line) = generate_diff_string("a\na\n", "b\na\n", 4, 1);
        assert_eq!(diff, "+1 b\n 1 a\n-2 a");
        assert_eq!(first_changed_line, Some(1));
    }

    #[test]
    fn diff_lines_handles_large_inputs_without_a_quadratic_matrix() {
        // A 20k-line edit must not allocate the (n+1)x(m+1) DP matrix (20001^2
        // usize cells ~= 3.2 GB) that the previous implementation built.
        let old: String = (0..20_000)
            .map(|index| format!("line {index} {{ \"k\": {index} }}\n"))
            .collect();
        let mut new = old.clone();
        new = new.replace("line 0 {", "line 0 changed {");
        new = new.replace("line 19999 {", "line 19999 changed {");
        let (diff, first_changed_line) = generate_diff_string(&old, &new, 4, 1);
        assert_eq!(first_changed_line, Some(1));
        let removed_first = format!("-{} line 0 {{", pad_start("1", 5, ' '));
        let added_first = format!("+{} line 0 changed {{", pad_start("1", 5, ' '));
        let removed_last = "-20000 line 19999 {".to_string();
        let added_last = "+20000 line 19999 changed {".to_string();
        assert!(diff.contains(&removed_first), "missing removed first line in:\n{diff}");
        assert!(diff.contains(&added_first), "missing added first line in:\n{diff}");
        assert!(diff.contains(&removed_last), "missing removed last line in:\n{diff}");
        assert!(diff.contains(&added_last), "missing added last line in:\n{diff}");
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
