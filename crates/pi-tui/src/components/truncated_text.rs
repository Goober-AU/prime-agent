//! Port of packages/tui/src/components/truncated-text.ts

use crate::tui::Component;
use crate::utils::{truncate_to_width, visible_width};

/// Text component that truncates to fit viewport width
pub struct TruncatedText {
    text: String,
    padding_x: usize,
    padding_y: usize,
}

impl TruncatedText {
    pub fn new(text: String, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text,
            padding_x,
            padding_y,
        }
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Component for TruncatedText {
    fn render(&mut self, width: f64) -> Vec<String> {
        let width = width.max(0.0).floor() as usize;
        let mut result: Vec<String> = Vec::new();

        let empty_line = " ".repeat(width);

        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        let available_width = width.saturating_sub(self.padding_x * 2).max(1);

        let mut single_line_text = self.text.clone();
        if let Some(newline_index) = self.text.find('\n') {
            single_line_text = self.text[..newline_index].to_string();
        }

        let display_text = truncate_to_width(&single_line_text, available_width as f64, "...", false);

        let left_padding = " ".repeat(self.padding_x);
        let right_padding = " ".repeat(self.padding_x);
        let line_with_padding = format!("{left_padding}{display_text}{right_padding}");

        let line_visible_width = visible_width(&line_with_padding);
        let padding_needed = width.saturating_sub(line_visible_width);
        let final_line = format!("{line_with_padding}{}", " ".repeat(padding_needed));

        result.push(final_line);

        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        result
    }

    fn invalidate(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_renders_first_line() {
        let mut text = TruncatedText::new("first\nsecond".to_string(), 0, 0);
        assert_eq!(text.render(10.0), vec!["first     ".to_string()]);
    }

    #[test]
    fn applies_vertical_and_horizontal_padding() {
        let mut text = TruncatedText::new("hi".to_string(), 1, 1);
        assert_eq!(
            text.render(6.0),
            vec![
                "      ".to_string(),
                " hi   ".to_string(),
                "      ".to_string()
            ]
        );
    }
}
