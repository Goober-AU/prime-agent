//! Port of packages/coding-agent/src/modes/interactive/components/compaction-summary-message.ts

use std::rc::Rc;
use std::sync::Arc;

use pi_tui::components::markdown::{Markdown, MarkdownTheme as TuiMarkdownTheme};
use pi_tui::components::r#box::Box_;
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::Component;

use crate::core::messages::CompactionSummaryMessage;
use crate::modes::interactive::theme::theme::{theme, MarkdownTheme};

use super::expandable_custom_message::custom_message_label;
use super::keybinding_hints::expand_collapse_hint;

/// `theme.ts`'s `MarkdownTheme` carries `Arc` closures with `Send + Sync`; the
/// `pi-tui` markdown component holds `Rc` closures without those bounds. Private
/// port of the conversion the other message cards apply.
fn to_tui_markdown_theme(source: MarkdownTheme) -> TuiMarkdownTheme {
    fn rc(value: Arc<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
        Rc::new(move |text: &str| value(text))
    }

    TuiMarkdownTheme {
        heading: rc(source.heading),
        link: rc(source.link),
        link_url: rc(source.link_url),
        code: rc(source.code),
        code_block: rc(source.code_block),
        code_block_border: rc(source.code_block_border),
        quote: rc(source.quote),
        quote_border: rc(source.quote_border),
        hr: rc(source.hr),
        list_bullet: rc(source.list_bullet),
        bold: rc(source.bold),
        italic: rc(source.italic),
        strikethrough: rc(source.strikethrough),
        underline: rc(source.underline),
        highlight_code: Some(Rc::new(move |code: &str, language: Option<&str>| {
            (source.highlight_code)(code, language)
        })),
        code_block_indent: source.code_block_indent,
        math: Some(rc(source.math)),
        math_block: Some(rc(source.math_block)),
    }
}

/// Compaction summary card: full markdown summary when expanded.
pub struct CompactionSummaryMessageComponent {
    message: CompactionSummaryMessage,
    markdown_theme: TuiMarkdownTheme,
    /// Port of `ExpandableCustomMessageBox` (the TypeScript parent class): a
    /// `Box(1, 1, customMessageBg)` with a shared expanded flag.
    box_component: Box_,
    expanded: bool,
}

impl CompactionSummaryMessageComponent {
    pub fn new(message: CompactionSummaryMessage, markdown_theme: MarkdownTheme) -> Self {
        let mut component = Self {
            message,
            markdown_theme: to_tui_markdown_theme(markdown_theme),
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
                clone_tui_markdown_theme(&self.markdown_theme),
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

/// The `pi-tui` theme closures are `Rc`, so a fresh owned theme can share them.
fn clone_tui_markdown_theme(source: &TuiMarkdownTheme) -> TuiMarkdownTheme {
    TuiMarkdownTheme {
        heading: Rc::clone(&source.heading),
        link: Rc::clone(&source.link),
        link_url: Rc::clone(&source.link_url),
        code: Rc::clone(&source.code),
        code_block: Rc::clone(&source.code_block),
        code_block_border: Rc::clone(&source.code_block_border),
        quote: Rc::clone(&source.quote),
        quote_border: Rc::clone(&source.quote_border),
        hr: Rc::clone(&source.hr),
        list_bullet: Rc::clone(&source.list_bullet),
        bold: Rc::clone(&source.bold),
        italic: Rc::clone(&source.italic),
        strikethrough: Rc::clone(&source.strikethrough),
        underline: Rc::clone(&source.underline),
        highlight_code: source.highlight_code.as_ref().map(Rc::clone),
        code_block_indent: source.code_block_indent.clone(),
        math: source.math.as_ref().map(Rc::clone),
        math_block: source.math_block.as_ref().map(Rc::clone),
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
