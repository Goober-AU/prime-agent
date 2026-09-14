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
    custom_editor::{CustomEditor, CustomEditorOptions},
    extension_editor::{AppKeybindingsManager, ExtensionEditorComponent},
    extension_input::{ExtensionInputComponent, ExtensionInputOptions},
    extension_selector::{ExtensionSelectorComponent, ExtensionSelectorOptions},
    login_dialog::LoginDialogComponent,
    tool_execution::{ToolExecutionComponent, ToolExecutionOptions, ToolExecutionResult},
    user_message::UserMessageComponent,
};
use crate::modes::interactive::interactive_mode_services as local;
use pi_tui::components::text::Text as TuiText;
use pi_tui::tui::{Component as TuiComponent, InputListenerResult, TuiStopOptions, TUI};
use std::cell::RefCell;
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
        // These are the controller's existing status, recap, extension widgets,
        // and queue-context containers, rendered at the same position as TS.
        lines.extend(local::Component::render(
            &mode.chat_container,
            width.max(1.0) as usize,
        ));
        for container in mode.get_prompt_context_containers() {
            lines.extend(local::Component::render(container, width.max(1.0) as usize));
        }
        if mode.should_show_working_loader() {
            lines.push(theme().fg("dim", &format!("⠋ {}", mode.get_working_loader_message())));
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
    Interrupt,
    Escape,
    Exit,
    ToggleTools,
    ToggleThinking,
    ToggleMessages,
    Model,
    AgentsBack,
}
enum HostEvent {
    Connection(wire::AgentConnectionEvent),
    Completed(Result<(), String>),
    Status(String),
    Models(Vec<wire::AgentConnectionModel>),
    LoginProviders(Vec<pi_tui::components::select_list::SelectItem>),
    BeginLogin(String, bool),
    LoginAuth(String, Option<String>),
    LoginProgress(String),
    LoginPrompt(String, Option<String>, tokio::sync::oneshot::Sender<String>),
    LoginFinished(Result<(), String>),
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
        prompt_stash_store: None,
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
    for (binding, make) in [
        (
            "app.clear",
            (|| InputAction::Interrupt) as fn() -> InputAction,
        ),
        ("app.tools.expand", || InputAction::ToggleTools),
        ("app.thinking.toggle", || InputAction::ToggleThinking),
        ("app.messages.expand", || InputAction::ToggleMessages),
        ("app.model.select", || InputAction::Model),
    ] {
        let actions = actions.clone();
        editor
            .borrow_mut()
            .on_action(binding, Box::new(move || actions.borrow_mut().push(make())));
    }
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
    let mut selector: Option<Rc<RefCell<pi_tui::components::select_list::SelectList>>> = None;
    let mut overlay: Option<pi_tui::tui::OverlayHandle> = None;
    let (selection_send, selection_receive) = mpsc::channel::<Option<String>>();
    let mut models = Vec::<wire::AgentConnectionModel>::new();
    let mut login_dialog: Option<Rc<RefCell<LoginDialogComponent>>> = None;
    let mut login_cancel: Option<tokio_util::sync::CancellationToken> = None;
    let mut extension: Option<ExtensionDialog> = None;
    let mut extension_queue =
        std::collections::VecDeque::<wire::AgentConnectionExtensionUiRequest>::new();
    let (extension_send, extension_receive) = mpsc::channel::<ExtensionReply>();
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
            if let Some(dialog) = &extension {
                dialog.component.borrow_mut().handle_input(&data);
            } else if let Some(dialog) = &login_dialog {
                dialog.borrow_mut().handle_input(&data);
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
                    let (connection, send, model) =
                        (connection.clone(), send.clone(), model.clone());
                    tokio::spawn(async move {
                        let result = connection
                            .set_model(&model.provider, &model.id)
                            .await
                            .map(|_| ());
                        let _ = send.send(HostEvent::Completed(result));
                    });
                }
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
                    if matches!(text.trim(), "/help" | "/hotkeys") {
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
                        let connection = connection.clone();
                        let send = send.clone();
                        tokio::spawn(async move {
                            let result = connection.abort_and_clear_queue().await.map(|_| ());
                            let _ = connection.abort_bash().await;
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
                    mode.borrow_mut()
                        .return_to_agents_view(InteractiveModeRunResultType::AgentsView);
                }
                InputAction::Model => {
                    submit(&connection, &send, "/model".into(), false, None);
                }
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
                    mode.borrow_mut()
                        .apply_connection_state_snapshot(project_state(state));
                    transcript.borrow_mut().replace(messages);
                }
                HostEvent::Connection(wire::AgentConnectionEvent::SessionResynced { snapshot }) => {
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
                HostEvent::Completed(result) => {
                    if let Err(error) = result {
                        mode.borrow_mut().show_error(&error);
                    }
                    match connection.get_state().await {
                        Ok(state) => mode
                            .borrow_mut()
                            .apply_connection_state_snapshot(project_state(state)),
                        Err(error) => mode.borrow_mut().show_error(&error),
                    }
                }
                HostEvent::Status(status) => mode.borrow_mut().show_status(&status, "dim"),
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
                            submit(&connection, &send, "/model".into(), false, None);
                        }
                        Err(error) if error == "Login cancelled" => {}
                        Err(error) => mode.borrow_mut().show_error(&error),
                    }
                }
                HostEvent::Models(available) => {
                    models = available;
                    if models.is_empty() {
                        mode.borrow_mut().show_warning(
                            &crate::core::auth_guidance::format_no_models_available_message(),
                        );
                    } else {
                        let items = models
                            .iter()
                            .map(|model| pi_tui::components::select_list::SelectItem {
                                value: format!("{}/{}", model.provider, model.id),
                                label: model.name.clone(),
                                description: Some(model.provider.clone()),
                                ..Default::default()
                            })
                            .collect();
                        let mut list = pi_tui::components::select_list::SelectList::new(
                            items,
                            12,
                            select_theme(),
                            Default::default(),
                        );
                        let tx = selection_send.clone();
                        list.on_select = Some(Box::new(move |item| {
                            let _ = tx.send(Some(item.value.clone()));
                        }));
                        let tx = selection_send.clone();
                        list.on_cancel = Some(Box::new(move || {
                            let _ = tx.send(None);
                        }));
                        let list = Rc::new(RefCell::new(list));
                        overlay = Some(
                            ui.borrow_mut()
                                .show_overlay(list.clone(), Default::default()),
                        );
                        selector = Some(list);
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
        if extension.is_none() && login_dialog.is_none() && selector.is_none() {
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
        let rows = ui.borrow().terminal_rows();
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

fn submit(
    connection: &Arc<dyn wire::AgentConnection>,
    send: &mpsc::Sender<HostEvent>,
    text: String,
    follow_up: bool,
    images: Option<Vec<ImageContent>>,
) {
    let (connection, send) = (connection.clone(), send.clone());
    tokio::spawn(async move {
        let result = if text.trim() == "/login" {
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
        } else if let Some(provider) = text.trim().strip_prefix("/login ") {
            let _ = send.send(HostEvent::BeginLogin(provider.trim().into(), false));
            Ok(())
        } else if text.trim() == "/model" {
            connection.get_available_models().await.map(|models| {
                let _ = send.send(HostEvent::Models(models));
            })
        } else if text.trim() == "/context" {
            connection.get_session_stats().await.map(|stats| {
                let _ = send.send(HostEvent::Status(
                    serde_json::to_string_pretty(&stats).unwrap_or_default(),
                ));
            })
        } else if text.trim() == "/new" {
            connection.new_session(None).await.map(|_| ())
        } else if let Some(command) = text.strip_prefix('!') {
            connection.execute_bash(command, None).await
        } else {
            connection
                .prompt(
                    &text,
                    Some(wire::AgentConnectionPromptOptions {
                        images,
                        streaming_behavior: Some(
                            if follow_up { "followUp" } else { "steer" }.into(),
                        ),
                        ..Default::default()
                    }),
                )
                .await
        };
        let _ = send.send(HostEvent::Completed(result));
    });
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
        "compaction_start" => mode
            .borrow_mut()
            .patch_connection_state(|s| s.is_compacting = true),
        "compaction_end" => {
            mode.borrow_mut()
                .patch_connection_state(|s| s.is_compacting = false);
            if let Some(error) = optional_string(&value, "errorMessage") {
                mode.borrow_mut().show_error(&error);
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
}
