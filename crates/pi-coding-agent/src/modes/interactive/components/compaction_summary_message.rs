//! Port of packages/coding-agent/src/modes/interactive/components/compaction-summary-message.ts

use std::rc::Rc;

use pi_tui::components::markdown::Markdown;
use pi_tui::components::r#box::Box_;
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::Component;

use crate::core::messages::CompactionSummaryMessage;
use crate::modes::interactive::theme::theme::{theme, MarkdownTheme};

use super::expandable_custom_message::custom_message_label;
use super::keybinding_hints::expand_collapse_hint;

/// Compaction summary card: full markdown summary when expanded.
pub struct CompactionSummaryMessageComponent {
    message: CompactionSummaryMessage,
    markdown_theme: MarkdownTheme,
    /// Port of `ExpandableCustomMessageBox` (the TypeScript parent class): a
    /// `Box(1, 1, customMessageBg)` with a shared expanded flag.
    box_component: Box_,
    expanded: bool,
}

impl CompactionSummaryMessageComponent {
    pub fn new(message: CompactionSummaryMessage, markdown_theme: MarkdownTheme) -> Self {
        let mut component = Self {
            message,
            markdown_theme,
            box_component: super::expandable_custom_message::custom_message_box(),
            expanded: false,
        };
        component.update_display();
        component
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded == expanded {
            return;
        }
        self.expanded = expanded;
        self.update_display();
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    fn update_display(&mut self) {
        self.box_component.clear();

        let token_str = format_number_with_separators(self.message.tokens_before);
        let label = custom_message_label("compaction");
        self.box_component
            .add_child(Box::new(Text::new(label, 0, 0, None)));
        self.box_component.add_child(Box::new(Spacer::new(1)));

        let instructions = self.message.custom_instructions.clone();
        if self.expanded {
            let mut header = format!("**Compacted from {token_str} tokens**\n\n");
            if let Some(instructions) = &instructions {
                header += &format!("**Focus:** {instructions}\n\n");
            }
            self.box_component.add_child(Box::new(Markdown::new(
                format!("{header}{}", self.message.summary),
                0,
                0,
                self.markdown_theme.clone(),
                Some(pi_tui::components::markdown::DefaultTextStyle {
                    color: Some(Rc::new(|text: &str| theme().fg("customMessageText", text))),
                    ..Default::default()
                }),
                Default::default(),
            )));
        } else {
            let focus = match &instructions {
                Some(instructions) => format!(" \u{b7} focus: {instructions}"),
                None => String::new(),
            };
            self.box_component.add_child(Box::new(Text::new(
                format!(
                    "{} {}",
                    theme().fg(
                        "customMessageText",
                        &format!("Compacted from {token_str} tokens{focus}")
                    ),
                    expand_collapse_hint("app.tools.expand", false)
                ),
                0,
                0,
                None,
            )));
        }
    }
}

/// Port of `toLocaleString()` for the token count (thousands separators, no
/// fractional part for integral values).
pub fn format_number_with_separators(value: f64) -> String {
    let negative = value < 0.0;
    let value = value.abs();
    let integral = value.trunc();
    let digits = format!("{integral:.0}");
    let mut grouped = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    if negative {
        format!("-{grouped}")
    } else {
        grouped
    }
}

impl Component for CompactionSummaryMessageComponent {
    fn invalidate(&mut self) {
        self.box_component.invalidate();
        self.update_display();
    }

    fn render(&mut self, width: f64) -> Vec<String> {
        self.box_component.render(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::messages::create_compaction_summary_message;
    use crate::modes::interactive::theme::theme::get_markdown_theme;

    fn summary() -> CompactionSummaryMessage {
        create_compaction_summary_message(
            "the summary".to_string(),
            12345.0,
            "1970-01-01T00:00:00.000Z",
            Some("be terse".to_string()),
            None,
            None,
            None,
        )
    }

    #[test]
    fn collapsed_shows_token_count_and_focus() {
        let mut component = CompactionSummaryMessageComponent::new(summary(), get_markdown_theme());
        let lines = component.render(60.0);
        let joined = lines.join("\n");
        assert!(joined.contains("Compacted from 12,345 tokens"));
        assert!(joined.contains("focus: be terse"));
    }

    #[test]
    fn expanded_uses_a_markdown_header() {
        let mut component = CompactionSummaryMessageComponent::new(summary(), get_markdown_theme());
        component.set_expanded(true);
        let joined = component.render(60.0).join("\n");
        assert!(joined.contains("Compacted from 12,345 tokens"));
        assert!(joined.contains("Focus:"));
    }

    #[test]
    fn groups_thousands_like_to_locale_string() {
        assert_eq!(format_number_with_separators(12345.0), "12,345");
        assert_eq!(format_number_with_separators(999.0), "999");
        assert_eq!(format_number_with_separators(1000000.0), "1,000,000");
    }
}
