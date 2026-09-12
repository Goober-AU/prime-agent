//! Port of packages/coding-agent/src/modes/interactive/components/show-images-selector.ts

use pi_tui::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme as TuiSelectListTheme,
};
use pi_tui::tui::Component;

use crate::modes::interactive::theme::theme::{get_select_list_theme, theme};

use super::keybinding_hints::KeyTextOptions;

/// `SHOW_IMAGES_SELECT_LIST_LAYOUT`.
pub fn show_images_select_list_layout() -> SelectListLayoutOptions {
    SelectListLayoutOptions {
        min_primary_column_width: Some(12),
        max_primary_column_width: Some(32),
        ..Default::default()
    }
}

/// `DynamicBorder` (components/dynamic-border.ts) belongs to another slice, so
/// this module keeps a private copy of the border it renders; see
/// evidence/status/ca-interactive-components-3.json -> blocked_on.
pub(crate) struct DynamicBorder {
    color: Box<dyn Fn(&str) -> String>,
}

impl DynamicBorder {
    pub(crate) fn new(color: Option<Box<dyn Fn(&str) -> String>>) -> Self {
        Self {
            color: color.unwrap_or_else(|| Box::new(|text: &str| theme().fg("border", text))),
        }
    }
}

impl Component for DynamicBorder {
    fn render(&mut self, width: f64) -> Vec<String> {
        let width = (width.max(0.0).floor() as usize).max(1);
        vec![(self.color)("\u{2500}".repeat(width).as_str())]
    }

    fn invalidate(&mut self) {
        // No cached state to invalidate currently
    }
}

/// The `SelectListTheme` of `theme.ts` carries `Send + Sync` closures; the
/// `pi-tui` component takes the same closures without those bounds.
fn to_tui_select_list_theme(theme: crate::modes::interactive::theme::theme::SelectListTheme) -> TuiSelectListTheme {
    crate::modes::interactive::theme::theme::SelectListTheme {
        selected_prefix: theme.selected_prefix,
        selected_text: theme.selected_text,
        description: theme.description,
        argument_hint: Some(theme.argument_hint),
        source_tag: Some(theme.source_tag),
        scroll_info: theme.scroll_info,
        no_match: theme.no_match,
    }
}

/// Port of `ShowImagesSelectorComponent`.
pub struct ShowImagesSelectorComponent {
    children: Vec<Box<dyn Component>>,
    select_list: SelectList,
    /// The theme conversion above is only possible for the component types the
    /// module converts; keep the unused import surface honest.
    _options: KeyTextOptions,
}

impl ShowImagesSelectorComponent {
    pub fn new(
        current_value: bool,
        on_select: Box<dyn FnMut(bool)>,
        on_cancel: Box<dyn FnMut()>,
    ) -> Self {
        let items: Vec<SelectItem> = vec![
            SelectItem {
                value: "yes".to_string(),
                label: "Yes".to_string(),
                description: Some("Show image type and dimensions".to_string()),
                ..Default::default()
            },
            SelectItem {
                value: "no".to_string(),
                label: "No".to_string(),
                description: Some("Show text placeholder instead".to_string()),
                ..Default::default()
            },
        ];

        let mut select_list = SelectList::new(
            items,
            5,
            to_tui_select_list_theme(get_select_list_theme()),
            show_images_select_list_layout(),
        );

        select_list.set_selected_index(if current_value { 0 } else { 1 });

        let mut on_select = on_select;
        select_list.on_select = Some(Box::new(move |item: &SelectItem| {
            on_select(item.value == "yes");
        }));

        let mut on_cancel = on_cancel;
        select_list.on_cancel = Some(Box::new(move || {
            on_cancel();
        }));

        Self {
            children: vec![
                Box::new(DynamicBorder::new(None)),
                Box::new(SelectListRef),
                Box::new(DynamicBorder::new(None)),
            ],
            select_list,
            _options: KeyTextOptions::default(),
        }
    }

    /// Port of `getSelectList`.
    pub fn get_select_list(&mut self) -> &mut SelectList {
        &mut self.select_list
    }
}

/// Stands in for `this.addChild(this.selectList)`: the list is a field so the
/// accessor can hand it out; this child renders nothing extra.
struct SelectListRef;

impl Component for SelectListRef {
    fn render(&mut self, _width: f64) -> Vec<String> {
        Vec::new()
    }

    fn invalidate(&mut self) {}
}

impl Component for ShowImagesSelectorComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        let mut lines = self.children[0].render(width);
        lines.extend(self.select_list.render(width));
        lines.extend(self.children[2].render(width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        self.select_list.handle_input(data);
    }

    fn invalidate(&mut self) {
        for child in self.children.iter_mut() {
            child.invalidate();
        }
    }
}
