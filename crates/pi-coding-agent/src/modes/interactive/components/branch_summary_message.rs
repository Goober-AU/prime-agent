//! Port of
//! packages/coding-agent/src/modes/interactive/components/branch-summary-message.ts

use std::cell::RefCell;
use std::rc::Rc;

use pi_tui::components::markdown::{Markdown, MarkdownOptions, MarkdownTheme};
use pi_tui::components::r#box::Box_;
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::Component;

use crate::modes::interactive::components::keybinding_hints::expand_collapse_hint;
use crate::modes::interactive::theme::theme::theme;

/// `pi-tui`'s `MarkdownTheme` holds `Rc` closures; `theme.ts`'s holds `Box`
/// closures with `Send + Sync`. This is the same conversion as
/// `toTuiMarkdownTheme` in the other message components.
fn to_tui_markdown_theme(
    source: crate::modes::interactive::theme::theme::MarkdownTheme,
) -> MarkdownTheme {
    fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
        Rc::new(move |text: &str| value(text))
    }

    MarkdownTheme {
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

/// The converted theme holds `Rc` closures, so a fresh owned theme can share them
/// (`pi-tui`'s `MarkdownTheme` is not `Clone`, like the TypeScript object which is
/// reused by reference on every `updateDisplay`).
fn clone_tui_markdown_theme(source: &MarkdownTheme) -> MarkdownTheme {
    MarkdownTheme {
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
        highlight_code: source.highlight_code.clone(),
        code_block_indent: source.code_block_indent.clone(),
        math: source.math.clone(),
        math_block: source.math_block.clone(),
    }
}

/// Component that renders a branch summary message with collapsed/expanded state.
/// Uses same background color as custom messages for visual consistency.
pub struct BranchSummaryMessageComponent {
    expanded: bool,
    summary: String,
    markdown_theme: MarkdownTheme,
    /// `Box` child that owns the rendered content (`this` in the TypeScript,
    /// which extends `Box`).
    box_: Rc<RefCell<Box_>>,
    selection_regions: Vec<pi_tui::selection_metadata::TableCellSelectionRegion>,
}

impl BranchSummaryMessageComponent {
    /// Port of the `BranchSummaryMessageComponent` constructor.
    ///
    /// The TypeScript takes the `BranchSummaryMessage`; the port takes its
    /// `summary` field because the port's `BranchSummaryMessage` carries the
    /// remaining fields (`role`, `fromId`, `timestamp`) unused by this component.
    pub fn new(summary: String, markdown_theme: MarkdownTheme) -> Self {
        let background = theme().get_user_message_background_color();
        let box_ = Rc::new(RefCell::new(Box_::new(
            1,
            1,
            Some(Box::new(move |text: &str| background(text))),
        )));
        let mut component = Self {
            expanded: false,
            summary,
            markdown_theme,
            box_: Rc::clone(&box_),
            selection_regions: Vec::new(),
        };
        component.update_display();
        component
    }

    /// Port of `setExpanded`.
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        self.update_display();
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Port of `updateDisplay`.
    fn update_display(&mut self) {
        let mut box_ = self.box_.borrow_mut();
        box_.clear();

        let label = theme().fg("customMessageLabel", "\u{1b}[1m[branch]\u{1b}[22m");
        box_.add_child(Box::new(Text::new(label, 0, 0, None)));
        box_.add_child(Box::new(Spacer::new(1)));

        if self.expanded {
            let header = "**Branch Summary**\n\n";
            let text = format!("{header}{}", self.summary);
            let default_style = pi_tui::components::markdown::DefaultTextStyle {
                color: Some(std::rc::Rc::new(|text: &str| {
                    theme().fg("customMessageText", text)
                })),
                bg_color: None,
                bold: false,
                italic: false,
                strikethrough: false,
                underline: false,
            };
            box_.add_child(Box::new(Markdown::new(
                text,
                0,
                0,
                clone_tui_markdown_theme(&self.markdown_theme),
                Some(default_style),
                MarkdownOptions::default(),
            )));
        } else {
            box_.add_child(Box::new(Text::new(
                format!(
                    "{} {}",
                    theme().fg("customMessageText", "Branch summary"),
                    expand_collapse_hint("app.tools.expand", false)
                ),
                0,
                0,
                None,
            )));
        }
    }
}

impl Component for BranchSummaryMessageComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        let mut box_ = self.box_.borrow_mut();
        let lines = box_.render(width);
        let regions = box_.get_selection_regions();
        drop(box_);
        self.selection_regions = regions;
        lines
    }

    fn get_selection_regions(&self) -> Vec<pi_tui::selection_metadata::TableCellSelectionRegion> {
        self.selection_regions.clone()
    }

    fn invalidate(&mut self) {
        // `invalidate` calls `super.invalidate()` then `updateDisplay()`.
        self.box_.borrow_mut().invalidate();
        self.update_display();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::interactive::theme::theme::{get_markdown_theme, init_theme};

    fn init() {
        init_theme(Some("prime"), false);
    }

    fn component() -> BranchSummaryMessageComponent {
        BranchSummaryMessageComponent::new(
            "Earlier work summarised.".to_string(),
            get_markdown_theme(),
        )
    }

    #[test]
    fn collapsed_renders_label_and_hint() {
        init();
        let mut component = component();
        let lines = component.render(40.0);
        assert!(lines.iter().any(|line| line.contains("[branch]")));
        assert!(lines.iter().any(|line| line.contains("Branch summary")));
        assert!(lines.iter().any(|line| line.contains("to expand")));
    }

    #[test]
    fn expanded_renders_the_markdown_body() {
        init();
        let mut component = component();
        component.set_expanded(true);
        let lines = component.render(40.0);
        assert!(lines.iter().any(|line| line.contains("[branch]")));
        assert!(lines.iter().any(|line| line.contains("Branch Summary")));
        assert!(lines.iter().any(|line| line.contains("Earlier work")));
        assert!(!lines.iter().any(|line| line.contains("to expand")));
    }

    #[test]
    fn invalidate_rebuilds_the_children() {
        init();
        let mut component = component();
        let before = component.render(30.0);
        component.invalidate();
        assert_eq!(component.render(30.0), before);
    }
}
