//! Port of packages/tui/src/components/text.ts

use crate::tui::Component;
use crate::utils::{apply_background_to_line, visible_width, wrap_text_with_ansi};

/// Text component - displays multi-line text with word wrapping
pub struct Text {
    text: String,
    padding_x: usize,
    padding_y: usize,
    custom_bg_fn: Option<Box<dyn Fn(&str) -> String + Send + Sync>>,

    cached_text: Option<String>,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl Text {
    pub fn new(
        text: String,
        padding_x: usize,
        padding_y: usize,
        custom_bg_fn: Option<Box<dyn Fn(&str) -> String + Send + Sync>>,
    ) -> Self {
        Self {
            text,
            padding_x,
            padding_y,
            custom_bg_fn,
            cached_text: None,
            cached_width: None,
            cached_lines: None,
        }
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }

    pub fn set_custom_bg_fn(&mut self, custom_bg_fn: Option<Box<dyn Fn(&str) -> String + Send + Sync>>) {
        self.custom_bg_fn = custom_bg_fn;
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Default for Text {
    fn default() -> Self {
        Self::new(String::new(), 1, 1, None)
    }
}

impl Component for Text {
    fn render(&mut self, width: usize) -> Vec<String> {
        if let (Some(lines), Some(cached_text), Some(cached_width)) =
            (&self.cached_lines, &self.cached_text, self.cached_width)
        {
            if *cached_text == self.text && cached_width == width {
                return lines.clone();
            }
        }

        if self.text.is_empty() || self.text.trim().is_empty() {
            let result: Vec<String> = Vec::new();
            self.cached_text = Some(self.text.clone());
            self.cached_width = Some(width);
            self.cached_lines = Some(result.clone());
            return result;
        }

        let normalized_text = self.text.replace('\t', "   ");

        let content_width = width.saturating_sub(self.padding_x * 2).max(1);

        let wrapped_lines = wrap_text_with_ansi(&normalized_text, content_width);

        let left_margin = " ".repeat(self.padding_x);
        let right_margin = " ".repeat(self.padding_x);
        let mut content_lines: Vec<String> = Vec::new();

        for line in wrapped_lines {
            let line_with_margins = format!("{left_margin}{line}{right_margin}");

            match &self.custom_bg_fn {
                Some(bg_fn) => content_lines.push(apply_background_to_line(
                    &line_with_margins,
                    width,
                    bg_fn.as_ref(),
                )),
                None => {
                    let visible_len = visible_width(&line_with_margins);
                    let padding_needed = width.saturating_sub(visible_len);
                    content_lines.push(format!("{line_with_margins}{}", " ".repeat(padding_needed)));
                }
            }
        }

        let empty_line = " ".repeat(width);
        let mut empty_lines: Vec<String> = Vec::new();
        for _ in 0..self.padding_y {
            let line = match &self.custom_bg_fn {
                Some(bg_fn) => apply_background_to_line(&empty_line, width, bg_fn.as_ref()),
                None => empty_line.clone(),
            };
            empty_lines.push(line);
        }

        let mut result: Vec<String> = Vec::new();
        result.extend(empty_lines.iter().cloned());
        result.extend(content_lines);
        result.extend(empty_lines);

        self.cached_text = Some(self.text.clone());
        self.cached_width = Some(width);
        self.cached_lines = Some(result.clone());

        if !result.is_empty() {
            result
        } else {
            vec![String::new()]
        }
    }

    fn invalidate(&mut self) {
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_renders_nothing() {
        let mut text = Text::new(String::new(), 1, 1, None);
        assert_eq!(text.render(10), Vec::<String>::new());
        let mut whitespace = Text::new("   ".to_string(), 1, 1, None);
        assert_eq!(whitespace.render(10), Vec::<String>::new());
    }

    #[test]
    fn renders_padding_and_pads_to_width() {
        let mut text = Text::new("hello".to_string(), 1, 1, None);
        let lines = text.render(9);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], " ".repeat(9));
        assert_eq!(lines[1], " hello   ");
        assert_eq!(lines[2], " ".repeat(9));
    }

    #[test]
    fn wraps_long_lines() {
        let mut text = Text::new("hello world".to_string(), 0, 0, None);
        let lines = text.render(6);
        assert_eq!(lines, vec!["hello ".to_string(), "world ".to_string()]);
    }

    #[test]
    fn cache_invalidated_by_set_text() {
        let mut text = Text::new("a".to_string(), 0, 0, None);
        let first = text.render(4);
        assert_eq!(first, vec!["a   ".to_string()]);
        text.set_text("bb".to_string());
        assert_eq!(text.render(4), vec!["bb  ".to_string()]);
    }
}
