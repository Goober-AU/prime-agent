//! TypeScript's mounted subagent summary, roster subscription and scoped handoff.

use super::*;
use crate::modes::agent_connection::daemon_agent_connection::AgentConnectionRosterStore;
use crate::modes::interactive::components::subagent_summary_line::{
    RosterParent, SubagentSummaryCounts, SubagentSummaryLine,
};
use pi_tui::tui::Focusable;

pub(super) struct Bar {
    mode: Rc<RefCell<InteractiveMode>>,
    editor: Option<Rc<RefCell<CustomEditor>>>,
    line: SubagentSummaryLine,
    roster: Arc<Mutex<Option<Arc<dyn AgentConnectionRosterStore>>>>,
}

impl Bar {
    pub(super) fn new(mode: Rc<RefCell<InteractiveMode>>) -> Self {
        let mut line = SubagentSummaryLine::default();
        line.set_openable(mode.borrow().options.return_to_agents_view);
        Self {
            mode,
            editor: None,
            line,
            roster: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) fn subscribe(
        &self,
        connection: Arc<dyn wire::AgentConnection>,
        send: mpsc::Sender<HostEvent>,
    ) {
        let slot = self.roster.clone();
        tokio::spawn(async move {
            let update = send.clone();
            if let Ok(store) = connection
                .subscribe_agent_roster(Arc::new(move || {
                    let _ = update.send(HostEvent::Render);
                }))
                .await
            {
                *slot.lock().unwrap() = Some(store);
                let _ = send.send(HostEvent::Render);
            }
        });
    }

    fn counts(&self) -> SubagentSummaryCounts {
        use crate::modes::interactive::components::subagent_summary_line::count_roster_subagent_statuses;
        let mode = self.mode.borrow();
        let summaries = self
            .roster
            .lock()
            .unwrap()
            .as_ref()
            .map(|store| store.summaries());
        if let (Some(values), Some(state)) = (summaries, &mode.connection_state) {
            let rows: Vec<crate::modes::daemon::daemon_session_list::SessionSummary> = values
                .into_iter()
                .filter_map(|row| serde_json::from_value(row).ok())
                .collect();
            if rows.iter().any(|row| row.session_id == state.session_id) {
                return count_roster_subagent_statuses(
                    &rows,
                    &RosterParent {
                        active_session_id: state.active_session_id.clone(),
                        session_id: Some(state.session_id.clone()),
                        session_file: state.session_file.clone(),
                    },
                );
            }
        }
        let children: Vec<_> = mode.subagent_snapshots.values().cloned().collect();
        let counts =
            super::super::count_direct_subagent_statuses(&children, mode.rlm_node_id.as_deref());
        SubagentSummaryCounts {
            total: counts.total,
            running: counts.running,
            idle: counts.idle,
            inactive: counts.inactive,
        }
    }

    /// Returns true when the tray consumed input; all other input stays in chat.
    pub(super) fn input(
        &mut self,
        data: &str,
        editor: &Rc<RefCell<CustomEditor>>,
        actions: &Rc<RefCell<Vec<InputAction>>>,
    ) -> bool {
        self.editor = Some(editor.clone());
        self.line.set_subagent_counts(self.counts());
        let keys = pi_tui::keybindings::get_keybindings();
        if self.line.focused() {
            if !self.line.is_selectable() {
                self.line.set_focused(false);
                editor.borrow_mut().editor_mut().set_focused(true);
                return false;
            }
            if keys.matches(data, "tui.select.confirm") || keys.matches(data, "app.agents.open") {
                actions.borrow_mut().push(InputAction::Subagents);
                return true;
            }
            self.line.set_focused(false);
            editor.borrow_mut().editor_mut().set_focused(true);
            return keys.matches(data, "tui.select.up")
                || keys.matches(data, "tui.select.cancel")
                || keys.matches(data, "app.agents.back");
        }
        if self.line.is_selectable() && keys.matches(data, "tui.editor.cursorDown") {
            let at_end = {
                let editor = editor.borrow();
                let text = editor.editor().get_text();
                editor.editor().get_cursor().0 + 1 >= text.lines().count().max(1)
                    && !editor.editor().is_showing_autocomplete()
            };
            if at_end {
                self.line.set_focused(true);
                editor.borrow_mut().editor_mut().set_focused(false);
                return true;
            }
        }
        false
    }
}

impl TuiComponent for Bar {
    fn render(&mut self, width: f64) -> Vec<String> {
        let counts = self.counts();
        self.line.set_subagent_counts(counts);
        if !self.line.is_selectable() && self.line.focused() {
            self.line.set_focused(false);
            if let Some(editor) = &self.editor {
                editor.borrow_mut().editor_mut().set_focused(true);
            }
        }
        let mut lines = self.line.render(width);
        if counts.running > 0 && !self.mode.borrow().is_agent_streaming() {
            lines.push(truncate_to_width(
                &theme().fg(
                    "muted",
                    &format!(
                        "Waiting for {} subagent{}",
                        counts.running,
                        if counts.running == 1 { "" } else { "s" }
                    ),
                ),
                width,
                "…",
                false,
            ));
        }
        lines
    }
    fn invalidate(&mut self) {
        self.line.invalidate();
    }
}

pub(super) fn seed(mode: &Rc<RefCell<InteractiveMode>>, snapshot: &wire::AgentConnectionSnapshot) {
    let mut mode = mode.borrow_mut();
    mode.rlm_node_id = snapshot
        .parent
        .as_ref()
        .and_then(|parent| parent.child_id.clone());
    let children = snapshot
        .children
        .as_ref()
        .into_iter()
        .flatten()
        .map(project_child)
        .collect::<Vec<_>>();
    mode.replace_subagent_summary(Some(&children));
}

pub(super) fn project_child(
    child: &wire::AgentConnectionRlmChildAgentSnapshot,
) -> local::AgentConnectionRlmChildAgentSnapshot {
    local::AgentConnectionRlmChildAgentSnapshot {
        id: child.id.clone(),
        parent_id: child.parent_id.clone(),
        active_session_id: child.active_session_id.clone(),
        session_name: child.session_name.clone(),
        model: child.model.clone(),
        label: child.label.clone(),
        status: child.status.clone(),
        duration_ms: child.duration_ms,
        answer_preview: child.answer_preview.clone(),
        replied_since_task: child.replied_since_task,
        tool_use_count: child.tool_use_count,
        token_count: child.token_count,
        recap: child.recap.clone(),
        session_dir: child.session_dir.clone(),
        activity: child
            .activity
            .as_ref()
            .map(|activity| activity.kind.clone()),
        error: child.error.clone(),
    }
}
