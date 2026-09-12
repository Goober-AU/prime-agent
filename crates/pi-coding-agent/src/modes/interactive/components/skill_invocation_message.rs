//! Port of packages/coding-agent/src/modes/interactive/components/skill-invocation-message.ts

use std::rc::Rc;

use pi_tui::components::markdown::Markdown;
use pi_tui::components::r#box::Box_;
use pi_tui::components::text::Text;
use pi_tui::tui::Component;

use crate::core::skill_blocks::ParsedSkillBlock;
use crate::modes::interactive::theme::theme::{get_markdown_theme, theme, MarkdownTheme};

use super::expandable_custom_message::{custom_message_label, ExpandableCustomMessageBox};

/// Skill invocation card; the user message is rendered separately.
pub struct SkillInvocationMessageComponent {
    skill_block: ParsedSkillBlock,
    markdown_theme: MarkdownTheme,
    /// Port of the `ExpandableCustomMessageBox` base class state.
    box_component: Box_,
    expanded: bool,
}

impl SkillInvocationMessageComponent {
    pub fn new(skill_block: ParsedSkillBlock, markdown_theme: MarkdownTheme) -> Self {
        let mut component = Self {
            skill_block,
            markdown_theme,
            box_component: super::expandable_custom_message::custom_message_box(),
            expanded: false,
        };
        component.update_display();
        component
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }
}

impl ExpandableCustomMessageBox for SkillInvocationMessageComponent {
    fn expanded(&self) -> bool {
        self.expanded
    }

    fn set_expanded_flag(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    fn update_display(&mut self) {
        self.box_component.clear();

        if self.expanded {
            self.box_component.add_child(Box::new(Text::new(
                custom_message_label("skill"),
                0,
                0,
                None,
            )));
            let header = format!("**{}**\n\n", self.skill_block.name);
            self.box_component.add_child(Box::new(Markdown::new(
                format!("{header}{}", self.skill_block.content),
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
            let line = format!(
                "{} {} {}",
                custom_message_label("skill"),
                theme().fg("customMessageText", &self.skill_block.name),
                expand_collapse_hint("app.tools.expand", false)
            );
            self.box_component
                .add_child(Box::new(Text::new(line, 0, 0, None)));
        }
    }
}

impl Component for SkillInvocationMessageComponent {
    fn invalidate(&mut self) {
        self.box_component.invalidate();
        ExpandableCustomMessageBox::invalidate(self);
    }

    fn render(&mut self, width: f64) -> Vec<String> {
        self.box_component.render(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::interactive::theme::theme::get_markdown_theme;
    use pi_tui::utils::strip_ansi;

    fn block() -> ParsedSkillBlock {
        ParsedSkillBlock {
            name: "goal".to_string(),
            location: "/skills/goal/SKILL.md".to_string(),
            content: "Body line".to_string(),
            user_message: None,
        }
    }

    #[test]
    fn collapsed_shows_the_skill_name_only() {
        let mut component = SkillInvocationMessageComponent::new(block(), get_markdown_theme());
        let joined: String = component
            .render(60.0)
            .iter()
            .map(|line| strip_ansi(line))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(joined.contains("[skill]"));
        assert!(joined.contains("goal"));
        assert!(!joined.contains("Body line"));
    }

    #[test]
    fn expanded_shows_the_skill_body() {
        let mut component = SkillInvocationMessageComponent::new(block(), get_markdown_theme());
        ExpandableCustomMessageBox::set_expanded(&mut component, true);
        let joined: String = component
            .render(60.0)
            .iter()
            .map(|line| strip_ansi(line))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(joined.contains("Body line"));
    }
}
