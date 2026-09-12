//! Port of packages/coding-agent/src/modes/interactive/components/theme-selector.ts

use pi_tui::components::select_list::{SelectItem, SelectList, SelectListLayoutOptions};
use pi_tui::tui::{Component, Container};
use std::cell::RefCell;
use std::rc::Rc;

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::theme::theme::{get_available_themes, get_select_list_theme};

/// `theme.ts`'s `SelectListTheme` carries `Send + Sync` closures; the `pi-tui`
/// component takes the same closures without those bounds. Private port of the
/// conversion the other selectors apply.
fn to_tui_select_list_theme(
    source: crate::modes::interactive::theme::theme::SelectListTheme,
) -> pi_tui::components::select_list::SelectListTheme {
    pi_tui::components::select_list::SelectListTheme {
        selected_prefix: source.selected_prefix,
        selected_text: source.selected_text,
        description: source.description,
        argument_hint: Some(source.argument_hint),
        source_tag: Some(source.source_tag),
        scroll_info: source.scroll_info,
        no_match: source.no_match,
    }
}

/// `THEME_SELECT_LIST_LAYOUT`
fn theme_select_list_layout() -> SelectListLayoutOptions {
    SelectListLayoutOptions {
        min_primary_column_width: Some(12),
        max_primary_column_width: Some(32),
        ..Default::default()
    }
}

/// Port of `ThemeSelectorComponent`.
pub struct ThemeSelectorComponent {
    container: Container,
    select_list: Rc<RefCell<SelectList>>,
    /// `onPreview` - stored behind a shared cell so `onSelectionChange` can call
    /// the owner's current handler, like the TypeScript closure does.
    on_preview: Rc<RefCell<Box<dyn FnMut(&str)>>>,
}

impl ThemeSelectorComponent {
    /// Port of the `ThemeSelectorComponent` constructor.
    pub fn new(
        current_theme: &str,
        on_select: Box<dyn FnMut(&SelectItem)>,
        on_cancel: Box<dyn FnMut()>,
        on_preview: Box<dyn FnMut(&str)>,
    ) -> Self {
        let themes = get_available_themes();
        let theme_items: Vec<SelectItem> = themes
            .iter()
            .map(|name| SelectItem {
                value: name.clone(),
                label: name.clone(),
                description: if name == current_theme {
                    Some("(current)".to_string())
                } else {
                    None
                },
                argument_hint: None,
                source_tag: None,
                takes_argument: None,
            })
            .collect();

        let select_list = Rc::new(RefCell::new(SelectList::new(
            theme_items,
            10,
            to_tui_select_list_theme(get_select_list_theme()),
            theme_select_list_layout(),
        )));

        let current_index = themes.iter().position(|name| name == current_theme);
        if let Some(current_index) = current_index {
            select_list.borrow_mut().set_selected_index(current_index);
        }

        select_list.borrow_mut().on_select = Some(on_select);
        select_list.borrow_mut().on_cancel = Some(on_cancel);

        // `onSelectionChange` calls `this.onPreview(item.value)`.
        let on_preview: Rc<RefCell<Box<dyn FnMut(&str)>>> = Rc::new(RefCell::new(on_preview));
        let preview_handler = Rc::clone(&on_preview);
        select_list.borrow_mut().on_selection_change = Some(Box::new(move |item: &SelectItem| {
            (preview_handler.borrow_mut())(&item.value);
        }));

        let mut container = Container::new();
        container.add_child(
            Rc::new(RefCell::new(DynamicBorder::default())) as Rc<RefCell<dyn Component>>
        );
        container.add_child(Rc::clone(&select_list) as Rc<RefCell<dyn Component>>);
        container.add_child(
            Rc::new(RefCell::new(DynamicBorder::default())) as Rc<RefCell<dyn Component>>
        );

        Self {
            container,
            select_list,
            on_preview,
        }
    }

    /// Port of `getSelectList`.
    pub fn get_select_list(&self) -> Rc<RefCell<SelectList>> {
        Rc::clone(&self.select_list)
    }

    /// The private `onPreview` handler, called by `onSelectionChange`.
    pub fn on_preview(&self, theme_name: &str) {
        (self.on_preview.borrow_mut())(theme_name);
    }
}

impl Component for ThemeSelectorComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.container.render(width)
    }

    fn get_selection_regions(&self) -> Vec<pi_tui::selection_metadata::TableCellSelectionRegion> {
        self.container.get_selection_regions()
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}
