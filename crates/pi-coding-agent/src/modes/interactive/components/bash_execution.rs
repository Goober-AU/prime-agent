//! Port of packages/coding-agent/src/modes/interactive/components/bash-execution.ts

use pi_tui::components::loader::{Loader, LoaderIndicatorOptions};
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::{Component, Container, TUI};
use std::cell::RefCell;
use std::rc::Rc;

use crate::core::tools::truncate::{
    truncate_tail, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::{
    expand_collapse_hint, key_text, KeyTextOptions,
};
use crate::modes::interactive::components::visual_truncate::truncate_to_visual_lines;
use crate::modes::interactive::theme::theme::theme;

const PREVIEW_LINES: usize = 20;

/// `"running" | "complete" | "cancelled" | "error"`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashExecutionStatus {
    Running,
    Complete,
    Cancelled,
    Error,
}

/// `{ suppressLeadingSpace?: boolean }`
#[derive(Debug, Clone, Copy, Default)]
pub struct BashExecutionOptions {
    pub suppress_leading_space: bool,
}

/// Port of `BashExecutionComponent`.
pub struct BashExecutionComponent {
    command: String,
    output_lines: Vec<String>,
    status: BashExecutionStatus,
    exit_code: Option<f64>,
    error_message: Option<String>,
    loader: Rc<RefCell<Loader>>,
    truncation_result: Option<TruncationResult>,
    full_output_path: Option<String>,
    expanded: bool,
    content_container: Rc<RefCell<Container>>,
    container: Container,
}

impl BashExecutionComponent {
    /// Port of the `BashExecutionComponent` constructor.
    pub fn new(
        command: &str,
        ui: Rc<RefCell<TUI>>,
        exclude_from_context: bool,
        options: BashExecutionOptions,
    ) -> Self {
        // Use dim border for excluded-from-context commands (!! prefix)
        let color_key: crate::modes::interactive::theme::theme::ThemeColor = if exclude_from_context
        {
            "dim"
        } else {
            "bashMode"
        };
        let border_color: crate::modes::interactive::components::dynamic_border::ColorFn =
            Box::new(move |text: &str| theme().fg(color_key, text));

        let mut container = Container::new();

        // Keep tool activity tight against a preceding agent-message notification.
        if !options.suppress_leading_space {
            container
                .add_child(Rc::new(RefCell::new(Spacer::new(1))) as Rc<RefCell<dyn Component>>);
        }

        container.add_child(
            Rc::new(RefCell::new(DynamicBorder::new(border_color.clone())))
                as Rc<RefCell<dyn Component>>,
        );

        let content_container = Rc::new(RefCell::new(Container::new()));
        container.add_child(Rc::clone(&content_container) as Rc<RefCell<dyn Component>>);

        let header = Text::new(
            theme().fg(color_key, &theme().bold(&format!("$ {command}"))),
            1,
            0,
            None,
        );
        content_container
            .borrow_mut()
            .add_child(Rc::new(RefCell::new(header)) as Rc<RefCell<dyn Component>>);

        let loader = Rc::new(RefCell::new(Loader::new(
            ui,
            Box::new(|spinner: &str| theme().fg("muted", spinner)),
            Box::new(|text: &str| theme().fg("muted", text)),
            format!(
                "Running... ({} to cancel)",
                key_text("tui.select.cancel", &KeyTextOptions::default())
            ),
            None::<LoaderIndicatorOptions>,
        )));
        // The TypeScript starts the loader animation through `Loader.setIndicator`.
        loader.borrow_mut().start();
        content_container
            .borrow_mut()
            .add_child(Rc::clone(&loader) as Rc<RefCell<dyn Component>>);

        container
            .add_child(Rc::new(RefCell::new(DynamicBorder::new(border_color)))
                as Rc<RefCell<dyn Component>>);

        Self {
            command: command.to_string(),
            output_lines: Vec::new(),
            status: BashExecutionStatus::Running,
            exit_code: None,
            error_message: None,
            loader,
            truncation_result: None,
            full_output_path: None,
            expanded: false,
            content_container,
            container,
        }
    }

    /// Set whether the output is expanded (shows full output) or collapsed
    /// (preview only).
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        self.update_display();
    }

    /// Port of `appendOutput`.
    pub fn append_output(&mut self, chunk: &str) {
        // Note: binary data is already sanitized in tui-renderer.ts executeBashCommand
        let clean = strip_ansi(chunk).replace("\r\n", "\n").replace('\r', "\n");

        let new_lines: Vec<String> = clean.split('\n').map(|line| line.to_string()).collect();
        if !self.output_lines.is_empty() && !new_lines.is_empty() {
            let last_index = self.output_lines.len() - 1;
            self.output_lines[last_index].push_str(&new_lines[0]);
            self.output_lines.extend(new_lines[1..].iter().cloned());
        } else {
            self.output_lines.extend(new_lines);
        }

        self.update_display();
    }

    /// Port of `setComplete`.
    pub fn set_complete(
        &mut self,
        exit_code: Option<f64>,
        cancelled: bool,
        truncation_result: Option<TruncationResult>,
        full_output_path: Option<String>,
    ) {
        self.exit_code = exit_code;
        self.status = if cancelled {
            BashExecutionStatus::Cancelled
        } else if exit_code.is_some_and(|exit_code| exit_code != 0.0) {
            // `exitCode !== 0 && exitCode !== undefined && exitCode !== null`
            BashExecutionStatus::Error
        } else {
            BashExecutionStatus::Complete
        };
        self.truncation_result = truncation_result;
        self.full_output_path = full_output_path;

        self.loader.borrow_mut().stop();

        self.update_display();
    }

    /// Mark the execution as failed before producing a result (e.g. spawn failure).
    pub fn set_failed(&mut self, message: &str) {
        self.error_message = Some(message.to_string());
        self.status = BashExecutionStatus::Error;
        self.loader.borrow_mut().stop();
        self.update_display();
    }

    /// Port of `updateDisplay`.
    fn update_display(&mut self) {
        // Apply truncation for LLM context limits (same limits as bash tool)
        let full_output = self.output_lines.join("\n");
        let context_truncation = truncate_tail(
            &full_output,
            TruncationOptions {
                max_lines: Some(DEFAULT_MAX_LINES),
                max_bytes: Some(DEFAULT_MAX_BYTES),
            },
        );

        // Recompute wrapping from the render width so resizes and split panes
        // cannot use stale columns.
        let available_lines: Vec<String> = if context_truncation.content.is_empty() {
            Vec::new()
        } else {
            context_truncation
                .content
                .split('\n')
                .map(|line| line.to_string())
                .collect()
        };

        let preview_start = available_lines.len().saturating_sub(PREVIEW_LINES);
        let preview_logical_lines: Vec<String> = available_lines[preview_start..].to_vec();
        let hidden_line_count = available_lines.len() - preview_logical_lines.len();

        self.content_container.borrow_mut().clear();

        let header = Text::new(
            theme().fg("bashMode", &theme().bold(&format!("$ {}", self.command))),
            1,
            0,
            None,
        );
        self.content_container
            .borrow_mut()
            .add_child(Rc::new(RefCell::new(header)) as Rc<RefCell<dyn Component>>);

        if !available_lines.is_empty() {
            if self.expanded {
                let display_text = available_lines
                    .iter()
                    .map(|line| theme().fg("muted", line))
                    .collect::<Vec<String>>()
                    .join("\n");
                self.content_container
                    .borrow_mut()
                    .add_child(Rc::new(RefCell::new(Text::new(
                        format!("\n{display_text}"),
                        1,
                        0,
                        None,
                    ))) as Rc<RefCell<dyn Component>>);
            } else {
                // Use shared visual truncation utility with width-aware caching
                let styled_output = preview_logical_lines
                    .iter()
                    .map(|line| theme().fg("muted", line))
                    .collect::<Vec<String>>()
                    .join("\n");
                let styled_input = format!("\n{styled_output}");
                self.content_container
                    .borrow_mut()
                    .add_child(Rc::new(RefCell::new(PreviewText::new(styled_input)))
                        as Rc<RefCell<dyn Component>>);
            }
        }

        if self.status == BashExecutionStatus::Running {
            self.content_container
                .borrow_mut()
                .add_child(Rc::clone(&self.loader) as Rc<RefCell<dyn Component>>);
        } else {
            let mut status_parts: Vec<String> = Vec::new();

            if hidden_line_count > 0 {
                if self.expanded {
                    status_parts.push(expand_collapse_hint("app.tools.expand", true));
                } else {
                    status_parts.push(format!(
                        "{} {}",
                        theme().fg("muted", &format!("... {hidden_line_count} more lines")),
                        expand_collapse_hint("app.tools.expand", false)
                    ));
                }
            }

            if self.status == BashExecutionStatus::Cancelled {
                status_parts.push(theme().fg("warning", "(cancelled)"));
            } else if self.status == BashExecutionStatus::Error {
                let text = match &self.error_message {
                    Some(message) => format!("(failed: {message})"),
                    None => format!("(exit {})", js_number_display(self.exit_code)),
                };
                status_parts.push(theme().fg("error", &text));
            }

            // Add truncation warning (context truncation, not preview truncation)
            let was_truncated = self
                .truncation_result
                .as_ref()
                .map(|result| result.truncated)
                .unwrap_or(false)
                || context_truncation.truncated;
            if was_truncated {
                if let Some(full_output_path) = &self.full_output_path {
                    status_parts.push(theme().fg(
                        "warning",
                        &format!("Output truncated. Full output: {full_output_path}"),
                    ));
                }
            }

            if !status_parts.is_empty() {
                self.content_container
                    .borrow_mut()
                    .add_child(Rc::new(RefCell::new(Text::new(
                        format!("\n{}", status_parts.join("\n")),
                        1,
                        0,
                        None,
                    ))) as Rc<RefCell<dyn Component>>);
            }
        }
    }

    /// Get the raw output for creating BashExecutionMessage.
    pub fn get_output(&self) -> String {
        self.output_lines.join("\n")
    }

    /// Get the command that was executed.
    pub fn get_command(&self) -> &str {
        &self.command
    }
}

/// JS template interpolation of an optional exit code (`${this.exitCode}` prints
/// `undefined` for a missing code).
fn js_number_display(value: Option<f64>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(value) => {
            if value.fract() == 0.0 && value.abs() < 1e21 {
                format!("{}", value as i64)
            } else {
                format!("{value}")
            }
        }
    }
}

/// Port of the inline width-aware preview component in `updateDisplay`.
struct PreviewText {
    styled_input: String,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl PreviewText {
    fn new(styled_input: String) -> Self {
        Self {
            styled_input,
            cached_width: None,
            cached_lines: None,
        }
    }
}

impl Component for PreviewText {
    fn render(&mut self, width: f64) -> Vec<String> {
        let width = width.max(0.0).floor() as usize;
        if self.cached_lines.is_none() || self.cached_width != Some(width) {
            let result =
                truncate_to_visual_lines(&self.styled_input, PREVIEW_LINES, width as f64, 1);
            self.cached_lines = Some(result.visual_lines);
            self.cached_width = Some(width);
        }
        self.cached_lines.clone().unwrap_or_default()
    }

    fn invalidate(&mut self) {
        self.cached_width = None;
        self.cached_lines = None;
    }
}

impl Component for BashExecutionComponent {
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

/// Port of `stripAnsi` for the output sanitised by the TUI renderer.
fn strip_ansi(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next == '\u{7}' {
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        result.push(character);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::interactive::theme::theme::init_theme;
    use pi_tui::terminal::ProcessTerminal;
    use pi_tui::tui::TUI;

    fn init() {
        init_theme(Some("prime"), false);
    }

    fn ui() -> Rc<RefCell<TUI>> {
        Rc::new(RefCell::new(TUI::new(
            Box::new(ProcessTerminal::new()),
            None,
        )))
    }

    fn strip(text: &str) -> String {
        strip_ansi(text)
    }

    #[test]
    fn running_component_renders_the_command_and_loader_message() {
        init();
        let mut component =
            BashExecutionComponent::new("echo hi", ui(), false, BashExecutionOptions::default());
        let lines = component.render(40.0);
        let text = lines
            .iter()
            .map(|line| strip(line))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(text.contains("$ echo hi"));
        assert!(text.contains("Running..."));
    }

    #[test]
    fn append_output_merges_partial_lines() {
        init();
        let mut component =
            BashExecutionComponent::new("cmd", ui(), false, BashExecutionOptions::default());
        component.append_output("a");
        component.append_output("b\nc");
        assert_eq!(component.get_output(), "ab\nc");
    }

    #[test]
    fn append_output_normalises_newlines_and_strips_ansi() {
        init();
        let mut component =
            BashExecutionComponent::new("cmd", ui(), false, BashExecutionOptions::default());
        component.append_output("\u{1b}[31mred\u{1b}[0m\r\nnext\rthird");
        assert_eq!(component.get_output(), "red\nnext\nthird");
    }

    #[test]
    fn complete_with_exit_code_zero_is_not_an_error() {
        init();
        let mut component =
            BashExecutionComponent::new("cmd", ui(), false, BashExecutionOptions::default());
        component.set_complete(Some(0.0), false, None, None);
        let text = component
            .render(40.0)
            .iter()
            .map(|line| strip(line))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(!text.contains("(exit"));
    }

    #[test]
    fn nonzero_exit_code_renders_the_error_status() {
        init();
        let mut component =
            BashExecutionComponent::new("cmd", ui(), false, BashExecutionOptions::default());
        component.set_complete(Some(2.0), false, None, None);
        let text = component
            .render(40.0)
            .iter()
            .map(|line| strip(line))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(text.contains("(exit 2)"));
    }

    #[test]
    fn cancelled_execution_renders_the_warning() {
        init();
        let mut component =
            BashExecutionComponent::new("cmd", ui(), false, BashExecutionOptions::default());
        component.set_complete(Some(130.0), true, None, None);
        let text = component
            .render(40.0)
            .iter()
            .map(|line| strip(line))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(text.contains("(cancelled)"));
    }

    #[test]
    fn set_failed_prefers_the_message_over_the_exit_code() {
        init();
        let mut component =
            BashExecutionComponent::new("cmd", ui(), false, BashExecutionOptions::default());
        component.set_failed("spawn ENOENT");
        let text = component
            .render(40.0)
            .iter()
            .map(|line| strip(line))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(text.contains("(failed: spawn ENOENT)"));
    }

    #[test]
    fn hidden_lines_are_counted_and_the_hint_renders() {
        init();
        let mut component =
            BashExecutionComponent::new("cmd", ui(), false, BashExecutionOptions::default());
        let output: String = (0..25).map(|index| format!("line {index}\n")).collect();
        component.append_output(&output);
        component.set_complete(Some(0.0), false, None, None);
        let text = component
            .render(60.0)
            .iter()
            .map(|line| strip(line))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(text.contains("... 6 more lines"));
        assert!(text.contains("to expand"));
    }

    #[test]
    fn truncation_warning_names_the_full_output_path() {
        init();
        let mut component =
            BashExecutionComponent::new("cmd", ui(), false, BashExecutionOptions::default());
        component.append_output("short\n");
        component.set_complete(Some(0.0), false, None, Some("/tmp/full.txt".to_string()));
        // `truncationResult.truncated` is false and the output is short, so no warning.
        let text = component
            .render(60.0)
            .iter()
            .map(|line| strip(line))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(!text.contains("Output truncated"));

        let truncated = crate::core::tools::truncate::truncate_tail(
            &"x\n".repeat(10),
            TruncationOptions {
                max_lines: Some(2),
                max_bytes: None,
            },
        );
        let mut component =
            BashExecutionComponent::new("cmd", ui(), false, BashExecutionOptions::default());
        component.append_output("x\n");
        component.set_complete(
            Some(0.0),
            false,
            Some(truncated),
            Some("/tmp/full.txt".to_string()),
        );
        let text = component
            .render(60.0)
            .iter()
            .map(|line| strip(line))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(text.contains("Output truncated. Full output: /tmp/full.txt"));
    }
}
