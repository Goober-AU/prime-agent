//! Port of packages/coding-agent/src/modes/interactive/components/menu-panel.ts

use pi_tui::components::input::Input;
use pi_tui::tui::{Component, Focusable};
use pi_tui::utils::{truncate_to_width, visible_width, wrap_text_with_ansi};
use std::cell::RefCell;
use std::rc::Rc;

use crate::modes::interactive::theme::theme::theme;

/// `MenuPanelOptions`
pub struct MenuPanelOptions {
    pub title: String,
    pub subtitle: Option<String>,
}

/// `MenuViewportProvider`
#[derive(Default, Clone)]
pub struct MenuViewportProvider {
    pub get_rows: Option<Rc<dyn Fn() -> f64>>,
}

/// `MenuListOptions`
#[derive(Default)]
pub struct MenuListOptions {
    /// `compact?: boolean | (() => boolean)`
    pub compact: Option<Box<dyn Fn() -> bool>>,
}

/// `MenuListLayoutOptions`
#[derive(Default)]
pub struct MenuListLayoutOptions {
    pub get_rows: Option<Rc<dyn Fn() -> f64>>,
    pub preferred_visible_items: usize,
    pub min_visible_items: Option<usize>,
    pub total_items: Option<usize>,
    pub reserved_rows: usize,
    pub comfortable_item_rows: usize,
    pub compact_item_rows: Option<usize>,
    pub scroll_indicator_rows: Option<usize>,
    pub comfortable_list_padding_rows: Option<usize>,
    pub compact_list_padding_rows: Option<usize>,
}

/// `MenuListLayout`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MenuListLayout {
    pub compact: bool,
    pub visible_items: usize,
}

const PANEL_PADDING_X: usize = 2;
const PANEL_PADDING_Y: usize = 1;
const FIELD_PADDING_X: usize = 2;
const ROW_PADDING_X: usize = 2;
const ROW_PADDING_Y: usize = 1;
const ANSI_RESET: &str = "\x1b[0m";

/// Port of `getMenuPanelInnerWidth`.
pub fn get_menu_panel_inner_width(width: f64) -> usize {
    let safe_width = f64_max(PANEL_PADDING_X as f64 * 2.0 + 1.0, width).floor();
    std::cmp::max(1, safe_width as usize - PANEL_PADDING_X * 2)
}

/// `Math.max` for the finite widths this module accepts.
fn f64_max(left: f64, right: f64) -> f64 {
    if left > right {
        left
    } else {
        right
    }
}

/// Port of `getViewportRows`.
fn get_viewport_rows(get_rows: Option<&Rc<dyn Fn() -> f64>>) -> Option<i64> {
    let rows = get_rows.map(|get_rows| get_rows())?;
    if !rows.is_finite() || rows <= 0.0 {
        return None;
    }
    Some(rows.floor() as i64)
}

/// Port of `visibleItemCount`.
fn visible_item_count(
    rows: i64,
    preferred_visible_items: usize,
    min_visible_items: usize,
    reserved_rows: usize,
    item_rows: usize,
    list_padding_rows: usize,
    extra_rows: usize,
) -> i64 {
    let capacity_rows = std::cmp::max(
        0,
        rows - reserved_rows as i64 - list_padding_rows as i64 - extra_rows as i64,
    );
    let item_capacity = capacity_rows / std::cmp::max(1, item_rows) as i64;
    std::cmp::max(
        min_visible_items as i64,
        std::cmp::min(preferred_visible_items as i64, item_capacity),
    )
}

/// Port of `listRowsUsed`.
fn list_rows_used(
    reserved_rows: usize,
    list_padding_rows: usize,
    visible_items: i64,
    item_rows: usize,
    extra_rows: usize,
) -> i64 {
    reserved_rows as i64
        + list_padding_rows as i64
        + extra_rows as i64
        + visible_items * item_rows as i64
}

/// Port of `scrollIndicatorRows`.
fn scroll_indicator_rows(
    total_items: Option<usize>,
    visible_items: i64,
    scroll_indicator_rows: usize,
) -> usize {
    match total_items {
        None => 0,
        Some(total_items) => {
            if scroll_indicator_rows == 0 {
                0
            } else if total_items as i64 > visible_items {
                scroll_indicator_rows
            } else {
                0
            }
        }
    }
}

/// Port of `getLayoutCandidate`.
fn get_layout_candidate(
    rows: i64,
    options: &MenuListLayoutOptions,
    item_rows: usize,
    list_padding_rows: usize,
    compact: bool,
) -> (MenuListLayout, i64, bool) {
    let min_visible_items = options.min_visible_items.unwrap_or(1);
    let preferred_visible_items = std::cmp::max(min_visible_items, options.preferred_visible_items);
    let visible_items_without_scroll = visible_item_count(
        rows,
        preferred_visible_items,
        min_visible_items,
        options.reserved_rows,
        item_rows,
        list_padding_rows,
        0,
    );
    let extra_rows = scroll_indicator_rows(
        options.total_items,
        visible_items_without_scroll,
        options.scroll_indicator_rows.unwrap_or(0),
    );
    let visible_items = if extra_rows > 0 {
        visible_item_count(
            rows,
            preferred_visible_items,
            min_visible_items,
            options.reserved_rows,
            item_rows,
            list_padding_rows,
            extra_rows,
        )
    } else {
        visible_items_without_scroll
    };
    let rows_used = list_rows_used(
        options.reserved_rows,
        list_padding_rows,
        visible_items,
        item_rows,
        extra_rows,
    );
    (
        MenuListLayout {
            compact,
            visible_items: visible_items.max(0) as usize,
        },
        rows_used,
        rows_used <= rows,
    )
}

/// Port of `getMenuListLayout`.
pub fn get_menu_list_layout(options: MenuListLayoutOptions) -> MenuListLayout {
    let min_visible_items = options.min_visible_items.unwrap_or(1);
    let preferred_visible_items = std::cmp::max(min_visible_items, options.preferred_visible_items);
    let rows = get_viewport_rows(options.get_rows.as_ref());
    let Some(rows) = rows else {
        return MenuListLayout {
            compact: false,
            visible_items: preferred_visible_items,
        };
    };

    let comfortable_layout = get_layout_candidate(
        rows,
        &options,
        std::cmp::max(1, options.comfortable_item_rows),
        options.comfortable_list_padding_rows.unwrap_or(1),
        false,
    );
    let Some(compact_item_rows) = options.compact_item_rows else {
        return MenuListLayout {
            compact: false,
            visible_items: comfortable_layout.0.visible_items,
        };
    };

    let compact_layout = get_layout_candidate(
        rows,
        &options,
        std::cmp::max(1, compact_item_rows),
        options.compact_list_padding_rows.unwrap_or(0),
        true,
    );
    if compact_layout.2
        && (!comfortable_layout.2
            || compact_layout.0.visible_items > comfortable_layout.0.visible_items)
    {
        return MenuListLayout {
            compact: true,
            visible_items: compact_layout.0.visible_items,
        };
    }
    if comfortable_layout.2 {
        return MenuListLayout {
            compact: false,
            visible_items: comfortable_layout.0.visible_items,
        };
    }
    if compact_layout.1 <= comfortable_layout.1 {
        MenuListLayout {
            compact: true,
            visible_items: compact_layout.0.visible_items,
        }
    } else {
        MenuListLayout {
            compact: false,
            visible_items: comfortable_layout.0.visible_items,
        }
    }
}

/// Port of `paddedBackgroundLine`.
fn padded_background_line(
    text: &str,
    width: f64,
    padding_x: usize,
    background: Option<&dyn Fn(&str) -> String>,
) -> String {
    let width = std::cmp::max(0.0, width.floor()) as usize;
    let inner_width = std::cmp::max(1, width - padding_x * 2);
    let content = truncate_to_width(text, inner_width as f64, "", false);
    let right_padding = " ".repeat(inner_width.saturating_sub(visible_width(&content)));
    let content_span = format!("{}{content}", " ".repeat(padding_x));
    let trailing_span = format!("{right_padding}{}", " ".repeat(padding_x));
    match background {
        None => format!("{content_span}{trailing_span}"),
        Some(background) => format!(
            "{}{}",
            apply_background(&content_span, background),
            background(&trailing_span)
        ),
    }
}

/// Port of `applyBackground`.
fn apply_background(text: &str, background: &dyn Fn(&str) -> String) -> String {
    text.split(ANSI_RESET)
        .map(|segment| background(segment))
        .collect::<Vec<String>>()
        .join(ANSI_RESET)
}

/// Port of `surfaceLine`.
fn surface_line(text: &str, width: f64, padding_x: usize) -> String {
    let background = theme().get_editor_background_color();
    padded_background_line(
        text,
        width,
        padding_x,
        background
            .as_ref()
            .map(|f| f.as_ref() as &dyn Fn(&str) -> String),
    )
}

/// Port of `surfaceWrappedLines`.
fn surface_wrapped_lines(text: &str, width: f64, padding_x: usize) -> Vec<String> {
    let width = std::cmp::max(0.0, width.floor()) as usize;
    let inner_width = std::cmp::max(1, width - padding_x * 2);
    wrap_text_with_ansi(text, inner_width)
        .into_iter()
        .map(|content| surface_line(&content, width as f64, padding_x))
        .collect()
}

/// Port of `MenuPanel`.
///
/// The TypeScript reads `this.children` and calls `render` on each child. The
/// port stores children behind `Rc<RefCell<dyn Component>>` and uses the
/// `fillsMenuPanel` marker through the [`MenuListChild::fills_menu_panel`] flag,
/// which is set when the child is added.
pub struct MenuPanel {
    title: Rc<RefCell<String>>,
    subtitle: Option<String>,
    pub children: Vec<MenuListChild>,
}

impl MenuPanel {
    pub fn new(options: MenuPanelOptions) -> Self {
        Self {
            title: Rc::new(RefCell::new(options.title)),
            subtitle: options.subtitle,
            children: Vec::new(),
        }
    }

    /// Port of `setTitle`.
    pub fn set_title(&mut self, title: &str) {
        *self.title.borrow_mut() = title.to_string();
    }

    /// The shared title slot the countdown timer writes into.
    pub fn title_slot(&self) -> &Rc<RefCell<String>> {
        &self.title
    }

    pub fn add_child(&mut self, component: Rc<RefCell<dyn Component>>) {
        self.children.push(MenuListChild {
            component,
            fills_menu_panel: false,
            row: None,
        });
    }

    /// Add a child that fills the whole panel width (`fillsMenuPanel === true`).
    pub fn add_full_width_child(&mut self, component: Rc<RefCell<dyn Component>>) {
        self.children.push(MenuListChild {
            component,
            fills_menu_panel: true,
            row: None,
        });
    }

    /// Add a [`MenuRow`] child, keeping the typed handle used by [`MenuList`].
    pub fn add_row_child(&mut self, row: Rc<RefCell<MenuRow>>, fills_menu_panel: bool) {
        self.children.push(MenuListChild {
            component: Rc::clone(&row) as Rc<RefCell<dyn Component>>,
            fills_menu_panel,
            row: Some(row),
        });
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }
}

impl Component for MenuPanel {
    fn render(&mut self, width: f64) -> Vec<String> {
        let safe_width = f64_max(PANEL_PADDING_X as f64 * 2.0 + 1.0, width);
        let inner_width = get_menu_panel_inner_width(width) as f64;
        let mut lines: Vec<String> = Vec::new();

        for _ in 0..PANEL_PADDING_Y {
            lines.push(surface_line("", safe_width, PANEL_PADDING_X));
        }
        let title = self.title.borrow().clone();
        let has_title = !title.trim().is_empty();
        let subtitle = self
            .subtitle
            .as_ref()
            .map(|subtitle| subtitle.trim().to_string());
        let has_subtitle = subtitle
            .as_deref()
            .map(|subtitle| !subtitle.is_empty())
            .unwrap_or(false);
        let has_header = has_title || has_subtitle;
        if has_title {
            lines.push(surface_line(
                &theme().bold(&theme().fg("text", &title)),
                safe_width,
                PANEL_PADDING_X,
            ));
        }
        if has_subtitle {
            let subtitle = subtitle.unwrap_or_default();
            for line in
                surface_wrapped_lines(&theme().fg("muted", &subtitle), safe_width, PANEL_PADDING_X)
            {
                lines.push(line);
            }
        }
        if has_header {
            lines.push(surface_line("", safe_width, PANEL_PADDING_X));
        }

        for child in self.children.iter() {
            let mut component = child.component.borrow_mut();
            let child_lines = if child.fills_menu_panel {
                component.render(safe_width)
            } else {
                component.render(inner_width)
            };
            drop(component);
            for line in child_lines {
                if child.fills_menu_panel {
                    lines.push(line);
                } else {
                    lines.push(surface_line(&line, safe_width, PANEL_PADDING_X));
                }
            }
        }

        for _ in 0..PANEL_PADDING_Y {
            lines.push(surface_line("", safe_width, PANEL_PADDING_X));
        }
        lines
    }

    fn invalidate(&mut self) {
        for child in self.children.iter() {
            child.component.borrow_mut().invalidate();
        }
    }
}

/// One child of a [`MenuPanel`] or [`MenuList`], with the `fillsMenuPanel` flag.
pub struct MenuListChild {
    pub component: Rc<RefCell<dyn Component>>,
    pub fills_menu_panel: bool,
    /// Present for [`MenuRow`] children, so parent-aware padding can read
    /// `selected` the way `child instanceof MenuRow` does in the TypeScript.
    pub row: Option<Rc<RefCell<MenuRow>>>,
}

/// Port of `MenuSearchInput`.
pub struct MenuSearchInput {
    input: Input,
    placeholder: String,
}

impl MenuSearchInput {
    pub fn new(placeholder: String) -> Self {
        Self {
            input: Input::new(),
            placeholder,
        }
    }

    /// Port of `set onSubmit`.
    pub fn set_on_submit(&mut self, handler: Option<Box<dyn FnMut(&str)>>) {
        self.input.on_submit = handler;
    }

    /// Port of `getValue`.
    pub fn get_value(&self) -> String {
        self.input.get_value().to_string()
    }

    /// Port of `getCursor`.
    pub fn get_cursor(&self) -> usize {
        self.input.get_cursor()
    }

    /// Port of `setValue`.
    pub fn set_value(&mut self, value: &str) {
        self.input.set_value(value.to_string());
    }

    /// Port of `stripInputPrompt`.
    fn strip_input_prompt(line: &str) -> String {
        match line.strip_prefix("> ") {
            Some(rest) => rest.to_string(),
            None => line.to_string(),
        }
    }
}

impl Focusable for MenuSearchInput {
    fn focused(&self) -> bool {
        self.input.focused()
    }

    fn set_focused(&mut self, focused: bool) {
        self.input.set_focused(focused);
    }
}

impl Component for MenuSearchInput {
    fn render(&mut self, width: f64) -> Vec<String> {
        let safe_width = f64_max(FIELD_PADDING_X as f64 * 2.0 + 1.0, width);
        let inner_width = std::cmp::max(1, safe_width.floor() as usize - FIELD_PADDING_X * 2);
        let focused = self.input.focused();
        let content = if self.get_value().is_empty() && !focused {
            theme().fg("dim", &self.placeholder)
        } else {
            let first_line = self
                .input
                .render((inner_width + 2) as f64)
                .into_iter()
                .next()
                .unwrap_or_default();
            Self::strip_input_prompt(&first_line)
        };
        let background = theme().get_editor_background_color();
        vec![padded_background_line(
            &content,
            safe_width,
            FIELD_PADDING_X,
            background
                .as_ref()
                .map(|f| f.as_ref() as &dyn Fn(&str) -> String),
        )]
    }

    fn handle_input(&mut self, data: &str) {
        self.input.handle_input(data);
    }

    fn invalidate(&mut self) {
        self.input.invalidate();
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

/// `MenuRowOptions`
pub struct MenuRowOptions {
    pub primary: String,
    pub secondary: Option<String>,
    pub meta: Option<String>,
    pub selected: bool,
}

/// Port of `MenuRow`.
pub struct MenuRow {
    options: MenuRowOptions,
}

impl MenuRow {
    pub fn new(options: MenuRowOptions) -> Self {
        Self { options }
    }

    /// Port of `get selected`.
    pub fn selected(&self) -> bool {
        self.options.selected
    }

    /// Port of `renderContent`.
    pub fn render_content(&mut self, width: f64) -> Vec<String> {
        let safe_width = f64_max(ROW_PADDING_X as f64 * 2.0 + 1.0, width);
        let meta = match &self.options.meta {
            Some(meta) if !meta.is_empty() => theme().fg("muted", meta),
            _ => String::new(),
        };
        let secondary = match &self.options.secondary {
            Some(secondary) if !secondary.is_empty() => theme().fg("muted", secondary),
            _ => String::new(),
        };
        let primary_text = self.options.primary.clone();
        let primary = if self.options.selected {
            theme().bold(&theme().fg("text", &primary_text))
        } else {
            theme().fg("text", &primary_text)
        };
        let inner_width = std::cmp::max(1, safe_width.floor() as usize - ROW_PADDING_X * 2);
        let meta_width = visible_width(&meta);
        let gap = if meta.is_empty() { 0 } else { 2 };
        let primary_width = std::cmp::max(
            1,
            inner_width.saturating_sub(meta_width).saturating_sub(gap),
        );
        let primary_text = truncate_to_width(&primary, primary_width as f64, "", true);
        let primary_line = if meta.is_empty() {
            primary_text
        } else {
            format!("{primary_text}{}{meta}", " ".repeat(gap))
        };
        let mut lines: Vec<String> = Vec::new();
        lines.push(self.row_line(&primary_line, safe_width, self.options.selected));
        if !secondary.is_empty() {
            lines.push(self.row_line(
                &truncate_to_width(&secondary, inner_width as f64, "", true),
                safe_width,
                self.options.selected,
            ));
        }
        lines
    }

    /// Port of `renderPadding`.
    pub fn render_padding(&mut self, width: f64, selected: bool) -> Vec<String> {
        let safe_width = f64_max(ROW_PADDING_X as f64 * 2.0 + 1.0, width);
        let mut lines: Vec<String> = Vec::new();
        for _ in 0..ROW_PADDING_Y {
            lines.push(self.row_line("", safe_width, selected));
        }
        lines
    }

    /// Port of `rowLine`.
    fn row_line(&self, text: &str, width: f64, selected: bool) -> String {
        let background = if selected {
            Some(theme().get_selection_background_color())
        } else {
            theme().get_editor_background_color()
        };
        padded_background_line(
            text,
            width,
            ROW_PADDING_X,
            background
                .as_ref()
                .map(|f| f.as_ref() as &dyn Fn(&str) -> String),
        )
    }
}

impl Component for MenuRow {
    fn render(&mut self, width: f64) -> Vec<String> {
        let safe_width = f64_max(ROW_PADDING_X as f64 * 2.0 + 1.0, width);
        let selected = self.options.selected;
        let mut lines = self.render_padding(safe_width, selected);
        lines.extend(self.render_content(safe_width));
        lines.extend(self.render_padding(safe_width, selected));
        lines
    }

    fn invalidate(&mut self) {
        // Row render is derived from constructor options.
    }
}

/// Port of `MenuList`.
pub struct MenuList {
    options: MenuListOptions,
    pub children: Vec<MenuListChild>,
}

impl MenuList {
    pub fn new(compact: Option<Box<dyn Fn() -> bool>>) -> Self {
        Self {
            options: MenuListOptions { compact },
            children: Vec::new(),
        }
    }

    /// Port of the inherited `addChild` for a plain child.
    pub fn add_child(&mut self, component: Rc<RefCell<dyn Component>>, fills_menu_panel: bool) {
        self.children.push(MenuListChild {
            component,
            fills_menu_panel,
            row: None,
        });
    }

    /// Port of the inherited `addChild` for a [`MenuRow`] child.
    pub fn add_row(&mut self, row: Rc<RefCell<MenuRow>>) {
        self.children.push(MenuListChild {
            component: Rc::clone(&row) as Rc<RefCell<dyn Component>>,
            fills_menu_panel: true,
            row: Some(row),
        });
    }

    /// Port of the inherited `clear`.
    pub fn clear(&mut self) {
        self.children.clear();
    }

    /// Port of `isCompact`.
    pub fn is_compact(&self) -> bool {
        match &self.options.compact {
            Some(compact) => compact(),
            None => false,
        }
    }
}

impl Component for MenuList {
    fn render(&mut self, width: f64) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let compact = self.is_compact();
        for index in 0..self.children.len() {
            let is_row = self.children[index].row.is_some();
            if is_row {
                let row = self.children[index].row.clone().expect("row child");
                if compact {
                    let content = row.borrow_mut().render_content(width);
                    lines.extend(content);
                    continue;
                }
                let previous_row = if index > 0 {
                    self.children[index - 1].row.clone()
                } else {
                    None
                };
                let next_row = self
                    .children
                    .get(index + 1)
                    .and_then(|child| child.row.clone());
                let selected = row.borrow().selected();
                let previous_selected = previous_row
                    .as_ref()
                    .map(|previous| previous.borrow().selected())
                    .unwrap_or(false);
                lines.extend(
                    row.borrow_mut()
                        .render_padding(width, selected || previous_selected),
                );
                lines.extend(row.borrow_mut().render_content(width));
                if next_row.is_none() {
                    lines.extend(row.borrow_mut().render_padding(width, selected));
                }
                continue;
            }

            let fills_menu_panel = self.children[index].fills_menu_panel;
            let mut component = self.children[index].component.borrow_mut();
            let child_lines = if fills_menu_panel {
                component.render(width)
            } else {
                component.render(f64_max(1.0, width - PANEL_PADDING_X as f64 * 2.0))
            };
            drop(component);
            for line in child_lines {
                if fills_menu_panel {
                    lines.push(line);
                } else {
                    lines.push(surface_line(&line, width, PANEL_PADDING_X));
                }
            }
        }
        lines
    }

    fn invalidate(&mut self) {
        for child in self.children.iter() {
            child.component.borrow_mut().invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::interactive::theme::theme::init_theme;

    fn init() {
        init_theme(Some("prime"), false);
    }

    #[test]
    fn no_viewport_uses_the_preferred_item_count() {
        let layout = get_menu_list_layout(MenuListLayoutOptions {
            preferred_visible_items: 8,
            reserved_rows: 5,
            comfortable_item_rows: 1,
            compact_item_rows: Some(1),
            ..Default::default()
        });
        assert_eq!(
            layout,
            MenuListLayout {
                compact: false,
                visible_items: 8
            }
        );
    }

    #[test]
    fn compact_layout_wins_when_it_fits_more_items() {
        let get_rows = Rc::new(|| 12.0);
        let layout = get_menu_list_layout(MenuListLayoutOptions {
            get_rows: Some(get_rows),
            preferred_visible_items: 8,
            reserved_rows: 5,
            comfortable_item_rows: 2,
            compact_item_rows: Some(1),
            scroll_indicator_rows: Some(1),
            ..Default::default()
        });
        assert!(layout.compact);
        assert_eq!(layout.visible_items, 6);
    }

    #[test]
    fn rows_render_padding_around_content() {
        init();
        let row = Rc::new(RefCell::new(MenuRow::new(MenuRowOptions {
            primary: "Option".to_string(),
            secondary: None,
            meta: None,
            selected: true,
        })));
        let mut list = MenuList::new(Some(Box::new(|| true)));
        list.add_row(Rc::clone(&row));
        let lines = list.render(20.0);
        // compact mode renders content only
        assert_eq!(lines.len(), 1);
        assert!(visible_width(&lines[0]) == 20);
    }

    #[test]
    fn panel_pads_children_and_keeps_the_title() {
        init();
        let mut panel = MenuPanel::new(MenuPanelOptions {
            title: "Select".to_string(),
            subtitle: Some("Choose one".to_string()),
        });
        panel.add_row_child(
            Rc::new(RefCell::new(MenuRow::new(MenuRowOptions {
                primary: "a".to_string(),
                secondary: None,
                meta: None,
                selected: false,
            }))),
            true,
        );
        let lines = panel.render(20.0);
        // 1 top padding + title + subtitle + blank + 3 row lines + 1 bottom padding
        assert_eq!(lines.len(), 8);
        assert!(lines[1].contains("Select"));
        assert!(lines[2].contains("Choose one"));
    }

    #[test]
    fn inner_width_reserves_panel_padding() {
        assert_eq!(get_menu_panel_inner_width(20.0), 16);
        assert_eq!(get_menu_panel_inner_width(1.0), 1);
    }
}
