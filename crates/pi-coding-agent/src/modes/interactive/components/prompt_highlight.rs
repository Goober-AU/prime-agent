//! Port of packages/coding-agent/src/modes/interactive/components/prompt-highlight.ts

use std::sync::OnceLock;

use regex::Regex;

use pi_tui::utils::{graphemes, visible_width};

use super::super::theme::theme::{theme, ThemeColor};

const ARG_TOKEN_PATTERN: &str =
    r#"@"[^"\n]*"|@(?:\\[^\s\x1b]|[^\s\x1b|])+|--[A-Za-z0-9][A-Za-z0-9-]*"#;
/// Also matches a bare `--` end-of-options separator; only used for argument-taking slash commands.
const ARG_TOKEN_PATTERN_WITH_SEPARATOR: &str =
    r#"@"[^"\n]*"|@(?:\\[^\s\x1b]|[^\s\x1b|])+|--[A-Za-z0-9][A-Za-z0-9-]*|--(?=\s|$)"#;
const FG_SGR_PATTERN: &str = r"\x1b\[(?:0|39|3[0-7]|9[0-7]|38;[0-9;]+)m";
/// Escape sequences the editor splices into displayed text (cursor highlight, IME marker).
const CURSOR_ESCAPE_PATTERN: &str = r"\x1b\[[0-9;]*m|\x1b_[^\x07]*\x07";

const MASK_BASE_START: u32 = 0xe000;
/// Each masked grapheme gets its own private-use base char, so restoring is a lookup, not positional.
const MASK_CAPACITY: usize = (0xf8ff - MASK_BASE_START + 1) as usize;
const MASK_EXTRA_WIDTH: char = '\u{ff9e}';
const MASK_PATTERN: &str = "[\u{E000}-\u{F8FF}]\u{FF9E}*";
/// Literal mask-range characters would alias generated placeholders; messages containing them skip masking.
const MASK_LITERAL_PATTERN: &str = "[\u{E000}-\u{F8FF}\u{FF9E}]";

fn compiled(slot: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    slot.get_or_init(|| Regex::new(pattern).expect("valid regex literal"))
}

fn arg_token_re() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    compiled(&PATTERN, ARG_TOKEN_PATTERN)
}

fn arg_token_separator_re() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    compiled(&PATTERN, ARG_TOKEN_PATTERN_WITH_SEPARATOR)
}

fn fg_sgr_re() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    compiled(&PATTERN, FG_SGR_PATTERN)
}

fn cursor_escape_re() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    compiled(&PATTERN, CURSOR_ESCAPE_PATTERN)
}

fn mask_literal_re() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    compiled(&PATTERN, MASK_LITERAL_PATTERN)
}

fn mask_re() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    compiled(&PATTERN, MASK_PATTERN)
}

/// Port of `ArgTokenSpan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgTokenSpan {
    pub start: usize,
    pub end: usize,
    pub color: ThemeColor,
}

fn token_color(token: &str) -> ThemeColor {
    if token.starts_with('@') {
        "success"
    } else {
        "mdLink"
    }
}

fn has_token_boundary(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    text[..index]
        .chars()
        .next_back()
        .map(char::is_whitespace)
        .unwrap_or(false)
}

/// `findArgTokens(text, fromIndex = 0, includeBareSeparator = false)`.
pub fn find_arg_tokens(
    text: &str,
    from_index: usize,
    include_bare_separator: bool,
) -> Vec<ArgTokenSpan> {
    let pattern = if include_bare_separator {
        arg_token_separator_re()
    } else {
        arg_token_re()
    };
    let mut spans: Vec<ArgTokenSpan> = Vec::new();
    for matched in pattern.find_iter(text) {
        if matched.start() < from_index || !has_token_boundary(text, matched.start()) {
            continue;
        }
        spans.push(ArgTokenSpan {
            start: matched.start(),
            end: matched.end(),
            color: token_color(matched.as_str()),
        });
    }
    spans
}

/// Foreground SGR active at index; `theme.fg()` closes with `\x1b[39m`, so it must be re-emitted.
fn active_fg_before(line: &str, index: usize) -> String {
    let mut active = String::new();
    let prefix = &line[..index.min(line.len())];
    for sgr in fg_sgr_re().find_iter(prefix) {
        active = if sgr.as_str() == "\x1b[0m" {
            String::new()
        } else {
            sgr.as_str().to_string()
        };
    }
    active
}

/// Port of `styleArgumentTokens`.
pub fn style_argument_tokens(
    text: &str,
    style_other: &dyn Fn(&str) -> String,
    include_bare_separator: bool,
) -> String {
    let mut result = String::new();
    let mut offset = 0usize;
    for token in find_arg_tokens(text, 0, include_bare_separator) {
        result.push_str(&style_other(&text[offset..token.start]));
        result.push_str(&theme().fg(token.color, &text[token.start..token.end]));
        offset = token.end;
    }
    result + &style_other(&text[offset..])
}

/// Port of `styleArgumentTokens`' default `styleOther` (the identity function).
pub fn style_argument_tokens_plain(text: &str, include_bare_separator: bool) -> String {
    style_argument_tokens(
        text,
        &|segment: &str| segment.to_string(),
        include_bare_separator,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MaskedGrapheme {
    segment: String,
    color: ThemeColor,
}

#[derive(Debug)]
struct ArgTokenSpanRun {
    start: usize,
    end: usize,
    color: ThemeColor,
    text: String,
}

fn flush_run(
    line: &str,
    result: &mut String,
    copied: &mut usize,
    run: &mut Option<ArgTokenSpanRun>,
) {
    if let Some(run) = run.take() {
        result.push_str(&line[*copied..run.start]);
        result.push_str(&theme().fg(run.color, &run.text));
        result.push_str(&active_fg_before(line, run.start));
        *copied = run.end;
    }
}

/// Masks the slash command and @path/--flag tokens with same-width placeholders before markdown layout.
pub struct PromptTokenMask {
    pub text: String,
    graphemes: Vec<MaskedGrapheme>,
}

impl PromptTokenMask {
    /// `constructor(source, commandEnd = 0, includeBareSeparator = false)`.
    pub fn new(source: &str, command_end: usize, include_bare_separator: bool) -> Self {
        let mut mask = Self {
            text: String::new(),
            graphemes: Vec::new(),
        };
        // Markdown turns tabs into three spaces; a masked raw tab would be restored into a three-column layout.
        let source = source.replace('\t', "   ");
        if mask_literal_re().is_match(&source) {
            mask.text = source;
            return mask;
        }
        let mut tokens: Vec<ArgTokenSpan> = Vec::new();
        if command_end > 0 {
            tokens.push(ArgTokenSpan {
                start: 0,
                end: command_end,
                color: "accent",
            });
        }
        tokens.extend(find_arg_tokens(
            &source,
            command_end,
            include_bare_separator,
        ));

        let mut text = String::new();
        let mut cursor = 0usize;
        for token in tokens {
            text.push_str(&source[cursor..token.start]);
            for segment_text in graphemes(&source[token.start..token.end]) {
                let width = visible_width(&segment_text);
                if width == 0 {
                    // Zero-width graphemes stay literal: invisible either way, and extracted text stays exact.
                    text.push_str(&segment_text);
                    continue;
                }
                if mask.graphemes.len() == MASK_CAPACITY {
                    mask.text = source;
                    mask.graphemes = Vec::new();
                    return mask;
                }
                let base = char::from_u32(MASK_BASE_START + mask.graphemes.len() as u32).unwrap();
                text.push(base);
                for _ in 1..width {
                    text.push(MASK_EXTRA_WIDTH);
                }
                mask.graphemes.push(MaskedGrapheme {
                    segment: segment_text,
                    color: token.color,
                });
            }
            cursor = token.end;
        }
        mask.text = format!("{text}{}", &source[cursor..]);
        mask
    }

    fn grapheme_for(&self, placeholder: &str) -> Option<&MaskedGrapheme> {
        let base = placeholder.chars().next()?;
        let index = (base as u32).checked_sub(MASK_BASE_START)? as usize;
        self.graphemes.get(index)
    }

    /// Restores masked graphemes in text extracted from a render, e.g. selection-region cell content.
    pub fn restore_text(&self, text: &str) -> String {
        let mut out = String::new();
        let mut last = 0usize;
        for matched in mask_re().find_iter(text) {
            out.push_str(&text[last..matched.start()]);
            match self.grapheme_for(matched.as_str()) {
                Some(grapheme) => out.push_str(&grapheme.segment),
                None => out.push_str(matched.as_str()),
            }
            last = matched.end();
        }
        out.push_str(&text[last..]);
        out
    }

    /// Port of `restoreLine`.
    pub fn restore_line(&self, line: &str) -> String {
        let mut result = String::new();
        let mut copied = 0usize;
        let mut run: Option<ArgTokenSpanRun> = None;

        for matched in mask_re().find_iter(line) {
            let grapheme = match self.grapheme_for(matched.as_str()) {
                Some(grapheme) => grapheme,
                // literal mask-range character from an unmasked source; leave it untouched
                None => continue,
            };
            let matches_run = match &run {
                Some(run) => run.color == grapheme.color && run.end == matched.start(),
                None => false,
            };
            if matches_run {
                let run = run.as_mut().unwrap();
                run.text.push_str(&grapheme.segment);
                run.end = matched.end();
            } else {
                flush_run(line, &mut result, &mut copied, &mut run);
                run = Some(ArgTokenSpanRun {
                    start: matched.start(),
                    end: matched.end(),
                    color: grapheme.color,
                    text: grapheme.segment.clone(),
                });
            }
        }
        flush_run(line, &mut result, &mut copied, &mut run);
        result + &line[copied..]
    }
}

/// Styles tokens in laid-out editor lines from spans on the logical source lines; `reset()` before each render pass.
#[derive(Default)]
pub struct ArgTokenHighlighter {
    spans: Vec<Vec<ArgTokenSpan>>,
}

impl ArgTokenHighlighter {
    pub fn new() -> Self {
        Self { spans: Vec::new() }
    }

    pub fn reset(&mut self, lines: &[String], include_bare_separator: bool) {
        self.spans = lines
            .iter()
            .map(|line| find_arg_tokens(line, 0, include_bare_separator))
            .collect();
    }

    /// displayText is chunkText with cursor escapes spliced in; chunkText starts at sourceStart within sourceLine.
    pub fn highlight_line(
        &self,
        display_text: &str,
        chunk_text: &str,
        source_line: usize,
        source_start: usize,
    ) -> String {
        let range_end = source_start + chunk_text.len();
        let mut spans: Vec<ArgTokenSpan> = Vec::new();
        let source_spans = self
            .spans
            .get(source_line)
            .map(|spans| spans.as_slice())
            .unwrap_or(&[]);
        for span in source_spans {
            if span.end <= source_start {
                continue;
            }
            if span.start >= range_end {
                break;
            }
            spans.push(ArgTokenSpan {
                start: span.start.max(source_start) - source_start,
                end: span.end.min(range_end) - source_start,
                color: span.color,
            });
        }
        if spans.is_empty() {
            return display_text.to_string();
        }

        // Maps visible code-unit offsets to displayText offsets, skipping the editor's cursor escapes.
        let mut visible_start: Vec<usize> = Vec::new();
        let mut pos = 0usize;
        for sequence in cursor_escape_re().find_iter(display_text) {
            while pos < sequence.start() {
                visible_start.push(pos);
                pos += 1;
            }
            pos = sequence.end();
        }
        while pos < display_text.len() {
            visible_start.push(pos);
            pos += 1;
        }

        let mut result = String::new();
        let mut copied = 0usize;
        for span in spans {
            let start = visible_start
                .get(span.start)
                .copied()
                .unwrap_or(display_text.len());
            let end = visible_start
                .get(span.end.saturating_sub(1))
                .copied()
                .unwrap_or(display_text.len().saturating_sub(1))
                + 1;
            // The cursor splice may carry a full reset mid-span; wrap each segment so the token color survives it.
            let styled = display_text[start..end]
                .split("\x1b[0m")
                .map(|segment| theme().fg(span.color, segment))
                .collect::<Vec<_>>()
                .join("\x1b[0m");
            result.push_str(&display_text[copied..start]);
            result.push_str(&styled);
            copied = end;
        }
        result + &display_text[copied..]
    }
}
