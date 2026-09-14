//! Native terminal ownership for the interactive-mode controller.
//!
//! The UI stays on one thread because pi-tui components use Rc/RefCell. Session
//! work runs through AgentConnection on the Tokio runtime; event callbacks only
//! enqueue owned data and never borrow the UI while it is rendering.

use super::*;
use crate::main_entry::InteractiveModeSeamOptions;
use crate::modes::agent_connection::types as wire;
use crate::modes::interactive::components::{
    assistant_message::{AssistantMessageComponent, AssistantMessageComponentOptions},
    thinking_selector::ThinkingSelectorComponent,
    custom_editor::{CustomEditor, CustomEditorOptions},
    extension_editor::{AppKeybindingsManager, ExtensionEditorComponent},
    extension_input::{ExtensionInputComponent, ExtensionInputOptions},
    extension_selector::{ExtensionSelectorComponent, ExtensionSelectorOptions},
    login_dialog::LoginDialogComponent,
    model_selector::{
        ModelItemModel, ModelSelectorComponent, ModelSelectorOptions, ScopedModelItem,
    },
    prime_onboarding_splash::{
        PrimeOnboardingSplashComponent, PrimeOnboardingSplashOptions,
    },
    tool_execution::{ToolExecutionComponent, ToolExecutionOptions, ToolExecutionResult},
    user_message::UserMessageComponent,
};
use crate::modes::interactive::interactive_mode_services as local;
use crate::modes::interactive::prompt_stash_state::{
    PromptStashCapture, PromptStashEditorEffect, PromptStashOutcome, PromptStashSession,
};
use pi_tui::components::text::Text as TuiText;
use pi_tui::tui::{Component as TuiComponent, InputListenerResult, TuiStopOptions, TUI};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub(crate) async fn run_interactive_mode(
    options: InteractiveModeSeamOptions,
) -> Result<(), String> {
    launch(options, false).await.map(|_| ())
}

pub(crate) async fn init_interactive_mode(
    options: InteractiveModeSeamOptions,
) -> Result<(), String> {
    launch(options, true).await.map(|_| ())
}

pub(crate) async fn run_interactive_mode_for_agents(
    options: InteractiveModeSeamOptions,
) -> Result<Option<InteractiveModeRunResult>, String> {
    launch(options, false).await
}

async fn launch(
    options: InteractiveModeSeamOptions,
    benchmark: bool,
) -> Result<Option<InteractiveModeRunResult>, String> {
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || handle.block_on(run_terminal(options, benchmark)))
        .await
        .map_err(|error| format!("Interactive terminal failed: {error}"))?
}

struct SharedComponent<T>(Rc<RefCell<T>>);
impl<T: TuiComponent> TuiComponent for SharedComponent<T> {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }
    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

struct Transcript {
    mode: Rc<RefCell<InteractiveMode>>,
    rows: Vec<Box<dyn TuiComponent>>,
    assistant: Option<Rc<RefCell<AssistantMessageComponent>>>,
    assistants: Vec<Rc<RefCell<AssistantMessageComponent>>>,
    tools: HashMap<String, Rc<RefCell<ToolExecutionComponent>>>,
}
struct ToolRow(Rc<RefCell<ToolExecutionComponent>>);
impl TuiComponent for ToolRow {
    fn render(&mut self, width: f64) -> Vec<String> {
        self.0.borrow_mut().render_lines(width)
    }
    fn invalidate(&mut self) {
        self.0.borrow_mut().content_panel().invalidate();
    }
}
impl Transcript {
    /// Port of `echoLocalCommand` (interactive-mode.ts:6405-6413): the submitted
    /// command is echoed as the user's own message.
    fn echo_local(&mut self, text: &str) {
        let mode = self.mode.borrow();
        self.rows
            .push(Box::new(pi_tui::components::spacer::Spacer::new(1)));
        self.rows.push(Box::new(UserMessageComponent::new(
            text,
            mode.get_markdown_theme_with_settings(),
            &|name| crate::core::slash_commands::is_builtin_slash_command_name(name),
        )));
        drop(mode);
    }

    /// The `chatContainer.addChild(new Spacer(1)); addChild(new Text(info, 1, 0))`
    /// pair every local command panel uses (`/session`
    /// interactive-mode.ts:9499-9501, `/logs` :9531-9533).
    fn panel(&mut self, message: &str) {
        self.rows
            .push(Box::new(pi_tui::components::spacer::Spacer::new(1)));
        self.rows.push(Box::new(TuiText::new(message.into(), 1, 0, None)));
    }

    fn new(mode: Rc<RefCell<InteractiveMode>>) -> Self {
        Self {
            mode,
            rows: Vec::new(),
            assistant: None,
            assistants: Vec::new(),
            tools: HashMap::new(),
        }
    }
    fn replace(&mut self, messages: Vec<AgentMessage>) {
        self.rows.clear();
        self.tools.clear();
        self.assistant = None;
        self.assistants.clear();
        for message in initial_render_messages(messages) {
            self.message(message, false);
        }
    }
    fn message(&mut self, message: AgentMessage, streaming: bool) {
        match message {
            AgentMessage::Message(pi_ai::types::Message::User(user)) => {
                let mode = self.mode.borrow();
                self.rows.push(Box::new(UserMessageComponent::new(
                    &mode.get_user_message_text(&user),
                    mode.get_markdown_theme_with_settings(),
                    &|name| crate::core::slash_commands::is_session_slash_command_name(name),
                )));
            }
            AgentMessage::Message(pi_ai::types::Message::Assistant(message)) => {
                let component = if let Some(component) = &self.assistant {
                    component.clone()
                } else {
                    let mode = self.mode.borrow();
                    let component = Rc::new(RefCell::new(AssistantMessageComponent::new(
                        None,
                        mode.hide_thinking_block,
                        mode.get_markdown_theme_with_settings(),
                        &mode.hidden_thinking_label,
                        AssistantMessageComponentOptions {
                            cwd: Some(mode.get_current_cwd()),
                            expanded: mode.agent_messages_expanded,
                            ..Default::default()
                        },
                    )));
                    self.rows.push(Box::new(SharedComponent(component.clone())));
                    self.assistants.push(component.clone());
                    self.assistant = Some(component.clone());
                    component
                };
                component
                    .borrow_mut()
                    .update_content(message.clone(), streaming);
                for block in &message.content {
                    if let pi_ai::types::ContentBlock::ToolCall(call) = block {
                        self.tool_start(
                            &call.id,
                            &call.name,
                            serde_json::Value::Object(call.arguments.clone()),
                        );
                    }
                }
                if !streaming {
                    self.assistant = None;
                }
            }
            AgentMessage::Message(pi_ai::types::Message::ToolResult(result)) => {
                let value = serde_json::to_value(&result).unwrap_or_default();
                self.tool_result(&result.tool_call_id, &value, result.is_error, false);
            }
            AgentMessage::Custom(message) => {
                let value = serde_json::to_value(message).unwrap_or_default();
                let text = value
                    .get("content")
                    .and_then(|value| value.as_str())
                    .or_else(|| value.get("text").and_then(|value| value.as_str()));
                if let Some(text) = text {
                    self.rows
                        .push(Box::new(TuiText::new(text.into(), 1, 1, None)));
                }
            }
        }
    }
    fn tool_start(&mut self, id: &str, name: &str, args: serde_json::Value) {
        if let Some(component) = self.tools.get(id) {
            component.borrow_mut().update_args(args);
            return;
        }
        let mode = self.mode.borrow();
        let mut component = ToolExecutionComponent::new(
            name,
            id,
            args,
            ToolExecutionOptions::default(),
            None,
            &mode.get_current_cwd(),
        );
        component.mark_execution_started();
        component.set_expanded(mode.tool_output_expanded);
        let component = Rc::new(RefCell::new(component));
        self.rows.push(Box::new(ToolRow(component.clone())));
        self.tools.insert(id.to_string(), component);
    }
    fn tool_result(&mut self, id: &str, result: &serde_json::Value, is_error: bool, partial: bool) {
        let Some(component) = self.tools.get(id) else {
            return;
        };
        let content = result
            .get("content")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .map(
                |block| crate::core::tools::render_utils::RenderContentBlock {
                    r#type: string(block, "type"),
                    text: optional_string(block, "text"),
                    data: optional_string(block, "data"),
                    mime_type: optional_string(block, "mimeType"),
                },
            )
            .collect();
        component.borrow_mut().update_result(
            ToolExecutionResult {
                content,
                is_error,
                details: result.get("details").cloned(),
            },
            partial,
        );
    }
}
impl TuiComponent for Transcript {
    fn render(&mut self, width: f64) -> Vec<String> {
        let mode = self.mode.borrow();
        let mut lines = Vec::new();
        if self.rows.is_empty() {
            let model = mode.get_current_model_id();
            let cwd = mode.get_current_cwd();
            let hint = mode.start_hint.to_string();
            let header = BrandSplashHeader::new(
                mode.version.clone(),
                Box::new(move || model.clone()),
                Box::new(move || cwd.clone()),
                None,
                BrandSplashHeaderOptions {
                    get_start_hint: Some(Box::new(move || hint.clone())),
                    ..Default::default()
                },
            );
            lines.extend(header.render(width, None));
        }
        drop(mode);
        for row in &mut self.rows {
            lines.extend(row.render(width));
        }
        let mode = self.mode.borrow();
        // The `mainViewContainer` child order (interactive-mode.ts:1268-1272), then
        // the prompt-context containers below it (TS:1575-1577).
        for container in mode.get_main_view_containers() {
            lines.extend(local::Component::render(container, width.max(1.0) as usize));
        }
        for container in mode.get_prompt_context_containers() {
            lines.extend(local::Component::render(container, width.max(1.0) as usize));
        }
        if mode.should_show_working_loader() {
            // `createWorkingLoader` mounts the animated pi-tui `Loader`
            // (interactive-mode.ts:3305-3313); the native host renders the same
            // component's frame cycle in place, advanced by the host ticker.
            let frames = pi_tui::components::loader::DEFAULT_FRAMES;
            // The pi-tui `Loader` advances one frame per `DEFAULT_INTERVAL_MS`
            // (crates/pi-tui/src/components/loader.rs:11-12); driving the index from
            // the wall clock reproduces that cadence without a second timer.
            let frame = ((now_ms() / pi_tui::components::loader::DEFAULT_INTERVAL_MS as f64)
                .floor() as i64)
                .rem_euclid(frames.len() as i64) as usize;
            lines.push(format!(
                "{}{}",
                theme().fg("accent", frames[frame]),
                theme().fg("muted", &format!(" {}", mode.get_working_loader_message())),
            ));
        }
        // Existing controller text may be unwrapped; enforce the terminal width.
        lines
            .into_iter()
            .flat_map(|line| pi_tui::utils::wrap_text_with_ansi(&line, width.max(1.0) as usize))
            .collect()
    }
    fn invalidate(&mut self) {
        for row in &mut self.rows {
            row.invalidate();
        }
    }
}

struct Tray(Rc<RefCell<InteractiveMode>>, Rc<RefCell<CustomEditor>>);
impl TuiComponent for Tray {
    fn render(&mut self, width: f64) -> Vec<String> {
        let mode = self.0.borrow();
        let text = mode
            .get_tray_override_label(&self.1.borrow().editor().get_text())
            .or_else(|| mode.get_tray_location_label())
            .unwrap_or_default();
        let mut lines = vec![truncate_to_width(
            &theme().fg("dim", &text),
            width,
            "…",
            false,
        )];
        if let Some(context) = mode.get_tray_context_label() {
            lines.push(truncate_to_width(
                &theme().fg("dim", &context),
                width,
                "…",
                false,
            ));
        }
        lines
    }
    fn invalidate(&mut self) {}
}

struct TerminalGuard(Rc<RefCell<TUI>>);
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Ok(mut ui) = self.0.try_borrow_mut() {
            ui.stop(TuiStopOptions::default());
        }
    }
}

#[derive(Debug)]
enum InputAction {
    Submit(String, bool),
    /// A chosen `/effort` level (`applyThinkingLevel`, interactive-mode.ts:8309-8321).
    ThinkingLevel(pi_agent_core::types::ThinkingLevel),
    /// The `/effort` picker's `onCancel` (interactive-mode.ts:8301-8304).
    DismissSelector,
    Interrupt,
    Escape,
    Exit,
    ToggleTools,
    ToggleThinking,
    ToggleMessages,
    Model,
    AgentsBack,
    Shortcuts,
    Suspend,
    PromptStash,
}
enum HostEvent {
    Connection(wire::AgentConnectionEvent),
    Completed(Result<(), String>),
    Status(String),
    /// A local command's reply that lands in the chat as a message
    /// (`chatContainer.addChild(new Text(info, 1, 0))`, e.g. `/session`
    /// interactive-mode.ts:9499-9501).
    Panel(String),
    /// The submitted command echoed as the user's own message
    /// (`echoLocalCommand`, interactive-mode.ts:6405-6413).
    EchoLocal(String),
    /// `showWarning` (interactive-mode.ts:7678-7682).
    Warning(String),
    /// `/effort` with no argument opens `ThinkingSelectorComponent`
    /// (`showThinkingSelector`, interactive-mode.ts:8285-8307).
    ThinkingLevels {
        current: pi_agent_core::types::ThinkingLevel,
        levels: Vec<pi_agent_core::types::ThinkingLevel>,
    },
    Models(wire::AgentConnectionModelCatalog, Option<String>),
    ModelSelected {
        session_id: String,
        model: wire::AgentConnectionModel,
        result: Result<wire::AgentConnectionState, String>,
    },
    LoginProviders(Vec<pi_tui::components::select_list::SelectItem>),
    BeginLogin(String, bool),
    LoginAuth(String, Option<String>),
    LoginProgress(String),
    LoginPrompt(String, Option<String>, tokio::sync::oneshot::Sender<String>),
    LoginFinished(Result<(), String>),
    /// `requestAgentsView()` - `/resume` without arguments and the
    /// `app.agents.open` handoff (interactive-mode.ts:8736-8738).
    AgentsView,
}

struct ExtensionDialog {
    request: wire::AgentConnectionExtensionUiRequest,
    component: Rc<RefCell<dyn TuiComponent>>,
    overlay: pi_tui::tui::OverlayHandle,
    deadline: Option<Instant>,
}

type ExtensionReply = (String, wire::AgentConnectionExtensionUiResponse);

fn respond_extension(
    connection: &Arc<dyn wire::AgentConnection>,
    send: &mpsc::Sender<HostEvent>,
    reply: ExtensionReply,
) {
    let (connection, send) = (connection.clone(), send.clone());
    tokio::spawn(async move {
        if let Err(error) = connection
            .respond_to_extension_ui_request(&reply.0, reply.1)
            .await
        {
            let _ = send.send(HostEvent::Completed(Err(error)));
        }
    });
}

fn cancel_extension_response(method: &str) -> wire::AgentConnectionExtensionUiResponse {
    if method == "confirm" {
        wire::AgentConnectionExtensionUiResponse::Confirmed { confirmed: false }
    } else {
        wire::AgentConnectionExtensionUiResponse::Cancelled { cancelled: true }
    }
}

fn extension_dialog(
    request: wire::AgentConnectionExtensionUiRequest,
    ui: &Rc<RefCell<TUI>>,
    send: &mpsc::Sender<ExtensionReply>,
) -> Option<ExtensionDialog> {
    let title = optional_string(&request.payload, "title").filter(|title| !title.is_empty())?;
    let selected_id = request.id.clone();
    let selected = send.clone();
    let cancelled_id = request.id.clone();
    let cancelled = send.clone();
    let cancel_response = cancel_extension_response(&request.method);
    let on_cancel = Box::new(move || {
        let _ = cancelled.send((cancelled_id.clone(), cancel_response.clone()));
    });
    let component: Rc<RefCell<dyn TuiComponent>> = match request.method.as_str() {
        "select" | "confirm" => {
            let confirm = request.method == "confirm";
            let (title, choices) = if confirm {
                let message = optional_string(&request.payload, "message")?;
                (
                    format!("{title}\n{message}"),
                    vec!["Yes".into(), "No".into()],
                )
            } else {
                let values = request.payload.get("options")?.as_array()?;
                let choices = values
                    .iter()
                    .map(|value| value.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()?;
                (title, choices)
            };
            Rc::new(RefCell::new(ExtensionSelectorComponent::new(
                &title,
                choices,
                Box::new(move |value| {
                    let response = if confirm {
                        wire::AgentConnectionExtensionUiResponse::Confirmed {
                            confirmed: value == "Yes",
                        }
                    } else {
                        wire::AgentConnectionExtensionUiResponse::Value {
                            value: value.into(),
                        }
                    };
                    let _ = selected.send((selected_id.clone(), response));
                }),
                on_cancel,
                ExtensionSelectorOptions::default(),
            )))
        }
        "input" => Rc::new(RefCell::new(ExtensionInputComponent::new(
            &title,
            optional_string(&request.payload, "placeholder"),
            Box::new(move |value| {
                let _ = selected.send((
                    selected_id.clone(),
                    wire::AgentConnectionExtensionUiResponse::Value {
                        value: value.into(),
                    },
                ));
            }),
            on_cancel,
            ExtensionInputOptions::default(),
        ))),
        "editor" => Rc::new(RefCell::new(ExtensionEditorComponent::new(
            ui.clone(),
            Arc::new(AppKeybindingsManager),
            &title,
            request
                .payload
                .get("prefill")
                .and_then(|value| value.as_str()),
            Box::new(move |value| {
                let _ = selected.send((
                    selected_id.clone(),
                    wire::AgentConnectionExtensionUiResponse::Value { value },
                ));
            }),
            on_cancel,
            Default::default(),
        ))),
        _ => return None,
    };
    let overlay = ui
        .borrow_mut()
        .show_overlay(component.clone(), Default::default());
    let deadline = number(&request.payload, "timeout")
        .filter(|timeout| timeout.is_finite() && *timeout > 0.0)
        .and_then(|timeout| Duration::try_from_secs_f64(timeout / 1000.0).ok())
        .and_then(|duration| Instant::now().checked_add(duration));
    Some(ExtensionDialog {
        request,
        component,
        overlay,
        deadline,
    })
}

/// The stash session for this host. `open` resolves the same store state for the
/// session id, so a stash survives the agents-view handoff and a reopen.
fn stash_session(mode: &InteractiveMode, session_id: &str) -> PromptStashSession {
    let store = mode
        .options
        .prompt_stash_store
        .clone()
        .unwrap_or_else(crate::modes::interactive::prompt_stash_state::shared_prompt_stash_store);
    PromptStashSession::open(store, session_id)
}

/// Port of `snapshotPromptStash` (interactive-mode.ts:4354-4356) over the host's editor.
fn snapshot_editor_prompt_stash(
    mode: &InteractiveMode,
    editor: &CustomEditor,
) -> PromptStashCapture {
    let text = editor.editor().get_text();
    let images = crate::modes::interactive::prompt_stash_state::stash_images(&mode.pasted_images, &text);
    // `editor.getPasteSnapshot?.()` (interactive-mode.ts:4344): the pi-tui `Editor`
    // implements it, so the snapshot is always present on this path.
    let paste_snapshot = editor.editor().get_paste_snapshot();
    PromptStashCapture {
        expanded_text: editor.editor().get_expanded_text(),
        text,
        paste_snapshot: Some(paste_snapshot),
        images,
    }
}

/// The app actions the host editor dispatches, in `editor.onAction` order
/// (`interactive-mode.ts:4290-4320`). `app.prompt.stash` is the Ctrl+S stash.
fn bind_editor_actions(
    editor: &Rc<RefCell<CustomEditor>>,
    actions: &Rc<RefCell<Vec<InputAction>>>,
) {
    for (binding, make) in [
        (
            "app.clear",
            (|| InputAction::Interrupt) as fn() -> InputAction,
        ),
        ("app.tools.expand", || InputAction::ToggleTools),
        ("app.thinking.toggle", || InputAction::ToggleThinking),
        ("app.messages.expand", || InputAction::ToggleMessages),
        ("app.model.select", || InputAction::Model),
        ("app.shortcuts", || InputAction::Shortcuts),
        ("app.suspend", || InputAction::Suspend),
        ("app.prompt.stash", || InputAction::PromptStash),
    ] {
        let actions = actions.clone();
        editor
            .borrow_mut()
            .on_action(binding, Box::new(move || actions.borrow_mut().push(make())));
    }
}

/// Port of `handlePromptStash` (interactive-mode.ts:4379-4394).
///
/// The host editor is a `CustomEditor` over the pi-tui `Editor`, which always
/// offers `restorePasteSnapshot` (`editor-component.ts:52`), so a paste snapshot
/// can always be restored on this path.
fn handle_prompt_stash_action(
    mode: &Rc<RefCell<InteractiveMode>>,
    editor: &Rc<RefCell<CustomEditor>>,
    session_id: &str,
) {
    let session = stash_session(&mode.borrow(), session_id);
    let capture = snapshot_editor_prompt_stash(&mode.borrow(), &editor.borrow());
    let outcome = session.handle_prompt_stash(&capture, true);
    apply_prompt_stash_outcome(mode, editor, &outcome);
}

/// Drops the previous status anchor so the next notice starts a fresh block.
///
/// `restorePromptStashOnOpen` clears `lastStatusText`/`lastStatusSpacer` before
/// restoring (interactive-mode.ts:4360-4364) because `showStatus` replaces the
/// anchored line, and the restore notice must not overwrite a notice that
/// `init()` just posted. `native_host` is a child module of `interactive_mode`,
/// so the anchor fields are reachable without widening their visibility.
fn reset_status_anchor(mode: &mut InteractiveMode) {
    mode.last_status_spacer_index = None;
    mode.last_status_text_index = None;
}

/// Port of `stashDraftForAgentsView` (interactive-mode.ts:4367-4377).
fn stash_editor_draft_for_agents_view(
    mode: &Rc<RefCell<InteractiveMode>>,
    editor: &Rc<RefCell<CustomEditor>>,
    session_id: &str,
) {
    let session = stash_session(&mode.borrow(), session_id);
    let capture = snapshot_editor_prompt_stash(&mode.borrow(), &editor.borrow());
    session.stash_draft_for_agents_view(&capture);
}

/// Applies a stash outcome to the host editor and posts its notice.
fn apply_prompt_stash_outcome(
    mode: &Rc<RefCell<InteractiveMode>>,
    editor: &Rc<RefCell<CustomEditor>>,
    outcome: &PromptStashOutcome,
) {
    match &outcome.editor {
        PromptStashEditorEffect::None => {}
        PromptStashEditorEffect::Clear => editor.borrow_mut().editor_mut().set_text(""),
        PromptStashEditorEffect::SetText {
            text,
            paste_snapshot,
        } => {
            let mut editor = editor.borrow_mut();
            editor.editor_mut().set_text(text);
            if let Some(snapshot) = paste_snapshot {
                editor.editor_mut().restore_paste_snapshot(snapshot.clone());
            }
        }
    }
    if let Some(status) = outcome.status {
        mode.borrow_mut().show_status(status, "dim");
    }
}

fn editor_theme() -> pi_tui::components::editor::EditorTheme {
    let source = crate::modes::interactive::theme::theme::get_editor_theme();
    pi_tui::components::editor::EditorTheme {
        border_color: Rc::new(move |text| (source.border_color)(text)),
        background_color: source
            .background_color
            .map(|color| Rc::new(move |text: &str| color(text)) as Rc<dyn Fn(&str) -> String>),
        autocomplete_background_color: Some(Rc::new(move |text| {
            (source.autocomplete_background_color)(text)
        })),
        command_color: Some(Rc::new(move |text| (source.command_color)(text))),
        select_list: select_theme(),
    }
}
fn select_theme() -> pi_tui::components::select_list::SelectListTheme {
    pi_tui::components::select_list::SelectListTheme {
        selected_prefix: Box::new(|s| theme().fg("accent", s)),
        selected_text: Box::new(|s| theme().fg("accent", s)),
        description: Box::new(|s| theme().fg("muted", s)),
        argument_hint: None,
        source_tag: None,
        scroll_info: Box::new(|s| theme().fg("dim", s)),
        no_match: Box::new(|s| theme().fg("muted", s)),
    }
}

async fn run_terminal(
    options: InteractiveModeSeamOptions,
    benchmark: bool,
) -> Result<Option<InteractiveModeRunResult>, String> {
    let connection: Arc<dyn wire::AgentConnection> = match options.connection.clone() {
        Some(connection) => connection,
        None => {
            let runtime = options
                .runtime
                .clone()
                .ok_or("Interactive mode requires a session runtime or connection")?;
            Arc::new(crate::modes::agent_connection::in_process_agent_connection::InProcessAgentConnection::new(
                Arc::new(crate::core::agent_session_runtime::InProcessRuntimeHostAdapter::new(runtime)),
            ))
        }
    };
    let snapshot = connection.get_initial_snapshot().await?;
    let mut current_session_id = snapshot.state.session_id.clone();
    let services = if let Some(runtime) = &options.runtime {
        local::create_interactive_mode_ui_services(&runtime.session())
    } else {
        let cwd = snapshot.state.cwd.clone();
        let name = snapshot.state.session_name.clone();
        local::InteractiveModeUiServices {
            settings_manager: Arc::new(Mutex::new(local::SettingsManager::create(&cwd, None))),
            model_registry: Arc::new(Mutex::new(local::ModelRegistry::in_memory())),
            get_initial_cwd: Box::new(move || cwd.clone()),
            get_initial_session_name: Box::new(move || name.clone()),
            get_themes: Box::new(Vec::new),
            refresh_mcp_providers: None,
        }
    };
    let selected_theme = services
        .settings_manager
        .lock()
        .map_err(|e| e.to_string())?
        .get_theme();
    crate::modes::interactive::theme::theme::init_theme(selected_theme.as_deref(), false);
    crate::core::keybindings::KeybindingsManager::create(None).install();
    let initial_message = options.initial_message.clone();
    let initial_images = options.initial_images.clone();
    let initial_messages = options.initial_messages.clone();
    let mut controller = InteractiveMode::new(InteractiveModeOptions {
        migrated_providers: Some(options.migrated_providers),
        model_fallback_message: options.model_fallback_message.clone(),
        startup_notice: None,
        initial_message: None,
        initial_images: None,
        initial_messages: None,
        initial_prompts: None,
        verbose: options.verbose,
        agent_connection: Arc::new(connection.clone()),
        daemon_socket_path: options.daemon_socket_path,
        local_session_host: None,
        bind_local_session_extensions: false,
        ui_services: Some(services),
        on_shutdown: None,
        return_to_agents_view: options.return_to_agents_view,
        force_fullscreen: false,
        agents_view_owns_startup_notices: false,
        session_depth: options.session_depth,
        session_has_children: options.session_has_children,
        // A real store, so Ctrl+S and the agents-view handoff keep a draft.
        // TypeScript creates one `ClientPromptStashStore` per process and shares it
        // across chat views (main.ts:1448, 1544).
        prompt_stash_store: Some(crate::modes::interactive::prompt_stash_state::shared_prompt_stash_store()),
        prompt_stash_session_id: Some(snapshot.state.session_id.clone()),
    })?;
    controller.apply_connection_state_snapshot(project_state(snapshot.state));
    controller.init().await?;
    if controller.get_current_model().is_none() {
        controller.show_status("Welcome to Optimus. Connect a provider with /login, then choose a model with /model. Type /help for commands.", "accent");
    }
    if let Some(warning) = options.model_fallback_message {
        controller.show_warning(&warning);
    }
    let mode = Rc::new(RefCell::new(controller));
    let transcript = Rc::new(RefCell::new(Transcript::new(mode.clone())));
    transcript.borrow_mut().replace(snapshot.messages);
    if let Some(message) = snapshot.streaming_message {
        transcript.borrow_mut().message(message, true);
    }
    let ui = Rc::new(RefCell::new(TUI::new(
        Box::new(pi_tui::terminal::ProcessTerminal::new()),
        None,
    )));
    let editor = Rc::new(RefCell::new(CustomEditor::new(
        ui.clone(),
        editor_theme(),
        CustomEditorOptions {
            placeholder: Some(mode.borrow().start_hint.into()),
            ..Default::default()
        },
    )));
    let actions = Rc::new(RefCell::new(Vec::<InputAction>::new()));
    {
        let actions = actions.clone();
        editor.borrow_mut().editor_mut().on_submit = Some(Box::new(move |text| {
            actions
                .borrow_mut()
                .push(InputAction::Submit(text.to_string(), false))
        }));
    }
    bind_editor_actions(&editor, &actions);
    {
        let actions = actions.clone();
        editor.borrow_mut().on_escape = Some(Box::new(move || {
            actions.borrow_mut().push(InputAction::Escape)
        }));
    }
    {
        let actions = actions.clone();
        editor.borrow_mut().on_ctrl_d = Some(Box::new(move || {
            actions.borrow_mut().push(InputAction::Exit)
        }));
    }
    if mode.borrow().options.return_to_agents_view {
        let actions = actions.clone();
        editor.borrow_mut().on_agents_back = Some(Box::new(move || {
            actions.borrow_mut().push(InputAction::AgentsBack);
            true
        }));
    }
    // `run()` restores a `restoreOnOpen` stash once init is done
    // (interactive-mode.ts:1628-1630, :4358-4365). The restore notice must land in a
    // fresh status block, so the previous status anchor is dropped first.
    {
        let session = stash_session(&mode.borrow(), &current_session_id);
        if session.restore_on_open_pending() {
            reset_status_anchor(&mut mode.borrow_mut());
            let editor_text = editor.borrow().editor().get_text();
            if let Some(outcome) =
                session.restore_prompt_stash_if_editor_empty(None, &editor_text, true)
            {
                apply_prompt_stash_outcome(&mode, &editor, &outcome);
            }
        }
    }
    let input = Rc::new(RefCell::new(Vec::<String>::new()));
    {
        let input = input.clone();
        let fullscreen = mode.borrow().fullscreen_enabled;
        // Dispatch component input after releasing the TUI borrow: Editor owns
        // the same TUI handle and requests rendering from its input handlers.
        ui.borrow_mut().add_input_listener(Box::new(move |data| {
            // Keep terminal replies and mouse scrolling in TUI's own handlers.
            if (data.starts_with("\x1b[6;") && data.ends_with('t'))
                || (fullscreen && pi_tui::mouse::is_mouse_sequence(data))
            {
                return InputListenerResult::default();
            }
            if pi_tui::keys::is_key_release(data) || pi_tui::mouse::is_mouse_sequence(data) {
                return InputListenerResult {
                    consume: true,
                    data: None,
                };
            }
            input.borrow_mut().push(data.to_string());
            InputListenerResult {
                consume: true,
                data: None,
            }
        }));
    }
    ui.borrow_mut().add_child(transcript.clone());
    ui.borrow_mut().add_child(editor.clone());
    ui.borrow_mut()
        .add_child(Rc::new(RefCell::new(Tray(mode.clone(), editor.clone()))));
    ui.borrow_mut().set_focus(Some(editor.clone()));
    ui.borrow_mut().start();
    let guard = TerminalGuard(ui.clone());
    if mode.borrow().fullscreen_enabled {
        let dock = Rc::new(RefCell::new(pi_tui::tui::Container::new()));
        dock.borrow_mut().add_child(editor.clone());
        dock.borrow_mut()
            .add_child(Rc::new(RefCell::new(Tray(mode.clone(), editor.clone()))));
        ui.borrow_mut()
            .enter_fullscreen(pi_tui::tui::FullscreenOptions {
                scroll: vec![transcript.clone()],
                dock,
                mouse: true,
                viewport_controls: true,
            });
    }
    ui.borrow_mut().run_pending_render(now_ms());
    if benchmark {
        mode.borrow_mut().shutdown().await;
        drop(guard);
        connection.dispose().await?;
        return Ok(None);
    }
    let (send, receive) = mpsc::channel();
    let event_send = send.clone();
    let unsubscribe = connection.subscribe(Arc::new(move |event| {
        let _ = event_send.send(HostEvent::Connection(event));
        Box::pin(async {})
    }));
    if let Some(message) = initial_message {
        submit(&connection, &send, message, false, initial_images);
    }
    for message in initial_messages {
        submit(&connection, &send, message, true, None);
    }
    let mut last_tick = Instant::now();
    let mut last_loader_tick = Instant::now();
    let mut selector: Option<Rc<RefCell<pi_tui::components::select_list::SelectList>>> = None;
    let mut overlay: Option<pi_tui::tui::OverlayHandle> = None;
    let (selection_send, selection_receive) = mpsc::channel::<Option<String>>();
    let mut models = Vec::<wire::AgentConnectionModel>::new();
    let mut configured_providers = std::collections::HashSet::<String>::new();
    let mut model_selector: Option<Rc<RefCell<ModelSelectorComponent>>> = None;
    let mut thinking_selector: Option<Rc<RefCell<ThinkingSelectorComponent>>> = None;
    let model_rows = Rc::new(Cell::new(ui.borrow().terminal_rows() as f64));
    let mut pending_login_model: Option<String> = None;
    let mut login_dialog: Option<Rc<RefCell<LoginDialogComponent>>> = None;
    let mut login_cancel: Option<tokio_util::sync::CancellationToken> = None;
    let mut extension: Option<ExtensionDialog> = None;
    let mut extension_queue =
        std::collections::VecDeque::<wire::AgentConnectionExtensionUiRequest>::new();
    let (extension_send, extension_receive) = mpsc::channel::<ExtensionReply>();
    // `await this.runStartupOnboarding()` (interactive-mode.ts:1803): persists
    // `onboardingShown` before the flow opens and shows the splash overlay for
    // first-run users. `PrimeOnboardingSplashComponent` swallows input while a
    // progress message is active (components/prime_onboarding_splash.rs:617-620).
    let mut onboarding_splash: Option<Rc<RefCell<PrimeOnboardingSplashComponent>>> = None;
    let mut onboarding_overlay: Option<pi_tui::tui::OverlayHandle> = None;
    let mut onboarding_settled: Option<Rc<Cell<i8>>> = None;
    if mode.borrow_mut().run_startup_onboarding() {
        let prime_cli_splash = mode.borrow().onboarding_uses_prime_cli_splash();
        let splash_rows = model_rows.clone();
        let splash_ui = ui.clone();
        // 0 = still open, 1 = accepted, -1 = cancelled
        // (`showOnboardingSplash`'s `settle`/`dismiss`, TS:8805-8832).
        let settled = Rc::new(Cell::new(0i8));
        let accepted_flag = settled.clone();
        let cancelled_flag = settled.clone();
        let component = Rc::new(RefCell::new(PrimeOnboardingSplashComponent::new(
            Box::new(move || accepted_flag.set(1)),
            Box::new(move || cancelled_flag.set(-1)),
            PrimeOnboardingSplashOptions {
                get_rows: Some(Box::new(move || splash_rows.get())),
                request_render: Some(Box::new(move || splash_ui.borrow_mut().request_render())),
                continue_action_label: prime_cli_splash.then(|| "choose a model".to_string()),
                ..Default::default()
            },
        )));
        let handle = ui.borrow_mut().show_overlay(
            component.clone(),
            pi_tui::tui::OverlayOptions {
                // `showOverlay(selector, { width: "100%", maxHeight: "100%",
                // row: 0, col: 0 })` (interactive-mode.ts:8839-8844).
                width: Some(pi_tui::tui::SizeValue::Percent("100%".into())),
                max_height: Some(pi_tui::tui::SizeValue::Percent("100%".into())),
                row: Some(pi_tui::tui::SizeValue::Number(0.0)),
                col: Some(pi_tui::tui::SizeValue::Number(0.0)),
                ..Default::default()
            },
        );
        onboarding_splash = Some(component);
        onboarding_overlay = Some(handle);
        onboarding_settled = Some(settled);
    }
    let mut exit_error = None;
    loop {
        match ui.borrow_mut().terminal.poll_input() {
            Ok(true) => {}
            Ok(false) => break,
            Err(error) => {
                exit_error = Some(error.to_string());
                break;
            }
        }
        ui.borrow_mut().drain_input();
        for data in std::mem::take(&mut *input.borrow_mut()) {
            if let Some(splash) = &onboarding_splash {
                splash.borrow_mut().handle_input(&data);
            } else if let Some(dialog) = &extension {
                dialog.component.borrow_mut().handle_input(&data);
            } else if let Some(dialog) = &login_dialog {
                dialog.borrow_mut().handle_input(&data);
            } else if let Some(picker) = &model_selector {
                let mut picker = picker.borrow_mut();
                picker.handle_input(&data);
                if picker.cancelled {
                    let _ = selection_send.send(None);
                } else if let Some(model) = picker.selected_model.take() {
                    let _ = selection_send.send(Some(format!("{}/{}", model.provider, model.id)));
                }
            } else if let Some(picker) = &thinking_selector {
                picker.borrow_mut().handle_input(&data);
            } else if let Some(selector) = &selector {
                selector.borrow_mut().handle_input(&data);
            } else if pi_tui::keybindings::get_keybindings().matches(&data, "app.message.followUp")
            {
                actions.borrow_mut().push(InputAction::Submit(
                    editor.borrow().editor().get_expanded_text(),
                    true,
                ));
            } else {
                editor.borrow_mut().handle_input(&data);
            }
            ui.borrow_mut().request_render();
        }
        while let Ok(reply) = extension_receive.try_recv() {
            if extension.as_ref().map(|dialog| dialog.request.id.as_str()) == Some(reply.0.as_str())
            {
                if let Some(dialog) = extension.take() {
                    dialog.overlay.hide();
                }
                respond_extension(&connection, &send, reply);
            }
        }
        while let Ok(selected) = selection_receive.try_recv() {
            if let Some(handle) = overlay.take() {
                handle.hide();
            }
            selector = None;
            model_selector = None;
            thinking_selector = None;
            if let Some(selected) = selected {
                if let Some(target) = selected.strip_prefix("login:") {
                    if let Some((kind, provider)) = target.split_once(':') {
                        let _ = send.send(HostEvent::BeginLogin(provider.into(), kind == "oauth"));
                    }
                    continue;
                }
                if let Some(model) = models
                    .iter()
                    .find(|model| format!("{}/{}", model.provider, model.id) == selected)
                {
                    if !configured_providers.contains(&model.provider) {
                        let oauth =
                            pi_ai::utils::oauth::get_oauth_provider(&model.provider).is_some();
                        let api_key =
                            crate::core::provider_display_names::built_in_provider_display_names()
                                .iter()
                                .any(|(id, _)| *id == model.provider);
                        if oauth || api_key {
                            pending_login_model = Some(selected);
                            let _ = send.send(HostEvent::BeginLogin(model.provider.clone(), oauth));
                        } else {
                            mode.borrow_mut().show_error(&format!(
                                "Authentication for {} must be configured externally.",
                                model.provider
                            ));
                        }
                        continue;
                    }
                    mode.borrow_mut()
                        .show_status(&format!("Switching model: {}", model.id), "dim");
                    let (connection, send, model, session_id) = (
                        connection.clone(),
                        send.clone(),
                        model.clone(),
                        current_session_id.clone(),
                    );
                    tokio::spawn(async move {
                        let result = async {
                            connection.set_model(&model.provider, &model.id).await?;
                            connection.get_state().await
                        }
                        .await;
                        let _ = send.send(HostEvent::ModelSelected {
                            session_id,
                            model,
                            result,
                        });
                    });
                }
            }
        }
        if let (Some(splash), Some(settled)) = (&onboarding_splash, &onboarding_settled) {
            match settled.get() {
                // `showOnboardingModelSelection` / `showConfigurationMenu("models")`
                // (interactive-mode.ts:1904-1921): the splash dismisses and the model
                // menu takes over.
                1 => {
                    splash.borrow_mut().dispose();
                    if let Some(handle) = onboarding_overlay.take() {
                        handle.hide();
                    }
                    onboarding_splash = None;
                    onboarding_settled = None;
                    submit(&connection, &send, "/model".into(), false, None);
                    ui.borrow_mut().request_render();
                }
                -1 => {
                    splash.borrow_mut().dispose();
                    if let Some(handle) = onboarding_overlay.take() {
                        handle.hide();
                    }
                    onboarding_splash = None;
                    onboarding_settled = None;
                    ui.borrow_mut().request_render();
                }
                _ => {}
            }
        }
        for action in std::mem::take(&mut *actions.borrow_mut()) {
            match action {
                InputAction::Submit(text, follow_up) => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    editor.borrow_mut().editor_mut().add_to_history(&text);
                    editor.borrow_mut().editor_mut().set_text("");
                    if matches!(text.trim(), "/quit" | "/exit") {
                        mode.borrow_mut().shutdown_requested = true;
                        continue;
                    }
                    if text.trim() == "/hotkeys" {
                        // `/hotkeys` appends the full reference to the chat, unlike
                        // the ephemeral `?` guide (interactive-mode.ts:4931-4936).
                        mode.borrow_mut().handle_hotkeys_command();
                    } else if text.trim() == "/help" {
                        mode.borrow_mut().show_status("/model  select a model\n/login  provider setup\n/new  new session\n/context  context usage\n/compact [instructions]  compact session\n/refine  refine reusable knowledge\n/goal <objective>  pursue a goal\n!<command>  run shell command\n/quit  exit\nEsc interrupts; Ctrl+C twice exits; Ctrl+D exits an empty prompt; Alt+Enter queues follow-up; Ctrl+O expands tools.", "dim");
                    } else {
                        submit(&connection, &send, text, follow_up, None);
                    }
                }
                InputAction::Interrupt | InputAction::Escape => {
                    let second = matches!(action, InputAction::Interrupt)
                        && mode.borrow().is_ctrl_c_exit_hint_visible();
                    if second {
                        mode.borrow_mut().shutdown_requested = true;
                        continue;
                    }
                    if mode.borrow().has_interruptible_work() {
                        // Read the activity flags here: the mode handle is not `Send`, so it cannot cross
                        // into the spawned task.
                        let (compacting, bash_running, retry_attempt, streaming) = {
                            let mode = mode.borrow();
                            (
                                mode.is_agent_compacting(),
                                mode.is_bash_running(),
                                mode.get_retry_attempt(),
                                mode.is_agent_streaming(),
                            )
                        };
                        let connection = connection.clone();
                        let send = send.clone();
                        tokio::spawn(async move {
                            // Port of `interruptOrClearInput`: abort the owner of each in-flight activity.
                            // The streaming abort PRESERVES the queue - it is held server-side and draining
                            // resumes on the next submit or queued-message edit. `abort_and_clear_queue`
                            // would silently discard follow-ups the user already typed.
                            if retry_attempt > 0.0 {
                                let _ = connection.abort_retry().await;
                            }
                            if compacting {
                                let _ = connection.abort_compaction().await;
                                let _ = connection.abort_branch_summary().await;
                            }
                            if bash_running {
                                let _ = connection.abort_bash().await;
                            }
                            let result = if streaming {
                                connection.abort().await.map(|_| ())
                            } else {
                                Ok(())
                            };
                            let _ = send.send(HostEvent::Completed(result));
                        });
                    } else {
                        editor.borrow_mut().editor_mut().set_text("");
                    }
                    if matches!(action, InputAction::Interrupt) {
                        mode.borrow_mut().show_ctrl_c_exit_hint();
                    }
                }
                InputAction::Exit => {
                    mode.borrow_mut().shutdown_requested = true;
                }
                InputAction::ToggleTools => {
                    mode.borrow_mut().toggle_tool_output_expansion();
                    for tool in transcript.borrow().tools.values() {
                        tool.borrow_mut()
                            .set_expanded(mode.borrow().tool_output_expanded);
                    }
                }
                InputAction::ToggleThinking => {
                    let mut mode = mode.borrow_mut();
                    mode.hide_thinking_block = !mode.hide_thinking_block;
                    for assistant in &transcript.borrow().assistants {
                        assistant
                            .borrow_mut()
                            .set_hide_thinking_block(mode.hide_thinking_block);
                    }
                }
                InputAction::ToggleMessages => {
                    mode.borrow_mut().toggle_agent_message_expansion();
                    for assistant in &transcript.borrow().assistants {
                        assistant
                            .borrow_mut()
                            .set_expanded(mode.borrow().agent_messages_expanded);
                    }
                }
                InputAction::AgentsBack => {
                    // `returnToAgentsView` stashes the live draft first
                    // (interactive-mode.ts:7122) so the handoff does not lose it.
                    stash_editor_draft_for_agents_view(&mode, &editor, &current_session_id);
                    mode.borrow_mut()
                        .return_to_agents_view(InteractiveModeRunResultType::AgentsView);
                }
                // Ctrl+S: `handlePromptStash` (interactive-mode.ts:4379-4394).
                InputAction::PromptStash => {
                    handle_prompt_stash_action(&mode, &editor, &current_session_id)
                }
                InputAction::Model => {
                    submit(&connection, &send, "/model".into(), false, None);
                }
                // `applyThinkingLevel` (interactive-mode.ts:8309-8321): the picker
                // already closed itself, so only the level is applied.
                InputAction::ThinkingLevel(level) => {
                    if let Some(handle) = overlay.take() {
                        handle.hide();
                    }
                    thinking_selector = None;
                    let (connection, send, mode) =
                        (connection.clone(), send.clone(), mode.clone());
                    tokio::spawn(async move {
                        let result = match connection.set_thinking_level(level).await {
                            Ok(()) => {
                                let _ = send.send(HostEvent::Status(format!(
                                    "Thinking level: {}",
                                    level.as_str()
                                )));
                                Ok(())
                            }
                            Err(error) => Err(error),
                        };
                        let _ = send.send(HostEvent::Completed(result));
                    });
                    let _ = &mode;
                }
                InputAction::DismissSelector => {
                    if let Some(handle) = overlay.take() {
                        handle.hide();
                    }
                    thinking_selector = None;
                    selector = None;
                    model_selector = None;
                }
                // `?` opens an ephemeral guide; it never reaches the chat history
                // (interactive-mode.ts:4289, :10197-10204).
                InputAction::Shortcuts => mode.borrow_mut().show_shortcut_guide(),
                InputAction::Suspend => mode.borrow_mut().handle_ctrl_z(),
            }
            ui.borrow_mut().request_render();
        }
        while let Ok(event) = receive.try_recv() {
            match event {
                HostEvent::Connection(wire::AgentConnectionEvent::Closed { error }) => {
                    exit_error = error;
                    mode.borrow_mut().shutdown_requested = true;
                }
                HostEvent::Connection(wire::AgentConnectionEvent::SessionEvent { event }) => {
                    apply_event(&mode, &transcript, event)
                }
                HostEvent::Connection(wire::AgentConnectionEvent::SessionReplaced {
                    state,
                    messages,
                }) => {
                    if let Some(dialog) = extension.take() {
                        dialog.overlay.hide();
                        respond_extension(
                            &connection,
                            &send,
                            (
                                dialog.request.id,
                                wire::AgentConnectionExtensionUiResponse::Cancelled {
                                    cancelled: true,
                                },
                            ),
                        );
                    }
                    for request in extension_queue.drain(..) {
                        respond_extension(
                            &connection,
                            &send,
                            (
                                request.id,
                                wire::AgentConnectionExtensionUiResponse::Cancelled {
                                    cancelled: true,
                                },
                            ),
                        );
                    }
                    current_session_id = state.session_id.clone();
                    mode.borrow_mut()
                        .apply_connection_state_snapshot(project_state(state));
                    transcript.borrow_mut().replace(messages);
                }
                HostEvent::Connection(wire::AgentConnectionEvent::SessionResynced { snapshot }) => {
                    current_session_id = snapshot.state.session_id.clone();
                    mode.borrow_mut()
                        .apply_connection_state_snapshot(project_state(snapshot.state));
                    transcript.borrow_mut().replace(snapshot.messages);
                    if let Some(message) = snapshot.streaming_message {
                        transcript.borrow_mut().message(message, true);
                    }
                }
                HostEvent::Connection(wire::AgentConnectionEvent::ExtensionError {
                    error, ..
                }) => mode.borrow_mut().show_error(&error),
                HostEvent::Connection(wire::AgentConnectionEvent::ExtensionUiRequest {
                    request,
                }) => match request.method.as_str() {
                    "select" | "confirm" | "input" | "editor" => extension_queue.push_back(request),
                    "notify" => {
                        let message = string(&request.payload, "message");
                        match string(&request.payload, "notifyType").as_str() {
                            "error" => mode.borrow_mut().show_error(&message),
                            "warning" => mode.borrow_mut().show_warning(&message),
                            _ => mode.borrow_mut().show_status(&message, "dim"),
                        }
                    }
                    "setEditorText" => {
                        if let Some(text) = optional_string(&request.payload, "text") {
                            editor.borrow_mut().editor_mut().set_text(&text);
                        }
                    }
                    "setTitle" => {
                        if let Some(title) = optional_string(&request.payload, "title") {
                            ui.borrow_mut().terminal.set_title(&title);
                        }
                    }
                    "setWorkingMessage" => {
                        mode.borrow_mut().working_message =
                            optional_string(&request.payload, "message")
                    }
                    "setWorkingVisible" => {
                        if let Some(visible) = request
                            .payload
                            .get("visible")
                            .and_then(|value| value.as_bool())
                        {
                            mode.borrow_mut().set_working_visible(visible);
                        }
                    }
                    "setHiddenThinkingLabel" => {
                        mode.borrow_mut()
                            .set_hidden_thinking_label(optional_string(&request.payload, "label"));
                        for assistant in &transcript.borrow().assistants {
                            assistant
                                .borrow_mut()
                                .set_hidden_thinking_label(&mode.borrow().hidden_thinking_label);
                        }
                    }
                    _ => mode.borrow_mut().show_status(
                        &format!("Unsupported extension UI request: {}", request.method),
                        "dim",
                    ),
                },
                HostEvent::Connection(wire::AgentConnectionEvent::ConnectionStatus {
                    status,
                    error,
                }) => mode
                    .borrow_mut()
                    .show_status(&error.unwrap_or(status), "dim"),
                HostEvent::Connection(wire::AgentConnectionEvent::SessionStatus { recap }) => {
                    mode.borrow_mut().session_recap = recap;
                    mode.borrow_mut().render_recap();
                }
                HostEvent::Connection(_) => {}
                HostEvent::ModelSelected {
                    session_id,
                    model,
                    result,
                } => match result {
                    Ok(state)
                        if current_session_id == session_id && state.session_id == session_id =>
                    {
                        let mut controller = mode.borrow_mut();
                        controller
                            .settings_manager()
                            .lock()
                            .map_err(|error| error.to_string())?
                            .set_default_model_and_provider(&model.provider, &model.id);
                        controller.apply_connection_state_snapshot(project_state(state));
                        controller.show_status(&format!("Model: {}", model.id), "success");
                    }
                    Ok(_) => {}
                    Err(error) => mode.borrow_mut().show_error(&error),
                },
                HostEvent::Completed(result) => {
                    if let Err(error) = result {
                        mode.borrow_mut().show_error(&error);
                    }
                    match connection.get_state().await {
                        Ok(state) => {
                            current_session_id = state.session_id.clone();
                            mode.borrow_mut()
                                .apply_connection_state_snapshot(project_state(state));
                        }
                        Err(error) => mode.borrow_mut().show_error(&error),
                    }
                }
                HostEvent::Status(status) => mode.borrow_mut().show_status(&status, "dim"),
                HostEvent::Warning(warning) => mode.borrow_mut().show_warning(&warning),
                HostEvent::Panel(panel) => {
                    transcript.borrow_mut().panel(&panel);
                }
                HostEvent::EchoLocal(text) => {
                    transcript.borrow_mut().echo_local(&text);
                }
                // `showThinkingSelector` (interactive-mode.ts:8285-8307) mounts
                // `ThinkingSelectorComponent`; selecting a level calls
                // `applyThinkingLevel`, cancelling just closes.
                HostEvent::ThinkingLevels { current, levels } => {
                    let levels_for_select = levels.clone();
                    let actions_for_select = actions.clone();
                    let levels_for_cancel = levels.clone();
                    let actions_for_cancel = actions.clone();
                    let picker = Rc::new(RefCell::new(ThinkingSelectorComponent::new(
                        current,
                        &levels,
                        Box::new(move |level| {
                            let _ = &levels_for_select;
                            actions_for_select
                                .borrow_mut()
                                .push(InputAction::ThinkingLevel(level));
                        }),
                        Box::new(move || {
                            let _ = &levels_for_cancel;
                            actions_for_cancel
                                .borrow_mut()
                                .push(InputAction::DismissSelector);
                        }),
                    )));
                    if let Some(handle) = overlay.take() {
                        handle.hide();
                    }
                    selector = None;
                    overlay = Some(ui.borrow_mut().show_overlay(
                        picker.clone() as Rc<RefCell<dyn TuiComponent>>,
                        Default::default(),
                    ));
                    thinking_selector = Some(picker);
                }
                HostEvent::LoginProviders(items) => {
                    let list = make_selector(items, selection_send.clone());
                    overlay = Some(
                        ui.borrow_mut()
                            .show_overlay(list.clone(), Default::default()),
                    );
                    selector = Some(list);
                }
                HostEvent::BeginLogin(provider, oauth) => {
                    let tx = send.clone();
                    let mut dialog = LoginDialogComponent::new(
                        ui.clone(),
                        &provider,
                        Box::new(move |success, error| {
                            if !success {
                                let _ = tx.send(HostEvent::LoginFinished(Err(
                                    error.unwrap_or_else(|| "Login cancelled".into())
                                )));
                            }
                        }),
                        None,
                        None,
                    );
                    let token = tokio_util::sync::CancellationToken::new();
                    if oauth {
                        dialog.show_progress("Starting sign-in...");
                    }
                    let dialog = Rc::new(RefCell::new(dialog));
                    overlay = Some(
                        ui.borrow_mut()
                            .show_overlay(dialog.clone(), Default::default()),
                    );
                    login_dialog = Some(dialog);
                    login_cancel = Some(token.clone());
                    start_login(provider, oauth, send.clone(), token);
                }
                HostEvent::LoginAuth(url, instructions) => {
                    if let Some(dialog) = &login_dialog {
                        dialog.borrow_mut().show_auth(&url, instructions.as_deref());
                    }
                }
                HostEvent::LoginProgress(message) => {
                    if let Some(dialog) = &login_dialog {
                        dialog.borrow_mut().show_progress(&message);
                    }
                }
                HostEvent::LoginPrompt(message, placeholder, sender) => {
                    if let Some(dialog) = &login_dialog {
                        let receiver = dialog
                            .borrow_mut()
                            .show_prompt(&message, placeholder.as_deref());
                        tokio::spawn(async move {
                            if let Ok(value) = receiver.await {
                                let _ = sender.send(value);
                            }
                        });
                    }
                }
                HostEvent::LoginFinished(result) => {
                    if let Some(cancel) = login_cancel.take() {
                        cancel.cancel();
                    }
                    if let Some(handle) = overlay.take() {
                        handle.hide();
                    }
                    login_dialog = None;
                    match result {
                        Ok(()) => {
                            mode.borrow_mut().show_status(
                                "Credentials saved. Choose a model with /model.",
                                "success",
                            );
                            if let Some(runtime) = &options.runtime {
                                runtime
                                    .services()
                                    .model_registry
                                    .lock()
                                    .map_err(|e| e.to_string())?
                                    .refresh();
                            }
                            let command = pending_login_model
                                .take()
                                .map(|key| format!("/model {key}"))
                                .unwrap_or_else(|| "/model".into());
                            submit(&connection, &send, command, false, None);
                        }
                        Err(error) => {
                            pending_login_model = None;
                            if error != "Login cancelled" {
                                mode.borrow_mut().show_error(&error);
                            }
                        }
                    }
                }
                HostEvent::AgentsView => {
                    // `requestAgentsView` (interactive-mode.ts:3509-3519): resident
                    // sessions return to the agents view, ephemeral ones report why
                    // they cannot.
                    mode.borrow_mut().request_agents_view();
                }
                HostEvent::Models(catalog, search) => {
                    models = catalog.models;
                    configured_providers = catalog.configured_providers.into_iter().collect();
                    if let Some(model) = search.as_deref().and_then(|query| {
                        crate::core::model_resolver::find_exact_model_reference_match(
                            query, &models,
                        )
                    }) {
                        let _ =
                            selection_send.send(Some(format!("{}/{}", model.provider, model.id)));
                    } else {
                        let current = mode.borrow().get_current_model().cloned();
                        let configured = configured_providers.iter().cloned().collect::<Vec<_>>();
                        if let Some(picker) = &model_selector {
                            picker.borrow_mut().update_state(
                                current.as_ref().map(model_item),
                                Some(models.iter().map(model_item).collect()),
                                Some(configured),
                            );
                        } else {
                            let scoped = mode
                                .borrow()
                                .get_scoped_model_state()
                                .into_iter()
                                .map(|entry| ScopedModelItem {
                                    model: model_item(&entry.model),
                                    thinking_level: None,
                                })
                                .collect();
                            let recent = mode
                                .borrow()
                                .settings_manager()
                                .lock()
                                .map_err(|error| error.to_string())?
                                .get_recent_models();
                            let picker = Rc::new(RefCell::new(make_model_selector(
                                current.as_ref(),
                                scoped,
                                &models,
                                configured,
                                recent,
                                search,
                                model_rows.clone(),
                            )));
                            if let Some(handle) = overlay.take() {
                                handle.hide();
                            }
                            selector = None;
                            overlay = Some(ui.borrow_mut().show_overlay(
                                picker.clone(),
                                pi_tui::tui::OverlayOptions {
                                    width: Some(pi_tui::tui::SizeValue::Number(96.0)),
                                    max_height: Some(pi_tui::tui::SizeValue::Percent(
                                        "100%".into(),
                                    )),
                                    ..Default::default()
                                },
                            ));
                            model_selector = Some(picker);
                        }
                    }
                }
            }
            ui.borrow_mut().request_render();
        }
        if extension
            .as_ref()
            .and_then(|dialog| dialog.deadline)
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            if let Some(dialog) = extension.take() {
                dialog.overlay.hide();
                let response = cancel_extension_response(&dialog.request.method);
                respond_extension(&connection, &send, (dialog.request.id, response));
            }
        }
        if extension.is_none()
            && login_dialog.is_none()
            && selector.is_none()
            && model_selector.is_none()
        {
            if let Some(request) = extension_queue.pop_front() {
                extension = extension_dialog(request.clone(), &ui, &extension_send);
                if extension.is_none() {
                    respond_extension(
                        &connection,
                        &send,
                        (
                            request.id,
                            wire::AgentConnectionExtensionUiResponse::Cancelled { cancelled: true },
                        ),
                    );
                }
                ui.borrow_mut().request_render();
            }
        }
        if mode.borrow().shutdown_requested {
            break;
        }
        if last_tick.elapsed() >= Duration::from_millis(250) {
            mode.borrow_mut().tick_working_pulse();
            ui.borrow_mut().request_render();
            last_tick = Instant::now();
        }
        // The pi-tui `Loader` repaints on an 80 ms interval; the host tick is
        // slower, so drive the extra frames while the working loader is mounted.
        if last_loader_tick.elapsed() >= Duration::from_millis(80) {
            if mode.borrow().should_show_working_loader() {
                ui.borrow_mut().request_render();
            }
            last_loader_tick = Instant::now();
        }
        // `showCtrlCExitHint`'s 2 s timer has no Rust counterpart, so the host
        // expires the hint on its own 16 ms cadence (interactive-mode.ts:7018-7025).
        mode.borrow_mut().expire_ctrl_c_exit_hint();
        let rows = ui.borrow().terminal_rows();
        model_rows.set(rows as f64);
        editor.borrow_mut().editor_mut().set_terminal_rows(rows);
        ui.borrow_mut().run_pending_render(now_ms());
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
    unsubscribe();
    if let Some(dialog) = extension {
        dialog.overlay.hide();
        let _ = connection
            .respond_to_extension_ui_request(
                &dialog.request.id,
                wire::AgentConnectionExtensionUiResponse::Cancelled { cancelled: true },
            )
            .await;
    }
    for request in extension_queue {
        let _ = connection
            .respond_to_extension_ui_request(
                &request.id,
                wire::AgentConnectionExtensionUiResponse::Cancelled { cancelled: true },
            )
            .await;
    }
    if let Some(cancel) = login_cancel {
        cancel.cancel();
    }
    mode.borrow_mut().shutdown().await;
    drop(guard);
    connection.dispose().await?;
    let result = if mode.borrow().agents_view_request.is_some() {
        Some(mode.borrow_mut().run().await)
    } else {
        None
    };
    match exit_error {
        Some(error) => Err(error),
        None => Ok(result),
    }
}

/// Dispatches one submitted line.
///
/// Port of the submission path at interactive-mode.ts:4784-5039: the line is
/// parsed through the shared built-in registry, built-ins run as local commands,
/// and every other line - free text and extension commands - reaches the model
/// exactly as typed.
fn submit(
    connection: &Arc<dyn wire::AgentConnection>,
    send: &mpsc::Sender<HostEvent>,
    text: String,
    follow_up: bool,
    images: Option<Vec<ImageContent>>,
) {
    let (connection, send) = (connection.clone(), send.clone());
    tokio::spawn(async move {
        // `const slashCommand = parseSlashCommand(text)` then
        // `resolveBuiltinSlashCommandName` (interactive-mode.ts:4784-4785).
        let dispatch = classify_submission(&text);
        // `/login` keeps its dedicated provider picker (interactive-mode.ts:4952-4956).
        let result = match dispatch {
            SlashDispatch::Builtin {
                name,
                args,
                raw,
            } if name == "login" && args.is_empty() => {
                let mut items = Vec::new();
                for provider in pi_ai::utils::oauth::get_oauth_providers() {
                    items.push(pi_tui::components::select_list::SelectItem {
                        value: format!("login:oauth:{}", provider.id),
                        label: provider.name,
                        description: Some("Subscription / OAuth".into()),
                        ..Default::default()
                    });
                }
                for (id, name) in crate::core::provider_display_names::built_in_provider_display_names()
                {
                    items.push(pi_tui::components::select_list::SelectItem {
                        value: format!("login:api:{id}"),
                        label: name.to_string(),
                        description: Some("API key".into()),
                        ..Default::default()
                    });
                }
                let _ = send.send(HostEvent::LoginProviders(items));
                Ok(())
            }
            SlashDispatch::Builtin {
                name,
                args,
                raw,
            } => {
                // `if (commandName === "login")` with an argument logs that
                // provider in directly (interactive-mode.ts:4952-4956).
                if name == "login" {
                    let _ = send.send(HostEvent::BeginLogin(args.trim().into(), false));
                    Ok(())
                } else {
                    run_builtin_command(&connection, &send, &raw, &name, &args)
                        .await
                        .map(|output| {
                            for event in output.into_events() {
                                let _ = send.send(event);
                            }
                        })
                }
            }
            // `!command` runs shell (interactive-mode.ts:5043-5070).
            SlashDispatch::Model(line) => match line.strip_prefix('!') {
                Some(command) => connection.execute_bash(command, None).await,
                // Anything else - free text and extension commands - prompts the
                // model with the original text (interactive-mode.ts:5130-5145).
                None => {
                    connection
                        .prompt(
                            &line,
                            Some(wire::AgentConnectionPromptOptions {
                                images,
                                streaming_behavior: Some(
                                    if follow_up { "followUp" } else { "steer" }.into(),
                                ),
                                ..Default::default()
                            }),
                        )
                        .await
                }
            },
        };
        let _ = send.send(HostEvent::Completed(result));
    });
}

/// `getAvailableThinkingLevels` (interactive-mode.ts:8189-8193).
///
/// The dispatch task holds only the connection, so it applies the same rule to
/// `AgentConnectionState` that `InteractiveMode::get_available_thinking_levels`
/// applies to its snapshot.
fn available_thinking_levels(
    state: &wire::AgentConnectionState,
) -> Vec<pi_agent_core::types::ThinkingLevel> {
    let levels = state.available_thinking_levels.clone();
    let supports_thinking = !levels.is_empty()
        && !(levels.len() == 1 && levels[0] == pi_agent_core::types::ThinkingLevel::Off);
    if supports_thinking {
        levels
    } else {
        Vec::new()
    }
}

/// The decision the host makes for one submitted line.
///
/// Port of interactive-mode.ts:4784-4786 (`parseSlashCommand` +
/// `resolveBuiltinSlashCommandName`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum SlashDispatch {
    /// A built-in command or one of its aliases: `name` is canonical, `args` are
    /// trimmed, `raw` is the command as the user typed it.
    Builtin {
        name: String,
        args: String,
        raw: String,
    },
    /// Free text, an extension command, or a bare `/` - it goes to the model
    /// exactly as typed (interactive-mode.ts:5130-5145).
    Model(String),
}

/// Resolves a submitted line through the shared built-in registry.
///
/// The registry (`core/slash_commands.rs`) is the single source of truth for
/// names and aliases, which is what routes `/clear`, `/usage`, `/thinking`,
/// `/rename`, and `/side` to their canonical commands.
fn classify_submission(text: &str) -> SlashDispatch {
    if let Some(command) = crate::core::slash_commands::resolve_leading_builtin_slash_command(text) {
        return SlashDispatch::Builtin {
            name: command.name,
            args: command.args,
            raw: text.trim().to_string(),
        };
    }

    // TypeScript dispatches a few commands straight off `parseSlashCommand`'s name
    // (`interactive-mode.ts:5025`) without them being registry entries, because
    // `resolveBuiltinSlashCommandName` only rewrites aliases and otherwise passes the
    // name through (slash-commands.ts:249-251). `/debug` is the remaining one: TS
    // handles it locally (`handleDebugCommand`, interactive-mode.ts:10259) instead of
    // sending it to the model, so the port must not forward it as chat text. The
    // registry gains no invented entry; the dispatcher reports it as a known command
    // the host does not implement yet.
    if let Some(command) = crate::core::slash_commands::parse_slash_command(text) {
        if command.name == "debug" {
            return SlashDispatch::Builtin {
                name: command.name,
                args: command.args,
                raw: text.trim().to_string(),
            };
        }
    }

    SlashDispatch::Model(text.to_string())
}

/// The text a local command leaves in the chat.
///
/// Each arm maps to one of the TypeScript's local render paths
/// (`showStatus`, `showWarning`, `echoLocalCommand`), so a dispatched command
/// never reaches the model as chat text.
enum CommandOutput {
    Nothing,
    Status(String),
    Warning(String),
    EchoLocal(String),
    /// `chatContainer.addChild(new Spacer(1)); addChild(new Text(info, 1, 0))`.
    Panel(String),
}

impl CommandOutput {
    /// Applies the output to the host, then reports any failure.
    fn report(self, mode: &Rc<RefCell<InteractiveMode>>, send: &mpsc::Sender<HostEvent>) {
        match self {
            CommandOutput::Nothing => {}
            CommandOutput::Status(status) => {
                mode.borrow_mut().show_status(&status, "dim");
            }
            CommandOutput::Warning(warning) => {
                mode.borrow_mut().show_warning(&warning);
            }
            CommandOutput::EchoLocal(text) => {
                let _ = send.send(HostEvent::EchoLocal(text));
            }
            CommandOutput::Panel(panel) => {
                let _ = send.send(HostEvent::Panel(panel));
            }
        }
    }
}

impl CommandOutput {
    /// Turns the local reply into the host events that render it.
    ///
    /// The mode handle is not `Send`, so the spawned dispatch task reports what
    /// to render instead of touching the UI itself.
    fn into_events(self) -> Vec<HostEvent> {
        match self {
            CommandOutput::Nothing => Vec::new(),
            CommandOutput::Status(status) => vec![HostEvent::Status(status)],
            CommandOutput::Warning(warning) => vec![HostEvent::Warning(warning)],
            CommandOutput::EchoLocal(text) => vec![HostEvent::EchoLocal(text)],
            CommandOutput::Panel(panel) => vec![HostEvent::Panel(panel)],
        }
    }
}

/// `handleSessionCommand` (interactive-mode.ts:9481-9502).
async fn session_panel(
    connection: &Arc<dyn wire::AgentConnection>,
    session_name: Option<String>,
) -> Result<CommandOutput, String> {
    let stats = connection.get_session_stats().await?;
    let text = |key: &str| {
        stats
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .map(|value| (value as i64).to_string())
            .unwrap_or_else(|| "0".to_string())
    };
    let mut info = String::from("Session Info\n\n");
    if let Some(name) = session_name {
        info.push_str(&format!("Name: {name}\n"));
    }
    info.push_str(&format!(
        "File: {}\nID: {}\n\nMessages\n",
        stats
            .get("sessionFile")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("In-memory"),
        stats
            .get("sessionId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
    ));
    info.push_str(&format!("User: {}\n", text("userMessages")));
    info.push_str(&format!("Assistant: {}\n", text("assistantMessages")));
    info.push_str(&format!("Tool Calls: {}\n", text("toolCalls")));
    info.push_str(&format!("Tool Results: {}\n", text("toolResults")));
    info.push_str(&format!("Total: {}\n\n", text("totalMessages")));
    info.push_str("Use /context for token, cost, and context usage.");
    Ok(CommandOutput::EchoLocal(info))
}

/// `handleLogsCommand` (interactive-mode.ts:9504-9536).
fn logs_panel() -> CommandOutput {
    let logs_dir = crate::config::get_logs_dir();
    let mut info = format!("Logs\n\nDirectory: {logs_dir}\n\n");
    let mut files: Vec<String> = std::fs::read_dir(&logs_dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| !name.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    if files.is_empty() {
        info.push_str("No logs written yet.\n");
    } else {
        for name in files {
            let size = std::fs::metadata(format!("{logs_dir}/{name}"))
                .map(|metadata| format!(" ({:.1} KB)", metadata.len() as f64 / 1024.0))
                .unwrap_or_default();
            info.push_str(&format!("\u{2022} {name}{size}\n"));
        }
    }
    info.push_str(
        "\nDaemon crashes log to <socket>.log; agent-open failures log to client-errors.log.",
    );
    CommandOutput::EchoLocal(info)
}

/// `handleChangelogCommand` (interactive-mode.ts:10004-10021).
fn changelog_panel() -> CommandOutput {
    let entries = crate::utils::changelog::parse_changelog(&crate::config::get_changelog_path());
    let markdown = if entries.is_empty() {
        "No changelog entries found.".to_string()
    } else {
        entries
            .iter()
            .rev()
            .map(|entry| entry.content.clone())
            .collect::<Vec<String>>()
            .join("\n\n")
    };
    CommandOutput::EchoLocal(format!("What's New\n\n{markdown}"))
}

/// `handleRlmMaxDepthCommand` (interactive-mode.ts:9430-9479).
async fn rlm_max_depth_command(
    connection: &Arc<dyn wire::AgentConnection>,
    args: &str,
) -> Result<CommandOutput, String> {
    let tokens: Vec<&str> = if args.is_empty() {
        Vec::new()
    } else {
        args.split_whitespace().collect()
    };
    if tokens.is_empty() {
        let status = connection.get_rlm_max_depth_status().await?;
        let max_depth = status
            .get("maxDepth")
            .map(|value| value.to_string())
            .unwrap_or_default();
        let source = status
            .get("source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("default");
        return Ok(CommandOutput::Panel(format!(
            "RLM max depth: {max_depth} ({source})"
        )));
    }
    let global = tokens.get(1) == Some(&"--global");
    let valid_depth = tokens
        .first()
        .map(|token| !token.is_empty() && token.chars().all(|ch| ch.is_ascii_digit()))
        .unwrap_or(false);
    if tokens.len() > if global { 2 } else { 1 } || !valid_depth {
        return Ok(CommandOutput::Warning(
            "Usage: /rlm-max-depth [<non-negative integer> [--global]]".to_string(),
        ));
    }
    let max_depth: f64 = tokens[0]
        .parse()
        .map_err(|_| "RLM max depth must be a non-negative integer.".to_string())?;
    let result = connection
        .set_rlm_max_depth(max_depth, Some(serde_json::json!({ "global": global })))
        .await?;
    let depth = result
        .get("maxDepth")
        .map(|value| value.to_string())
        .unwrap_or_else(|| max_depth.to_string());
    let saved = result
        .get("globalSaved")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok(CommandOutput::Panel(format!(
        "RLM max depth set: {depth}{}",
        if saved {
            " and saved as global default"
        } else {
            ""
        }
    )))
}

/// `handleHeartbeatCommand` (interactive-mode.ts:9812-9873).
async fn heartbeat_command(
    connection: &Arc<dyn wire::AgentConnection>,
    command_text: &str,
) -> Result<CommandOutput, String> {
    match crate::core::cron_jobs::parse_heartbeat_command(command_text)? {
        crate::core::cron_jobs::ParsedHeartbeatCommand::Status => {
            Ok(match connection.get_heartbeat().await? {
                Some(job) => CommandOutput::Panel(format!("Heartbeat\n\n{job}")),
                None => CommandOutput::Panel("No active heartbeat".to_string()),
            })
        }
        crate::core::cron_jobs::ParsedHeartbeatCommand::Set {
            schedule,
            instruction,
            delivery_mode,
        } => {
            let job = connection
                .set_heartbeat(&schedule, &instruction, delivery_mode.as_deref())
                .await?;
            let delivery = job
                .get("deliveryMode")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(crate::core::cron_jobs::DEFAULT_HEARTBEAT_DELIVERY_MODE);
            let next_run = job
                .get("nextRunAt")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-");
            Ok(CommandOutput::Status(format!(
                "Heartbeat set\nDelivery: {delivery}\nNext run: {next_run}"
            )))
        }
        command => {
            let action = match command {
                crate::core::cron_jobs::ParsedHeartbeatCommand::Pause => "pause",
                crate::core::cron_jobs::ParsedHeartbeatCommand::Resume => "resume",
                _ => "clear",
            };
            let Some(job) = connection
                .update_heartbeat(serde_json::Value::String(action.to_string()))
                .await?
            else {
                return Ok(CommandOutput::Status("No active heartbeat".to_string()));
            };
            if action == "resume" {
                let next_run = job
                    .get("nextRunAt")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("-");
                return Ok(CommandOutput::Status(format!(
                    "Heartbeat resumed\nNext run: {next_run}"
                )));
            }
            Ok(CommandOutput::Status(
                if action == "clear" {
                    "Heartbeat cleared"
                } else {
                    "Heartbeat paused"
                }
                .to_string(),
            ))
        }
    }
}

/// Runs one built-in slash command.
///
/// Rust counterpart of the `if (commandName === "<name>")` chain at
/// interactive-mode.ts:4821-5030. `name` is always canonical because the
/// registry already resolved any alias.
async fn run_builtin_command(
    connection: &Arc<dyn wire::AgentConnection>,
    send: &mpsc::Sender<HostEvent>,
    text: &str,
    name: &str,
    args: &str,
) -> Result<CommandOutput, String> {
    match name {
        // `/model` keeps its existing search behaviour exactly.
        "model" => {
            let search = (!args.is_empty()).then(|| args.to_string());
            connection.get_model_catalog().await.map(|catalog| {
                let _ = send.send(HostEvent::Models(catalog, search));
            })?;
            Ok(CommandOutput::Nothing)
        }
        // `commandName === "effort"` (interactive-mode.ts:4843-4847): with no
        // argument the selector opens, with one the level is set directly.
        "effort" => {
            let state = connection.get_state().await?;
            let levels = available_thinking_levels(&state);
            if levels.is_empty() {
                return Ok(CommandOutput::Status(
                    "Current model does not support thinking".to_string(),
                ));
            }
            let requested = args.trim().to_lowercase();
            if requested.is_empty() {
                let _ = send.send(HostEvent::ThinkingLevels {
                    current: state.thinking_level,
                    levels,
                });
                return Ok(CommandOutput::Nothing);
            }
            let Some(level) = levels
                .iter()
                .find(|level| level.as_str() == requested.as_str())
                .copied()
            else {
                let available: Vec<&str> = levels.iter().map(|level| level.as_str()).collect();
                return Ok(CommandOutput::Warning(format!(
                    "Unknown thinking level '{requested}'. Available: {}",
                    available.join(", ")
                )));
            };
            connection.set_thinking_level(level).await?;
            Ok(CommandOutput::Status(format!(
                "Thinking level: {}",
                level.as_str()
            )))
        }
        // `commandName === "fast"` (interactive-mode.ts:4848-4853).
        "fast" => {
            if !args.is_empty() {
                return Ok(CommandOutput::Warning("Usage: /fast".to_string()));
            }
            let unavailable = "Fast mode requires GPT-5.4, GPT-5.5, or GPT-5.6 with ChatGPT or OpenAI API key authentication";
            let state = connection.get_state().await?;
            let supports = state
                .model
                .as_ref()
                .map(pi_ai::models::supports_fast_mode)
                .unwrap_or(false);
            if !supports {
                return Ok(CommandOutput::Status(unavailable.to_string()));
            }
            let enabled =
                state.service_tier.as_ref().and_then(|tier| tier.as_deref()) == Some("priority");
            connection
                .set_service_tier(Some(Some(
                    if enabled { "default" } else { "priority" }.to_string(),
                )))
                .await?;
            let next = connection.get_state().await?;
            let on =
                next.service_tier.as_ref().and_then(|tier| tier.as_deref()) == Some("priority");
            Ok(CommandOutput::Status(format!(
                "Fast mode: {}",
                if on { "on" } else { "off" }
            )))
        }
        // `commandName === "name"` (interactive-mode.ts:9410-9428).
        "name" => {
            let requested = text
                .trim()
                .strip_prefix("/name")
                .or_else(|| text.trim().strip_prefix("/rename"))
                .unwrap_or("")
                .trim()
                .to_string();
            if requested.is_empty() {
                let state = connection.get_state().await?;
                return Ok(match state.session_name {
                    Some(current) => CommandOutput::Panel(format!("Session name: {current}")),
                    None => CommandOutput::Warning("Usage: /name <name>".to_string()),
                });
            }
            connection.set_session_name(&requested).await?;
            Ok(CommandOutput::Panel(format!(
                "Session name set: {requested}"
            )))
        }
        // `commandName === "session"` (interactive-mode.ts:4885-4889).
        "session" => {
            let state = connection.get_state().await?;
            session_panel(connection, state.session_name).await
        }
        // `commandName === "system-prompt"` (interactive-mode.ts:4891-4895).
        "system-prompt" => {
            let prompt = connection.get_system_prompt().await?;
            Ok(CommandOutput::Panel(format!(
                "System Prompt ({} chars)\n\n{prompt}",
                prompt.chars().count()
            )))
        }
        // `commandName === "context"` (interactive-mode.ts:4903-4907).
        "context" => connection.get_session_stats().await.map(|stats| {
            CommandOutput::Panel(serde_json::to_string_pretty(&stats).unwrap_or_default())
        }),
        // `commandName === "logs"` (interactive-mode.ts:4909-4913).
        "logs" => Ok(logs_panel()),
        // `commandName === "changelog"` (interactive-mode.ts:4925-4929).
        "changelog" => Ok(changelog_panel()),
        // `commandName === "rlm-max-depth"` (interactive-mode.ts:4881-4884).
        "rlm-max-depth" => rlm_max_depth_command(connection, args).await,
        // `commandName === "heartbeat"` (interactive-mode.ts:4914-4918).
        "heartbeat" => heartbeat_command(connection, text).await,
        // `commandName === "heartbeats"` (interactive-mode.ts:4919-4923).
        "heartbeats" => connection.list_heartbeats().await.map(|heartbeats| {
            if heartbeats.is_empty() {
                return CommandOutput::Panel("No heartbeats.".to_string());
            }
            let lines: Vec<String> = heartbeats
                .iter()
                .map(|heartbeat| {
                    crate::core::cron_jobs::format_agent_cron_job(
                        &serde_json::from_value(heartbeat.job.clone()).unwrap_or_default(),
                    )
                })
                .collect();
            CommandOutput::Panel(lines.join("\n"))
        }),
        // `commandName === "export"` (interactive-mode.ts:4854-4858, 9210-9225).
        "export" => {
            let output_path = path_command_argument(text.trim(), "/export");
            let as_jsonl = output_path
                .as_deref()
                .map(|path| path.ends_with(".jsonl"))
                .unwrap_or(false);
            let exported = if as_jsonl {
                connection.export_to_jsonl(output_path.as_deref()).await
            } else {
                connection.export_to_html(output_path.as_deref()).await
            };
            exported.map(|file_path| {
                CommandOutput::Status(format!("Session exported to: {file_path}"))
            })
        }
        // `commandName === "import"` (interactive-mode.ts:4859-4863, 9255-9270).
        "import" => {
            let Some(input_path) = path_command_argument(text.trim(), "/import") else {
                return Ok(CommandOutput::Warning("Usage: /import <path.jsonl>".to_string()));
            };
            let cancelled = connection.import_from_jsonl(&input_path, None).await?;
            if cancelled {
                return Ok(CommandOutput::Status("Import cancelled".to_string()));
            }
            Ok(CommandOutput::Status(format!(
                "Session imported from: {input_path}"
            )))
        }
        // `commandName === "copy"` (interactive-mode.ts:4868-4872, 9395-9408).
        "copy" => {
            let Some(last) = connection.get_last_assistant_text().await? else {
                return Ok(CommandOutput::Status(
                    "No agent messages to copy yet.".to_string(),
                ));
            };
            crate::utils::clipboard::copy_to_clipboard(&last)
                .await
                .map_err(|error| error.to_string())?;
            Ok(CommandOutput::Status(
                "Copied last agent message to clipboard".to_string(),
            ))
        }
        // `commandName === "clone"` (interactive-mode.ts:4942-4945, 8582-8602).
        "clone" => {
            let tree = connection.get_session_tree().await?;
            let Some(leaf_id) = tree.leaf_id.clone() else {
                return Ok(CommandOutput::Status("Nothing to clone yet".to_string()));
            };
            let result = connection
                .fork(
                    &leaf_id,
                    Some(wire::AgentConnectionForkOptions {
                        position: Some("at".into()),
                        ..Default::default()
                    }),
                )
                .await?;
            if result
                .get("cancelled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(CommandOutput::Nothing);
            }
            Ok(CommandOutput::Status("Cloned to new session".to_string()))
        }
        // `commandName === "resume"` (interactive-mode.ts:4991-4994, 8734-8750).
        "resume" => {
            if args.trim().is_empty() {
                let _ = send.send(HostEvent::AgentsView);
                return Ok(CommandOutput::Nothing);
            }
            let state = connection.get_state().await?;
            let resolved = crate::core::session_resolver::resolve_session_path(
                args.trim(),
                &state.cwd,
                state.session_dir.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())?;
            let session_path = match resolved {
                crate::core::session_resolver::ResolvedSession::Path { path }
                | crate::core::session_resolver::ResolvedSession::Local { path }
                | crate::core::session_resolver::ResolvedSession::Global { path, .. } => path,
            };
            if connection.switch_session(&session_path, None).await? {
                return Ok(CommandOutput::Status("Resume cancelled".to_string()));
            }
            Ok(CommandOutput::Status("Resumed session".to_string()))
        }
        // `commandName === "reload"` (interactive-mode.ts:4996-4999, 9124-9133).
        "reload" => {
            let state = connection.get_state().await?;
            if state.is_streaming {
                return Ok(CommandOutput::Warning(
                    "Wait for the current response to finish before reloading.".to_string(),
                ));
            }
            if state.is_compacting {
                return Ok(CommandOutput::Warning(
                    "Wait for compaction to finish before reloading.".to_string(),
                ));
            }
            connection.reload().await?;
            Ok(CommandOutput::Status(
                "Reloaded keybindings, extensions, skills, prompts, themes".to_string(),
            ))
        }
        // `commandName === "clear"` / `"new"` (interactive-mode.ts:4968-4989).
        // `/clear` is the alias, so the registry reports `new` with raw `/clear`.
        "new" => {
            let parsed = crate::core::new_session_command::parse_new_session_command(
                text.trim().strip_prefix("/new").unwrap_or(""),
            );
            let parsed = match parsed {
                Ok(parsed) => parsed,
                Err(error) => return Ok(CommandOutput::Warning(error)),
            };
            if parsed.prompt.is_some() {
                return Ok(CommandOutput::Warning(
                    "Usage: /new [--name <name>] [-- <prompt>]".to_string(),
                ));
            }
            if connection.new_session(None).await? {
                return Ok(CommandOutput::Nothing);
            }
            if let Some(name) = parsed.name {
                connection.set_session_name(&name).await?;
            }
            Ok(CommandOutput::Status("New session started".to_string()))
        }
        // `commandName === "quit"` (interactive-mode.ts:5035-5039) is handled in
        // the input loop, which owns `shutdown_requested`.
        "quit" => Ok(CommandOutput::Nothing),
        other => Ok(CommandOutput::Status(format!(
            "/{other} is recognised but the native host has no handler for it yet."
        ))),
    }
}

fn model_command_search(text: &str) -> Option<Option<String>> {
    let rest = text.trim().strip_prefix("/model")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let search = rest.trim();
    Some((!search.is_empty()).then(|| search.to_string()))
}

fn model_item(model: &wire::AgentConnectionModel) -> ModelItemModel {
    ModelItemModel {
        provider: model.provider.clone(),
        id: model.id.clone(),
        name: model.name.clone(),
        featured: model.featured.unwrap_or(false),
        raw: serde_json::to_value(model).unwrap_or_default(),
    }
}

fn make_model_selector(
    current: Option<&wire::AgentConnectionModel>,
    scoped: Vec<ScopedModelItem>,
    models: &[wire::AgentConnectionModel],
    configured: Vec<String>,
    recent: Vec<String>,
    search: Option<String>,
    rows: Rc<Cell<f64>>,
) -> ModelSelectorComponent {
    ModelSelectorComponent::new(
        current.map(model_item),
        scoped,
        ModelSelectorOptions {
            available_models: Some(models.iter().map(model_item).collect()),
            configured_providers: Some(configured),
            recent_models: Some(recent),
            initial_search_input: search,
            get_rows: Some(Rc::new(move || rows.get())),
            ..Default::default()
        },
    )
}

fn string(value: &serde_json::Value, key: &str) -> String {
    optional_string(value, key).unwrap_or_default()
}
fn optional_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}
fn number(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(|value| value.as_f64())
}
fn project_state(state: wire::AgentConnectionState) -> local::AgentConnectionState {
    let actions = &state.session_actions;
    let texts = |key| {
        actions
            .get(key)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| optional_string(v, "text"))
            })
            .collect::<Vec<_>>()
    };
    local::AgentConnectionState {
        active_session_id: state.active_session_id,
        cwd: state.cwd,
        model: state.model,
        thinking_level: state.thinking_level,
        service_tier: state.service_tier,
        available_thinking_levels: state.available_thinking_levels,
        is_streaming: state.is_streaming,
        is_compacting: state.is_compacting,
        is_bash_running: state.is_bash_running,
        retry_attempt: state.retry_attempt,
        steering_mode: state.steering_mode,
        follow_up_mode: state.follow_up_mode,
        session_file: state.session_file,
        session_id: state.session_id,
        session_name: state.session_name,
        session_dir: state.session_dir,
        leaf_id: state.leaf_id,
        auto_compaction_enabled: state.auto_compaction_enabled,
        message_count: state.message_count,
        session_actions: local::SessionActionSnapshot {
            steering: texts("steering"),
            follow_ups: texts("followUps"),
            queued_count: number(actions, "queuedCount").unwrap_or(0.0) as usize,
            active: optional_string(actions, "active"),
        },
        compaction_count: state.compaction_count,
        goal: serde_json::from_value(state.goal).unwrap_or_else(|_| empty_goal_state()),
        heartbeat: None,
        scoped_models: state
            .scoped_models
            .into_iter()
            .map(|model| local::AgentConnectionScopedModel { model: model.model })
            .collect(),
        active_tool_names: state.active_tool_names,
        context_usage: local::ContextUsage {
            tokens: number(&state.context_usage, "tokens"),
            context_window: number(&state.context_usage, "contextWindow").unwrap_or(0.0),
            percent: number(&state.context_usage, "percent"),
        },
        recap: state.recap,
    }
}

fn apply_event(
    mode: &Rc<RefCell<InteractiveMode>>,
    transcript: &Rc<RefCell<Transcript>>,
    event: wire::AgentConnectionSessionEvent,
) {
    let value = match &event {
        wire::AgentConnectionSessionEvent::Agent(event) => serde_json::to_value(event),
        event => serde_json::to_value(event),
    }
    .unwrap_or_default();
    match event.type_name() {
        "agent_start" => {
            let mut mode = mode.borrow_mut();
            mode.patch_connection_state(|s| s.is_streaming = true);
            mode.working_started_at = Some(now_ms());
        }
        "agent_end" => {
            let mut mode = mode.borrow_mut();
            mode.patch_connection_state(|s| {
                s.is_streaming = false;
                s.active_tool_names.clear();
            });
            mode.stop_working_loader();
        }
        "message_start" | "message_update" | "message_end" => {
            if let Some(message) = value
                .get("message")
                .and_then(|value| serde_json::from_value::<AgentMessage>(value.clone()).ok())
            {
                let kind = event.type_name();
                if message.role() == "assistant"
                    || (kind == "message_end" && message.role() != "assistant")
                {
                    transcript
                        .borrow_mut()
                        .message(message, kind != "message_end");
                }
                if kind == "message_end" {
                    mode.borrow_mut()
                        .patch_connection_state(|s| s.message_count += 1.0);
                }
            }
        }
        "tool_execution_start" => transcript.borrow_mut().tool_start(
            &string(&value, "toolCallId"),
            &string(&value, "toolName"),
            value.get("args").cloned().unwrap_or_default(),
        ),
        "tool_execution_end" | "tool_execution_update" => transcript.borrow_mut().tool_result(
            &string(&value, "toolCallId"),
            value
                .get("result")
                .or_else(|| value.get("partialResult"))
                .unwrap_or(&serde_json::Value::Null),
            value
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            event.type_name() == "tool_execution_update",
        ),
        "bash_start" => {
            mode.borrow_mut()
                .patch_connection_state(|s| s.is_bash_running = true);
            mode.borrow_mut()
                .show_status(&format!("$ {}", string(&value, "command")), "dim");
        }
        "bash_output" => mode
            .borrow_mut()
            .show_status(&string(&value, "chunk"), "text"),
        "bash_end" => {
            mode.borrow_mut()
                .patch_connection_state(|s| s.is_bash_running = false);
            if let Some(error) = optional_string(&value, "errorMessage") {
                mode.borrow_mut().show_error(&error);
            }
        }
        "goal_update" => {
            if let wire::AgentConnectionSessionEvent::GoalUpdate { goal } = event {
                mode.borrow_mut().handle_goal_update(&goal, 120.0);
                mode.borrow_mut().patch_connection_state(|s| s.goal = goal);
            }
        }
        "compaction_start" => {
            if let wire::AgentConnectionSessionEvent::CompactionStart {
                reason,
                custom_instructions,
            } = event
            {
                let mut mode = mode.borrow_mut();
                mode.patch_connection_state(|s| s.is_compacting = true);
                mode.start_compaction_loader(&reason, custom_instructions.as_deref());
            }
        }
        "compaction_end" => {
            let mut mode = mode.borrow_mut();
            mode.patch_connection_state(|s| s.is_compacting = false);
            mode.stop_compaction_loader();
            if let Some(error) = optional_string(&value, "errorMessage") {
                mode.show_error(&error);
            }
        }
        "recap_update" => {
            mode.borrow_mut().session_recap = optional_string(&value, "recap");
            mode.borrow_mut().render_recap();
        }
        "auto_retry_start" => mode
            .borrow_mut()
            .show_warning(&string(&value, "errorMessage")),
        _ => {}
    }
}

fn make_selector(
    items: Vec<pi_tui::components::select_list::SelectItem>,
    send: mpsc::Sender<Option<String>>,
) -> Rc<RefCell<pi_tui::components::select_list::SelectList>> {
    let mut list = pi_tui::components::select_list::SelectList::new(
        items,
        12,
        select_theme(),
        Default::default(),
    );
    let selected = send.clone();
    list.on_select = Some(Box::new(move |item| {
        let _ = selected.send(Some(item.value.clone()));
    }));
    list.on_cancel = Some(Box::new(move || {
        let _ = send.send(None);
    }));
    Rc::new(RefCell::new(list))
}

fn start_login(
    provider: String,
    oauth: bool,
    send: mpsc::Sender<HostEvent>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        runtime.block_on(async move {
        let mut auth = crate::core::auth_storage::AuthStorage::create(None, None);
        let result = if oauth {
            use pi_ai::utils::oauth::types::OAuthLoginCallbacks;
            let auth_send = send.clone();
            let prompt_send = send.clone();
            let progress_send = send.clone();
            let manual_send = send.clone();
            let callbacks = OAuthLoginCallbacks {
                on_auth: Some(Arc::new(move |info| { let _ = auth_send.send(HostEvent::LoginAuth(info.url, info.instructions)); })),
                on_progress: Some(Arc::new(move |message| { let _ = progress_send.send(HostEvent::LoginProgress(message)); })),
                on_prompt: Some(Arc::new(move |prompt| {
                    let (sender, receiver) = tokio::sync::oneshot::channel();
                    let _ = prompt_send.send(HostEvent::LoginPrompt(prompt.message, prompt.placeholder, sender));
                    Box::pin(async move { receiver.await.unwrap_or_default() })
                })),
                on_manual_code_input: Some(Arc::new(move || {
                    let (sender, receiver) = tokio::sync::oneshot::channel();
                    let _ = manual_send.send(HostEvent::LoginPrompt("Paste the authorization code or redirect URL:".into(), None, sender));
                    Box::pin(async move { receiver.await.map_err(|_| "Login cancelled".into()) })
                })),
                signal: Some(cancel.clone()),
                ..Default::default()
            };
            tokio::select! {
                _ = cancel.cancelled() => Err("Login cancelled".into()),
                result = auth.login(&provider, callbacks) => result,
            }
        } else {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let _ = send.send(HostEvent::LoginPrompt("Enter API key:".into(), None, sender));
            let key = tokio::select! { _ = cancel.cancelled() => None, value = receiver => value.ok() };
            match key {
                Some(key) if !key.trim().is_empty() => {
                    auth.set(&provider, crate::core::auth_storage::AuthCredential::ApiKey { key: key.trim().into(), prime_team: None });
                    Ok(())
                }
                Some(_) => Err("API key cannot be empty.".into()),
                None => Err("Login cancelled".into()),
            }
        };
        let errors = auth.drain_errors();
        let result = if result.is_ok() && !errors.is_empty() { Err(errors.join("\n")) } else { result };
        let _ = send.send(HostEvent::LoginFinished(result));
    })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_dialogs_return_real_component_selection_and_text() {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
        crate::core::keybindings::KeybindingsManager::new(Default::default(), None).install();
        let ui = Rc::new(RefCell::new(TUI::new(
            Box::new(pi_tui::terminal::ProcessTerminal::new()),
            None,
        )));
        let (send, receive) = mpsc::channel();
        let confirm = wire::AgentConnectionExtensionUiRequest {
            id: "confirm-1".into(),
            method: "confirm".into(),
            payload: serde_json::json!({"title":"Run tool?","message":"Review the request","timeout":5000}),
        };
        let dialog = extension_dialog(confirm, &ui, &send).expect("confirm dialog");
        assert!(dialog.deadline.is_some());
        dialog.component.borrow_mut().handle_input("\r");
        assert_eq!(
            receive.try_recv().unwrap(),
            (
                "confirm-1".into(),
                wire::AgentConnectionExtensionUiResponse::Confirmed { confirmed: true }
            )
        );
        dialog.overlay.hide();

        let input = wire::AgentConnectionExtensionUiRequest {
            id: "input-1".into(),
            method: "input".into(),
            payload: serde_json::json!({"title":"Project name"}),
        };
        let dialog = extension_dialog(input, &ui, &send).expect("input dialog");
        dialog.component.borrow_mut().handle_input("demo");
        dialog.component.borrow_mut().handle_input("\r");
        assert_eq!(
            receive.try_recv().unwrap(),
            (
                "input-1".into(),
                wire::AgentConnectionExtensionUiResponse::Value {
                    value: "demo".into()
                }
            )
        );
        dialog.overlay.hide();

        assert!(extension_dialog(
            wire::AgentConnectionExtensionUiRequest {
                id: "invalid".into(),
                method: "select".into(),
                payload: serde_json::json!({"title":"Select","options":[false]}),
            },
            &ui,
            &send
        )
        .is_none());
    }

    /// The registry resolves names and aliases; everything else reaches the model.
    /// This is the executable form of interactive-mode.ts:4784-4786.
    #[test]
    fn submission_classification_resolves_through_the_builtin_registry() {
        // Canonical names keep their arguments.
        assert_eq!(
            classify_submission("/effort high"),
            SlashDispatch::Builtin {
                name: "effort".into(),
                args: "high".into(),
                raw: "/effort high".into(),
            }
        );
        assert_eq!(
            classify_submission("  /effort   xhigh  ".trim()),
            SlashDispatch::Builtin {
                name: "effort".into(),
                args: "xhigh".into(),
                raw: "/effort   xhigh".into(),
            }
        );
        // Aliases resolve to their canonical target
        // (`builtin_slash_command_aliases`, core/slash_commands.rs).
        for (alias, canonical) in [
            ("/clear", "new"),
            ("/usage", "context"),
            ("/thinking", "effort"),
            ("/rename", "name"),
            ("/side", "btw"),
        ] {
            match classify_submission(alias) {
                SlashDispatch::Builtin { name, .. } => assert_eq!(name, canonical, "{alias}"),
                other => panic!("{alias} resolved to {other:?}"),
            }
        }
        // Aliases with arguments too.
        match classify_submission("/thinking minimal") {
            SlashDispatch::Builtin { name, args, .. } => {
                assert_eq!(name, "effort");
                assert_eq!(args, "minimal");
            }
            other => panic!("{other:?}"),
        }
        // Free text, extension commands, and a bare slash are not built-ins.
        for text in [
            "hello there",
            "tell me about /model",
            "/",
            "/not-a-builtin",
            "/effortx",
            "!ls -la",
        ] {
            assert_eq!(
                classify_submission(text),
                SlashDispatch::Model(text.to_string()),
                "{text}"
            );
        }
    }

    /// Every built-in the TypeScript dispatches is recognised here, and none of
    /// them fall through to the model by accident.
    ///
    /// `/debug` is listed because TypeScript dispatches it from
    /// `parseSlashCommand`'s name (`interactive-mode.ts:5025`) even though it is NOT a
    /// registry entry: `resolveBuiltinSlashCommandName` only maps aliases and passes
    /// unknown names through, so `commandName === "debug"` matches without the name
    /// ever being a `builtinSlashCommand`. The registry stays the authority for names
    /// and gains no invented entry; the dispatcher recognises `debug` as a known
    /// command the host does not implement yet.
    #[test]
    fn every_dispatched_builtin_is_a_registry_name() {
        for name in [
            "btw", "changelog", "clone", "copy", "context", "debug", "effort", "export", "fast",
            "fork", "fullscreen", "heartbeat", "heartbeats", "hotkeys", "import", "login",
            "logout", "logs", "mcp", "model", "name", "new", "reload", "resume",
            "rlm-max-depth", "scoped-models", "session", "settings", "share", "system-prompt",
            "traces", "tree", "update",
        ] {
            match classify_submission(&format!("/{name}")) {
                SlashDispatch::Builtin { name: resolved, .. } => assert_eq!(resolved, name),
                other => panic!("/{name} resolved to {other:?}"),
            }
        }
    }

    /// `availableThinkingLevels` (interactive-mode.ts:8189-8193): a model that
    /// only reports `off` supports no thinking, so `/effort` says so.
    #[test]
    fn available_thinking_levels_drops_off_only_models() {
        let mut state = wire::AgentConnectionState::default();
        state.available_thinking_levels =
            vec![pi_agent_core::types::ThinkingLevel::Off];
        assert!(available_thinking_levels(&state).is_empty());

        state.available_thinking_levels = vec![
            pi_agent_core::types::ThinkingLevel::Off,
            pi_agent_core::types::ThinkingLevel::High,
        ];
        assert_eq!(available_thinking_levels(&state).len(), 2);

        state.available_thinking_levels = Vec::new();
        assert!(available_thinking_levels(&state).is_empty());
    }

    #[test]
    fn model_command_supports_prefilled_search_without_consuming_other_commands() {
        assert_eq!(model_command_search(" /model "), Some(None));
        assert_eq!(
            model_command_search("/model signed/model-2"),
            Some(Some("signed/model-2".into()))
        );
        assert_eq!(
            model_command_search("/model\t model 2 "),
            Some(Some("model 2".into()))
        );
        assert_eq!(model_command_search("/models"), None);
        assert_eq!(model_command_search("tell me about /model"), None);
    }

    /// A mode with no connection, enough for the Ctrl+S dispatch chain.
    fn stash_mode(session_id: &str) -> InteractiveMode {
        crate::modes::interactive::theme::theme::init_theme(Some("prime"), false);
        crate::core::keybindings::KeybindingsManager::new(Default::default(), None).install();
        let services = local::InteractiveModeUiServices {
            settings_manager: Arc::new(std::sync::Mutex::new(
                local::SettingsManager::in_memory(serde_json::Map::new()),
            )),
            model_registry: Arc::new(std::sync::Mutex::new(local::ModelRegistry::in_memory())),
            get_initial_cwd: Box::new(|| "/initial".to_string()),
            get_initial_session_name: Box::new(|| Some("initial".to_string())),
            get_themes: Box::new(Vec::new),
            refresh_mcp_providers: None,
        };
        InteractiveMode::new(InteractiveModeOptions {
            migrated_providers: None,
            model_fallback_message: None,
            startup_notice: None,
            initial_message: None,
            initial_images: None,
            initial_messages: None,
            initial_prompts: None,
            verbose: false,
            agent_connection: Arc::new(()),
            daemon_socket_path: None,
            local_session_host: None,
            bind_local_session_extensions: false,
            ui_services: Some(services),
            on_shutdown: None,
            return_to_agents_view: true,
            force_fullscreen: false,
            agents_view_owns_startup_notices: false,
            session_depth: None,
            session_has_children: false,
            prompt_stash_store: Some(
                crate::modes::interactive::prompt_stash_state::shared_prompt_stash_store(),
            ),
            prompt_stash_session_id: Some(session_id.to_string()),
        })
        .expect("mode")
    }

    /// The Ctrl+S byte reaches a real handler: the editor is cleared and a stash is
    /// stored. This is the row-2 path (`interactive-mode.ts:4307`).
    #[test]
    fn ctrl_s_dispatches_to_the_prompt_stash_handler() {
        let session_id = "host-ctrl-s-session";
        let mode = Rc::new(RefCell::new(stash_mode(session_id)));
        let tui = Rc::new(RefCell::new(TUI::new(
            Box::new(pi_tui::terminal::ProcessTerminal::new()),
            None,
        )));
        let editor = Rc::new(RefCell::new(CustomEditor::new(
            tui,
            editor_theme(),
            CustomEditorOptions::default(),
        )));
        let actions = Rc::new(RefCell::new(Vec::<InputAction>::new()));
        bind_editor_actions(&editor, &actions);
        editor
            .borrow_mut()
            .editor_mut()
            .set_text("half-written draft");

        // Ctrl+S decoded by pi-tui is 0x13.
        let stash_key = pi_tui::keybindings::get_keybindings()
            .get_keys("app.prompt.stash")
            .first()
            .cloned()
            .expect("ctrl+s binding");
        assert_eq!(stash_key, "ctrl+s");
        editor.borrow_mut().handle_input("\u{13}");

        let queued = actions.borrow();
        assert!(
            matches!(queued.as_slice(), [InputAction::PromptStash]),
            "Ctrl+S must queue the stash action, got {queued:?}"
        );
        drop(queued);
        actions.borrow_mut().clear();

        handle_prompt_stash_action(&mode, &editor, session_id);

        assert_eq!(editor.borrow().editor().get_text(), "");
        let stored = crate::modes::interactive::prompt_stash_state::PromptStashSession::open(
            crate::modes::interactive::prompt_stash_state::shared_prompt_stash_store(),
            session_id,
        )
        .state();
        assert_eq!(
            stored.stash.map(|stash| stash.text),
            Some("half-written draft".to_string())
        );
    }

    /// A real Ctrl+S round trip preserves the collapsed marker text and the paste
    /// table, so the restore is faithful (`interactive-mode.ts:4343-4356`, :4405-4410;
    /// test `interactive-mode-prompt-stash.test.ts:433-458`).
    #[test]
    fn ctrl_s_round_trip_keeps_the_collapsed_markers_and_paste_table() {
        let session_id = "host-paste-round-trip";
        let mode = Rc::new(RefCell::new(stash_mode(session_id)));
        let tui = Rc::new(RefCell::new(TUI::new(
            Box::new(pi_tui::terminal::ProcessTerminal::new()),
            None,
        )));
        let editor = Rc::new(RefCell::new(CustomEditor::new(
            tui,
            editor_theme(),
            CustomEditorOptions::default(),
        )));

        // A bracketed paste above the 10-line threshold collapses to a marker.
        let pasted = (1..=12)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        pi_tui::tui::Component::handle_input(
            &mut *editor.borrow_mut().editor_mut(),
            &format!("\u{1b}[200~{pasted}\u{1b}[201~"),
        );
        assert_eq!(
            editor.borrow().editor().get_text(),
            "[paste #1 +12 lines]",
            "the draft keeps the collapsed marker"
        );

        handle_prompt_stash_action(&mode, &editor, session_id);
        assert_eq!(editor.borrow().editor().get_text(), "", "stash clears the editor");

        let session = crate::modes::interactive::prompt_stash_state::PromptStashSession::open(
            crate::modes::interactive::prompt_stash_state::shared_prompt_stash_store(),
            session_id,
        );
        let stored = session.state().stash.expect("stash");
        assert_eq!(stored.text, "[paste #1 +12 lines]");
        assert_eq!(stored.expanded_text.as_deref(), Some(pasted.as_str()));
        assert_eq!(
            stored.paste_snapshot.as_ref().map(|snapshot| snapshot.paste_counter),
            Some(1)
        );

        let restored = session
            .restore_prompt_stash_if_editor_empty(None, "", true)
            .expect("restore");
        apply_prompt_stash_outcome(&mode, &editor, &restored);

        assert_eq!(editor.borrow().editor().get_text(), "[paste #1 +12 lines]");
        assert_eq!(
            editor.borrow().editor().get_expanded_text(),
            pasted,
            "the paste table is restored, so the marker expands again"
        );
    }

    /// The on-open restore notice must land in its own status block: `showStatus`
    /// replaces the anchored previous line, so `restorePromptStashOnOpen` drops the
    /// anchor first (`interactive-mode.ts:4360-4364`; test
    /// `interactive-mode-prompt-stash.test.ts:358-375`).
    #[test]
    fn the_on_open_restore_notice_starts_a_fresh_status_block() {
        let session_id = "host-on-open-anchor";
        let mut mode = stash_mode(session_id);
        mode.show_status("Compaction finished", "dim");
        let anchored = mode.chat_container.len();
        let before_second = anchored;

        // Without the anchor reset the second notice coalesces into one block.
        mode.show_status("Restored stashed prompt", "dim");
        assert_eq!(
            mode.chat_container.len(),
            before_second,
            "the coalescing path reuses the previous status block"
        );

        reset_status_anchor(&mut mode);
        mode.show_status("Restored stashed prompt", "dim");
        assert_eq!(
            mode.chat_container.len(),
            before_second + 2,
            "the restore notice appends a fresh spacer + line"
        );
    }

    /// An auto-stash for the agents view survives a reopen: the handoff stashes the
    /// draft (`interactive-mode.ts:7122`), the reopen restores it on open
    /// (`interactive-mode.ts:1628-1630`).
    #[test]
    fn agents_view_handoff_stashes_and_the_reopen_restores() {
        let session_id = "host-handoff-session";
        let mode = Rc::new(RefCell::new(stash_mode(session_id)));
        let tui = Rc::new(RefCell::new(TUI::new(
            Box::new(pi_tui::terminal::ProcessTerminal::new()),
            None,
        )));
        let editor = Rc::new(RefCell::new(CustomEditor::new(
            tui,
            editor_theme(),
            CustomEditorOptions::default(),
        )));
        editor
            .borrow_mut()
            .editor_mut()
            .set_text("draft typed before leaving");

        stash_editor_draft_for_agents_view(&mode, &editor, session_id);

        let store = crate::modes::interactive::prompt_stash_state::shared_prompt_stash_store();
        let session = crate::modes::interactive::prompt_stash_state::PromptStashSession::open(
            store,
            session_id,
        );
        assert!(session.restore_on_open_pending(), "handoff marks restoreOnOpen");

        // The reopened view restores the draft into its own empty editor.
        let reopened_editor = Rc::new(RefCell::new(CustomEditor::new(
            Rc::new(RefCell::new(TUI::new(
                Box::new(pi_tui::terminal::ProcessTerminal::new()),
                None,
            ))),
            editor_theme(),
            CustomEditorOptions::default(),
        )));
        let restored =
            session.restore_prompt_stash_if_editor_empty(None, "", true)
                .expect("the auto-stash restores");
        apply_prompt_stash_outcome(&mode, &reopened_editor, &restored);
        assert_eq!(
            reopened_editor.borrow().editor().get_text(),
            "draft typed before leaving"
        );
        assert!(!session.restore_on_open_pending(), "the stash is consumed");
    }
}
