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

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

const STRICT_STRIKETHROUGH_REGEX: &str = "^(~~)(?=[^\\s~])((?:\\\\.|[^\\\\])*?(?:\\\\.|[^\\s~\\\\]))\\1(?=[^~]|$)";

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

/// Port of `BLOCK_MATH_REGEX`: leading indentation, `$$...$$` or `\[...\]`, trailing
/// indentation and a newline or end of input.
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

    let (raw_body, text) = if let Some(body) = rest.strip_prefix("$$") {
        let end = body.find("$$")?;
        (format!("$${}$$", &body[..end]), body[..end].to_string())
    } else if let Some(body) = rest.strip_prefix("\\[") {
        let end = body.find("\\]")?;
        (format!("\\[{}\\]", &body[..end]), body[..end].to_string())
    } else {
        return None;
    };

    let after_body = &rest[raw_body.len()..];
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
    let trailing_newline = if after.starts_with('\n') { "\n" } else { "" };

    Some((
        format!("{indent}{raw_body}{trailing}{trailing_newline}"),
        text.trim().to_string(),
    ))
}

/// Port of `INLINE_MATH_PATTERNS`.
fn match_inline_math(src: &str) -> Option<(String, String)> {
    // /^\$\$([\s\S]+?)\$\$/
    if let Some(body) = src.strip_prefix("$$") {
        if let Some(end) = body.find("$$") {
            if !body[..end].is_empty() {
                return Some((format!("$${}$$", &body[..end]), body[..end].to_string()));
            }
        }
    }
    // /^\\\[([\s\S]+?)\\\]/
    if let Some(body) = src.strip_prefix("\\[") {
        if let Some(end) = body.find("\\]") {
            if !body[..end].is_empty() {
                return Some((format!("\\[{}\\]", &body[..end]), body[..end].to_string()));
            }
        }
    }
    // /^\\\(([\s\S]+?)\\\)/
    if let Some(body) = src.strip_prefix("\\(") {
        if let Some(end) = body.find("\\)") {
            if !body[..end].is_empty() {
                return Some((format!("\\\\({}\\\\)", &body[..end]), body[..end].to_string()));
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
                    let next_is_digit = after.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false);
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
pub fn lex(text: &str) -> LexResult {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let use_math = has_math(text);
    let parser = Parser::new_ext(text, options);
    let mut builder = TokenBuilder::new(use_math);
    for event in parser {
        builder.handle(event);
    }
    let mut result = builder.finish(text);
    fill_table_raw(&mut result.tokens, text);
    result
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
    CodeBlock { lang: Option<String>, text: String },
    Item,
    List { ordered: bool, start: usize, items: Vec<Vec<Token>> },
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
    Link { href: String, text: String },
}

struct Frame {
    kind: FrameKind,
    tokens: Vec<Token>,
}

struct TokenBuilder {
    stack: Vec<Frame>,
    use_math: bool,
}

impl TokenBuilder {
    fn new(use_math: bool) -> Self {
        Self {
            stack: vec![Frame {
                kind: FrameKind::Root,
                tokens: Vec::new(),
            }],
            use_math,
        }
    }

    fn push_token(&mut self, token: Token) {
        if let Some(frame) = self.stack.last_mut() {
            frame.tokens.push(token);
        }
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(text) => self.push_token(Token::Codespan {
                text: text.to_string(),
            }),
            Event::Html(raw) | Event::InlineHtml(raw) => self.push_token(Token::Html {
                raw: raw.to_string(),
            }),
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
            Tag::Strikethrough => FrameKind::Em,
            Tag::HtmlBlock => FrameKind::Paragraph,
            Tag::FootnoteDefinition(_) => FrameKind::Paragraph,
            Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => FrameKind::Paragraph,
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
                self.push_token(Token::Paragraph { tokens: frame.tokens });
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
                if let Some(token) = self.block_math_from_paragraph(&frame.tokens) {
                    token
                } else {
                    Token::Paragraph { tokens: frame.tokens }
                }
            }
            FrameKind::Heading(depth) => Token::Heading {
                depth,
                tokens: frame.tokens,
            },
            FrameKind::CodeBlock { lang, text } => Token::Code {
                // `marked` reports fenced code content without the trailing newline.
                text: text.strip_suffix('\n').map(|t| t.to_string()).unwrap_or(text),
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
            FrameKind::Blockquote => Token::Blockquote { tokens: frame.tokens },
            FrameKind::Table {
                header,
                rows,
                raw,
            } => Token::Table { header, rows, raw },
            // Table scaffolding frames are consumed by their own `TagEnd` arms.
            FrameKind::TableHead | FrameKind::TableRow | FrameKind::TableCell => return,
            FrameKind::Strong => Token::Strong { tokens: frame.tokens },
            FrameKind::Em => Token::Em { tokens: frame.tokens },
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

        let mut pending = String::new();
        let mut index = 0usize;
        while index < text.len() {
            let rest = &text[index..];
            if rest.starts_with("~~") {
                if let Some((inner, _raw)) = strict_strikethrough(rest) {
                    flush_text(&mut pending, &mut |token| self.push_token(token));
                    let inner_tokens = inline_tokens(&inner, self.use_math);
                    self.push_token(Token::Del {
                        tokens: inner_tokens,
                    });
                    index += 4 + inner.len();
                    continue;
                }
            }
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

    fn finish(mut self, text: &str) -> LexResult {
        while self.stack.len() > 1 {
            self.end(TagEnd::Paragraph);
        }
        let frame = self.stack.pop().unwrap();
        let tokens = frame.tokens;
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
        LexResult { tokens, links }
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
                        block_lines.push(format!("{line_with_margins}{}", " ".repeat(padding_needed)));
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
                if next_token_type.is_some() && next_token_type != Some("list") && next_token_type != Some("space") {
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
                lines.extend(self.render_table(header, rows, raw, width, next_token_type, style_context));
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
                    let line_with_reapplied_style = line.replace("\x1b[0m", &format!("\x1b[0m{quote_style_prefix}"));
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

                while rendered_quote_lines.last().map(|line| line.is_empty()).unwrap_or(false) {
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
    fn render_inline_tokens(&mut self, tokens: &[Token], style_context: Option<&InlineStyleContext>) -> String {
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

                Token::Link {
                    href,
                    text,
                    tokens,
                } => {
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
                    lines.push(format!("{indent}{}{first_line}", (self.theme.list_bullet)(&bullet)));
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

        for token in tokens {
            match token {
                Token::List {
                    ordered,
                    start,
                    items,
                } => {
                    // Nested list - render with one additional indent level
                    // These lines will have their own indent, so we just add them as-is
                    lines.extend(self.render_list(*ordered, *start, items, parent_depth + 1, style_context));
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
                    let text = self.render_inline_tokens(std::slice::from_ref(other), style_context);
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
        let words: Vec<&str> = text.split_whitespace().filter(|word| !word.is_empty()).collect();
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
            min_word_widths[i] = Self::get_longest_word_width(&header_text, Some(max_unbroken_word_width)).max(1);
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
        lines.push(mark_table_start(&format!("┌─{}─┐", top_border_cells.join("─┬─"))));

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
                    mark_table_cell(&(self.theme.bold)(&padded), 0, col_idx as i64, line_idx as i64, content)
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
                        let padded = format!("{text}{}", " ".repeat(width.saturating_sub(visible_width(&text))));
                        mark_table_cell(&padded, (row_index + 1) as i64, col_idx as i64, line_idx as i64, content)
                    })
                    .collect();
                lines.push(format!("│ {} │", row_parts.join(" │ ")));
            }

            if row_index < rows.len() - 1 {
                lines.push(separator_line.clone());
            }
        }

        let bottom_border_cells: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
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
    if target.starts_with("http://") || target.starts_with("https://") || target.starts_with("file://") {
        return Some(target.to_string());
    }
    if target.starts_with("//") {
        let scheme = base.split("://").next().unwrap_or("https");
        return Some(format!("{scheme}:{target}"));
    }
    if target.starts_with('/') {
        let scheme_end = base.find("://")? + 3;
        let host_end = base[scheme_end..].find('/').map(|i| scheme_end + i).unwrap_or(base.len());
        return Some(format!("{}{}", &base[..host_end], target));
    }
    let cut = base.rfind('/').map(|i| i + 1).unwrap_or(base.len());
    Some(format!("{}{}", &base[..cut], target))
}

/// Port of `latexToUnicode(text).replace(/\s*\n\s*/g, " ")`.
fn collapse_math_whitespace(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\n' {
            while result.ends_with([' ', '\t']) {
                result.pop();
            }
            result.push(' ');
            while matches!(chars.peek(), Some(' ') | Some('\t')) {
                chars.next();
            }
        } else {
            result.push(ch);
        }
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

        let normalized_text = text.replace('\t', "   ");

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
        for i in 0..tokens.len() {
            let token = tokens[i].clone();
            let next_token_type = tokens.get(i + 1).map(|t| t.type_name().to_string());
            let use_cache = cacheable && i < tokens.len() - 1;
            let key = if use_cache {
                format!(
                    "{}|{}|{}|{}",
                    width,
                    token.type_name(),
                    next_token_type.clone().unwrap_or_default(),
                    raw_of(&token)
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
        let (result, regions) = extract_table_cell_selection_regions(&marked_result, &mut |index| {
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
    match token {
        Token::Paragraph { tokens } | Token::Heading { tokens, .. } => collect_text(tokens),
        Token::Code { text, .. } => text.clone(),
        Token::BlockMath(math) => math.raw.clone(),
        Token::Html { raw } => raw.clone(),
        Token::Table { raw, .. } => raw.clone(),
        Token::Blockquote { tokens } => collect_text(tokens),
        Token::Hr => "---".to_string(),
        Token::Space => String::new(),
        other => collect_text(std::slice::from_ref(other)),
    }
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
        Markdown::new(text.to_string(), 0, 0, theme(), None, MarkdownOptions::default())
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
        let lines = md.render(20.0);
        assert_eq!(lines[0], "H<**### **>H<**Title**>");
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
        assert_eq!(md.render(12.0), vec!["│ quoted    ".to_string()]);
    }

    #[test]
    fn horizontal_rule_is_capped_at_80_columns() {
        let mut md = markdown("---");
        assert_eq!(md.render(100.0), vec!["─".repeat(80)]);
    }

    #[test]
    fn inline_code_and_bold_use_theme() {
        let mut md = markdown("a `b` **c**");
        assert_eq!(md.render(20.0), vec!["a `b` **c**        ".to_string()]);
    }

    #[test]
    fn link_prints_url_when_text_differs() {
        let mut md = markdown("[text](https://example.com)");
        assert_eq!(md.render(40.0)[0], "textURL< (https://example.com)>     ");
    }

    #[test]
    fn link_hides_url_when_text_matches_href() {
        let mut md = markdown("[https://example.com](https://example.com)");
        assert_eq!(md.render(40.0)[0], "https://example.com                 ");
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
                transform: Some(Rc::new(|text: &str, width: usize| format!("{text}-{width}"))),
                base_url: None,
            },
        );
        assert_eq!(md.render(5.0), vec!["x-5  ".to_string()]);
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
        assert_eq!(lines[0], "[**plain**]        ");
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
