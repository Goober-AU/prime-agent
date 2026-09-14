//! Port of packages/coding-agent/src/modes/interactive/components/slash-command-message.ts

use pi_tui::components::r#box::Box_ as TuiBox;
use pi_tui::components::text::Text;
use pi_tui::tui::{Component, Container};

use crate::core::slash_commands::{builtin_slash_command_takes_argument, parse_slash_command};

use super::super::theme::theme::theme;
use super::prompt_highlight::style_argument_tokens;

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

/// Port of `isLeadingSlashCommand`.
pub fn is_leading_slash_command(text: &str, is_recognized: &dyn Fn(&str) -> bool) -> bool {
    let command = parse_slash_command(text);
    match command {
        Some(command) => is_recognized(&command.name),
        None => false,
    }
}

/// Port of `styleSlashCommandText`.
///
/// The TypeScript `styleRest?` default is `styleArgumentTokens(rest, undefined, includeBareSeparator)`;
/// the port takes an explicit closure so the same default can be passed in.
pub fn style_slash_command_text(text: &str, style_rest: &dyn Fn(&str, bool) -> String) -> String {
    let parsed = parse_slash_command(text);
    // `command.name.length` counts UTF-16 code units in the TypeScript.
    let command_end = match &parsed {
        Some(parsed) => parsed.name.encode_utf16().count() + 1,
        None => text.len(),
    };
    // Matches the editor's gate: a bare -- is only meaningful in commands that take arguments.
    let include_bare_separator = match &parsed {
        Some(parsed) => builtin_slash_command_takes_argument(&parsed.name),
        None => false,
    };
    format!(
        "{}{}",
        theme().fg("accent", &text[..command_end.min(text.len())]),
        style_rest(&text[command_end.min(text.len())..], include_bare_separator)
    )
}

/// Port of `styleSlashCommandText`'s default `styleRest` argument.
pub fn style_slash_command_text_default(text: &str) -> String {
    style_slash_command_text(text, &|rest: &str, include_bare_separator: bool| {
        style_argument_tokens(
            rest,
            &|segment: &str| segment.to_string(),
            include_bare_separator,
        )
    })
}

/// Renders a durable session command with the same layout as a user message.
pub struct SlashCommandMessageComponent {
    container: Container,
    /// The TypeScript keeps the `contentBox` field; it is the sole child of the
    /// container, so the port keeps it in place inside `container.children`.
    content_box_index: usize,
}

impl SlashCommandMessageComponent {
    pub fn new(text: &str) -> Self {
        let mut content_box = TuiBox::new(
            2,
            1,
            Some(Box::new(|content: &str| {
                theme().get_user_message_background_color()(content)
            })),
        );
        content_box.add_child(Box::new(Text::new(
            style_slash_command_text_default(text),
            0,
            0,
            None,
        )));
        let mut container = Container::new();
        container.add_child(std::rc::Rc::new(std::cell::RefCell::new(content_box)));
        Self {
            container,
            content_box_index: 0,
        }
    }

    /// `setExpanded(_expanded)` - the message has no expanded state.
    pub fn set_expanded(&mut self, _expanded: bool) {}

    /// Port of `render(width)` in the subclass.
    pub fn render(&mut self, width: f64) -> Vec<String> {
        let mut lines = Component::render(&mut self.container, width);
        if lines.is_empty() {
            return lines;
        }
        let last = lines.len() - 1;
        lines[0] = format!("{OSC133_ZONE_START}{}", lines[0]);
        lines[last] = format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}{}", lines[last]);
        lines
    }

    /// `contentBox` - the child box that carries the user-message background.
    pub fn content_box(&self) -> std::rc::Rc<std::cell::RefCell<dyn Component>> {
        std::rc::Rc::clone(&self.container.children[self.content_box_index])
    }
}

impl Component for SlashCommandMessageComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        SlashCommandMessageComponent::render(self, width)
    }

    fn invalidate(&mut self) {
        Component::invalidate(&mut self.container);
    }
}
