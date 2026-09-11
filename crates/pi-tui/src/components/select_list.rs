//! Port of packages/tui/src/components/select-list.ts

use crate::keybindings::get_keybindings;
use crate::tui::Component;
use crate::utils::{truncate_to_width, visible_width, wrap_text_with_ansi};

pub const DEFAULT_PRIMARY_COLUMN_WIDTH: usize = 32;
pub const PRIMARY_COLUMN_GAP: usize = 2;
pub const MIN_DESCRIPTION_WIDTH: usize = 10;

fn normalize_to_single_line(text: &str) -> String {
    // `/[\r\n]+/g` -> " "
    let mut result = String::new();
    let mut last_was_newline = false;
    for ch in text.chars() {
        if ch == '\r' || ch == '\n' {
            if !last_was_newline {
                result.push(' ');
            }
            last_was_newline = true;
        } else {
            last_was_newline = false;
            result.push(ch);
        }
    }
    result.trim().to_string()
}

fn clamp(value: i64, min: i64, max: i64) -> i64 {
    min.max(value.min(max))
}

/// Port of `SelectItem`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelectItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub source_tag: Option<String>,
    pub takes_argument: Option<bool>,
}

/// Port of `SelectListTheme`.
pub struct SelectListTheme {
    pub selected_prefix: Box<dyn Fn(&str) -> String>,
    pub selected_text: Box<dyn Fn(&str) -> String>,
    pub description: Box<dyn Fn(&str) -> String>,
    pub argument_hint: Option<Box<dyn Fn(&str) -> String>>,
    pub source_tag: Option<Box<dyn Fn(&str) -> String>>,
    pub scroll_info: Box<dyn Fn(&str) -> String>,
    pub no_match: Box<dyn Fn(&str) -> String>,
}

impl SelectListTheme {
    fn argument_hint(&self, text: &str) -> String {
        match &self.argument_hint {
            Some(f) => f(text),
            None => (self.description)(text),
        }
    }

    fn source_tag(&self, text: &str) -> String {
        match &self.source_tag {
            Some(f) => f(text),
            None => (self.description)(text),
        }
    }
}

/// Port of `SelectListTruncatePrimaryContext`.
#[derive(Clone)]
pub struct SelectListTruncatePrimaryContext {
    pub text: String,
    pub max_width: usize,
    pub column_width: usize,
    pub item: SelectItem,
    pub is_selected: bool,
}

/// Port of `SelectListLayoutOptions`.
#[derive(Default)]
pub struct SelectListLayoutOptions {
    pub min_primary_column_width: Option<usize>,
    pub max_primary_column_width: Option<usize>,
    pub truncate_primary: Option<Box<dyn Fn(&SelectListTruncatePrimaryContext) -> String>>,
    pub show_item_metadata: bool,
    pub show_directional_scroll_info: bool,
    pub show_selected_description: bool,
}

pub struct SelectList {
    items: Vec<SelectItem>,
    filtered_items: Vec<SelectItem>,
    selected_index: usize,
    max_visible: usize,
    theme: SelectListTheme,
    layout: SelectListLayoutOptions,

    pub on_select: Option<Box<dyn FnMut(&SelectItem)>>,
    pub on_cancel: Option<Box<dyn FnMut()>>,
    pub on_selection_change: Option<Box<dyn FnMut(&SelectItem)>>,
}

impl SelectList {
    pub fn new(
        items: Vec<SelectItem>,
        max_visible: usize,
        theme: SelectListTheme,
        layout: SelectListLayoutOptions,
    ) -> Self {
        Self {
            filtered_items: items.clone(),
            items,
            selected_index: 0,
            max_visible,
            theme,
            layout,
            on_select: None,
            on_cancel: None,
            on_selection_change: None,
        }
    }

    pub fn set_filter(&mut self, filter: &str) {
        let filter_lower = filter.to_lowercase();
        self.filtered_items = self
            .items
            .iter()
            .filter(|item| item.value.to_lowercase().starts_with(&filter_lower))
            .cloned()
            .collect();
        self.selected_index = 0;
    }

    pub fn set_selected_index(&mut self, index: usize) {
        let max = self.filtered_items.len().saturating_sub(1) as i64;
        self.selected_index = clamp(index as i64, 0, max).max(0) as usize;
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn get_selected_item(&self) -> Option<SelectItem> {
        self.filtered_items.get(self.selected_index).cloned()
    }

    pub fn filtered_items(&self) -> &[SelectItem] {
        &self.filtered_items
    }

    fn render_item(
        &self,
        item: &SelectItem,
        is_selected: bool,
        width: usize,
        description_single_line: Option<&str>,
        primary_column_width: usize,
    ) -> String {
        let prefix = if is_selected { "› " } else { "  " };
        let prefix_width = visible_width(prefix);

        if self.layout.show_item_metadata {
            return self.render_metadata_item(item, is_selected, width, primary_column_width, prefix, prefix_width);
        }

        if let Some(description) = description_single_line {
            if width > 40 {
                let effective_primary_column_width = primary_column_width
                    .min(width.saturating_sub(prefix_width).saturating_sub(4))
                    .max(1);
                let max_primary_width = effective_primary_column_width.saturating_sub(PRIMARY_COLUMN_GAP).max(1);
                let truncated_value = self.truncate_primary(item, is_selected, max_primary_width, effective_primary_column_width);
                let truncated_value_width = visible_width(&truncated_value);
                let spacing = " ".repeat(
                    effective_primary_column_width
                        .saturating_sub(truncated_value_width)
                        .max(1),
                );
                let description_start = prefix_width + truncated_value_width + spacing.len();
                let remaining_width = width.saturating_sub(description_start).saturating_sub(2); // -2 for safety

                if remaining_width > MIN_DESCRIPTION_WIDTH {
                    let truncated_desc = truncate_to_width(description, remaining_width as f64, "…", false);
                    if is_selected {
                        return (self.theme.selected_text)(&format!("{prefix}{truncated_value}{spacing}{truncated_desc}"));
                    }

                    let desc_text = (self.theme.description)(&format!("{spacing}{truncated_desc}"));
                    return format!("{prefix}{truncated_value}{desc_text}");
                }
            }
        }

        let max_width = width.saturating_sub(prefix_width).saturating_sub(2);
        let truncated_value = self.truncate_primary(item, is_selected, max_width, max_width);
        if is_selected {
            return (self.theme.selected_text)(&format!("{prefix}{truncated_value}"));
        }

        format!("{prefix}{truncated_value}")
    }

    fn format_directional_scroll_info(&self, hidden_above: usize, hidden_below: usize) -> String {
        let mut indicators: Vec<String> = Vec::new();
        if hidden_above > 0 {
            indicators.push(format!("↑ {hidden_above} more"));
        }
        if hidden_below > 0 {
            indicators.push(format!("↓ {hidden_below} more"));
        }
        format!("  {}", indicators.join("  "))
    }

    fn render_metadata_item(
        &self,
        item: &SelectItem,
        is_selected: bool,
        width: usize,
        primary_column_width: usize,
        prefix: &str,
        prefix_width: usize,
    ) -> String {
        let argument_hint = item.argument_hint.as_deref().map(normalize_to_single_line);
        let source_tag = item.source_tag.as_deref().map(normalize_to_single_line);
        let has_metadata = argument_hint.is_some() || source_tag.is_some();
        let content_width = width.saturating_sub(prefix_width).saturating_sub(2).max(1);
        let show_metadata = has_metadata && content_width > primary_column_width;
        let effective_primary_column_width = if show_metadata {
            primary_column_width
        } else {
            content_width
        };
        let max_primary_width = if show_metadata {
            effective_primary_column_width.saturating_sub(PRIMARY_COLUMN_GAP).max(1)
        } else {
            effective_primary_column_width
        };
        let primary = self.truncate_primary(item, is_selected, max_primary_width, effective_primary_column_width);
        let styled_prefix = if is_selected {
            (self.theme.selected_prefix)(prefix)
        } else {
            prefix.to_string()
        };
        let styled_primary = if is_selected {
            (self.theme.selected_text)(&primary)
        } else {
            primary.clone()
        };
        if !show_metadata {
            return truncate_to_width(&format!("{styled_prefix}{styled_primary}"), width as f64, "", false);
        }

        let spacing = " ".repeat(
            effective_primary_column_width
                .saturating_sub(visible_width(&primary))
                .max(1),
        );
        let mut remaining_width = width
            .saturating_sub(prefix_width)
            .saturating_sub(visible_width(&primary))
            .saturating_sub(spacing.len())
            .saturating_sub(2);
        let mut metadata: Vec<String> = Vec::new();
        if let Some(argument_hint) = &argument_hint {
            if remaining_width > 0 {
                let truncated_argument_hint = truncate_to_width(argument_hint, remaining_width as f64, "…", false);
                metadata.push(self.theme.argument_hint(&truncated_argument_hint));
                remaining_width = remaining_width.saturating_sub(visible_width(&truncated_argument_hint));
            }
        }
        if let Some(source_tag) = &source_tag {
            let threshold = if !metadata.is_empty() { PRIMARY_COLUMN_GAP } else { 0 };
            if remaining_width > threshold {
                if !metadata.is_empty() {
                    metadata.push(" ".repeat(PRIMARY_COLUMN_GAP));
                    remaining_width = remaining_width.saturating_sub(PRIMARY_COLUMN_GAP);
                }
                metadata.push(self.theme.source_tag(&truncate_to_width(source_tag, remaining_width as f64, "…", false)));
            }
        }
        truncate_to_width(
            &format!("{styled_prefix}{styled_primary}{spacing}{}", metadata.join("")),
            width as f64,
            "",
            false,
        )
    }

    fn get_primary_column_width(&self) -> usize {
        let (min, max) = self.get_primary_column_bounds();
        let widest_primary = self.filtered_items.iter().fold(0usize, |widest, item| {
            widest.max(visible_width(&self.get_display_value(item)) + PRIMARY_COLUMN_GAP)
        });

        clamp(widest_primary as i64, min as i64, max as i64) as usize
    }

    fn get_primary_column_bounds(&self) -> (usize, usize) {
        let raw_min = self
            .layout
            .min_primary_column_width
            .or(self.layout.max_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let raw_max = self
            .layout
            .max_primary_column_width
            .or(self.layout.min_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);

        (
            raw_min.min(raw_max).max(1),
            raw_min.max(raw_max).max(1),
        )
    }

    fn truncate_primary(&self, item: &SelectItem, is_selected: bool, max_width: usize, column_width: usize) -> String {
        let display_value = self.get_display_value(item);
        let truncated_value = match &self.layout.truncate_primary {
            Some(truncate_primary) => truncate_primary(&SelectListTruncatePrimaryContext {
                text: display_value.clone(),
                max_width,
                column_width,
                item: item.clone(),
                is_selected,
            }),
            None => truncate_to_width(&display_value, max_width as f64, "", false),
        };

        truncate_to_width(&truncated_value, max_width as f64, "", false)
    }

    fn get_display_value(&self, item: &SelectItem) -> String {
        if item.label.is_empty() {
            item.value.clone()
        } else {
            item.label.clone()
        }
    }

    fn render_selected_description(&self, lines: &mut Vec<String>, width: usize) {
        let description = self
            .filtered_items
            .get(self.selected_index)
            .and_then(|item| item.description.as_deref())
            .map(|description| description.trim().to_string());
        let description = match description {
            Some(description) if !description.is_empty() => description,
            _ => return,
        };

        let indent = if width >= 4 { "  " } else { "" };
        let content_width = width.saturating_sub(visible_width(indent)).saturating_sub(2).max(1);
        lines.push(String::new());
        for line in wrap_text_with_ansi(&description, content_width) {
            lines.push((self.theme.description)(&format!("{indent}{line}")));
        }
    }

    fn notify_selection_change(&mut self) {
        if let Some(item) = self.filtered_items.get(self.selected_index).cloned() {
            if let Some(callback) = self.on_selection_change.as_mut() {
                callback(&item);
            }
        }
    }
}

impl Component for SelectList {
    fn render(&mut self, width: f64) -> Vec<String> {
        let width = width.max(0.0).floor() as usize;
        let mut lines: Vec<String> = Vec::new();

        if self.filtered_items.is_empty() {
            lines.push((self.theme.no_match)("  No matching commands"));
            return lines;
        }

        let primary_column_width = self.get_primary_column_width();

        let half = self.max_visible / 2;
        let start_index = 0.max(
            (self.selected_index as i64 - half as i64)
                .min(self.filtered_items.len() as i64 - self.max_visible as i64),
        ) as usize;
        let end_index = (start_index + self.max_visible).min(self.filtered_items.len());

        for i in start_index..end_index {
            let item = match self.filtered_items.get(i) {
                Some(item) => item.clone(),
                None => continue,
            };

            let is_selected = i == self.selected_index;
            let description_single_line = item.description.as_deref().map(normalize_to_single_line);
            let line = self.render_item(
                &item,
                is_selected,
                width,
                description_single_line.as_deref(),
                primary_column_width,
            );
            lines.push(line);
        }

        if start_index > 0 || end_index < self.filtered_items.len() {
            let scroll_text = if self.layout.show_directional_scroll_info {
                self.format_directional_scroll_info(start_index, self.filtered_items.len() - end_index)
            } else {
                format!("  ({}/{})", self.selected_index + 1, self.filtered_items.len())
            };
            lines.push((self.theme.scroll_info)(&truncate_to_width(&scroll_text, (width - 2) as f64, "", false)));
        }

        if self.layout.show_selected_description {
            self.render_selected_description(&mut lines, width);
        }

        lines
    }

    fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.up") {
            self.selected_index = if self.selected_index == 0 {
                self.filtered_items.len().saturating_sub(1)
            } else {
                self.selected_index - 1
            };
            self.notify_selection_change();
        } else if kb.matches(key_data, "tui.select.down") {
            self.selected_index = if self.selected_index == self.filtered_items.len().saturating_sub(1) {
                0
            } else {
                self.selected_index + 1
            };
            self.notify_selection_change();
        } else if kb.matches(key_data, "tui.select.confirm") {
            let selected_item = self.filtered_items.get(self.selected_index).cloned();
            if let Some(item) = selected_item {
                if let Some(callback) = self.on_select.as_mut() {
                    callback(&item);
                }
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            if let Some(callback) = self.on_cancel.as_mut() {
                callback();
            }
        }
    }

    fn invalidate(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> String {
        text.to_string()
    }

    fn theme() -> SelectListTheme {
        SelectListTheme {
            selected_prefix: Box::new(plain),
            selected_text: Box::new(plain),
            description: Box::new(plain),
            argument_hint: None,
            source_tag: None,
            scroll_info: Box::new(plain),
            no_match: Box::new(plain),
        }
    }

    fn items() -> Vec<SelectItem> {
        vec![
            SelectItem {
                value: "/help".to_string(),
                label: "/help".to_string(),
                description: Some("show help".to_string()),
                ..SelectItem::default()
            },
            SelectItem {
                value: "/clear".to_string(),
                label: "/clear".to_string(),
                ..SelectItem::default()
            },
        ]
    }

    #[test]
    fn empty_list_renders_no_match() {
        let mut list = SelectList::new(Vec::new(), 5, theme(), SelectListLayoutOptions::default());
        assert_eq!(list.render(20.0), vec!["  No matching commands".to_string()]);
    }

    #[test]
    fn selected_item_is_marked_with_prefix() {
        let mut list = SelectList::new(items(), 5, theme(), SelectListLayoutOptions::default());
        let lines = list.render(40.0);
        assert!(lines[0].starts_with("› "));
        assert!(lines[1].starts_with("  "));
    }

    #[test]
    fn filter_is_case_insensitive_prefix_match() {
        let mut list = SelectList::new(items(), 5, theme(), SelectListLayoutOptions::default());
        list.set_filter("/HE");
        assert_eq!(list.filtered_items().len(), 1);
        assert_eq!(list.get_selected_item().unwrap().value, "/help");
        assert_eq!(list.selected_index(), 0);
    }

    #[test]
    fn selected_index_is_clamped() {
        let mut list = SelectList::new(items(), 5, theme(), SelectListLayoutOptions::default());
        list.set_selected_index(99);
        assert_eq!(list.selected_index(), 1);
        list.set_selected_index(0);
        assert_eq!(list.selected_index(), 0);
    }

    #[test]
    fn scroll_info_shows_position_when_list_overflows() {
        let mut list = SelectList::new(items(), 1, theme(), SelectListLayoutOptions::default());
        let lines = list.render(40.0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], "  (1/2)");
    }

    #[test]
    fn directional_scroll_info_lists_hidden_rows() {
        let mut list = SelectList::new(items(), 1, theme(), SelectListLayoutOptions::default());
        list.set_selected_index(1);
        list.layout.show_directional_scroll_info = true;
        let lines = list.render(40.0);
        assert_eq!(lines[1], "  ↑ 1 more");
    }

    #[test]
    fn selected_description_is_appended() {
        let mut list = SelectList::new(items(), 5, theme(), SelectListLayoutOptions::default());
        list.layout.show_selected_description = true;
        let lines = list.render(40.0);
        assert_eq!(lines.last().unwrap(), "  show help");
    }

    #[test]
    fn primary_column_bounds_default_to_32() {
        let list = SelectList::new(items(), 5, theme(), SelectListLayoutOptions::default());
        assert_eq!(
            list.get_primary_column_bounds(),
            (DEFAULT_PRIMARY_COLUMN_WIDTH, DEFAULT_PRIMARY_COLUMN_WIDTH)
        );
        assert_eq!(list.get_primary_column_width(), 32);
    }

    #[test]
    fn metadata_item_truncates_to_width() {
        let mut layout = SelectListLayoutOptions::default();
        layout.show_item_metadata = true;
        layout.min_primary_column_width = Some(4);
        layout.max_primary_column_width = Some(4);
        let mut list = SelectList::new(
            vec![SelectItem {
                value: "/help".to_string(),
                label: "/help".to_string(),
                argument_hint: Some("<topic>".to_string()),
                source_tag: Some("core".to_string()),
                ..SelectItem::default()
            }],
            5,
            theme(),
            layout,
        );
        let lines = list.render(12.0);
        assert!(visible_width(&lines[0]) <= 12, "line too wide: {:?}", lines[0]);
        assert!(lines[0].contains("core"));
    }
}
