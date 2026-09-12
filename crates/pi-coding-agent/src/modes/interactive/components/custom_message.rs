//! Port of packages/coding-agent/src/modes/interactive/components/custom-message.ts

use std::rc::Rc;

use pi_agent_core::types::{CustomAgentMessage, CustomMessageContent};
use pi_tui::components::markdown::Markdown;
use pi_tui::components::r#box::Box_;
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::Component;
use pi_tui::utils::strip_ansi;

use crate::core::extensions::types::MessageRenderer;
use crate::modes::interactive::theme::theme::{get_markdown_theme, theme, MarkdownTheme};

use super::expandable_custom_message::custom_message_label;

/// `CustomMessage<unknown>` - the `custom` member of the agent message union.
pub type CustomMessage = CustomAgentMessage;

/// Port of `CustomMessageComponent`.
///
/// The TypeScript version is a `Container` with one spacer plus a rebuilt box.
/// Rust components own their children as boxed trait objects, so the port keeps
/// the spacer and rebuilds the same children list in `rebuild`.
pub struct CustomMessageComponent {
    message: CustomMessage,
    custom_renderer: Option<MessageRenderer>,
    box_component: Box_,
    custom_component: Option<Box<dyn Component>>,
    markdown_theme: MarkdownTheme,
    expanded: bool,
    spacer: Spacer,
    use_custom_component: bool,
}

/// The `customType` / `content` / `display` / `details` fields the component reads.
fn message_parts(message: &CustomMessage) -> (&str, &CustomMessageContent, bool) {
    match message {
        CustomAgentMessage::Custom {
            custom_type,
            content,
            display,
            ..
        } => (custom_type.as_str(), content, *display),
        _ => ("", &CustomMessageContent::Text(String::new()), false),
    }
}

impl CustomMessageComponent {
    pub fn new(
        message: CustomMessage,
        custom_renderer: Option<MessageRenderer>,
        markdown_theme: MarkdownTheme,
    ) -> Self {
        let mut component = Self {
            message,
            custom_renderer,
            box_component: Box_::new(
                1,
                1,
                Some(Box::new(|text: &str| theme().bg("customMessageBg", text))),
            ),
            custom_component: None,
            markdown_theme,
            expanded: false,
            spacer: Spacer::new(1),
            use_custom_component: false,
        };
        component.rebuild();
        component
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded {
            self.expanded = expanded;
            self.rebuild();
        }
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    fn rebuild(&mut self) {
        self.custom_component = None;
        self.use_custom_component = false;

        if let Some(renderer) = self.custom_renderer.clone() {
            let component = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                renderer(
                    self.message.clone(),
                    crate::core::extensions::types::MessageRenderOptions {
                        expanded: self.expanded,
                    },
                    (*theme()).clone(),
                )
            }))
            .ok()
            .flatten();
            if let Some(component) = component {
                self.custom_component = Some(Box::new(ArcComponent(component)));
                self.use_custom_component = true;
                return;
            }
        }

        // Fall back to the default renderer.
        self.box_component.clear();

        let (custom_type, content, _) = message_parts(&self.message);
        let label = theme().fg(
            "customMessageLabel",
            &format!("\u{1b}[1m[{custom_type}]\u{1b}[22m"),
        );
        let _ = custom_message_label(""); // keep the shared label helper referenced
        self.box_component
            .add_child(Box::new(Text::new(label, 0, 0, None)));
        self.box_component.add_child(Box::new(Spacer::new(1)));

        let text = match content {
            CustomMessageContent::Text(text) => text.clone(),
            CustomMessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    pi_agent_core::types::ContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<String>>()
                .join("\n"),
        };

        self.box_component.add_child(Box::new(Markdown::new(
            text,
            0,
            0,
            self.markdown_theme.clone(),
            Some(pi_tui::components::markdown::DefaultTextStyle {
                color: Some(Rc::new(|text: &str| theme().fg("customMessageText", text))),
                ..Default::default()
            }),
            Default::default(),
        )));
    }
}

/// Adapts an `Arc<dyn extensions::Component>` (the extension message renderer
/// contract) to the pi-tui `Component` trait the container renders.
struct ArcComponent(std::sync::Arc<dyn crate::core::extensions::types::Component>);

impl Component for ArcComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.0.render(width.max(0.0).floor() as usize)
    }

    fn invalidate(&mut self) {
        self.0.invalidate();
    }
}

impl Component for CustomMessageComponent {
    fn invalidate(&mut self) {
        self.spacer.invalidate();
        self.box_component.invalidate();
        if let Some(component) = self.custom_component.as_mut() {
            component.invalidate();
        }
        self.rebuild();
    }

    fn render(&mut self, width: f64) -> Vec<String> {
        let mut lines = self.spacer.render(width);
        if self.use_custom_component {
            if let Some(component) = self.custom_component.as_mut() {
                lines.extend(component.render(width));
            }
        } else {
            lines.extend(self.box_component.render(width));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom_message(custom_type: &str, content: &str) -> CustomMessage {
        CustomAgentMessage::Custom {
            custom_type: custom_type.to_string(),
            content: CustomMessageContent::Text(content.to_string()),
            display: true,
            details: None,
            timestamp: 0,
        }
    }

    #[test]
    fn default_renderer_labels_the_custom_type() {
        let mut component =
            CustomMessageComponent::new(custom_message("note", "body"), None, get_markdown_theme());
        let lines = component.render(20.0);
        let plain: Vec<String> = lines.iter().map(|line| strip_ansi(line)).collect();
        assert_eq!(plain[0], "");
        assert_eq!(plain[1], " [note]");
    }

    #[test]
    fn custom_renderer_takes_over_when_it_returns_a_component() {
        let renderer: MessageRenderer = std::sync::Arc::new(|_message, _options, _theme| {
            Some(std::sync::Arc::new(FixedComponent)
                as std::sync::Arc<
                    dyn crate::core::extensions::types::Component,
                >)
        });
        let mut component = CustomMessageComponent::new(
            custom_message("note", "body"),
            Some(renderer),
            get_markdown_theme(),
        );
        assert_eq!(component.render(10.0), vec!["custom".to_string()]);
    }

    struct FixedComponent;

    impl crate::core::extensions::types::Component for FixedComponent {
        fn render(&self, _width: usize) -> Vec<String> {
            vec!["custom".to_string()]
        }
    }
}
