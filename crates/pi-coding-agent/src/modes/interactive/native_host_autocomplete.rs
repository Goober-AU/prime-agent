//! Adapter between the shared completion provider and the native terminal editor.

use std::{cell::RefCell, rc::Rc};

use pi_tui::autocomplete::{self, CombinedAutocompleteProvider};
use pi_tui::components::{editor, select_list::SelectItem};

use super::{CustomEditor, InteractiveMode};

struct NativeAutocomplete(CombinedAutocompleteProvider);

// The editor stores UTF-8 byte offsets; the shared provider uses scalar offsets.
fn scalar_column(lines: &[String], line: usize, column: usize) -> usize {
    lines.get(line).map_or(0, |text| {
        text.char_indices()
            .take_while(|(offset, _)| *offset < column)
            .count()
    })
}

impl editor::EditorAutocompleteProvider for NativeAutocomplete {
    fn get_suggestions(
        &mut self,
        lines: &[String],
        line: usize,
        column: usize,
        force: bool,
    ) -> Option<editor::AutocompleteSuggestions> {
        // The shared provider currently performs bounded synchronous filesystem
        // scans; its future does not depend on a Tokio reactor.
        let result = futures::executor::block_on(self.0.get_suggestions(
            lines,
            line,
            scalar_column(lines, line, column),
            &autocomplete::AbortSignal::new(),
            force,
        ))?;
        Some(editor::AutocompleteSuggestions {
            items: result
                .items
                .into_iter()
                .map(|item| SelectItem {
                    value: item.value,
                    label: item.label,
                    description: item.description,
                    argument_hint: item.argument_hint,
                    source_tag: item.source_tag,
                    takes_argument: item.takes_argument,
                })
                .collect(),
            prefix: result.prefix,
            kind: result.kind,
        })
    }

    fn apply_completion(
        &mut self,
        lines: &[String],
        line: usize,
        column: usize,
        item: &SelectItem,
        prefix: &str,
    ) -> editor::ApplyCompletionResult {
        let result = self.0.apply_completion(
            lines,
            line,
            scalar_column(lines, line, column),
            &autocomplete::AutocompleteItem {
                value: item.value.clone(),
                label: item.label.clone(),
                description: item.description.clone(),
                argument_hint: item.argument_hint.clone(),
                source_tag: item.source_tag.clone(),
                takes_argument: item.takes_argument,
            },
            prefix,
        );
        let cursor_col = result.lines.get(result.cursor_line).map_or(0, |text| {
            text.char_indices()
                .nth(result.cursor_col)
                .map_or(text.len(), |(offset, _)| offset)
        });
        editor::ApplyCompletionResult {
            lines: result.lines,
            cursor_line: result.cursor_line,
            cursor_col,
        }
    }

    fn should_trigger_file_completion(&self, lines: &[String], line: usize, column: usize) -> bool {
        self.0
            .should_trigger_file_completion(lines, line, scalar_column(lines, line, column))
    }
}

pub(super) fn configure(editor: &mut CustomEditor, mode: Rc<RefCell<InteractiveMode>>, cwd: &str) {
    let commands = crate::core::slash_commands::builtin_slash_commands()
        .iter()
        .map(|command| {
            let mode = mode.clone();
            let name = command.name.clone();
            let completions = matches!(name.as_str(), "effort" | "heartbeat").then(|| {
                Box::new(move |prefix: &str| {
                    let mode = mode.borrow();
                    let items = if name == "effort" {
                        mode.get_thinking_level_completions(prefix)
                    } else {
                        mode.get_heartbeat_argument_completions(prefix)
                    };
                    items
                        .unwrap_or_default()
                        .into_iter()
                        .map(|item| autocomplete::AutocompleteItem {
                            value: item.value,
                            label: item.label,
                            description: item.description,
                            ..Default::default()
                        })
                        .collect()
                }) as Box<dyn Fn(&str) -> Vec<autocomplete::AutocompleteItem>>
            });
            autocomplete::CommandEntry::Slash(autocomplete::SlashCommand {
                name: command.name.clone(),
                aliases: command.aliases.clone().unwrap_or_default(),
                description: Some(command.description.clone()),
                argument_hint: command.argument_hint.clone(),
                takes_argument: command.takes_argument,
                source_tag: None,
                get_argument_completions: completions,
            })
        })
        .collect();
    let fd = crate::utils::tools_manager::get_tool_path(crate::utils::tools_manager::FD);
    editor
        .editor_mut()
        .set_autocomplete_provider(Rc::new(RefCell::new(NativeAutocomplete(
            CombinedAutocompleteProvider::new(commands, cwd, fd.as_deref()),
        ))));
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_tui::components::editor::EditorAutocompleteProvider;

    #[test]
    fn completions_translate_unicode_cursor_offsets_in_both_directions() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("résumé.txt"), "fixture").unwrap();
        let mut provider = NativeAutocomplete(CombinedAutocompleteProvider::new(
            vec![],
            root.path().to_str().unwrap(),
            None,
        ));
        let lines = vec!["explain é ./rés".to_string()];
        let suggestions = provider
            .get_suggestions(&lines, 0, lines[0].len(), true)
            .unwrap();
        let item = suggestions
            .items
            .iter()
            .find(|item| item.value.contains("résumé"))
            .unwrap();
        let completed =
            provider.apply_completion(&lines, 0, lines[0].len(), item, &suggestions.prefix);
        assert_eq!(completed.lines, ["explain é ./résumé.txt"]);
        assert_eq!(completed.cursor_col, completed.lines[0].len());
    }

    #[test]
    fn native_editor_shows_and_accepts_shared_slash_completions() {
        use pi_tui::tui::Component;
        let ui = Rc::new(RefCell::new(pi_tui::tui::TUI::new(
            Box::new(pi_tui::terminal::ProcessTerminal::new()),
            None,
        )));
        let mut editor = CustomEditor::new(ui, super::super::editor_theme(), Default::default());
        let command = autocomplete::CommandEntry::Slash(autocomplete::SlashCommand {
            name: "model".into(),
            aliases: vec![],
            description: Some("Select model".into()),
            argument_hint: None,
            source_tag: None,
            takes_argument: Some(true),
            get_argument_completions: None,
        });
        editor
            .editor_mut()
            .set_autocomplete_provider(Rc::new(RefCell::new(NativeAutocomplete(
                CombinedAutocompleteProvider::new(vec![command], ".", None),
            ))));
        for key in ["/", "m", "o"] {
            editor.handle_input(key);
        }
        editor.editor_mut().poll_autocomplete();
        assert!(editor.editor().is_showing_autocomplete());
        editor.handle_input("\t");
        assert_eq!(editor.editor().get_text(), "/model ");
    }
}
