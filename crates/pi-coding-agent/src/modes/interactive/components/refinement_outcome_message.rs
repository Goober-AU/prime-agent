//! Port of packages/coding-agent/src/modes/interactive/components/refinement-outcome-message.ts

use serde_json::{Map, Value};

use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::Component;
use pi_tui::utils::{truncate_to_width, visible_width};

use crate::core::messages::RefinementOutcomeDetails;
use crate::core::refinement::refinement::{AppliedRefinementEdit, HarnessEntry, HarnessScope};
use crate::core::tools::edit_diff::generate_diff_string;

use super::super::theme::theme::theme;
use super::diff::{render_diff, RenderDiffOptions};
use super::expandable_custom_message::{
    custom_message_label, ExpandableBox, ExpandableCustomMessageBox,
};
use super::keybinding_hints::expand_collapse_hint;

/// `editableEntry(entry)`.
fn editable_entry(entry: &HarnessEntry) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("title".to_string(), Value::String(entry.title.clone()));
    map.insert("content".to_string(), Value::String(entry.content.clone()));
    map.insert("path".to_string(), Value::String(entry.path.clone()));
    map.insert(
        "reference".to_string(),
        Value::Object(entry.reference.clone()),
    );
    map.insert(
        "arguments".to_string(),
        Value::Object(entry.arguments.clone()),
    );
    map.insert(
        "metadata".to_string(),
        Value::Object(entry.metadata.clone()),
    );
    map
}

/// `proposedEntry(edit)` - only the supplied keys are present.
fn proposed_entry(edit: &AppliedRefinementEdit) -> Map<String, Value> {
    let mut map = Map::new();
    if let Some(title) = &edit.edit.title {
        map.insert("title".to_string(), Value::String(title.clone()));
    }
    if let Some(content) = &edit.edit.content {
        map.insert("content".to_string(), Value::String(content.clone()));
    }
    if let Some(path) = &edit.edit.path {
        map.insert("path".to_string(), Value::String(path.clone()));
    }
    if let Some(reference) = &edit.edit.reference {
        map.insert("reference".to_string(), Value::Object(reference.clone()));
    }
    if let Some(arguments) = &edit.edit.arguments {
        map.insert("arguments".to_string(), Value::Object(arguments.clone()));
    }
    if let Some(metadata) = &edit.edit.metadata {
        map.insert("metadata".to_string(), Value::Object(metadata.clone()));
    }
    map
}

/// `JSON.stringify(entry, null, 2)`.
fn json_pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| String::new())
}

/// `editDiff(edit)`.
pub fn edit_diff(edit: &AppliedRefinementEdit) -> String {
    let before = edit.before.as_ref().map(editable_entry);
    let after = match &edit.after {
        Some(after) => Some(editable_entry(after)),
        None => {
            if edit.edit.action == "delete" {
                None
            } else {
                Some(proposed_entry(edit))
            }
        }
    };
    let before_text = before.as_ref().map(json_pretty).unwrap_or_default();
    let after_text = after.as_ref().map(json_pretty).unwrap_or_default();
    let before_text = if before.is_some() {
        format!("{before_text}\n")
    } else {
        String::new()
    };
    let after_text = if after.is_some() {
        format!("{after_text}\n")
    } else {
        String::new()
    };
    generate_diff_string(&before_text, &after_text, 4, 1).0
}

/// `editScope(edit, fallback)`.
pub fn edit_scope(edit: &AppliedRefinementEdit, fallback: HarnessScope) -> HarnessScope {
    if let Some(after) = &edit.after {
        if let Some(scope) = after.scope {
            return scope;
        }
    }
    if let Some(before) = &edit.before {
        if let Some(scope) = before.scope {
            return scope;
        }
    }
    fallback
}

/// `editLabel(edit, fallbackScope)`.
pub fn edit_label(edit: &AppliedRefinementEdit, fallback_scope: HarnessScope) -> String {
    let scope = edit_scope(edit, fallback_scope);
    if !edit.applied {
        let error = edit
            .error
            .as_ref()
            .map(|error| format!(": {error}"))
            .unwrap_or_default();
        return theme().fg(
            "error",
            &format!(
                "Failed to {} {} {} `{}`{error}",
                edit.edit.action,
                scope.as_str(),
                edit.edit.kind,
                edit.id
            ),
        );
    }
    let verb = match edit.edit.action.as_str() {
        "create" => "Created",
        "update" => "Updated",
        _ => "Deleted",
    };
    format!(
        "{} {} {} `{}`",
        theme().fg("success", verb),
        scope.as_str(),
        edit.edit.kind,
        edit.id
    )
}

/// `editCount(edits)`.
pub fn edit_count(edits: &[AppliedRefinementEdit]) -> String {
    let applied = edits.iter().filter(|edit| edit.applied).count();
    if edits.len() == applied {
        format!(
            "{applied} edit{} applied",
            if applied == 1 { "" } else { "s" }
        )
    } else {
        format!("{applied}/{} edits applied", edits.len())
    }
}

/// Width-aware collapsed line: truncates the summary so the line never wraps.
///
/// The TypeScript `CollapsedOutcomeLine implements Component`; the port keeps it
/// as a private render helper plus a `Component` wrapper for the container.
pub struct CollapsedOutcomeLine {
    summary: String,
    suffix: String,
}

impl CollapsedOutcomeLine {
    pub fn new(summary: &str, suffix: &str) -> Self {
        Self {
            summary: summary.to_string(),
            suffix: suffix.to_string(),
        }
    }
}

impl Component for CollapsedOutcomeLine {
    fn render(&mut self, width: f64) -> Vec<String> {
        let room = 20.0f64.max(width - visible_width(&self.suffix) as f64 - 1.0);
        let line = format!(
            "{} {}",
            theme().fg(
                "customMessageText",
                &truncate_to_width(&self.summary, room, "\u{2026}", false)
            ),
            self.suffix
        );
        vec![truncate_to_width(&line, 1.0f64.max(width), "", false)]
    }

    fn invalidate(&mut self) {}
}

/// Durable refinement outcome card: per-edit rows with before/after diffs when expanded.
pub struct RefinementOutcomeMessageComponent {
    message: RefinementOutcomeDetails,
    box_: ExpandableBox,
}

impl RefinementOutcomeMessageComponent {
    pub fn new(message: RefinementOutcomeDetails) -> Self {
        let mut component = Self {
            message,
            box_: ExpandableBox::new(),
        };
        component.update_display();
        component
    }

    /// `setExpanded(expanded)` from the shared base class.
    pub fn set_expanded(&mut self, expanded: bool) {
        ExpandableCustomMessageBox::set_expanded(self, expanded);
    }

    pub fn expanded(&self) -> bool {
        self.box_.expanded
    }

    /// `protected updateDisplay()`.
    pub fn update_display(&mut self) {
        self.box_.clear();

        let summary = self.message.summary.clone();
        let edits = self.message.edits.clone();
        let scope = self.message.scope;

        self.box_.add_child(Box::new(Text::new(
            custom_message_label("refinement"),
            0,
            0,
            None,
        )));
        self.box_.add_child(Box::new(Spacer::new(1)));
        if !self.box_.expanded {
            let suffix = format!(
                "{} {}",
                theme().fg("customMessageText", &format!("· {}", edit_count(&edits))),
                expand_collapse_hint("app.tools.expand", false)
            );
            self.box_
                .add_child(Box::new(CollapsedOutcomeLine::new(&summary, &suffix)));
            return;
        }

        self.box_.add_child(Box::new(Text::new(
            theme().fg(
                "customMessageText",
                &format!("{summary} · {}", edit_count(&edits)),
            ),
            0,
            0,
            None,
        )));
        for edit in &edits {
            self.box_.add_child(Box::new(Text::new(
                format!("{}{}", theme().fg("dim", "  ╰─ "), edit_label(edit, scope)),
                0,
                0,
                None,
            )));
            let diff = edit_diff(edit);
            if !diff.is_empty() {
                self.box_.add_child(Box::new(Text::new(
                    render_diff(&diff, RenderDiffOptions::default()),
                    4,
                    0,
                    None,
                )));
            }
        }
    }
}

impl ExpandableCustomMessageBox for RefinementOutcomeMessageComponent {
    fn expanded(&self) -> bool {
        self.box_.expanded
    }

    fn set_expanded_flag(&mut self, expanded: bool) {
        self.box_.expanded = expanded;
    }

    fn update_display(&mut self) {
        RefinementOutcomeMessageComponent::update_display(self);
    }
}

impl Component for RefinementOutcomeMessageComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.box_.render(width)
    }

    fn invalidate(&mut self) {
        ExpandableCustomMessageBox::invalidate(self);
    }
}

/// Port of `MalformedRefinementOutcomeMessageComponent`.
pub struct MalformedRefinementOutcomeMessageComponent {
    box_: ExpandableBox,
}

impl MalformedRefinementOutcomeMessageComponent {
    pub fn new() -> Self {
        let mut component = Self {
            box_: ExpandableBox::new(),
        };
        component.update_display();
        component
    }

    /// `protected updateDisplay()`.
    pub fn update_display(&mut self) {
        self.box_.clear();
        self.box_.add_child(Box::new(Text::new(
            theme().fg("error", "[Malformed refinement outcome message]"),
            0,
            0,
            None,
        )));
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        ExpandableCustomMessageBox::set_expanded(self, expanded);
    }
}

impl Default for MalformedRefinementOutcomeMessageComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl ExpandableCustomMessageBox for MalformedRefinementOutcomeMessageComponent {
    fn expanded(&self) -> bool {
        self.box_.expanded
    }

    fn set_expanded_flag(&mut self, expanded: bool) {
        self.box_.expanded = expanded;
    }

    fn update_display(&mut self) {
        MalformedRefinementOutcomeMessageComponent::update_display(self);
    }
}

impl Component for MalformedRefinementOutcomeMessageComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.box_.render(width)
    }

    fn invalidate(&mut self) {
        ExpandableCustomMessageBox::invalidate(self);
    }
}
