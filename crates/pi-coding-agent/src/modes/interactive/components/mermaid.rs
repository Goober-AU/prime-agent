//! Port of packages/coding-agent/src/modes/interactive/components/mermaid.ts
//!
//! Mermaid layout uses a native Unicode renderer. Only complete top-level
//! Mermaid fences are replaced; all other Markdown retains its original bytes.

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

/// Native layout with explicit fallback for invalid or unsupported diagrams.
fn render(text: &str) -> Option<MermaidArt> {
    if text.trim().is_empty() {
        return None;
    }
    match mermansi::render_source_unicode(text) {
        Ok(output) => {
            let plain: Vec<String> = output.lines().map(str::to_string).collect();
            let width = plain
                .iter()
                .map(|line| visible_width(line))
                .max()
                .unwrap_or(0);
            let styled = plain
                .iter()
                .map(|line| {
                    vec![MermaidSpan {
                        text: line.clone(),
                        cls: MermaidSpanClass::Text,
                    }]
                })
                .collect();
            Some(MermaidArt {
                plain,
                styled,
                width,
                warnings: Vec::new(),
            })
        }
        Err(error) => Some(MermaidArt {
            plain: Vec::new(),
            styled: Vec::new(),
            width: 0,
            warnings: vec![error.to_string()],
        }),
    }
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

/// Replace top-level fenced Mermaid blocks while preserving source ranges.
/// Re-serializing lexer tokens loses list indentation, link targets and math.
pub fn create_mermaid_markdown_transform(
    options: MermaidTransformOptions,
) -> MermaidMarkdownTransform {
    Box::new(move |markdown, available_width, is_streaming| {
        use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
        let mode = (options.get_mode)();
        if mode == MERMAID_RENDERING_MODE_OFF
            || (is_streaming && mode != MERMAID_RENDERING_MODE_STREAMING)
        {
            return markdown;
        }
        let mut out = String::new();
        let mut cursor = 0;
        let mut depth = 0usize;
        let mut block: Option<(usize, String)> = None;
        for (event, range) in Parser::new(&markdown).into_offset_iter() {
            match event {
                Event::Start(tag) => {
                    if depth == 0 {
                        if let Tag::CodeBlock(CodeBlockKind::Fenced(language)) = &tag {
                            if language
                                .split_whitespace()
                                .next()
                                .is_some_and(|s| s.eq_ignore_ascii_case("mermaid"))
                            {
                                block = Some((range.start, String::new()));
                            }
                        }
                    }
                    depth += 1;
                }
                Event::Text(text) => {
                    if let Some((_, code)) = &mut block {
                        code.push_str(&text);
                    }
                }
                Event::End(tag) => {
                    depth = depth.saturating_sub(1);
                    if tag != TagEnd::CodeBlock {
                        continue;
                    }
                    let Some((start, code)) = block.take() else {
                        continue;
                    };
                    let raw = &markdown[start..range.end];
                    out.push_str(&markdown[cursor..start]);
                    cursor = range.end;
                    let Some(art) = render(&code) else {
                        out.push_str(raw);
                        continue;
                    };
                    if art.width as f64 > available_width
                        || art.plain.is_empty() && art.warnings.is_empty()
                    {
                        out.push_str(raw);
                        continue;
                    }
                    if !art.warnings.is_empty() {
                        out.push_str(raw);
                        if !is_streaming {
                            let warning =
                                format!("Mermaid diagram not rendered: {}", art.warnings[0]);
                            let warning = options
                                .theme
                                .as_ref()
                                .map(|t| t.fg("warning", &warning))
                                .unwrap_or(warning);
                            out.push_str(&format!("\n{}  \n", code_span(&warning)));
                        }
                        continue;
                    }
                    let lines = options
                        .theme
                        .as_ref()
                        .map(|theme| themed_lines(&art, theme))
                        .unwrap_or(art.plain);
                    out.push_str(
                        &lines
                            .iter()
                            .map(|line| code_span(line))
                            .collect::<Vec<_>>()
                            .join("  \n"),
                    );
                    out.push('\n');
                }
                _ => {}
            }
        }
        out.push_str(&markdown[cursor..]);
        out
    })
}
