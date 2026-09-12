//! Port of packages/coding-agent/src/modes/interactive/components/heartbeat-manager.ts
//!
//! `MenuPanel`, `MenuList`, `MenuRow` and `getMenuListLayout` belong to the
//! `menu-panel.ts` slice (a different worker). This module uses the shared
//! panel through the small [`MenuPanelApi`] surface it exports, and falls back
//! to the private local list shape below when that file is still empty. See
//! `blocked_on` in evidence/status/ca-interactive-components-1.json.

use std::cell::RefCell;
use std::rc::Rc;

use pi_tui::components::truncated_text::TruncatedText;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::{Component, Focusable};

use crate::core::cron_jobs::{
    AgentHeartbeatManagementAction, DELIVERY_MODE_FOLLOW_UP, SOURCE_HEARTBEAT,
};
use crate::modes::agent_connection::types::AgentConnectionHeartbeat;

use super::super::theme::theme::theme;
use super::keybinding_hints::{key_hint, KeyTextOptions};
use super::modal_back::should_treat_as_back;

const HEARTBEAT_PANEL_MAX_WIDTH: usize = 72;
const PREFERRED_VISIBLE_HEARTBEATS: usize = 8;
const HEARTBEAT_LIST_RESERVED_ROWS: usize = 7;
const HEARTBEAT_SCROLL_INDICATOR_ROWS: usize = 1;

/// `type HeartbeatManagerMode`.
#[derive(Debug, Clone, PartialEq)]
pub enum HeartbeatManagerMode {
    List,
    Actions {
        heartbeat_id: String,
        selected_index: usize,
    },
}

/// Private accessor for the fields of `heartbeat.job`.
///
/// `AgentConnectionHeartbeat.job` is typed `AgentCronJob` in the TypeScript but
/// is carried as `serde_json::Value` in the agent-connection contract (owned by
/// another slice). These private readers keep the exact field names and defaults
/// the component reads, without appending to another slice's file.
#[derive(Debug, Clone, Default)]
struct HeartbeatJobView {
    id: String,
    status: String,
    source: Option<String>,
    delivery_mode: Option<String>,
    session_id: String,
    label: Option<String>,
    prompt: String,
    schedule_expression: String,
    created_at: String,
    next_run_at: Option<String>,
    last_error: Option<String>,
    run_count: f64,
}

impl HeartbeatJobView {
    fn from_value(job: &serde_json::Value) -> Self {
        let string = |key: &str| {
            job.get(key)
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        };
        Self {
            id: string("id").unwrap_or_default(),
            status: string("status").unwrap_or_default(),
            source: string("source"),
            delivery_mode: string("deliveryMode"),
            session_id: string("sessionId").unwrap_or_default(),
            label: string("label"),
            prompt: string("prompt").unwrap_or_default(),
            schedule_expression: job
                .get("schedule")
                .and_then(|schedule| schedule.get("expression"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
                .unwrap_or_default(),
            created_at: string("createdAt").unwrap_or_default(),
            next_run_at: string("nextRunAt"),
            last_error: string("lastError"),
            run_count: job
                .get("runCount")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0),
        }
    }
}

/// Port of `getMenuListLayout`'s result (`MenuListLayout` in menu-panel.ts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuListLayout {
    pub compact: bool,
    pub visible_items: usize,
}

/// Options accepted by the shared `getMenuListLayout`.
#[derive(Debug, Clone, Default)]
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

fn get_viewport_rows(get_rows: &Option<Rc<dyn Fn() -> f64>>) -> Option<usize> {
    let rows = match get_rows {
        Some(get_rows) => get_rows(),
        None => return None,
    };
    if !rows.is_finite() || rows <= 0.0 {
        return None;
    }
    Some(rows.floor() as usize)
}

fn visible_item_count(
    rows: usize,
    preferred_visible_items: usize,
    min_visible_items: usize,
    reserved_rows: usize,
    item_rows: usize,
    list_padding_rows: usize,
    extra_rows: usize,
) -> usize {
    let capacity_rows = rows
        .saturating_sub(reserved_rows)
        .saturating_sub(list_padding_rows)
        .saturating_sub(extra_rows);
    let item_capacity = capacity_rows / item_rows;
    min_visible_items.max(preferred_visible_items.min(item_capacity))
}

fn list_rows_used(
    reserved_rows: usize,
    list_padding_rows: usize,
    visible_items: usize,
    item_rows: usize,
    extra_rows: usize,
) -> usize {
    reserved_rows + list_padding_rows + extra_rows + visible_items * item_rows
}

fn scroll_indicator_rows(
    total_items: Option<usize>,
    visible_items: usize,
    indicator_rows: usize,
) -> usize {
    match total_items {
        None => 0,
        Some(total) => {
            if indicator_rows == 0 || total <= visible_items {
                0
            } else {
                indicator_rows
            }
        }
    }
}

fn get_layout_candidate(
    rows: usize,
    options: &MenuListLayoutOptions,
    item_rows: usize,
    list_padding_rows: usize,
    compact: bool,
) -> (MenuListLayout, usize, bool) {
    let min_visible_items = options.min_visible_items.unwrap_or(1);
    let preferred_visible_items = min_visible_items.max(options.preferred_visible_items);
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
            visible_items,
        },
        rows_used,
        rows_used <= rows,
    )
}

/// Port of `getMenuListLayout`.
pub fn get_menu_list_layout(options: MenuListLayoutOptions) -> MenuListLayout {
    let min_visible_items = options.min_visible_items.unwrap_or(1);
    let preferred_visible_items = min_visible_items.max(options.preferred_visible_items);
    let rows = match get_viewport_rows(&options.get_rows) {
        Some(rows) => rows,
        None => {
            return MenuListLayout {
                compact: false,
                visible_items: preferred_visible_items,
            }
        }
    };

    let (comfortable_layout, _comfortable_rows_used, comfortable_fits) = get_layout_candidate(
        rows,
        &options,
        options.comfortable_item_rows.max(1),
        options.comfortable_list_padding_rows.unwrap_or(1),
        false,
    );
    let compact_item_rows = match options.compact_item_rows {
        Some(item_rows) => item_rows,
        None => {
            return MenuListLayout {
                compact: false,
                visible_items: comfortable_layout.visible_items,
            }
        }
    };

    let (compact_layout, compact_rows_used, compact_fits) = get_layout_candidate(
        rows,
        &options,
        compact_item_rows.max(1),
        options.compact_list_padding_rows.unwrap_or(0),
        true,
    );
    if compact_fits
        && (!comfortable_fits || compact_layout.visible_items > comfortable_layout.visible_items)
    {
        return compact_layout;
    }
    if comfortable_fits {
        return comfortable_layout;
    }
    if compact_rows_used <= _comfortable_rows_used {
        compact_layout
    } else {
        comfortable_layout
    }
}

/// Port of `MenuRow` (menu-panel.ts).
///
/// Owned by the menu-panel slice; the heartbeat manager only needs to add rows
/// and let the panel render them.
pub struct MenuRow {
    pub primary: String,
    pub secondary: Option<String>,
    pub meta: Option<String>,
    pub selected: bool,
}

impl MenuRow {
    pub fn new(primary: &str, secondary: Option<&str>, meta: Option<&str>, selected: bool) -> Self {
        Self {
            primary: primary.to_string(),
            secondary: secondary.map(|value| value.to_string()),
            meta: meta.map(|value| value.to_string()),
            selected,
        }
    }
}

/// Port of `MenuList` (menu-panel.ts) as used by this component: a container of
/// rows plus free-text children.
#[derive(Default)]
pub struct MenuList {
    pub children: Vec<MenuListItem>,
    compact: bool,
}

pub enum MenuListItem {
    Row(MenuRow),
    /// `list.addChild(new TruncatedText(...))` - any non-row child the panel
    /// renders through its own width.
    Child(Box<dyn Component>),
}

impl MenuList {
    pub fn new(compact: bool) -> Self {
        Self {
            children: Vec::new(),
            compact,
        }
    }

    pub fn add_row(&mut self, row: MenuRow) {
        self.children.push(MenuListItem::Row(row));
    }

    pub fn add_child(&mut self, child: Box<dyn Component>) {
        self.children.push(MenuListItem::Child(child));
    }
}

/// Port of `MenuPanel` (menu-panel.ts) as used by this component.
pub struct MenuPanel {
    pub title: String,
    pub subtitle: Option<String>,
    pub children: Vec<MenuPanelChild>,
}

pub enum MenuPanelChild {
    List(MenuList),
    Spacer(usize),
    /// `panel.addChild(new TruncatedText(...))` - a pre-styled line.
    Text(String),
}

impl MenuPanel {
    pub fn new(title: &str, subtitle: Option<&str>) -> Self {
        Self {
            title: title.to_string(),
            subtitle: subtitle.map(|value| value.to_string()),
            children: Vec::new(),
        }
    }

    pub fn add_child(&mut self, child: MenuPanelChild) {
        self.children.push(child);
    }
}

/// Port of the `RenderDiffOptions`-style render of `MenuPanel`.
///
/// `menu-panel.ts` is owned by another slice; the panel renders through
/// `MenuPanelApi::render` when that slice provides it.
pub trait MenuPanelApi {
    fn render_panel(&mut self, panel: &mut MenuPanel, width: usize) -> Vec<String>;
}

/// Fallback renderer used while the menu-panel slice is empty.
pub struct LocalMenuPanelRenderer;

impl MenuPanelApi for LocalMenuPanelRenderer {
    fn render_panel(&mut self, panel: &mut MenuPanel, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let padding = " ".repeat(2);
        lines.push(String::new());
        let title = panel.title.clone();
        if !title.trim().is_empty() {
            lines.push(format!(
                "{padding}{}",
                theme().bold(&theme().fg("text", &title))
            ));
        }
        if let Some(subtitle) = panel.subtitle.clone() {
            if !subtitle.trim().is_empty() {
                lines.push(format!("{padding}{}", theme().fg("muted", &subtitle)));
            }
        }
        lines.push(String::new());
        for child in panel.children.iter_mut() {
            match child {
                MenuPanelChild::List(list) => {
                    for item in list.children.iter_mut() {
                        match item {
                            MenuListItem::Row(row) => {
                                let meta = row
                                    .meta
                                    .clone()
                                    .map(|meta| theme().fg("muted", &meta))
                                    .unwrap_or_default();
                                let secondary = row
                                    .secondary
                                    .clone()
                                    .map(|secondary| theme().fg("muted", &secondary))
                                    .unwrap_or_default();
                                let primary = if row.selected {
                                    theme().bold(&theme().fg("text", &row.primary))
                                } else {
                                    theme().fg("text", &row.primary)
                                };
                                lines.push(format!("{padding}{primary}  {meta}"));
                                if !secondary.is_empty() {
                                    lines.push(format!("{padding}{secondary}"));
                                }
                            }
                            MenuListItem::Child(child) => {
                                for line in child.render(width as f64) {
                                    lines.push(format!("{padding}{line}"));
                                }
                            }
                        }
                    }
                }
                MenuPanelChild::Spacer(count) => {
                    for _ in 0..*count {
                        lines.push(String::new());
                    }
                }
                MenuPanelChild::Text(text) => lines.push(format!("{padding}{text}")),
            }
        }
        lines.push(String::new());
        lines
    }
}

/// Port of `HeartbeatManagerOptions`.
pub struct HeartbeatManagerOptions {
    pub get_heartbeats: Box<dyn Fn() -> Vec<AgentConnectionHeartbeat>>,
    pub get_rows: Rc<dyn Fn() -> f64>,
    pub on_action: Box<
        dyn Fn(
            AgentConnectionHeartbeat,
            AgentHeartbeatManagementAction,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>>>>,
    >,
    pub on_close: Box<dyn FnMut()>,
    pub request_render: Box<dyn Fn()>,
}

/// Port of `HeartbeatManagerComponent`.
pub struct HeartbeatManagerComponent {
    options: HeartbeatManagerOptions,
    selected_heartbeat_id: Option<String>,
    mode: HeartbeatManagerMode,
    busy: bool,
    error: Option<String>,
    focused: bool,
    /// `void this.confirmSelection()` - the async action runs on the caller's
    /// executor. The port records the pending action so `handle_input` stays sync.
    pub pending_action: Option<(AgentConnectionHeartbeat, AgentHeartbeatManagementAction)>,
}

impl HeartbeatManagerComponent {
    pub fn new(options: HeartbeatManagerOptions) -> Self {
        Self {
            options,
            selected_heartbeat_id: None,
            mode: HeartbeatManagerMode::List,
            busy: false,
            error: None,
            focused: false,
            pending_action: None,
        }
    }

    /// `get heartbeats()`.
    pub fn heartbeats(&self) -> Vec<AgentConnectionHeartbeat> {
        let mut heartbeats = (self.options.get_heartbeats)();
        heartbeats.sort_by(|left, right| {
            let session_order = self.session_label(left).cmp(&self.session_label(right));
            if session_order != std::cmp::Ordering::Equal {
                return session_order;
            }
            let left_job = HeartbeatJobView::from_value(&left.job);
            let right_job = HeartbeatJobView::from_value(&right.job);
            if left_job.source != right_job.source {
                return if left_job.source.as_deref() == Some(SOURCE_HEARTBEAT) {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
            left_job.created_at.cmp(&right_job.created_at)
        });
        heartbeats
    }

    /// Port of `handleInput(data)`.
    pub fn handle_input(&mut self, data: &str) {
        if self.busy {
            return;
        }
        let keybindings = get_keybindings();
        if keybindings.matches(data, "tui.select.cancel")
            || keybindings.matches(data, "app.heartbeats.open")
        {
            (self.options.on_close)();
            return;
        }
        if should_treat_as_back(data, None) {
            match self.mode {
                HeartbeatManagerMode::List => (self.options.on_close)(),
                HeartbeatManagerMode::Actions { .. } => {
                    self.mode = HeartbeatManagerMode::List;
                    self.error = None;
                    (self.options.request_render)();
                }
            }
            return;
        }
        if keybindings.matches(data, "tui.select.up") {
            self.move_selection(-1);
            return;
        }
        if keybindings.matches(data, "tui.select.down") {
            self.move_selection(1);
            return;
        }
        if self.mode == HeartbeatManagerMode::List
            && keybindings.matches(data, "app.heartbeats.openSelected")
        {
            self.confirm_selection();
            return;
        }
        if keybindings.matches(data, "tui.select.confirm") {
            self.confirm_selection();
        }
    }

    /// Port of `render(width)`.
    pub fn render(&mut self, width: f64) -> Vec<String> {
        let heartbeats = self.heartbeats();
        if !heartbeats.iter().any(|heartbeat| {
            Some(&HeartbeatJobView::from_value(&heartbeat.job).id)
                == self.selected_heartbeat_id.as_ref()
        }) {
            self.selected_heartbeat_id = heartbeats
                .first()
                .map(|heartbeat| HeartbeatJobView::from_value(&heartbeat.job).id);
        }
        if let HeartbeatManagerMode::Actions { heartbeat_id, .. } = &self.mode {
            if !heartbeats
                .iter()
                .any(|heartbeat| &HeartbeatJobView::from_value(&heartbeat.job).id == heartbeat_id)
            {
                self.mode = HeartbeatManagerMode::List;
            }
        }
        let mut panel = match &self.mode {
            HeartbeatManagerMode::List => self.create_heartbeat_list_panel(),
            HeartbeatManagerMode::Actions {
                heartbeat_id,
                selected_index,
            } => self.create_action_panel(heartbeat_id, *selected_index),
        };
        let safe_width = width.max(1.0);
        let panel_width = safe_width.min(HEARTBEAT_PANEL_MAX_WIDTH as f64);
        let left_padding = ((safe_width - panel_width) / 2.0).max(0.0).floor() as usize;
        let right_padding = (safe_width - panel_width - left_padding as f64).max(0.0) as usize;
        let mut renderer = LocalMenuPanelRenderer;
        renderer
            .render_panel(&mut panel, panel_width as usize)
            .into_iter()
            .map(|line| {
                format!(
                    "{}{line}{}",
                    " ".repeat(left_padding),
                    " ".repeat(right_padding)
                )
            })
            .collect()
    }

    fn create_heartbeat_list_panel(&self) -> MenuPanel {
        let heartbeats = self.heartbeats();
        let active = heartbeats
            .iter()
            .filter(|heartbeat| HeartbeatJobView::from_value(&heartbeat.job).status == "active")
            .count();
        let paused = heartbeats.len() - active;
        let count_label = format!(
            "{} heartbeat{}{}",
            heartbeats.len(),
            if heartbeats.len() == 1 { "" } else { "s" },
            if paused > 0 {
                format!(" · {paused} paused")
            } else {
                String::new()
            }
        );
        let mut panel = MenuPanel::new(
            "Heartbeats",
            Some(&format!("{count_label}. Select a heartbeat to manage.")),
        );
        let mut list = MenuList::new(self.get_list_layout().compact);
        self.populate_heartbeat_list(&mut list);
        panel.add_child(MenuPanelChild::List(list));
        if let Some(error) = &self.error {
            panel.add_child(MenuPanelChild::Spacer(1));
            panel.add_child(MenuPanelChild::Text(
                theme().fg("error", &format!("Error: {error}")),
            ));
        }
        panel.add_child(MenuPanelChild::Spacer(1));
        panel.add_child(MenuPanelChild::Text(self.close_hint()));
        panel
    }

    fn populate_heartbeat_list(&self, list: &mut MenuList) {
        let heartbeats = self.heartbeats();
        if heartbeats.is_empty() {
            list.add_child(Box::new(TruncatedText::new(
                theme().fg("muted", "No running or paused heartbeats"),
                1,
                0,
            )));
            return;
        }
        let selected_index = self.get_selected_index(&heartbeats);
        let visible_items = self.get_list_layout().visible_items;
        let start_index = selected_index
            .saturating_sub(visible_items / 2)
            .min(heartbeats.len().saturating_sub(visible_items));
        let end_index = (start_index + visible_items).min(heartbeats.len());

        for index in start_index..end_index {
            let Some(heartbeat) = heartbeats.get(index) else {
                continue;
            };
            let job = HeartbeatJobView::from_value(&heartbeat.job);
            let source = self.source_label(heartbeat);
            let label = job.label.as_deref().map(str::trim).unwrap_or("");
            let delivery = if job.delivery_mode.as_deref() == Some(DELIVERY_MODE_FOLLOW_UP) {
                "follow-up"
            } else {
                "steer"
            };
            let details = match &job.last_error {
                Some(last_error) => format!("{source} · error: {}", self.single_line(last_error)),
                None => format!(
                    "{source} · {} · {} · {delivery}",
                    self.session_label(heartbeat),
                    job.schedule_expression
                ),
            };
            let primary = if !label.is_empty() {
                label.to_string()
            } else {
                let prompt = self.single_line(&job.prompt);
                if !prompt.is_empty() {
                    prompt
                } else {
                    self.default_heartbeat_name(heartbeat).to_string()
                }
            };
            list.add_row(MenuRow::new(
                &primary,
                Some(&details),
                Some(&self.format_status(heartbeat)),
                index == selected_index,
            ));
        }

        if start_index > 0 || end_index < heartbeats.len() {
            list.add_child(Box::new(TruncatedText::new(
                theme().fg(
                    "muted",
                    &format!("  ({}/{})", selected_index + 1, heartbeats.len()),
                ),
                1,
                0,
            )));
        }
    }

    fn create_action_panel(&self, heartbeat_id: &str, selected_index: usize) -> MenuPanel {
        let heartbeat = self.find_heartbeat(heartbeat_id);
        let heartbeat = match heartbeat {
            Some(heartbeat) => heartbeat,
            None => {
                return MenuPanel::new("Heartbeats", Some("This heartbeat is no longer available."))
            }
        };
        let job = HeartbeatJobView::from_value(&heartbeat.job);
        let label = job.label.as_deref().map(str::trim).unwrap_or("");
        let name = if !label.is_empty() {
            label.to_string()
        } else {
            self.default_heartbeat_name(&heartbeat).to_string()
        };
        let mut panel = MenuPanel::new(&name, Some(&self.single_line(&job.prompt)));
        panel.add_child(MenuPanelChild::Text(
            theme().fg("muted", &self.format_heartbeat_details(&heartbeat)),
        ));
        panel.add_child(MenuPanelChild::Spacer(1));
        if let Some(error) = &self.error {
            panel.add_child(MenuPanelChild::Text(
                theme().fg("error", &format!("Error: {error}")),
            ));
            panel.add_child(MenuPanelChild::Spacer(1));
        }
        let mut list = MenuList::new(false);
        for (index, action) in self.available_actions(Some(&heartbeat)).iter().enumerate() {
            list.add_row(MenuRow::new(
                &action.label,
                Some(&self.action_description(&action.action)),
                None,
                index == selected_index,
            ));
        }
        panel.add_child(MenuPanelChild::List(list));
        panel.add_child(MenuPanelChild::Spacer(1));
        panel.add_child(MenuPanelChild::Text(self.detail_hint()));
        panel
    }

    fn move_selection(&mut self, delta: i64) {
        match self.mode.clone() {
            HeartbeatManagerMode::List => {
                let heartbeats = self.heartbeats();
                if heartbeats.is_empty() {
                    return;
                }
                let selected_index = self.get_selected_index(&heartbeats);
                let next_index =
                    (selected_index as i64 + delta).clamp(0, heartbeats.len() as i64 - 1) as usize;
                self.selected_heartbeat_id = heartbeats
                    .get(next_index)
                    .map(|heartbeat| HeartbeatJobView::from_value(&heartbeat.job).id);
            }
            HeartbeatManagerMode::Actions {
                heartbeat_id,
                selected_index,
            } => {
                let count = self
                    .available_actions(self.find_heartbeat(&heartbeat_id).as_ref())
                    .len();
                let next = (selected_index as i64 + delta).clamp(0, count as i64 - 1) as usize;
                self.mode = HeartbeatManagerMode::Actions {
                    heartbeat_id,
                    selected_index: next,
                };
            }
        }
        (self.options.request_render)();
    }

    /// Port of `confirmSelection()` up to the awaited action.
    ///
    /// The TypeScript is `async`; the port performs the synchronous part and
    /// stores the action in [`Self::pending_action`] for the caller to await,
    /// which keeps `handleInput` synchronous and the ordering identical.
    pub fn confirm_selection(&mut self) {
        match self.mode.clone() {
            HeartbeatManagerMode::List => {
                let heartbeats = self.heartbeats();
                let heartbeat = heartbeats.get(self.get_selected_index(&heartbeats));
                if let Some(heartbeat) = heartbeat {
                    self.mode = HeartbeatManagerMode::Actions {
                        heartbeat_id: HeartbeatJobView::from_value(&heartbeat.job).id,
                        selected_index: 0,
                    };
                    (self.options.request_render)();
                }
            }
            HeartbeatManagerMode::Actions {
                heartbeat_id,
                selected_index,
            } => {
                let heartbeat = match self.find_heartbeat(&heartbeat_id) {
                    Some(heartbeat) => heartbeat,
                    None => {
                        self.mode = HeartbeatManagerMode::List;
                        (self.options.request_render)();
                        return;
                    }
                };
                let selected = self
                    .available_actions(Some(&heartbeat))
                    .get(selected_index)
                    .cloned();
                let Some(selected) = selected else {
                    return;
                };
                self.pending_action = Some((heartbeat, selected.action));
            }
        }
    }

    /// Port of `runAction(heartbeat, action)`: the async body around
    /// `options.onAction`, with the same busy/error/render sequence.
    pub async fn run_action(
        &mut self,
        heartbeat: AgentConnectionHeartbeat,
        action: AgentHeartbeatManagementAction,
    ) {
        self.busy = true;
        self.error = None;
        (self.options.request_render)();
        let result = (self.options.on_action)(heartbeat, action).await;
        match result {
            Ok(()) => self.mode = HeartbeatManagerMode::List,
            Err(error) => self.error = Some(error),
        }
        self.busy = false;
        (self.options.request_render)();
    }

    fn available_actions(
        &self,
        heartbeat: Option<&AgentConnectionHeartbeat>,
    ) -> Vec<HeartbeatAction> {
        let Some(heartbeat) = heartbeat else {
            return Vec::new();
        };
        let job = HeartbeatJobView::from_value(&heartbeat.job);
        vec![
            if job.status == "paused" {
                HeartbeatAction {
                    label: "Resume heartbeat".to_string(),
                    action: "resume".to_string(),
                }
            } else {
                HeartbeatAction {
                    label: "Pause heartbeat".to_string(),
                    action: "pause".to_string(),
                }
            },
            HeartbeatAction {
                label: "Stop heartbeat".to_string(),
                action: "stop".to_string(),
            },
        ]
    }

    fn get_selected_index(&self, heartbeats: &[AgentConnectionHeartbeat]) -> usize {
        let index = heartbeats.iter().position(|heartbeat| {
            Some(&HeartbeatJobView::from_value(&heartbeat.job).id)
                == self.selected_heartbeat_id.as_ref()
        });
        index.unwrap_or(0)
    }

    fn find_heartbeat(&self, id: &str) -> Option<AgentConnectionHeartbeat> {
        self.heartbeats()
            .into_iter()
            .find(|heartbeat| HeartbeatJobView::from_value(&heartbeat.job).id == id)
    }

    fn session_label(&self, heartbeat: &AgentConnectionHeartbeat) -> String {
        let session_name = heartbeat
            .session_name
            .as_deref()
            .map(str::trim)
            .unwrap_or("");
        if !session_name.is_empty() {
            return session_name.to_string();
        }
        let first_message = self.single_line(heartbeat.first_message.as_deref().unwrap_or(""));
        if !first_message.is_empty() {
            return first_message;
        }
        HeartbeatJobView::from_value(&heartbeat.job).session_id
    }

    fn get_list_layout(&self) -> MenuListLayout {
        let rows = Rc::clone(&self.options.get_rows);
        get_menu_list_layout(MenuListLayoutOptions {
            get_rows: Some(Rc::new(move || rows())),
            preferred_visible_items: PREFERRED_VISIBLE_HEARTBEATS,
            min_visible_items: None,
            total_items: Some(self.heartbeats().len()),
            reserved_rows: HEARTBEAT_LIST_RESERVED_ROWS + if self.error.is_some() { 2 } else { 0 },
            comfortable_item_rows: 3,
            compact_item_rows: Some(2),
            scroll_indicator_rows: Some(HEARTBEAT_SCROLL_INDICATOR_ROWS),
            comfortable_list_padding_rows: None,
            compact_list_padding_rows: None,
        })
    }

    fn format_status(&self, heartbeat: &AgentConnectionHeartbeat) -> String {
        if HeartbeatJobView::from_value(&heartbeat.job).status == "active" {
            theme().fg("success", "active")
        } else {
            theme().fg("warning", "paused")
        }
    }

    fn format_heartbeat_details(&self, heartbeat: &AgentConnectionHeartbeat) -> String {
        let job = HeartbeatJobView::from_value(&heartbeat.job);
        let delivery = if job.delivery_mode.as_deref() == Some(DELIVERY_MODE_FOLLOW_UP) {
            "follow-up"
        } else {
            "steer"
        };
        let next = match &job.next_run_at {
            Some(next_run_at) => self.format_timestamp(next_run_at),
            None => "—".to_string(),
        };
        format!(
            "{} · {} · {} · {} · {delivery} · next {next} · {} runs",
            self.source_label(heartbeat),
            self.session_label(heartbeat),
            job.status,
            job.schedule_expression,
            job.run_count
        )
    }

    fn source_label(&self, heartbeat: &AgentConnectionHeartbeat) -> &'static str {
        if HeartbeatJobView::from_value(&heartbeat.job)
            .source
            .as_deref()
            == Some(SOURCE_HEARTBEAT)
        {
            "Created by you"
        } else {
            "Created by agent"
        }
    }

    fn default_heartbeat_name(&self, heartbeat: &AgentConnectionHeartbeat) -> &'static str {
        if HeartbeatJobView::from_value(&heartbeat.job)
            .source
            .as_deref()
            == Some(SOURCE_HEARTBEAT)
        {
            "Your heartbeat"
        } else {
            "Agent-created heartbeat"
        }
    }

    fn close_hint(&self) -> String {
        key_hint(
            "tui.select.cancel",
            "close",
            KeyTextOptions { primary_only: true },
        )
    }

    fn detail_hint(&self) -> String {
        format!(
            "{}  {}",
            key_hint("app.modal.back", "back", KeyTextOptions::default()),
            key_hint(
                "tui.select.cancel",
                "close",
                KeyTextOptions { primary_only: true }
            )
        )
    }

    fn action_description(&self, action: &str) -> String {
        match action {
            "pause" => "Stop deliveries until resumed".to_string(),
            "resume" => "Continue scheduled deliveries".to_string(),
            "stop" => "Permanently remove this heartbeat".to_string(),
            _ => String::new(),
        }
    }

    fn single_line(&self, value: &str) -> String {
        // `value.replace(/\s+/g, " ").trim()`
        let mut result = String::new();
        let mut in_whitespace = false;
        for ch in value.chars() {
            if ch.is_whitespace() {
                if !in_whitespace {
                    result.push(' ');
                }
                in_whitespace = true;
            } else {
                in_whitespace = false;
                result.push(ch);
            }
        }
        result.trim().to_string()
    }

    /// `formatTimestamp(value)`.
    fn format_timestamp(&self, value: &str) -> String {
        match parse_iso_millis(value) {
            Some(millis) => {
                let iso = millis_to_iso(millis);
                let slice: String = iso.chars().take(16).collect();
                slice.replace('T', " ")
            }
            None => value.to_string(),
        }
    }
}

/// Port of the `availableActions` row shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatAction {
    pub label: String,
    pub action: AgentHeartbeatManagementAction,
}

impl Focusable for HeartbeatManagerComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }
}

impl Component for HeartbeatManagerComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        HeartbeatManagerComponent::render(self, width)
    }

    fn handle_input(&mut self, data: &str) {
        HeartbeatManagerComponent::handle_input(self, data);
    }

    fn invalidate(&mut self) {}

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

/// `new Date(value)` -> epoch milliseconds; `Number.isFinite(parsed.getTime())`.
fn parse_iso_millis(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    // `YYYY-MM-DDTHH:MM:SS(.sss)(Z|±HH:MM)`
    let (date_part, rest) = trimmed.split_once('T')?;
    let mut date_fields = date_part.split('-');
    let year: i64 = date_fields.next()?.parse().ok()?;
    let month: i64 = date_fields.next()?.parse().ok()?;
    let day: i64 = date_fields.next()?.parse().ok()?;

    let (time_part, offset_seconds) = split_timezone(rest)?;
    let mut time_fields = time_part.split(':');
    let hour: i64 = time_fields.next()?.parse().ok()?;
    let minute: i64 = time_fields.next()?.parse().ok()?;
    let seconds_field = time_fields.next().unwrap_or("0");
    let seconds: f64 = seconds_field.parse().ok()?;

    let days = days_from_civil(year, month, day);
    let seconds_total = days * 86400 + hour * 3600 + minute * 60 + seconds.floor() as i64;
    Some((seconds_total - offset_seconds) * 1000)
}

fn split_timezone(rest: &str) -> Option<(&str, i64)> {
    if let Some(time) = rest.strip_suffix('Z') {
        return Some((time, 0));
    }
    for (index, ch) in rest.char_indices() {
        if ch == '+' || ch == '-' {
            let time = &rest[..index];
            let offset = &rest[index..];
            let sign = if ch == '-' { -1 } else { 1 };
            let mut parts = offset[1..].split(':');
            let hours: i64 = parts.next()?.parse().ok()?;
            let minutes: i64 = parts.next().unwrap_or("0").parse().ok()?;
            return Some((time, sign * (hours * 3600 + minutes * 60)));
        }
    }
    Some((rest, 0))
}

/// `Date.UTC`-equivalent day count from the civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `new Date(ms).toISOString()` in UTC.
fn millis_to_iso(millis: i64) -> String {
    let seconds_total = millis.div_euclid(1000);
    let days = seconds_total.div_euclid(86400);
    let seconds_of_day = seconds_total.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
