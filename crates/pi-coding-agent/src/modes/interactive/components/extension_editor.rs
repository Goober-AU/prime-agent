//! Port of packages/coding-agent/src/modes/interactive/components/extension-editor.ts
//!
//! `spawnSync(editor, [...args, tmpFile], { stdio: "inherit", shell })` runs the
//! external editor; the port uses `std::process::Command` with inherited
//! stdio, which is the same synchronous contract.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use pi_tui::components::editor::{Editor, EditorOptions, EditorTheme as TuiEditorTheme};
use pi_tui::components::select_list::SelectListTheme as TuiSelectListTheme;
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Focusable as _;
use pi_tui::tui::{Component, Container, Focusable, TuiStopOptions, TUI};

use super::super::theme::theme::{get_editor_theme, theme};
use super::dynamic_border::{ColorFn, DynamicBorder};
use super::keybinding_hints::{key_hint, KeyTextOptions};

/// `theme.ts`'s `EditorTheme` carries `Box` closures with `Send + Sync`; the
/// `pi-tui` editor takes `Rc` closures without those bounds. Private port of the
/// conversion the other selectors apply to `SelectListTheme`.
fn to_tui_editor_theme(source: super::super::theme::theme::EditorTheme) -> TuiEditorTheme {
    TuiEditorTheme {
        border_color: Rc::new(move |text: &str| (source.border_color)(text)),
        background_color: source.background_color.map(|color| {
            let color: Rc<dyn Fn(&str) -> String> = Rc::new(move |text: &str| (color)(text));
            color
        }),
        autocomplete_background_color: Some({
            let color: Rc<dyn Fn(&str) -> String> =
                Rc::new(move |text: &str| (source.autocomplete_background_color)(text));
            color
        }),
        select_list: TuiSelectListTheme {
            selected_prefix: source.select_list.selected_prefix,
            selected_text: source.select_list.selected_text,
            description: source.select_list.description,
            argument_hint: Some(source.select_list.argument_hint),
            source_tag: Some(source.select_list.source_tag),
            scroll_info: source.select_list.scroll_info,
            no_match: source.select_list.no_match,
        },
        command_color: Some({
            let color: Rc<dyn Fn(&str) -> String> =
                Rc::new(move |text: &str| (source.command_color)(text));
            color
        }),
    }
}

/// Port of `KeybindingsManager` from `core/keybindings.ts` (another slice).
/// Only `matches` is observable here.
pub trait KeybindingsManager {
    fn matches(&self, data: &str, keybinding: &str) -> bool;
}

/// Port of the app keybinding manager wrapper used by interactive mode.
#[derive(Clone, Default)]
pub struct AppKeybindingsManager;

impl KeybindingsManager for AppKeybindingsManager {
    fn matches(&self, data: &str, keybinding: &str) -> bool {
        get_keybindings().matches(data, keybinding)
    }
}

/// Port of `ExtensionEditorComponent extends Container implements Focusable`.
pub struct ExtensionEditorComponent {
    container: Container,
    editor: Rc<RefCell<Editor>>,
    on_submit_callback: Box<dyn FnMut(String)>,
    on_cancel_callback: Box<dyn FnMut()>,
    tui: Rc<RefCell<TUI>>,
    keybindings: Arc<dyn KeybindingsManager + Send + Sync>,
    focused: bool,
    /// `this.editor.onSubmit = (text) => this.onSubmitCallback(text)`.
    /// The pi-tui editor owns the callback, so the submitted text is routed
    /// through this slot and delivered from `handle_input`.
    submitted: Rc<RefCell<Option<String>>>,
}

impl ExtensionEditorComponent {
    pub fn new(
        tui: Rc<RefCell<TUI>>,
        keybindings: Arc<dyn KeybindingsManager + Send + Sync>,
        title: &str,
        prefill: Option<&str>,
        on_submit: Box<dyn FnMut(String)>,
        on_cancel: Box<dyn FnMut()>,
        options: EditorOptions,
    ) -> Self {
        let mut container = Container::new();

        container.add_child(Rc::new(RefCell::new(DynamicBorder::new(border_color()))));
        container.add_child(Rc::new(RefCell::new(Spacer::new(1))));

        container.add_child(Rc::new(RefCell::new(Text::new(
            theme().fg("accent", title),
            1,
            0,
            None,
        ))));
        container.add_child(Rc::new(RefCell::new(Spacer::new(1))));

        let mut editor = Editor::new(Rc::clone(&tui), to_tui_editor_theme(get_editor_theme()), options);
        if let Some(prefill) = prefill {
            editor.set_text(prefill);
        }
        let submitted: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        {
            let submitted = Rc::clone(&submitted);
            editor.on_submit = Some(Box::new(move |text: &str| {
                *submitted.borrow_mut() = Some(text.to_string());
            }));
        }
        let editor = Rc::new(RefCell::new(editor));
        container.add_child(Rc::clone(&editor) as Rc<RefCell<dyn Component>>);

        container.add_child(Rc::new(RefCell::new(Spacer::new(1))));

        let has_external_editor = process_env_visual_editor().is_some();
        let hint = format!(
            "{}{}{}{}",
            key_hint("tui.select.confirm", "submit", &KeyTextOptions::default()),
            format!(
                "  {}",
                key_hint("tui.input.newLine", "newline", &KeyTextOptions::default())
            ),
            format!(
                "  {}",
                key_hint("tui.select.cancel", "cancel", &KeyTextOptions::default())
            ),
            if has_external_editor {
                format!(
                    "  {}",
                    key_hint(
                        "app.editor.external",
                        "external editor",
                        &KeyTextOptions::default()
                    )
                )
            } else {
                String::new()
            }
        );
        container.add_child(Rc::new(RefCell::new(Text::new(hint, 1, 0, None))));

        container.add_child(Rc::new(RefCell::new(Spacer::new(1))));

        container.add_child(Rc::new(RefCell::new(DynamicBorder::new(border_color()))));

        Self {
            container,
            editor,
            on_submit_callback: on_submit,
            on_cancel_callback: on_cancel,
            tui,
            keybindings,
            focused: false,
            submitted,
        }
    }

    /// Port of `handleInput(keyData)`.
    pub fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.cancel") {
            (self.on_cancel_callback)();
            return;
        }

        if self.keybindings.matches(key_data, "app.editor.external") {
            self.open_external_editor();
            return;
        }

        Component::handle_input(&mut *self.editor.borrow_mut(), key_data);
        let submitted = self.submitted.borrow_mut().take();
        if let Some(text) = submitted {
            (self.on_submit_callback)(text);
        }
    }

    /// Port of `openExternalEditor()`.
    pub fn open_external_editor(&mut self) {
        let editor_cmd = match process_env_visual_editor() {
            Some(command) => command,
            None => return,
        };

        let current_text = self.editor.borrow().get_text();
        let tmp_file =
            std::env::temp_dir().join(format!("pi-extension-editor-{}.md", now_millis()));

        let write_result = std::fs::write(&tmp_file, &current_text);
        if write_result.is_err() {
            return;
        }
        self.tui.borrow_mut().stop(TuiStopOptions::default());

        let parts: Vec<String> = editor_cmd.split(' ').map(|part| part.to_string()).collect();
        let editor = parts.first().cloned().unwrap_or_default();
        let mut command = std::process::Command::new(&editor);
        for arg in parts.iter().skip(1) {
            command.arg(arg);
        }
        command.arg(&tmp_file);
        let result = command.status();

        if let Ok(status) = result {
            if status.success() {
                if let Ok(content) = std::fs::read_to_string(&tmp_file) {
                    let new_content = strip_trailing_newline(&content);
                    self.editor.borrow_mut().set_text(&new_content);
                }
            }
        }

        // `finally`: always clean up, restart the TUI and force a full re-render.
        let _ = std::fs::remove_file(&tmp_file);
        self.tui.borrow_mut().start();
        self.tui.borrow_mut().request_render_forced();
    }
}

/// `new DynamicBorder()`'s default color: `theme.fg("border", str)`.
fn border_color() -> ColorFn {
    Box::new(|text: &str| theme().fg("border", text))
}

pub(in crate::modes::interactive) fn strip_trailing_newline(content: &str) -> String {
    content
        .strip_suffix('\n')
        .map(|text| text.to_string())
        .unwrap_or_else(|| content.to_string())
}

/// `process.env.VISUAL || process.env.EDITOR`.
pub(in crate::modes::interactive) fn process_env_visual_editor() -> Option<String> {
    match std::env::var("VISUAL") {
        Ok(value) if !value.is_empty() => Some(value),
        _ => match std::env::var("EDITOR") {
            Ok(value) if !value.is_empty() => Some(value),
            _ => None,
        },
    }
}

/// `Date.now()`.
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

impl Focusable for ExtensionEditorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.editor.borrow_mut().set_focused(focused);
    }
}

impl Component for ExtensionEditorComponent {
    fn render(&mut self, width: f64) -> Vec<String> {
        Component::render(&mut self.container, width)
    }

    fn handle_input(&mut self, data: &str) {
        ExtensionEditorComponent::handle_input(self, data);
    }

    fn invalidate(&mut self) {
        Component::invalidate(&mut self.container);
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}
