//! Port of packages/coding-agent/src/modes/interactive/components/prime-team-selector.ts
//!
//! `MenuPanel`, `MenuList`, `MenuRow`, `MenuSearchInput` and `getMenuListLayout`
//! belong to the `menu-panel.ts` slice. This module drives the shared panel
//! through the private local `MenuPanelChild` shape defined in
//! `heartbeat_manager.rs` and a private local renderer, so it never appends to
//! another slice's file. See `blocked_on` in the slice status entry.

use std::cell::RefCell;
use std::rc::Rc;

use pi_tui::components::input::Input;
use pi_tui::components::truncated_text::TruncatedText;
use pi_tui::fuzzy::fuzzy_filter;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::{Component, Container, Focusable};

use crate::core::prime_inference_auth::PrimeTeam;

use super::super::theme::theme::theme;
use super::heartbeat_manager::{
    get_menu_list_layout, LocalMenuPanelRenderer, MenuList, MenuListItem, MenuListLayout,
    MenuListLayoutOptions, MenuPanel, MenuPanelApi, MenuPanelChild, MenuRow,
};

type PrimeTeamOptionType = &'static str;
const OPTION_PERSONAL: PrimeTeamOptionType = "personal";
const OPTION_TEAM: PrimeTeamOptionType = "team";

/// `type PrimeTeamOption`.
#[derive(Debug, Clone, PartialEq)]
pub struct PrimeTeamOption {
    pub type_: PrimeTeamOptionType,
    pub team: Option<PrimeTeam>,
}

/// `MenuViewportProvider` (menu-panel.ts).
#[derive(Clone, Default)]
pub struct MenuViewportProvider {
    pub get_rows: Option<Rc<dyn Fn() -> f64>>,
}

const PREFERRED_VISIBLE_TEAMS: usize = 8;
const TEAM_LIST_RESERVED_ROWS: usize = 7;
const TEAM_SCROLL_INDICATOR_ROWS: usize = 1;

/// Port of `MenuSearchInput` (menu-panel.ts) as used by this component.
pub struct MenuSearchInput {
    input: Input,
    placeholder: String,
}

impl MenuSearchInput {
    pub fn new(placeholder: &str) -> Self {
        Self {
            input: Input::new(),
            placeholder: placeholder.to_string(),
        }
    }

    pub fn get_value(&self) -> String {
        self.input.get_value().to_string()
    }

    pub fn set_value(&mut self, value: &str) {
        self.input.set_value(value.to_string());
    }

    pub fn get_cursor(&self) -> usize {
        self.input.get_cursor()
    }

    pub fn handle_input(&mut self, data: &str) {
        Component::handle_input(&mut self.input, data);
    }

    pub fn on_submit(&mut self, handler: Box<dyn FnMut(&str)>) {
        self.input.on_submit = Some(handler);
    }

    pub fn focused(&self) -> bool {
        Focusable::focused(&self.input)
    }

    /// The component keeps the placeholder for the unfocused render pass.
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }
}

/// Port of `PrimeTeamSelectorComponent`.
pub struct PrimeTeamSelectorComponent {
    container: Container,
    search_input: MenuSearchInput,
    list_container: MenuList,
    all_options: Vec<PrimeTeamOption>,
    filtered_options: Vec<PrimeTeamOption>,
    selected_index: usize,
    search_query: String,
    focused: bool,
    list_layout: MenuListLayout,
    current_team_id: Option<String>,
    on_select: Box<dyn FnMut(Option<PrimeTeam>)>,
    on_cancel: Box<dyn FnMut()>,
    viewport: MenuViewportProvider,
}

impl PrimeTeamSelectorComponent {
    pub fn new(
        teams: &[PrimeTeam],
        current_team_id: Option<String>,
        on_select: Box<dyn FnMut(Option<PrimeTeam>)>,
        on_cancel: Box<dyn FnMut()>,
        viewport: MenuViewportProvider,
    ) -> Self {
        let mut all_options: Vec<PrimeTeamOption> = vec![PrimeTeamOption {
            type_: OPTION_PERSONAL,
            team: None,
        }];
        all_options.extend(teams.iter().cloned().map(|team| PrimeTeamOption {
            type_: OPTION_TEAM,
            team: Some(team),
        }));

        let list_layout = get_menu_list_layout(MenuListLayoutOptions {
            get_rows: None,
            preferred_visible_items: PREFERRED_VISIBLE_TEAMS,
            min_visible_items: None,
            total_items: None,
            reserved_rows: TEAM_LIST_RESERVED_ROWS,
            comfortable_item_rows: 3,
            compact_item_rows: Some(2),
            scroll_indicator_rows: Some(TEAM_SCROLL_INDICATOR_ROWS),
            comfortable_list_padding_rows: None,
            compact_list_padding_rows: None,
        });

        let search_input = MenuSearchInput::new("Search teams");
        let mut component = Self {
            container: Container::new(),
            search_input,
            list_container: MenuList::new(list_layout.compact),
            all_options: all_options.clone(),
            filtered_options: all_options,
            selected_index: 0,
            search_query: String::new(),
            focused: false,
            list_layout,
            current_team_id,
            on_select,
            on_cancel,
            viewport,
        };
        component.search_input.on_submit(Box::new(|_value: &str| {
            // `this.filteredOptions[this.selectedIndex]` is read from the
            // component; the submitted value is handled in `handle_input`.
        }));
        component.filter_options("");
        component
    }

    /// Port of `filterOptions(query)`.
    pub fn filter_options(&mut self, query: &str) {
        let query_changed = query != self.search_query;
        self.search_query = query.to_string();
        self.filtered_options = if !query.is_empty() {
            let all = self.all_options.clone();
            fuzzy_filter(&all, query, &|option: &PrimeTeamOption| {
                self.get_search_text(option)
            })
        } else {
            self.all_options.clone()
        };
        self.selected_index = if query_changed {
            0
        } else {
            self.selected_index
                .min(self.filtered_options.len().saturating_sub(1))
        };
        self.update_list();
    }

    fn get_search_text(&self, option: &PrimeTeamOption) -> String {
        if option.type_ == OPTION_PERSONAL {
            return "personal account".to_string();
        }
        match &option.team {
            Some(team) => format!(
                "{} {} {} {}",
                team.name,
                team.slug.clone().unwrap_or_default(),
                team.role.clone().unwrap_or_default(),
                team.team_id
            ),
            None => String::new(),
        }
    }

    /// Port of `render(width)`.
    pub fn render(&mut self, width: f64) -> Vec<String> {
        let previous_layout = self.list_layout;
        self.update_layout();
        if self.list_layout.compact != previous_layout.compact
            || self.list_layout.visible_items != previous_layout.visible_items
        {
            self.update_list();
        }
        let mut panel = self.build_panel();
        let mut renderer = LocalMenuPanelRenderer;
        renderer.render_panel(&mut panel, width.max(1.0) as usize)
    }

    fn build_panel(&self) -> MenuPanel {
        let mut panel = MenuPanel::new(
            "Prime Team",
            Some("Choose which account pays for Prime Inference usage."),
        );
        panel.add_child(MenuPanelChild::Text(self.search_input.get_value()));
        panel.add_child(MenuPanelChild::Spacer(1));
        panel.add_child(MenuPanelChild::List(self.list_container_rows()));
        panel
    }

    /// Snapshot of the list container rows for the panel render. The panel owns
    /// the rows, so the port rebuilds the same row set from the filtered options.
    fn list_container_rows(&self) -> MenuList {
        let mut list = MenuList::new(self.list_layout.compact);
        let max_visible = self.list_layout.visible_items;
        let start_index = self
            .selected_index
            .saturating_sub(max_visible / 2)
            .min(self.filtered_options.len().saturating_sub(max_visible));
        let end_index = (start_index + max_visible).min(self.filtered_options.len());
        for i in start_index..end_index {
            let Some(option) = self.filtered_options.get(i) else {
                continue;
            };
            list.add_row(MenuRow::new(
                &self.get_primary(option),
                Some(&self.get_secondary(option)),
                Some(&self.get_meta(option)),
                i == self.selected_index,
            ));
        }
        if start_index > 0 || end_index < self.filtered_options.len() {
            list.add_child(Box::new(TruncatedText::new(
                theme().fg(
                    "muted",
                    &format!(
                        "  ({}/{})",
                        self.selected_index + 1,
                        self.filtered_options.len()
                    ),
                ),
                1,
                0,
            )));
        }
        if self.filtered_options.is_empty() {
            list.add_child(Box::new(TruncatedText::new(
                theme().fg("muted", "No matching teams"),
                1,
                0,
            )));
        }
        list
    }

    /// Port of `updateList()`.
    fn update_list(&mut self) {
        self.update_layout();
        self.list_container = MenuList::new(self.list_layout.compact);

        let max_visible = self.list_layout.visible_items;
        let start_index = self
            .selected_index
            .saturating_sub(max_visible / 2)
            .min(self.filtered_options.len().saturating_sub(max_visible));
        let end_index = (start_index + max_visible).min(self.filtered_options.len());

        for i in start_index..end_index {
            let Some(option) = self.filtered_options.get(i) else {
                continue;
            };
            self.list_container.add_row(MenuRow::new(
                &self.get_primary(option),
                Some(&self.get_secondary(option)),
                Some(&self.get_meta(option)),
                i == self.selected_index,
            ));
        }

        if start_index > 0 || end_index < self.filtered_options.len() {
            self.list_container.add_child(Box::new(TruncatedText::new(
                theme().fg(
                    "muted",
                    &format!(
                        "  ({}/{})",
                        self.selected_index + 1,
                        self.filtered_options.len()
                    ),
                ),
                1,
                0,
            )));
        }

        if self.filtered_options.is_empty() {
            self.list_container.add_child(Box::new(TruncatedText::new(
                theme().fg("muted", "No matching teams"),
                1,
                0,
            )));
        }
    }

    fn get_primary(&self, option: &PrimeTeamOption) -> String {
        match &option.team {
            Some(team) => team.name.clone(),
            None => "Personal".to_string(),
        }
    }

    fn get_secondary(&self, option: &PrimeTeamOption) -> String {
        let Some(team) = &option.team else {
            return "personal account".to_string();
        };
        let role = team
            .role
            .clone()
            .unwrap_or_else(|| "member".to_string())
            .to_lowercase();
        match &team.slug {
            Some(slug) => format!("slug: {slug}, role: {role}"),
            None => format!("role: {role}"),
        }
    }

    fn get_meta(&self, option: &PrimeTeamOption) -> String {
        let is_current = match &option.team {
            Some(team) => Some(&team.team_id) == self.current_team_id.as_ref(),
            None => self.current_team_id.is_none(),
        };
        if is_current {
            theme().fg("success", "current")
        } else {
            String::new()
        }
    }

    /// Port of `handleInput(keyData)`.
    pub fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.up") {
            if self.filtered_options.is_empty() {
                return;
            }
            self.selected_index = self.selected_index.saturating_sub(1);
            self.update_list();
        } else if kb.matches(key_data, "tui.select.down") {
            if self.filtered_options.is_empty() {
                return;
            }
            self.selected_index = (self.selected_index + 1).min(self.filtered_options.len() - 1);
            self.update_list();
        } else if kb.matches(key_data, "tui.select.confirm") {
            let selected = self.filtered_options.get(self.selected_index).cloned();
            if let Some(selected) = selected {
                (self.on_select)(selected.team);
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.on_cancel)();
        } else {
            self.search_input.handle_input(key_data);
            let value = self.search_input.get_value();
            self.filter_options(&value);
        }
    }

    fn update_layout(&mut self) {
        let get_rows = self.viewport.get_rows.clone();
        self.list_layout = get_menu_list_layout(MenuListLayoutOptions {
            get_rows,
            preferred_visible_items: PREFERRED_VISIBLE_TEAMS,
            min_visible_items: None,
            total_items: Some(self.filtered_options.len()),
            reserved_rows: TEAM_LIST_RESERVED_ROWS,
            comfortable_item_rows: 3,
            compact_item_rows: Some(2),
            scroll_indicator_rows: Some(TEAM_SCROLL_INDICATOR_ROWS),
            comfortable_list_padding_rows: None,
            compact_list_padding_rows: None,
        });
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn filtered_options(&self) -> &[PrimeTeamOption] {
        &self.filtered_options
    }
}

impl Default for PrimeTeamOption {
    fn default() -> Self {
        Self {
            type_: OPTION_PERSONAL,
            team: None,
        }
    }
}

impl Focusable for PrimeTeamSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        Focusable::set_focused(&mut self.search_input.input, focused);
    }
}

impl Component for PrimeTeamSelectorComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        PrimeTeamSelectorComponent::render(self, width)
    }

    fn handle_input(&mut self, data: &str) {
        PrimeTeamSelectorComponent::handle_input(self, data);
    }

    fn invalidate(&mut self) {
        Component::invalidate(&mut self.container);
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}
