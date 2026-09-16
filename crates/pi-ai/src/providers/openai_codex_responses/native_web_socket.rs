//! Native transport for the Codex provider's existing cached WebSocket state machine.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use indexmap::IndexMap;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    http::header::{HeaderName, HeaderValue},
    protocol::{frame::coding::CloseCode, CloseFrame, Message},
};
use tokio_util::sync::CancellationToken;

use super::{WebSocketConstructor, WebSocketEventType, WebSocketLike, WebSocketListener};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Events {
    listeners: HashMap<WebSocketEventType, Vec<WebSocketListener>>,
    terminal: HashMap<WebSocketEventType, Value>,
}

struct SocketState {
    ready: AtomicI32,
    events: Mutex<Events>,
    close: CancellationToken,
    close_frame: Mutex<Option<CloseFrame>>,
}

impl SocketState {
    fn emit(&self, kind: WebSocketEventType, event: Value) {
        let listeners = {
            let mut events = self
                .events
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if kind != WebSocketEventType::Message {
                events.terminal.insert(kind, event.clone());
            }
            events.listeners.get(&kind).cloned().unwrap_or_default()
        };
        // Never invoke application callbacks while holding a transport lock.
        for listener in listeners {
            listener(event.clone());
        }
    }

    fn failed(&self, error: impl std::fmt::Display) {
        self.ready.store(3, Ordering::SeqCst);
        self.emit(
            WebSocketEventType::Error,
            json!({"message": error.to_string()}),
        );
        self.emit(
            WebSocketEventType::Close,
            json!({"code": 1006, "wasClean": false}),
        );
    }
}

struct NativeWebSocket {
    state: Arc<SocketState>,
    sends: mpsc::Sender<String>,
}

impl Drop for NativeWebSocket {
    fn drop(&mut self) {
        self.state.close.cancel();
    }
}

impl WebSocketLike for NativeWebSocket {
    fn close(&self, code: Option<i32>, reason: Option<&str>) {
        if self.state.ready.load(Ordering::SeqCst) == 3 {
            return;
        }
        self.state.ready.store(2, Ordering::SeqCst);
        *self
            .state
            .close_frame
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(CloseFrame {
            code: CloseCode::from(code.unwrap_or(1000) as u16),
            reason: reason.unwrap_or("done").to_string().into(),
        });
        self.state.close.cancel();
    }

    fn send(&self, data: &str) {
        if self.state.ready.load(Ordering::SeqCst) != 1 {
            self.state.failed("WebSocket is not open");
            return;
        }
        // A session sends one turn at a time. Bound accidental producer backlog.
        if self.sends.try_send(data.to_string()).is_err() {
            self.state.failed("WebSocket send queue is unavailable");
            self.state.close.cancel();
        }
    }

    fn ready_state(&self) -> Option<i32> {
        Some(self.state.ready.load(Ordering::SeqCst))
    }

    fn add_event_listener(&self, kind: WebSocketEventType, listener: WebSocketListener) {
        let already_fired = {
            let mut events = self
                .state
                .events
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            events
                .listeners
                .entry(kind)
                .or_default()
                .push(listener.clone());
            events.terminal.get(&kind).cloned()
        };
        // A native IO task may finish the handshake before its caller subscribes.
        if let Some(event) = already_fired {
            listener(event);
        }
    }

    fn remove_event_listener(&self, kind: WebSocketEventType, listener: &WebSocketListener) {
        let mut events = self
            .state
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(listeners) = events.listeners.get_mut(&kind) {
            listeners.retain(|registered| !Arc::ptr_eq(registered, listener));
        }
    }
}

pub(super) fn constructor() -> WebSocketConstructor {
    Arc::new(|url, headers| {
        let state = Arc::new(SocketState {
            ready: AtomicI32::new(0),
            events: Mutex::new(Events::default()),
            close: CancellationToken::new(),
            close_frame: Mutex::new(None),
        });
        let (sends, receiver) = mpsc::channel(2);
        let socket = Arc::new(NativeWebSocket {
            state: state.clone(),
            sends,
        });
        tokio::spawn(run_socket(url.to_string(), headers, state, receiver));
        socket
    })
}

async fn run_socket(
    url: String,
    headers: IndexMap<String, String>,
    state: Arc<SocketState>,
    mut sends: mpsc::Receiver<String>,
) {
    let request = (|| {
        let mut request = url
            .into_client_request()
            .map_err(|error| error.to_string())?;
        for (name, value) in headers {
            let name =
                HeaderName::from_bytes(name.as_bytes()).map_err(|error| error.to_string())?;
            let value = HeaderValue::from_str(&value).map_err(|error| error.to_string())?;
            request.headers_mut().insert(name, value);
        }
        Ok::<_, String>(request)
    })();
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            state.failed(error);
            return;
        }
    };
    let connected = tokio::select! {
        biased;
        _ = state.close.cancelled() => return,
        result = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request)) => result,
    };
    let (mut socket, _) = match connected {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => {
            state.failed(error);
            return;
        }
        Err(_) => {
            state.failed("WebSocket connection timed out");
            return;
        }
    };
    state.ready.store(1, Ordering::SeqCst);
    state.emit(WebSocketEventType::Open, json!({}));

    loop {
        tokio::select! {
            biased;
            _ = state.close.cancelled() => {
                let frame = state.close_frame.lock().unwrap_or_else(|error| error.into_inner()).clone();
                let _ = tokio::time::timeout(CLOSE_TIMEOUT, socket.close(frame)).await;
                break;
            }
            outgoing = sends.recv() => {
                let Some(outgoing) = outgoing else { break; };
                let result = tokio::select! {
                    biased;
                    _ = state.close.cancelled() => break,
                    result = tokio::time::timeout(WRITE_TIMEOUT, socket.send(Message::Text(outgoing.into()))) => result,
                };
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => { state.failed(error); return; }
                    Err(_) => { state.failed("WebSocket send timed out"); return; }
                }
            }
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => state.emit(WebSocketEventType::Message, json!({"data": text.as_str()})),
                    Some(Ok(Message::Binary(bytes))) => state.emit(WebSocketEventType::Message, json!({"data": String::from_utf8_lossy(&bytes)})),
                    Some(Ok(Message::Ping(_))) => {
                        // Tungstenite queues the matching pong; flush while otherwise idle.
                        let _ = tokio::time::timeout(CLOSE_TIMEOUT, socket.flush()).await;
                    }
                    Some(Ok(Message::Close(frame))) => {
                        state.ready.store(3, Ordering::SeqCst);
                        state.emit(WebSocketEventType::Close, json!({
                            "code": frame.as_ref().map(|frame| u16::from(frame.code)).unwrap_or(1000),
                            "reason": frame.map(|frame| frame.reason.to_string()).unwrap_or_default(),
                            "wasClean": true,
                        }));
                        let _ = tokio::time::timeout(CLOSE_TIMEOUT, socket.flush()).await;
                        return;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => { state.failed(error); return; }
                    None => { state.failed("WebSocket stream ended without a close frame"); return; }
                }
            }
        }
    }
    state.ready.store(3, Ordering::SeqCst);
    state.emit(
        WebSocketEventType::Close,
        json!({"code": 1000, "wasClean": true}),
    );
}
