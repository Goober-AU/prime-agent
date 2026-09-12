//! Port of packages/coding-agent/src/modes/interactive/components/extension-selector.ts

use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::{Component, Container, TUI};
use std::cell::RefCell;
use std::rc::Rc;

use crate::modes::interactive::components::countdown_timer::CountdownTimer;
use crate::modes::interactive::components::keybinding_hints::{
    key_hint, raw_key_hint, KeyTextOptions,
};
use crate::modes::interactive::components::menu_panel::{
    get_menu_list_layout, MenuList, MenuListLayout, MenuListLayoutOptions, MenuPanel,
    MenuPanelOptions, MenuRow, MenuRowOptions, MenuViewportProvider,
};
use crate::modes::interactive::theme::theme::theme;

/// `ExtensionSelectorOptions`
pub struct ExtensionSelectorOptions {
    pub tui: Option<Rc<RefCell<TUI>>>,
    pub timeout: Option<f64>,
    pub get_rows: Option<Rc<dyn Fn() -> f64>>,
}

impl Default for ExtensionSelectorOptions {
    fn default() -> Self {
        Self {
            tui: None,
            timeout: None,
            get_rows: None,
        }
    }
}

const PREFERRED_VISIBLE_OPTIONS: usize = 8;
const OPTION_LIST_RESERVED_BASE_ROWS: usize = 5;
const OPTION_SCROLL_INDICATOR_ROWS: usize = 1;

/// Port of `splitTitleAndDescription`.
pub fn split_title_and_description(value: &str) -> (String, Option<String>, usize) {
    let lines: Vec<String> = split_lines_crlf(value)
        .into_iter()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    let title = lines.first().cloned().unwrap_or_default();
    let description_lines: Vec<String> = lines.iter().skip(1).cloned().collect();
    let description = if description_lines.is_empty() {
        None
    } else {
        Some(description_lines.join("\n"))
    };
    (title, description, description_lines.len())
}

/// `value.split(/\r?\n/)`
fn split_lines_crlf(value: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let characters: Vec<char> = value.chars().collect();
    let mut index = 0usize;
    while index < characters.len() {
        let character = characters[index];
        if character == '\r' && characters.get(index + 1) == Some(&'\n') {
            lines.push(std::mem::take(&mut current));
            index += 2;
            continue;
        }
        if character == '\n' {
            lines.push(std::mem::take(&mut current));
            index += 1;
            continue;
        }
        current.push(character);
        index += 1;
    }
    lines.push(current);
    lines
}

/// Port of `ExtensionSelectorComponent`.
pub struct ExtensionSelectorComponent {
    options: Vec<String>,
    selected_index: usize,
    list_container: Rc<RefCell<MenuList>>,
    on_select_callback: Box<dyn FnMut(&str)>,
    on_cancel_callback: Rc<RefCell<Box<dyn FnMut()>>>,
    base_title: String,
    countdown: Option<CountdownTimer>,
    panel: Rc<RefCell<MenuPanel>>,
    reserved_rows: usize,
    list_layout: MenuListLayout,
    viewport: MenuViewportProvider,
    container: Container,
}

impl ExtensionSelectorComponent {
    /// Port of the `ExtensionSelectorComponent` constructor.
    pub fn new(
        title: &str,
        options: Vec<String>,
        on_select: Box<dyn FnMut(&str)>,
        on_cancel: Box<dyn FnMut()>,
        opts: ExtensionSelectorOptions,
    ) -> Self {
        let header = split_title_and_description(title);
        let base_title = header.0.clone();
        let reserved_rows = OPTION_LIST_RESERVED_BASE_ROWS + header.2;
        let viewport = MenuViewportProvider {
            get_rows: opts.get_rows.clone(),
        };
        let tui = opts.tui.clone();

        let panel = Rc::new(RefCell::new(MenuPanel::new(MenuPanelOptions {
            title: header.0.clone(),
            subtitle: header.1.clone(),
        })));

        let list_container = Rc::new(RefCell::new(MenuList::new(Some(Box::new(|| true)))));
        panel
            .borrow_mut()
            .add_full_width_child(Rc::clone(&list_container) as Rc<RefCell<dyn Component>>);
        panel.borrow_mut().add_full_width_child(
            Rc::new(RefCell::new(Spacer::new(1))) as Rc<RefCell<dyn Component>>
        );
        panel
            .borrow_mut()
            .add_full_width_child(Rc::new(RefCell::new(Text::new(
                format!(
                    "{}  {}  {}",
                    raw_key_hint("\u{2191}\u{2193}", "navigate"),
                    key_hint("tui.select.confirm", "select", &KeyTextOptions::default()),
                    key_hint("tui.select.cancel", "cancel", &KeyTextOptions::default())
                ),
                1,
                0,
                None,
            ))) as Rc<RefCell<dyn Component>>);

        let mut container = Container::new();
        container.add_child(Rc::clone(&panel) as Rc<RefCell<dyn Component>>);

        let mut selector = Self {
            options,
            selected_index: 0,
            list_container,
            on_select_callback: on_select,
            on_cancel_callback: Rc::new(RefCell::new(on_cancel)),
            base_title,
            countdown: None,
            panel,
            reserved_rows,
            list_layout: MenuListLayout {
                compact: true,
                visible_items: PREFERRED_VISIBLE_OPTIONS,
            },
            viewport,
            container,
        };

        if let Some(timeout) = opts.timeout {
            if timeout > 0.0 {
                if let Some(tui) = tui {
                    let base_title = selector.base_title.clone();
                    let panel_title = Rc::clone(selector.panel.borrow().title_slot());
                    // `() => this.onCancelCallback()` - the timer calls the same
                    // callback the cancel key does, so both share the box.
                    let on_expire = Rc::clone(&selector.on_cancel_callback);
                    let timer = CountdownTimer::new(
                        timeout,
                        Some(tui),
                        Box::new(move |seconds| {
                            *panel_title.borrow_mut() = format!("{base_title} ({seconds}s)");
                        }),
                        Box::new(move || (on_expire.borrow_mut())()),
                    );
                    selector.countdown = Some(timer);
                }
            }
        }

        selector.update_list();
        selector
    }
}

impl Component for ExtensionSelectorComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        // The timer posts its ticks to the owner (see `CountdownTimer`), so the
        // render pass drains them before producing output.
        if let Some(countdown) = self.countdown.as_mut() {
            countdown.poll();
        }
        let previous_layout = self.list_layout;
        self.update_layout();
        if self.list_layout.compact != previous_layout.compact
            || self.list_layout.visible_items != previous_layout.visible_items
        {
            self.update_list();
        }
        self.container.render(width)
    }

    fn get_selection_regions(&self) -> Vec<pi_tui::selection_metadata::TableCellSelectionRegion> {
        self.container.get_selection_regions()
    }

    fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.up") || key_data == "k" {
            self.selected_index = self.selected_index.saturating_sub(1);
            self.update_list();
        } else if kb.matches(key_data, "tui.select.down") || key_data == "j" {
            self.selected_index = std::cmp::min(
                self.options.len().saturating_sub(1),
                self.selected_index + 1,
            );
            self.update_list();
        } else if kb.matches(key_data, "tui.select.confirm") || key_data == "\n" {
            if let Some(selected) = self.options.get(self.selected_index).cloned() {
                (self.on_select_callback)(&selected);
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.on_cancel_callback.borrow_mut())();
        }
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}

impl ExtensionSelectorComponent {
    /// Port of `dispose`.
    pub fn dispose(&mut self) {
        if let Some(countdown) = self.countdown.as_mut() {
            countdown.dispose();
        }
    }

    /// Port of `updateLayout`.
    fn update_layout(&mut self) {
        self.list_layout = get_menu_list_layout(MenuListLayoutOptions {
            get_rows: self.viewport.get_rows.clone(),
            preferred_visible_items: PREFERRED_VISIBLE_OPTIONS,
            total_items: Some(self.options.len()),
            reserved_rows: self.reserved_rows,
            comfortable_item_rows: 1,
            compact_item_rows: Some(1),
            scroll_indicator_rows: Some(OPTION_SCROLL_INDICATOR_ROWS),
            ..Default::default()
        });
    }

    /// Port of `updateList`.
    fn update_list(&mut self) {
        self.update_layout();
        let mut list = self.list_container.borrow_mut();
        list.clear();
        let max_visible = self.list_layout.visible_items;
        let start_index = std::cmp::max(
            0,
            std::cmp::min(
                self.selected_index as i64 - (max_visible / 2) as i64,
                self.options.len() as i64 - max_visible as i64,
            ),
        ) as usize;
        let end_index = std::cmp::min(start_index + max_visible, self.options.len());
        for index in start_index..end_index {
            let is_selected = index == self.selected_index;
            list.add_row(Rc::new(RefCell::new(MenuRow::new(MenuRowOptions {
                primary: self.options.get(index).cloned().unwrap_or_default(),
                secondary: None,
                meta: None,
                selected: is_selected,
            }))));
        }
        if start_index > 0 || end_index < self.options.len() {
            list.add_child(
                Rc::new(RefCell::new(Text::new(
                    theme().fg(
                        "muted",
                        &format!("  ({}/{})", self.selected_index + 1, self.options.len()),
                    ),
                    0,
                    0,
                    None,
                ))) as Rc<RefCell<dyn Component>>,
                true,
            );
        }
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn init() {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
    }

    #[test]
    fn splits_title_and_description_lines() {
        let (title, description, rows) =
            split_title_and_description("Pick one\n\n second line \nthird");
        assert_eq!(title, "Pick one");
        assert_eq!(description.as_deref(), Some("second line\nthird"));
        assert_eq!(rows, 2);
    }

    #[test]
    fn renders_selected_row_and_scroll_indicator() {
        init();
        let options: Vec<String> = (0..20).map(|index| format!("option {index}")).collect();
        let selected = Rc::new(RefCell::new(String::new()));
        let selected_for_select = Rc::clone(&selected);
        let mut component = ExtensionSelectorComponent::new(
            "Pick one",
            options,
            Box::new(move |value| *selected_for_select.borrow_mut() = value.to_string()),
            Box::new(|| {}),
            ExtensionSelectorOptions::default(),
        );
        component.selected_index = 10;
        component.update_list();
        let lines = component.render(40.0);
        assert!(lines.iter().any(|line| line.contains("(11/20)")));
        assert!(lines.iter().any(|line| line.contains("option 10")));
    }

    #[test]
    fn confirm_selects_the_current_option() {
        init();
        let selected = Rc::new(RefCell::new(String::new()));
        let selected_for_select = Rc::clone(&selected);
        let mut component = ExtensionSelectorComponent::new(
            "Pick one",
            vec!["a".to_string(), "b".to_string()],
            Box::new(move |value| *selected_for_select.borrow_mut() = value.to_string()),
            Box::new(|| {}),
            ExtensionSelectorOptions::default(),
        );
        component.handle_input("\n");
        assert_eq!(*selected.borrow(), "a");
        component.handle_input("j");
        component.handle_input("\n");
        assert_eq!(*selected.borrow(), "b");
    }

    #[test]
    fn cancel_invokes_the_cancel_callback() {
        init();
        let cancelled = Rc::new(RefCell::new(false));
        let cancelled_for_cancel = Rc::clone(&cancelled);
        let mut component = ExtensionSelectorComponent::new(
            "Pick one",
            vec!["a".to_string()],
            Box::new(|_| {}),
            Box::new(move || *cancelled_for_cancel.borrow_mut() = true),
            ExtensionSelectorOptions::default(),
        );
        component.handle_input("\u{1b}");
        assert!(*cancelled.borrow());
    }
}
