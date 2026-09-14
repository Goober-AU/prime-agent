//! Port of packages/coding-agent/src/modes/interactive/components/heartbeat-manager.ts

use std::cell::RefCell;
use std::rc::Rc;

use pi_tui::components::spacer::Spacer;
use pi_tui::components::truncated_text::TruncatedText;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::{Component, Focusable};

use super::menu_panel::{
    get_menu_list_layout, MenuList, MenuListLayout, MenuListLayoutOptions, MenuPanel,
    MenuPanelOptions, MenuRow, MenuRowOptions,
};

use crate::core::cron_jobs::{
    AgentCronJob, AgentHeartbeatManagementAction, DELIVERY_MODE_FOLLOW_UP, SOURCE_HEARTBEAT,
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

/// `heartbeat.job`, typed the way the TypeScript types it (`AgentCronJob`).
///
/// `AgentConnectionHeartbeat.job` is carried as `serde_json::Value` in the
/// agent-connection contract (owned by another slice), so this private helper
/// reads it back into the real `AgentCronJob` shape. Field names and defaults
/// are the same ones the TypeScript component reads; a missing or malformed job
/// falls back to `Default`, which makes the panel show the same unknown values
/// the TypeScript would show for `undefined` fields.
fn heartbeat_job(heartbeat: &AgentConnectionHeartbeat) -> AgentCronJob {
    serde_json::from_value(heartbeat.job.clone()).unwrap_or_default()
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
    pending_future:
        Option<std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>>>>>,
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
            pending_future: None,
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
            let left_job = heartbeat_job(left);
            let right_job = heartbeat_job(right);
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
            Some(&heartbeat_job(heartbeat).id) == self.selected_heartbeat_id.as_ref()
        }) {
            self.selected_heartbeat_id = heartbeats
                .first()
                .map(|heartbeat| heartbeat_job(heartbeat).id);
        }
        if let HeartbeatManagerMode::Actions { heartbeat_id, .. } = &self.mode {
            if !heartbeats
                .iter()
                .any(|heartbeat| &heartbeat_job(heartbeat).id == heartbeat_id)
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
        Component::render(&mut panel, panel_width)
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
            .filter(|heartbeat| heartbeat_job(heartbeat).status == "active")
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
        let mut panel = MenuPanel::new(MenuPanelOptions {
            title: "Heartbeats".to_string(),
            subtitle: Some(format!("{count_label}. Select a heartbeat to manage.")),
        });
        let compact = self.get_list_layout().compact;
        let mut list = MenuList::new(Some(Box::new(move || compact)));
        self.populate_heartbeat_list(&mut list);
        panel.add_full_width_child(Rc::new(RefCell::new(list)));
        if let Some(error) = &self.error {
            panel.add_child(Rc::new(RefCell::new(Spacer::new(1))));
            panel.add_child(Rc::new(RefCell::new(TruncatedText::new(
                theme().fg("error", &format!("Error: {error}")),
                0,
                0,
            ))));
        }
        panel.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        panel.add_child(Rc::new(RefCell::new(TruncatedText::new(
            self.close_hint(),
            0,
            0,
        ))));
        panel
    }

    fn populate_heartbeat_list(&self, list: &mut MenuList) {
        let heartbeats = self.heartbeats();
        if heartbeats.is_empty() {
            list.add_child(
                Rc::new(RefCell::new(TruncatedText::new(
                    theme().fg("muted", "No running or paused heartbeats"),
                    1,
                    0,
                ))),
                false,
            );
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
            let job = heartbeat_job(heartbeat);
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
                    job.schedule.expression
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
            list.add_row(Rc::new(RefCell::new(MenuRow::new(MenuRowOptions {
                primary,
                secondary: Some(details),
                meta: Some(self.format_status(heartbeat)),
                selected: index == selected_index,
            }))));
        }

        if start_index > 0 || end_index < heartbeats.len() {
            list.add_child(
                Rc::new(RefCell::new(TruncatedText::new(
                    theme().fg(
                        "muted",
                        &format!("  ({}/{})", selected_index + 1, heartbeats.len()),
                    ),
                    1,
                    0,
                ))),
                false,
            );
        }
    }

    fn create_action_panel(&self, heartbeat_id: &str, selected_index: usize) -> MenuPanel {
        let heartbeat = self.find_heartbeat(heartbeat_id);
        let heartbeat = match heartbeat {
            Some(heartbeat) => heartbeat,
            None => {
                return MenuPanel::new(MenuPanelOptions {
                    title: "Heartbeats".to_string(),
                    subtitle: Some("This heartbeat is no longer available.".to_string()),
                })
            }
        };
        let job = heartbeat_job(&heartbeat);
        let label = job.label.as_deref().map(str::trim).unwrap_or("");
        let name = if !label.is_empty() {
            label.to_string()
        } else {
            self.default_heartbeat_name(&heartbeat).to_string()
        };
        let mut panel = MenuPanel::new(MenuPanelOptions {
            title: name,
            subtitle: Some(self.single_line(&job.prompt)),
        });
        panel.add_child(Rc::new(RefCell::new(TruncatedText::new(
            theme().fg("muted", &self.format_heartbeat_details(&heartbeat)),
            0,
            0,
        ))));
        panel.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        if let Some(error) = &self.error {
            panel.add_child(Rc::new(RefCell::new(TruncatedText::new(
                theme().fg("error", &format!("Error: {error}")),
                0,
                0,
            ))));
            panel.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        }
        let mut list = MenuList::new(None);
        for (index, action) in self.available_actions(Some(&heartbeat)).iter().enumerate() {
            list.add_row(Rc::new(RefCell::new(MenuRow::new(MenuRowOptions {
                primary: action.label.clone(),
                secondary: Some(self.action_description(&action.action)),
                meta: None,
                selected: index == selected_index,
            }))));
        }
        panel.add_full_width_child(Rc::new(RefCell::new(list)));
        panel.add_child(Rc::new(RefCell::new(Spacer::new(1))));
        panel.add_child(Rc::new(RefCell::new(TruncatedText::new(
            self.detail_hint(),
            0,
            0,
        ))));
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
                    .map(|heartbeat| heartbeat_job(heartbeat).id);
            }
            HeartbeatManagerMode::Actions {
                heartbeat_id,
                selected_index,
            } => {
                // `Math.max(0, Math.min(selectedIndex + delta, count - 1))`
                // (heartbeat-manager.ts:201-202). A vanished heartbeat leaves
                // `count == 0`, where `clamp(0, -1)` would panic.
                let count = self
                    .available_actions(self.find_heartbeat(&heartbeat_id).as_ref())
                    .len();
                let upper = count as i64 - 1;
                let next = (selected_index as i64 + delta).min(upper).max(0) as usize;
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
                        heartbeat_id: heartbeat_job(heartbeat).id,
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

    // The owner ticks the callback future without keeping a RefCell borrow
    // across await, so streaming, cancellation and terminal resize still run.
    pub(crate) fn poll_action(&mut self) {
        if self.pending_future.is_none() {
            if let Some((heartbeat, action)) = self.pending_action.take() {
                self.busy = true;
                self.error = None;
                self.pending_future = Some((self.options.on_action)(heartbeat, action));
                (self.options.request_render)();
            }
        }
        let result = self.pending_future.as_mut().and_then(|future| {
            let mut context = std::task::Context::from_waker(futures::task::noop_waker_ref());
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Ready(result) => Some(result),
                std::task::Poll::Pending => None,
            }
        });
        if let Some(result) = result {
            self.pending_future = None;
            match result {
                Ok(()) => self.mode = HeartbeatManagerMode::List,
                Err(error) => self.error = Some(error),
            }
            self.busy = false;
            (self.options.request_render)();
        }
    }

    fn available_actions(
        &self,
        heartbeat: Option<&AgentConnectionHeartbeat>,
    ) -> Vec<HeartbeatAction> {
        let Some(heartbeat) = heartbeat else {
            return Vec::new();
        };
        let job = heartbeat_job(heartbeat);
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
            Some(&heartbeat_job(heartbeat).id) == self.selected_heartbeat_id.as_ref()
        });
        index.unwrap_or(0)
    }

    fn find_heartbeat(&self, id: &str) -> Option<AgentConnectionHeartbeat> {
        self.heartbeats()
            .into_iter()
            .find(|heartbeat| heartbeat_job(heartbeat).id == id)
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
        heartbeat_job(heartbeat).session_id
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
        if heartbeat_job(heartbeat).status == "active" {
            theme().fg("success", "active")
        } else {
            theme().fg("warning", "paused")
        }
    }

    fn format_heartbeat_details(&self, heartbeat: &AgentConnectionHeartbeat) -> String {
        let job = heartbeat_job(heartbeat);
        let delivery = if job.delivery_mode.as_deref() == Some(DELIVERY_MODE_FOLLOW_UP) {
            "follow-up"
        } else {
            "steer"
        };
        let next = match &job.next_run_at {
            Some(next_run_at) => format_timestamp(next_run_at),
            None => "—".to_string(),
        };
        format!(
            "{} · {} · {} · {} · {delivery} · next {next} · {} runs",
            self.source_label(heartbeat),
            self.session_label(heartbeat),
            job.status,
            job.schedule.expression,
            job.run_count
        )
    }

    fn source_label(&self, heartbeat: &AgentConnectionHeartbeat) -> &'static str {
        if heartbeat_job(heartbeat).source.as_deref() == Some(SOURCE_HEARTBEAT) {
            "Created by you"
        } else {
            "Created by agent"
        }
    }

    fn default_heartbeat_name(&self, heartbeat: &AgentConnectionHeartbeat) -> &'static str {
        if heartbeat_job(heartbeat).source.as_deref() == Some(SOURCE_HEARTBEAT) {
            "Your heartbeat"
        } else {
            "Agent-created heartbeat"
        }
    }

    fn close_hint(&self) -> String {
        key_hint(
            "tui.select.cancel",
            "close",
            &KeyTextOptions { primary_only: true },
        )
    }

    fn detail_hint(&self) -> String {
        format!(
            "{}  {}",
            key_hint("app.modal.back", "back", &KeyTextOptions::default()),
            key_hint(
                "tui.select.cancel",
                "close",
                &KeyTextOptions { primary_only: true }
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

/// Port of `formatTimestamp(value)` (heartbeat-manager.ts:321-325):
/// `parsed.toISOString().slice(0, 16).replace("T", " ")`, or the raw value when
/// `new Date(value)` is not finite.
fn format_timestamp(value: &str) -> String {
    match parse_iso_millis(value) {
        Some(millis) => {
            let iso = millis_to_iso(millis);
            let slice: String = iso.chars().take(16).collect();
            slice.replace('T', " ")
        }
        None => value.to_string(),
    }
}

/// `new Date(value)` -> epoch milliseconds; `Number.isFinite(parsed.getTime())`.
///
/// The ECMAScript `Date` grammar is narrower than the ISO 8601 one the parser
/// used to accept, so the accepted forms are spelled out here (`Date.parse`,
/// `Date Time String Format`):
/// - the date part is exactly `YYYY`, `YYYY-MM` or `YYYY-MM-DD`; the 6-digit
///   `±YYYYYY` expanded form is also allowed,
/// - month is `01..12`, day is `01..31` (a day past the end of its month rolls
///   forward, as `Date.UTC(2024, 1, 30)` does), hour is `00..24`, minute and
///   second are `00..59`; `24` is only valid with `00:00` and zero fraction,
/// - the fraction, when present, must be non-empty and all digits,
/// - the offset is `Z` or `±HH:MM` with hour `00..23` and minute `00..59`,
/// - leading/trailing whitespace is rejected (unlike the trimmed Rust parser),
/// - the result must survive the `±8.64e15` ms range check that makes
///   `Number.isFinite(parsed.getTime())` false outside it.
pub(crate) fn parse_iso_millis(value: &str) -> Option<i64> {
    const MAX_MILLIS: i64 = 8_640_000_000_000_000;
    let trimmed = value.trim();
    if trimmed != value {
        return None;
    }
    // `YYYY-MM-DDTHH:MM:SS(.sss)(Z|±HH:MM)`, or a bare `YYYY(-MM(-DD))` date.
    // `T` and `Z` may be lowercase; Node accepts both spellings.
    let (date_part, rest) = match trimmed
        .find(['T', 't'])
        .map(|index| (&trimmed[..index], &trimmed[index + 1..]))
    {
        Some((date_part, rest)) => (date_part, Some(rest)),
        None => (trimmed, None),
    };
    let date_fields: Vec<&str> = date_part.split('-').collect();
    let (year, month, day) = match date_fields.as_slice() {
        [year] => (parse_year(year)?, 1, 1),
        [year, month] => (parse_year(year)?, parse_component(month, 1, 12)?, 1),
        [year, month, day] => (
            parse_year(year)?,
            parse_component(month, 1, 12)?,
            parse_component(day, 1, 31)?,
        ),
        _ => return None,
    };

    let Some(rest) = rest else {
        let days = days_from_civil(year, month, day);
        return days
            .checked_mul(86_400_000)
            .filter(|millis| millis.abs() <= MAX_MILLIS);
    };
    let (time_part, offset_seconds) = split_timezone(rest)?;
    let time_fields: Vec<&str> = time_part.split(':').collect();
    let (hour, minute, seconds, fraction) = match time_fields.as_slice() {
        [hour] => (parse_component(hour, 0, 24)?, 0, 0, (0, false)),
        [hour, minute] => (
            parse_component(hour, 0, 24)?,
            parse_component(minute, 0, 59)?,
            0,
            (0, false),
        ),
        [hour, minute, seconds] => {
            let (whole, fraction) = split_seconds(seconds)?;
            (
                parse_component(hour, 0, 24)?,
                parse_component(minute, 0, 59)?,
                parse_component(whole, 0, 59)?,
                fraction,
            )
        }
        _ => return None,
    };
    // `24:00` is the only hour-24 form. Node also rejects a fractional second
    // there unless every fraction digit is zero: `24:00:00.000Z` is valid but
    // `24:00:00.0001Z` is not.
    if hour == 24 && (minute != 0 || seconds != 0 || fraction.1) {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let millis = days
        .checked_mul(86_400_000)?
        .checked_add(hour.checked_mul(3_600_000)?)?
        .checked_add(minute.checked_mul(60_000)?)?
        .checked_add(seconds.checked_mul(1_000)?)?
        .checked_add(fraction.0)?
        .checked_sub(offset_seconds.checked_mul(1_000)?)?;
    (millis.abs() <= MAX_MILLIS).then_some(millis)
}

/// `YYYY` or `±YYYYYY`; exactly the widths the grammar allows.
fn parse_year(value: &str) -> Option<i64> {
    let digits = value.strip_prefix('+').or_else(|| value.strip_prefix('-')).unwrap_or(value);
    let signed = value.starts_with('+') || value.starts_with('-');
    let expected = if signed { 6 } else { 4 };
    if digits.len() != expected || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

/// A fixed-width unsigned field within `min..=max`.
fn parse_component(value: &str, min: i64, max: i64) -> Option<i64> {
    if value.len() != 2 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let parsed: i64 = value.parse().ok()?;
    (min..=max).contains(&parsed).then_some(parsed)
}

/// Splits `SS` or `SS.sss`. A present fraction must be non-empty and all
/// digits. Returns the whole part (range-checked by the caller), the
/// milliseconds truncated after three fraction digits, and whether any
/// fraction digit was non-zero.
fn split_seconds(value: &str) -> Option<(&str, (i64, bool))> {
    match value.split_once('.') {
        Some((whole, fraction)) => {
            if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let mut digits: String = fraction.chars().take(3).collect();
            while digits.len() < 3 {
                digits.push('0');
            }
            let millis: i64 = digits.parse().ok()?;
            let any_non_zero = fraction.bytes().any(|byte| byte != b'0');
            Some((whole, (millis, any_non_zero)))
        }
        None => Some((value, (0, false))),
    }
}

fn split_timezone(rest: &str) -> Option<(&str, i64)> {
    if let Some(time) = rest.strip_suffix(['Z', 'z']) {
        return Some((time, 0));
    }
    for (index, ch) in rest.char_indices() {
        if ch == '+' || ch == '-' {
            let time = &rest[..index];
            let offset = &rest[index..];
            let sign = if ch == '-' { -1 } else { 1 };
            let digits = &offset[1..];
            let (hours, minutes) = match digits.split_once(':') {
                Some((hours, minutes)) => (hours, minutes),
                None if digits.len() == 4 => digits.split_at(2),
                None => return None,
            };
            let hours = parse_component(hours, 0, 23)?;
            let minutes = parse_component(minutes, 0, 59)?;
            return Some((time, sign * (hours * 3_600 + minutes * 60)));
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
    let year = if year < 0 {
        format!("-{:06}", -year)
    } else if year > 9999 {
        format!("+{year:06}")
    } else {
        format!("{year:04}")
    };
    format!("{year}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.000Z")
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

#[cfg(test)]
mod pr18_tests {
    use super::*;

    /// `formatTimestamp` must mirror `new Date(value).toISOString().slice(0,16)`
    /// (heartbeat-manager.ts:321-325). The expectations below are the output of
    /// `node -e "new Date(s)"` for the same inputs.
    #[test]
    fn format_timestamp_follows_the_javascript_date_grammar() {
        let format = format_timestamp;
        // Valid forms.
        assert_eq!(format("2024-01-01T00:00:00Z"), "2024-01-01 00:00");
        assert_eq!(format("2024-01-01T00:00:00+05:00"), "2023-12-31 19:00");
        assert_eq!(format("2024-01-01T00:00:00-05:00"), "2024-01-01 05:00");
        assert_eq!(format("2024-01-01T00:00:00+0500"), "2023-12-31 19:00");
        assert_eq!(format("2024-01-01T00:00Z"), "2024-01-01 00:00");
        assert_eq!(format("2024-01-01T00:00:00"), "2024-01-01 00:00");
        assert_eq!(format("2024-01-01"), "2024-01-01 00:00");
        assert_eq!(format("2024-01-01T00:00:00.1234Z"), "2024-01-01 00:00");
        assert_eq!(format("2024-01-01t00:00:00z"), "2024-01-01 00:00");
        // Out-of-range components roll forward instead of being rejected.
        assert_eq!(format("2024-02-30T00:00:00Z"), "2024-03-01 00:00");
        assert_eq!(format("2023-02-29T00:00:00Z"), "2023-03-01 00:00");
        assert_eq!(format("2024-01-01T24:00:00Z"), "2024-01-02 00:00");
        assert_eq!(format("2024-01-01T24:00:00.0Z"), "2024-01-02 00:00");
        // Out-of-range components and malformed shapes fall back to the raw value.
        for invalid in [
            "2024-13-01T00:00:00Z",
            "2024-00-01T00:00:00Z",
            "2024-01-00T00:00:00Z",
            "2024-01-32T00:00:00Z",
            "2024-01-01T25:00:00Z",
            "2024-01-01T00:60:00Z",
            "2024-01-01T00:00:60Z",
            "2024-01-01T24:00:01Z",
            "2024-01-01T24:00:00.5Z",
            "2024-01-01T24:00:00.0001Z",
            "2024-01-01T00:00:00+24:00",
            "2024-01-01T00:00:00+05:60",
            "2024-01-01T00:00:00+99:00",
            "2024-1-5T00:00:00Z",
            "2024-01-01T0:00:00Z",
            "2024-01-01T00:00:00.Z",
            "2024-01-01T00:00:00,5Z",
            "2024-01-01T00:00:1e5Z",
            "2024-01-01T00:00:00Z+01:00",
            "not a date",
            "  2024-01-01T00:00:00Z  ",
        ] {
            assert_eq!(format(invalid), invalid, "{invalid} must not parse");
        }
        // Out-of-range years must not overflow the millisecond arithmetic.
        assert_eq!(format("9223372036854775807-01-01T00:00:00Z"), "9223372036854775807-01-01T00:00:00Z");
        assert_eq!(format("999999999999-01-01T00:00:00Z"), "999999999999-01-01T00:00:00Z");
        assert_eq!(format("099999-01-01T00:00:00Z"), "099999-01-01T00:00:00Z");
        assert_eq!(format("+275761-09-13T00:00:00Z"), "+275761-09-13T00:00:00Z");
        // The expanded year form is valid. `toISOString().slice(0, 16)` counts
        // the `±YYYYYY` prefix, so an expanded year truncates to the hour:
        // Node gives "+275760-09-13 00", not the full "+275760-09-13 00:00".
        assert_eq!(format("+275760-09-13T00:00:00Z"), "+275760-09-13 00");
        assert_eq!(format("+099999-01-01T00:00:00Z"), "+099999-01-01 00");
    }

    /// `moveSelection` clamps with `Math.max(0, Math.min(...))`
    /// (heartbeat-manager.ts:201-202), which cannot panic. A heartbeat that
    /// disappears while its action list is open leaves the action count at 0,
    /// and the Rust `clamp(0, count - 1)` port panicked there with
    /// `min > max. min = 0, max = -1`.
    #[test]
    fn move_selection_survives_a_heartbeat_that_vanished() {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
        let job = AgentCronJob {
            id: "h".into(),
            status: "active".into(),
            ..Default::default()
        };
        // The catalog empties out after the panel was opened.
        let heartbeat = Rc::new(std::cell::RefCell::new(Some(AgentConnectionHeartbeat {
            job: serde_json::to_value(&job).unwrap(),
            ..Default::default()
        })));
        let heartbeats = heartbeat.clone();
        let mut component = HeartbeatManagerComponent::new(HeartbeatManagerOptions {
            get_heartbeats: Box::new(move || heartbeats.borrow().clone().into_iter().collect()),
            get_rows: Rc::new(|| 24.0),
            on_action: Box::new(|_, _| Box::pin(async { Ok(()) })),
            on_close: Box::new(|| {}),
            request_render: Box::new(|| {}),
        });
        component.render(80.0);
        // Enter the action list for the heartbeat that is about to disappear.
        component.handle_input("\r");
        *heartbeat.borrow_mut() = None;

        // Both directions must be harmless rather than panic. `tui.select.up`
        // and `.down` default to the plain arrow keys.
        component.handle_input("\x1b[A");
        component.handle_input("\x1b[B");
        // `render` falls back to the list panel when the selected heartbeat
        // disappeared (`heartbeat-manager.ts:91-95`), so the panel must show the
        // empty list rather than a stale action list.
        let rendered = component.render(80.0).join("\n");
        assert!(
            rendered.contains("No running or paused heartbeats"),
            "a vanished heartbeat must fall back to the empty list, got: {rendered}"
        );
    }

    #[test]
    fn owner_tick_runs_one_action_and_preserves_failure_for_retry() {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
        let job = AgentCronJob {
            id: "h".into(),
            status: "active".into(),
            ..Default::default()
        };
        let calls = Rc::new(std::cell::Cell::new(0));
        let called = calls.clone();
        let mut component = HeartbeatManagerComponent::new(HeartbeatManagerOptions {
            get_heartbeats: Box::new(move || {
                vec![AgentConnectionHeartbeat {
                    job: serde_json::to_value(&job).unwrap(),
                    ..Default::default()
                }]
            }),
            get_rows: Rc::new(|| 24.0),
            on_action: Box::new(move |_, action| {
                assert_eq!(action, "pause");
                called.set(called.get() + 1);
                Box::pin(async { Err("provider unavailable".into()) })
            }),
            on_close: Box::new(|| {}),
            request_render: Box::new(|| {}),
        });
        component.render(80.0);
        component.handle_input("\r");
        component.handle_input("\r");
        component.poll_action();
        assert_eq!(calls.get(), 1);
        assert!(component
            .render(80.0)
            .join("\n")
            .contains("provider unavailable"));
        component.poll_action();
        assert_eq!(calls.get(), 1);
        component.handle_input("\r");
        component.poll_action();
        assert_eq!(calls.get(), 2);
    }
}
