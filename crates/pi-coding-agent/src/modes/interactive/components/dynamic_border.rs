//! Port of packages/coding-agent/src/modes/interactive/components/dynamic-border.ts

use pi_tui::tui::Component;

use crate::modes::interactive::theme::theme::theme;

/// `interface ColorFn` - `(str: string) => string`.
pub type ColorFn = Box<dyn Fn(&str) -> String + Send + Sync>;

/// Dynamic border component that adjusts to viewport width.
///
/// Note: When used from extensions loaded via jiti, the global `theme` may be undefined
/// because jiti creates a separate module cache. Always pass an explicit color
/// function when using DynamicBorder in components exported for extension use.
pub struct DynamicBorder {
    color: ColorFn,
}

impl DynamicBorder {
    pub fn new(color: ColorFn) -> Self {
        Self { color }
    }
}

impl Default for DynamicBorder {
    fn default() -> Self {
        Self::new(Box::new(|text: &str| theme().fg("border", text)))
    }
}

impl Component for DynamicBorder {
    fn invalidate(&mut self) {
        // No cached state to invalidate currently
    }

    fn render(&mut self, width: f64) -> Vec<String> {
        let count = (width.max(1.0)) as usize;
        vec![(self.color)("\u{2500}".repeat(count))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_tui::utils::strip_ansi;

    #[test]
    fn renders_a_single_border_line_of_at_least_one_cell() {
        let mut border = DynamicBorder::new(Box::new(|text: &str| text.to_string()));
        assert_eq!(border.render(3.0), vec!["\u{2500}\u{2500}\u{2500}"]);
        assert_eq!(border.render(0.0), vec!["\u{2500}"]);
    }

    #[test]
    fn default_color_fn_uses_the_border_theme_color() {
        let mut border = DynamicBorder::default();
        let lines = border.render(2.0);
        assert_eq!(strip_ansi(&lines[0]), "\u{2500}\u{2500}");
    }
}
