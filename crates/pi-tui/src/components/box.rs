//! Port of packages/tui/src/components/box.ts

use crate::selection_metadata::TableCellSelectionRegion;
use crate::tui::Component;
use crate::utils::{apply_background_to_line, visible_width};

/// Port of the `RenderCache` type.
struct RenderCache {
    child_lines: Vec<String>,
    width: usize,
    bg_sample: Option<String>,
    lines: Vec<String>,
    selection_regions: Vec<TableCellSelectionRegion>,
}

/// Box component - a container that applies padding and background to all children
pub struct Box_ {
    pub children: Vec<Box<dyn Component>>,
    padding_x: usize,
    padding_y: usize,
    bg_fn: Option<Box<dyn Fn(&str) -> String>>,

    cache: Option<RenderCache>,
}

impl Box_ {
    pub fn new(padding_x: usize, padding_y: usize, bg_fn: Option<Box<dyn Fn(&str) -> String>>) -> Self {
        Self {
            children: Vec::new(),
            padding_x,
            padding_y,
            bg_fn,
            cache: None,
        }
    }

    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.children.push(component);
        self.invalidate_cache();
    }

    /// TypeScript removes a child by identity; Rust components are not comparable,
    /// so the port removes by index.
    pub fn remove_child_at(&mut self, index: usize) {
        if index < self.children.len() {
            self.children.remove(index);
            self.invalidate_cache();
        }
    }

    pub fn clear(&mut self) {
        self.children = Vec::new();
        self.invalidate_cache();
    }

    pub fn set_bg_fn(&mut self, bg_fn: Option<Box<dyn Fn(&str) -> String>>) {
        self.bg_fn = bg_fn;
        // Don't invalidate here - we'll detect bgFn changes by sampling output
    }

    fn invalidate_cache(&mut self) {
        self.cache = None;
    }

    fn match_cache(&self, width: usize, child_lines: &[String], bg_sample: Option<&str>) -> bool {
        match &self.cache {
            Some(cache) => {
                cache.width == width
                    && cache.bg_sample.as_deref() == bg_sample
                    && cache.child_lines.len() == child_lines.len()
                    && cache
                        .child_lines
                        .iter()
                        .zip(child_lines.iter())
                        .all(|(a, b)| a == b)
            }
            None => false,
        }
    }

    fn apply_bg(&self, line: &str, width: usize) -> String {
        let vis_len = visible_width(line);
        let pad_needed = width.saturating_sub(vis_len);
        let padded = format!("{line}{}", " ".repeat(pad_needed));

        match &self.bg_fn {
            Some(bg_fn) => apply_background_to_line(&padded, width, bg_fn.as_ref()),
            None => padded,
        }
    }
}

impl Default for Box_ {
    fn default() -> Self {
        Self::new(1, 1, None)
    }
}

impl Component for Box_ {
    fn render(&mut self, width: f64) -> Vec<String> {
        let width = width.max(0.0).floor() as usize;
        if self.children.is_empty() {
            self.cache = None;
            return Vec::new();
        }

        let content_width = width.saturating_sub(self.padding_x * 2).max(1);
        let left_pad = " ".repeat(self.padding_x);

        let mut child_lines: Vec<String> = Vec::new();
        let mut selection_regions: Vec<TableCellSelectionRegion> = Vec::new();
        for child in self.children.iter_mut() {
            let line_offset = child_lines.len();
            let lines = child.render(content_width as f64);
            for region in child.get_selection_regions() {
                selection_regions.push(TableCellSelectionRegion {
                    line: region.line + line_offset + self.padding_y,
                    col: region.col + self.padding_x,
                    table_top: region.table_top + line_offset + self.padding_y,
                    table_bottom: region.table_bottom + line_offset + self.padding_y,
                    table_left: region.table_left + self.padding_x,
                    table_right: region.table_right + self.padding_x,
                    ..region
                });
            }
            for line in lines {
                child_lines.push(format!("{left_pad}{line}"));
            }
        }

        if child_lines.is_empty() {
            self.cache = None;
            return Vec::new();
        }

        let bg_sample = self.bg_fn.as_ref().map(|bg_fn| bg_fn("test"));

        if self.match_cache(width, &child_lines, bg_sample.as_deref()) {
            if let Some(cache) = self.cache.as_mut() {
                cache.selection_regions = selection_regions;
                return cache.lines.clone();
            }
        }

        let mut result: Vec<String> = Vec::new();

        for _ in 0..self.padding_y {
            result.push(self.apply_bg("", width));
        }

        for line in &child_lines {
            result.push(self.apply_bg(line, width));
        }

        for _ in 0..self.padding_y {
            result.push(self.apply_bg("", width));
        }

        self.cache = Some(RenderCache {
            child_lines,
            width,
            bg_sample,
            lines: result.clone(),
            selection_regions,
        });

        result
    }

    fn get_selection_regions(&self) -> Vec<TableCellSelectionRegion> {
        match &self.cache {
            Some(cache) => cache.selection_regions.clone(),
            None => Vec::new(),
        }
    }

    fn invalidate(&mut self) {
        self.invalidate_cache();
        for child in self.children.iter_mut() {
            child.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lines(Vec<String>);

    impl Component for Lines {
        fn render(&mut self, _width: f64) -> Vec<String> {
            self.0.clone()
        }
        fn invalidate(&mut self) {}
    }

    #[test]
    fn empty_box_renders_nothing() {
        let mut box_ = Box_::new(1, 1, None);
        assert_eq!(box_.render(10.0), Vec::<String>::new());
    }

    #[test]
    fn pads_children_and_vertical_padding() {
        let mut box_ = Box_::new(1, 1, None);
        box_.add_child(Box::new(Lines(vec!["hi".to_string()])));
        assert_eq!(
            box_.render(6.0),
            vec![
                "      ".to_string(),
                " hi   ".to_string(),
                "      ".to_string()
            ]
        );
    }

    #[test]
    fn background_sampling_invalidates_cache() {
        let mut box_ = Box_::new(0, 0, None);
        box_.add_child(Box::new(Lines(vec!["hi".to_string()])));
        assert_eq!(box_.render(4.0), vec!["hi  ".to_string()]);
        box_.set_bg_fn(Some(Box::new(|text: &str| format!("<{text}>"))));
        assert_eq!(box_.render(4.0), vec!["<hi  >".to_string()]);
    }

    #[test]
    fn remove_child_and_clear() {
        let mut box_ = Box_::new(0, 0, None);
        box_.add_child(Box::new(Lines(vec!["a".to_string()])));
        box_.add_child(Box::new(Lines(vec!["b".to_string()])));
        box_.remove_child_at(0);
        assert_eq!(box_.render(1.0), vec!["b".to_string()]);
        box_.clear();
        assert_eq!(box_.render(1.0), Vec::<String>::new());
    }
}
