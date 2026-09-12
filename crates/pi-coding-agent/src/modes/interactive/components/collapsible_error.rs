//! Port of packages/coding-agent/src/modes/interactive/components/collapsible-error.ts

use pi_tui::tui::Component;
use pi_tui::utils::{strip_ansi, truncate_to_width, visible_width, wrap_text_with_ansi};

use crate::modes::interactive::theme::theme::theme;

use super::keybinding_hints::expand_collapse_hint;

/// `CollapsibleErrorOptions`
#[derive(Debug, Clone, Default)]
pub struct CollapsibleErrorOptions {
    pub text: String,
    pub summary: Option<String>,
    pub expanded: Option<bool>,
    pub force_collapse: Option<bool>,
    pub padding_x: Option<usize>,
}

/// Port of `normalizeErrorDetails`.
pub fn normalize_error_details(text: &str) -> String {
    strip_ansi(text)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_end()
        .to_string()
}

struct ErrorDetailLine {
    raw: String,
    trimmed: String,
}

fn error_detail_lines(text: &str) -> Vec<ErrorDetailLine> {
    normalize_error_details(text)
        .split('\n')
        .map(|raw| ErrorDetailLine {
            raw: raw.to_string(),
            trimmed: raw.trim().to_string(),
        })
        .filter(|line| !line.trimmed.is_empty())
        .collect()
}

fn starts_stack_context(line: &ErrorDetailLine) -> bool {
    if line.trimmed.starts_with("Traceback ") {
        return true;
    }
    if line.trimmed.starts_with("File ") && line.trimmed.contains(", line ") {
        return true;
    }
    if line.trimmed.starts_with("Cell In[") && line.trimmed.contains(", line ") {
        return true;
    }
    if line.trimmed.starts_with("---->") {
        return true;
    }
    false
}

fn is_stack_context_line(line: &ErrorDetailLine) -> bool {
    if starts_stack_context(line) {
        return true;
    }
    line.raw.starts_with(' ') || line.raw.starts_with('\t')
}

fn summarize_stack_context(lines: &[ErrorDetailLine]) -> Option<String> {
    let mut index = lines.len();
    while index > 0 {
        index -= 1;
        let line = &lines[index];
        if !is_stack_context_line(line) {
            return Some(line.trimmed.clone());
        }
    }
    None
}

/// Port of `summarizeErrorDetails`.
pub fn summarize_error_details(text: &str) -> String {
    let lines = error_detail_lines(text);
    if lines.is_empty() {
        return "Error".to_string();
    }
    if lines.len() > 1 && starts_stack_context(&lines[0]) {
        return summarize_stack_context(&lines).unwrap_or_else(|| "Error".to_string());
    }
    lines[0].trimmed.clone()
}

/// Port of `shouldCollapseErrorDetails`.
pub fn should_collapse_error_details(text: &str) -> bool {
    normalize_error_details(text).split('\n').count() > 1
}

pub struct CollapsibleErrorComponent {
    options: CollapsibleErrorOptions,
    expanded: bool,
}

impl CollapsibleErrorComponent {
    pub fn new(options: CollapsibleErrorOptions) -> Self {
        Self {
            expanded: options.expanded.unwrap_or(false),
            options,
        }
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    fn render_text(&self, text: &str, width: f64, color: &str) -> Vec<String> {
        let safe_width = width.max(1.0);
        let padding_x = self.options.padding_x.unwrap_or(1);
        let content_width = (safe_width - padding_x as f64).max(1.0);
        let prefix = " ".repeat(padding_x);
        let mut lines: Vec<String> = Vec::new();
        for raw_line in text.split('\n') {
            let styled = theme().fg(color, if raw_line.is_empty() { " " } else { raw_line });
            let wrapped = wrap_text_with_ansi(&styled, content_width as usize);
            for line in if wrapped.is_empty() {
                vec![String::new()]
            } else {
                wrapped
            } {
                let padded = format!("{prefix}{line}");
                lines.push(truncate_to_width(&padded, safe_width, "", false));
            }
        }
        lines
            .into_iter()
            .map(|line| {
                let pad = (safe_width - visible_width(&line) as f64).max(0.0).floor() as usize;
                format!("{line}{}", " ".repeat(pad))
            })
            .collect()
    }
}

impl Component for CollapsibleErrorComponent {
    fn invalidate(&mut self) {
        // Render output is derived from constructor options and expansion state.
    }

    fn render(&mut self, width: f64) -> Vec<String> {
        let text = normalize_error_details(&self.options.text);
        if text.is_empty() {
            return Vec::new();
        }

        let collapsible = self
            .options
            .force_collapse
            .unwrap_or_else(|| should_collapse_error_details(&text));
        if !collapsible || self.expanded {
            return self.render_text(&text, width, "error");
        }

        let summary = normalize_error_details(
            self.options
                .summary
                .clone()
                .unwrap_or_else(|| summarize_error_details(&text))
                .as_str(),
        );
        let inline_hint = format!(
            "{summary} {}",
            expand_collapse_hint("app.tools.expand", false)
        );
        self.render_text(&inline_hint, width, "error")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_ansi_and_normalizes_newlines() {
        assert_eq!(
            normalize_error_details("\u{1b}[31mboom\u{1b}[0m\r\nnext\r\n"),
            "boom\nnext"
        );
    }

    #[test]
    fn summarize_returns_last_non_stack_line_for_tracebacks() {
        let text = "Traceback (most recent call last):\n  File \"a.py\", line 1\nValueError: bad";
        assert_eq!(summarize_error_details(text), "ValueError: bad");
    }

    #[test]
    fn summarize_returns_first_line_for_plain_errors() {
        assert_eq!(summarize_error_details("first\nsecond"), "first");
        assert_eq!(summarize_error_details("\n\n"), "Error");
    }

    #[test]
    fn collapse_only_when_more_than_one_line() {
        assert!(!should_collapse_error_details("one"));
        assert!(should_collapse_error_details("one\ntwo"));
    }

    #[test]
    fn collapsed_render_uses_the_summary_and_hint() {
        let mut component = CollapsibleErrorComponent::new(CollapsibleErrorOptions {
            text: "first\nsecond".to_string(),
            ..Default::default()
        });
        let lines = component.render(40.0);
        assert_eq!(lines.len(), 1);
        assert!(strip_ansi(&lines[0]).starts_with(" first "));
        assert!(strip_ansi(&lines[0]).contains("to expand"));
    }

    #[test]
    fn empty_text_renders_nothing() {
        let mut component = CollapsibleErrorComponent::new(CollapsibleErrorOptions::default());
        assert_eq!(component.render(10.0), Vec::<String>::new());
    }
}
