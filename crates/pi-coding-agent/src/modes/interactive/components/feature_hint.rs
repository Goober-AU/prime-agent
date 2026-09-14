//! Port of packages/coding-agent/src/modes/interactive/components/feature-hint.ts

use pi_tui::tui::Component;
use pi_tui::utils::{truncate_to_width, visible_width};

use crate::modes::interactive::theme::theme::theme;

const LABEL: &str = "Hint:";
const SHIMMER_RADIUS: i64 = 1;
const SHIMMER_PAUSE_FRAMES: i64 = 14;

/// `FEATURE_HINT_ANIMATION_INTERVAL_MS`
pub const FEATURE_HINT_ANIMATION_INTERVAL_MS: u64 = 160;

/// Port of `renderLabelShimmer`.
fn render_label_shimmer(characters: &[String], frame: i64) -> String {
    if characters.is_empty() {
        return String::new();
    }

    let travel_frames = characters.len() as i64 + SHIMMER_RADIUS * 2;
    let phase = frame % (travel_frames + SHIMMER_PAUSE_FRAMES);
    let center = phase - SHIMMER_RADIUS;
    characters
        .iter()
        .enumerate()
        .map(|(index, character)| {
            // `Math.abs(index - center) <= SHIMMER_RADIUS ? "muted" : "thinkingText"`
            let color = if (index as i64 - center).abs() <= SHIMMER_RADIUS {
                "muted"
            } else {
                "thinkingText"
            };
            theme().fg(color, character)
        })
        .collect::<Vec<String>>()
        .join("")
}

/// Port of `FeatureHintComponent`.
pub struct FeatureHintComponent {
    text: String,
    frame: i64,
}

impl FeatureHintComponent {
    pub fn new(text: String) -> Self {
        Self { text, frame: 0 }
    }

    /// Port of `advance`.
    pub fn advance(&mut self) {
        self.frame += 1;
    }

    pub fn frame(&self) -> i64 {
        self.frame
    }
}

impl Component for FeatureHintComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        let width = width.max(0.0).floor() as usize;
        let padding_x = if width > 2 { 1 } else { 0 };
        let available_width = std::cmp::max(1, width - padding_x * 2);
        let display_text = truncate_to_width(
            &format!("{LABEL} {}", self.text),
            available_width as f64,
            "",
            false,
        );
        let characters: Vec<String> = display_text
            .chars()
            .map(|character| character.to_string())
            .collect();
        let label_length = std::cmp::min(LABEL.chars().count(), characters.len());
        let label = render_label_shimmer(&characters[..label_length], self.frame);
        let hint = theme().fg("muted", &characters[label_length..].join(""));
        let line = format!("{}{label}{hint}", " ".repeat(padding_x));

        vec![
            format!(
                "{line}{}",
                " ".repeat(width.saturating_sub(visible_width(&line)))
            ),
            " ".repeat(width),
        ]
    }

    fn invalidate(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
    }

    #[test]
    fn renders_padded_hint_then_blank_line() {
        init();
        let mut component = FeatureHintComponent::new("Use /goal to keep working.".to_string());
        let lines = component.render(40.0);
        assert_eq!(lines.len(), 2);
        assert_eq!(visible_width(&lines[0]), 40);
        assert_eq!(lines[1], " ".repeat(40));
        let plain = strip_ansi(&lines[0]);
        assert!(plain.starts_with(" Hint: Use /goal"));
    }

    #[test]
    fn advance_moves_the_shimmer_phase() {
        init();
        let mut component = FeatureHintComponent::new("x".to_string());
        let first = component.render(20.0)[0].clone();
        component.advance();
        let second = component.render(20.0)[0].clone();
        assert_ne!(first, second);
    }

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
