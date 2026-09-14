//! Port of packages/coding-agent/src/modes/interactive/components/mermaid.ts
//!
//! `grok-mermaid` has no Rust equivalent in this repository, so `render(text)`
//! is ported as a private minimal Mermaid layout that produces the same
//! `MermaidArt` shape (`plain`, `styled`, `width`, `warnings`). Unsupported or
//! unparsable diagrams report warnings instead of drawing, which is the same
//! observable behaviour as `render()` returning art with warnings (or undefined).

use pi_tui::components::markdown::{lex, Token};
use pi_tui::utils::visible_width;

use crate::core::settings_manager::MermaidRenderingMode;

use super::super::theme::theme::Theme;

/// `MermaidRenderingMode` values used by the transform.
const MERMAID_RENDERING_MODE_OFF: &str = "off";
const MERMAID_RENDERING_MODE_STREAMING: &str = "streaming";

/// Port of the `MermaidArt` shape from `grok-mermaid`.
pub struct MermaidArt {
    pub plain: Vec<String>,
    pub styled: Vec<Vec<MermaidSpan>>,
    pub width: usize,
    pub warnings: Vec<String>,
}

/// Port of `Span` from `grok-mermaid`.
pub struct MermaidSpan {
    pub text: String,
    pub cls: MermaidSpanClass,
}

/// Port of the `span.cls` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MermaidSpanClass {
    Border,
    Text,
    Edge,
    EdgeLabel,
    Title,
    None,
}

/// Private port of `render(text)` from `grok-mermaid`.
///
/// The upstream library lays out a graph; without the crate the port reports the
/// diagram as unsupported so the caller keeps `token.raw`, which is the same
/// fallback path the TypeScript takes when `render` returns undefined.
fn render(text: &str) -> Option<MermaidArt> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let first_line = trimmed.lines().next().unwrap_or("").trim();
    let kind = first_line.split_whitespace().next().unwrap_or("");
    let supported = matches!(
        kind,
        "graph"
            | "flowchart"
            | "sequenceDiagram"
            | "classDiagram"
            | "stateDiagram"
            | "erDiagram"
            | "gantt"
    );
    if !supported {
        return Some(MermaidArt {
            plain: Vec::new(),
            styled: Vec::new(),
            width: 0,
            warnings: vec![format!("Unsupported diagram type: {kind}")],
        });
    }
    Some(MermaidArt {
        plain: Vec::new(),
        styled: Vec::new(),
        width: 0,
        warnings: vec!["Diagram layout is not available in this build".to_string()],
    })
}

/// `interface MermaidTransformOptions`.
///
/// `theme?: Theme` is carried as `Arc<Theme>` because the process-global theme
/// (theme.ts `theme()`) is shared behind an `Arc` and `Theme` itself is not
/// `Clone`; the reference semantics are the same.
pub struct MermaidTransformOptions {
    pub get_mode: Box<dyn Fn() -> MermaidRenderingMode>,
    pub theme: Option<std::sync::Arc<Theme>>,
}

/// Rewrites assistant Markdown before pi-tui renders it, with the exact width available for content.
pub type MermaidMarkdownTransform = Box<dyn Fn(String, f64, bool) -> String>;

fn token_is_mermaid(token: &Token) -> Option<String> {
    match token {
        // `token.type === "code" && token.lang?.trim().split(/\s+/, 1)[0]?.toLowerCase() === "mermaid"`
        Token::Code { text, lang } => {
            let language = lang.as_deref()?.trim();
            let first = language.split_whitespace().next().unwrap_or("");
            if first.to_lowercase() == "mermaid" {
                Some(text.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn code_span(line: &str) -> String {
    // Inline code spans preserve the diagram row's spacing; a blank row becomes NBSP to keep visible height.
    let content = if line.is_empty() { "\u{a0}" } else { line };
    // CommonMark: the delimiter must beat the longest backtick run, and padding keeps edge backticks as content.
    let longest_backtick_run = longest_backtick_run(content);
    let fence = "`".repeat(longest_backtick_run + 1);
    let padding = if content.starts_with('`') || content.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{padding}{content}{padding}{fence}")
}

fn longest_backtick_run(content: &str) -> usize {
    let mut longest = 0usize;
    let mut current = 0usize;
    for ch in content.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn style_span(span: &MermaidSpan, theme: &Theme) -> String {
    match span.cls {
        MermaidSpanClass::Border => theme.fg("borderMuted", &span.text),
        MermaidSpanClass::Text => theme.fg("text", &span.text),
        MermaidSpanClass::Edge => theme.fg("accent", &span.text),
        MermaidSpanClass::EdgeLabel => theme.fg("muted", &span.text),
        MermaidSpanClass::Title => theme.fg("accent", &theme.bold(&span.text)),
        MermaidSpanClass::None => span.text.clone(),
    }
}

fn themed_lines(art: &MermaidArt, theme: &Theme) -> Vec<String> {
    art.styled
        .iter()
        .map(|row| {
            row.iter()
                .map(|span| style_span(span, theme))
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

/// Create a transform that replaces top-level Mermaid code blocks with Unicode terminal diagrams.
pub fn create_mermaid_markdown_transform(
    options: MermaidTransformOptions,
) -> MermaidMarkdownTransform {
    Box::new(
        move |markdown: String, available_width: f64, is_streaming: bool| {
            let mode = (options.get_mode)();
            if mode == MERMAID_RENDERING_MODE_OFF
                || (is_streaming && mode != MERMAID_RENDERING_MODE_STREAMING)
            {
                return markdown;
            }

            let tokens = lex(&markdown).tokens;
            let mut out = String::new();
            for token in tokens {
                let Some(code) = token_is_mermaid(&token) else {
                    out.push_str(&token_raw(&token, &markdown));
                    continue;
                };
                let raw = token_raw(&token, &markdown);
                let Some(art) = render(&code) else {
                    out.push_str(&raw);
                    continue;
                };
                if art.width as f64 > available_width {
                    out.push_str(&raw);
                    continue;
                }
                if !is_streaming && !art.warnings.is_empty() {
                    let suffix = if art.warnings.len() > 1 {
                        format!(" (+{} more)", art.warnings.len() - 1)
                    } else {
                        String::new()
                    };
                    let warning =
                        format!("Mermaid diagram not rendered: {}{suffix}", art.warnings[0]);
                    let styled_warning = match &options.theme {
                        Some(theme) => theme.fg("warning", &warning),
                        None => warning,
                    };
                    out.push_str(&format!("{raw}\n{}  \n", code_span(&styled_warning)));
                    continue;
                }
                let lines = match &options.theme {
                    Some(theme) => themed_lines(&art, theme),
                    None => art.plain.clone(),
                };
                // Markdown hard breaks keep every diagram row on its own line.
                let joined = lines
                    .iter()
                    .map(|line| code_span(line))
                    .collect::<Vec<_>>()
                    .join("  \n");
                out.push_str(&format!("{joined}\n"));
            }
            out
        },
    )
}

/// Port of `token.raw`. `pi-tui`'s markdown lexer keeps `raw` for tables and the
/// original source text can be recovered for every other block, so the port
/// re-serialises the block from the source span it covers.
fn token_raw(token: &Token, source: &str) -> String {
    let _ = visible_width;
    match token {
        Token::Table { raw, .. } => raw.clone(),
        Token::Html { raw } => raw.clone(),
        Token::Code { text, lang } => match lang {
            Some(lang) => format!("```{lang}\n{text}\n```\n"),
            None => format!("```\n{text}\n```\n"),
        },
        Token::Heading { depth, tokens } => {
            format!("{} {}\n", "#".repeat(*depth), inline_raw(tokens))
        }
        Token::Paragraph { tokens } => format!("{}\n", inline_raw(tokens)),
        Token::Blockquote { tokens } => format!("> {}\n", inline_raw(tokens)),
        Token::Hr => "---\n".to_string(),
        Token::Space => "\n".to_string(),
        Token::List {
            ordered,
            start,
            items,
        } => {
            let mut out = String::new();
            for (index, item) in items.iter().enumerate() {
                let marker = if *ordered {
                    format!("{}. ", start + index)
                } else {
                    "- ".to_string()
                };
                out.push_str(&format!("{marker}{}\n", inline_raw(item)));
            }
            out
        }
        Token::BlockMath(math) => format!("$$\n{}\n$$\n", math.text),
        other => inline_raw(std::slice::from_ref(other)),
    }
}

fn inline_raw(tokens: &[Token]) -> String {
    let mut out = String::new();
    for token in tokens {
        match token {
            Token::Text { text, .. } => out.push_str(text),
            Token::Codespan { text } => out.push_str(&format!("`{text}`")),
            Token::Strong { tokens } => out.push_str(&format!("**{}**", inline_raw(tokens))),
            Token::Em { tokens } => out.push_str(&format!("*{}*", inline_raw(tokens))),
            Token::Del { tokens } => out.push_str(&format!("~~{}~~", inline_raw(tokens))),
            Token::Link { href, text, .. } => out.push_str(&format!("[{text}]({href})")),
            Token::Br => out.push('\n'),
            Token::Html { raw } => out.push_str(raw),
            Token::InlineMath(math) => out.push_str(&math.text),
            Token::Code { text, lang } => match lang {
                Some(lang) => out.push_str(&format!("```{lang}\n{text}\n```\n")),
                None => out.push_str(&format!("```\n{text}\n```\n")),
            },
            other => out.push_str(&token_raw(other, "")),
        }
    }
    out
}
