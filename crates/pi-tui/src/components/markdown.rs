//! Port of packages/tui/src/components/markdown.ts
//!
//! The TypeScript parses with `marked`. The port parses the same CommonMark
//! subset with `pulldown-cmark` and adapts its events into a small token tree so
//! the renderer keeps the original block/inline boundaries (heading, paragraph,
//! code, blockMath, list, table, blockquote, hr, html, space, inline text,
//! strong, em, codespan, inlineMath, link, br, del).
//!
//! `marked`'s `StrictStrikethroughTokenizer` is reproduced in `strict_strikethrough`
//! (pulldown-cmark's `~~` rule is more permissive).

use std::collections::HashMap;
use std::rc::Rc;

use crate::latex::latex_to_unicode;
use crate::selection_metadata::{
    extract_table_cell_selection_regions, mark_table_cell, mark_table_end, mark_table_start,
    TableCellSelectionRegion,
};
use crate::terminal_image::{get_capabilities, hyperlink, is_image_line};
use crate::tui::Component;
use crate::utils::{apply_background_to_line, strip_ansi, visible_width, wrap_text_with_ansi};

use std::ops::Range as StdRange;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

const STRICT_STRIKETHROUGH_REGEX: &str =
    "^(~~)(?=[^\\s~])((?:\\\\.|[^\\\\])*?(?:\\\\.|[^\\s~\\\\]))\\1(?=[^~]|$)";

/// Port of `StrictStrikethroughTokenizer.del` regex behaviour: `~~text~~` where the
/// content does not start or end with whitespace or `~`, and the run after the
/// closing delimiter is not another `~`.
fn strict_strikethrough(src: &str) -> Option<(String, String)> {
    if !src.starts_with("~~") {
        return None;
    }
    let rest = &src[2..];
    let first = rest.chars().next()?;
    if first.is_whitespace() || first == '~' {
        return None;
    }

    let bytes = rest.as_bytes();
    let mut index = 0usize;
    while index + 1 < bytes.len() {
        if bytes[index] == b'\\' {
            index += 2;
            continue;
        }
        if bytes[index] == b'~' && bytes[index + 1] == b'~' {
            let inner = &rest[..index];
            let after = &rest[index + 2..];
            let last = inner.chars().last()?;
            if last.is_whitespace() || last == '~' {
                return None;
            }
            if after.starts_with('~') {
                return None;
            }
            return Some((inner.to_string(), format!("~~{inner}~~")));
        }
        index += 1;
    }
    None
}

/// Port of the `MathToken` interface.
#[derive(Clone, Debug, PartialEq)]
pub struct MathToken {
    /// "blockMath" | "inlineMath"
    pub r#type: String,
    pub raw: String,
    /// Raw LaTeX source without the delimiters.
    pub text: String,
}

/// Port of `BLOCK_MATH_REGEX` (packages/tui/src/components/markdown.ts:45):
/// `/^[ \t]*(?:\$\$([\s\S]+?)\$\$|\\\[([\s\S]+?)\\\])[ \t]*(?:\n|$)/`.
///
/// Two details of that regex are load-bearing and were missing here:
/// * the trailing `[ \t]*(?:\n|$)` - display math only wins when the closing
///   delimiter is followed by the end of the line. `$$x=2$$. Therefore y=3` does
///   NOT match, so the inline `inlineMath` extension handles it and the trailing
///   prose survives (TUIR-22).
/// * `([\s\S]+?)` needs at least one body character, so `$$$$` stays literal
///   text instead of becoming an empty block-math token (TUIR-28).
///
/// `([\s\S]+?)` is lazy but the regex engine backtracks, so `$$a=1$$ and $$b=2$$`
/// ends up matching up to the LAST `$$` with body `a=1$$ and $$b=2`; the loop below
/// reproduces that backtracking instead of blindly taking the first closing `$$`.
fn match_block_math(src: &str) -> Option<(String, String)> {
    let mut rest = src;
    let mut indent = String::new();
    for ch in src.chars() {
        if ch == ' ' || ch == '\t' {
            indent.push(ch);
            rest = &rest[ch.len_utf8()..];
        } else {
            break;
        }
    }

    let (open, close) = if rest.starts_with("$$") {
        ("$$", "$$")
    } else if rest.starts_with("\\[") {
        ("\\[", "\\]")
    } else {
        return None;
    };
    let after_open = &rest[open.len()..];

    let mut search_from = 0usize;
    while let Some(relative) = after_open[search_from..].find(close) {
        let end = search_from + relative;
        let body = &after_open[..end];
        let after_body = &after_open[end + close.len()..];
        // `[ \t]*(?:\n|$)`
        let mut trailing = String::new();
        let mut after = after_body;
        for ch in after_body.chars() {
            if ch == ' ' || ch == '\t' {
                trailing.push(ch);
                after = &after[ch.len_utf8()..];
            } else {
                break;
            }
        }
        // `([\s\S]+?)` requires a non-empty body; the regex engine then extends the
        // body until this tail test succeeds.
        if !body.is_empty() && (after.is_empty() || after.starts_with('\n')) {
            let trailing_newline = if after.starts_with('\n') { "\n" } else { "" };
            return Some((
                format!("{indent}{open}{body}{close}{trailing}{trailing_newline}"),
                body.trim().to_string(),
            ));
        }
        search_from = end + 1;
    }
    None
}

/// Port of `INLINE_MATH_PATTERNS`.
fn match_inline_math(src: &str) -> Option<(String, String)> {
    // /^\$\$([\s\S]+?)\$\$/
    if let Some(body) = src.strip_prefix("$$") {
        if let Some(end) = body
            .char_indices()
            .nth(1)
            .and_then(|(start, _)| body[start..].find("$$").map(|end| start + end))
        {
            if !body[..end].is_empty() {
                return Some((format!("$${}$$", &body[..end]), body[..end].to_string()));
            }
        }
    }
    // /^\\\[([\s\S]+?)\\\]/
    if let Some(body) = src.strip_prefix("\\[") {
        if let Some(end) = body
            .char_indices()
            .nth(1)
            .and_then(|(start, _)| body[start..].find("\\]").map(|end| start + end))
        {
            if !body[..end].is_empty() {
                return Some((format!("\\[{}\\]", &body[..end]), body[..end].to_string()));
            }
        }
    }
    // /^\\\(([\s\S]+?)\\\)/
    if let Some(body) = src.strip_prefix("\\(") {
        if let Some(end) = body
            .char_indices()
            .nth(1)
            .and_then(|(start, _)| body[start..].find("\\)").map(|end| start + end))
        {
            if !body[..end].is_empty() {
                return Some((format!("\\({}\\)", &body[..end]), body[..end].to_string()));
            }
        }
    }
    // /^\$([^\s$](?:[^$\n]*[^\s$])?)\$(?!\d)/
    if let Some(body) = src.strip_prefix('$') {
        let mut inner = String::new();
        let mut chars = body.char_indices();
        if let Some((_, first)) = chars.next() {
            if !first.is_whitespace() && first != '$' {
                inner.push(first);
                let mut end_index: Option<usize> = None;
                for (index, ch) in chars {
                    if ch == '$' {
                        end_index = Some(index);
                        break;
                    }
                    if ch == '\n' {
                        return None;
                    }
                    inner.push(ch);
                }
                if let Some(end_index) = end_index {
                    let after = &body[end_index + 1..];
                    let next_is_digit = after
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_digit())
                        .unwrap_or(false);
                    if !next_is_digit {
                        let last = inner.chars().last()?;
                        if !last.is_whitespace() && last != '$' {
                            return Some((format!("${inner}$"), inner));
                        }
                        // Single-character content still matches: `[^\s$]` alone.
                        if inner.chars().count() == 1 {
                            return Some((format!("${inner}$"), inner));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Raw-source math spans: the port's stand-in for marked's tokenizer extensions.
///
/// The TypeScript registers `blockMath` / `inlineMath` as marked extensions
/// (packages/tui/src/components/markdown.ts:54-111). marked runs extensions on the
/// RAW source, before its escape handling and before the codespan tokenizer - which
/// is why its own comment says math "must tokenize before marked's escape/emphasis
/// handling, or \[ collapses to [ and underscores inside formulas become italics"
/// (markdown.ts:40-44).
///
/// pulldown-cmark instead hands `TokenBuilder` already escape-processed `Text`
/// payloads: `\(` / `\[` lose their backslash and `\\` collapses to `\`, so the math
/// matchers could never see the original delimiters (TUIR-21). This pre-pass lifts
/// the spans out of the raw source and swaps each one for an opaque placeholder that
/// survives lexing intact; `TokenBuilder` turns the placeholders back into
/// `inlineMath` / `blockMath` tokens.
const MATH_MARKER: char = '\u{e000}';

/// One raw math span found before pulldown-cmark lexing.
#[derive(Clone)]
struct RawMathSpan {
    /// `raw` as marked reports it, delimiters included. A span that satisfies
    /// `BLOCK_MATH_REGEX` also carries the trailing `[ \t]*(?:\n|$)` the regex
    /// consumes (markdown.ts:45,73).
    raw: String,
    /// LaTeX body, `.trim()`ed like the TS extensions do (markdown.ts:73,105).
    text: String,
    /// The span matched `BLOCK_MATH_REGEX` (markdown.ts:45) and starts its block
    /// content, so it becomes a `blockMath` token instead of `inlineMath`. This is
    /// what stops `$$x=2$$. Therefore y=3` from swallowing the trailing prose: its
    /// closing `$$` is not followed by `\n` or end of input, so the span is
    /// inline-only and the tail survives (TUIR-22).
    block_ok: bool,
}

/// Placeholder standing in for span `index` in the rewritten source.
fn math_placeholder(index: usize) -> String {
    format!("{MATH_MARKER}{index}{MATH_MARKER}")
}

/// If `text` starts with a math placeholder, return `(index, placeholder length)`.
fn marker_at(text: &str) -> Option<(usize, usize)> {
    let digits = text.strip_prefix(MATH_MARKER)?;
    let count = digits.chars().take_while(|c| c.is_ascii_digit()).count();
    if count == 0 {
        return None;
    }
    let (number, rest) = digits.split_at(count);
    rest.strip_prefix(MATH_MARKER)?;
    Some((
        number.parse::<usize>().ok()?,
        MATH_MARKER.len_utf8() * 2 + count,
    ))
}

/// How the text before a math opener on its line relates to the block it opens.
///
/// marked runs `blockMathExtension` ahead of its paragraph tokenizer, on source cut
/// at the index reported by `blockMathExtension.start`
/// (packages/tui/src/components/markdown.ts:59-63). That cut only happens on the
/// paragraph path, which the fixtures in .port-env/tmp/wts_full.json, wts_mid.out and
/// wts_place.json.out pin down:
///
/// * `$$x=2$$`, `  $$x=2$$`, `- $$y=2$$`, `> $$x=1$$`, `text\n$$x=1$$` -> block math
///   (the opener starts the block content),
/// * `text $$x=2$$`, `see $$E=mc^2$$` -> `blockMath` with the preceding text kept in
///   its own paragraph token of raw `"text "` (wts_mid `M3_text_then_math_eol`,
///   wts_full `AC_math_after_text_sameline`),
/// * `- item $$x=1$$`, `# $$x=1$$`, a table row -> INLINE math: the enclosing item /
///   heading / table already claimed the line (wts_full `S_list_math`, wts_place
///   `P18_math_in_heading`, wts_mid `M13_table_math_cell`).
#[derive(PartialEq, Clone, Copy)]
enum MathPrefixKind {
    /// Nothing but indentation (or nothing at all) precedes the opener.
    Indent,
    /// A bare block marker (`> `, `- `, `> > `, `1. `) precedes the opener.
    Marker,
    /// Ordinary text precedes the opener, with no block marker on the line.
    PlainText,
    /// A block marker followed by text (`- item `), or a heading/table line.
    MarkerText,
}

/// Classify the text before a math opener on its line. See [`MathPrefixKind`].
fn math_prefix_kind(prefix: &str) -> MathPrefixKind {
    let mut rest = prefix.trim_matches([' ', '\t']);
    if rest.is_empty() {
        return MathPrefixKind::Indent;
    }
    if rest.starts_with('#') || rest.contains('|') {
        return MathPrefixKind::MarkerText;
    }
    let mut saw_marker = false;
    loop {
        let mut advanced = false;
        if let Some(after) = rest.strip_prefix('>') {
            rest = after.trim_matches([' ', '\t']);
            saw_marker = true;
            advanced = true;
        } else if let Some(after) = rest
            .strip_prefix('-')
            .or_else(|| rest.strip_prefix('+'))
            .or_else(|| rest.strip_prefix('*'))
        {
            if after.is_empty() || after.starts_with([' ', '\t']) {
                rest = after.trim_matches([' ', '\t']);
                saw_marker = true;
                advanced = true;
            }
        } else {
            let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
            if digits > 0 {
                let tail = &rest[digits..];
                if tail.starts_with(". ") || tail.starts_with(") ") {
                    rest = tail[2..].trim_matches([' ', '\t']);
                    saw_marker = true;
                    advanced = true;
                }
            }
        }
        if !advanced {
            break;
        }
    }
    match (saw_marker, rest.is_empty()) {
        (false, _) => MathPrefixKind::PlainText,
        (true, true) => MathPrefixKind::Marker,
        (true, false) => MathPrefixKind::MarkerText,
    }
}

/// Inline code spans: the math extensions sit before marked's codespan tokenizer
/// (marked.esm.js `inline()`: extensions -> escape -> tag -> link -> reflink ->
/// emStrong -> codespan), so `\(a\)` inside a code span stays literal text
/// (`R_code_with_math` in .port-env/tmp/wts_full.json). Returns the end offset, or
/// `None` for an unclosed backtick run, which CommonMark also leaves literal.
fn skip_code_span(src: &str, index: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let fence = bytes[index..].iter().take_while(|b| **b == b'`').count();
    if fence == 0 {
        return None;
    }
    let mut at = index + fence;
    while at < bytes.len() {
        if bytes[at] == b'`' {
            let run = bytes[at..].iter().take_while(|b| **b == b'`').count();
            if run == fence {
                return Some(at + run);
            }
            at += run;
            continue;
        }
        at += 1;
    }
    None
}

/// Lift every math span out of the raw source, replacing it with a placeholder.
///
/// Opener order follows `BLOCK_MATH_REGEX` (markdown.ts:45) then
/// `INLINE_MATH_PATTERNS` (markdown.ts:81-86): `$$`, `\[`, `\(`, `$`. Code spans and
/// fenced code blocks are skipped, because the TS extensions never see them.
fn extract_math_spans(source: &str) -> (String, Vec<RawMathSpan>) {
    if !has_math(source) {
        return (source.to_string(), Vec::new());
    }
    let mut spans: Vec<RawMathSpan> = Vec::new();
    let mut out = String::with_capacity(source.len());
    let mut index = 0usize;
    let mut fence: Option<(char, usize)> = None;
    let mut line_start = 0usize;
    // Preserve the enclosing list's content indent when replacing display math.
    // Pulldown already knows item source ranges, including nested/loose items.
    let list_indents: Vec<_> = Parser::new(source)
        .into_offset_iter()
        .filter_map(|(event, range)| {
            if !matches!(event, Event::Start(Tag::Item)) {
                return None;
            }
            let start = source[..range.start].rfind('\n').map_or(0, |pos| pos + 1);
            let item = &source[range.start..range.end];
            let marker = item
                .bytes()
                .take_while(|b| !b.is_ascii_whitespace())
                .count();
            let spacing = item[marker..]
                .bytes()
                .take_while(|b| *b == b' ' || *b == b'\t')
                .count();
            let indent = range.start - start + marker + spacing.clamp(1, 4);
            Some((range, indent))
        })
        .collect();

    while index < source.len() {
        let rest = &source[index..];
        let ch = rest.chars().next().unwrap();

        // Fenced code blocks (` ``` ` / `~~~`, >= 3 chars) are verbatim, exactly as
        // pulldown-cmark lexes them.
        if index == line_start {
            let indent = rest.chars().take_while(|c| *c == ' ').count();
            let body = &rest[indent..];
            if let Some(fence_char) = body.chars().next().filter(|c| *c == '`' || *c == '~') {
                let run = body.chars().take_while(|c| *c == fence_char).count();
                if run >= 3 {
                    match fence {
                        Some((open_char, open_run))
                            if open_char == fence_char && open_run == run =>
                        {
                            fence = None;
                        }
                        None => fence = Some((fence_char, run)),
                        _ => {}
                    }
                    let end = index + indent + run;
                    out.push_str(&source[index..end]);
                    index = end;
                    continue;
                }
            }
        }

        if fence.is_some() {
            out.push(ch);
            index += ch.len_utf8();
            if ch == '\n' {
                line_start = index;
            }
            continue;
        }

        if ch == '\n' {
            out.push(ch);
            index += 1;
            line_start = index;
            continue;
        }

        if ch == '`' {
            if let Some(end) = skip_code_span(source, index) {
                // `blockMathExtension.start` scans the paragraph text for `$$` / `\[`
                // before marked's codespan tokenizer runs, so a code span does not hide
                // a display-math opener from the block scan (wts_place
                // `P11_inline_code_dollars`, where the backtick stays literal text).
                let span = &source[index..end];
                let hides_block = !span.contains("$$") && !span.contains("\\[");
                if hides_block {
                    out.push_str(span);
                    index = end;
                    continue;
                }
            }
        }

        let opener: Option<&str> = if rest.starts_with("$$") {
            Some("$$")
        } else if rest.starts_with("\\[") {
            Some("\\[")
        } else if rest.starts_with("\\(") {
            Some("\\(")
        } else if ch == '$' {
            Some("$")
        } else {
            None
        };

        if let Some(open) = opener {
            // A `$$` / `\[` span whose closing delimiter is followed by
            // `[ \t]*(?:\n|$)` is display math (BLOCK_MATH_REGEX, markdown.ts:45);
            // everything else uses the inline patterns (INLINE_MATH_PATTERNS,
            // markdown.ts:81-86). `match_block_math` already backtracks, so
            // `$$a=1$$ and $$b=2$$` is one block spanning to the LAST `$$`, while
            // `$$x=2$$. Therefore y=3` is not a block at all and keeps its tail
            // (TUIR-22).
            let block_span = if open == "$$" || open == "\\[" {
                match_block_math(rest)
            } else {
                None
            };
            let (raw, math_text, block_ok) = match block_span {
                Some((raw, math_text)) => (raw, math_text, true),
                None => match match_inline_math(rest).filter(|(raw, _)| {
                    // Inline extensions run after block lexing and cannot cross
                    // a paragraph boundary, unlike display-math blocks.
                    !raw.split('\n')
                        .skip(1)
                        .take(raw.split('\n').count().saturating_sub(2))
                        .any(|line| line.trim_matches([' ', '\t']).is_empty())
                }) {
                    Some((raw, math_text)) => (raw, math_text, false),
                    None => (String::new(), String::new(), false),
                },
            };
            if !raw.is_empty() {
                let line_prefix = &source[line_start..index];
                let prefix_kind = math_prefix_kind(line_prefix);
                // A span inside a list item that already holds text stays inline: the
                // item consumed the line before the extension could run (wts_full
                // `S_list_math`, whose second item `- $$y=2$$` *is* a block).
                let block_ok = block_ok && prefix_kind != MathPrefixKind::MarkerText;
                // BLOCK_MATH_REGEX consumes the leading `[ \t]*` (markdown.ts:45). The
                // indentation already written to the output must therefore go: left in
                // place it would make pulldown-cmark lex the placeholder as an indented
                // code block (TUIR-27, wts_place `J_indented_math`).
                if block_ok && prefix_kind == MathPrefixKind::Indent {
                    let keep = list_indents
                        .iter()
                        .rev()
                        .find(|(range, _)| range.contains(&index))
                        .map_or(0, |(_, indent)| *indent)
                        .min(line_prefix.len());
                    out.truncate(out.len() - line_prefix.len() + keep);
                }
                // marked's `blockMathExtension.start` reports the math index to
                // `block()`, which cuts the pending paragraph there (markdown.ts:59-63):
                // preceding text on the same line closes as its own paragraph.
                let cut_allowed = matches!(
                    prefix_kind,
                    MathPrefixKind::PlainText | MathPrefixKind::Marker
                );
                if block_ok && cut_allowed && prefix_kind == MathPrefixKind::PlainText {
                    // Plain text before the math: the cut splits the paragraph in two.
                    out.push('\n');
                    out.push('\n');
                } else if !block_ok && cut_allowed && (open == "$$" || open == "\\[") {
                    // Same cut, but the paragraph continues: marked keeps both chunks in
                    // one paragraph joined by a newline (wts_mid `M2_midline_inline`
                    // renders `a` and `` `b` c `` on separate lines).
                    out.push('\n');
                }
                spans.push(RawMathSpan {
                    raw: raw.clone(),
                    text: math_text.trim().to_string(),
                    block_ok,
                });
                out.push_str(&math_placeholder(spans.len() - 1));
                // `BLOCK_MATH_REGEX` ends on the newline after the closing delimiter
                // (markdown.ts:45,73), so that newline is consumed by the span. Re-emit
                // it to keep the line structure: without it the next line's block marker
                // (`> text`) would be glued onto the math line (wts_full `T_quote_math`).
                // `TokenBuilder::after_block_math` drops the resulting blank line in front
                // of the paragraph that follows.
                if raw.ends_with('\n') {
                    out.push('\n');
                }
                index += raw.len();
                // The span may have consumed its line's newline; without this the next
                // line would be measured against the previous line's prefix and lose its
                // block-marker classification (e.g. the second `- $$y=2$$` item).
                line_start = index;
                continue;
            }
            if open == "$$" || open == "\\[" {
                let prefix_kind = math_prefix_kind(&source[line_start..index]);
                if prefix_kind == MathPrefixKind::PlainText && !out.ends_with('\n') {
                    out.push('\n');
                }
                // Marked looks for the next extension start after the first byte
                // when its block tokenizer rejects an empty delimiter run.
                if open == "$$" && rest[1..].starts_with("$$") {
                    out.push('$');
                    out.push('\n');
                    index += 1;
                    continue;
                }
            }
        }

        // A backslash escape of ASCII punctuation is literal text (marked's escape
        // tokenizer runs after its extensions, so this only applies when no math
        // delimiter was recognised above).
        if ch == '\\' {
            if let Some(next) = rest.chars().nth(1) {
                if next.is_ascii_punctuation() {
                    out.push_str(&source[index..index + 1 + next.len_utf8()]);
                    index += 1 + next.len_utf8();
                    continue;
                }
            }
        }

        out.push(ch);
        index += ch.len_utf8();
    }

    (out, spans)
}

/// Port of marked's `text.replace(/\r\n|\r/g, "\n")` line-ending normalisation
/// (marked.esm.js:2233, applied at the head of `Lexer.lex`, :25991).
fn normalize_line_endings(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Port of `pickMarkdownParser`.
fn has_math(text: &str) -> bool {
    text.contains('$') || text.contains("\\(") || text.contains("\\[")
}

/// Port of the `Token` shape the renderer switches on.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Heading {
        depth: usize,
        tokens: Vec<Token>,
    },
    Paragraph {
        tokens: Vec<Token>,
    },
    Code {
        text: String,
        lang: Option<String>,
    },
    BlockMath(MathToken),
    List {
        ordered: bool,
        start: usize,
        items: Vec<Vec<Token>>,
    },
    Table {
        header: Vec<Vec<Token>>,
        rows: Vec<Vec<Vec<Token>>>,
        raw: String,
    },
    Blockquote {
        tokens: Vec<Token>,
    },
    Hr,
    Html {
        raw: String,
    },
    Space,
    Text {
        text: String,
        tokens: Option<Vec<Token>>,
    },
    Strong {
        tokens: Vec<Token>,
    },
    Em {
        tokens: Vec<Token>,
    },
    Codespan {
        text: String,
    },
    InlineMath(MathToken),
    Link {
        href: String,
        text: String,
        tokens: Vec<Token>,
    },
    Br,
    Del {
        tokens: Vec<Token>,
    },
}

impl Token {
    pub fn type_name(&self) -> &'static str {
        match self {
            Token::Heading { .. } => "heading",
            Token::Paragraph { .. } => "paragraph",
            Token::Code { .. } => "code",
            Token::BlockMath(_) => "blockMath",
            Token::List { .. } => "list",
            Token::Table { .. } => "table",
            Token::Blockquote { .. } => "blockquote",
            Token::Hr => "hr",
            Token::Html { .. } => "html",
            Token::Space => "space",
            Token::Text { .. } => "text",
            Token::Strong { .. } => "strong",
            Token::Em { .. } => "em",
            Token::Codespan { .. } => "codespan",
            Token::InlineMath(_) => "inlineMath",
            Token::Link { .. } => "link",
            Token::Br => "br",
            Token::Del { .. } => "del",
        }
    }
}

/// Port of the `marked` lexer result: top-level tokens plus link definitions.
pub struct LexResult {
    pub tokens: Vec<Token>,
    pub links: Vec<String>,
}

/// Port of `pickMarkdownParser(...).lexer(text)`.
/// Top-level block source range, as `pulldown-cmark`'s offset iterator reports it.
type BlockRange = (usize, usize);

pub fn lex(text: &str) -> LexResult {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    // ENABLE_STRIKETHROUGH is deliberately NOT set. pulldown-cmark's rule accepts a
    // single `~` run, while marked's `StrictStrikethroughTokenizer` - the pinned TS
    // behaviour (markdown.ts:14-31) - requires `~~` and rejects whitespace/`~` at the
    // edges; `markdown.test.ts:1048-1057` asserts `Use ~strikethrough~ literally`
    // stays literal text. pulldown claims the run before `TokenBuilder::text` could
    // call `strict_strikethrough` on it, so the run must reach `text()` as plain text
    // and be split there instead.
    let use_math = has_math(text);
    // Marked's math tokenizers run on the raw source; pulldown-cmark lexes
    // escape-processed text, so math spans are lifted out first (TUIR-21).
    let (lex_source, math_spans) = extract_math_spans(text);
    let parser = Parser::new_ext(&lex_source, options);
    let mut builder = TokenBuilder::new(use_math, math_spans);
    // Block ranges are read from the offset iterator: each top-level block is the
    // outermost `Start`/`End` pair, which is what `block_ranges` records so the
    // blank-line pass can tell a between-blocks run from one inside a block.
    for (event, range) in parser.into_offset_iter() {
        builder.handle_at(event, range);
    }
    let (mut result, block_ranges) = builder.finish(text);
    // marked's block loop checks its `space` tokenizer first on every iteration
    // (marked.esm.js:26185), so a blank-line run between two top-level blocks becomes a
    // `space` token that renders one empty line (markdown.ts:553-555). pulldown-cmark
    // has no such event, so the runs are recovered from the source. Offsets are taken
    // over `lex_source`, the same text pulldown lexes: a math span that already
    // contains blank lines is a placeholder there, matching the extensions claiming it
    // before marked's block loop could.
    insert_space_tokens(&mut result.tokens, &lex_source, &block_ranges);
    fill_table_raw(&mut result.tokens, text);
    result
}

/// Runs of whitespace-only lines, as `(start, len)`, matching marked's `newline`
/// tokenizer rule `/^(?:[ \t]*(?:\n|$))+/` (marked.esm.js:2808). A run of exactly one
/// character is not a `space` token: marked appends it to the previous token's raw
/// (`r.raw.length === 1 && o !== undefined ? o.raw += "\n"`, marked.esm.js:26185),
/// which is why `"para\n"` lexes to `[paragraph]` while `"para\n\n"` also yields
/// `[paragraph, space]`.
fn blank_line_runs(source: &str) -> Vec<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let start = offset;
        // `(?:[ \t]*(?:\n|$))+`: one repetition is optional blanks plus a newline,
        // or optional blanks at the very end of the source (`$` is not multiline).
        loop {
            let mut after_blanks = offset;
            while after_blanks < bytes.len()
                && (bytes[after_blanks] == b' ' || bytes[after_blanks] == b'\t')
            {
                after_blanks += 1;
            }
            if after_blanks < bytes.len() && bytes[after_blanks] == b'\n' {
                offset = after_blanks + 1;
                continue;
            }
            if after_blanks == bytes.len() {
                offset = after_blanks;
                break;
            }
            break;
        }
        if offset == start {
            // No match at this offset; the regex is unanchored, so advance one byte.
            offset = start + 1;
            continue;
        }
        // A one-byte match is not a `space` token (marked appends it to the previous
        // token's raw), so only longer runs are kept.
        if offset - start > 1 {
            runs.push((start, offset - start));
        }
    }
    runs
}

/// Insert a `Token::Space` for every top-level blank-line run, in source order.
///
/// `blocks[i]` is the `(start, end)` source range of the top-level block that produced
/// token `i`; a run is placed after every block that starts before it. A run inside a
/// top-level block (a loose list, an indented code block) belongs to that block and is
/// left to it, exactly as marked's recursive `blockTokens` call for that block does
/// (marked.esm.js:26185).
fn insert_space_tokens(tokens: &mut Vec<Token>, source: &str, blocks: &[BlockRange]) {
    // A run belongs to a block when it starts before that block's last line of
    // content: a loose list (`- a\n\n- b`) owns its blank line, and marked's recursive
    // `blockTokens` for the list turns it into per-item spacing rather than a `space`
    // token (marked.esm.js:26185). A run that starts at or after the block's content
    // end follows the block instead, which is the `space` token marked emits - its list
    // tokenizer stops at the last item's line, leaving the trailing newline to the
    // `newline` tokenizer (`"- a\n- b\n\npara"` -> `[list, space("\n\n"), paragraph]`).
    let runs: Vec<(usize, usize)> = blank_line_runs(source)
        .into_iter()
        .filter(|(start, _)| {
            !blocks.iter().any(|(block_start, block_end)| {
                let slice = &source[*block_start..*block_end];
                let content_end = *block_start + slice.trim_end().len();
                *block_start <= *start && content_end > *start
            })
        })
        .collect();
    if runs.is_empty() {
        return;
    }
    let mut insertions: Vec<(usize, Token)> = Vec::new();
    for (start, _) in runs {
        let index = blocks
            .iter()
            .take_while(|(block_start, _)| *block_start < start)
            .count();
        insertions.push((index, Token::Space));
    }
    for (index, token) in insertions.into_iter().rev() {
        tokens.insert(index.min(tokens.len()), token);
    }
}

/// Port of `token.raw` for tables. `marked` exposes the matched source text;
/// `pulldown-cmark` does not, so the port re-reads the table block from the source
/// in document order.
fn fill_table_raw(tokens: &mut [Token], source: &str) {
    let raws = collect_table_raw_blocks(source);
    let mut index = 0usize;
    for token in tokens.iter_mut() {
        if let Token::Table { raw, .. } = token {
            if let Some(block) = raws.get(index) {
                *raw = block.clone();
            }
            index += 1;
        }
    }
}

/// Collects raw markdown table blocks: a header line containing `|` followed by a
/// delimiter row of `-`, `:`, `|` and spaces.
fn collect_table_raw_blocks(source: &str) -> Vec<String> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut blocks: Vec<String> = Vec::new();
    let mut index = 0usize;
    while index + 1 < lines.len() {
        let header = lines[index];
        let delimiter = lines[index + 1];
        if is_table_header_line(header) && is_table_delimiter_line(delimiter) {
            let mut block: Vec<&str> = vec![header, delimiter];
            let mut next = index + 2;
            while next < lines.len() && is_table_row_line(lines[next]) {
                block.push(lines[next]);
                next += 1;
            }
            blocks.push(block.join("\n"));
            index = next;
            continue;
        }
        index += 1;
    }
    blocks
}

fn is_table_header_line(line: &str) -> bool {
    line.contains('|') && !line.trim().is_empty()
}

fn is_table_delimiter_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.contains('-') {
        return false;
    }
    trimmed
        .chars()
        .all(|ch| ch == '-' || ch == ':' || ch == '|' || ch == ' ')
}

fn is_table_row_line(line: &str) -> bool {
    line.contains('|') && !line.trim().is_empty()
}

enum FrameKind {
    Root,
    Paragraph,
    Heading(usize),
    CodeBlock {
        lang: Option<String>,
        text: String,
    },
    Item,
    List {
        ordered: bool,
        start: usize,
        items: Vec<Vec<Token>>,
    },
    Blockquote,
    Table {
        header: Vec<Vec<Token>>,
        rows: Vec<Vec<Vec<Token>>>,
        raw: String,
    },
    TableHead,
    TableRow,
    TableCell,
    Strong,
    Em,
    /// Port of marked's `del` token: the TS renderer wraps the content in
    /// `this.theme.strikethrough(...)` (packages/tui/src/components/markdown.ts:645-648),
    /// NOT `theme.italic`. `Token::Del` already renders that way; pulldown's
    /// `Tag::Strikethrough` previously collapsed into `Em` and so rendered as italics.
    Del,
    Link {
        href: String,
        text: String,
    },
}

struct Frame {
    kind: FrameKind,
    tokens: Vec<Token>,
}

struct TokenBuilder {
    stack: Vec<Frame>,
    /// Source range of each top-level block, in document order.
    block_ranges: Vec<BlockRange>,
    block_start: usize,
    depth: usize,
    use_math: bool,
    /// Math spans lifted from the raw source, indexed by the placeholder number
    /// that stands in for them in the lexed text.
    math_spans: Vec<RawMathSpan>,
    /// Set right after a block-math split so the next text event can drop the blank
    /// line the split left in front of the following paragraph.
    after_block_math: bool,
}

impl TokenBuilder {
    fn new(use_math: bool, math_spans: Vec<RawMathSpan>) -> Self {
        Self {
            stack: vec![Frame {
                kind: FrameKind::Root,
                tokens: Vec::new(),
            }],
            block_ranges: Vec::new(),
            block_start: 0,
            depth: 0,
            use_math,
            math_spans,
            after_block_math: false,
        }
    }

    /// Emit a `blockMath` token for `span`, splitting the enclosing paragraph the way
    /// marked does.
    ///
    /// `blockMathExtension.start` (packages/tui/src/components/markdown.ts:59-63)
    /// reports the math index to marked's `block()` loop, which cuts the pending
    /// paragraph there (`paragraph(i)` with `i` ending at the math position). The
    /// preceding text therefore closes as its own paragraph, the math token lands in
    /// the enclosing block, and the text that follows starts a fresh paragraph -
    /// visible in the fixtures as `paragraph raw="text\n"`, `blockMath`,
    /// `paragraph raw="more"` (.port-env/tmp/wts_place.json.out `P1_text_then_math_line`).
    /// Suppressing the empty trailing paragraph matches marked, which emits no
    /// paragraph token at all for `$$x=2$$` on its own.
    fn split_paragraph_for_block_math(&mut self, span: RawMathSpan) {
        if matches!(
            self.stack.last().map(|frame| &frame.kind),
            Some(FrameKind::Paragraph)
        ) {
            let mut frame = self.stack.pop().unwrap();
            // The line break that put the math on its own line is part of the
            // paragraph in pulldown-cmark but only of `token.raw` in marked, whose
            // `Text` token stops at `"text"` (wts_place `P1_text_then_math_line`).
            while matches!(
                frame.tokens.last(),
                Some(Token::Text { text, tokens: None }) if text.trim().is_empty()
            ) {
                frame.tokens.pop();
            }
            if !frame.tokens.is_empty() {
                self.push_token(Token::Paragraph {
                    tokens: frame.tokens,
                });
            }
        }
        self.push_token(Token::BlockMath(MathToken {
            r#type: "blockMath".to_string(),
            raw: span.raw,
            text: span.text,
        }));
        self.stack.push(Frame {
            kind: FrameKind::Paragraph,
            tokens: Vec::new(),
        });
        self.after_block_math = true;
    }

    /// Port of marked's inline math extensions (markdown.ts:81-111): each placeholder
    /// written by `extract_math_spans` becomes `inlineMath` (or `blockMath` when the
    /// span satisfied `BLOCK_MATH_REGEX`) carrying the raw source text, so neither
    /// `raw` nor `text` is ever escape-processed.
    fn token_text(&mut self, text: &str) {
        // Inside a code block the raw text is collected verbatim and math never runs:
        // the TS math extensions are unreachable from a fenced code block
        // (wts_full `U_fence_math` renders `  $$x=1$$` unchanged).
        if matches!(
            self.stack.last().map(|frame| &frame.kind),
            Some(FrameKind::CodeBlock { .. })
        ) {
            self.text(text);
            return;
        }
        // The blank line left by a block-math split is consumed in `text`; anything
        // still flagged here is real content, so only the flag is cleared.
        let text = if self.after_block_math {
            self.after_block_math = false;
            text.trim_start_matches('\n')
        } else {
            text
        };
        let mut pending = String::new();
        let mut index = 0usize;
        while index < text.len() {
            let rest = &text[index..];
            let mut matched = false;
            if rest.starts_with(MATH_MARKER) {
                if let Some((span_index, length)) = marker_at(rest) {
                    // Take an owned copy first: `split_paragraph_for_block_math` needs
                    // `&mut self`, so no borrow of `self.math_spans` may stay alive.
                    let span = self.math_spans.get(span_index).cloned();
                    if let Some(span) = span {
                        // Text before the math stays in the paragraph marked cut off.
                        let preceding = std::mem::take(&mut pending);
                        if !preceding.is_empty() {
                            self.push_token(Token::Text {
                                text: preceding,
                                tokens: None,
                            });
                        }
                        if span.block_ok {
                            // marked cut the paragraph here, so the math closes it.
                            self.split_paragraph_for_block_math(span);
                        } else {
                            self.push_token(Token::InlineMath(MathToken {
                                r#type: "inlineMath".to_string(),
                                raw: span.raw,
                                text: span.text,
                            }));
                        }
                        index += length;
                        matched = true;
                    }
                }
            }
            // `StrictStrikethroughTokenizer.del` (markdown.ts:16-31): pulldown-cmark's
            // own strikethrough rule is not enabled in `lex`, so the `~~` run reaches
            // here as ordinary text and marked's stricter rule is applied directly.
            // marked checks `del` before `url` (marked.esm.js:28659).
            if !matched && rest.starts_with("~~") {
                if let Some((inner, _raw)) = strict_strikethrough(rest) {
                    let preceding = std::mem::take(&mut pending);
                    if !preceding.is_empty() {
                        self.push_token(Token::Text {
                            text: preceding,
                            tokens: None,
                        });
                    }
                    let inner_tokens = inline_tokens(&inner, self.use_math);
                    self.push_token(Token::Del {
                        tokens: inner_tokens,
                    });
                    index += 4 + inner.len();
                    continue;
                }
            }
            // marked's GFM `url` rule runs before its `inlineText` rule
            // (marked.esm.js:28659: extensions -> escape -> tag -> link -> reflink ->
            // emStrong -> codespan -> br -> del -> autolink -> url -> text), so an
            // `https://` / `www.` host or an email is a link token in every plain-text
            // position, including inside emphasis and link labels.
            if !matched {
                if let Some((href, text, raw_len)) = self.autolink_at(rest) {
                    let preceding = std::mem::take(&mut pending);
                    if !preceding.is_empty() {
                        self.push_token(Token::Text {
                            text: preceding,
                            tokens: None,
                        });
                    }
                    self.push_token(Token::Link {
                        href,
                        text: text.clone(),
                        tokens: vec![Token::Text { text, tokens: None }],
                    });
                    index += raw_len;
                    continue;
                }
            }
            if matched {
                continue;
            }
            let ch = rest.chars().next().unwrap();
            pending.push(ch);
            index += ch.len_utf8();
        }
        flush_text(&mut pending, &mut |token| self.push_token(token));
    }

    /// Bare-URL / email autolink at `rest` (marked's GFM `url` rule). `None` when
    /// `rest` does not start one, or when the current frame is inside a link
    /// (`!this.state.inLink`, marked.esm.js:28659).
    fn autolink_at(&self, rest: &str) -> Option<(String, String, usize)> {
        if self
            .stack
            .iter()
            .any(|frame| matches!(frame.kind, FrameKind::Link { .. }))
        {
            return None;
        }
        if let Some((href, text)) = match_autolink(rest) {
            return Some((href, text.clone(), text.len()));
        }
        match_email_autolink(rest).map(|(href, text)| {
            let len = text.len();
            (href, text, len)
        })
    }

    /// Put the original math source back where a placeholder landed in verbatim text
    /// (HTML blocks and inline HTML), which the math extensions never rewrite.
    fn restore_math_placeholders(&self, text: &str) -> String {
        if !text.contains(MATH_MARKER) {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(offset) = rest.find(MATH_MARKER) {
            out.push_str(&rest[..offset]);
            rest = &rest[offset..];
            match marker_at(rest) {
                Some((index, length)) => {
                    match self.math_spans.get(index) {
                        Some(span) => out.push_str(&span.raw),
                        None => out.push_str(&rest[..length]),
                    }
                    rest = &rest[length..];
                }
                None => {
                    out.push(MATH_MARKER);
                    rest = &rest[MATH_MARKER.len_utf8()..];
                }
            }
        }
        out.push_str(rest);
        out
    }

    fn push_token(&mut self, token: Token) {
        if let Some(frame) = self.stack.last_mut() {
            frame.tokens.push(token);
        }
    }

    /// Handle one event together with its source range, recording the top-level
    /// block ranges `insert_space_tokens` needs.
    fn handle_at(&mut self, event: Event<'_>, range: StdRange<usize>) {
        match &event {
            Event::Start(_) => {
                if self.depth == 0 {
                    self.block_start = range.start;
                }
                self.depth += 1;
            }
            Event::End(_) => {
                self.depth = self.depth.saturating_sub(1);
                if self.depth == 0 {
                    self.block_ranges.push((self.block_start, range.end));
                }
            }
            _ => {}
        }
        self.handle(event);
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            // Math spans are lifted out of the raw source before lexing
            // (`extract_math_spans`). A `BLOCK_MATH_REGEX` match is emitted as a
            // `blockMath` token here, mirroring marked's `blockMathExtension` firing
            // at a block boundary (markdown.ts:54-76); all other placeholders are
            // turned into `inlineMath` by `token_text`.
            Event::Text(text) => self.token_text(&text),
            Event::Code(text) => self.push_token(Token::Codespan {
                text: text.to_string(),
            }),
            Event::Html(raw) | Event::InlineHtml(raw) => {
                // An HTML block is tokenized by marked's `html` tokenizer, so its raw
                // source is preserved verbatim and never reaches the math extensions
                // (wts_place `P19_math_html_block` keeps `$$x=1$$` inside the block).
                self.push_token(Token::Html {
                    raw: self.restore_math_placeholders(&raw),
                })
            }
            Event::SoftBreak => self.text("\n"),
            Event::HardBreak => self.push_token(Token::Br),
            Event::Rule => self.push_token(Token::Hr),
            Event::FootnoteReference(name) => self.text(&format!("[^{name}]")),
            Event::TaskListMarker(checked) => self.text(if checked { "[x] " } else { "[ ] " }),
            // pulldown-cmark only emits these with `ENABLE_MATH`, which the port
            // does not set; the TypeScript `marked` math extensions are ported
            // directly above instead.
            Event::InlineMath(_) | Event::DisplayMath(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        let kind = match tag {
            Tag::Paragraph => FrameKind::Paragraph,
            Tag::Heading { level, .. } => FrameKind::Heading(heading_depth(level)),
            Tag::BlockQuote(_) => FrameKind::Blockquote,
            Tag::CodeBlock(kind) => FrameKind::CodeBlock {
                lang: match kind {
                    CodeBlockKind::Fenced(info) => {
                        let info = info.trim().to_string();
                        if info.is_empty() {
                            None
                        } else {
                            Some(info)
                        }
                    }
                    CodeBlockKind::Indented => None,
                },
                text: String::new(),
            },
            Tag::List(start) => FrameKind::List {
                ordered: start.is_some(),
                start: start.unwrap_or(1) as usize,
                items: Vec::new(),
            },
            Tag::Item => FrameKind::Item,
            Tag::Table(_) => FrameKind::Table {
                header: Vec::new(),
                rows: Vec::new(),
                raw: String::new(),
            },
            Tag::TableHead => FrameKind::TableHead,
            Tag::TableRow => FrameKind::TableRow,
            Tag::TableCell => FrameKind::TableCell,
            Tag::Emphasis => FrameKind::Em,
            Tag::Strong => FrameKind::Strong,
            Tag::Link { dest_url, .. } => FrameKind::Link {
                href: dest_url.to_string(),
                text: String::new(),
            },
            Tag::Image { dest_url, .. } => FrameKind::Link {
                href: dest_url.to_string(),
                text: String::new(),
            },
            // pulldown-cmark's strikethrough rule accepts a single `~` run, marked's
            // `StrictStrikethroughTokenizer` and GFM require `~~` (markdown.ts:14-31,
            // markdown.test.ts:1048-1057). pulldown consumes the run before
            // `TokenBuilder::text` can reject it, so the frame records the run's length
            // and `end()` discards the pair when it is not two tildes.
            Tag::Strikethrough => FrameKind::Del,
            Tag::HtmlBlock => FrameKind::Paragraph,
            Tag::FootnoteDefinition(_) => FrameKind::Paragraph,
            Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {
                FrameKind::Paragraph
            }
            Tag::MetadataBlock(_) => FrameKind::Paragraph,
        };
        self.stack.push(Frame {
            kind,
            tokens: Vec::new(),
        });
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::TableHead | TagEnd::TableRow => {
                let frame = match self.stack.pop() {
                    Some(frame) => frame,
                    None => return,
                };
                let cells = cells_from_row(&frame.tokens);
                if let Some(Frame {
                    kind: FrameKind::Table { header, rows, .. },
                    ..
                }) = self.stack.last_mut()
                {
                    if tag == TagEnd::TableHead {
                        *header = cells;
                    } else {
                        rows.push(cells);
                    }
                }
                return;
            }
            TagEnd::TableCell => {
                let frame = match self.stack.pop() {
                    Some(frame) => frame,
                    None => return,
                };
                self.push_token(Token::Paragraph {
                    tokens: frame.tokens,
                });
                return;
            }
            _ => {}
        }

        if self.stack.len() <= 1 {
            return;
        }
        let frame = self.stack.pop().unwrap();
        let token = match frame.kind {
            FrameKind::Root => return,
            FrameKind::Paragraph => {
                // The math split can leave an empty paragraph frame behind; marked
                // emits no paragraph token for it, so it must not render a blank line.
                if frame.tokens.is_empty() {
                    return;
                }
                if let Some(token) = self.block_math_from_paragraph(&frame.tokens) {
                    token
                } else {
                    Token::Paragraph {
                        tokens: frame.tokens,
                    }
                }
            }
            FrameKind::Heading(depth) => Token::Heading {
                depth,
                tokens: frame.tokens,
            },
            FrameKind::CodeBlock { lang, text } => Token::Code {
                // `marked` reports fenced code content without the trailing newline.
                text: text
                    .strip_suffix('\n')
                    .map(|t| t.to_string())
                    .unwrap_or(text),
                lang,
            },
            FrameKind::Item => {
                if let Some(Frame {
                    kind: FrameKind::List { items, .. },
                    ..
                }) = self.stack.last_mut()
                {
                    items.push(frame.tokens);
                }
                return;
            }
            FrameKind::List {
                ordered,
                start,
                items,
            } => Token::List {
                ordered,
                start,
                items,
            },
            FrameKind::Blockquote => Token::Blockquote {
                tokens: frame.tokens,
            },
            FrameKind::Table { header, rows, raw } => Token::Table { header, rows, raw },
            // Table scaffolding frames are consumed by their own `TagEnd` arms.
            FrameKind::TableHead | FrameKind::TableRow | FrameKind::TableCell => return,
            FrameKind::Strong => Token::Strong {
                tokens: frame.tokens,
            },
            FrameKind::Em => Token::Em {
                tokens: frame.tokens,
            },
            FrameKind::Del => Token::Del {
                tokens: frame.tokens,
            },
            FrameKind::Link { href, .. } => {
                let text = collect_text(&frame.tokens);
                Token::Link {
                    href,
                    text,
                    tokens: frame.tokens,
                }
            }
        };
        self.push_token(token);
    }

    /// Port of the `blockMath` tokenizer extension: a paragraph that is exactly one
    /// display-math block becomes a `blockMath` token.
    fn block_math_from_paragraph(&self, tokens: &[Token]) -> Option<Token> {
        if !self.use_math {
            return None;
        }
        let text = match tokens {
            [Token::Text { text, tokens: None }] => text.clone(),
            _ => return None,
        };
        let (raw, math_text) = match_block_math(&text)?;
        Some(Token::BlockMath(MathToken {
            r#type: "blockMath".to_string(),
            raw,
            text: math_text,
        }))
    }

    fn text(&mut self, text: &str) {
        // Inside a code block the raw text is collected verbatim.
        if let Some(Frame {
            kind: FrameKind::CodeBlock { text: code, .. },
            ..
        }) = self.stack.last_mut()
        {
            code.push_str(text);
            return;
        }

        // The line break that followed a block-math span carries no content: marked's
        // paragraph tokenizer drops it (wts_place `P9_math_then_blank` renders
        // `paragraph raw="text"`, not `"\ntext"`).
        if self.after_block_math && text.trim().is_empty() {
            self.after_block_math = false;
            return;
        }
        self.after_block_math = false;

        let mut pending = String::new();
        let mut index = 0usize;
        while index < text.len() {
            let rest = &text[index..];
            if self.use_math && (rest.starts_with('$') || rest.starts_with('\\')) {
                if let Some((raw, math_text)) = match_inline_math(rest) {
                    flush_text(&mut pending, &mut |token| self.push_token(token));
                    self.push_token(Token::InlineMath(MathToken {
                        r#type: "inlineMath".to_string(),
                        raw: raw.clone(),
                        text: math_text,
                    }));
                    index += raw.len();
                    continue;
                }
            }
            let ch = rest.chars().next().unwrap();
            pending.push(ch);
            index += ch.len_utf8();
        }
        flush_text(&mut pending, &mut |token| self.push_token(token));
    }

    fn finish(mut self, text: &str) -> (LexResult, Vec<BlockRange>) {
        while self.stack.len() > 1 {
            self.end(TagEnd::Paragraph);
        }
        let frame = self.stack.pop().unwrap();
        let tokens = frame.tokens;
        let blocks = self.block_ranges;
        // Port of `Object.keys(tokens.links).length === 0`: link reference
        // definitions disable per-block caching.
        let mut links: Vec<String> = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('[') {
                if let Some(close) = trimmed.find("]:") {
                    if close > 1 {
                        links.push(trimmed[1..close].to_string());
                    }
                }
            }
        }
        (LexResult { tokens, links }, blocks)
    }
}

/// Port of `token.header[i].tokens` / `row[i].tokens`: each table cell frame wraps
/// its inline tokens in a paragraph token.
fn cells_from_row(tokens: &[Token]) -> Vec<Vec<Token>> {
    tokens
        .iter()
        .map(|token| match token {
            Token::Paragraph { tokens } => tokens.clone(),
            other => vec![other.clone()],
        })
        .collect()
}

/// Port of marked's GFM `url` inline rule (`Q.url`, marked.esm.js:24802 and its
/// regex `/^((?:protocol):\/\/|www\.)(?:[a-zA-Z0-9\-]+\.?)+[^\s<]*|^email/`
/// with `protocol=/[hH][tT][tT][pP][sS]?|[fF][tT][pP]/` and
/// `email=/[A-Za-z0-9._+-]+(@)[a-zA-Z0-9-_]+(?:\.[a-zA-Z0-9-_]*[a-zA-Z0-9])+(?![-_])/`)
/// plus the `_backpedal` cleanup marked applies to the match
/// (`J._backpedal` = `/(?:[^?!.,:;*_'"~()&]+|\([^)]*\)|&(?![a-zA-Z0-9]+;$)|[?!.,:;*_'"~)]+(?!$))+/`).
///
/// marked tokenizes bare URLs, `www.` hosts and emails into `link` tokens
/// (markdown.test.ts:1065-1087, 1144-1156); pulldown-cmark has no GFM autolink and
/// `ENABLE_GFM` covers only blockquote tags, so the rule is ported here.
///
/// Returns `(href, text)` exactly as marked's `url()` builds them: `www.` hosts gain
/// an `http://` scheme, emails a `mailto:` prefix, and a scheme URL keeps its own
/// text. The renderer's `text == href` comparison (markdown.ts:632-636) then prints
/// the parenthesised target only for the `www.` case, matching the TS output.
fn match_autolink(src: &str) -> Option<(String, String)> {
    let scheme_len = scheme_prefix_len(src)?;
    let rest = &src[scheme_len..];
    let mut body_len = 0usize;
    let mut labels = 0usize;
    // `(?:[a-zA-Z0-9\-]+\.?)+` needs at least one label ...
    loop {
        let label = rest[body_len..]
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-')
            .count();
        if label == 0 {
            break;
        }
        body_len += label;
        labels += 1;
        if rest[body_len..].starts_with('.') {
            body_len += 1;
        }
        if rest[body_len..].starts_with('.') {
            // `\.?` allows only one trailing dot per label.
            break;
        }
    }
    if labels == 0 {
        return None;
    }
    // ... then `[^\s<]*` takes the rest of the token.
    body_len += rest[body_len..]
        .bytes()
        .take_while(|b| *b != b' ' && *b != b'\t' && *b != b'\n' && *b != b'<')
        .count();
    let matched = &src[..scheme_len + body_len];
    let matched = backpedal(matched);
    if matched.is_empty() {
        return None;
    }
    let href = if scheme_len == WWW_SCHEME_LEN {
        format!("http://{matched}")
    } else {
        matched.to_string()
    };
    Some((href, matched.to_string()))
}

/// `www.` is not a scheme, but marked's rule treats it as one and prefixes `http://`.
const WWW_SCHEME_LEN: usize = 4;

/// Match `(?:[hH][tT][tT][pP][sS]?|[fF][tT][pP]):\/\/` or `www.` at the start.
/// Returns the length of that prefix.
fn scheme_prefix_len(src: &str) -> Option<usize> {
    let bytes = src.as_bytes();
    let http = |b: &[u8], offset: usize| {
        b.len() >= offset + 4
            && (b[offset] | 0x20) == b'h'
            && (b[offset + 1] | 0x20) == b't'
            && (b[offset + 2] | 0x20) == b't'
            && (b[offset + 3] | 0x20) == b'p'
    };
    if http(bytes, 0) {
        let mut len = 4;
        if bytes.len() > 4 && (bytes[4] | 0x20) == b's' {
            len = 5;
        }
        if bytes.len() >= len + 3 && &bytes[len..len + 3] == b"://" {
            return Some(len + 3);
        }
    }
    if bytes.len() >= 6
        && (bytes[0] | 0x20) == b'f'
        && (bytes[1] | 0x20) == b't'
        && (bytes[2] | 0x20) == b'p'
        && &bytes[3..6] == b"://"
    {
        return Some(6);
    }
    // `www.` is pure ASCII, so a byte-length guard plus `is_char_boundary` keeps the
    // slice safe for a multi-byte character at the same offset (the math placeholder
    // is U+E000, three bytes long).
    if bytes.len() >= WWW_SCHEME_LEN
        && src.is_char_boundary(WWW_SCHEME_LEN)
        && src[..WWW_SCHEME_LEN].eq_ignore_ascii_case("www.")
    {
        return Some(WWW_SCHEME_LEN);
    }
    None
}

/// Match `/email` (`[A-Za-z0-9._+-]+(@)[a-zA-Z0-9-_]+(?:\.[a-zA-Z0-9-_]*[a-zA-Z0-9])+(?![-_])/`)
/// at the start of `src`. Returns `(href, text)` with marked's `mailto:` scheme.
fn match_email_autolink(src: &str) -> Option<(String, String)> {
    let local = src
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || b"._+-".contains(b))
        .count();
    if local == 0 || !src.is_char_boundary(local) || !src[local..].starts_with('@') {
        return None;
    }
    let domain_start = local + 1;
    let first = src[domain_start..]
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
        .count();
    if first == 0 {
        return None;
    }
    let mut end = domain_start + first;
    let mut dots = 0usize;
    loop {
        if !src[end..].starts_with('.') {
            break;
        }
        let label_start = end + 1;
        // `[a-zA-Z0-9-_]*[a-zA-Z0-9]` - a label must end in an alphanumeric.
        let span = src[label_start..]
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
            .count();
        let trailing = src[label_start..label_start + span]
            .bytes()
            .rev()
            .take_while(|b| *b == b'-' || *b == b'_')
            .count();
        if span - trailing == 0 {
            break;
        }
        end = label_start + span - trailing;
        dots += 1;
    }
    if dots == 0 {
        return None;
    }
    // `(?![-_])`
    if src[end..].starts_with('-') || src[end..].starts_with('_') {
        return None;
    }
    let matched = backpedal(&src[..end]);
    if matched.is_empty() {
        return None;
    }
    Some((format!("mailto:{matched}"), matched.to_string()))
}

/// Port of marked's `_backpedal` cleanup: strip the trailing punctuation the URL
/// regexes greedily swallow, keep balanced `(...)` groups, and keep a trailing `&`
/// unless it closes an HTML entity (`&(?![a-zA-Z0-9]+;$)`).
fn backpedal(text: &str) -> &str {
    let mut current = text;
    loop {
        let trimmed = backpedal_once(current);
        if trimmed.len() == current.len() {
            return trimmed;
        }
        current = trimmed;
    }
}

/// One greedy pass of `_backpedal` from offset 0. The regex is an alternation, so
/// the scan tries each branch at the current offset and advances by the longest one:
/// a plain run, a balanced `(...)` group, a non-entity `&`, then a punctuation run
/// that is not the end of the string.
fn backpedal_once(text: &str) -> &str {
    const PUNCT: &str = "?!.,:;*_'\"~)";
    let mut offset = 0usize;
    while offset < text.len() {
        let rest = &text[offset..];
        let mut advanced = 0usize;
        // `[^?!.,:;*_'\"~()&]+`
        for ch in rest.chars() {
            if PUNCT.contains(ch) || ch == '(' || ch == '&' {
                break;
            }
            advanced += ch.len_utf8();
        }
        if advanced == 0 {
            // `\([^)]*\)`
            if rest.starts_with('(') {
                if let Some(close) = rest.find(')') {
                    advanced = close + 1;
                }
            }
        }
        if advanced == 0 && rest.starts_with('&') {
            // `&(?![a-zA-Z0-9]+;$)` - a lone `&` is kept, an entity terminator is not.
            let tail = &rest[1..];
            let word = tail
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric())
                .count();
            if !(word > 0 && tail[word..].starts_with(';') && tail[word + 1..].is_empty()) {
                advanced = 1;
            }
        }
        if advanced == 0 {
            // `[?!.,:;*_'\"~)]+(?!$)`
            let run = rest.chars().take_while(|ch| PUNCT.contains(*ch)).count();
            if run > 0 && run < rest.chars().count() {
                advanced = rest.chars().take(run).map(char::len_utf8).sum();
            }
        }
        if advanced == 0 {
            break;
        }
        offset += advanced;
    }
    &text[..offset]
}

fn flush_text(pending: &mut String, push: &mut impl FnMut(Token)) {
    if !pending.is_empty() {
        push(Token::Text {
            text: std::mem::take(pending),
            tokens: None,
        });
    }
}

/// Inline tokenization of a plain string (used for strict strikethrough content).
fn inline_tokens(text: &str, use_math: bool) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut pending = String::new();
    let mut index = 0usize;
    while index < text.len() {
        let rest = &text[index..];
        if rest.starts_with("~~") {
            if let Some((inner, _raw)) = strict_strikethrough(rest) {
                if !pending.is_empty() {
                    tokens.push(Token::Text {
                        text: std::mem::take(&mut pending),
                        tokens: None,
                    });
                }
                tokens.push(Token::Del {
                    tokens: inline_tokens(&inner, use_math),
                });
                index += 4 + inner.len();
                continue;
            }
        }
        if use_math && (rest.starts_with('$') || rest.starts_with('\\')) {
            if let Some((raw, math_text)) = match_inline_math(rest) {
                if !pending.is_empty() {
                    tokens.push(Token::Text {
                        text: std::mem::take(&mut pending),
                        tokens: None,
                    });
                }
                tokens.push(Token::InlineMath(MathToken {
                    r#type: "inlineMath".to_string(),
                    raw: raw.clone(),
                    text: math_text,
                }));
                index += raw.len();
                continue;
            }
        }
        // Same GFM `url` rule as `TokenBuilder::text`, applied to strikethrough
        // content: marked lexes `del` content with `this.lexer.inlineTokens(text)`
        // (markdown.ts:28), which runs the full inline tokenizer.
        if let Some((href, text, raw_len)) = match_autolink(rest)
            .map(|(href, text)| {
                let len = text.len();
                (href, text, len)
            })
            .or_else(|| {
                match_email_autolink(rest).map(|(href, text)| {
                    let len = text.len();
                    (href, text, len)
                })
            })
        {
            if !pending.is_empty() {
                tokens.push(Token::Text {
                    text: std::mem::take(&mut pending),
                    tokens: None,
                });
            }
            tokens.push(Token::Link {
                href,
                text: text.clone(),
                tokens: vec![Token::Text { text, tokens: None }],
            });
            index += raw_len;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        pending.push(ch);
        index += ch.len_utf8();
    }
    if !pending.is_empty() {
        tokens.push(Token::Text {
            text: pending,
            tokens: None,
        });
    }
    tokens
}

fn heading_depth(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn collect_text(tokens: &[Token]) -> String {
    let mut result = String::new();
    for token in tokens {
        match token {
            Token::Text { text, .. } => result.push_str(text),
            Token::Codespan { text } => result.push_str(text),
            Token::Strong { tokens } | Token::Em { tokens } | Token::Del { tokens } => {
                result.push_str(&collect_text(tokens))
            }
            Token::Link { text, .. } => result.push_str(text),
            Token::InlineMath(math) => result.push_str(&math.text),
            _ => {}
        }
    }
    result
}

/// Port of `DefaultTextStyle`.
#[derive(Default)]
pub struct DefaultTextStyle {
    pub color: Option<Rc<dyn Fn(&str) -> String>>,
    pub bg_color: Option<Rc<dyn Fn(&str) -> String>>,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub underline: bool,
}

/// Port of `MarkdownTheme`. Theme functions are `Rc` so inline style contexts can
/// hold them while the renderer keeps borrowing the theme.
pub struct MarkdownTheme {
    pub heading: Rc<dyn Fn(&str) -> String>,
    pub link: Rc<dyn Fn(&str) -> String>,
    pub link_url: Rc<dyn Fn(&str) -> String>,
    pub code: Rc<dyn Fn(&str) -> String>,
    pub code_block: Rc<dyn Fn(&str) -> String>,
    pub code_block_border: Rc<dyn Fn(&str) -> String>,
    pub quote: Rc<dyn Fn(&str) -> String>,
    pub quote_border: Rc<dyn Fn(&str) -> String>,
    pub hr: Rc<dyn Fn(&str) -> String>,
    pub list_bullet: Rc<dyn Fn(&str) -> String>,
    pub bold: Rc<dyn Fn(&str) -> String>,
    pub italic: Rc<dyn Fn(&str) -> String>,
    pub strikethrough: Rc<dyn Fn(&str) -> String>,
    pub underline: Rc<dyn Fn(&str) -> String>,
    pub highlight_code: Option<Rc<dyn Fn(&str, Option<&str>) -> Vec<String>>>,
    pub code_block_indent: Option<String>,
    pub math: Option<Rc<dyn Fn(&str) -> String>>,
    pub math_block: Option<Rc<dyn Fn(&str) -> String>>,
}

/// Port of `MarkdownOptions`.
#[derive(Default)]
pub struct MarkdownOptions {
    /// Transform source Markdown before parsing, with the exact width available for content.
    pub transform: Option<Rc<dyn Fn(&str, usize) -> String>>,
    /// Base URL for relative link targets. Directory URLs must end with a slash.
    pub base_url: Option<String>,
}

/// Port of `InlineStyleContext`.
#[derive(Clone)]
struct InlineStyleContext {
    apply_text: Rc<dyn Fn(&str) -> String>,
    style_prefix: String,
}

pub struct Markdown {
    text: String,
    padding_x: usize,
    padding_y: usize,
    default_text_style: Option<DefaultTextStyle>,
    theme: MarkdownTheme,
    options: MarkdownOptions,
    default_style_prefix: Option<String>,

    cached_text: Option<String>,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
    selection_regions: Vec<TableCellSelectionRegion>,
    table_identities: Vec<usize>,
    // Per-block render cache so streaming appends only re-render the changing
    // final block instead of the whole document. Keyed by width/type/nextType/raw;
    // rebuilt each render so it stays bounded to the current document's blocks.
    block_cache: HashMap<String, Vec<String>>,
}

impl Markdown {
    pub fn new(
        text: String,
        padding_x: usize,
        padding_y: usize,
        theme: MarkdownTheme,
        default_text_style: Option<DefaultTextStyle>,
        options: MarkdownOptions,
    ) -> Self {
        Self {
            text,
            padding_x,
            padding_y,
            theme,
            default_text_style,
            options,
            default_style_prefix: None,
            cached_text: None,
            cached_width: None,
            cached_lines: None,
            selection_regions: Vec::new(),
            table_identities: Vec::new(),
            block_cache: HashMap::new(),
        }
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
        // Only the whole-result cache is dropped; the per-block cache stays so a
        // streaming append re-renders just the blocks that actually changed.
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
        self.selection_regions = Vec::new();
    }

    pub fn get_selection_regions(&self) -> &[TableCellSelectionRegion] {
        &self.selection_regions
    }

    /// Builds the base styling closure for the default text style.
    fn build_default_apply_text(&mut self) -> Rc<dyn Fn(&str) -> String> {
        if self.default_text_style.is_none() {
            return Rc::new(|text: &str| text.to_string());
        }
        let color = self
            .default_text_style
            .as_ref()
            .and_then(|style| style.color.clone());
        let bold = self
            .default_text_style
            .as_ref()
            .map(|style| style.bold)
            .unwrap_or(false);
        let italic = self
            .default_text_style
            .as_ref()
            .map(|style| style.italic)
            .unwrap_or(false);
        let strikethrough = self
            .default_text_style
            .as_ref()
            .map(|style| style.strikethrough)
            .unwrap_or(false);
        let underline = self
            .default_text_style
            .as_ref()
            .map(|style| style.underline)
            .unwrap_or(false);
        let theme_bold = Rc::clone(&self.theme.bold);
        let theme_italic = Rc::clone(&self.theme.italic);
        let theme_strikethrough = Rc::clone(&self.theme.strikethrough);
        let theme_underline = Rc::clone(&self.theme.underline);

        Rc::new(move |text: &str| {
            let mut styled = text.to_string();
            if let Some(color) = &color {
                styled = color(&styled);
            }
            if bold {
                styled = theme_bold(&styled);
            }
            if italic {
                styled = theme_italic(&styled);
            }
            if strikethrough {
                styled = theme_strikethrough(&styled);
            }
            if underline {
                styled = theme_underline(&styled);
            }
            styled
        })
    }

    fn get_default_style_prefix(&mut self) -> String {
        if self.default_text_style.is_none() {
            return String::new();
        }

        if let Some(prefix) = &self.default_style_prefix {
            return prefix.clone();
        }

        let apply_text = self.build_default_apply_text();
        let sentinel = "\u{0000}";
        let styled = apply_text(sentinel);

        let prefix = match styled.find(sentinel) {
            Some(index) => styled[..index].to_string(),
            None => String::new(),
        };
        self.default_style_prefix = Some(prefix.clone());
        prefix
    }

    fn get_style_prefix(style_fn: &dyn Fn(&str) -> String) -> String {
        let sentinel = "\u{0000}";
        let styled = style_fn(sentinel);
        match styled.find(sentinel) {
            Some(index) => styled[..index].to_string(),
            None => String::new(),
        }
    }

    fn get_default_inline_style_context(&mut self) -> InlineStyleContext {
        let style_prefix = self.get_default_style_prefix();
        let apply_text = self.build_default_apply_text();
        InlineStyleContext {
            apply_text,
            style_prefix,
        }
    }

    /// Render one top-level block: token lines, wrapping, margins, background.
    fn render_block(
        &mut self,
        token: &Token,
        next_token_type: Option<&str>,
        width: usize,
        content_width: usize,
    ) -> Vec<String> {
        let token_lines = self.render_token(token, content_width, next_token_type, None);

        let left_margin = " ".repeat(self.padding_x);
        let right_margin = " ".repeat(self.padding_x);
        let bg_fn = self
            .default_text_style
            .as_ref()
            .and_then(|style| style.bg_color.clone());
        let mut block_lines: Vec<String> = Vec::new();

        for line in token_lines {
            if is_image_line(&line) {
                block_lines.push(line);
                continue;
            }
            for wrapped in wrap_text_with_ansi(&line, content_width) {
                let line_with_margins = format!("{left_margin}{wrapped}{right_margin}");
                match &bg_fn {
                    Some(bg_fn) => block_lines.push(apply_background_to_line(
                        &line_with_margins,
                        width,
                        bg_fn.as_ref(),
                    )),
                    None => {
                        let visible_len = visible_width(&line_with_margins);
                        let padding_needed = width.saturating_sub(visible_len);
                        block_lines
                            .push(format!("{line_with_margins}{}", " ".repeat(padding_needed)));
                    }
                }
            }
        }

        block_lines
    }

    fn render_token(
        &mut self,
        token: &Token,
        width: usize,
        next_token_type: Option<&str>,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();

        match token {
            Token::Heading { depth, tokens } => {
                let heading_level = *depth;
                let heading_prefix = format!("{} ", "#".repeat(heading_level));

                // Build a heading-specific style context so inline tokens (codespan, bold, etc.)
                // restore heading styling after their own ANSI resets instead of falling back to
                // the default text style.
                let theme_heading = Rc::clone(&self.theme.heading);
                let theme_bold = Rc::clone(&self.theme.bold);
                let theme_underline = Rc::clone(&self.theme.underline);
                let heading_style_fn: Rc<dyn Fn(&str) -> String> = if heading_level == 1 {
                    let underline = Rc::clone(&theme_underline);
                    Rc::new(move |text: &str| theme_heading(&theme_bold(&underline(text))))
                } else {
                    Rc::new(move |text: &str| theme_heading(&theme_bold(text)))
                };

                let heading_style_context = InlineStyleContext {
                    apply_text: Rc::clone(&heading_style_fn),
                    style_prefix: Self::get_style_prefix(heading_style_fn.as_ref()),
                };

                let heading_text = self.render_inline_tokens(tokens, Some(&heading_style_context));
                let styled_heading = if heading_level >= 3 {
                    format!("{}{heading_text}", heading_style_fn(&heading_prefix))
                } else {
                    heading_text
                };
                lines.push(styled_heading);
                if next_token_type.is_some() && next_token_type != Some("space") {
                    lines.push(String::new()); // Add spacing after headings (unless space token follows)
                }
            }

            Token::Paragraph { tokens } => {
                let paragraph_text = self.render_inline_tokens(tokens, style_context);
                lines.push(paragraph_text);
                if next_token_type.is_some()
                    && next_token_type != Some("list")
                    && next_token_type != Some("space")
                {
                    lines.push(String::new());
                }
            }

            Token::Code { text, lang } => {
                lines.extend(self.render_code_block(text, lang.as_deref()));
                if next_token_type.is_some() && next_token_type != Some("space") {
                    lines.push(String::new()); // Add spacing after code blocks (unless space token follows)
                }
            }

            Token::BlockMath(math) => {
                lines.extend(self.render_math_block(math));
                if next_token_type.is_some() && next_token_type != Some("space") {
                    lines.push(String::new()); // Add spacing after math blocks (unless space token follows)
                }
            }

            Token::List {
                ordered,
                start,
                items,
            } => {
                lines.extend(self.render_list(*ordered, *start, items, 0, style_context));
            }

            Token::Table { header, rows, raw } => {
                lines.extend(self.render_table(
                    header,
                    rows,
                    raw,
                    width,
                    next_token_type,
                    style_context,
                ));
            }

            Token::Blockquote { tokens } => {
                let theme_quote = Rc::clone(&self.theme.quote);
                let theme_italic = Rc::clone(&self.theme.italic);
                let quote_style = Rc::new(move |text: &str| theme_quote(&theme_italic(text)));
                let quote_style_prefix = Self::get_style_prefix(quote_style.as_ref());
                let apply_quote_style = |line: &str| -> String {
                    if quote_style_prefix.is_empty() {
                        return quote_style(line);
                    }
                    let line_with_reapplied_style =
                        line.replace("\x1b[0m", &format!("\x1b[0m{quote_style_prefix}"));
                    quote_style(&line_with_reapplied_style)
                };

                let quote_content_width = width.saturating_sub(2).max(1);

                // Blockquotes contain block-level tokens (paragraph, list, code, etc.), so render
                // children with renderToken() instead of renderInlineTokens().
                // Default message style should not apply inside blockquotes.
                let quote_inline_style_context = InlineStyleContext {
                    apply_text: Rc::new(|text: &str| text.to_string()),
                    style_prefix: quote_style_prefix.clone(),
                };
                let mut rendered_quote_lines: Vec<String> = Vec::new();
                for (i, quote_token) in tokens.iter().enumerate() {
                    let next_quote_token = tokens.get(i + 1);
                    rendered_quote_lines.extend(self.render_token(
                        quote_token,
                        quote_content_width,
                        next_quote_token.map(|t| t.type_name()),
                        Some(&quote_inline_style_context),
                    ));
                }

                while rendered_quote_lines
                    .last()
                    .map(|line| line.is_empty())
                    .unwrap_or(false)
                {
                    rendered_quote_lines.pop();
                }

                let quote_border = Rc::clone(&self.theme.quote_border);
                for quote_line in rendered_quote_lines {
                    let styled_line = apply_quote_style(&quote_line);
                    let wrapped_lines = wrap_text_with_ansi(&styled_line, quote_content_width);
                    for wrapped_line in wrapped_lines {
                        lines.push(format!("{}{wrapped_line}", quote_border("│ ")));
                    }
                }
                if next_token_type.is_some() && next_token_type != Some("space") {
                    lines.push(String::new()); // Add spacing after blockquotes (unless space token follows)
                }
            }

            Token::Hr => {
                lines.push((self.theme.hr)(&"─".repeat(width.min(80))));
                if next_token_type.is_some() && next_token_type != Some("space") {
                    lines.push(String::new()); // Add spacing after horizontal rules (unless space token follows)
                }
            }

            Token::Html { raw } => {
                let trimmed = raw.trim().to_string();
                lines.push(self.apply_default_style_with_context(&trimmed, style_context));
            }

            Token::Space => {
                lines.push(String::new());
            }

            Token::Text { text, .. } => {
                lines.push(text.clone());
            }

            other => {
                // Inline token rendered as a block: fall back to inline rendering.
                lines.push(self.render_inline_tokens(std::slice::from_ref(other), style_context));
            }
        }

        lines
    }

    fn apply_default_style_with_context(
        &mut self,
        text: &str,
        style_context: Option<&InlineStyleContext>,
    ) -> String {
        match style_context {
            Some(context) => (context.apply_text)(text),
            None => {
                let apply_text = self.build_default_apply_text();
                apply_text(text)
            }
        }
    }
}

impl Markdown {
    fn render_inline_tokens(
        &mut self,
        tokens: &[Token],
        style_context: Option<&InlineStyleContext>,
    ) -> String {
        let mut result = String::new();
        let resolved = match style_context {
            Some(context) => context.clone(),
            None => self.get_default_inline_style_context(),
        };
        let apply_text = Rc::clone(&resolved.apply_text);
        let style_prefix = resolved.style_prefix.clone();

        for token in tokens {
            match token {
                Token::Text { text, tokens } => {
                    if let Some(inner) = tokens {
                        if !inner.is_empty() {
                            result.push_str(&self.render_inline_tokens(inner, Some(&resolved)));
                        } else {
                            result.push_str(&apply_text_with_newlines(&apply_text, text));
                        }
                    } else {
                        result.push_str(&apply_text_with_newlines(&apply_text, text));
                    }
                }

                Token::Paragraph { tokens } => {
                    result.push_str(&self.render_inline_tokens(tokens, Some(&resolved)));
                }

                Token::Strong { tokens } => {
                    let bold_content = self.render_inline_tokens(tokens, Some(&resolved));
                    result.push_str(&(self.theme.bold)(&bold_content));
                    result.push_str(&style_prefix);
                }

                Token::Em { tokens } => {
                    let italic_content = self.render_inline_tokens(tokens, Some(&resolved));
                    result.push_str(&(self.theme.italic)(&italic_content));
                    result.push_str(&style_prefix);
                }

                Token::Del { tokens } => {
                    let del_content = self.render_inline_tokens(tokens, Some(&resolved));
                    result.push_str(&(self.theme.strikethrough)(&del_content));
                    result.push_str(&style_prefix);
                }

                Token::Codespan { text } => {
                    result.push_str(&(self.theme.code)(text));
                    result.push_str(&style_prefix);
                }

                Token::InlineMath(math) => {
                    let math_style = self
                        .theme
                        .math
                        .clone()
                        .unwrap_or_else(|| Rc::clone(&self.theme.code));
                    let converted = collapse_math_whitespace(&latex_to_unicode(&math.text));
                    result.push_str(&math_style(&converted));
                    result.push_str(&style_prefix);
                }

                Token::Link { href, text, tokens } => {
                    let link_text = self.render_inline_tokens(tokens, Some(&resolved));
                    let styled_link = (self.theme.link)(&(self.theme.underline)(&link_text));
                    if get_capabilities().hyperlinks {
                        // A Windows drive letter is a file path, not a URL scheme.
                        let target = replace_windows_drive(href);
                        let resolved_href = if !target.starts_with('#')
                            && (self.options.base_url.is_some() || target != *href)
                        {
                            match &self.options.base_url {
                                Some(base) => resolve_url(&target, base),
                                None => None,
                            }
                        } else {
                            None
                        };
                        let href_final = resolved_href.unwrap_or(target);
                        // OSC 8: render as a clickable hyperlink. The URL is not printed inline,
                        // so we always show only the link text regardless of whether it matches href.
                        result.push_str(&hyperlink(&styled_link, &href_final));
                        result.push_str(&style_prefix);
                    } else {
                        // Compare raw token.text (not styled) against href for the equality check.
                        // For mailto: links strip the prefix (autolinked emails use text="foo@bar.com"
                        // but href="mailto:foo@bar.com").
                        let href_for_comparison = match href.strip_prefix("mailto:") {
                            Some(rest) => rest.to_string(),
                            None => href.clone(),
                        };
                        if *text == *href || *text == href_for_comparison {
                            result.push_str(&styled_link);
                            result.push_str(&style_prefix);
                        } else {
                            result.push_str(&styled_link);
                            result.push_str(&(self.theme.link_url)(&format!(" ({href})")));
                            result.push_str(&style_prefix);
                        }
                    }
                }

                Token::Br => {
                    result.push('\n');
                }

                Token::Html { raw } => {
                    result.push_str(&apply_text_with_newlines(&apply_text, raw));
                }

                other => {
                    if let Token::Text { text, .. } = other {
                        result.push_str(&apply_text_with_newlines(&apply_text, text));
                    }
                }
            }
        }

        while !style_prefix.is_empty() && result.ends_with(&style_prefix) {
            result.truncate(result.len() - style_prefix.len());
        }

        result
    }

    /// Render a list with proper nesting support
    fn render_list(
        &mut self,
        ordered: bool,
        start: usize,
        items: &[Vec<Token>],
        depth: usize,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let indent = "  ".repeat(depth);
        let start_number = start;

        for (i, item) in items.iter().enumerate() {
            let bullet = if ordered {
                format!("{}. ", start_number + i)
            } else {
                "- ".to_string()
            };

            let item_lines = self.render_list_item(item, depth, style_context);

            if !item_lines.is_empty() {
                // A nested list will start with indent (spaces) followed by cyan bullet
                let first_line = item_lines[0].clone();
                let is_nested_list = is_nested_list_line(&first_line);

                if is_nested_list {
                    lines.push(first_line);
                } else {
                    lines.push(format!(
                        "{indent}{}{first_line}",
                        (self.theme.list_bullet)(&bullet)
                    ));
                }

                for line in item_lines.iter().skip(1) {
                    if is_nested_list_line(line) {
                        lines.push(line.clone());
                    } else {
                        lines.push(format!("{indent}  {line}"));
                    }
                }
            } else {
                lines.push(format!("{indent}{}", (self.theme.list_bullet)(&bullet)));
            }
        }

        lines
    }

    /// Render list item tokens, handling nested lists
    /// Returns lines WITHOUT the parent indent (renderList will add it)
    fn render_list_item(
        &mut self,
        tokens: &[Token],
        parent_depth: usize,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();

        // marked wraps a TIGHT item's content in a single `text` token carrying child
        // tokens, so `renderListItem` renders the whole item on one line
        // (packages/tui/src/components/markdown.ts:731-737). pulldown-cmark emits the
        // inline tokens directly, so a list of only-inline tokens is merged here.
        let all_inline = !tokens.is_empty()
            && tokens.iter().all(|token| {
                !matches!(
                    token,
                    Token::List { .. }
                        | Token::Paragraph { .. }
                        | Token::Code { .. }
                        | Token::BlockMath(_)
                )
            });
        if all_inline {
            let rendered = self.render_inline_tokens(tokens, style_context);
            if !rendered.is_empty() {
                lines.push(rendered);
            }
            return lines;
        }

        for token in tokens {
            match token {
                Token::List {
                    ordered,
                    start,
                    items,
                } => {
                    // Nested list - render with one additional indent level
                    // These lines will have their own indent, so we just add them as-is
                    lines.extend(self.render_list(
                        *ordered,
                        *start,
                        items,
                        parent_depth + 1,
                        style_context,
                    ));
                }
                Token::Text { text, tokens } => {
                    // Text content (may have inline tokens)
                    let rendered = match tokens {
                        Some(inner) if !inner.is_empty() => {
                            self.render_inline_tokens(inner, style_context)
                        }
                        _ => text.clone(),
                    };
                    lines.push(rendered);
                }
                Token::Paragraph { tokens } => {
                    // Paragraph in list item
                    lines.push(self.render_inline_tokens(tokens, style_context));
                }
                Token::Code { text, lang } => {
                    // Code block in list item
                    lines.extend(self.render_code_block(text, lang.as_deref()));
                }
                Token::BlockMath(math) => {
                    // Display math in list item
                    lines.extend(self.render_math_block(math));
                }
                other => {
                    // Other token types - try to render as inline
                    let text =
                        self.render_inline_tokens(std::slice::from_ref(other), style_context);
                    if !text.is_empty() {
                        lines.push(text);
                    }
                }
            }
        }

        lines
    }

    fn render_code_block(&mut self, text: &str, lang: Option<&str>) -> Vec<String> {
        let indent = self
            .theme
            .code_block_indent
            .clone()
            .unwrap_or_else(|| "  ".to_string());
        let rendered_code_lines = match &self.theme.highlight_code {
            Some(highlight_code) => highlight_code(text, lang),
            None => text
                .split('\n')
                .map(|code_line| (self.theme.code_block)(code_line))
                .collect(),
        };
        let code_lines = if rendered_code_lines.is_empty() {
            vec![(self.theme.code_block)("")]
        } else {
            rendered_code_lines
        };

        code_lines
            .into_iter()
            .map(|code_line| format!("{indent}{code_line}"))
            .collect()
    }

    /// Render display math: converted to Unicode, indented like a code block.
    fn render_math_block(&mut self, token: &MathToken) -> Vec<String> {
        let indent = self
            .theme
            .code_block_indent
            .clone()
            .unwrap_or_else(|| "  ".to_string());
        let style = self
            .theme
            .math_block
            .clone()
            .unwrap_or_else(|| Rc::clone(&self.theme.code_block));
        let math_lines: Vec<String> = latex_to_unicode(&token.text)
            .split('\n')
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        math_lines
            .into_iter()
            .map(|line| format!("{indent}{}", style(&line)))
            .collect()
    }

    /// Get the visible width of the longest word in a string.
    fn get_longest_word_width(text: &str, max_width: Option<usize>) -> usize {
        let words: Vec<&str> = text
            .split_whitespace()
            .filter(|word| !word.is_empty())
            .collect();
        let mut longest = 0usize;
        for word in words {
            longest = longest.max(visible_width(word));
        }
        match max_width {
            None => longest,
            Some(max_width) => longest.min(max_width),
        }
    }

    /// Wrap a table cell to fit into a column.
    ///
    /// Delegates to wrapTextWithAnsi() so ANSI codes + long tokens are handled
    /// consistently with the rest of the renderer.
    fn wrap_cell_text(text: &str, max_width: usize) -> Vec<String> {
        wrap_text_with_ansi(text, max_width.max(1))
    }

    /// Render a table with width-aware cell wrapping.
    /// Cells that don't fit are wrapped to multiple lines.
    fn render_table(
        &mut self,
        header: &[Vec<Token>],
        rows: &[Vec<Vec<Token>>],
        raw: &str,
        available_width: usize,
        next_token_type: Option<&str>,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let num_cols = header.len();

        if num_cols == 0 {
            return lines;
        }

        // = 2 + (n-1) * 3 + 2 = 3n + 1
        let border_overhead = 3 * num_cols + 1;
        if available_width <= border_overhead || available_width - border_overhead < num_cols {
            // Too narrow to render a stable table. Fall back to raw markdown.
            let mut fallback_lines = if !raw.is_empty() {
                wrap_text_with_ansi(raw, available_width)
            } else {
                Vec::new()
            };
            if next_token_type.is_some() && next_token_type != Some("space") {
                fallback_lines.push(String::new());
            }
            return fallback_lines;
        }
        let available_for_cells = available_width - border_overhead;

        let max_unbroken_word_width = 30;

        let mut natural_widths: Vec<usize> = vec![0; num_cols];
        let mut min_word_widths: Vec<usize> = vec![0; num_cols];
        for i in 0..num_cols {
            let header_text = self.render_inline_tokens(&header[i], style_context);
            natural_widths[i] = visible_width(&header_text);
            min_word_widths[i] =
                Self::get_longest_word_width(&header_text, Some(max_unbroken_word_width)).max(1);
        }
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                if i >= num_cols {
                    continue;
                }
                let cell_text = self.render_inline_tokens(cell, style_context);
                natural_widths[i] = natural_widths[i].max(visible_width(&cell_text));
                min_word_widths[i] = min_word_widths[i].max(Self::get_longest_word_width(
                    &cell_text,
                    Some(max_unbroken_word_width),
                ));
            }
        }

        let mut min_column_widths = min_word_widths.clone();
        let mut min_cells_width: usize = min_column_widths.iter().sum();

        if min_cells_width > available_for_cells {
            min_column_widths = vec![1; num_cols];
            let remaining = available_for_cells.saturating_sub(num_cols);

            if remaining > 0 {
                let total_weight: usize = min_word_widths
                    .iter()
                    .map(|width| width.saturating_sub(1))
                    .sum();
                let growth: Vec<usize> = min_word_widths
                    .iter()
                    .map(|width| {
                        let weight = width.saturating_sub(1);
                        if total_weight > 0 {
                            (weight * remaining) / total_weight
                        } else {
                            0
                        }
                    })
                    .collect();

                for i in 0..num_cols {
                    min_column_widths[i] += growth.get(i).copied().unwrap_or(0);
                }

                let allocated: usize = growth.iter().sum();
                let mut leftover = remaining.saturating_sub(allocated);
                let mut i = 0;
                while leftover > 0 && i < num_cols {
                    min_column_widths[i] += 1;
                    leftover -= 1;
                    i += 1;
                }
            }

            min_cells_width = min_column_widths.iter().sum();
        }

        let total_natural_width: usize = natural_widths.iter().sum::<usize>() + border_overhead;
        let mut column_widths: Vec<usize>;

        if total_natural_width <= available_width {
            column_widths = natural_widths
                .iter()
                .enumerate()
                .map(|(index, width)| (*width).max(min_column_widths[index]))
                .collect();
        } else {
            let total_grow_potential: usize = natural_widths
                .iter()
                .enumerate()
                .map(|(index, width)| width.saturating_sub(min_column_widths[index]))
                .sum();
            let extra_width = available_for_cells.saturating_sub(min_cells_width);
            column_widths = min_column_widths
                .iter()
                .enumerate()
                .map(|(index, min_width)| {
                    let natural_width = natural_widths[index];
                    let min_width_delta = natural_width.saturating_sub(*min_width);
                    let mut grow = 0;
                    if total_grow_potential > 0 {
                        grow = (min_width_delta * extra_width) / total_grow_potential;
                    }
                    min_width + grow
                })
                .collect();

            // Adjust for rounding errors - distribute remaining space
            let allocated: usize = column_widths.iter().sum();
            let mut remaining = available_for_cells.saturating_sub(allocated);
            while remaining > 0 {
                let mut grew = false;
                for i in 0..num_cols {
                    if remaining == 0 {
                        break;
                    }
                    if column_widths[i] < natural_widths[i] {
                        column_widths[i] += 1;
                        remaining -= 1;
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
        }

        let top_border_cells: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
        lines.push(mark_table_start(&format!(
            "┌─{}─┐",
            top_border_cells.join("─┬─")
        )));

        let header_cells: Vec<(Vec<String>, String)> = header
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let text = self.render_inline_tokens(cell, style_context);
                (
                    Self::wrap_cell_text(&text, column_widths[i]),
                    strip_ansi(&text),
                )
            })
            .collect();
        let header_line_count = header_cells
            .iter()
            .map(|(cell_lines, _)| cell_lines.len())
            .max()
            .unwrap_or(0);

        for line_idx in 0..header_line_count {
            let row_parts: Vec<String> = header_cells
                .iter()
                .enumerate()
                .map(|(col_idx, (cell_lines, content))| {
                    let text = cell_lines.get(line_idx).cloned().unwrap_or_default();
                    let padded = format!(
                        "{text}{}",
                        " ".repeat(column_widths[col_idx].saturating_sub(visible_width(&text)))
                    );
                    mark_table_cell(
                        &(self.theme.bold)(&padded),
                        0,
                        col_idx as i64,
                        line_idx as i64,
                        content,
                    )
                })
                .collect();
            lines.push(format!("│ {} │", row_parts.join(" │ ")));
        }

        let separator_cells: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
        let separator_line = format!("├─{}─┤", separator_cells.join("─┼─"));
        lines.push(separator_line.clone());

        for (row_index, row) in rows.iter().enumerate() {
            let row_cells: Vec<(Vec<String>, String)> = row
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    let text = self.render_inline_tokens(cell, style_context);
                    let width = column_widths.get(i).copied().unwrap_or(1);
                    (Self::wrap_cell_text(&text, width), strip_ansi(&text))
                })
                .collect();
            let row_line_count = row_cells
                .iter()
                .map(|(cell_lines, _)| cell_lines.len())
                .max()
                .unwrap_or(0);

            for line_idx in 0..row_line_count {
                let row_parts: Vec<String> = row_cells
                    .iter()
                    .enumerate()
                    .map(|(col_idx, (cell_lines, content))| {
                        let text = cell_lines.get(line_idx).cloned().unwrap_or_default();
                        let width = column_widths.get(col_idx).copied().unwrap_or(1);
                        let padded = format!(
                            "{text}{}",
                            " ".repeat(width.saturating_sub(visible_width(&text)))
                        );
                        mark_table_cell(
                            &padded,
                            (row_index + 1) as i64,
                            col_idx as i64,
                            line_idx as i64,
                            content,
                        )
                    })
                    .collect();
                lines.push(format!("│ {} │", row_parts.join(" │ ")));
            }

            if row_index < rows.len() - 1 {
                lines.push(separator_line.clone());
            }
        }

        let bottom_border_cells: Vec<String> =
            column_widths.iter().map(|w| "─".repeat(*w)).collect();
        lines.push(mark_table_end(&format!(
            "└─{}─┘",
            bottom_border_cells.join("─┴─")
        )));

        if next_token_type.is_some() && next_token_type != Some("space") {
            lines.push(String::new()); // Add spacing after table
        }
        lines
    }
}

fn apply_text_with_newlines(apply_text: &Rc<dyn Fn(&str) -> String>, text: &str) -> String {
    let segments: Vec<&str> = text.split('\n').collect();
    segments
        .into_iter()
        .map(|segment| apply_text(segment))
        .collect::<Vec<String>>()
        .join("\n")
}

/// Port of `/^\s+\x1b\[36m[-\d]/`.
fn is_nested_list_line(line: &str) -> bool {
    let trimmed_len = line.len() - line.trim_start().len();
    if trimmed_len == 0 {
        return false;
    }
    let rest = &line[trimmed_len..];
    let rest = match rest.strip_prefix("\x1b[36m") {
        Some(rest) => rest,
        None => return false,
    };
    match rest.chars().next() {
        Some(ch) => ch == '-' || ch.is_ascii_digit(),
        None => false,
    }
}

/// Port of `/^([a-z]:[\\/])/i` -> `file:///$1`.
fn replace_windows_drive(href: &str) -> String {
    let mut chars = href.chars();
    let first = chars.next();
    let second = chars.next();
    let third = chars.next();
    if let (Some(letter), Some(':'), Some(sep)) = (first, second, third) {
        if letter.is_ascii_alphabetic() && (sep == '\\' || sep == '/') {
            return format!("file:///{letter}:{sep}{}", &href[3..]);
        }
    }
    href.to_string()
}

/// Port of `new URL(target, baseUrl).href` for the subset the renderer needs.
fn resolve_url(target: &str, base: &str) -> Option<String> {
    if target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("file://")
    {
        return Some(target.to_string());
    }
    if target.starts_with("//") {
        let scheme = base.split("://").next().unwrap_or("https");
        return Some(format!("{scheme}:{target}"));
    }
    if target.starts_with('/') {
        let scheme_end = base.find("://")? + 3;
        let host_end = base[scheme_end..]
            .find('/')
            .map(|i| scheme_end + i)
            .unwrap_or(base.len());
        return Some(format!("{}{}", &base[..host_end], target));
    }
    let cut = base.rfind('/').map(|i| i + 1).unwrap_or(base.len());
    Some(format!("{}{}", &base[..cut], target))
}

/// Port of `latexToUnicode(text).replace(/\s*\n\s*/g, " ")`.
fn collapse_math_whitespace(text: &str) -> String {
    let mut result = String::new();
    let mut pending = String::new();
    for ch in text.chars() {
        if ch == '\u{feff}' || (ch != '\u{85}' && ch.is_whitespace()) {
            pending.push(ch);
        } else {
            if pending.contains('\n') {
                result.push(' ');
            } else {
                result.push_str(&pending);
            }
            pending.clear();
            result.push(ch);
        }
    }
    if pending.contains('\n') {
        result.push(' ');
    } else {
        result.push_str(&pending);
    }
    result
}

impl Component for Markdown {
    fn render(&mut self, width: f64) -> Vec<String> {
        let width = width.max(0.0).floor() as usize;
        if let (Some(lines), Some(cached_text), Some(cached_width)) =
            (&self.cached_lines, &self.cached_text, self.cached_width)
        {
            if *cached_text == self.text && cached_width == width {
                return lines.clone();
            }
        }

        let content_width = width.saturating_sub(self.padding_x * 2).max(1);
        let text = match &self.options.transform {
            Some(transform) => transform(&self.text, content_width),
            None => self.text.clone(),
        };

        if text.is_empty() || text.trim().is_empty() {
            let result: Vec<String> = Vec::new();
            self.selection_regions = Vec::new();
            self.cached_text = Some(self.text.clone());
            self.cached_width = Some(width);
            self.cached_lines = Some(result.clone());
            return result;
        }

        // `marked` normalises line endings before its block tokenizers run
        // (`lex(e){e=e.replace(m.carriageReturn,"\n")}`, marked.esm.js:25991 with
        // `carriageReturn:/\r\n|\r/g` at :2233), so a CRLF document reaches the
        // extensions as LF. Without this the `[ \t]*(?:\n|$)` tail of BLOCK_MATH_REGEX
        // (markdown.ts:45) never sees its `\n` after `\]` and display math lexes as
        // inline math instead (markdown-latex.test.ts:110-113).
        let normalized_text = normalize_line_endings(&text.replace('\t', "   "));

        // Parse markdown to HTML-like tokens
        let lexed = lex(&normalized_text);
        let tokens = lexed.tokens;

        // Reference-link definitions make a block's rendering depend on other
        // blocks, so per-block caching is disabled when any are present.
        let cacheable = lexed.links.is_empty();

        // Render, wrap, and pad per top-level block so unchanged blocks can be
        // served from the cache. The final block is never cached: while streaming,
        // appended text can reinterpret it (unterminated fences, growing lists);
        // once a block is no longer last, its raw text is final.
        let mut next_cache: HashMap<String, Vec<String>> = HashMap::new();
        let mut content_lines: Vec<String> = Vec::new();
        // `marked` exposes the matched source text as `token.raw`, so the TS key
        // (markdown.ts:271) carries the block's own bytes: a list's raw starts with
        // its `1. ` / `2. ` marker (verified with `marked.lexer`). The port derives
        // `raw_of` from the token tree instead, where a list's start number is not
        // recoverable from its items, so two same-shaped blocks collided and the
        // second was served the first's lines. The block index restores the missing
        // identity; it is stable while text is appended, so unchanged blocks still hit.
        for i in 0..tokens.len() {
            let token = tokens[i].clone();
            let next_token_type = tokens.get(i + 1).map(|t| t.type_name().to_string());
            let use_cache = cacheable && i < tokens.len() - 1;
            let key = if use_cache {
                format!(
                    "{}|{}|{}|{}|{}",
                    width,
                    token.type_name(),
                    next_token_type.clone().unwrap_or_default(),
                    raw_of(&token),
                    i
                )
            } else {
                String::new()
            };
            let mut block_lines = if use_cache {
                next_cache
                    .get(&key)
                    .cloned()
                    .or_else(|| self.block_cache.get(&key).cloned())
            } else {
                None
            };
            if block_lines.is_none() {
                block_lines = Some(self.render_block(
                    &token,
                    next_token_type.as_deref(),
                    width,
                    content_width,
                ));
            }
            let block_lines = block_lines.unwrap_or_default();
            if use_cache {
                next_cache.insert(key, block_lines.clone());
            }
            content_lines.extend(block_lines);
        }
        self.block_cache = next_cache;

        let bg_fn = self
            .default_text_style
            .as_ref()
            .and_then(|style| style.bg_color.clone());
        let empty_line = " ".repeat(width);
        let mut empty_lines: Vec<String> = Vec::new();
        for _ in 0..self.padding_y {
            let line = match &bg_fn {
                Some(bg_fn) => apply_background_to_line(&empty_line, width, bg_fn.as_ref()),
                None => empty_line.clone(),
            };
            empty_lines.push(line);
        }

        let mut marked_result: Vec<String> = Vec::new();
        marked_result.extend(empty_lines.iter().cloned());
        marked_result.extend(content_lines);
        marked_result.extend(empty_lines);

        let mut identities: Vec<usize> = std::mem::take(&mut self.table_identities);
        let (result, regions) =
            extract_table_cell_selection_regions(&marked_result, &mut |index| {
                if index >= identities.len() {
                    identities.resize(index + 1, 0);
                    identities[index] = index;
                }
                identities[index]
            });
        self.table_identities = identities;
        self.selection_regions = regions;

        self.cached_text = Some(self.text.clone());
        self.cached_width = Some(width);
        let result = if result.is_empty() {
            vec![String::new()]
        } else {
            result
        };
        self.cached_lines = Some(result.clone());

        if !result.is_empty() {
            result
        } else {
            vec![String::new()]
        }
    }

    fn get_selection_regions(&self) -> Vec<TableCellSelectionRegion> {
        self.selection_regions.clone()
    }

    fn invalidate(&mut self) {
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
        self.selection_regions = Vec::new();
        // External invalidation (e.g. theme change) affects rendered output, so
        // the per-block cache must go too.
        self.block_cache = HashMap::new();
    }
}

/// Port of `token.raw` for the block cache key.
fn raw_of(token: &Token) -> String {
    // Preserve every render-affecting property, including nested list text,
    // heading depth, link destinations and inline formatting.
    format!("{token:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> String {
        text.to_string()
    }

    fn theme() -> MarkdownTheme {
        MarkdownTheme {
            heading: Rc::new(|text: &str| format!("H<{text}>")),
            link: Rc::new(plain),
            link_url: Rc::new(|text: &str| format!("URL<{text}>")),
            code: Rc::new(|text: &str| format!("`{text}`")),
            code_block: Rc::new(plain),
            code_block_border: Rc::new(plain),
            quote: Rc::new(plain),
            quote_border: Rc::new(plain),
            hr: Rc::new(plain),
            list_bullet: Rc::new(plain),
            bold: Rc::new(|text: &str| format!("**{text}**")),
            italic: Rc::new(|text: &str| format!("_{text}_")),
            strikethrough: Rc::new(|text: &str| format!("~~{text}~~")),
            underline: Rc::new(plain),
            highlight_code: None,
            code_block_indent: None,
            math: None,
            math_block: None,
        }
    }

    fn markdown(text: &str) -> Markdown {
        Markdown::new(
            text.to_string(),
            0,
            0,
            theme(),
            None,
            MarkdownOptions::default(),
        )
    }

    #[test]
    fn strict_strikethrough_matches_marked_rule() {
        assert_eq!(
            strict_strikethrough("~~gone~~ rest"),
            Some(("gone".to_string(), "~~gone~~".to_string()))
        );
        assert_eq!(strict_strikethrough("~~ gone~~"), None);
        assert_eq!(strict_strikethrough("~~gone ~~"), None);
        assert_eq!(strict_strikethrough("~~gone~~~"), None);
        assert_eq!(strict_strikethrough("~gone~"), None);
    }

    /// `Tag::Strikethrough` must become a `del` token, which the TS renderer draws
    /// with `this.theme.strikethrough(...)` (packages/tui/src/components/markdown.ts:645-648),
    /// not with `theme.italic`. Mapping it to `FrameKind::Em` rendered `~~x~~` as italics.
    #[test]
    fn strikethrough_renders_as_strikethrough_not_italic() {
        let mut component = markdown("~~gone~~ and *em*");
        let lines = component.render(60.0);
        let joined = lines.join("\n");

        // The theme stub wraps strikethrough as `~~x~~` (theme() above) and italics as `_x_`.
        assert!(
            joined.contains("~~gone~~"),
            "strikethrough text must use theme.strikethrough: {joined:?}"
        );
        assert!(
            joined.contains("_em_"),
            "emphasis must still use theme.italic: {joined:?}"
        );
        assert!(
            !joined.contains("_gone_"),
            "strikethrough must not fall through to theme.italic: {joined:?}"
        );
    }

    /// marked's GFM `url` rule turns bare URLs, `www.` hosts and emails into `link`
    /// tokens (markdown.ts uses marked's GFM defaults; markdown.test.ts:1065-1087 and
    /// :1144-1156). Expected values in this test are the live `marked.lexer` output.
    #[test]
    fn bare_urls_and_emails_are_autolinked_like_marked() {
        // The matcher is tried at each remaining offset, so the token starts at the
        // URL itself, not at the start of the paragraph.
        assert_eq!(
            match_autolink("https://example.com for more"),
            Some((
                "https://example.com".to_string(),
                "https://example.com".to_string()
            ))
        );
        // `www.` hosts gain an `http://` scheme in `href` (marked.esm.js:24802).
        assert_eq!(
            match_autolink("www.example.com now"),
            Some((
                "http://www.example.com".to_string(),
                "www.example.com".to_string()
            ))
        );
        assert_eq!(
            match_email_autolink("user@example.com for help"),
            Some((
                "mailto:user@example.com".to_string(),
                "user@example.com".to_string()
            ))
        );
        // `_backpedal` (marked.esm.js `J._backpedal`) strips prose punctuation and
        // keeps a balanced `(...)` group; a trailing entity terminator is dropped.
        assert_eq!(
            match_autolink("https://example.com. b").unwrap().1,
            "https://example.com"
        );
        assert_eq!(
            match_autolink("https://example.com)").unwrap().1,
            "https://example.com"
        );
        assert_eq!(
            match_autolink("https://example.com/x_(y)").unwrap().1,
            "https://example.com/x_(y)"
        );
        assert_eq!(
            match_autolink("https://ex.com/a(b").unwrap().1,
            "https://ex.com/a"
        );
        assert_eq!(
            match_autolink("https://ex.com&").unwrap().1,
            "https://ex.com&"
        );
        // Non-matches: a bare `user@example` has no dotted domain and a `2. ` list
        // marker is not a `www.` host (both verified against marked).
        assert_eq!(match_email_autolink("user@example"), None);
        assert_eq!(match_autolink("cost $5 or $10"), None);
    }

    #[test]
    fn autolinked_urls_render_as_links_and_do_not_duplicate() {
        let mut md = markdown("Visit https://example.com for more");
        let joined = md.render(80.0).join(" ");
        assert!(
            joined.contains("https://example.com"),
            "URL must survive: {joined:?}"
        );
        assert_eq!(
            joined.matches("https://example.com").count(),
            1,
            "an autolinked URL must appear exactly once (markdown.test.ts:1077-1087)"
        );

        // An autolinked email must not print its `mailto:` prefix, because marked's
        // link token has `text == href - "mailto:"` (markdown.test.ts:1065-1075).
        let mut md = markdown("Contact user@example.com for help");
        let joined = md.render(80.0).join(" ");
        assert!(joined.contains("user@example.com"), "{joined:?}");
        assert!(!joined.contains("mailto:"), "{joined:?}");

        // A `www.` host prints its `http://` target in parentheses, because there
        // text != href (markdown.ts:632-636).
        let mut md = markdown("Go to www.example.com now");
        let joined = md.render(80.0).join(" ");
        assert!(joined.contains("www.example.com"), "{joined:?}");
        assert!(joined.contains("(http://www.example.com)"), "{joined:?}");
    }

    /// marked's `StrictStrikethroughTokenizer` requires `~~`; pulldown-cmark's own
    /// rule also accepts a single `~` run (markdown.test.ts:1048-1057).
    #[test]
    fn single_tilde_stays_literal_text() {
        let mut md = markdown("Use ~strikethrough~ literally");
        let joined = md.render(80.0).join(" ");
        assert!(joined.contains("~strikethrough~"), "{joined:?}");
        assert_eq!(
            joined.matches("~~strikethrough~~").count(),
            0,
            "single tildes must not become strikethrough: {joined:?}"
        );
    }

    /// The assigned fixture (packages/tui/test/markdown.test.ts:118-148): two lists
    /// separated by unindented code blocks, with a warm block cache because the
    /// second list reuses the first list's shape. The key must carry the block's own
    /// identity, as marked's `token.raw` does (markdown.ts:271).
    #[test]
    fn ordered_list_numbering_is_not_lost_to_the_block_cache() {
        let fixture = "1. First item\n\n```typescript\n// code block\n```\n\n\
                       2. Second item\n\n```typescript\n// another code block\n```\n\n\
                       3. Third item";
        let mut md = markdown(fixture);
        let plain: Vec<String> = md
            .render(80.0)
            .iter()
            .map(|line| line.trim().to_string())
            .collect();
        let numbered: Vec<&String> = plain
            .iter()
            .filter(|line| {
                let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
                digits > 0 && line[digits..].starts_with('.')
            })
            .collect();
        assert_eq!(
            numbered.len(),
            3,
            "expected 3 numbered items, got {numbered:?}"
        );
        for (index, prefix) in ["1.", "2.", "3."].iter().enumerate() {
            assert!(
                numbered[index].starts_with(prefix),
                "item {index} must start with {prefix}, got {:?}",
                numbered[index]
            );
        }

        // A second render on the same object (the streaming path) must reproduce them.
        let again: Vec<String> = md
            .render(80.0)
            .iter()
            .map(|line| line.trim().to_string())
            .collect();
        assert_eq!(plain, again, "the warm cache must return the same lines");
    }

    /// The exact rendered lines for a fixture, with trailing pad removed, as the
    /// `space`-token fixtures below compare whole documents.
    fn rendered(text: &str) -> Vec<String> {
        let mut md = markdown(text);
        md.render(80.0)
            .iter()
            .map(|line| line.trim_end().to_string())
            .collect()
    }

    /// marked emits a `space` token per blank-line run between top-level blocks and
    /// `renderToken` prints one empty line for it (markdown.ts:553-555). Every expected
    /// value here is the live `marked.lexer` + TS `Markdown.render` output.
    #[test]
    fn blank_line_run_between_blocks_renders_one_empty_line() {
        assert_eq!(rendered("para\n\n- a\n- b"), ["para", "", "- a", "- b"]);
        assert_eq!(rendered("- a\n- b\n\npara"), ["- a", "- b", "", "para"]);
        assert_eq!(rendered("\n\npara\n\n"), ["", "para", ""]);
        assert_eq!(rendered("a\n\nb\n\nc"), ["a", "", "b", "", "c"]);
        // Table headers go through `theme.bold`, wrapped as `**a**` by the test theme.
        assert_eq!(
            rendered("| a |\n| --- |\n| 1 |\n\npara"),
            ["┌───┐", "│ **a** │", "├───┤", "│ 1 │", "└───┘", "", "para"]
        );
        // A run of two or more newlines is one token however long it is.
        assert_eq!(rendered("para\n\n\n\n- a"), ["para", "", "- a"]);
        // A single trailing newline is not a `space` token: marked appends it to the
        // previous token's raw (marked.esm.js:26185, `r.raw.length === 1`).
        assert_eq!(rendered("para\n"), ["para"]);
        // A blank line inside a list belongs to the list, not to a top-level `space`
        // token: marked recurses into `blockTokens` for the items instead.
        assert_eq!(rendered("- a\n\n- b"), ["- a", "- b"]);
    }

    /// `marked` normalises `\r\n` / `\r` to `\n` before its block tokenizers run
    /// (marked.esm.js:25991, `carriageReturn:/\r\n|\r/g` at :2233), so CRLF display
    /// math reaches `BLOCK_MATH_REGEX` with the LF its `[ \t]*(?:\n|$)` tail needs
    /// (markdown-latex.test.ts:110-113).
    #[test]
    fn crlf_display_math_renders_as_math() {
        assert_eq!(normalize_line_endings("a\r\nb\rc\n"), "a\nb\nc\n");
        let lines = rendered("\\[\r\nE = mc^2\r\n\\]\r\n");
        assert!(
            lines.iter().any(|line| line.contains("E = mc²")),
            "CRLF math must convert: {lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("\\[")),
            "delimiters must be consumed: {lines:?}"
        );
        // The `\\[` block sits in a code block indent: the TS render adds the two
        // spaces `theme.codeBlockIndent` supplies (markdown.ts:776-783).
        assert_eq!(lines[0], "  E = mc²");
    }

    /// Render with a theme whose `strikethrough` marker is distinct from the source
    /// delimiters, so a literal `~x~` is distinguishable from a struck `x`. The TS test
    /// theme is chalk-based and shows the difference through the SGR sequence;
    /// markdown.test.ts:1056 asserts exactly that (no `ESC[9m` for `~x~`).
    #[test]
    fn only_double_tildes_use_the_strikethrough_theme() {
        let mut strike_theme = theme();
        strike_theme.strikethrough = Rc::new(|text: &str| format!("<DEL>{text}</DEL>"));
        fn render_with(theme: &MarkdownTheme, text: &str) -> String {
            let mut md = Markdown::new(
                text.to_string(),
                0,
                0,
                MarkdownTheme {
                    heading: Rc::clone(&theme.heading),
                    link: Rc::clone(&theme.link),
                    link_url: Rc::clone(&theme.link_url),
                    code: Rc::clone(&theme.code),
                    code_block: Rc::clone(&theme.code_block),
                    code_block_border: Rc::clone(&theme.code_block_border),
                    quote: Rc::clone(&theme.quote),
                    quote_border: Rc::clone(&theme.quote_border),
                    hr: Rc::clone(&theme.hr),
                    list_bullet: Rc::clone(&theme.list_bullet),
                    bold: Rc::clone(&theme.bold),
                    italic: Rc::clone(&theme.italic),
                    strikethrough: Rc::clone(&theme.strikethrough),
                    underline: Rc::clone(&theme.underline),
                    highlight_code: None,
                    code_block_indent: None,
                    math: None,
                    math_block: None,
                },
                None,
                MarkdownOptions::default(),
            );
            md.render(80.0)
                .iter()
                .map(|line| line.trim_end().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        }
        let render = |text: &str| render_with(&strike_theme, text);

        // markdown.test.ts:1036-1045: `~~x~~` is struck.
        assert_eq!(
            render("Use ~~strikethrough~~ here"),
            "Use <DEL>strikethrough</DEL> here"
        );
        // markdown.test.ts:1048-1057: `~x~` stays literal text, with no strike styling.
        assert_eq!(
            render("Use ~strikethrough~ literally"),
            "Use ~strikethrough~ literally"
        );
        // Both on one line: only the `~~` pair is struck.
        assert_eq!(render("a ~~b~~ c ~d~ e"), "a <DEL>b</DEL> c ~d~ e");
    }

    #[test]
    fn block_math_matches_delimiters() {
        let (raw, text) = match_block_math("$$x^2$$").unwrap();
        assert_eq!(raw, "$$x^2$$");
        assert_eq!(text, "x^2");
        let (raw, text) = match_block_math("  \\[a+b\\]\n").unwrap();
        assert_eq!(raw, "  \\[a+b\\]\n");
        assert_eq!(text, "a+b");
        assert!(match_block_math("$x$").is_none());
    }

    #[test]
    fn inline_math_rejects_prose_dollar_amounts() {
        assert_eq!(
            match_inline_math("$x+1$ and more").unwrap(),
            ("$x+1$".to_string(), "x+1".to_string())
        );
        assert_eq!(match_inline_math("$5 and $10"), None);
        assert_eq!(
            match_inline_math("\\(y\\)").unwrap(),
            ("\\(y\\)".to_string(), "y".to_string())
        );
    }

    #[test]
    fn empty_markdown_renders_nothing() {
        let mut md = markdown("   ");
        assert_eq!(md.render(20.0), Vec::<String>::new());
    }

    #[test]
    fn heading_uses_heading_theme_and_prefix_for_level_3() {
        let mut md = markdown("### Title");
        let lines = md.render(24.0);
        // Level >= 3 repeats the heading style on the `### ` prefix, then pads.
        assert!(
            lines[0].starts_with("H<**### **>H<**Title**>"),
            "{:?}",
            lines[0]
        );
    }

    #[test]
    fn paragraph_is_padded_to_width() {
        let mut md = markdown("hi");
        assert_eq!(md.render(6.0), vec!["hi    ".to_string()]);
    }

    #[test]
    fn code_block_is_indented_and_wrapped() {
        let mut md = markdown("```\nabc\n```");
        assert_eq!(md.render(8.0), vec!["  abc   ".to_string()]);
    }

    #[test]
    fn bullet_list_renders_bullets() {
        let mut md = markdown("- one\n- two");
        let lines = md.render(10.0);
        assert_eq!(lines[0], "- one     ");
        assert_eq!(lines[1], "- two     ");
    }

    #[test]
    fn ordered_list_uses_start_number() {
        let mut md = markdown("3. three\n4. four");
        let lines = md.render(10.0);
        assert_eq!(lines[0], "3. three  ");
        assert_eq!(lines[1], "4. four   ");
    }

    #[test]
    fn table_renders_borders_and_cells() {
        let mut md = markdown("| a | b |\n| --- | --- |\n| 1 | 2 |");
        let lines = md.render(20.0);
        let joined = lines.join("\n");
        assert!(joined.contains('┌'), "{joined}");
        assert!(joined.contains("a"), "{joined}");
        assert!(joined.contains("1"), "{joined}");
        // Selection regions are extracted from the marked-up table.
        assert!(!md.get_selection_regions().is_empty());
    }

    #[test]
    fn narrow_table_falls_back_to_raw_markdown() {
        let mut md = markdown("| a | b |\n| --- | --- |\n| 1 | 2 |");
        let lines = md.render(4.0);
        let joined = lines.join("\n");
        assert!(!joined.contains('┌'), "{joined}");
        assert!(joined.contains('|'), "{joined}");
    }

    #[test]
    fn blockquote_is_prefixed_with_border() {
        let mut md = markdown("> quoted");
        // Blockquotes render `theme.quote(theme.italic(text))`, then pad to width.
        assert_eq!(md.render(12.0), vec!["│ _quoted_  ".to_string()]);
    }

    #[test]
    fn horizontal_rule_is_capped_at_80_columns() {
        let mut md = markdown("---");
        let lines = md.render(100.0);
        // The rule is capped at 80 columns, then the block pads to the full width.
        assert!(lines[0].starts_with(&"─".repeat(80)), "{:?}", lines[0]);
        assert_eq!(visible_width(&lines[0]), 100);
    }

    #[test]
    fn inline_code_and_bold_use_theme() {
        let mut md = markdown("a `b` **c**");
        assert_eq!(md.render(20.0), vec!["a `b` **c**         ".to_string()]);
    }

    #[test]
    fn link_prints_url_when_text_differs() {
        let mut md = markdown("[text](https://example.com)");
        assert_eq!(
            md.render(40.0)[0],
            "textURL< (https://example.com)>         "
        );
    }

    #[test]
    fn link_hides_url_when_text_matches_href() {
        let mut md = markdown("[https://example.com](https://example.com)");
        assert_eq!(
            md.render(40.0)[0],
            "https://example.com                     "
        );
    }

    #[test]
    fn block_math_uses_math_block_style() {
        let mut md = markdown("$$x^2$$");
        let lines = md.render(20.0);
        assert!(lines[0].contains("x"), "{lines:?}");
    }

    #[test]
    fn inline_math_is_rendered_through_latex_to_unicode() {
        let mut md = markdown("value $\\alpha$ end");
        let rendered = md.render(40.0)[0].clone();
        assert!(rendered.contains("value"), "{rendered}");
    }

    #[test]
    fn render_cache_returns_same_lines_for_same_input() {
        let mut md = markdown("hello");
        let first = md.render(10.0);
        let second = md.render(10.0);
        assert_eq!(first, second);
        md.set_text("world".to_string());
        assert_eq!(md.render(10.0), vec!["world     ".to_string()]);
    }

    #[test]
    fn invalidate_clears_block_cache() {
        let mut md = markdown("a\n\nb");
        let _ = md.render(10.0);
        assert!(!md.block_cache.is_empty());
        md.invalidate();
        assert!(md.block_cache.is_empty());
    }

    #[test]
    fn transform_option_runs_before_parsing() {
        let mut md = Markdown::new(
            "x".to_string(),
            0,
            0,
            theme(),
            None,
            MarkdownOptions {
                transform: Some(Rc::new(|text: &str, width: usize| {
                    format!("{text}-{width}")
                })),
                base_url: None,
            },
        );
        assert_eq!(md.render(5.0), vec!["x-5  ".to_string()]);
    }

    /// TUIR-22: `BLOCK_MATH_REGEX` requires `[ \t]*(?:\n|$)` after the closing `$$`
    /// (packages/tui/src/components/markdown.ts:45), so display math on a line with a
    /// text tail falls back to the inline pattern
    /// (`INLINE_MATH_PATTERNS[0]`, markdown.ts:82) and the tail survives. The previous
    /// port returned `Some` from `match_block_math` unconditionally and replaced the
    /// whole paragraph with `BlockMath`, silently dropping `. Therefore y=3`.
    #[test]
    fn block_math_requires_line_end_so_trailing_text_survives() {
        let mut md = markdown("$$x=2$$. Therefore y=3");
        let lines: Vec<String> = md
            .render(60.0)
            .into_iter()
            .map(|l| l.trim_end().to_string())
            .collect();
        assert_eq!(
            lines,
            vec!["`x=2`. Therefore y=3".to_string()],
            "the trailing prose must survive as inline math + text"
        );

        // `then y=3` after the delimiter behaves the same way.
        let mut md = markdown("$$x=2$$ then y=3");
        let lines: Vec<String> = md
            .render(60.0)
            .into_iter()
            .map(|l| l.trim_end().to_string())
            .collect();
        assert_eq!(lines, vec!["`x=2` then y=3".to_string()]);
    }

    /// TUIR-21: marked's math extensions see the RAW source
    /// (packages/tui/src/components/markdown.ts:40-44, 81-111), but pulldown-cmark
    /// strips the backslash of `\(` / `\[` before `match_inline_math` ever runs, so
    /// those delimiters were never recognised and the text rendered literally.
    #[test]
    fn paren_and_bracket_math_delimiters_are_recognised() {
        // `\(...\)` -> INLINE_MATH_PATTERNS[2] (markdown.ts:84).
        let mut md = markdown("value \\(a+b\\) end");
        let lines: Vec<String> = md
            .render(60.0)
            .into_iter()
            .map(|l| l.trim_end().to_string())
            .collect();
        assert_eq!(
            lines,
            vec!["value `a+b` end".to_string()],
            "\\(...\\) must become inline math, not literal text"
        );

        // `\[...\]` at the end of its own line -> BLOCK_MATH_REGEX (markdown.ts:45).
        let mut md = markdown("\\[a+b\\]\n");
        let lines: Vec<String> = md
            .render(60.0)
            .into_iter()
            .map(|l| l.trim_end().to_string())
            .collect();
        assert_eq!(
            lines,
            vec!["  a+b".to_string()],
            "\\[...\\] alone on a line must become display math"
        );
    }

    /// TUIR-21 (second symptom): pulldown-cmark collapses `\\` to `\`, which killed
    /// latex's row separator (`ESCAPES`, packages/tui/src/latex.ts:277). The TS reads
    /// the raw source, so `$$a=b \\ c=d$$` renders as two aligned rows.
    #[test]
    fn double_backslash_row_separator_survives_in_display_math() {
        let mut md = markdown("$$a=b \\\\ c=d$$");
        let lines: Vec<String> = md
            .render(60.0)
            .into_iter()
            .map(|l| l.trim_end().to_string())
            .collect();
        assert_eq!(
            lines,
            vec!["  a=b".to_string(), "  c=d".to_string()],
            "the `\\\\` row separator must produce two rows"
        );
    }

    #[test]
    fn default_text_style_wraps_paragraph_text() {
        let mut md = Markdown::new(
            "plain".to_string(),
            0,
            0,
            theme(),
            Some(DefaultTextStyle {
                color: Some(Rc::new(|text: &str| format!("[{text}]"))),
                bg_color: None,
                bold: true,
                italic: false,
                strikethrough: false,
                underline: false,
            }),
            MarkdownOptions::default(),
        );
        let lines = md.render(20.0);
        // `applyDefaultStyle` applies the color first, then the bold wrapper.
        assert_eq!(lines[0], "**[plain]**         ");
    }

    #[test]
    fn default_background_color_extends_to_full_width() {
        let mut md = Markdown::new(
            "x".to_string(),
            0,
            0,
            theme(),
            Some(DefaultTextStyle {
                color: None,
                bg_color: Some(Rc::new(|text: &str| format!("<{text}>"))),
                bold: false,
                italic: false,
                strikethrough: false,
                underline: false,
            }),
            MarkdownOptions::default(),
        );
        assert_eq!(md.render(3.0), vec!["<x  >".to_string()]);
    }
}
