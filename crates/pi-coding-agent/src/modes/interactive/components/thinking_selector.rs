//! Port of packages/coding-agent/src/modes/interactive/components/thinking-selector.ts

use std::cell::RefCell;
use std::rc::Rc;

use pi_agent_core::types::ThinkingLevel;
use pi_tui::components::select_list::{SelectItem, SelectList, SelectListLayoutOptions};
use pi_tui::tui::{Component, Container};

use super::super::theme::theme::get_select_list_theme;
use super::dynamic_border::{ColorFn, DynamicBorder};
// `theme.ts`'s `SelectListTheme` carries `Send + Sync` closures; the `pi-tui`
// select list takes the same closures without those bounds. The conversion is
// shared with the other selectors (components/show-images-selector.ts).
use super::show_images_selector::to_tui_select_list_theme;

/// `THINKING_SELECT_LIST_LAYOUT` (kept as a function because the layout owns closures).
fn thinking_select_list_layout() -> SelectListLayoutOptions {
    SelectListLayoutOptions {
        min_primary_column_width: Some(12),
        max_primary_column_width: Some(32),
        truncate_primary: None,
        show_item_metadata: false,
        show_directional_scroll_info: false,
        show_selected_description: false,
    }
}

/// `LEVEL_DESCRIPTIONS: Record<ThinkingLevel, string>`.
pub fn level_description(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "No reasoning",
        ThinkingLevel::Minimal => "Very brief reasoning",
        ThinkingLevel::Low => "Light reasoning",
        ThinkingLevel::Medium => "Moderate reasoning",
        ThinkingLevel::High => "Deep reasoning",
        ThinkingLevel::Xhigh => "Very deep reasoning",
        ThinkingLevel::Max => "Maximum reasoning",
    }
}

/// `new DynamicBorder()`'s default color: `theme.fg("border", str)`.
fn border_color() -> ColorFn {
    Box::new(|text: &str| crate::modes::interactive::theme::theme::theme().fg("border", text))
}

/// `item.value as ThinkingLevel`.
fn thinking_level_from_str(value: &str) -> Option<ThinkingLevel> {
    match value {
        "off" => Some(ThinkingLevel::Off),
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" => Some(ThinkingLevel::Xhigh),
        "max" => Some(ThinkingLevel::Max),
        _ => None,
    }
}

/// Port of `ThinkingSelectorComponent extends Container`.
pub struct ThinkingSelectorComponent {
    container: Container,
    select_list: Rc<RefCell<SelectList>>,
}

impl ThinkingSelectorComponent {
    pub fn new(
        current_level: ThinkingLevel,
        available_levels: &[ThinkingLevel],
        on_select: Box<dyn FnMut(ThinkingLevel)>,
        on_cancel: Box<dyn FnMut()>,
    ) -> Self {
        let thinking_levels: Vec<SelectItem> = available_levels
            .iter()
            .map(|level| SelectItem {
                value: level.as_str().to_string(),
                label: level.as_str().to_string(),
                description: Some(level_description(*level).to_string()),
                argument_hint: None,
                source_tag: None,
                takes_argument: None,
            })
            .collect();

        let mut on_select = on_select;
        let mut select_list = SelectList::new(
            thinking_levels.clone(),
            thinking_levels.len(),
            to_tui_select_list_theme(get_select_list_theme()),
            thinking_select_list_layout(),
        );

        let current_index = thinking_levels
            .iter()
            .position(|item| item.value == current_level.as_str());
        if let Some(current_index) = current_index {
            select_list.set_selected_index(current_index);
        }

        select_list.on_select = Some(Box::new(move |item: &SelectItem| {
            if let Some(level) = thinking_level_from_str(&item.value) {
                on_select(level);
            }
        }));

        select_list.on_cancel = Some(on_cancel);

        let mut container = Container::new();
        container.add_child(Rc::new(RefCell::new(DynamicBorder::new(border_color()))));
        let select_list = Rc::new(RefCell::new(select_list));
        container.add_child(Rc::clone(&select_list) as Rc<RefCell<dyn Component>>);
        container.add_child(Rc::new(RefCell::new(DynamicBorder::new(border_color()))));

        Self {
            container,
            select_list,
        }
    }

    pub fn get_select_list(&self) -> Rc<RefCell<SelectList>> {
        Rc::clone(&self.select_list)
    }
}

impl Component for ThinkingSelectorComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        Component::render(&mut self.container, width)
    }

    fn handle_input(&mut self, data: &str) {
        // Container only lays out its children; its default input handler is a
        // no-op. The focus owner must forward keys to the actual selectable list.
        self.select_list.borrow_mut().handle_input(data);
    }

    fn invalidate(&mut self) {
        Component::invalidate(&mut self.container);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn effort_menu_accepts_navigation_and_selection() {
        crate::modes::interactive::theme::theme::init_theme(Some("dark"), false);
        let selected = Rc::new(Cell::new(None));
        let output = selected.clone();
        let mut picker = ThinkingSelectorComponent::new(
            ThinkingLevel::High,
            &[ThinkingLevel::Low, ThinkingLevel::Medium, ThinkingLevel::High,
              ThinkingLevel::Xhigh, ThinkingLevel::Max],
            Box::new(move |value| output.set(Some(value))),
            Box::new(|| panic!("selection must not cancel")),
        );
        assert!(picker.render(80.0).join("\n").contains("xhigh"));
        picker.handle_input("\x1b[B");
        picker.handle_input("\r");
        assert_eq!(selected.get(), Some(ThinkingLevel::Xhigh));
        picker.handle_input("\x1b[A");
        picker.handle_input("\r");
        assert_eq!(selected.get(), Some(ThinkingLevel::High));
    }

    #[test]
    fn effort_menu_escape_cancels_without_changing_level() {
        let cancelled = Rc::new(Cell::new(false));
        let output = cancelled.clone();
        let mut picker = ThinkingSelectorComponent::new(
            ThinkingLevel::Max, &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max],
            Box::new(|_| panic!("cancel must not select")),
            Box::new(move || output.set(true)),
        );
        picker.handle_input("\x1b");
        assert!(cancelled.get());
    }
}
