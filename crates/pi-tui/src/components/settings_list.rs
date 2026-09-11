//! Port of packages/tui/src/components/settings-list.ts

use std::cell::RefCell;
use std::rc::Rc;

use crate::components::input::Input;
use crate::fuzzy::fuzzy_filter;
use crate::keybindings::get_keybindings;
use crate::tui::Component;
use crate::utils::{truncate_to_width, visible_width, wrap_text_with_ansi};

/// `done(selectedValue?: string)` from the TypeScript submenu callback.
pub type SubmenuDone = Rc<dyn Fn(Option<String>)>;

/// `submenu(currentValue, done) => Component` factory.
pub type SubmenuFactory = Box<dyn FnMut(&str, SubmenuDone) -> Box<dyn Component>>;

/// Port of `SettingItem`.
pub struct SettingItem {
    /// Unique identifier for this setting
    pub id: String,
    /// Display label (left side)
    pub label: String,
    /// Optional description shown when selected
    pub description: Option<String>,
    /// Current value to display (right side)
    pub current_value: String,
    /// If provided, Enter/Space cycles through these values
    pub values: Option<Vec<String>>,
    /// If provided, Enter opens this submenu. Receives current value and done callback.
    pub submenu: Option<SubmenuFactory>,
}

/// Port of `SettingsListTheme`.
pub struct SettingsListTheme {
    pub label: Box<dyn Fn(&str, bool) -> String>,
    pub value: Box<dyn Fn(&str, bool) -> String>,
    pub description: Box<dyn Fn(&str) -> String>,
    pub cursor: String,
    pub hint: Box<dyn Fn(&str) -> String>,
}

/// Port of `SettingsListOptions`.
#[derive(Default)]
pub struct SettingsListOptions {
    pub enable_search: Option<bool>,
}

pub struct SettingsList {
    items: Vec<SettingItem>,
    filtered_items: Vec<usize>,
    theme: SettingsListTheme,
    selected_index: usize,
    max_visible: usize,
    on_change: Box<dyn FnMut(&str, &str)>,
    on_cancel: Box<dyn FnMut()>,
    search_input: Option<Input>,
    search_enabled: bool,

    submenu_component: Option<Box<dyn Component>>,
    submenu_item_index: Option<usize>,
    /// Shared with the active submenu's `done` callback: the TypeScript closure
    /// captures its own `item`, so the port records that item index alongside the
    /// selected value. `Some((index, value))` closes the submenu and applies the
    /// value to that item when it is `Some`.
    submenu_done: Option<Rc<RefCell<Option<(usize, Option<String>)>>>>,
}

impl SettingsList {
    pub fn new(
        items: Vec<SettingItem>,
        max_visible: usize,
        theme: SettingsListTheme,
        on_change: Box<dyn FnMut(&str, &str)>,
        on_cancel: Box<dyn FnMut()>,
        options: SettingsListOptions,
    ) -> Self {
        let search_enabled = options.enable_search.unwrap_or(false);
        let filtered_items = (0..items.len()).collect();
        Self {
            items,
            filtered_items,
            theme,
            selected_index: 0,
            max_visible,
            on_change,
            on_cancel,
            search_input: if search_enabled { Some(Input::new()) } else { None },
            search_enabled,
            submenu_component: None,
            submenu_item_index: None,
            submenu_done: None,
        }
    }

    pub fn update_value(&mut self, id: &str, new_value: String) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.current_value = new_value;
        }
    }

    pub fn items(&self) -> &[SettingItem] {
        &self.items
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Indices into `items` that the search filter currently shows.
    fn display_indices(&self) -> Vec<usize> {
        if self.search_enabled {
            self.filtered_items.clone()
        } else {
            (0..self.items.len()).collect()
        }
    }

    fn render_main_list(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();

        if self.search_enabled {
            if let Some(search_input) = self.search_input.as_mut() {
                lines.extend(search_input.render(width));
                lines.push(String::new());
            }
        }

        if self.items.is_empty() {
            lines.push((self.theme.hint)("  No settings available"));
            if self.search_enabled {
                self.add_hint_line(&mut lines, width);
            }
            return lines;
        }

        let display_items = self.display_indices();
        if display_items.is_empty() {
            lines.push(truncate_to_width(
                &(self.theme.hint)("  No matching settings"),
                width,
                "...",
                false,
            ));
            self.add_hint_line(&mut lines, width);
            return lines;
        }

        let half = self.max_visible / 2;
        let start_index = 0.max(
            (self.selected_index as i64 - half as i64).min(display_items.len() as i64 - self.max_visible as i64),
        ) as usize;
        let end_index = (start_index + self.max_visible).min(display_items.len());

        let max_label_width = 30.min(
            self.items
                .iter()
                .map(|item| visible_width(&item.label))
                .max()
                .unwrap_or(0),
        );

        for i in start_index..end_index {
            let item_index = match display_items.get(i) {
                Some(index) => *index,
                None => continue,
            };
            let item = &self.items[item_index];

            let is_selected = i == self.selected_index;
            let prefix = if is_selected {
                self.theme.cursor.clone()
            } else {
                "  ".to_string()
            };
            let prefix_width = visible_width(&prefix);

            let label_padded = format!(
                "{}{}",
                item.label,
                " ".repeat(max_label_width.saturating_sub(visible_width(&item.label)))
            );
            let label_text = (self.theme.label)(&label_padded, is_selected);

            let separator = "  ";
            let used_width = prefix_width + max_label_width + visible_width(separator);
            let value_max_width = width.saturating_sub(used_width).saturating_sub(2);

            let value_text = (self.theme.value)(
                &truncate_to_width(&item.current_value, value_max_width, "", false),
                is_selected,
            );

            lines.push(truncate_to_width(
                &format!("{prefix}{label_text}{separator}{value_text}"),
                width,
                "...",
                false,
            ));
        }

        if start_index > 0 || end_index < display_items.len() {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, display_items.len());
            lines.push((self.theme.hint)(&truncate_to_width(&scroll_text, width - 2, "", false)));
        }

        let selected_item = display_items.get(self.selected_index).and_then(|i| self.items.get(*i));
        if let Some(description) = selected_item.and_then(|item| item.description.as_deref()) {
            lines.push(String::new());
            let wrapped_desc = wrap_text_with_ansi(description, width - 4);
            for line in wrapped_desc {
                lines.push((self.theme.description)(&format!("  {line}")));
            }
        }

        self.add_hint_line(&mut lines, width);

        lines
    }

    fn activate_item(&mut self) {
        let item_index = if self.search_enabled {
            self.filtered_items.get(self.selected_index).copied()
        } else if self.selected_index < self.items.len() {
            Some(self.selected_index)
        } else {
            None
        };
        let item_index = match item_index {
            Some(index) => index,
            None => return,
        };

        let has_submenu = self.items[item_index].submenu.is_some();
        if has_submenu {
            self.submenu_item_index = Some(self.selected_index);
            let current_value = self.items[item_index].current_value.clone();
            let done_cell: Rc<RefCell<Option<(usize, Option<String>)>>> = Rc::new(RefCell::new(None));
            let done: SubmenuDone = {
                let cell = Rc::clone(&done_cell);
                Rc::new(move |selected_value: Option<String>| {
                    *cell.borrow_mut() = Some((item_index, selected_value));
                })
            };
            let component = {
                let factory = self.items[item_index].submenu.as_mut().unwrap();
                factory(&current_value, done)
            };
            self.submenu_done = Some(done_cell);
            self.submenu_component = Some(component);
        } else {
            let values = self.items[item_index].values.clone();
            if let Some(values) = values {
                if !values.is_empty() {
                    let current_value = self.items[item_index].current_value.clone();
                    let current_index = values.iter().position(|v| *v == current_value);
                    let next_index = match current_index {
                        Some(index) => (index + 1) % values.len(),
                        None => 0,
                    };
                    let new_value = values[next_index].clone();
                    self.items[item_index].current_value = new_value.clone();
                    (self.on_change)(&self.items[item_index].id, &new_value);
                }
            }
        }
    }

    fn close_submenu(&mut self) {
        self.submenu_component = None;
        self.submenu_done = None;
        if let Some(index) = self.submenu_item_index.take() {
            self.selected_index = index;
        }
    }

    /// Applies the result of the submenu `done` callback recorded during input.
    fn settle_submenu_done(&mut self) {
        let pending = self.submenu_done.as_ref().and_then(|cell| cell.borrow_mut().take());
        if let Some((item_index, selected_value)) = pending {
            if let Some(selected_value) = selected_value {
                if item_index < self.items.len() {
                    self.items[item_index].current_value = selected_value.clone();
                    let id = self.items[item_index].id.clone();
                    (self.on_change)(&id, &selected_value);
                }
            }
            self.close_submenu();
        }
    }

    fn apply_filter(&mut self, query: &str) {
        let labels: Vec<String> = self.items.iter().map(|item| item.label.clone()).collect();
        let indices: Vec<usize> = (0..self.items.len()).collect();
        self.filtered_items = fuzzy_filter(&indices, query, &|index: &usize| labels[*index].clone());
        self.selected_index = 0;
    }

    fn add_hint_line(&self, lines: &mut Vec<String>, width: usize) {
        lines.push(String::new());
        lines.push(truncate_to_width(
            &(self.theme.hint)(if self.search_enabled {
                "  Type to search · Enter/Space to change · Esc to cancel"
            } else {
                "  Enter/Space to change · Esc to cancel"
            }),
            width,
            "...",
            false,
        ));
    }
}

impl Component for SettingsList {
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.submenu_component.is_some() {
            let lines = self.submenu_component.as_mut().unwrap().render(width);
            self.settle_submenu_done();
            return lines;
        }

        self.render_main_list(width)
    }

    fn handle_input(&mut self, data: &str) {
        // If submenu is active, delegate all input to it
        // The submenu's onCancel (triggered by escape) will call done() which closes it
        if self.submenu_component.is_some() {
            if let Some(component) = self.submenu_component.as_mut() {
                component.handle_input(data);
            }
            self.settle_submenu_done();
            return;
        }

        let kb = get_keybindings();
        let display_len = self.display_indices().len();
        if kb.matches(data, "tui.select.up") {
            if display_len == 0 {
                return;
            }
            self.selected_index = if self.selected_index == 0 {
                display_len - 1
            } else {
                self.selected_index - 1
            };
        } else if kb.matches(data, "tui.select.down") {
            if display_len == 0 {
                return;
            }
            self.selected_index = if self.selected_index == display_len - 1 {
                0
            } else {
                self.selected_index + 1
            };
        } else if kb.matches(data, "tui.select.confirm") || data == " " {
            self.activate_item();
        } else if kb.matches(data, "tui.select.cancel") {
            (self.on_cancel)();
        } else if self.search_enabled {
            let sanitized: String = data.chars().filter(|ch| *ch != ' ').collect();
            if sanitized.is_empty() {
                return;
            }
            if let Some(search_input) = self.search_input.as_mut() {
                search_input.handle_input(&sanitized);
                let value = search_input.get_value().to_string();
                self.apply_filter(&value);
            }
        }
    }

    fn invalidate(&mut self) {
        if let Some(component) = self.submenu_component.as_mut() {
            component.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str, _selected: bool) -> String {
        text.to_string()
    }

    fn theme() -> SettingsListTheme {
        SettingsListTheme {
            label: Box::new(plain),
            value: Box::new(plain),
            description: Box::new(|text: &str| text.to_string()),
            cursor: "› ".to_string(),
            hint: Box::new(|text: &str| text.to_string()),
        }
    }

    fn item(id: &str, label: &str, values: Option<Vec<String>>) -> SettingItem {
        SettingItem {
            id: id.to_string(),
            label: label.to_string(),
            description: None,
            current_value: values
                .as_ref()
                .and_then(|v| v.first().cloned())
                .unwrap_or_default(),
            values,
            submenu: None,
        }
    }

    #[test]
    fn empty_settings_render_hint() {
        let mut list = SettingsList::new(
            Vec::new(),
            5,
            theme(),
            Box::new(|_, _| {}),
            Box::new(|| {}),
            SettingsListOptions::default(),
        );
        let lines = list.render(20);
        assert_eq!(lines[0], "  No settings available");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "  Enter/Space to change · Esc to cancel");
    }

    #[test]
    fn values_cycle_on_confirm_and_notify_change() {
        let changes = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
        let sink = Rc::clone(&changes);
        let mut list = SettingsList::new(
            vec![item("theme", "Theme", Some(vec!["dark".into(), "light".into()]))],
            5,
            theme(),
            Box::new(move |id, value| sink.borrow_mut().push((id.to_string(), value.to_string()))),
            Box::new(|| {}),
            SettingsListOptions::default(),
        );
        list.handle_input("\r");
        assert_eq!(list.items()[0].current_value, "light");
        assert_eq!(*changes.borrow(), vec![("theme".to_string(), "light".to_string())]);
        list.handle_input("\r");
        assert_eq!(list.items()[0].current_value, "dark");
    }

    #[test]
    fn update_value_by_id() {
        let mut list = SettingsList::new(
            vec![item("a", "A", None)],
            5,
            theme(),
            Box::new(|_, _| {}),
            Box::new(|| {}),
            SettingsListOptions::default(),
        );
        list.update_value("a", "42".to_string());
        assert_eq!(list.items()[0].current_value, "42");
        list.update_value("missing", "x".to_string());
        assert_eq!(list.items()[0].current_value, "42");
    }

    #[test]
    fn escape_cancels() {
        let cancelled = Rc::new(RefCell::new(0));
        let sink = Rc::clone(&cancelled);
        let mut list = SettingsList::new(
            vec![item("a", "A", None)],
            5,
            theme(),
            Box::new(|_, _| {}),
            Box::new(move || *sink.borrow_mut() += 1),
            SettingsListOptions::default(),
        );
        list.handle_input("\x1b");
        assert_eq!(*cancelled.borrow(), 1);
    }

    #[test]
    fn selection_wraps() {
        let mut list = SettingsList::new(
            vec![item("a", "A", None), item("b", "B", None)],
            5,
            theme(),
            Box::new(|_, _| {}),
            Box::new(|| {}),
            SettingsListOptions::default(),
        );
        list.handle_input("\x1b[B");
        assert_eq!(list.selected_index(), 1);
        list.handle_input("\x1b[B");
        assert_eq!(list.selected_index(), 0);
        list.handle_input("\x1b[A");
        assert_eq!(list.selected_index(), 1);
    }

    #[test]
    fn search_filters_labels_and_ignores_spaces() {
        let mut list = SettingsList::new(
            vec![item("a", "Alpha", None), item("b", "Beta", None)],
            5,
            theme(),
            Box::new(|_, _| {}),
            Box::new(|| {}),
            SettingsListOptions {
                enable_search: Some(true),
            },
        );
        list.handle_input(" ");
        assert_eq!(list.filtered_items.len(), 2);
        list.handle_input("b");
        assert_eq!(list.filtered_items, vec![1]);
        assert_eq!(list.selected_index(), 0);
    }

    #[test]
    fn submenu_opens_and_closes_with_value() {
        struct Choice {
            done: SubmenuDone,
        }
        impl Component for Choice {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["submenu".to_string()]
            }
            fn handle_input(&mut self, data: &str) {
                if data == "\r" {
                    (self.done)(Some("picked".to_string()));
                }
            }
            fn invalidate(&mut self) {}
        }

        let changes = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
        let sink = Rc::clone(&changes);
        let mut items = vec![item("m", "Mode", None)];
        items[0].submenu = Some(Box::new(move |_current: &str, done: SubmenuDone| {
            Box::new(Choice { done }) as Box<dyn Component>
        }));
        let mut list = SettingsList::new(
            items,
            5,
            theme(),
            Box::new(move |id, value| sink.borrow_mut().push((id.to_string(), value.to_string()))),
            Box::new(|| {}),
            SettingsListOptions::default(),
        );

        list.handle_input("\r");
        assert_eq!(list.render(20), vec!["submenu".to_string()]);
        list.handle_input("\r");
        // done("picked") applies the value and closes the submenu.
        assert_eq!(list.items()[0].current_value, "picked");
        assert_eq!(*changes.borrow(), vec![("m".to_string(), "picked".to_string())]);
        assert!(list.render(20)[0].starts_with("› "));
    }
}
