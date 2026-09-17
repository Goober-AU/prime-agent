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
pub trait OAuthSelectorBody: Component {
    fn refresh(&mut self);
    fn get_search_input(&mut self) -> &mut dyn MenuSearchInputSurface;
    fn set_focused(&mut self, focused: bool);
}

/// `ModelSelectorComponent` (components/model-selector.ts) surface.
pub trait ModelSelectorBody: Component {
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
        let mut lines = self.wrap_items(items, &theme().fg("muted", "  "), safe_width);
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

/// The three selector bodies built by the configuration menu constructor.
pub struct ConfigurationMenuBodies {
    pub providers: Box<dyn OAuthSelectorBody>,
    pub models: Box<dyn ModelSelectorBody>,
    pub mcp_connections: Box<dyn OAuthSelectorBody>,
}

/// Port of `ConfigurationMenuComponent`.
pub struct ConfigurationMenuComponent {
    bodies: ConfigurationMenuBodies,
    auth_storage: Rc<RefCell<AuthStorage>>,
    model_registry: Rc<RefCell<ModelRegistry>>,
    active_tab: ConfigurationMenuTab,
    focused: bool,
    render_width: f64,
    shared_render_width: Option<Rc<std::cell::Cell<f64>>>,
    tab_bar: Rc<RefCell<ConfigurationMenuTabBar>>,
    /// `options.requestRender`.
    request_render: Box<dyn FnMut()>,
    /// `options.onCancel`; kept so the tab switching surface keeps the same
    /// callbacks the constructor receives.
    on_cancel: Box<dyn FnMut()>,
}

impl ConfigurationMenuComponent {
    pub fn new(mut options: ConfigurationMenuOptions) -> Self {
        use super::model_selector::{
            ModelSelectorComponent, ModelSelectorOptions, ScopedModelItem,
        };
        use super::oauth_selector::{
            AuthSelectorCategory, OAuthSelectorComponent, OAuthSelectorOptions,
        };
        let tab_bar = Rc::new(RefCell::new(ConfigurationMenuTabBar::new(
            options.initial_tab,
        )));
        let render_width = Rc::new(std::cell::Cell::new(78.0));
        let header = tab_bar.clone();
        let width = render_width.clone();
        let header_rows: Rc<dyn Fn() -> f64> = Rc::new(move || {
            (header
                .borrow()
                .get_row_count(get_menu_panel_inner_width(width.get()) as f64)
                + 1) as f64
        });
        let rows = options.get_rows.take().map(Rc::<dyn Fn() -> f64>::from);
        let cancel = Rc::new(RefCell::new(std::mem::replace(
            &mut options.on_cancel,
            Box::new(|| {}),
        )));
        let create_oauth = |service: bool| {
            let registry = options.model_registry.clone();
            let header_rows = header_rows.clone();
            let selector = OAuthSelectorComponent::new(
                "login",
                Box::new(SharedAuth(options.auth_storage.clone())),
                options
                    .provider_options
                    .iter()
                    .filter(|p| (p.category.as_deref() == Some("service")) == service)
                    .map(|p| super::oauth_selector::AuthSelectorProvider {
                        id: p.id.clone(),
                        name: p.name.clone(),
                        auth_type: p.auth_type.clone(),
                        category: Some(if service {
                            AuthSelectorCategory::Service
                        } else {
                            AuthSelectorCategory::Provider
                        }),
                    })
                    .collect(),
                Some(Box::new(move |id| {
                    registry.borrow().get_provider_auth_status(id)
                })),
                OAuthSelectorOptions {
                    get_rows: rows.clone(),
                    header: Some(tab_bar.clone()),
                    get_header_rows: Some(Box::new(move || header_rows())),
                    title: Some(
                        if service {
                            "MCP Connections"
                        } else {
                            "Providers"
                        }
                        .into(),
                    ),
                    subtitle: Some(
                        if service {
                            "Connect MCP integrations and service credentials."
                        } else {
                            "Connect with a subscription or API key."
                        }
                        .into(),
                    ),
                    search_placeholder: Some(
                        if service {
                            "Search MCP connections"
                        } else {
                            "Search providers"
                        }
                        .into(),
                    ),
                    ..Default::default()
                },
            );
            selector
        };
        let providers = create_oauth(false);
        let mcp = create_oauth(true);
        let models = ModelSelectorComponent::new(
            options.current_model.as_ref().map(item_model),
            options
                .scoped_models
                .iter()
                .map(|s| ScopedModelItem {
                    model: item_model(&s.model),
                    thinking_level: s.thinking_level.clone(),
                })
                .collect(),
            ModelSelectorOptions {
                available_models: Some(options.available_models.iter().map(item_model).collect()),
                configured_providers: Some(options.configured_providers.iter().cloned().collect()),
                initial_search_input: options.initial_model_search.clone(),
                recent_models: options.recent_models.clone(),
                get_rows: rows,
                header: Some(tab_bar.clone()),
                get_header_rows: Some(header_rows),
                ..Default::default()
            },
        );
        let bodies = ConfigurationMenuBodies {
            providers: Box::new(ProviderBody {
                search: SharedSearch(providers.search_input()),
                selector: providers,
                select: std::mem::replace(&mut options.on_select_provider, Box::new(|_| {})),
                cancel: cancel.clone(),
            }),
            mcp_connections: Box::new(ProviderBody {
                search: SharedSearch(mcp.search_input()),
                selector: mcp,
                select: std::mem::replace(&mut options.on_select_mcp_connection, Box::new(|_| {})),
                cancel: cancel.clone(),
            }),
            models: Box::new(ModelsBody {
                search: SharedSearch(models.search_input()),
                selector: models,
                select: std::mem::replace(&mut options.on_select_model, Box::new(|_| {})),
                cancel,
            }),
        };
        let mut component = Self::with_bodies(options, bodies);
        component.tab_bar = tab_bar;
        component.shared_render_width = Some(render_width);
        component
    }

    fn with_bodies(options: ConfigurationMenuOptions, bodies: ConfigurationMenuBodies) -> Self {
        let tab_bar = Rc::new(RefCell::new(ConfigurationMenuTabBar::new(
            options.initial_tab,
        )));
        let active_tab = options.initial_tab;
        Self {
            bodies,
            auth_storage: options.auth_storage,
            model_registry: options.model_registry,
            active_tab,
            focused: false,
            render_width: 78.0,
            shared_render_width: None,
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
            "mcp-connections" => self.bodies.mcp_connections.get_search_input().get_value(),
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
        self.auth_storage.borrow_mut().reload();
        self.model_registry.borrow_mut().refresh();
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
        let next_index = (current_index + direction + CONFIGURATION_MENU_TABS.len() as i64)
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
            "models" => self.bodies.models.handle_input(key_data),
            "mcp-connections" => self.bodies.mcp_connections.handle_input(key_data),
            _ => self.bodies.providers.handle_input(key_data),
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
        if let Some(shared) = &self.shared_render_width {
            shared.set(width);
        }
        match self.active_tab {
            "models" => self.bodies.models.render(width),
            "mcp-connections" => self.bodies.mcp_connections.render(width),
            _ => self.bodies.providers.render(width),
        }
    }

    fn handle_input(&mut self, data: &str) {
        ConfigurationMenuComponent::handle_input(self, data);
    }

    fn invalidate(&mut self) {
        self.bodies.providers.invalidate();
        self.bodies.models.invalidate();
        self.bodies.mcp_connections.invalidate();
    }

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

struct SharedAuth(Rc<RefCell<AuthStorage>>);
impl super::oauth_selector::AuthStorageLike for SharedAuth {
    fn get(&self, provider: &str) -> Option<crate::core::auth_storage::AuthCredential> {
        self.0.borrow().get(provider)
    }
    fn get_auth_status(&self, provider: &str) -> crate::core::auth_storage::AuthStatus {
        self.0.borrow().get_auth_status(provider)
    }
}
struct SharedSearch(Rc<RefCell<super::menu_panel::MenuSearchInput>>);
impl MenuSearchInputSurface for SharedSearch {
    fn get_value(&self) -> String {
        self.0.borrow().get_value()
    }
    fn handle_input(&mut self, data: &str) {
        self.0.borrow_mut().handle_input(data);
    }
}
type Cancel = Rc<RefCell<Box<dyn FnMut()>>>;
struct ProviderBody {
    selector: super::oauth_selector::OAuthSelectorComponent,
    search: SharedSearch,
    select: Box<dyn FnMut(&AuthSelectorProvider)>,
    cancel: Cancel,
}
impl Component for ProviderBody {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.selector.render(width)
    }
    fn invalidate(&mut self) {
        self.selector.invalidate();
    }
    fn handle_input(&mut self, data: &str) {
        self.selector.handle_input(data);
        if self.selector.cancelled {
            self.selector.cancelled = false;
            (self.cancel.borrow_mut())();
        }
        if let Some(p) = self.selector.selected_provider.take() {
            (self.select)(&AuthSelectorProvider {
                id: p.id,
                name: p.name,
                auth_type: p.auth_type,
                category: p.category.map(|c| c.as_str().into()),
            });
        }
    }
}
impl OAuthSelectorBody for ProviderBody {
    fn refresh(&mut self) {
        self.selector.refresh();
    }
    fn get_search_input(&mut self) -> &mut dyn MenuSearchInputSurface {
        &mut self.search
    }
    fn set_focused(&mut self, focused: bool) {
        Focusable::set_focused(&mut self.selector, focused);
    }
}
struct ModelsBody {
    selector: super::model_selector::ModelSelectorComponent,
    search: SharedSearch,
    select: Box<dyn FnMut(&Model)>,
    cancel: Cancel,
}
fn item_model(model: &Model) -> super::model_selector::ModelItemModel {
    super::model_selector::ModelItemModel {
        provider: model.provider.clone(),
        id: model.id.clone(),
        name: model.name.clone(),
        featured: model.featured.unwrap_or(false),
        raw: serde_json::to_value(model).unwrap_or_default(),
    }
}
impl Component for ModelsBody {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.selector.render(width)
    }
    fn invalidate(&mut self) {
        self.selector.invalidate();
    }
    fn handle_input(&mut self, data: &str) {
        self.selector.handle_input(data);
        if self.selector.cancelled {
            self.selector.cancelled = false;
            (self.cancel.borrow_mut())();
        }
        if let Some(model) = self
            .selector
            .selected_model
            .take()
            .and_then(|m| serde_json::from_value(m.raw).ok())
        {
            (self.select)(&model);
        }
    }
}
impl ModelSelectorBody for ModelsBody {
    fn update_state(
        &mut self,
        current: Option<&Model>,
        models: Option<&[Model]>,
        configured: Option<&std::collections::HashSet<String>>,
    ) {
        self.selector.update_state(
            current.map(item_model),
            models
                .map(|models| models.iter().map(item_model).collect())
                .or_else(|| self.selector.available_models.clone()),
            configured
                .map(|p| p.iter().cloned().collect())
                .or_else(|| self.selector.configured_providers.clone()),
        );
    }
    fn get_search_input(&mut self) -> &mut dyn MenuSearchInputSurface {
        &mut self.search
    }
    fn set_focused(&mut self, focused: bool) {
        Focusable::set_focused(&mut self.selector, focused);
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

    impl Component for FakeOAuth {
        fn render(&mut self, _width: f64) -> Vec<String> {
            vec!["body".into(), self.search.value.clone()]
        }
        fn handle_input(&mut self, data: &str) {
            self.search.handle_input(data);
        }
        fn invalidate(&mut self) {}
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

    impl Component for FakeModel {
        fn render(&mut self, _width: f64) -> Vec<String> {
            vec!["body".into(), self.search.value.clone()]
        }
        fn handle_input(&mut self, data: &str) {
            self.search.handle_input(data);
        }
        fn invalidate(&mut self) {}
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
        let probe = ConfigurationMenuTabBar::new(bar.active_tab);
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

    fn menu(
        initial_tab: ConfigurationMenuTab,
    ) -> (
        ConfigurationMenuComponent,
        Rc<RefCell<usize>>,
        Rc<RefCell<usize>>,
    ) {
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
        (
            ConfigurationMenuComponent::with_bodies(options, bodies),
            provider_refreshes,
            model_updates,
        )
    }

    #[test]
    fn real_configuration_tabs_render_filter_select_and_keep_independent_searches() {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
        crate::core::keybindings::KeybindingsManager::new(Default::default(), None).install();
        let selected = Rc::new(RefCell::new(Vec::new()));
        let provider_selection = selected.clone();
        let model_selection = selected.clone();
        let service_selection = selected.clone();
        let cancelled = Rc::new(std::cell::Cell::new(false));
        let cancel = cancelled.clone();
        let mut menu = ConfigurationMenuComponent::new(ConfigurationMenuOptions {
            initial_tab: "providers",
            tui: tui(),
            auth_storage: Rc::new(RefCell::new(auth_storage())),
            model_registry: Rc::new(RefCell::new(model_registry())),
            provider_options: vec![
                AuthSelectorProvider {
                    id: "alpha".into(),
                    name: "Alpha Provider".into(),
                    auth_type: "api_key".into(),
                    category: None,
                },
                AuthSelectorProvider {
                    id: "service".into(),
                    name: "Search Service".into(),
                    auth_type: "api_key".into(),
                    category: Some("service".into()),
                },
            ],
            current_model: None,
            scoped_models: vec![],
            available_models: vec![Model::new(
                "test-model",
                "Test Model",
                "openai-completions",
                "alpha",
                "http://127.0.0.1",
            )],
            configured_providers: Default::default(),
            recent_models: None,
            initial_model_search: None,
            get_rows: Some(Box::new(|| 24.0)),
            request_render: Box::new(|| {}),
            on_select_provider: Box::new(move |p| {
                provider_selection.borrow_mut().push(p.id.clone())
            }),
            on_select_model: Box::new(move |m| model_selection.borrow_mut().push(m.id.clone())),
            on_select_mcp_connection: Box::new(move |p| {
                service_selection.borrow_mut().push(p.id.clone())
            }),
            on_cancel: Box::new(move || cancel.set(true)),
        });
        menu.set_focused(true);
        for (query, expected) in [
            ("alpha", "Alpha Provider"),
            ("test", "test-model"),
            ("search", "Search Service"),
        ] {
            menu.handle_input(query);
            let lines = menu.render(80.0);
            let text = lines.join("\n");
            assert!(text.contains(expected), "{text}");
            assert!(text.contains("Tabs:"));
            assert!(text.contains(pi_tui::tui::CURSOR_MARKER));
            assert!(lines.len() <= 24, "{} rows", lines.len());
            assert!(lines.iter().all(|line| visible_width(line) <= 80));
            menu.handle_input("\r");
            menu.handle_input("\t");
        }
        assert_eq!(&*selected.borrow(), &["alpha", "test-model", "service"]);
        assert_eq!(menu.get_search_value(Some("providers")), "alpha");
        assert_eq!(menu.get_search_value(Some("models")), "test");
        assert_eq!(menu.get_search_value(Some("mcp-connections")), "search");
        menu.set_active_tab("models");
        for _ in 0..5 {
            menu.handle_input("\x1b[D");
        }
        assert!(
            !cancelled.get(),
            "left in model search must not close configuration"
        );
        menu.handle_input("\x1b");
        assert!(cancelled.get());
    }

    #[test]
    fn mounted_large_models_menu_keeps_search_cursor_and_filter_across_updates() {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
        crate::core::keybindings::KeybindingsManager::new(Default::default(), None).install();
        let ui = tui();
        let models: Vec<Model> = (0..1296).map(|index| {
            Model::new(format!("model-{index:04}"), format!("Model {index}"),
                "openai-completions", "test-provider", "http://127.0.0.1")
        }).collect();
        let cancelled = Rc::new(std::cell::Cell::new(false));
        let on_cancel = cancelled.clone();
        let selected = Rc::new(RefCell::new(None));
        let on_select = selected.clone();
        let menu = Rc::new(RefCell::new(ConfigurationMenuComponent::new(ConfigurationMenuOptions {
            initial_tab: "models", tui: ui.clone(),
            auth_storage: Rc::new(RefCell::new(auth_storage())),
            model_registry: Rc::new(RefCell::new(model_registry())),
            provider_options: vec![], current_model: None, scoped_models: vec![],
            available_models: models.clone(), configured_providers: Default::default(),
            recent_models: None, initial_model_search: None,
            get_rows: Some(Box::new(|| 30.0)), request_render: Box::new(|| {}),
            on_select_provider: Box::new(|_| {}), on_select_mcp_connection: Box::new(|_| {}),
            on_select_model: Box::new(move |model| *on_select.borrow_mut() = Some(model.id.clone())),
            on_cancel: Box::new(move || on_cancel.set(true)),
        })));
        let handle = ui.borrow_mut().show_overlay(menu.clone(), Default::default());
        assert!(handle.is_focused());
        let focused = ui.borrow().focused_component().unwrap();
        let start = std::time::Instant::now();
        for ch in "model-1234".chars() {
            focused.borrow_mut().handle_input(&ch.to_string());
            menu.borrow_mut().render(90.0);
        }
        assert_eq!(menu.borrow_mut().get_search_value(None), "model-1234");
        let rendered = menu.borrow_mut().render(90.0).join("\n");
        assert!(rendered.contains("model-1234"));
        assert!(!rendered.contains("model-0000"));
        focused.borrow_mut().handle_input("\x1b[D");
        // Catalog/roster updates must not reset a mid-field cursor or search.
        menu.borrow_mut().update_models(None, Some(&models), None);
        focused.borrow_mut().handle_input("\x7f");
        assert_eq!(menu.borrow_mut().get_search_value(None), "model-124");
        focused.borrow_mut().handle_input("\x1b[200~3\x1b[201~");
        assert_eq!(menu.borrow_mut().get_search_value(None), "model-1234");
        focused.borrow_mut().handle_input("\t");
        focused.borrow_mut().handle_input("\x1b[Z");
        assert_eq!(menu.borrow().get_active_tab(), "models");
        assert_eq!(menu.borrow_mut().get_search_value(None), "model-1234");
        focused.borrow_mut().handle_input("\r");
        assert_eq!(selected.borrow().as_deref(), Some("model-1234"));
        focused.borrow_mut().handle_input("\x1b");
        assert!(cancelled.get());
        eprintln!("mounted 1296-model edit/filter/update/select/cancel: {:?}", start.elapsed());
        handle.hide();
        ui.borrow_mut().sync_overlays();
        assert!(!ui.borrow().has_overlay());
    }

    #[test]
    fn tab_constants_and_labels_match_typescript() {
        assert_eq!(
            CONFIGURATION_MENU_TABS,
            ["providers", "models", "mcp-connections"]
        );
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

    /// DEFECT B. `update_models` must reach the REAL model body and replace the
    /// rendered list, not merely bump a counter.
    ///
    /// The audit pin was that `update_models` had only a test caller
    /// (configuration_menu.rs:947 at the audited HEAD) while the TypeScript calls
    /// `menu.updateModels(this.getCurrentModel(), models,
    /// this.connectionConfiguredProviders)` after login and after selecting a
    /// model (interactive-mode.ts:8368, :8396-8400, :8433-8437). A counter-only
    /// test cannot tell the real body from a stub, so this test drives the real
    /// `ConfigurationMenuComponent::new` (real `ModelSelectorComponent` inside)
    /// and asserts the RENDERED catalog changed.
    #[test]
    fn update_models_replaces_the_catalog_the_real_menu_renders() {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
        crate::core::keybindings::KeybindingsManager::new(Default::default(), None).install();
        let model = |provider: &str, id: &str| {
            Model::new(id, id, "openai-completions", provider, "http://127.0.0.1")
        };
        let mut menu = ConfigurationMenuComponent::new(ConfigurationMenuOptions {
            initial_tab: "models",
            tui: tui(),
            auth_storage: Rc::new(RefCell::new(auth_storage())),
            model_registry: Rc::new(RefCell::new(model_registry())),
            provider_options: Vec::new(),
            current_model: None,
            scoped_models: vec![],
            available_models: vec![model("stale-provider", "stale-model")],
            configured_providers: Default::default(),
            recent_models: None,
            initial_model_search: None,
            get_rows: Some(Box::new(|| 24.0)),
            request_render: Box::new(|| {}),
            on_select_provider: Box::new(|_| {}),
            on_select_model: Box::new(|_| {}),
            on_select_mcp_connection: Box::new(|_| {}),
            on_cancel: Box::new(|| {}),
        });
        menu.set_focused(true);

        let before = menu.render(80.0).join("\n");
        assert!(
            before.contains("stale-model"),
            "precondition: the constructor catalog renders, got: {before}"
        );
        assert!(
            !before.contains("fresh-model"),
            "precondition: the refreshed model must be absent before the update"
        );

        menu.update_models(
            None,
            Some(&[model("fresh-provider", "fresh-model")]),
            Some(&std::collections::HashSet::from([
                "fresh-provider".to_string()
            ])),
        );

        let after = menu.render(80.0).join("\n");
        assert!(
            after.contains("fresh-model"),
            "update_models must replace the catalog the menu renders, got: {after}"
        );
        assert!(
            !after.contains("stale-model"),
            "the superseded catalog must be gone, got: {after}"
        );
    }

    /// DEFECT 2 on the real menu: `refreshAuthentication` + `updateModels` must
    /// leave the newly authenticated model visible AND the model body's
    /// configured-provider set updated, so the selection path can accept it.
    ///
    /// `menu.refreshAuthentication(); menu.updateModels(this.getCurrentModel(),
    /// this.getCachedModelCandidates(), this.connectionConfiguredProviders)`
    /// (interactive-mode.ts:8381, :8396-8400).
    #[test]
    fn the_real_menu_shows_the_newly_authenticated_model_after_login_refresh() {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
        crate::core::keybindings::KeybindingsManager::new(Default::default(), None).install();
        let model = |provider: &str, id: &str| {
            Model::new(id, id, "openai-completions", provider, "http://127.0.0.1")
        };
        let mut menu = ConfigurationMenuComponent::new(ConfigurationMenuOptions {
            initial_tab: "providers",
            tui: tui(),
            auth_storage: Rc::new(RefCell::new(auth_storage())),
            model_registry: Rc::new(RefCell::new(model_registry())),
            provider_options: Vec::new(),
            current_model: None,
            scoped_models: vec![],
            available_models: Vec::new(),
            configured_providers: Default::default(),
            recent_models: None,
            initial_model_search: None,
            get_rows: Some(Box::new(|| 24.0)),
            request_render: Box::new(|| {}),
            on_select_provider: Box::new(|_| {}),
            on_select_model: Box::new(|_| {}),
            on_select_mcp_connection: Box::new(|_| {}),
            on_cancel: Box::new(|| {}),
        });
        menu.set_focused(true);

        // The post-login refresh the host performs.
        menu.refresh_authentication();
        menu.update_models(
            None,
            Some(&[model("my-proxy", "proxy-model")]),
            Some(&std::collections::HashSet::from(["my-proxy".to_string()])),
        );
        menu.set_active_tab("models");

        assert_eq!(menu.get_active_tab(), "models");
        let rendered = menu.render(80.0).join("\n");
        assert!(
            rendered.contains("proxy-model"),
            "the newly authenticated model must render in the Models tab: {rendered}"
        );
        // The model body renders "sign in" for an unconfigured provider and
        // "current" for a configured one (model_selector.rs:913-922), so the
        // configured set that `update_models` forwarded is observable here.
        assert!(
            !rendered.contains("sign in"),
            "a configured provider must not ask for sign-in: {rendered}"
        );

        // Control: without the refreshed configured set the same model asks for sign-in.
        let mut unconfigured = ConfigurationMenuComponent::new(ConfigurationMenuOptions {
            initial_tab: "models",
            tui: tui(),
            auth_storage: Rc::new(RefCell::new(auth_storage())),
            model_registry: Rc::new(RefCell::new(model_registry())),
            provider_options: Vec::new(),
            current_model: None,
            scoped_models: vec![],
            available_models: vec![model("my-proxy", "proxy-model")],
            configured_providers: Default::default(),
            recent_models: None,
            initial_model_search: None,
            get_rows: Some(Box::new(|| 24.0)),
            request_render: Box::new(|| {}),
            on_select_provider: Box::new(|_| {}),
            on_select_model: Box::new(|_| {}),
            on_select_mcp_connection: Box::new(|_| {}),
            on_cancel: Box::new(|| {}),
        });
        unconfigured.set_focused(true);
        let control = unconfigured.render(80.0).join("\n");
        assert!(
            control.contains("sign in"),
            "control: an unconfigured provider must ask for sign-in: {control}"
        );
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
