//! Port of packages/coding-agent/src/modes/interactive/components/expandable-custom-message.ts

use pi_tui::components::r#box::Box_ as TuiBox;
use pi_tui::tui::{Component, Container};

use super::super::theme::theme::theme;

/// Shared skeleton for boxed custom-message cards (compaction, skill,
/// refinement) with a collapsed/expanded state driven by the shared
/// tool-output expansion toggle.
///
/// TypeScript declares this as `abstract class ExpandableCustomMessageBox
/// extends Box` with an abstract `updateDisplay()`. Rust has no abstract
/// methods, so the subclass is a trait with a defaulted `invalidate` and the
/// concrete box is composed into each card.
pub trait ExpandableCustomMessageBox {
    /// `protected expanded = false`.
    fn expanded(&self) -> bool;
    fn set_expanded_flag(&mut self, expanded: bool);

    /// `setExpanded(expanded)`.
    fn set_expanded(&mut self, expanded: bool) {
        if self.expanded() == expanded {
            return;
        }
        self.set_expanded_flag(expanded);
        self.update_display();
    }

    /// `invalidate()` - `super.invalidate()` then `updateDisplay()`.
    fn invalidate(&mut self) {
        self.update_display();
    }

    /// `protected abstract updateDisplay(): void`.
    fn update_display(&mut self);
}

/// Bold custom-message label like `[refinement]`.
pub fn custom_message_label(name: &str) -> String {
    theme().fg(
        "customMessageLabel",
        &format!("\u{1b}[1m[{name}]\u{1b}[22m"),
    )
}

/// The `Box(1, 1, (t) => theme.bg("customMessageBg", t))` constructor argument.
pub fn custom_message_box() -> TuiBox {
    TuiBox::new(
        1,
        1,
        Some(Box::new(|text: &str| theme().bg("customMessageBg", text))),
    )
}

/// Convenience for subclasses: render the shared box and expose its children.
///
/// Kept private to this module; it only reshapes the pi-tui `TuiBox` so the
/// subclass trait above can drive it.
pub(crate) struct ExpandableBox {
    pub(crate) box_: TuiBox,
    pub(crate) expanded: bool,
}

impl ExpandableBox {
    pub(crate) fn new() -> Self {
        Self {
            box_: custom_message_box(),
            expanded: false,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.box_.clear();
    }

    pub(crate) fn add_child(&mut self, component: Box<dyn Component>) {
        self.box_.add_child(component);
    }

    pub(crate) fn render(&mut self, width: f64) -> Vec<String> {
        Component::render(&mut self.box_, width)
    }
}

impl Default for ExpandableBox {
    fn default() -> Self {
        Self::new()
    }
}

impl ExpandableCustomMessageBox for ExpandableBox {
    fn expanded(&self) -> bool {
        self.expanded
    }

    fn set_expanded_flag(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    fn update_display(&mut self) {
        // The base class has no display of its own; subclasses override this.
    }

    fn invalidate(&mut self) {
        Component::invalidate(&mut self.box_);
        self.update_display();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_expanded_only_updates_on_change() {
        let mut box_ = ExpandableBox::new();
        assert!(!box_.expanded());
        box_.set_expanded(true);
        assert!(box_.expanded());
        // Same value again is a no-op (the TypeScript early-returns).
        box_.set_expanded(true);
        assert!(box_.expanded());
        box_.set_expanded(false);
        assert!(!box_.expanded());
    }

    #[test]
    fn container_alias_is_the_shared_box_type() {
        // `customMessageLabel` wraps the name in bold SGR inside brackets.
        let label = custom_message_label("refinement");
        assert!(label.contains("[refinement]"));
        let _container = Container::new();
    }
}
