//! Display-only disclosure of restored Python state. The transcript/model keeps
//! the original notice, including every restored and failed name.
use pi_tui::components::text::Text;
use pi_tui::tui::Component;
use crate::modes::interactive::components::keybinding_hints::expand_collapse_hint;
use crate::modes::interactive::theme::theme::theme;

pub(super) struct RecoveryNotice {
    text: String,
    summary: String,
    warning: bool,
    expanded: bool,
}

fn name_count(text: &str, prefix: &str) -> Option<usize> {
    let line = text.lines().find_map(|line| line.split_once(prefix).map(|(_, names)| names))?;
    Some(line.trim_end_matches('.').split(", ").filter(|name| !name.trim().is_empty()).count())
}

impl RecoveryNotice {
    pub(super) fn new(text: String, expanded: bool) -> Self {
        let restored = name_count(&text, "These names are available again: ");
        let failed = name_count(&text, "These could not be restored and must be recreated if needed: ");
        let fresh = text.contains("kernel is starting fresh");
        let mut summary = match restored {
            Some(count) => format!("Python state restored: {count} variables."),
            None if fresh => "Python state not restored; starting fresh.".into(),
            None => "Python recovery notice: check details.".into(),
        };
        if let Some(count) = failed.filter(|count| *count > 0) {
            summary.push_str(&format!("\nWarning: {count} names not restored; recreate if needed."));
        }
        Self { text, summary, warning: fresh || failed.unwrap_or(0) > 0 || restored.is_none(), expanded }
    }

    pub(super) fn set_expanded(&mut self, expanded: bool) { self.expanded = expanded; }
}

impl Component for RecoveryNotice {
    fn render(&mut self, width: f64) -> Vec<String> {
        let mut text = theme().fg(if self.warning { "warning" } else { "dim" }, &self.summary);
        text.push_str(&format!("\n{}", expand_collapse_hint("app.tools.expand", self.expanded)));
        if self.expanded { text.push_str(&format!("\n{}", self.text)); }
        Text::new(text, 1, 0, None).render(width)
    }
    fn invalidate(&mut self) {}
}
