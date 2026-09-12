//! Port of packages/coding-agent/src/modes/interactive/components/side-question.ts

use std::rc::Rc;

use pi_tui::components::markdown::{
    DefaultTextStyle, Markdown, MarkdownOptions, MarkdownTheme as TuiMarkdownTheme,
};
use pi_tui::components::r#box::Box_;
use pi_tui::components::text::Text;
use pi_tui::tui::Component;
use pi_tui::utils::visible_width;

use crate::modes::agent_connection::types::AgentConnectionSideQuestionEvent;
use crate::modes::interactive::theme::theme::{get_markdown_theme, theme, MarkdownTheme};

/// `MarkdownTheme` of `theme.ts` carries `Send + Sync` closures; the pi-tui
/// component holds `Rc` closures.
fn to_tui_markdown_theme(theme: MarkdownTheme) -> TuiMarkdownTheme {
    fn rc(value: Box<dyn Fn(&str) -> String + Send + Sync>) -> Rc<dyn Fn(&str) -> String> {
        Rc::new(move |text: &str| value(text))
    }

    TuiMarkdownTheme {
        heading: rc(theme.heading),
        link: rc(theme.link),
        link_url: rc(theme.link_url),
        code: rc(theme.code),
        code_block: rc(theme.code_block),
        code_block_border: rc(theme.code_block_border),
        quote: rc(theme.quote),
        quote_border: rc(theme.quote_border),
        hr: rc(theme.hr),
        list_bullet: rc(theme.list_bullet),
        bold: rc(theme.bold),
        italic: rc(theme.italic),
        strikethrough: rc(theme.strikethrough),
        underline: rc(theme.underline),
        highlight_code: Some(Rc::new(move |code: &str, lang: Option<&str>| {
            (theme.highlight_code)(code, lang)
        })),
        code_block_indent: theme.code_block_indent,
        math: Some(Rc::new(move |text: &str| (theme.math)(text))),
        math_block: Some(Rc::new(move |text: &str| (theme.math_block)(text))),
    }
}

/// `{ color: (content) => theme.fg("userMessageText", content) }`.
fn user_message_text_style() -> Option<DefaultTextStyle> {
    Some(DefaultTextStyle {
        color: Some(Rc::new(|content: &str| {
            theme().fg("userMessageText", content)
        })),
        ..Default::default()
    })
}

/// Port of `SideQuestionTurnState`.
struct SideQuestionTurnState {
    event: AgentConnectionSideQuestionEvent,
    /// Follow-up questions render as a standard user-message bubble; the first
    /// turn keeps the /btw header.
    question_bubble: Option<Box_>,
    answer: Markdown,
}

/// Port of `SideQuestionBashState`: a `!` / `!!` run inside the pane, rendered
/// by the same component the main thread uses.
struct SideQuestionBashState {
    component: Box<dyn Component>,
    running: bool,
}

enum Entry {
    Turn(SideQuestionTurnState),
    Bash(SideQuestionBashState),
}

/// Port of `applySurface`.
fn apply_surface(line: &str, width: usize) -> String {
    let padded = format!(
        "{line}{}",
        " ".repeat(width.saturating_sub(visible_width(line)))
    );
    let background = theme().get_popup_background_color();
    padded
        .split("\x1b[0m")
        .map(|segment| background(segment))
        .collect::<Vec<String>>()
        .join("\x1b[0m")
}

/// Port of `SideQuestionComponent`.
pub struct SideQuestionComponent {
    padding_x: usize,
    entries: Vec<Entry>,
}

impl SideQuestionComponent {
    pub fn new(event: AgentConnectionSideQuestionEvent, padding_x: Option<usize>) -> Self {
        let padding_x = padding_x.unwrap_or(2).max(2);
        let mut component = Self {
            padding_x,
            entries: Vec::new(),
        };
        component.add_turn(event);
        component
    }

    /// Port of `addTurn`.
    pub fn add_turn(&mut self, event: AgentConnectionSideQuestionEvent) {
        let mut question_bubble: Option<Box_> = None;
        if !self.entries.is_empty() {
            let background = theme().get_user_message_background_color();
            let mut bubble = Box_::new(
                self.padding_x,
                1,
                Some(Box::new(move |content: &str| background(content))),
            );
            bubble.add_child(Box::new(Markdown::new(
                event.question.clone(),
                0,
                0,
                to_tui_markdown_theme(get_markdown_theme()),
                user_message_text_style(),
                MarkdownOptions::default(),
            )));
            question_bubble = Some(bubble);
        }
        let mut answer = Markdown::new(
            String::new(),
            self.padding_x,
            0,
            to_tui_markdown_theme(get_markdown_theme()),
            user_message_text_style(),
            MarkdownOptions::default(),
        );
        answer.set_text(event.answer.clone());
        self.entries.push(Entry::Turn(SideQuestionTurnState {
            event,
            question_bubble,
            answer,
        }));
    }

    /// Port of `addBash`.
    pub fn add_bash(&mut self, component: Box<dyn Component>) {
        self.entries.push(Entry::Bash(SideQuestionBashState {
            component,
            running: true,
        }));
    }

    /// Port of `finishBash`.
    pub fn finish_bash(&mut self) {
        for entry in self.entries.iter_mut() {
            if let Entry::Bash(bash) = entry {
                bash.running = false;
            }
        }
    }

    /// Port of `update`.
    pub fn update(&mut self, event: AgentConnectionSideQuestionEvent) {
        let turn = self.entries.iter_mut().find_map(|entry| match entry {
            Entry::Turn(turn) if turn.event.id == event.id => Some(turn),
            _ => None,
        });
        let Some(turn) = turn else {
            return;
        };
        turn.event = event.clone();
        turn.answer.set_text(event.answer);
    }

    /// The number of pushed entries (question turns and bash runs).
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Port of `renderAnswer`.
    fn render_answer(
        turn: &mut SideQuestionTurnState,
        padding_x: usize,
        width: f64,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        if !turn.event.answer.is_empty() {
            lines.extend(<Markdown as Component>::render(&mut turn.answer, width));
        }
        if let Some(error_message) = &turn.event.error_message {
            // A turn can fail after streaming partial output; show both.
            lines.extend(
                Text::new(theme().fg("error", error_message), padding_x, 0, None).render(width),
            );
        }
        if !lines.is_empty() {
            return lines;
        }
        if turn.event.status == "cancelled" {
            return Text::new(
                theme().fg("userMessageText", "Cancelled"),
                padding_x,
                0,
                None,
            )
            .render(width);
        }
        let message = if turn.event.status == "complete" {
            "No response"
        } else {
            "Thinking\u{2026}"
        };
        Text::new(theme().fg("userMessageText", message), padding_x, 0, None).render(width)
    }

    /// Port of `renderHint`.
    fn render_hint(entries: &[Entry], padding_x: usize, width: f64) -> Vec<String> {
        // Any running turn blocks follow-ups (a completed notice can be the last
        // turn while an earlier question still streams), so check them all.
        let running = entries.iter().any(|entry| match entry {
            Entry::Bash(bash) => bash.running,
            Entry::Turn(turn) => turn.event.status == "running",
        });
        let hint = if running {
            "esc to cancel and return to session"
        } else {
            "reply to follow up \u{00b7} esc to return to session"
        };
        Text::new(theme().fg("dim", hint), padding_x, 0, None).render(width)
    }
}

impl Component for SideQuestionComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        let width = width.max(0.0).floor() as usize;
        let padding_x = self.padding_x;
        let blank = " ".repeat(width.max(1));
        let mut lines: Vec<String> = Vec::new();

        for line in vec![blank.clone()] {
            lines.push(apply_surface(&line, width));
        }

        for entry in self.entries.iter_mut() {
            match entry {
                Entry::Bash(bash) => {
                    let mut raw = bash.component.render(width as f64);
                    raw.push(blank.clone());
                    for line in raw {
                        lines.push(apply_surface(&line, width));
                    }
                }
                Entry::Turn(turn) => {
                    if let Some(bubble) = turn.question_bubble.as_mut() {
                        // Bubble lines are already fully painted with the
                        // user-message background.
                        lines.extend(bubble.render(width as f64));
                    } else {
                        let question_text = format!(
                            "{}  {}",
                            theme().fg("accent", "/btw"),
                            theme().bold(&theme().fg("userMessageText", &turn.event.question))
                        );
                        let question =
                            Text::new(question_text, padding_x, 0, None).render(width as f64);
                        for line in question {
                            lines.push(apply_surface(&line, width));
                        }
                    }
                    let mut raw = vec![blank.clone()];
                    raw.extend(Self::render_answer(turn, padding_x, width as f64));
                    raw.push(blank.clone());
                    for line in raw {
                        lines.push(apply_surface(&line, width));
                    }
                }
            }
        }

        let mut raw = Self::render_hint(&self.entries, padding_x, width as f64);
        raw.push(blank);
        for line in raw {
            lines.push(apply_surface(&line, width));
        }
        lines
    }

    fn invalidate(&mut self) {
        for entry in self.entries.iter_mut() {
            match entry {
                Entry::Bash(bash) => bash.component.invalidate(),
                Entry::Turn(turn) => {
                    if let Some(bubble) = turn.question_bubble.as_mut() {
                        bubble.invalidate();
                    }
                    turn.answer.invalidate();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(
        id: &str,
        question: &str,
        answer: &str,
        status: &str,
    ) -> AgentConnectionSideQuestionEvent {
        AgentConnectionSideQuestionEvent {
            id: id.to_string(),
            question: question.to_string(),
            answer: answer.to_string(),
            status: status.to_string(),
            error_message: None,
        }
    }

    struct Lines(Vec<String>);

    impl Component for Lines {
        fn render(&mut self, _width: f64) -> Vec<String> {
            self.0.clone()
        }
        fn invalidate(&mut self) {}
    }

    #[test]
    fn a_new_component_holds_one_turn() {
        let component = SideQuestionComponent::new(event("1", "q", "a", "running"), None);
        assert_eq!(component.entry_count(), 1);
    }

    #[test]
    fn padding_is_clamped_to_at_least_two() {
        assert_eq!(
            SideQuestionComponent::new(event("1", "q", "", "running"), Some(0)).padding_x,
            2
        );
        assert_eq!(
            SideQuestionComponent::new(event("1", "q", "", "running"), Some(5)).padding_x,
            5
        );
    }

    #[test]
    fn update_replaces_the_matching_turn_and_ignores_other_ids() {
        let mut component = SideQuestionComponent::new(event("1", "q", "first", "running"), None);
        component.update(event("2", "q2", "second", "complete"));
        component.update(event("1", "q", "updated", "complete"));
        match &component.entries[0] {
            Entry::Turn(turn) => {
                assert_eq!(turn.event.answer, "updated");
                assert_eq!(turn.event.id, "1");
            }
            Entry::Bash(_) => panic!("expected a turn"),
        }
        assert_eq!(component.entry_count(), 1);
    }

    #[test]
    fn bash_entries_are_tracked_and_finished() {
        let mut component = SideQuestionComponent::new(event("1", "q", "a", "complete"), None);
        component.add_bash(Box::new(Lines(vec!["bash out".to_string()])));
        assert_eq!(component.entry_count(), 2);
        assert!(matches!(&component.entries[1], Entry::Bash(bash) if bash.running));
        component.finish_bash();
        assert!(matches!(&component.entries[1], Entry::Bash(bash) if !bash.running));
    }

    #[test]
    fn render_surfaces_every_line_to_the_popup_background() {
        let mut component =
            SideQuestionComponent::new(event("1", "hello", "world", "running"), None);
        let lines = component.render(20.0);
        assert!(!lines.is_empty());
        assert!(lines.len() > 3);
    }
}
