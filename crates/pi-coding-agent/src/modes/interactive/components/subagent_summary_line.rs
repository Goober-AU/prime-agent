//! Port of packages/coding-agent/src/modes/interactive/components/subagent-summary-line.ts

use pi_tui::tui::{Component, Focusable};
use pi_tui::utils::{truncate_to_width, visible_width};

use crate::modes::agent_connection::types::AgentConnectionRlmChildAgentSnapshot;
use crate::modes::daemon::agent_roster::{
    classify_agent_status, classify_session_roster_status, AgentRosterStatus, AgentStatusInput,
    RosterSummaryView,
};
use crate::modes::daemon::daemon_session_list::SessionSummary;
use crate::modes::interactive::theme::theme::theme;

use super::keybinding_hints::{key_text, KeyTextOptions};

/// `SubagentSummaryCounts`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubagentSummaryCounts {
    pub total: usize,
    pub running: usize,
    pub idle: usize,
    pub inactive: usize,
    pub background: usize,
    pub waiting: usize,
}

/// Port of `classifySubagentSnapshotStatus`.
pub fn classify_subagent_snapshot_status(
    child: &AgentConnectionRlmChildAgentSnapshot,
) -> AgentRosterStatus {
    if is_terminal_waiting(child) {
        return AgentRosterStatus::Idle;
    }
    // Activity implies a live session; the in-process connection never stamps activeSessionId.
    let resident = child.active_session_id.is_some() || child.activity.is_some();
    let busy = child.status == "running" || child.status == "queued" || child.activity.is_some();
    classify_agent_status(AgentStatusInput {
        resident,
        queued_child: !resident && busy,
        busy,
    })
}

fn is_terminal_waiting(child: &AgentConnectionRlmChildAgentSnapshot) -> bool {
    matches!(child.status.as_str(), "done" | "error")
        && child.activity.as_ref().is_some_and(|activity| activity.kind == "waiting")
}

/// Port of `countDirectSubagentStatuses`.
pub fn count_direct_subagent_statuses<'a>(
    children: impl IntoIterator<Item = &'a AgentConnectionRlmChildAgentSnapshot>,
    parent_id: Option<&str>,
) -> SubagentSummaryCounts {
    let mut counts = SubagentSummaryCounts::default();
    for child in children {
        if child.parent_id.as_deref() != parent_id || child.status == "cancelled" {
            continue;
        }
        counts.total += 1;
        if is_terminal_waiting(child) {
            counts.waiting += 1;
            continue;
        }
        match classify_subagent_snapshot_status(child) {
            AgentRosterStatus::Running => counts.running += 1,
            AgentRosterStatus::Idle => counts.idle += 1,
            AgentRosterStatus::Inactive => counts.inactive += 1,
        }
    }
    counts
}

/// The parent fields `countRosterSubagentStatuses` reads.
#[derive(Debug, Clone, Default)]
pub struct RosterParent {
    pub active_session_id: Option<String>,
    pub session_id: Option<String>,
    pub session_file: Option<String>,
}

/// `isDirectAgentChild` (agents-view-state.ts) is declared over the agents-view
/// row type there, which is a second declaration of the daemon `SessionSummary`
/// this module ports (`daemon-session-list.ts`). Project the parent-linkage
/// fields `getParentKeys` reads onto an agents-view row so the shared direct-child
/// check can run on the daemon wire row, the same bridge `InteractiveMode` uses
/// for its roster bar.
fn linkage_row_for_roster_summary(
    child: &SessionSummary,
) -> crate::modes::agents_view::agents_view_state::SessionSummary {
    let mut view = crate::modes::agents_view::agents_view_state::SessionSummary::new(
        child.id.clone(),
        child.session_id.clone(),
        child.cwd.clone(),
    );
    view.parent_active_session_id = child.parent_active_session_id.clone();
    view.parent_session_id = child.parent_session_id.clone();
    view.parent_session_path = child.parent_session_path.clone();
    view
}

/// Port of `countRosterSubagentStatuses`.
pub fn count_roster_subagent_statuses<'a>(
    summaries: impl IntoIterator<Item = &'a SessionSummary>,
    parent: &RosterParent,
) -> SubagentSummaryCounts {
    let mut counts = SubagentSummaryCounts::default();
    for child in summaries {
        if child.runtime_kind.as_deref() != Some("subagent") || child.lifecycle != "live" {
            continue;
        }
        if !crate::modes::agents_view::agents_view_state::is_direct_agent_child(
            &linkage_row_for_roster_summary(child),
            parent.active_session_id.as_deref(),
            parent.session_id.as_deref(),
            parent.session_file.as_deref(),
        ) {
            continue;
        }
        counts.total += 1;
        if child.status_label.as_deref() == Some("background helper")
            || crate::modes::agents_view::native_wire::is_background_only(&serde_json::to_value(child).unwrap_or_default())
        {
            counts.background += 1;
            continue;
        }
        let status = child.roster_status.unwrap_or_else(|| {
            classify_session_roster_status(
                &RosterSummaryView {
                    active_session_id: child.active_session_id.clone(),
                    activity: Some(child.activity.clone()),
                    is_session_active: Some(child.is_session_active),
                },
                false,
            )
        });
        match status {
            AgentRosterStatus::Running => counts.running += 1,
            AgentRosterStatus::Idle => counts.idle += 1,
            AgentRosterStatus::Inactive => counts.inactive += 1,
        }
    }
    counts
}

/// One-line entry into the current session's scoped agents view.
pub struct SubagentSummaryLine {
    focused: bool,
    counts: SubagentSummaryCounts,
    openable: bool,
    always_visible: bool,
    get_location_label: Box<dyn Fn() -> Option<String>>,
    get_context_label: Box<dyn Fn() -> Option<String>>,
    get_override_label: Box<dyn Fn() -> Option<String>>,
    pub on_open: Option<Box<dyn FnMut()>>,
    pub on_cancel: Option<Box<dyn FnMut()>>,
    pub on_chat_action: Option<Box<dyn FnMut(&str)>>,
}

impl SubagentSummaryLine {
    pub fn new(
        get_location_label: Box<dyn Fn() -> Option<String>>,
        get_context_label: Box<dyn Fn() -> Option<String>>,
        get_override_label: Box<dyn Fn() -> Option<String>>,
    ) -> Self {
        Self {
            focused: false,
            counts: SubagentSummaryCounts::default(),
            openable: false,
            always_visible: false,
            get_location_label,
            get_context_label,
            get_override_label,
            on_open: None,
            on_cancel: None,
            on_chat_action: None,
        }
    }

    pub fn set_subagent_counts(&mut self, counts: SubagentSummaryCounts) {
        self.counts = counts;
    }

    pub fn set_openable(&mut self, openable: bool) {
        self.openable = openable;
    }

    pub(crate) fn set_always_visible(&mut self, always_visible: bool) {
        self.always_visible = always_visible;
    }

    pub fn is_selectable(&self) -> bool {
        self.counts.total > 0 && self.openable
    }

    pub fn handle_input(&mut self, data: &str) {
        let keybindings = pi_tui::keybindings::get_keybindings();
        if keybindings.matches(data, "tui.select.confirm")
            || keybindings.matches(data, "app.agents.open")
        {
            if self.is_selectable() {
                if let Some(handler) = self.on_open.as_mut() {
                    handler();
                }
            }
            return;
        }
        if keybindings.matches(data, "tui.select.up")
            || keybindings.matches(data, "tui.select.cancel")
            || keybindings.matches(data, "app.agents.back")
        {
            if let Some(handler) = self.on_cancel.as_mut() {
                handler();
            }
            return;
        }
        if let Some(handler) = self.on_chat_action.as_mut() {
            handler(data);
        }
    }

    fn render_info_line(&self, width: f64) -> Vec<String> {
        let override_label = (self.get_override_label)().map(|label| label.trim().to_string());
        let location_label = (self.get_location_label)().map(|label| label.trim().to_string());
        let context_label = (self.get_context_label)().map(|label| label.trim().to_string());
        let left = override_label
            .filter(|label| !label.is_empty())
            .or(location_label.filter(|label| !label.is_empty()))
            .unwrap_or_default();
        let context_label = context_label.filter(|label| !label.is_empty());
        if left.is_empty() && context_label.is_none() {
            return Vec::new();
        }
        let safe_width = width.max(1.0);
        let right = context_label.unwrap_or_default();
        let gap = if !left.is_empty() && !right.is_empty() {
            2.0
        } else {
            0.0
        };
        let right_width = (visible_width(&right) as f64).min((safe_width - gap).max(0.0));
        let left_width = (safe_width - right_width - gap).max(0.0);
        let rendered_left = truncate_to_width(&left, left_width, "\u{2026}", false);
        let rendered_right = truncate_to_width(&right, right_width, "\u{2026}", false);
        let padding = (safe_width
            - visible_width(&rendered_left) as f64
            - visible_width(&rendered_right) as f64)
            .max(0.0)
            .floor() as usize;
        vec![theme().fg(
            "muted",
            &format!("{rendered_left}{}{rendered_right}", " ".repeat(padding)),
        )]
    }
}

impl Default for SubagentSummaryLine {
    fn default() -> Self {
        Self::new(Box::new(|| None), Box::new(|| None), Box::new(|| None))
    }
}

impl Component for SubagentSummaryLine {
    fn render(&mut self, width: f64) -> Vec<String> {
        let mut lines = self.render_info_line(width);
        if self.counts.total == 0 && !self.always_visible {
            return lines;
        }
        if width < 2.0 {
            return lines;
        }
        let safe_width = width;
        let inner = safe_width - 2.0;
        let title = if self.always_visible {
            format!(
                "{} {}",
                self.counts.total,
                if self.counts.total == 1 { "agent" } else { "agents" }
            )
        } else {
            "subagents".to_string()
        };
        let label = theme().fg("accent", &format!("\u{1b}[1m{title}\u{1b}[22m"));
        let top = truncate_to_width(
            &format!(
                "{}{}{}",
                theme().fg("border", "\u{256d}\u{2500} "),
                label,
                theme().fg(
                    "border",
                    &format!(
                        " {}\u{256e}",
                        "\u{2500}".repeat(
                            (inner - 3.0 - visible_width(&label) as f64)
                                .max(0.0)
                                .floor() as usize
                        )
                    )
                )
            ),
            safe_width,
            "\u{2026}",
            false,
        );
        let mut counts = format!(
            "{}{}{}{}{}",
            theme().fg(
                "success",
                &format!("\u{25cf} {} running", self.counts.running)
            ),
            "   ",
            theme().fg("warning", &format!("\u{25d0} {} idle", self.counts.idle)),
            "   ",
            theme().fg(
                "dim",
                &format!("\u{25cb} {} inactive", self.counts.inactive)
            )
        );
        if self.counts.background > 0 {
            counts.push_str(&format!("   · {} background", self.counts.background));
        }
        if self.counts.waiting > 0 {
            counts.push_str(&format!("   · {} waiting", self.counts.waiting));
        }
        let open_hint = if self.is_selectable() {
            if self.focused {
                format!(
                    "{}/{} open",
                    key_text("tui.select.confirm", &KeyTextOptions::default()),
                    key_text("app.agents.open", &KeyTextOptions::default())
                )
            } else {
                format!("{} select", key_text_primary_only("tui.editor.cursorDown"))
            }
        } else {
            String::new()
        };
        let gap = (inner - 2.0 - visible_width(&counts) as f64 - visible_width(&open_hint) as f64)
            .max(1.0);
        let body = truncate_to_width(
            &format!(
                " {counts}{}{} ",
                " ".repeat(gap.floor() as usize),
                theme().fg("dim", &open_hint)
            ),
            inner,
            "\u{2026}",
            false,
        );
        let pad = " ".repeat((inner - visible_width(&body) as f64).max(0.0).floor() as usize);
        // Truncation may inject full ANSI resets; wrap each segment so the
        // selection background survives past them (custom-editor precedent).
        let content = if self.focused {
            format!("{body}{pad}")
                .split("\u{1b}[0m")
                .map(|segment| theme().bg("selectedBg", segment))
                .collect::<Vec<String>>()
                .join("\u{1b}[0m")
        } else {
            format!("{body}{pad}")
        };
        lines.push(top);
        lines.push(format!(
            "{}{content}{}",
            theme().fg("border", "\u{2502}"),
            theme().fg("border", "\u{2502}")
        ));
        lines.push(theme().fg(
            "border",
            &format!(
                "\u{2570}{}\u{256f}",
                "\u{2500}".repeat(inner.max(0.0).floor() as usize)
            ),
        ));
        lines
    }

    fn invalidate(&mut self) {
        // Render output is derived from counts and focus state.
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for SubagentSummaryLine {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }
}

/// `keyText(keybinding, { primaryOnly: true })`.
fn key_text_primary_only(keybinding: &str) -> String {
    key_text(keybinding, &KeyTextOptions { primary_only: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(
        id: &str,
        parent: &str,
        status: &str,
        resident: bool,
    ) -> AgentConnectionRlmChildAgentSnapshot {
        AgentConnectionRlmChildAgentSnapshot {
            id: id.to_string(),
            parent_id: Some(parent.to_string()),
            active_session_id: if resident {
                Some("active".to_string())
            } else {
                None
            },
            status: status.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn counts_only_direct_children_and_skips_cancelled() {
        let children = vec![
            snapshot("a", "p", "running", true),
            snapshot("b", "other", "running", true),
            snapshot("c", "p", "cancelled", true),
            snapshot("d", "p", "idle", true),
        ];
        let counts = count_direct_subagent_statuses(children.iter(), Some("p"));
        assert_eq!(counts.total, 2);
        assert_eq!(counts.running, 1);
        assert_eq!(counts.idle, 1);
    }

    #[test]
    fn queued_child_without_a_session_counts_as_running() {
        let child = snapshot("a", "p", "queued", false);
        assert_eq!(
            classify_subagent_snapshot_status(&child),
            AgentRosterStatus::Running
        );
    }

    #[test]
    fn summary_line_is_selectable_only_with_children_and_openable() {
        let mut line = SubagentSummaryLine::default();
        assert!(!line.is_selectable());
        line.set_subagent_counts(SubagentSummaryCounts {
            total: 1,
            ..Default::default()
        });
        assert!(!line.is_selectable());
        line.set_openable(true);
        assert!(line.is_selectable());
    }

    #[test]
    fn renders_no_box_without_children() {
        let mut line = SubagentSummaryLine::default();
        assert!(line.render(20.0).is_empty());
    }
}
