//! Port of packages/coding-agent/src/core/export-html/ansi-to-html.ts
//!
//! ANSI escape code to HTML converter.
//!
//! Converts terminal ANSI color/style codes to HTML with inline styles.
//! Supports:
//! - Standard foreground colors (30-37) and bright variants (90-97)
//! - Standard background colors (40-47) and bright variants (100-107)
//! - 256-color palette (38;5;N and 48;5;N)
//! - RGB true color (38;2;R;G;B and 48;2;R;G;B)
//! - Text styles: bold (1), dim (2), italic (3), underline (4)
//! - Reset (0)

use std::sync::OnceLock;

use regex::Regex;

const ANSI_COLORS: [&str; 16] = [
    "#000000", // 0: black
    "#800000", // 1: red
    "#008000", // 2: green
    "#808000", // 3: yellow
    "#000080", // 4: blue
    "#800080", // 5: magenta
    "#008080", // 6: cyan
    "#c0c0c0", // 7: white
    "#808080", // 8: bright black
    "#ff0000", // 9: bright red
    "#00ff00", // 10: bright green
    "#ffff00", // 11: bright yellow
    "#0000ff", // 12: bright blue
    "#ff00ff", // 13: bright magenta
    "#00ffff", // 14: bright cyan
    "#ffffff", // 15: bright white
];

/// Convert 256-color index to hex.
fn color256_to_hex(index: i64) -> String {
    if index < 16 {
        return ANSI_COLORS[index as usize].to_string();
    }

    if index < 232 {
        let cube_index = index - 16;
        let r = cube_index / 36;
        let g = (cube_index % 36) / 6;
        let b = cube_index % 6;
        let to_component = |n: i64| if n == 0 { 0 } else { 55 + n * 40 };
        let to_hex = |n: i64| format!("{:02x}", to_component(n));
        return format!("#{}{}{}", to_hex(r), to_hex(g), to_hex(b));
    }

    let gray = 8 + (index - 232) * 10;
    let gray_hex = format!("{gray:02x}");
    format!("#{gray_hex}{gray_hex}{gray_hex}")
}

/// Escape HTML special characters.
fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#039;"),
            other => out.push(other),
        }
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq)]
struct TextStyle {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

fn style_to_inline_css(style: &TextStyle) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(fg) = &style.fg {
        parts.push(format!("color:{fg}"));
    }
    if let Some(bg) = &style.bg {
        parts.push(format!("background-color:{bg}"));
    }
    if style.bold {
        parts.push("font-weight:bold".to_string());
    }
    if style.dim {
        parts.push("opacity:0.6".to_string());
    }
    if style.italic {
        parts.push("font-style:italic".to_string());
    }
    if style.underline {
        parts.push("text-decoration:underline".to_string());
    }
    parts.join(";")
}

fn has_style(style: &TextStyle) -> bool {
    style.fg.is_some() || style.bg.is_some() || style.bold || style.dim || style.italic || style.underline
}

/// Parse ANSI SGR (Select Graphic Rendition) codes and update style.
fn apply_sgr_code(params: &[i64], style: &mut TextStyle) {
    let mut i = 0usize;
    while i < params.len() {
        let code = params[i];

        if code == 0 {
            style.fg = None;
            style.bg = None;
            style.bold = false;
            style.dim = false;
            style.italic = false;
            style.underline = false;
        } else if code == 1 {
            style.bold = true;
        } else if code == 2 {
            style.dim = true;
        } else if code == 3 {
            style.italic = true;
        } else if code == 4 {
            style.underline = true;
        } else if code == 22 {
            style.bold = false;
            style.dim = false;
        } else if code == 23 {
            style.italic = false;
        } else if code == 24 {
            style.underline = false;
        } else if (30..=37).contains(&code) {
            style.fg = Some(ANSI_COLORS[(code - 30) as usize].to_string());
        } else if code == 38 {
            if params.get(i + 1) == Some(&5) && params.len() > i + 2 {
                style.fg = Some(color256_to_hex(params[i + 2]));
                i += 2;
            } else if params.get(i + 1) == Some(&2) && params.len() > i + 4 {
                let r = params[i + 2];
                let g = params[i + 3];
                let b = params[i + 4];
                style.fg = Some(format!("rgb({r},{g},{b})"));
                i += 4;
            }
        } else if code == 39 {
            style.fg = None;
        } else if (40..=47).contains(&code) {
            style.bg = Some(ANSI_COLORS[(code - 40) as usize].to_string());
        } else if code == 48 {
            if params.get(i + 1) == Some(&5) && params.len() > i + 2 {
                style.bg = Some(color256_to_hex(params[i + 2]));
                i += 2;
            } else if params.get(i + 1) == Some(&2) && params.len() > i + 4 {
                let r = params[i + 2];
                let g = params[i + 3];
                let b = params[i + 4];
                style.bg = Some(format!("rgb({r},{g},{b})"));
                i += 4;
            }
        } else if code == 49 {
            style.bg = None;
        } else if (90..=97).contains(&code) {
            style.fg = Some(ANSI_COLORS[(code - 90 + 8) as usize].to_string());
        } else if (100..=107).contains(&code) {
            style.bg = Some(ANSI_COLORS[(code - 100 + 8) as usize].to_string());
        }

        i += 1;
    }
}

fn ansi_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new("\\x1b\\[([\\d;]*)m").expect("ansi regex"))
}

/// Convert ANSI-escaped text to HTML with inline styles.
pub fn ansi_to_html(text: &str) -> String {
    let mut style = TextStyle::default();
    let mut result = String::new();
    let mut last_index = 0usize;
    let mut in_span = false;

    let regex = ansi_regex();
    for captures in regex.captures_iter(text) {
        let matched = captures.get(0).expect("match");
        let before_text = &text[last_index..matched.start()];
        if !before_text.is_empty() {
            result.push_str(&escape_html(before_text));
        }

        let param_str = captures.get(1).map(|group| group.as_str()).unwrap_or("");
        let params: Vec<i64> = if param_str.is_empty() {
            vec![0]
        } else {
            param_str
                .split(';')
                .map(|part| part.parse::<i64>().unwrap_or(0))
                .collect()
        };

        if in_span {
            result.push_str("</span>");
            in_span = false;
        }

        apply_sgr_code(&params, &mut style);

        if has_style(&style) {
            result.push_str(&format!("<span style=\"{}\">", style_to_inline_css(&style)));
            in_span = true;
        }

        last_index = matched.end();
    }

    let remaining_text = &text[last_index..];
    if !remaining_text.is_empty() {
        result.push_str(&escape_html(remaining_text));
    }

    if in_span {
        result.push_str("</span>");
    }

    result
}

/// Convert array of ANSI-escaped lines to HTML.
/// Each line is wrapped in a div element.
pub fn ansi_lines_to_html(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| {
            let converted = ansi_to_html(line);
            let body = if converted.is_empty() { "&nbsp;".to_string() } else { converted };
            format!("<div class=\"ansi-line\">{body}</div>")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html_specials_and_keeps_plain_text() {
        assert_eq!(ansi_to_html("a<b>&\"c\"'"), "a&lt;b&gt;&amp;&quot;c&quot;&#039;");
    }

    #[test]
    fn maps_basic_and_bright_colors() {
        assert_eq!(ansi_to_html("\u{1b}[31mred\u{1b}[0m"), "<span style=\"color:#800000\">red</span>");
        assert_eq!(
            ansi_to_html("\u{1b}[91mred\u{1b}[39m"),
            "<span style=\"color:#ff0000\">red</span>"
        );
    }

    #[test]
    fn maps_256_color_and_truecolor() {
        assert_eq!(
            ansi_to_html("\u{1b}[38;5;196mx\u{1b}[0m"),
            "<span style=\"color:#ff0000\">x</span>"
        );
        assert_eq!(
            ansi_to_html("\u{1b}[38;2;1;2;3mx\u{1b}[0m"),
            "<span style=\"color:rgb(1,2,3)\">x</span>"
        );
    }

    #[test]
    fn empty_params_mean_reset() {
        assert_eq!(ansi_to_html("\u{1b}[31ma\u{1b}[mb"), "<span style=\"color:#800000\">a</span>b");
    }

    #[test]
    fn styles_compose_in_declaration_order() {
        assert_eq!(
            ansi_to_html("\u{1b}[1;3;4mx\u{1b}[0m"),
            "<span style=\"font-weight:bold;font-style:italic;text-decoration:underline\">x</span>"
        );
    }

    #[test]
    fn background_and_dim_and_resets() {
        assert_eq!(
            ansi_to_html("\u{1b}[44;2mx\u{1b}[22;49m"),
            "<span style=\"background-color:#000080;opacity:0.6\">x</span>"
        );
    }

    #[test]
    fn ansi_lines_wrap_each_line_in_a_div() {
        assert_eq!(
            ansi_lines_to_html(&["a".to_string(), "".to_string()]),
            "<div class=\"ansi-line\">a</div><div class=\"ansi-line\">&nbsp;</div>"
        );
    }
}
