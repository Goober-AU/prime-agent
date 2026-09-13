//! Port of packages/coding-agent/src/modes/interactive/components/visual-truncate.ts
//!
//! Shared utility for truncating text to visual lines (accounting for line
//! wrapping). Used by tool-execution and bash-execution for consistent behavior.

use pi_tui::components::text::Text;
use pi_tui::tui::Component;

/// Port of `VisualTruncateResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VisualTruncateResult {
    /// The visual lines to display
    pub visual_lines: Vec<String>,
    /// Number of visual lines that were skipped (hidden)
    pub skipped_count: usize,
}

/// Port of `truncateToVisualLines`.
///
/// `padding_x` is the TypeScript default parameter (`paddingX = 0`): use 0 when
/// the result is placed in a `Box` (the `Box` adds its own padding), 1 when it is
/// placed in a plain `Container`.
pub fn truncate_to_visual_lines(
    text: &str,
    max_visual_lines: usize,
    width: f64,
    padding_x: usize,
) -> VisualTruncateResult {
    if text.is_empty() {
        return VisualTruncateResult {
            visual_lines: Vec::new(),
            skipped_count: 0,
        };
    }

    // Create a temporary Text component to render and get visual lines
    let mut temp_text = Text::new(text.to_string(), padding_x, 0, None);
    let all_visual_lines = temp_text.render(width);

    if all_visual_lines.len() <= max_visual_lines {
        return VisualTruncateResult {
            visual_lines: all_visual_lines,
            skipped_count: 0,
        };
    }

    // Take the last N visual lines
    let start = all_visual_lines.len() - max_visual_lines;
    let truncated_lines = all_visual_lines[start..].to_vec();
    let skipped_count = all_visual_lines.len() - max_visual_lines;

    VisualTruncateResult {
        visual_lines: truncated_lines,
        skipped_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_returns_nothing() {
        let result = truncate_to_visual_lines("", 5, 20.0, 0);
        assert_eq!(result.visual_lines, Vec::<String>::new());
        assert_eq!(result.skipped_count, 0);
    }

    #[test]
    fn short_text_is_returned_whole() {
        let result = truncate_to_visual_lines("hello", 5, 20.0, 0);
        assert_eq!(
            result.visual_lines,
            vec!["hello               ".to_string()]
        );
        assert_eq!(result.skipped_count, 0);
    }

    #[test]
    fn takes_the_last_visual_lines_and_counts_the_skipped_ones() {
        let text = "one\ntwo\nthree\nfour";
        let result = truncate_to_visual_lines(text, 2, 10.0, 0);
        assert_eq!(
            result.visual_lines,
            vec!["three     ".to_string(), "four      ".to_string()]
        );
        assert_eq!(result.skipped_count, 2);
    }

    #[test]
    fn padding_is_applied_to_each_visual_line() {
        let result = truncate_to_visual_lines("hi", 5, 6.0, 1);
        assert_eq!(result.visual_lines, vec![" hi   ".to_string()]);
    }
}
