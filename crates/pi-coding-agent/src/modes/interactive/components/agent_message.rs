//! Port of packages/coding-agent/src/modes/interactive/components/agent-message.ts

use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::{Component, Container};
use pi_tui::utils::{truncate_to_width, visible_width, wrap_text_with_ansi};
use std::cell::RefCell;
use std::rc::Rc;

use crate::core::agent_messages::{
    format_agent_message_participant, AgentMessageDirection, AgentSessionMessageDetails,
    AGENT_MESSAGE_DIRECTION_RECEIVED,
};
use crate::modes::interactive::components::keybinding_hints::expand_collapse_hint;
use crate::modes::interactive::theme::theme::theme;

/// Port of the TypeScript `collapseText`: `text.replace(/\s+/g, " ").trim()`.
fn collapse_text(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut in_whitespace = false;
    let mut pending_space = false;
    for character in text.chars() {
        if character.is_whitespace() {
            in_whitespace = true;
            continue;
        }
        if in_whitespace {
            pending_space = true;
            in_whitespace = false;
        }
        if pending_space && !collapsed.is_empty() {
            collapsed.push(' ');
        }
        pending_space = false;
        collapsed.push(character);
    }
    collapsed
}

/// `◆ <label> · <participant>[ · <preview>]` summary line shared by received and
/// sent agent-message UI.
pub fn agent_message_summary_line(label: &str, participant: &str, preview: Option<&str>) -> String {
    let mut parts = vec![
        format!(
            "{} {}",
            theme().fg("accent", "\u{25c6}"),
            theme().fg("muted", label)
        ),
        theme().fg("muted", participant),
    ];
    if let Some(preview) = preview.filter(|preview| !preview.is_empty()) {
        parts.push(theme().fg("muted", preview));
    }
    parts.join(&theme().fg("dim", " \u{b7} "))
}

/// Single-line message preview sized to fit after the summary-line prefix.
pub fn agent_message_preview(prefix_width: usize, message: &str) -> String {
    let max_columns = std::cmp::max(20, 100usize.saturating_sub(prefix_width));
    truncate_to_width(&collapse_text(message), max_columns as f64, "", false)
}

/// `╰─`-guttered message body lines shared by received and sent agent-message UI.
pub fn agent_message_body_lines(message: &str, width: f64) -> Vec<String> {
    let safe_width = std::cmp::max(1, width.max(0.0).floor() as usize);
    let text_width = std::cmp::max(1, safe_width - 4);
    let body_lines: Vec<String> = message
        .split('\n')
        .flat_map(|line| {
            let wrapped = wrap_text_with_ansi(line, text_width);
            if wrapped.is_empty() {
                vec![String::new()]
            } else {
                wrapped
            }
        })
        .collect();
    body_lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let prefix = if index == 0 {
                theme().fg("dim", "\u{2570}\u{2500} ")
            } else {
                "   ".to_string()
            };
            truncate_to_width(
                &format!(" {prefix}{}", theme().fg("customMessageText", line)),
                safe_width as f64,
                "",
                false,
            )
        })
        .collect()
}

/// Port of `AgentMessageBodyComponent`.
struct AgentMessageBodyComponent {
    message: String,
}

impl Component for AgentMessageBodyComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        agent_message_body_lines(&self.message, width)
    }

    fn invalidate(&mut self) {}
}

/// Port of `AgentMessageComponent`.
pub struct AgentMessageComponent {
    message: AgentSessionMessageDetails,
    content: Rc<RefCell<Container>>,
    header: Rc<RefCell<Text>>,
    expanded: bool,
    container: Container,
}

impl AgentMessageComponent {
    /// Port of the `AgentMessageComponent` constructor.
    ///
    /// The TypeScript takes a markdown theme (unused by this component) and
    /// `{ suppressLeadingSpace }`; the port takes only the suppression flag.
    pub fn new(message: AgentSessionMessageDetails, suppress_leading_space: bool) -> Self {
        let mut container = Container::new();
        if !suppress_leading_space {
            container
                .add_child(Rc::new(RefCell::new(Spacer::new(1))) as Rc<RefCell<dyn Component>>);
        }
        let content = Rc::new(RefCell::new(Container::new()));
        container.add_child(Rc::clone(&content) as Rc<RefCell<dyn Component>>);

        let mut component = Self {
            message,
            content,
            header: Rc::new(RefCell::new(Text::new(String::new(), 1, 0, None))),
            expanded: false,
            container,
        };
        component.update_display();
        component
    }

    /// Port of `setExpanded`.
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

    /// Port of `updateDisplay`.
    fn update_display(&mut self) {
        let header_text = self.header_text();
        self.header.borrow_mut().set_text(header_text);
        let mut content = self.content.borrow_mut();
        content.clear();
        content.add_child(Rc::clone(&self.header) as Rc<RefCell<dyn Component>>);
        if self.expanded {
            content.add_child(Rc::new(RefCell::new(AgentMessageBodyComponent {
                message: self.message.message.clone(),
            })) as Rc<RefCell<dyn Component>>);
        }
    }

    /// Port of `headerText`.
    fn header_text(&self) -> String {
        let label = "Agent message received";
        let direction: AgentMessageDirection = AGENT_MESSAGE_DIRECTION_RECEIVED.to_string();
        let participant = format_agent_message_participant(
            &direction,
            self.message.from_relationship.as_deref(),
            self.message.from.as_ref(),
        );
        let hint = expand_collapse_hint("app.messages.expand", self.expanded);
        if self.expanded {
            return format!(
                "{} {hint}",
                agent_message_summary_line(label, &participant, None)
            );
        }

        let prefix_width = visible_width(&format!("\u{25c6} {label} \u{b7} {participant} \u{b7} "));
        let preview = agent_message_preview(prefix_width, &self.message.message);
        format!(
            "{} {hint}",
            agent_message_summary_line(label, &participant, Some(&preview))
        )
    }
}

impl Component for AgentMessageComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.container.render(width)
    }

    fn get_selection_regions(&self) -> Vec<pi_tui::selection_metadata::TableCellSelectionRegion> {
        self.container.get_selection_regions()
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
        self.update_display();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::interactive::theme::theme::init_theme;

    fn init() {
        init_theme(Some("prime"), false);
    }

    fn details(message: &str) -> AgentSessionMessageDetails {
        AgentSessionMessageDetails {
            id: "m1".to_string(),
            message: message.to_string(),
            from: None,
            from_relationship: Some("parent".to_string()),
            target: None,
        }
    }

    #[test]
    fn summary_line_joins_parts_with_a_dim_separator() {
        init();
        let line = agent_message_summary_line("Agent message received", "from parent", Some("hi"));
        let plain = strip_ansi(&line);
        assert!(plain.starts_with("\u{25c6} Agent message received"));
        assert!(plain.contains("from parent"));
        assert!(plain.ends_with("hi"));
    }

    #[test]
    fn preview_collapses_whitespace() {
        init();
        assert_eq!(
            agent_message_preview(0, "line one\n\n  line   two"),
            "line one line two"
        );
    }

    #[test]
    fn body_lines_gutter_from_the_first_line() {
        init();
        let lines = agent_message_body_lines("hello world", 20.0);
        assert_eq!(lines.len(), 1);
        assert_eq!(strip_ansi(&lines[0]), " \u{2570}\u{2500} hello world");
    }

    #[test]
    fn collapsed_header_includes_the_preview_and_hint() {
        init();
        let mut component = AgentMessageComponent::new(details("pong"), true);
        let lines = component.render(80.0);
        let plain = strip_ansi(&lines[0]);
        assert!(plain.contains("Agent message received"));
        assert!(plain.contains("pong"));
        assert!(plain.contains("to expand"));
    }

    #[test]
    fn expanded_header_drops_the_preview_and_shows_the_body() {
        init();
        let mut component = AgentMessageComponent::new(details("pong"), true);
        component.set_expanded(true);
        let lines = component.render(80.0);
        let plain = strip_ansi(&lines[0]);
        assert!(plain.contains("to collapse"));
        assert!(!plain.contains("pong"));
        assert!(strip_ansi(&lines[1]).contains("pong"));
    }

    #[test]
    fn leading_space_is_suppressed_on_request() {
        init();
        let mut spaced = AgentMessageComponent::new(details("x"), false);
        assert_eq!(spaced.render(40.0).len(), 2);
        let mut tight = AgentMessageComponent::new(details("x"), true);
        assert_eq!(tight.render(40.0).len(), 1);
    }

    /// Test helper: drop SGR sequences so assertions read the visible text.
    fn strip_ansi(text: &str) -> String {
        let mut result = String::new();
        let mut chars = text.chars().peekable();
        while let Some(character) = chars.next() {
            if character == '\u{1b}' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            } else {
                result.push(character);
            }
        }
        result
    }
}
