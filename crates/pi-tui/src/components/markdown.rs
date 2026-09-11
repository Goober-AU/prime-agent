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
        (format!("\\[{}]", &body[..end]), body[..end].to_string())
    } else {
        return None;
    };

    let after_body = &rest[indent.len() + raw_body.len()..];
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
    if !after.is_empty() && !after.starts_with('\n') {
        return None;
    }

    Some((format!("{indent}{raw_body}{trailing}"), text.trim().to_string()))
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
                return Some((format!("\\[{}]", &body[..end]), body[..end].to_string()));
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
    builder.finish(text)
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
        in_head: bool,
        current_row: Vec<Vec<Token>>,
        cell: Vec<Token>,
    },
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
    table_start: Option<usize>,
}

impl TokenBuilder {
    fn new(use_math: bool) -> Self {
        Self {
            stack: vec![Frame {
                kind: FrameKind::Root,
                tokens: Vec::new(),
            }],
            use_math,
            table_start: None,
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
            Tag::Table(_) => {
                self.table_start = Some(0);
                FrameKind::Table {
                    header: Vec::new(),
                    rows: Vec::new(),
                    raw: String::new(),
                    in_head: false,
                    current_row: Vec::new(),
                    cell: Vec::new(),
                }
            }
            Tag::TableHead => {
                if let Some(Frame {
                    kind: FrameKind::Table { in_head, .. },
                    ..
                }) = self.stack.last_mut()
                {
                    *in_head = true;
                }
                return;
            }
            Tag::TableRow => {
                if let Some(Frame {
                    kind: FrameKind::Table { current_row, .. },
                    ..
                }) = self.stack.last_mut()
                {
                    current_row.clear();
                }
                return;
            }
            Tag::TableCell => {
                if let Some(Frame {
                    kind: FrameKind::Table { cell, .. },
                    ..
                }) = self.stack.last_mut()
                {
                    cell.clear();
                }
                return;
            }
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
            TagEnd::TableHead => {
                if let Some(Frame {
                    kind: FrameKind::Table { header, in_head, current_row, .. },
                    ..
                }) = self.stack.last_mut()
                {
                    *header = std::mem::take(current_row);
                    *in_head = false;
                }
                return;
            }
            TagEnd::TableRow => {
                if let Some(Frame {
                    kind: FrameKind::Table { rows, current_row, .. },
                    ..
                }) = self.stack.last_mut()
                {
                    let row = std::mem::take(current_row);
                    rows.push(row);
                }
                return;
            }
            TagEnd::TableCell => {
                if let Some(Frame {
                    kind: FrameKind::Table { current_row, cell, .. },
                    ..
                }) = self.stack.last_mut()
                {
                    let cell_tokens = std::mem::take(cell);
                    current_row.push(cell_tokens);
                }
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
            FrameKind::CodeBlock { lang, text } => Token::Code { text, lang },
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
                ..
            } => Token::Table { header, rows, raw },
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
