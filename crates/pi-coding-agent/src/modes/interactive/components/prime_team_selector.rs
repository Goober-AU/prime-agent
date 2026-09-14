//! Port of packages/coding-agent/src/modes/interactive/components/prime-team-selector.ts

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use pi_tui::components::spacer::Spacer;
use pi_tui::components::truncated_text::TruncatedText;
use pi_tui::fuzzy::fuzzy_filter;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::{Component, Container, Focusable};

use crate::core::prime_inference_auth::PrimeTeam;

use super::super::theme::theme::theme;
use super::menu_panel::{
    get_menu_list_layout, MenuList, MenuListLayout, MenuListLayoutOptions, MenuPanel,
    MenuPanelOptions, MenuRow, MenuRowOptions, MenuSearchInput, MenuViewportProvider,
};

type PrimeTeamOptionType = &'static str;
const OPTION_PERSONAL: PrimeTeamOptionType = "personal";
const OPTION_TEAM: PrimeTeamOptionType = "team";

const PREFERRED_VISIBLE_TEAMS: usize = 8;
const TEAM_LIST_RESERVED_ROWS: usize = 7;
const TEAM_SCROLL_INDICATOR_ROWS: usize = 1;

/// `type PrimeTeamOption`.
#[derive(Debug, Clone, PartialEq)]
pub struct PrimeTeamOption {
    pub type_: PrimeTeamOptionType,
    pub team: Option<PrimeTeam>,
}

/// Port of `PrimeTeamSelectorComponent`.
pub struct PrimeTeamSelectorComponent {
    container: Container,
    search_input: Rc<RefCell<MenuSearchInput>>,
    list_container: Rc<RefCell<MenuList>>,
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
    /// `new MenuList({ compact: () => this.listLayout.compact })`: the list reads
    /// the component's current layout on every render, so the flag is shared.
    compact_flag: Rc<Cell<bool>>,
    /// `this.searchInput.onSubmit = () => {... this.onSelect(...) }`.
    ///
    /// The submit handler cannot borrow the component, so it records the request
    /// and `handle_input` performs the same selection right after the search
    /// input consumed the key.
    confirm_request: Rc<Cell<bool>>,
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
        let filtered_options = all_options.clone();

        let mut panel = MenuPanel::new(MenuPanelOptions {
            title: "Prime Team".to_string(),
            subtitle: Some("Choose which account pays for Prime Inference usage.".to_string()),
        });

        let confirm_request: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let mut search_input = MenuSearchInput::new("Search teams".to_string());
        {
            let request = Rc::clone(&confirm_request);
            search_input.set_on_submit(Some(Box::new(move |_value: &str| request.set(true))));
        }
        let search_input = Rc::new(RefCell::new(search_input));
        panel.add_full_width_child(Rc::clone(&search_input) as Rc<RefCell<dyn Component>>);
        panel.add_child(Rc::new(RefCell::new(Spacer::new(1))));

        let list_layout = get_menu_list_layout(MenuListLayoutOptions {
            preferred_visible_items: PREFERRED_VISIBLE_TEAMS,
            reserved_rows: TEAM_LIST_RESERVED_ROWS,
            comfortable_item_rows: 3,
            compact_item_rows: Some(2),
            ..Default::default()
        });
        let compact_flag = Rc::new(Cell::new(list_layout.compact));
        let list_container = Rc::new(RefCell::new(MenuList::new({
            let compact_flag = Rc::clone(&compact_flag);
            Some(Box::new(move || compact_flag.get()))
        })));
        panel.add_full_width_child(Rc::clone(&list_container) as Rc<RefCell<dyn Component>>);

        let mut container = Container::new();
        container.add_child(Rc::new(RefCell::new(panel)) as Rc<RefCell<dyn Component>>);

        let mut component = Self {
            container,
            search_input,
            list_container,
            all_options,
            filtered_options,
            selected_index: 0,
            search_query: String::new(),
            focused: false,
            list_layout,
            current_team_id,
            on_select,
            on_cancel,
            viewport,
            compact_flag,
            confirm_request,
        };
        component.filter_options("");
        component
    }

    /// Port of `filterOptions(query)`.
    pub fn filter_options(&mut self, query: &str) {
        let query_changed = query != self.search_query;
        self.search_query = query.to_string();
        self.filtered_options = if query.is_empty() {
            self.all_options.clone()
        } else {
            let all = self.all_options.clone();
            fuzzy_filter(&all, query, &|option: &PrimeTeamOption| {
                Self::get_search_text(option)
            })
        };
        let last_index = self.filtered_options.len().saturating_sub(1);
        self.selected_index = if query_changed {
            0
        } else {
            self.selected_index.min(last_index)
        };
        self.update_list();
    }

    /// Port of `getSearchText(option)`.
    fn get_search_text(option: &PrimeTeamOption) -> String {
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
        Component::render(&mut self.container, width)
    }

    /// Port of `updateList()`.
    fn update_list(&mut self) {
        self.update_layout();
        let max_visible = self.list_layout.visible_items;
        let start_index = self
            .selected_index
            .saturating_sub(max_visible / 2)
            .min(self.filtered_options.len().saturating_sub(max_visible));
        let end_index = (start_index + max_visible).min(self.filtered_options.len());
        let rows: Vec<MenuRow> = self.filtered_options[start_index..end_index]
            .iter()
            .enumerate()
            .map(|(offset, option)| {
                MenuRow::new(MenuRowOptions {
                    primary: self.get_primary(option),
                    secondary: Some(self.get_secondary(option)),
                    meta: Some(self.get_meta(option)),
                    selected: start_index + offset == self.selected_index,
                })
            })
            .collect();
        let scroll_indicator = if start_index > 0 || end_index < self.filtered_options.len() {
            Some(theme().fg(
                "muted",
                &format!(
                    "  ({}/{})",
                    self.selected_index + 1,
                    self.filtered_options.len()
                ),
            ))
        } else {
            None
        };
        let empty_state = self.filtered_options.is_empty();

        let mut list = self.list_container.borrow_mut();
        list.clear();
        for row in rows {
            list.add_row(Rc::new(RefCell::new(row)));
        }
        if let Some(scroll_indicator) = scroll_indicator {
            list.add_child(
                Rc::new(RefCell::new(TruncatedText::new(scroll_indicator, 1, 0))),
                false,
            );
        }
        if empty_state {
            list.add_child(
                Rc::new(RefCell::new(TruncatedText::new(
                    theme().fg("muted", "No matching teams"),
                    1,
                    0,
                ))),
                false,
            );
        }
    }

    /// Port of `getPrimary(option)`.
    fn get_primary(&self, option: &PrimeTeamOption) -> String {
        match &option.team {
            Some(team) => team.name.clone(),
            None => "Personal".to_string(),
        }
    }

    /// Port of `getSecondary(option)`.
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

    /// Port of `getMeta(option)`.
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
            self.confirm_selection();
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.on_cancel)();
        } else {
            Component::handle_input(&mut *self.search_input.borrow_mut(), key_data);
            if self.confirm_request.replace(false) {
                // `this.searchInput.onSubmit` selects the current option.
                self.confirm_selection();
            }
            let value = self.search_input.borrow().get_value();
            self.filter_options(&value);
        }
    }

    /// The shared body of `tui.select.confirm` and `searchInput.onSubmit`.
    fn confirm_selection(&mut self) {
        let selected = self.filtered_options.get(self.selected_index).cloned();
        if let Some(selected) = selected {
            (self.on_select)(selected.team);
        }
    }

    /// Port of `updateLayout()`.
    fn update_layout(&mut self) {
        self.list_layout = self.compute_layout();
        self.compact_flag.set(self.list_layout.compact);
    }

    fn compute_layout(&self) -> MenuListLayout {
        get_menu_list_layout(MenuListLayoutOptions {
            get_rows: self.viewport.get_rows.clone(),
            preferred_visible_items: PREFERRED_VISIBLE_TEAMS,
            total_items: Some(self.filtered_options.len()),
            reserved_rows: TEAM_LIST_RESERVED_ROWS,
            comfortable_item_rows: 3,
            compact_item_rows: Some(2),
            scroll_indicator_rows: Some(TEAM_SCROLL_INDICATOR_ROWS),
            ..Default::default()
        })
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
        Focusable::set_focused(&mut *self.search_input.borrow_mut(), focused);
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
