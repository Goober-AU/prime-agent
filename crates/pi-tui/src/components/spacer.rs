//! Port of packages/tui/src/components/spacer.ts

use crate::tui::Component;

/// Spacer component that renders empty lines
pub struct Spacer {
    lines: usize,
}

impl Spacer {
    pub fn new(lines: usize) -> Self {
        Self { lines }
    }

    pub fn set_lines(&mut self, lines: usize) {
        self.lines = lines;
    }
}

impl Default for Spacer {
    fn default() -> Self {
        Self::new(1)
    }
}

impl Component for Spacer {
    fn render(&mut self, _width: f64) -> Vec<String> {
        let mut result: Vec<String> = Vec::new();
        for _ in 0..self.lines {
            result.push(String::new());
        }
        result
    }

    fn invalidate(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_requested_number_of_empty_lines() {
        let mut spacer = Spacer::new(3);
        assert_eq!(spacer.render(10.0), vec!["", "", ""]);
        spacer.set_lines(0);
        assert_eq!(spacer.render(10.0), Vec::<String>::new());
        assert_eq!(Spacer::default().render(1.0), vec![""]);
    }
}
