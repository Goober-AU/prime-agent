//! Port of packages/coding-agent/src/modes/interactive/components/diff.ts

use pi_tui::utils::{truncate_to_width, visible_width, wrap_text_with_ansi};
use regex::Regex;

use crate::core::tools::edit_diff::generate_diff_string;
use crate::modes::interactive::theme::theme::{highlight_code, theme};

/// Parse diff line to extract prefix, line number, and content.
/// Format: "+123 content" or "-123 content" or " 123 content" or "     ..."
fn parse_diff_line(line: &str) -> Option<(String, String, String)> {
    // `^([+-\s])(\s*\d*)\s(.*)$`
    let mut chars = line.chars();
    let prefix = chars.next()?;
    if prefix != '+' && prefix != '-' && !prefix.is_whitespace() {
        return None;
    }
    let rest = &line[prefix.len_utf8()..];

    let mut line_num_end = 0usize;
    for ch in rest.chars() {
        if ch.is_whitespace() || ch.is_ascii_digit() {
            line_num_end += ch.len_utf8();
        } else {
            break;
        }
    }
    let line_num = &rest[..line_num_end];
    let after_line_num = &rest[line_num_end..];
    let sep = after_line_num.chars().next()?;
    if !sep.is_whitespace() {
        return None;
    }
    let content = &after_line_num[sep.len_utf8()..];
    Some((
        prefix.to_string(),
        line_num.to_string(),
        content.to_string(),
    ))
}

/// Replace tabs with spaces for consistent rendering.
fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

fn leading_whitespace(text: &str) -> String {
    text.chars().take_while(|ch| ch.is_whitespace()).collect()
}

struct WordPart {
    value: String,
    added: bool,
    removed: bool,
}

/// Port of `Diff.diffWords` for the intra-line case: split on whitespace runs
/// (the `diff` package groups whitespace with the adjacent word), then LCS over
/// the resulting tokens.
fn diff_words(old_content: &str, new_content: &str) -> Vec<WordPart> {
    let old_tokens = split_keeping_whitespace(old_content);
    let new_tokens = split_keeping_whitespace(new_content);
    let n = old_tokens.len();
    let m = new_tokens.len();
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old_tokens[i] == new_tokens[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                std::cmp::max(lcs[i + 1][j], lcs[i][j + 1])
            };
        }
    }

    let mut parts: Vec<WordPart> = Vec::new();
    let mut push = |parts: &mut Vec<WordPart>, value: &str, added: bool, removed: bool| {
        if let Some(last) = parts.last_mut() {
            if last.added == added && last.removed == removed {
                last.value.push_str(value);
                return;
            }
        }
        parts.push(WordPart {
            value: value.to_string(),
            added,
            removed,
        });
    };

    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old_tokens[i] == new_tokens[j] {
            push(&mut parts, &old_tokens[i], false, false);
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            push(&mut parts, &old_tokens[i], false, true);
            i += 1;
        } else {
            push(&mut parts, &new_tokens[j], true, false);
            j += 1;
        }
    }
    while i < n {
        push(&mut parts, &old_tokens[i], false, true);
        i += 1;
    }
    while j < m {
        push(&mut parts, &new_tokens[j], true, false);
        j += 1;
    }
    parts
}

/// `diffWords` tokenization: a run of whitespace and the following word form one
/// token; whitespace at the end of the string is its own token.
fn split_keeping_whitespace(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut pending_whitespace = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            pending_whitespace.push(ch);
        } else {
            current.push_str(&pending_whitespace);
            pending_whitespace.clear();
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    if !pending_whitespace.is_empty() {
        tokens.push(pending_whitespace);
    }
    tokens
}

/// Compute word-level diff and render with inverse on changed parts.
/// Uses diffWords which groups whitespace with adjacent words for cleaner highlighting.
/// Strips leading whitespace from inverse to avoid highlighting indentation.
fn render_intra_line_diff(old_content: &str, new_content: &str) -> (String, String) {
    let word_diff = diff_words(old_content, new_content);

    let mut removed_line = String::new();
    let mut added_line = String::new();
    let mut is_first_removed = true;
    let mut is_first_added = true;

    for part in word_diff {
        if part.removed {
            let mut value = part.value;
            // Strip leading whitespace from the first removed part
            if is_first_removed {
                let leading_ws = leading_whitespace(&value);
                value = value[leading_ws.len()..].to_string();
                removed_line.push_str(&leading_ws);
                is_first_removed = false;
            }
            if !value.is_empty() {
                removed_line.push_str(&theme().inverse(&value));
            }
        } else if part.added {
            let mut value = part.value;
            // Strip leading whitespace from the first added part
            if is_first_added {
                let leading_ws = leading_whitespace(&value);
                value = value[leading_ws.len()..].to_string();
                added_line.push_str(&leading_ws);
                is_first_added = false;
            }
            if !value.is_empty() {
                added_line.push_str(&theme().inverse(&value));
            }
        } else {
            removed_line.push_str(&part.value);
            added_line.push_str(&part.value);
        }
    }

    (removed_line, added_line)
}

/// `RenderDiffOptions`
#[derive(Debug, Clone, Default)]
pub struct RenderDiffOptions {
    /// File path (unused, kept for API compatibility)
    pub file_path: Option<String>,
}

/// Render a diff string with colored lines and intra-line change highlighting.
/// - Context lines: dim/gray
/// - Removed lines: red, with inverse on changed tokens
/// - Added lines: green, with inverse on changed tokens
pub fn render_diff(diff_text: &str, _options: RenderDiffOptions) -> String {
    let lines: Vec<&str> = diff_text.split('\n').collect();
    let mut result: Vec<String> = Vec::new();

    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let parsed = parse_diff_line(line);

        let Some((prefix, line_num, content)) = parsed else {
            result.push(theme().fg("toolDiffContext", line));
            i += 1;
            continue;
        };

        if prefix == "-" {
            // Collect consecutive removed lines
            let mut removed_lines: Vec<(String, String)> = Vec::new();
            while i < lines.len() {
                match parse_diff_line(lines[i]) {
                    Some((p, line_num, content)) if p == "-" => {
                        removed_lines.push((line_num, content));
                        i += 1;
                    }
                    _ => break,
                }
            }

            // Collect consecutive added lines
            let mut added_lines: Vec<(String, String)> = Vec::new();
            while i < lines.len() {
                match parse_diff_line(lines[i]) {
                    Some((p, line_num, content)) if p == "+" => {
                        added_lines.push((line_num, content));
                        i += 1;
                    }
                    _ => break,
                }
            }

            // Only do intra-line diffing when there's exactly one removed and one added line
            // (indicating a single line modification). Otherwise, show lines as-is.
            if removed_lines.len() == 1 && added_lines.len() == 1 {
                let (removed_line_num, removed_content) = &removed_lines[0];
                let (added_line_num, added_content) = &added_lines[0];

                let (removed_line, added_line) = render_intra_line_diff(
                    &replace_tabs(removed_content),
                    &replace_tabs(added_content),
                );

                result.push(theme().fg(
                    "toolDiffRemoved",
                    &format!("-{removed_line_num} {removed_line}"),
                ));
                result
                    .push(theme().fg("toolDiffAdded", &format!("+{added_line_num} {added_line}")));
            } else {
                // Show all removed lines first, then all added lines
                for (line_num, content) in &removed_lines {
                    result.push(theme().fg(
                        "toolDiffRemoved",
                        &format!("-{line_num} {}", replace_tabs(content)),
                    ));
                }
                for (line_num, content) in &added_lines {
                    result.push(theme().fg(
                        "toolDiffAdded",
                        &format!("+{line_num} {}", replace_tabs(content)),
                    ));
                }
            }
        } else if prefix == "+" {
            // Standalone added line
            result.push(theme().fg(
                "toolDiffAdded",
                &format!("+{line_num} {}", replace_tabs(&content)),
            ));
            i += 1;
        } else {
            // Context line
            result.push(theme().fg(
                "toolDiffContext",
                &format!(" {line_num} {}", replace_tabs(&content)),
            ));
            i += 1;
        }
    }

    result.join("\n")
}

/// `BG_CLEARING_RESET` - Rewrite full/background resets to foreground-only so a
/// row's background block survives the syntax-highlighted content.
fn bg_clearing_reset() -> &'static Regex {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| Regex::new("\u{1b}\\[(?:0|49)m").expect("valid bg reset pattern"))
}

fn keep_background(highlighted: &str) -> String {
    bg_clearing_reset()
        .replace_all(highlighted, "\u{1b}[39m")
        .to_string()
}

fn highlight_content(content: &str, language: Option<&str>) -> String {
    if content.is_empty() {
        return String::new();
    }
    if let Some(language) = language {
        return keep_background(
            highlight_code(content, Some(language))
                .first()
                .cloned()
                .unwrap_or_else(|| content.to_string())
                .as_str(),
        );
    }
    theme().fg("mdCodeBlock", content)
}

struct DiffLineSpec {
    bg: String,
    gutter: String,
    gutter_fg: String,
    content: String,
    language: Option<String>,
    width: f64,
    /// Flat content color instead of syntax highlighting (256-color fallback).
    content_fg: Option<String>,
}

fn pad_to_width(inner: &str, width: f64) -> String {
    let mut inner = inner.to_string();
    if visible_width(&inner) as f64 > width {
        inner = truncate_to_width(&inner, width, "", false);
    }
    // Pad after truncating too: a 2-cell character straddling the cutoff leaves
    // the result a cell short.
    let pad = width - visible_width(&inner) as f64;
    if pad > 0.0 {
        format!("{inner}{}", " ".repeat(pad.floor() as usize))
    } else {
        inner
    }
}

// One diff line as one-or-more full-width background rows. Content wider than the
// row wraps onto continuation rows with a blank gutter, so nothing is truncated.
fn build_rich_diff_line(spec: DiffLineSpec) -> Vec<String> {
    let rendered_content = match &spec.content_fg {
        Some(content_fg) => theme().fg(content_fg, &spec.content),
        None => highlight_content(&spec.content, spec.language.as_deref()),
    };

    let gutter_width = visible_width(&spec.gutter);
    let content_width = (spec.width - gutter_width as f64).max(1.0);
    let content_rows = wrap_text_with_ansi(&rendered_content, content_width as usize);
    let content_rows = if content_rows.is_empty() {
        vec![String::new()]
    } else {
        content_rows
    };

    let styled_gutter = theme().fg(&spec.gutter_fg, &spec.gutter);
    let styled_cont = " ".repeat(gutter_width);
    content_rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let gutter = if index == 0 {
                styled_gutter.clone()
            } else {
                styled_cont.clone()
            };
            theme().bg(
                &spec.bg,
                &pad_to_width(&format!("{gutter}{row}\u{1b}[39m"), spec.width),
            )
        })
        .collect()
}

/// `RichDiffOptions`
#[derive(Debug, Clone, Default)]
pub struct RichDiffOptions {
    /// Language id for syntax highlighting the diff content (e.g. "typescript").
    pub language: Option<String>,
}

/// A dim `⋮` row separating non-adjacent hunks of one file's diff.
pub fn render_diff_separator(content_width: f64) -> String {
    let width = content_width.max(1.0);
    let marker = theme().fg("toolDiffContext", " \u{22ee}");
    let pad = (width - visible_width(&marker) as f64).max(0.0).floor() as usize;
    theme().bg("toolPanelBg", &format!("{marker}{}", " ".repeat(pad)))
}

/// Render a unified diff as full-width rows: green/red blocks, syntax-highlighted.
pub fn render_rich_diff(
    diff_text: &str,
    content_width: f64,
    options: RichDiffOptions,
) -> Vec<String> {
    let width = content_width.max(1.0);
    let language = options.language;
    // 256-color can't render subtle tints (a dark block quantizes to black), so
    // color the text instead of the background there.
    let use_blocks = matches!(
        theme().color_mode(),
        pi_tui::terminal_colors::TerminalColorMode::Truecolor
    );
    let mut rows: Vec<String> = Vec::new();

    for raw_line in diff_text.split('\n') {
        let parsed = parse_diff_line(raw_line);
        let Some((prefix, line_num, content)) = parsed else {
            rows.extend(build_rich_diff_line(DiffLineSpec {
                bg: "toolPanelBg".to_string(),
                gutter_fg: "toolDiffContext".to_string(),
                // Leading space keeps the text off the edge while the bg still reaches it.
                gutter: " ".to_string(),
                content: replace_tabs(raw_line),
                language: language.clone(),
                width,
                content_fg: None,
            }));
            continue;
        };
        let gutter = format!(
            " {line_num} {} ",
            if prefix == " " { " " } else { prefix.as_str() }
        );
        let text = replace_tabs(&content);
        if prefix == "+" {
            rows.extend(build_rich_diff_line(DiffLineSpec {
                bg: if use_blocks {
                    "toolDiffAddedBg"
                } else {
                    "toolPanelBg"
                }
                .to_string(),
                gutter_fg: "toolDiffAdded".to_string(),
                gutter,
                content: text,
                language: language.clone(),
                width,
                content_fg: if use_blocks {
                    None
                } else {
                    Some("toolDiffAdded".to_string())
                },
            }));
        } else if prefix == "-" {
            rows.extend(build_rich_diff_line(DiffLineSpec {
                bg: if use_blocks {
                    "toolDiffRemovedBg"
                } else {
                    "toolPanelBg"
                }
                .to_string(),
                gutter_fg: "toolDiffRemoved".to_string(),
                gutter,
                content: text,
                language: language.clone(),
                width,
                content_fg: if use_blocks {
                    None
                } else {
                    Some("toolDiffRemoved".to_string())
                },
            }));
        } else {
            rows.extend(build_rich_diff_line(DiffLineSpec {
                bg: "toolPanelBg".to_string(),
                gutter_fg: "toolDiffContext".to_string(),
                gutter,
                content: text,
                language: language.clone(),
                width,
                content_fg: None,
            }));
        }
    }

    rows
}

/// `generateDiffString` re-export used by the ipython cell renderer (the
/// TypeScript imports it from `core/tools/edit-diff.ts`).
pub fn generate_diff_text(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
    start_line: usize,
) -> (String, Option<usize>) {
    generate_diff_string(old_content, new_content, context_lines, start_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_diff_lines_like_the_typescript_regex() {
        let parsed = parse_diff_line("+12 hello").expect("parsed");
        assert_eq!(parsed.0, "+");
        assert_eq!(parsed.1, "12");
        assert_eq!(parsed.2, "hello");
        let context = parse_diff_line(" 3 ctx").expect("parsed");
        assert_eq!(context.0, " ");
        assert_eq!(context.2, "ctx");
        assert!(parse_diff_line("plain text").is_none());
    }

    #[test]
    fn replaces_tabs_with_three_spaces() {
        assert_eq!(replace_tabs("a\tb"), "a   b");
    }

    #[test]
    fn intra_line_diff_keeps_leading_whitespace_uninverted() {
        let (removed, added) = render_intra_line_diff("  const a = 1;", "  const a = 2;");
        assert!(removed.starts_with("  const a = "));
        assert!(added.starts_with("  const a = "));
    }

    #[test]
    fn rich_diff_emits_one_row_per_diff_line() {
        let rows = render_rich_diff(
            "+1 added\n-2 removed\n 3 ctx",
            20.0,
            RichDiffOptions::default(),
        );
        assert_eq!(rows.len(), 3);
        assert!(rows[0].contains("added"));
    }

    #[test]
    fn separator_pads_to_the_requested_width() {
        let separator = render_diff_separator(5.0);
        assert_eq!(pi_tui::utils::visible_width(&separator), 5);
    }
}
