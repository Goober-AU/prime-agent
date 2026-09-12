//! Port of packages/coding-agent/src/modes/interactive/components/slash-command-result-message.ts

use pi_tui::components::r#box::Box_;
use pi_tui::components::text::Text;
use pi_tui::tui::Component;

use crate::core::messages::CustomMessage;
use crate::modes::interactive::theme::theme::theme;

/// Port of `SessionSlashCommandResultMessage`: a `CustomMessage` whose
/// `customType` is `SESSION_SLASH_COMMAND_RESULT_CUSTOM_TYPE` and whose `content`
/// is a string. The port takes the already-narrowed message.
pub struct SlashCommandResultMessageComponent {
    content_box: Box_,
}

impl SlashCommandResultMessageComponent {
    pub fn new(message: &CustomMessage) -> Self {
        let background = theme().get_user_message_background_color();
        let mut content_box = Box_::new(2, 1, Some(Box::new(move |text: &str| background(text))));
        content_box.add_child(Box::new(Text::new(
            custom_message_text(message),
            0,
            0,
            None,
        )));
        Self { content_box }
    }

    /// Port of `setExpanded` - a no-op for this component.
    pub fn set_expanded(&mut self, _expanded: bool) {}
}

/// `SessionSlashCommandResultMessage.content` is declared `string`.
fn custom_message_text(message: &CustomMessage) -> String {
    match &message.content {
        pi_agent_core::types::CustomMessageContent::Text(text) => text.clone(),
        pi_agent_core::types::CustomMessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block.as_text() {
                Some(text) => text.to_string(),
                None => "[image]".to_string(),
            })
            .collect::<Vec<String>>()
            .join("\n"),
    }
}

impl Component for SlashCommandResultMessageComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.content_box.render(width)
    }

    fn invalidate(&mut self) {
        self.content_box.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::messages::{
        create_session_slash_command_result_message, SessionSlashCommandResultDetails, SessionSlashCommand,
    };

    fn message(content: &str) -> CustomMessage {
        create_session_slash_command_result_message(
            content.to_string(),
            SessionSlashCommandResultDetails {
                command: SessionSlashCommand {
                    name: "compact".to_string(),
                    args: String::new(),
                    text: "/compact".to_string(),
                },
                success: true,
                severity: "info".to_string(),
                error: None,
                command_entry_id: None,
            },
            true,
            0,
        )
    }

    #[test]
    fn renders_the_message_content_inside_a_padded_box() {
        let mut component = SlashCommandResultMessageComponent::new(&message("done"));
        let lines = component.render(12.0);
        assert!(lines.iter().any(|line| line.contains("done")));
    }
}
