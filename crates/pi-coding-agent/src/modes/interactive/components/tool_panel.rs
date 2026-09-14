//! Port of packages/coding-agent/src/modes/interactive/components/tool-panel.ts

use pi_tui::tui::Component;
use pi_tui::utils::{truncate_to_width, visible_width};

use crate::modes::interactive::theme::theme::theme;

/// `TOOL_PANEL_PADDING_X`
pub const TOOL_PANEL_PADDING_X: usize = 2;

/// Port of `toolPanelContentWidth`.
pub fn tool_panel_content_width(width: f64) -> f64 {
    (width - (TOOL_PANEL_PADDING_X * 2) as f64).max(1.0)
}

/// Format a single tool panel line: content indented by the panel padding and
/// padded to the full width, on the subtle panel background that groups the
/// block.
pub fn tool_panel_line(line: &str, width: f64) -> String {
    let content_width = tool_panel_content_width(width);
    let truncated = truncate_to_width(line, content_width, "", false);
    let padding = " ".repeat(
        (content_width - visible_width(&truncated) as f64)
            .max(0.0)
            .floor() as usize,
    );
    let side_pad = " ".repeat(TOOL_PANEL_PADDING_X);
    theme().bg(
        "toolPanelBg",
        &format!("{side_pad}{truncated}{padding}{side_pad}"),
    )
}

struct ToolPanelCache {
    width: f64,
    header: String,
    bg_sample: String,
    child_lines: Vec<String>,
    lines: Vec<String>,
}

/// Panel shell for tool executions: a status header line followed by the
/// tool's own call/result components, every line on the panel background so
/// the whole block reads as one unit. Children render at the reduced content
/// width; lines that still overflow are truncated, matching ipython cell
/// behavior.
pub struct ToolPanel {
    children: Vec<Box<dyn Component>>,
    header: String,
    cache: Option<ToolPanelCache>,
}

impl ToolPanel {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            header: String::new(),
            cache: None,
        }
    }

    pub fn set_header(&mut self, header: impl Into<String>) {
        self.header = header.into();
    }

    pub fn header(&self) -> &str {
        &self.header
    }

    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.children.push(component);
        self.cache = None;
    }

    pub fn clear(&mut self) {
        self.children = Vec::new();
        self.cache = None;
    }

    pub fn render_impl(&mut self, width: f64) -> Vec<String> {
        let mut child_lines: Vec<String> = Vec::new();
        for child in self.children.iter_mut() {
            child_lines.extend(child.render(tool_panel_content_width(width)));
        }

        // The background sample detects theme changes that don't go through
        // invalidate().
        let bg_sample = theme().bg("toolPanelBg", " ");
        if let Some(cache) = &self.cache {
            if cache.width == width
                && cache.header == self.header
                && cache.bg_sample == bg_sample
                && cache.child_lines.len() == child_lines.len()
                && cache
                    .child_lines
                    .iter()
                    .zip(child_lines.iter())
                    .all(|(line, other)| line == other)
            {
                return cache.lines.clone();
            }
        }

        let mut lines: Vec<String> = vec![tool_panel_line(&self.header, width)];
        if !child_lines.is_empty() {
            lines.push(tool_panel_line("", width));
            for line in &child_lines {
                lines.push(tool_panel_line(line, width));
            }
        }
        self.cache = Some(ToolPanelCache {
            width,
            header: self.header.clone(),
            bg_sample,
            child_lines,
            lines: lines.clone(),
        });
        lines
    }
}

impl Default for ToolPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for ToolPanel {
    fn invalidate(&mut self) {
        self.cache = None;
        for child in self.children.iter_mut() {
            child.invalidate();
        }
    }

    fn render(&mut self, width: f64) -> Vec<String> {
        self.render_impl(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_tui::components::text::Text;
    use pi_tui::utils::strip_ansi;

    #[test]
    fn content_width_keeps_at_least_one_column() {
        assert_eq!(tool_panel_content_width(20.0), 16.0);
        assert_eq!(tool_panel_content_width(2.0), 1.0);
    }

    #[test]
    fn panel_line_pads_to_full_width_with_panel_padding() {
        let line = tool_panel_line("hi", 6.0);
        assert_eq!(strip_ansi(&line), "  hi  ");
    }

    #[test]
    fn renders_header_then_blank_then_child_lines() {
        let mut panel = ToolPanel::new();
        panel.set_header("header");
        panel.add_child(Box::new(Text::new("child".to_string(), 0, 0, None)));
        let lines = panel.render_impl(9.0);
        assert_eq!(lines.len(), 3);
        assert_eq!(strip_ansi(&lines[0]), "  heade  ");
        assert_eq!(strip_ansi(&lines[1]), " ".repeat(9));
        assert_eq!(strip_ansi(&lines[2]), "  child  ");
    }

    #[test]
    fn empty_panel_renders_only_the_header_line() {
        let mut panel = ToolPanel::new();
        panel.set_header("only");
        assert_eq!(panel.render_impl(6.0).len(), 1);
    }
}
