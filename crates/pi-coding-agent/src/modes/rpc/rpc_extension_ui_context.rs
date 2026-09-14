//! Port of packages/coding-agent/src/modes/rpc/rpc-extension-ui-context.ts

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::sync::oneshot;

use crate::core::extensions::types::{
    AutocompleteProviderFactory, CustomComponentResult, EditorFactory, ExtensionUIDialogOptions,
    ExtensionUiContext, ExtensionWidgetOptions, FooterFactory, HeaderFactory, SetThemeResult,
    TerminalInputHandler, Theme, ThemeInfo, WidgetPlacement, WorkingIndicatorOptions,
};
use crate::modes::rpc::rpc_types::{RpcExtensionUiRequest, RpcExtensionUiResponse};

/// `RpcExtensionUiBridge`.
pub struct RpcExtensionUiBridge {
    state: Arc<RpcExtensionUiState>,
    ui_context: Arc<dyn ExtensionUiContext>,
}

/// `createRpcExtensionUiBridge(output)`.
pub fn create_rpc_extension_ui_bridge(
    output: Arc<dyn Fn(RpcExtensionUiRequest) + Send + Sync>,
) -> RpcExtensionUiBridge {
    let state = Arc::new(RpcExtensionUiState {
        output,
        pending: Mutex::new(HashMap::new()),
        closed: AtomicBool::new(false),
    });
    RpcExtensionUiBridge {
        ui_context: Arc::new(RpcExtensionUiContext {
            state: state.clone(),
        }),
        state,
    }
}

impl RpcExtensionUiBridge {
    pub fn ui_context(&self) -> Arc<dyn ExtensionUiContext> {
        self.ui_context.clone()
    }

    /// `handleResponse(response)`.
    pub fn handle_response(&self, response: RpcExtensionUiResponse) -> bool {
        let sender = self
            .state
            .pending
            .lock()
            .expect("pending dialogs poisoned")
            .remove(response.id());
        match sender {
            Some(sender) => {
                let _ = sender.send(response);
                true
            }
            None => false,
        }
    }

    /// `close()`.
    pub fn close(&self) {
        self.state.closed.store(true, Ordering::SeqCst);
        let mut pending = self.state.pending.lock().expect("pending dialogs poisoned");
        let drained: Vec<(String, oneshot::Sender<RpcExtensionUiResponse>)> = pending.drain().collect();
        drop(pending);
        for (id, sender) in drained {
            let _ = sender.send(RpcExtensionUiResponse::Cancelled {
                type_: "extension_ui_response".to_string(),
                id,
                cancelled: true,
            });
        }
    }
}

struct RpcExtensionUiState {
    output: Arc<dyn Fn(RpcExtensionUiRequest) + Send + Sync>,
    pending: Mutex<HashMap<String, oneshot::Sender<RpcExtensionUiResponse>>>,
    closed: AtomicBool,
}

impl RpcExtensionUiState {
    fn emit(&self, id: &str, method: &str, payload: Map<String, Value>) {
        let mut request = Map::new();
        request.insert(
            "type".to_string(),
            Value::String("extension_ui_request".to_string()),
        );
        request.insert("id".to_string(), Value::String(id.to_string()));
        request.insert("method".to_string(), Value::String(method.to_string()));
        for (key, value) in &payload {
            request.insert(key.clone(), value.clone());
        }
        (self.output)(RpcExtensionUiRequest {
            type_: "extension_ui_request".to_string(),
            id: id.to_string(),
            method: method.to_string(),
            payload: Value::Object(payload),
        });
    }

    fn fire_and_forget(&self, method: &str, payload: Map<String, Value>) {
        self.emit(&uuid::Uuid::new_v4().to_string(), method, payload);
    }

    /// `createDialogPromise(opts, defaultValue, request, parseResponse)`.
    fn create_dialog_promise<T: Send + 'static>(
        self: &Arc<Self>,
        opts: Option<&ExtensionUIDialogOptions>,
        default_value: T,
        method: &str,
        payload: Map<String, Value>,
        parse_response: impl FnOnce(RpcExtensionUiResponse) -> T + Send + 'static,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>> {
        let signal = opts.and_then(|opts| opts.signal.clone());
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().expect("pending dialogs poisoned");
            if self.closed.load(Ordering::SeqCst)
                || signal.as_ref().is_some_and(|signal| signal.is_cancelled())
            {
                return Box::pin(async move { default_value });
            }
            pending.insert(id.clone(), sender);
        }
        let deadline = opts.and_then(|opts| opts.timeout)
            .filter(|timeout| *timeout != 0.0 && !timeout.is_nan())
            .map(|timeout| {
                let millis = if !timeout.is_finite() || timeout < 1.0 || timeout > i32::MAX as f64 { 1 } else { timeout as u64 };
                tokio::time::Instant::now() + Duration::from_millis(millis)
            });
        self.emit(&id, method, payload);
        let cleanup = PendingDialogCleanup { state: self.clone(), id };
        Box::pin(async move {
            let _cleanup = cleanup;
            let cancelled = async move {
                match signal {
                    Some(signal) => signal.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            };
            let timed_out = async move {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                response = receiver => match response {
                    Ok(response) => parse_response(response),
                    Err(_) => default_value,
                },
                _ = cancelled => default_value,
                _ = timed_out => default_value,
            }
        })
    }

}

struct PendingDialogCleanup {
    state: Arc<RpcExtensionUiState>,
    id: String,
}

impl Drop for PendingDialogCleanup {
    fn drop(&mut self) {
        self.state.pending.lock().expect("pending dialogs poisoned").remove(&self.id);
    }
}

/// The object literal `uiContext` in `createRpcExtensionUiBridge`.
struct RpcExtensionUiContext {
    state: Arc<RpcExtensionUiState>,
}

impl RpcExtensionUiContext {
    fn parse_value(response: RpcExtensionUiResponse) -> Option<String> {
        match response {
            RpcExtensionUiResponse::Cancelled { cancelled, .. } if cancelled => None,
            RpcExtensionUiResponse::Value { value, .. } => Some(value),
            _ => None,
        }
    }
}

impl ExtensionUiContext for RpcExtensionUiContext {
    fn select(
        &self,
        title: String,
        options: Vec<String>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
        let state = self.state.clone();
            let mut payload = Map::new();
            payload.insert("title".to_string(), Value::String(title));
            payload.insert(
                "options".to_string(),
                Value::Array(options.into_iter().map(Value::String).collect()),
            );
            if let Some(timeout) = opts.as_ref().and_then(|opts| opts.timeout) {
                payload.insert("timeout".to_string(), json_number(timeout));
            }
            state
                .create_dialog_promise(opts.as_ref(), None, "select", payload, RpcExtensionUiContext::parse_value)
    }

    fn confirm(
        &self,
        title: String,
        message: String,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> {
        let state = self.state.clone();
            let mut payload = Map::new();
            payload.insert("title".to_string(), Value::String(title));
            payload.insert("message".to_string(), Value::String(message));
            if let Some(timeout) = opts.as_ref().and_then(|opts| opts.timeout) {
                payload.insert("timeout".to_string(), json_number(timeout));
            }
            state
                .create_dialog_promise(opts.as_ref(), false, "confirm", payload, |response| match response {
                    RpcExtensionUiResponse::Cancelled { cancelled, .. } => {
                        if cancelled {
                            false
                        } else {
                            false
                        }
                    }
                    RpcExtensionUiResponse::Confirmed { confirmed, .. } => confirmed,
                    RpcExtensionUiResponse::Value { .. } => false,
                })
    }

    fn input(
        &self,
        title: String,
        placeholder: Option<String>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
        let state = self.state.clone();
            let mut payload = Map::new();
            payload.insert("title".to_string(), Value::String(title));
            if let Some(placeholder) = placeholder {
                payload.insert("placeholder".to_string(), Value::String(placeholder));
            }
            if let Some(timeout) = opts.as_ref().and_then(|opts| opts.timeout) {
                payload.insert("timeout".to_string(), json_number(timeout));
            }
            state
                .create_dialog_promise(opts.as_ref(), None, "input", payload, RpcExtensionUiContext::parse_value)
    }

    fn notify(&self, message: String, kind: Option<String>) {
        let mut payload = Map::new();
        payload.insert("message".to_string(), Value::String(message));
        if let Some(kind) = kind {
            payload.insert("notifyType".to_string(), Value::String(kind));
        }
        self.state.fire_and_forget("notify", payload);
    }

    fn on_terminal_input(&self, _handler: TerminalInputHandler) -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    fn set_status(&self, key: String, text: Option<String>) {
        let mut payload = Map::new();
        payload.insert("statusKey".to_string(), Value::String(key));
        payload.insert(
            "statusText".to_string(),
            match text {
                Some(text) => Value::String(text),
                None => Value::Null,
            },
        );
        self.state.fire_and_forget("setStatus", payload);
    }

    fn set_working_message(&self, _message: Option<String>) {}

    fn set_working_visible(&self, _visible: bool) {}

    fn set_working_indicator(&self, _options: Option<WorkingIndicatorOptions>) {}

    fn set_hidden_thinking_label(&self, _label: Option<String>) {}

    fn set_widget_strings(
        &self,
        key: String,
        content: Option<Vec<String>>,
        options: Option<ExtensionWidgetOptions>,
    ) {
        let mut payload = Map::new();
        payload.insert("widgetKey".to_string(), Value::String(key));
        payload.insert(
            "widgetLines".to_string(),
            match content {
                Some(lines) => Value::Array(lines.into_iter().map(Value::String).collect()),
                None => Value::Null,
            },
        );
        if let Some(placement) = options.and_then(|options| options.placement) {
            payload.insert(
                "widgetPlacement".to_string(),
                Value::String(widget_placement_name(placement).to_string()),
            );
        }
        self.state.fire_and_forget("setWidget", payload);
    }

    fn set_widget_factory(
        &self,
        _key: String,
        _content: Option<crate::core::extensions::types::WidgetFactory>,
        _options: Option<ExtensionWidgetOptions>,
    ) {
    }

    fn set_footer(&self, _factory: Option<FooterFactory>) {}

    fn set_header(&self, _factory: Option<HeaderFactory>) {}

    fn set_title(&self, title: String) {
        let mut payload = Map::new();
        payload.insert("title".to_string(), Value::String(title));
        self.state.fire_and_forget("setTitle", payload);
    }

    fn custom(&self, _factory: crate::core::extensions::types::CustomComponentFactory, _options: Option<Value>) -> CustomComponentResult {
        Box::pin(async { None })
    }

    fn paste_to_editor(&self, text: String) {
        self.set_editor_text(text);
    }

    fn set_editor_text(&self, text: String) {
        let mut payload = Map::new();
        payload.insert("text".to_string(), Value::String(text));
        self.state.fire_and_forget("set_editor_text", payload);
    }

    fn get_editor_text(&self) -> String {
        String::new()
    }

    fn editor(
        &self,
        title: String,
        prefill: Option<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
        let state = self.state.clone();
            let mut payload = Map::new();
            payload.insert("title".to_string(), Value::String(title));
            if let Some(prefill) = prefill {
                payload.insert("prefill".to_string(), Value::String(prefill));
            }
            state
                .create_dialog_promise(None, None, "editor", payload, RpcExtensionUiContext::parse_value)
    }

    fn add_autocomplete_provider(&self, _factory: AutocompleteProviderFactory) {}

    fn set_editor_component(&self, _factory: Option<EditorFactory>) {}

    fn get_editor_component(&self) -> Option<EditorFactory> {
        None
    }

    fn theme(&self) -> Theme {
        // blocked_on: the real theme lives in modes/interactive/theme/theme.ts;
        // core/extensions/types.rs currently declares a structural stand-in.
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
            error: Some("Theme switching not supported in RPC mode".to_string()),
        }
    }

    fn get_tools_expanded(&self) -> bool {
        false
    }

    fn set_tools_expanded(&self, _expanded: bool) {}
}

fn widget_placement_name(placement: WidgetPlacement) -> &'static str {
    match placement {
        WidgetPlacement::AboveEditor => "aboveEditor",
        WidgetPlacement::BelowEditor => "belowEditor",
    }
}

/// `JSON.stringify`-style numbers: integral values stay integral.
fn json_number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        Value::from(value as i64)
    } else {
        Value::from(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge() -> (RpcExtensionUiBridge, Arc<Mutex<Vec<Value>>>) {
        let emitted = Arc::new(Mutex::new(Vec::new()));
        let sink = emitted.clone();
        let bridge = create_rpc_extension_ui_bridge(Arc::new(move |request: RpcExtensionUiRequest| {
            let mut value = Map::new();
            value.insert("method".to_string(), Value::String(request.method.clone()));
            value.insert("id".to_string(), Value::String(request.id.clone()));
            value.insert("payload".to_string(), request.payload.clone());
            sink.lock().unwrap().push(Value::Object(value));
        }));
        (bridge, emitted)
    }

    #[tokio::test]
    async fn notify_is_fire_and_forget() {
        let (bridge, emitted) = bridge();
        bridge.ui_context().notify("hello".to_string(), Some("info".to_string()));
        let emitted = emitted.lock().unwrap().clone();
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0]["method"], Value::String("notify".to_string()));
        assert_eq!(
            emitted[0]["payload"],
            serde_json::json!({"message": "hello", "notifyType": "info"})
        );
    }

    #[tokio::test]
    async fn a_dialog_resolves_from_its_response() {
        let (bridge, emitted) = bridge();
        let ui = bridge.ui_context();
        let dialog = ui.select("pick".to_string(), vec!["a".to_string()], None);
        let id = {
            let emitted = emitted.lock().unwrap();
            emitted[0]["id"].as_str().unwrap().to_string()
        };
        assert!(bridge.handle_response(RpcExtensionUiResponse::Value {
            type_: "extension_ui_response".to_string(),
            id,
            value: "a".to_string(),
        }));
        assert_eq!(dialog.await, Some("a".to_string()));
    }

    #[tokio::test]
    async fn an_unknown_response_is_not_handled() {
        let (bridge, _emitted) = bridge();
        assert!(!bridge.handle_response(RpcExtensionUiResponse::Value {
            type_: "extension_ui_response".to_string(),
            id: "missing".to_string(),
            value: "a".to_string(),
        }));
    }

    #[tokio::test]
    async fn close_cancels_pending_dialogs() {
        let (bridge, _emitted) = bridge();
        let ui = bridge.ui_context();
        let dialog = ui.confirm("sure?".to_string(), "body".to_string(), None);
        bridge.close();
        assert!(!dialog.await);
        assert!(!bridge.handle_response(RpcExtensionUiResponse::Cancelled {
            type_: "extension_ui_response".to_string(),
            id: "x".to_string(),
            cancelled: true,
        }));
    }

    #[tokio::test]
    async fn a_closed_bridge_returns_the_default_without_emitting() {
        let (bridge, emitted) = bridge();
        bridge.close();
        let ui = bridge.ui_context();
        assert_eq!(ui.input("t".to_string(), None, None).await, None);
        assert!(emitted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn set_theme_reports_unsupported() {
        let (bridge, _emitted) = bridge();
        let result = bridge.ui_context().set_theme(Value::String("dark".to_string()));
        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("Theme switching not supported in RPC mode"));
    }

    #[tokio::test]
    async fn set_widget_emits_the_placement() {
        let (bridge, emitted) = bridge();
        bridge.ui_context().set_widget_strings(
            "w".to_string(),
            Some(vec!["line".to_string()]),
            Some(ExtensionWidgetOptions {
                placement: Some(WidgetPlacement::BelowEditor),
            }),
        );
        let emitted = emitted.lock().unwrap().clone();
        assert_eq!(emitted[0]["method"], Value::String("setWidget".to_string()));
        assert_eq!(
            emitted[0]["payload"],
            serde_json::json!({"widgetKey": "w", "widgetLines": ["line"], "widgetPlacement": "belowEditor"})
        );
    }

    #[tokio::test]
    async fn editor_requests_carry_their_prefill() {
        let (bridge, emitted) = bridge();
        let ui = bridge.ui_context();
        let dialog = ui.editor("edit".to_string(), Some("seed".to_string()));
        {
            let emitted = emitted.lock().unwrap();
            assert_eq!(emitted[0]["method"], Value::String("editor".to_string()));
            assert_eq!(emitted[0]["payload"], serde_json::json!({"title": "edit", "prefill": "seed"}));
        }
        let id = emitted.lock().unwrap()[0]["id"].as_str().unwrap().to_string();
        bridge.handle_response(RpcExtensionUiResponse::Cancelled {
            type_: "extension_ui_response".to_string(),
            id,
            cancelled: true,
        });
        assert_eq!(dialog.await, None);
    }

    #[test]
    fn integral_timeouts_serialize_without_a_decimal_point() {
        assert_eq!(json_number(5000.0), serde_json::json!(5000));
        assert_eq!(json_number(1.5), serde_json::json!(1.5));
    }
}
