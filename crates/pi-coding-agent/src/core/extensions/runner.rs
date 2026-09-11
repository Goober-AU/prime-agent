//! Port of packages/coding-agent/src/core/extensions/runner.ts
//!
//! Extension runner - executes extensions and manages their lifecycle.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

use crate::core::diagnostics::ResourceDiagnostic;
use crate::core::slash_commands::SlashCommandInfo;

use super::types::{
    AbortSignal, BeforeAgentStartEventResult, CancelledResult, CompactOptions, ContextEventResult, ContextUsage,
    CustomMessagePayload, Extension, ExtensionActions, ExtensionCommandContext, ExtensionCommandContextActions,
    ExtensionContext, ExtensionContextActions, ExtensionError, ExtensionEvent, ExtensionFlag, ExtensionHandler,
    ExtensionRuntime, ExtensionShortcut, ExtensionUIContext, ExtensionUIDialogOptions, ForkOptions,
    InputEventResult, MessageEndEventResult, MessageRenderer, NavigateTreeOptions, NewSessionOptions,
    ProviderActions, ProviderConfig, RegisteredCommand, RegisteredTool, ReplacedSessionContext, ResolvedCommand,
    SendMessageOptions, SendUserMessageOptions, SessionBeforeCompactResult, SessionBeforeForkResult,
    SessionBeforeRefineResult, SessionBeforeSwitchResult, SessionBeforeTreeResult, SharedExtension, SwitchSessionOptions,
    Theme, ToolCallEvent, ToolCallEventResult, ToolInfo, ToolResultEvent, ToolResultEventResult, UserBashEventResult,
    WidgetPlacement, WorkingIndicatorOptions,
};
use super::types::{
    AutocompleteItem, AutocompleteProviderFactory, Component, EditorFactory, ExtensionWidgetOptions,
    ReadonlyFooterDataProvider, ReadonlySessionManager, SessionManager, TerminalInputHandler, ThemeInfo,
    SetThemeResult, ModelRegistry,
};

// Extension shortcuts compete with canonical keybinding ids from keybindings.json.
// Only editor-global shortcuts are reserved here. Picker-specific bindings are not.
pub const RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS: [&str; 18] = [
    "app.interrupt",
    "app.clear",
    "app.exit",
    "app.suspend",
    "app.model.select",
    "app.tools.expand",
    "app.messages.expand",
    "app.edits.expand",
    "app.thinking.toggle",
    "app.subagents.focus",
    "app.editor.external",
    "app.message.followUp",
    "tui.input.submit",
    "tui.select.confirm",
    "tui.select.cancel",
    "tui.input.copy",
    "tui.editor.deleteToLineEnd",
];

/// `{ keybinding; restrictOverride }`.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltInKeyBinding {
    pub keybinding: String,
    pub restrict_override: bool,
}

/// `buildBuiltinKeybindings(resolvedKeybindings)`.
pub fn build_builtin_keybindings(resolved_keybindings: &Map<String, Value>) -> HashMap<String, BuiltInKeyBinding> {
    let mut builtin_keybindings: HashMap<String, BuiltInKeyBinding> = HashMap::new();
    for (keybinding, keys) in resolved_keybindings {
        if keys.is_null() {
            continue;
        }
        let key_list: Vec<String> = match keys {
            Value::Array(list) => list
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect(),
            Value::String(value) => vec![value.clone()],
            _ => continue,
        };
        let restrict_override = RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS.contains(&keybinding.as_str());
        for key in key_list {
            let normalized_key = key.to_lowercase();
            // If multiple actions bind the same key, the reserved action wins so
            // extensions remain blocked by reserved shortcuts regardless of
            // iteration order.
            let existing = builtin_keybindings.get(&normalized_key);
            if existing.map(|existing| existing.restrict_override).unwrap_or(false) && !restrict_override {
                continue;
            }
            builtin_keybindings.insert(
                normalized_key,
                BuiltInKeyBinding {
                    keybinding: keybinding.clone(),
                    restrict_override,
                },
            );
        }
    }
    builtin_keybindings
}

/// Combined result from all `before_agent_start` handlers.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAgentStartCombinedResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<CustomMessagePayload>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// `resources_discover` result entry `{ path; extensionPath }`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcePathEntry {
    pub path: String,
    pub extension_path: String,
}

/// Result of `emitResourcesDiscover`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesDiscoverPaths {
    pub skill_paths: Vec<ResourcePathEntry>,
    pub prompt_paths: Vec<ResourcePathEntry>,
    pub theme_paths: Vec<ResourcePathEntry>,
}

/// `ExtensionErrorListener`.
pub type ExtensionErrorListener = Arc<dyn Fn(ExtensionError) + Send + Sync>;

/// `NewSessionHandler`.
pub type NewSessionHandler = Arc<
    dyn Fn(Option<NewSessionOptions>) -> std::pin::Pin<Box<dyn std::future::Future<Output = CancelledResult> + Send>>
        + Send
        + Sync,
>;
/// `ForkHandler`.
pub type ForkHandler = Arc<
    dyn Fn(String, Option<ForkOptions>) -> std::pin::Pin<Box<dyn std::future::Future<Output = CancelledResult> + Send>>
        + Send
        + Sync,
>;
/// `NavigateTreeHandler`.
pub type NavigateTreeHandler = Arc<
    dyn Fn(String, Option<NavigateTreeOptions>) -> std::pin::Pin<Box<dyn std::future::Future<Output = CancelledResult> + Send>>
        + Send
        + Sync,
>;
/// `SwitchSessionHandler`.
pub type SwitchSessionHandler = Arc<
    dyn Fn(String, Option<SwitchSessionOptions>) -> std::pin::Pin<Box<dyn std::future::Future<Output = CancelledResult> + Send>>
        + Send
        + Sync,
>;
/// `ReloadHandler`.
pub type ReloadHandler =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;
/// `ShutdownHandler`.
pub type ShutdownHandler = Arc<dyn Fn() + Send + Sync>;

/// Helper to emit `session_shutdown` to extensions.
/// Returns true if the event was emitted, false if there were no handlers.
pub async fn emit_session_shutdown_event(
    extension_runner: &ExtensionRunner,
    event: ExtensionEvent,
) -> bool {
    if extension_runner.has_handlers("session_shutdown") {
        extension_runner.emit(event).await;
        return true;
    }
    false
}

/// `noOpUIContext` - every method is a no-op, exactly like the TypeScript object.
pub struct NoOpUiContext;

impl ExtensionUIContext for NoOpUiContext {
    fn select(
        &self,
        _title: String,
        _options: Vec<String>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
        Box::pin(async { None })
    }

    fn confirm(
        &self,
        _title: String,
        _message: String,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> {
        Box::pin(async { false })
    }

    fn input(
        &self,
        _title: String,
        _placeholder: Option<String>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
        Box::pin(async { None })
    }

    fn notify(&self, _message: String, _kind: Option<String>) {}

    fn on_terminal_input(&self, _handler: TerminalInputHandler) -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    fn set_status(&self, _key: String, _text: Option<String>) {}
    fn set_working_message(&self, _message: Option<String>) {}
    fn set_working_visible(&self, _visible: bool) {}
    fn set_working_indicator(&self, _options: Option<WorkingIndicatorOptions>) {}
    fn set_hidden_thinking_label(&self, _label: Option<String>) {}

    fn set_widget_strings(
        &self,
        _key: String,
        _content: Option<Vec<String>>,
        _options: Option<ExtensionWidgetOptions>,
    ) {
    }

    fn set_widget_factory(
        &self,
        _key: String,
        _content: Option<super::types::WidgetFactory>,
        _options: Option<ExtensionWidgetOptions>,
    ) {
    }

    fn set_footer(&self, _factory: Option<super::types::FooterFactory>) {}
    fn set_header(&self, _factory: Option<super::types::HeaderFactory>) {}
    fn set_title(&self, _title: String) {}

    fn custom(&self, _factory: Value, _options: Option<Value>) -> super::types::CustomComponentResult {
        Box::pin(async { Arc::new(NoOpComponent) })
    }

    fn paste_to_editor(&self, _text: String) {}
    fn set_editor_text(&self, _text: String) {}

    fn get_editor_text(&self) -> String {
        String::new()
    }

    fn editor(
        &self,
        _title: String,
        _prefill: Option<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
        Box::pin(async { None })
    }

    fn add_autocomplete_provider(&self, _factory: AutocompleteProviderFactory) {}
    fn set_editor_component(&self, _factory: Option<EditorFactory>) {}

    fn get_editor_component(&self) -> Option<EditorFactory> {
        None
    }

    fn theme(&self) -> Theme {
        Theme::default()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        Vec::new()
    }

    fn get_theme(&self, _name: String) -> Option<Theme> {
        None
    }

    fn set_theme(&self, _theme: Value) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: Some("UI not available".to_string()),
        }
    }

    fn get_tools_expanded(&self) -> bool {
        false
    }

    fn set_tools_expanded(&self, _expanded: bool) {}
}

/// A component that renders nothing.
pub struct NoOpComponent;

impl Component for NoOpComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        Vec::new()
    }
}

/// Null session manager used when no session is bound yet.
pub struct NullSessionManager;

impl ReadonlySessionManager for NullSessionManager {
    fn get_session_id(&self) -> String {
        String::new()
    }
    fn get_session_file(&self) -> Option<String> {
        None
    }
    fn get_session_dir(&self) -> String {
        String::new()
    }
    fn get_branch(&self) -> Vec<super::types::SessionEntry> {
        Vec::new()
    }
}

impl SessionManager for NullSessionManager {}

/// Null model registry used when no registry is bound yet.
pub struct NullModelRegistry;

impl ModelRegistry for NullModelRegistry {
    fn register_provider(&self, _name: &str, _config: &ProviderConfig) {}
    fn unregister_provider(&self, _name: &str) {}

    fn get_api_key_and_headers(
        &self,
        _model: &pi_ai::types::Model,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> {
        Box::pin(async { Ok(Value::Null) })
    }
}


/// Bindable callbacks. `bindCore()` and `bindCommandContext()` replace these
/// after construction; the TypeScript reassigns plain fields, so the port keeps
/// one lock-protected record instead of unsafe field mutation.
pub struct RunnerCallbacks {
    pub get_model: Arc<dyn Fn() -> Option<pi_ai::types::Model> + Send + Sync>,
    pub is_idle: Arc<dyn Fn() -> bool + Send + Sync>,
    pub get_signal: Arc<dyn Fn() -> Option<AbortSignal> + Send + Sync>,
    pub wait_for_idle: Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>,
    pub abort: Arc<dyn Fn() + Send + Sync>,
    pub has_pending_messages: Arc<dyn Fn() -> bool + Send + Sync>,
    pub shutdown: ShutdownHandler,
    pub get_context_usage: Arc<dyn Fn() -> Option<ContextUsage> + Send + Sync>,
    pub compact: Arc<dyn Fn(Option<CompactOptions>) + Send + Sync>,
    pub get_system_prompt: Arc<dyn Fn() -> String + Send + Sync>,
    pub new_session: NewSessionHandler,
    pub fork: ForkHandler,
    pub navigate_tree: NavigateTreeHandler,
    pub switch_session: SwitchSessionHandler,
    pub reload: ReloadHandler,
}

impl Default for RunnerCallbacks {
    fn default() -> Self {
        Self {
            get_model: Arc::new(|| None),
            is_idle: Arc::new(|| true),
            get_signal: Arc::new(|| None),
            wait_for_idle: Arc::new(|| Box::pin(async {})),
            abort: Arc::new(|| {}),
            has_pending_messages: Arc::new(|| false),
            shutdown: Arc::new(|| {}),
            get_context_usage: Arc::new(|| None),
            compact: Arc::new(|_| {}),
            get_system_prompt: Arc::new(String::new),
            new_session: Arc::new(|_| Box::pin(async { CancelledResult { cancelled: false } })),
            fork: Arc::new(|_, _| Box::pin(async { CancelledResult { cancelled: false } })),
            navigate_tree: Arc::new(|_, _| Box::pin(async { CancelledResult { cancelled: false } })),
            switch_session: Arc::new(|_, _| Box::pin(async { CancelledResult { cancelled: false } })),
            reload: Arc::new(|| Box::pin(async {})),
        }
    }
}

impl std::fmt::Debug for RunnerCallbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerCallbacks").finish_non_exhaustive()
    }
}

/// `ExtensionRunner`.
pub struct ExtensionRunner {
    extensions: Vec<SharedExtension>,
    runtime: ExtensionRuntime,
    ui_context: Arc<dyn ExtensionUIContext>,
    cwd: String,
    session_manager: Arc<dyn SessionManager>,
    model_registry: Arc<dyn ModelRegistry>,
    error_listeners: Mutex<Vec<ExtensionErrorListener>>,
    callbacks: Mutex<RunnerCallbacks>,
    shortcut_diagnostics: Mutex<Vec<ResourceDiagnostic>>,
    command_diagnostics: Mutex<Vec<ResourceDiagnostic>>,
    stale_message: Mutex<Option<String>>,
}

impl std::fmt::Debug for ExtensionRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionRunner")
            .field("cwd", &self.cwd)
            .field("extensions", &self.extensions.len())
            .finish_non_exhaustive()
    }
}

impl ExtensionRunner {
    pub fn new(
        extensions: Vec<SharedExtension>,
        runtime: ExtensionRuntime,
        cwd: String,
        session_manager: Arc<dyn SessionManager>,
        model_registry: Arc<dyn ModelRegistry>,
    ) -> Self {
        Self {
            extensions,
            runtime,
            ui_context: Arc::new(NoOpUiContext),
            cwd,
            session_manager,
            model_registry,
            error_listeners: Mutex::new(Vec::new()),
            callbacks: Mutex::new(RunnerCallbacks::default()),
            shortcut_diagnostics: Mutex::new(Vec::new()),
            command_diagnostics: Mutex::new(Vec::new()),
            stale_message: Mutex::new(None),
        }
    }

    /// `bindCore(actions, contextActions, providerActions?)`.
    pub fn bind_core(
        &self,
        actions: ExtensionActions,
        context_actions: ExtensionContextActions,
        provider_actions: Option<ProviderActions>,
    ) {
        {
            let mut guard = self
                .runtime
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.actions = Some(actions);
            if let Some(provider_actions) = provider_actions {
                guard.provider_actions = Some(provider_actions);
            }
        }
        let mut callbacks = self
            .callbacks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        callbacks.get_model = context_actions.get_model.clone();
        callbacks.is_idle = context_actions.is_idle.clone();
        callbacks.get_signal = context_actions.get_signal.clone();
        callbacks.abort = context_actions.abort.clone();
        callbacks.has_pending_messages = context_actions.has_pending_messages.clone();
        callbacks.shutdown = context_actions.shutdown.clone();
        callbacks.get_context_usage = context_actions.get_context_usage.clone();
        callbacks.compact = context_actions.compact.clone();
        callbacks.get_system_prompt = context_actions.get_system_prompt.clone();
        drop(callbacks);

        for registration in self.runtime.take_pending_provider_registrations() {
            let result = {
                let guard = self
                    .runtime
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match guard
                    .provider_actions
                    .as_ref()
                    .and_then(|actions| actions.register_provider.clone())
                {
                    Some(register) => {
                        register(registration.name.clone(), registration.config.clone());
                        Ok(())
                    }
                    None => {
                        self.model_registry
                            .register_provider(&registration.name, &registration.config);
                        Ok(())
                    }
                }
            };
            if let Err(error) = result {
                self.emit_error(ExtensionError {
                    extension_path: registration.extension_path,
                    event: "register_provider".to_string(),
                    error,
                    stack: None,
                });
            }
        }
    }

    /// `bindCommandContext(actions?)`.
    pub fn bind_command_context(&self, actions: Option<ExtensionCommandContextActions>) {
        let mut callbacks = self
            .callbacks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match actions {
            Some(actions) => {
                callbacks.wait_for_idle = actions.wait_for_idle;
                callbacks.new_session = actions.new_session;
                callbacks.fork = actions.fork;
                callbacks.navigate_tree = actions.navigate_tree;
                callbacks.switch_session = actions.switch_session;
                callbacks.reload = actions.reload;
            }
            None => {
                callbacks.wait_for_idle = Arc::new(|| Box::pin(async {}));
                callbacks.new_session = Arc::new(|_| Box::pin(async { CancelledResult { cancelled: false } }));
                callbacks.fork = Arc::new(|_, _| Box::pin(async { CancelledResult { cancelled: false } }));
                callbacks.navigate_tree = Arc::new(|_, _| Box::pin(async { CancelledResult { cancelled: false } }));
                callbacks.switch_session = Arc::new(|_, _| Box::pin(async { CancelledResult { cancelled: false } }));
                callbacks.reload = Arc::new(|| Box::pin(async {}));
            }
        }
    }

    pub fn set_ui_context(&mut self, ui_context: Option<Arc<dyn ExtensionUIContext>>) {
        self.ui_context = ui_context.unwrap_or_else(|| Arc::new(NoOpUiContext));
    }

    pub fn get_ui_context(&self) -> Arc<dyn ExtensionUIContext> {
        self.ui_context.clone()
    }

    /// `hasUI()` - true when a real (non-no-op) UI context is installed.
    pub fn has_ui(&self) -> bool {
        self.ui_context.as_ref().type_id() != std::any::TypeId::of::<NoOpUiContext>()
    }

    pub fn get_extension_paths(&self) -> Vec<String> {
        self.extensions
            .iter()
            .map(|extension| extension.lock().unwrap_or_else(|p| p.into_inner()).path.clone())
            .collect()
    }

    /// Get all registered tools from all extensions (first registration per name wins).
    pub fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        let mut tools_by_name: indexmap::IndexMap<String, RegisteredTool> = indexmap::IndexMap::new();
        for extension in &self.extensions {
            let guard = extension.lock().unwrap_or_else(|p| p.into_inner());
            for tool in guard.tools.values() {
                if !tools_by_name.contains_key(&tool.definition.name) {
                    tools_by_name.insert(tool.definition.name.clone(), tool.clone());
                }
            }
        }
        tools_by_name.into_values().collect()
    }

    /// Get a tool definition by name. Returns `None` if not found.
    pub fn get_tool_definition(&self, tool_name: &str) -> Option<super::types::ToolDefinition> {
        for extension in &self.extensions {
            let guard = extension.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(tool) = guard.tools.get(tool_name) {
                return Some(tool.definition.clone());
            }
        }
        None
    }

    pub fn get_flags(&self) -> indexmap::IndexMap<String, ExtensionFlag> {
        let mut all_flags: indexmap::IndexMap<String, ExtensionFlag> = indexmap::IndexMap::new();
        for extension in &self.extensions {
            let guard = extension.lock().unwrap_or_else(|p| p.into_inner());
            for (name, flag) in &guard.flags {
                if !all_flags.contains_key(name) {
                    all_flags.insert(name.clone(), flag.clone());
                }
            }
        }
        all_flags
    }

    pub fn set_flag_value(&self, name: &str, value: Value) {
        self.runtime.flag_values_set(name, value);
    }

    pub fn get_flag_values(&self) -> indexmap::IndexMap<String, Value> {
        self.runtime.flag_values_snapshot()
    }
