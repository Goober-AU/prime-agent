use std::sync::{Arc, Mutex};

use pi_ai::types::BoxFuture;
use pi_coding_agent::modes::agent_connection::daemon_agent_connection::{
    DaemonAgentConnection, DaemonAgentConnectionOptions, DaemonOutbound, DaemonResponse,
    DaemonTransportClient,
};
use pi_coding_agent::modes::agent_connection::types::{AgentConnectionQueueState, AgentConnectionState};
use pi_coding_agent::modes::telegram::{
    api::{TelegramApi, TelegramFetcher, TelegramHttpResponse, TelegramUpdate},
    bridge::TelegramBridge,
    commands::telegram_help,
    store::{create_pairing, TelegramConnectionSettings, TelegramStore},
};
use serde_json::{json, Value};

struct RecordingTransport {
    state: AgentConnectionState,
    requests: Mutex<Vec<Value>>,
}

impl DaemonTransportClient for RecordingTransport {
    fn request(&self, command: Value, _: Option<u64>) -> BoxFuture<Result<DaemonResponse, String>> {
        let data = match command["type"].as_str().unwrap() {
            "get_connection_state" => serde_json::to_value(&self.state).unwrap(),
            "abort_and_clear_queue" => serde_json::to_value(AgentConnectionQueueState::default()).unwrap(),
            "prompt" | "abort_compaction" | "abort_retry" | "abort_bash" => Value::Null,
            other => panic!("Unexpected daemon request: {other}"),
        };
        self.requests.lock().unwrap().push(command);
        Box::pin(async move { Ok(DaemonResponse::ok(data)) })
    }
    fn on_message(&self, _: Arc<dyn Fn(DaemonOutbound) + Send + Sync>) -> Box<dyn Fn() + Send + Sync> { Box::new(|| {}) }
    fn on_close(&self, _: Arc<dyn Fn(String) + Send + Sync>) -> Box<dyn Fn() + Send + Sync> { Box::new(|| {}) }
    fn supports_server_capability(&self, _: &str) -> bool { false }
    fn hello_socket_path(&self) -> Option<String> { None }
    fn is_connected(&self) -> bool { true }
    fn enable_request_recovery(&self) {}
    fn close(&self) {}
    fn connect(&self, _: u64) -> BoxFuture<Result<(), String>> { panic!("No socket connections in this test") }
    fn wait_for_hello(&self, _: u64) -> BoxFuture<Result<(), String>> { panic!("No socket connections in this test") }
    fn reconnect(&self, _: u64) -> BoxFuture<Result<(), String>> { panic!("No socket connections in this test") }
    fn disconnect_for_reconnect(&self, _: &str) {}
    fn reset_transport_for_reconnect(&self) {}
    fn control_plane_transport(self: Arc<Self>) -> Arc<dyn DaemonTransportClient> { self }
}

struct NoNetwork;
impl TelegramFetcher for NoNetwork {
    fn fetch(&self, _: String, _: String, _: u64) -> BoxFuture<Result<TelegramHttpResponse, String>> {
        panic!("No Telegram requests in this test")
    }
}

struct Harness {
    _directory: tempfile::TempDir,
    store: TelegramStore,
    bridge: Arc<TelegramBridge>,
    transport: Arc<RecordingTransport>,
    pairing_code: String,
}

impl Harness {
    fn new(streaming: bool, paired: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = TelegramStore::new(directory.path().to_str().unwrap());
        let transport = Arc::new(RecordingTransport {
            state: AgentConnectionState {
                session_id: "telegram-test".into(),
                cwd: directory.path().to_string_lossy().into_owned(),
                is_streaming: streaming,
                ..Default::default()
            },
            requests: Mutex::new(Vec::new()),
        });
        let (pairing_code, pairing) = create_pairing(
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64,
        );
        let settings = TelegramConnectionSettings {
            version: 1.0, enabled: true, bot_token: "123456789:AAAAAAAAAAAAAAAAAAAA".into(),
            bot_id: 7.0, bot_username: "optimus_test_bot".into(), daemon_socket: "unused".into(),
            cwd: transport.state.cwd.clone(), session_id: transport.state.session_id.clone(),
            session_file: None, paired_user_id: paired.then_some(42.0),
            pairing: (!paired).then_some(pairing),
        };
        let api = Arc::new(TelegramApi::new(&settings.bot_token, "https://unused.invalid", Arc::new(NoNetwork)).unwrap());
        let connection = Arc::new(DaemonAgentConnection::new(
            transport.clone(), "telegram-test".into(), DaemonAgentConnectionOptions::default(),
        ));
        let bridge = Arc::new(TelegramBridge::new(
            TelegramStore::new(directory.path().to_str().unwrap()), settings, api, connection,
            Arc::new(|error| assert!(error.is_none(), "Bridge error: {error:?}")),
        ).unwrap());
        Self { _directory: directory, store, bridge, transport, pairing_code }
    }

    async fn accept(&self, id: f64, sender: u64, text: &str) {
        self.bridge.accept(&TelegramUpdate {
            update_id: id,
            message: Some(json!({ "message_id": id + 1.0, "date": 1000,
                "from": {"id": sender, "is_bot": false},
                "chat": {"id": sender, "type": "private"}, "text": text })),
        }).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), self.bridge.wait_for_dispatch()).await.unwrap();
    }
}

#[tokio::test]
async fn plain_text_uses_the_steering_prompt_contract_for_busy_and_idle_sessions() {
    for streaming in [true, false] {
        let harness = Harness::new(streaming, true);
        harness.accept(1.0, 42, "Use the updated requirement").await;
        let requests = harness.transport.requests.lock().unwrap();
        let prompt = requests.iter().find(|request| request["type"] == "prompt").unwrap();
        assert_eq!(prompt["message"], "Use the updated requirement");
        assert_eq!(prompt["streamingBehavior"], "steer");
        assert_eq!(prompt["queueIfBusy"], true);
        assert_eq!(prompt["source"], "interactive");
        assert_eq!(prompt["activeSessionId"], "telegram-test");
        let state = harness.store.state(7.0).unwrap();
        assert!(state.inbox.is_empty());
        assert!(state.interrupted_update.is_none());
    }
}

#[tokio::test]
async fn commands_still_use_command_handlers_and_unpaired_senders_cannot_prompt() {
    let harness = Harness::new(true, true);
    harness.accept(1.0, 99, "Unauthorized text").await;
    harness.accept(2.0, 42, "/help@another_bot").await;
    assert!(harness.transport.requests.lock().unwrap().is_empty());
    harness.accept(3.0, 42, "/help").await;
    assert!(harness.store.state(7.0).unwrap().outbox.iter().any(|reply| reply.text == telegram_help()));
    harness.accept(4.0, 42, "/stop").await;
    let requests = harness.transport.requests.lock().unwrap();
    assert!(requests.iter().all(|request| request["type"] != "prompt"));
    assert!(requests.iter().any(|request| request["type"] == "abort_and_clear_queue"));
}

#[tokio::test]
async fn pairing_is_required_before_plain_text_can_steer() {
    let harness = Harness::new(false, false);
    harness.accept(1.0, 42, "Text before pairing").await;
    harness.accept(2.0, 42, "/start wrong-code").await;
    assert!(harness.transport.requests.lock().unwrap().is_empty());
    harness.accept(3.0, 42, &format!("/start {}", harness.pairing_code)).await;
    assert_eq!(harness.store.settings().unwrap().unwrap().paired_user_id, Some(42.0));
    harness.accept(4.0, 42, "Text after pairing").await;
    assert_eq!(harness.transport.requests.lock().unwrap().iter().filter(|request| request["type"] == "prompt").count(), 1);
}
