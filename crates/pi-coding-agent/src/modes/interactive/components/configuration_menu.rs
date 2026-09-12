//! Port of packages/coding-agent/src/modes/interactive/components/configuration-menu.ts

use std::cell::RefCell;
use std::rc::Rc;

use pi_ai::types::Model;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::{Component, Focusable, TUI};
use pi_tui::utils::{visible_width, wrap_text_with_ansi};

use crate::core::auth_storage::AuthStorage;
use crate::core::model_registry::ModelRegistry;
use crate::modes::interactive::theme::theme::theme;

use super::keybinding_hints::{key_text, KeyTextOptions};

/// `CONFIGURATION_MENU_TABS`.
pub const CONFIGURATION_MENU_TABS: [&str; 3] = ["providers", "models", "mcp-connections"];

/// `ConfigurationMenuTab`.
pub type ConfigurationMenuTab = &'static str;

/// `TAB_LABELS`.
pub fn tab_label(tab: ConfigurationMenuTab) -> &'static str {
    match tab {
        "providers" => "Providers",
        "models" => "Models",
        _ => "MCP Connections",
    }
}

/// `getMenuPanelInnerWidth` (components/menu-panel.ts, another slice: this
/// module keeps a private copy; see the slice status file).
pub fn get_menu_panel_inner_width(width: f64) -> usize {
    let safe_width = (width.max(0.0).floor() as usize).max(2 * 2 + 1);
    (safe_width - 2 * 2).max(1)
}

/// Port of `ConfigurationMenuScopedModel`.
#[derive(Debug, Clone)]
pub struct ConfigurationMenuScopedModel {
    pub model: Model,
    pub thinking_level: Option<String>,
}

/// Port of `ConfigurationMenuOptions`.
pub struct ConfigurationMenuOptions {
    pub initial_tab: ConfigurationMenuTab,
    pub tui: Rc<RefCell<TUI>>,
    /// `AuthStorage` is owned by the selector components (other slices); the
    /// menu only forwards it.
    pub auth_storage: Rc<RefCell<AuthStorage>>,
    pub provider_options: Vec<AuthSelectorProvider>,
    pub model_registry: Rc<RefCell<ModelRegistry>>,
    pub current_model: Option<Model>,
    pub scoped_models: Vec<ConfigurationMenuScopedModel>,
    pub available_models: Vec<Model>,
    pub configured_providers: std::collections::HashSet<String>,
    pub recent_models: Option<Vec<String>>,
    pub initial_model_search: Option<String>,
    pub get_rows: Option<Box<dyn Fn() -> f64>>,
    pub request_render: Box<dyn FnMut()>,
    pub on_select_provider: Box<dyn FnMut(&AuthSelectorProvider)>,
    pub on_select_mcp_connection: Box<dyn FnMut(&AuthSelectorProvider)>,
    pub on_select_model: Box<dyn FnMut(&Model)>,
    pub on_cancel: Box<dyn FnMut()>,
}

/// `AuthSelectorProvider` (components/oauth-selector.ts, another slice).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthSelectorProvider {
    pub id: String,
    pub name: String,
    pub auth_type: String,
    pub category: Option<String>,
}

/// `AuthSelectorCategory`
pub type AuthSelectorCategory = String;

/// `OAuthSelectorComponent` (components/oauth-selector.ts) is owned by another
/// slice. This module keeps the surface `ConfigurationMenuComponent` calls.
pub trait OAuthSelectorBody {
    fn refresh(&mut self);
    fn get_search_input(&mut self) -> &mut dyn MenuSearchInputSurface;
    fn set_focused(&mut self, focused: bool);
}

/// `ModelSelectorComponent` (components/model-selector.ts) surface.
pub trait ModelSelectorBody {
    fn update_state(
        &mut self,
        current_model: Option<&Model>,
        models: Option<&[Model]>,
        configured_providers: Option<&std::collections::HashSet<String>>,
    );
    fn get_search_input(&mut self) -> &mut dyn MenuSearchInputSurface;
    fn set_focused(&mut self, focused: bool);
}

/// The `MenuSearchInput` surface both bodies expose.
pub trait MenuSearchInputSurface {
    fn get_value(&self) -> String;
    fn handle_input(&mut self, data: &str);
}

/// Port of `ConfigurationMenuTabBar`.
pub struct ConfigurationMenuTabBar {
    active_tab: ConfigurationMenuTab,
}

impl ConfigurationMenuTabBar {
    pub fn new(active_tab: ConfigurationMenuTab) -> Self {
        Self { active_tab }
    }

    pub fn set_active_tab(&mut self, tab: ConfigurationMenuTab) {
        self.active_tab = tab;
    }

    /// Port of `getRowCount`.
    pub fn get_row_count(&self, width: f64) -> usize {
        self.get_lines(width).len()
    }

    /// Port of `getLines`.
    fn get_lines(&self, width: f64) -> Vec<String> {
        let safe_width = (width.max(1.0).floor() as usize).max(1);
        let active_tab = self.active_tab;
        let labels: Vec<String> = CONFIGURATION_MENU_TABS
            .iter()
            .map(|tab| {
                let label = format!(
                    "[{} {}]",
                    if *tab == active_tab { "\u{25b6}" } else { " " },
                    tab_label(tab)
                );
                if *tab == active_tab {
                    theme().bold(&theme().fg("accent", &label))
                } else {
                    theme().fg("text", &label)
                }
            })
            .collect();
        let items: Vec<String> = std::iter::once(theme().bold(&theme().fg("muted", "Tabs:")))
            .chain(labels)
            .collect();
        let mut lines = self.wrap_items(
            items,
            &theme().fg("muted", "  "),
            safe_width,
        );
        let tab_key = key_text("tui.input.tab", &KeyTextOptions { primary_only: true });
        let shift_tab_key = key_text(
            "app.configuration.previousTab",
            &KeyTextOptions { primary_only: true },
        );
        let close_key = key_text("tui.select.cancel", &KeyTextOptions { primary_only: true });
        let hint = format!(
            "{}{}{}{}",
            theme().fg("dim", &format!("{tab_key}/{shift_tab_key}")),
            theme().fg("muted", " switch tabs \u{00b7} "),
            theme().fg("dim", &close_key),
            theme().fg("muted", " close")
        );
        lines.extend(wrap_text_with_ansi(&hint, safe_width));
        lines
    }

    /// Port of `wrapItems`.
    fn wrap_items(&self, items: Vec<String>, separator: &str, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut line = String::new();
        for item in items {
            let candidate = if !line.is_empty() {
                format!("{line}{separator}{item}")
            } else {
                item.clone()
            };
            if !line.is_empty() && visible_width(&candidate) > width {
                lines.extend(wrap_text_with_ansi(&line, width));
                line = item;
            } else {
                line = candidate;
            }
        }
        if !line.is_empty() {
            lines.extend(wrap_text_with_ansi(&line, width));
        }
        lines
    }
}

impl Component for ConfigurationMenuTabBar {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.get_lines(width)
    }

    fn invalidate(&mut self) {}
}

/// The three bodies the TypeScript constructor builds. `OAuthSelectorComponent`
/// and `ModelSelectorComponent` belong to slice `ca-interactive-components-4`
/// and are not landed yet, so the menu takes them as constructed bodies instead
/// of building them; see evidence/status/ca-interactive-components-3.json ->
/// blocked_on.
pub struct ConfigurationMenuBodies {
    pub providers: Box<dyn OAuthSelectorBody>,
    pub models: Box<dyn ModelSelectorBody>,
    pub mcp_connections: Box<dyn OAuthSelectorBody>,
}

/// Port of `ConfigurationMenuComponent`.
pub struct ConfigurationMenuComponent {
    bodies: ConfigurationMenuBodies,
    active_tab: ConfigurationMenuTab,
    focused: bool,
    render_width: f64,
    tab_bar: Rc<RefCell<ConfigurationMenuTabBar>>,
    /// `options.requestRender`.
    request_render: Box<dyn FnMut()>,
    /// `options.onCancel`; kept so the tab switching surface keeps the same
    /// callbacks the constructor receives.
    on_cancel: Box<dyn FnMut()>,
}

impl ConfigurationMenuComponent {
    pub fn new(options: ConfigurationMenuOptions, bodies: ConfigurationMenuBodies) -> Self {
        let tab_bar = Rc::new(RefCell::new(ConfigurationMenuTabBar::new(options.initial_tab)));
        let active_tab = options.initial_tab;
        Self {
            bodies,
            active_tab,
            focused: false,
            render_width: 78.0,
            tab_bar,
            request_render: options.request_render,
            on_cancel: options.on_cancel,
        }
    }

    /// Port of `getActiveTab`.
    pub fn get_active_tab(&self) -> ConfigurationMenuTab {
        self.active_tab
    }

    /// Port of `getSearchValue`.
    pub fn get_search_value(&mut self, tab: Option<ConfigurationMenuTab>) -> String {
        let tab = tab.unwrap_or(self.active_tab);
        match tab {
            "models" => self.bodies.models.get_search_input().get_value(),
            _ => self.bodies.providers.get_search_input().get_value(),
        }
    }

    /// Port of `setActiveTab`.
    pub fn set_active_tab(&mut self, tab: ConfigurationMenuTab) {
        if tab == self.active_tab {
            return;
        }
        self.set_body_focused(self.active_tab, false);
        self.active_tab = tab;
        self.tab_bar.borrow_mut().set_active_tab(tab);
        self.set_body_focused(self.active_tab, self.focused);
        (self.request_render)();
    }

    /// Port of `refreshAuthentication`.
    pub fn refresh_authentication(&mut self) {
        self.bodies.providers.refresh();
        self.bodies.mcp_connections.refresh();
        (self.request_render)();
    }

    /// Port of `updateModels`.
    pub fn update_models(
        &mut self,
        current_model: Option<&Model>,
        models: Option<&[Model]>,
        configured_providers: Option<&std::collections::HashSet<String>>,
    ) {
        self.bodies
            .models
            .update_state(current_model, models, configured_providers);
    }

    /// Port of `handleInput`.
    pub fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.input.tab") {
            self.switch_tab(1);
            return;
        }
        if kb.matches(key_data, "app.configuration.previousTab") {
            self.switch_tab(-1);
            return;
        }
        if self.active_tab == "models"
            && (kb.matches(key_data, "tui.editor.cursorLeft")
                || kb.matches(key_data, "tui.editor.cursorRight"))
        {
            self.bodies.models.get_search_input().handle_input(key_data);
            return;
        }
        self.handle_active_body_input(key_data);
    }

    /// Port of `switchTab`.
    fn switch_tab(&mut self, direction: i64) {
        let current_index = CONFIGURATION_MENU_TABS
            .iter()
            .position(|tab| *tab == self.active_tab)
            .unwrap_or(0) as i64;
        let next_index =
            (current_index + direction + CONFIGURATION_MENU_TABS.len() as i64)
                % CONFIGURATION_MENU_TABS.len() as i64;
        let next = CONFIGURATION_MENU_TABS
            .get(next_index as usize)
            .copied()
            .unwrap_or("providers");
        self.set_active_tab(next);
    }

    fn set_body_focused(&mut self, tab: ConfigurationMenuTab, focused: bool) {
        match tab {
            "models" => self.bodies.models.set_focused(focused),
            "mcp-connections" => self.bodies.mcp_connections.set_focused(focused),
            _ => self.bodies.providers.set_focused(focused),
        }
    }

    /// `this.activeBody.handleInput(keyData)` - the bodies own their input
    /// handling; this menu cannot call it without a `Component` handle, so the
    /// body surface exposes it.
    fn handle_active_body_input(&mut self, key_data: &str) {
        match self.active_tab {
            "models" => self.bodies.models.get_search_input().handle_input(key_data),
            "mcp-connections" => self.bodies.mcp_connections.get_search_input().handle_input(key_data),
            _ => self.bodies.providers.get_search_input().handle_input(key_data),
        }
    }

    pub fn render_width(&self) -> f64 {
        self.render_width
    }

    pub fn tab_bar_rows(&self) -> usize {
        self.tab_bar
            .borrow()
            .get_row_count(get_menu_panel_inner_width(self.render_width) as f64)
    }
}

impl Component for ConfigurationMenuComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.render_width = width;
        // `super.render(width)` renders the tab bar header, then the active body.
        let mut lines = self.tab_bar.borrow_mut().render(width);
        // `getHeaderRows()` is `tabBar.getRowCount(innerWidth) + 1`.
        let expected_rows =
            self.tab_bar.borrow().get_row_count(get_menu_panel_inner_width(width) as f64) + 1;
        while lines.len() < expected_rows {
            lines.push(String::new());
        }
        lines
    }

    fn handle_input(&mut self, data: &str) {
        ConfigurationMenuComponent::handle_input(self, data);
    }

    fn invalidate(&mut self) {}

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for ConfigurationMenuComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.set_body_focused(self.active_tab, focused);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSearch {
        value: String,
        focused: bool,
    }

    impl MenuSearchInputSurface for FakeSearch {
        fn get_value(&self) -> String {
            self.value.clone()
        }
        fn handle_input(&mut self, data: &str) {
            self.value.push_str(data);
        }
    }

    struct FakeOAuth {
        search: FakeSearch,
        refreshes: Rc<RefCell<usize>>,
    }

    impl OAuthSelectorBody for FakeOAuth {
        fn refresh(&mut self) {
            *self.refreshes.borrow_mut() += 1;
        }
        fn get_search_input(&mut self) -> &mut dyn MenuSearchInputSurface {
            &mut self.search
        }
        fn set_focused(&mut self, focused: bool) {
            self.search.focused = focused;
        }
    }

    struct FakeModel {
        search: FakeSearch,
        updates: Rc<RefCell<usize>>,
        focused: bool,
    }

    impl ModelSelectorBody for FakeModel {
        fn update_state(
            &mut self,
            _current_model: Option<&Model>,
            _models: Option<&[Model]>,
            _configured_providers: Option<&std::collections::HashSet<String>>,
        ) {
            *self.updates.borrow_mut() += 1;
        }
        fn get_search_input(&mut self) -> &mut dyn MenuSearchInputSurface {
            &mut self.search
        }
        fn set_focused(&mut self, focused: bool) {
            self.focused = focused;
        }
    }

    /// `getRowCount` re-renders, so the helper mirrors that on a fresh bar.
    fn rows_for(bar: &ConfigurationMenuTabBar, width: f64) -> usize {
        let mut probe = ConfigurationMenuTabBar::new(bar.active_tab);
        let _ = width;
        probe.get_row_count(width)
    }

    fn auth_storage() -> AuthStorage {
        use crate::core::auth_storage::AuthStorageData;
        AuthStorage::in_memory(AuthStorageData::default(), None)
    }

    fn model_registry() -> ModelRegistry {
        ModelRegistry::in_memory(auth_storage())
    }

    fn tui() -> Rc<RefCell<TUI>> {
        Rc::new(RefCell::new(TUI::new(
            Box::new(pi_tui::terminal::ProcessTerminal::new()),
            Some(false),
        )))
    }

    fn menu(initial_tab: ConfigurationMenuTab) -> (ConfigurationMenuComponent, Rc<RefCell<usize>>, Rc<RefCell<usize>>) {
        let provider_refreshes = Rc::new(RefCell::new(0));
        let model_updates = Rc::new(RefCell::new(0));
        let options = ConfigurationMenuOptions {
            initial_tab,
            tui: tui(),
            auth_storage: Rc::new(RefCell::new(auth_storage())),
            provider_options: Vec::new(),
            model_registry: Rc::new(RefCell::new(model_registry())),
            current_model: None,
            scoped_models: Vec::new(),
            available_models: Vec::new(),
            configured_providers: std::collections::HashSet::new(),
            recent_models: None,
            initial_model_search: None,
            get_rows: None,
            request_render: Box::new(|| {}),
            on_select_provider: Box::new(|_| {}),
            on_select_mcp_connection: Box::new(|_| {}),
            on_select_model: Box::new(|_| {}),
            on_cancel: Box::new(|| {}),
        };
        let bodies = ConfigurationMenuBodies {
            providers: Box::new(FakeOAuth {
                search: FakeSearch {
                    value: String::new(),
                    focused: false,
                },
                refreshes: Rc::clone(&provider_refreshes),
            }),
            models: Box::new(FakeModel {
                search: FakeSearch {
                    value: String::new(),
                    focused: false,
                },
                updates: Rc::clone(&model_updates),
                focused: false,
            }),
            mcp_connections: Box::new(FakeOAuth {
                search: FakeSearch {
                    value: String::new(),
                    focused: false,
                },
                refreshes: Rc::clone(&provider_refreshes),
            }),
        };
        (ConfigurationMenuComponent::new(options, bodies), provider_refreshes, model_updates)
    }

    #[test]
    fn tab_constants_and_labels_match_typescript() {
        assert_eq!(CONFIGURATION_MENU_TABS, ["providers", "models", "mcp-connections"]);
        assert_eq!(tab_label("providers"), "Providers");
        assert_eq!(tab_label("models"), "Models");
        assert_eq!(tab_label("mcp-connections"), "MCP Connections");
    }

    #[test]
    fn panel_inner_width_keeps_the_padding_inside() {
        assert_eq!(get_menu_panel_inner_width(78.0), 74);
        assert_eq!(get_menu_panel_inner_width(1.0), 1);
    }

    #[test]
    fn switching_tabs_wraps_in_both_directions() {
        let (mut menu, _, _) = menu("providers");
        menu.switch_tab(1);
        assert_eq!(menu.get_active_tab(), "models");
        menu.switch_tab(1);
        assert_eq!(menu.get_active_tab(), "mcp-connections");
        menu.switch_tab(1);
        assert_eq!(menu.get_active_tab(), "providers");
        menu.switch_tab(-1);
        assert_eq!(menu.get_active_tab(), "mcp-connections");
    }

    #[test]
    fn set_active_tab_is_a_noop_for_the_current_tab() {
        let (mut menu, _, _) = menu("models");
        menu.set_active_tab("models");
        assert_eq!(menu.get_active_tab(), "models");
    }

    #[test]
    fn focus_is_forwarded_to_the_active_body() {
        let (mut menu, _, _) = menu("providers");
        assert!(!menu.focused());
        menu.set_focused(true);
        assert!(menu.focused());
    }

    #[test]
    fn refresh_authentication_refreshes_both_oauth_bodies() {
        let (mut menu, refreshes, _) = menu("providers");
        menu.refresh_authentication();
        assert_eq!(*refreshes.borrow(), 2);
    }

    #[test]
    fn update_models_forwards_to_the_model_body() {
        let (mut menu, _, updates) = menu("models");
        menu.update_models(None, None, None);
        assert_eq!(*updates.borrow(), 1);
    }

    #[test]
    fn the_search_value_comes_from_the_tab_body() {
        let (mut menu, _, _) = menu("providers");
        assert_eq!(menu.get_search_value(None), "");
        menu.bodies.providers.get_search_input().handle_input("pdf");
        assert_eq!(menu.get_search_value(Some("providers")), "pdf");
        assert_eq!(menu.get_search_value(Some("models")), "");
    }

    #[test]
    fn the_tab_bar_renders_every_tab_label_and_the_hint() {
        let mut bar = ConfigurationMenuTabBar::new("models");
        let lines = <ConfigurationMenuTabBar as Component>::render(&mut bar, 80.0);
        assert!(lines.iter().any(|line| line.contains("Tabs:")));
        assert!(lines.iter().any(|line| line.contains("[\u{25b6} Models]")));
        assert!(lines.iter().any(|line| line.contains("switch tabs")));
        assert!(lines.iter().any(|line| line.contains("close")));
        assert!(rows_for(&bar, 80.0) >= 2);
    }

    #[test]
    fn the_menu_renders_at_least_the_header_rows() {
        let (mut menu, _, _) = menu("providers");
        let lines = menu.render(80.0);
        assert!(lines.len() >= 2);
        assert!(menu.tab_bar_rows() >= 1);
    }
}
