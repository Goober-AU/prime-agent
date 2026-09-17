use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn connection(defer: bool) -> DaemonAgentConnection {
    let client = crate::modes::daemon::daemon_client::DaemonClient::create("unused-backlog-test-socket");
    let transport = Arc::new(crate::main_entry::MainEntryDaemonTransport::new(client));
    DaemonAgentConnection::new(transport, "active".into(), DaemonAgentConnectionOptions { defer_session_events: defer, ..Default::default() })
}
fn message(text: &str, timestamp: i64) -> AgentMessage {
    serde_json::from_value(json!({"role":"user", "content":text, "timestamp":timestamp})).unwrap()
}
fn snapshot(sequence: i64) -> DaemonSessionSnapshot {
    parse_session_snapshot(&json!({
        "summary":{"sessionId":"saved", "activeSessionId":"active"},
        "state":{"sessionId":"saved", "activeSessionId":"active"},
        "messages":[], "lastEventSequence":sequence,
        "lastEventCursor":{"generation":"generation", "sequence":sequence}
    })).unwrap()
}
fn update(sequence: i64, text: &str) -> DaemonOutbound {
    DaemonOutbound::SessionEvent {
        active_session_id: "active".into(),
        event: AgentConnectionSessionEvent::MessageEnd { message: message(text, sequence) },
        meta: Some(DaemonEventMeta { sequence: Some(sequence), cursor: Some(DaemonEventCursor { generation: "generation".into(), sequence }) }),
    }
}
async fn begin(connection: &DaemonAgentConnection, id: &str, sequence: i64, purpose: &str) {
    connection.handle_daemon_message(DaemonOutbound::SessionSnapshotBegin {
        active_session_id: "active".into(), snapshot_id: id.into(), snapshot: snapshot(sequence), message_count: 1, purpose: Some(purpose.into()),
    }).await.unwrap();
    connection.handle_daemon_message(DaemonOutbound::SessionSnapshotChunk {
        active_session_id: "active".into(), snapshot_id: id.into(), index: 0, messages: vec![message(id, sequence)],
    }).await.unwrap();
}
async fn end(connection: &DaemonAgentConnection, id: &str, sequence: i64) {
    connection.handle_daemon_message(DaemonOutbound::SessionSnapshotEnd {
        active_session_id: "active".into(), snapshot_id: id.into(), chunk_count: 1, last_event_sequence: sequence,
        last_event_cursor: Some(DaemonEventCursor { generation: "generation".into(), sequence }),
    }).await.unwrap();
}

#[tokio::test]
async fn backlog_snapshot_replays_updates_after_captured_cursor_exactly_once() {
    let connection = connection(false);
    begin(&connection, "captured", 5, "resync").await;
    connection.handle_daemon_message(update(5, "already represented")).await.unwrap();
    connection.handle_daemon_message(update(6, "new update")).await.unwrap();
    end(&connection, "captured", 5).await;
    let cached = connection.get_initial_snapshot_inner(false).await.unwrap();
    assert_eq!(cached.messages, vec![message("captured", 5), message("new update", 6)]);
    assert_eq!(cached.last_event_sequence, Some(6.0));
    assert!(connection.deferred_session_events.lock().unwrap().queue.is_empty());
}

#[tokio::test]
async fn backlog_newer_snapshot_supersedes_late_old_completion() {
    let connection = connection(false);
    begin(&connection, "old", 5, "resync").await;
    begin(&connection, "new", 8, "resync").await;
    end(&connection, "new", 8).await;
    end(&connection, "old", 5).await;
    begin(&connection, "late", 6, "resync").await;
    end(&connection, "late", 6).await;
    let cached = connection.latest_snapshot.lock().unwrap().clone().unwrap();
    assert_eq!(cached.messages, vec![message("new", 8)]);
    assert_eq!(cached.last_event_sequence, Some(8.0));
    assert!(connection.wait_for_snapshot("old").await.is_err());
}

#[tokio::test]
async fn backlog_snapshot_rejects_new_cursor_on_older_content_and_disposed_frames() {
    let connection = connection(false);
    begin(&connection, "mismatched", 2, "attach").await;
    end(&connection, "mismatched", 3).await;
    assert!(connection.wait_for_snapshot("mismatched").await.is_err());
    assert!(connection.latest_snapshot.lock().unwrap().is_none());
    *connection.disposed.lock().unwrap() = true;
    begin(&connection, "after-dispose", 4, "attach").await;
    end(&connection, "after-dispose", 4).await;
    assert!(connection.wait_for_snapshot("after-dispose").await.is_err());
    assert!(connection.snapshot_assemblies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn backlog_replacement_discards_old_history_and_ignores_other_sessions() {
    let connection = connection(false);
    let mut initial = snapshot(5);
    initial.messages = vec![message("old history", 5)];
    connection.apply_replacement_snapshot(&initial, None);
    connection.handle_daemon_message(DaemonOutbound::SessionSnapshotBegin {
        active_session_id: "another-chat".into(), snapshot_id: "wrong-chat".into(), snapshot: snapshot(9), message_count: 1, purpose: Some("replacement".into()),
    }).await.unwrap();
    begin(&connection, "native-checkpoint", 8, "replacement").await;
    end(&connection, "native-checkpoint", 8).await;
    assert_eq!(connection.latest_snapshot.lock().unwrap().as_ref().unwrap().messages, vec![message("native-checkpoint", 8)]);
    assert!(!connection.snapshot_assemblies.lock().unwrap().contains_key("wrong-chat"));
}

#[tokio::test]
async fn backlog_interactive_attach_defers_updates_until_listener_without_refetch() {
    let connection = connection(true);
    let mut initial = snapshot(4);
    initial.messages = vec![message("initial", 4)];
    connection.apply_replacement_snapshot(&initial, None);
    connection.handle_daemon_message(update(5, "after attach")).await.unwrap();
    assert_eq!(connection.get_initial_snapshot_inner(false).await.unwrap().messages.len(), 1);
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let _unsubscribe = connection.subscribe(Arc::new(move |event| {
        let _ = sent.send(event); Box::pin(async {})
    }));
    tokio::time::timeout(Duration::from_secs(1), received.recv()).await.unwrap().unwrap();
    let current = connection.get_initial_snapshot_inner(false).await.unwrap();
    assert_eq!(current.messages, vec![message("initial", 4), message("after attach", 5)]);
    assert_eq!(current.last_event_sequence, Some(5.0));
}

#[tokio::test]
async fn backlog_opening_overflow_before_subscription_is_a_persistent_visible_error() {
    let connection = connection(true);
    connection.apply_replacement_snapshot(&snapshot(4), None);
    connection.handle_daemon_message(update(5, &"x".repeat(8 * 1024 * 1024))).await.unwrap();
    let error = connection.get_initial_snapshot_inner(false).await.unwrap_err();
    assert!(error.contains("bounded update buffer"));
    assert!(connection.wait_for_snapshot("not-yet-complete").await.unwrap_err().contains("bounded update buffer"));
    assert!(connection.get_state().await.unwrap_err().contains("bounded update buffer"));
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let unsubscribe = connection.subscribe(Arc::new(move |event| {
        let _ = sent.send(event); Box::pin(async {})
    }));
    let event = tokio::time::timeout(Duration::from_secs(1), received.recv()).await.unwrap().unwrap();
    assert!(matches!(event, AgentConnectionEvent::Closed { error: Some(error) } if error.contains("bounded update buffer")));
    connection.handle_daemon_message(update(6, "must not skip the missing event")).await.unwrap();
    assert_eq!(*connection.last_event_sequence.lock().unwrap(), Some(4));
    assert!(connection.deferred_session_events.lock().unwrap().queue.is_empty());
    assert!(!*connection.disposed.lock().unwrap()); // No session stop/abort.
    unsubscribe();
}

#[tokio::test]
async fn backlog_metadata_and_paged_message_end_invalidate_incomplete_caches() {
    let connection = connection(false);
    for event in [
        AgentConnectionSessionEvent::ThinkingLevelChanged { level: serde_json::from_value(json!("high")).unwrap() },
        AgentConnectionSessionEvent::AutoRetryEnd { success: true, attempt: 1.0, final_error: None },
        AgentConnectionSessionEvent::RetryUpdate { active: false, attempt: 1, max_attempts: 3, message: None },
    ] {
        connection.apply_replacement_snapshot(&snapshot(4), None);
        connection.handle_daemon_message(DaemonOutbound::SessionEvent { active_session_id: "active".into(), event, meta: None }).await.unwrap();
        assert!(!*connection.latest_snapshot_is_fresh.lock().unwrap());
    }
    connection.apply_replacement_snapshot(&snapshot(4), None);
    connection.latest_snapshot.lock().unwrap().as_mut().unwrap().history = Some(AgentConnectionHistoryWindow {
        entry_ids: vec![], total_message_count: 0.0, ..Default::default()
    });
    connection.handle_daemon_message(update(5, "a new final message has no wire entry id")).await.unwrap();
    assert!(!*connection.latest_snapshot_is_fresh.lock().unwrap());
    // Do not invent an identity or serve a mismatched recent-first window.
    assert!(connection.latest_snapshot.lock().unwrap().as_ref().unwrap().history.as_ref().unwrap().entry_ids.is_empty());
}

struct PausedTransport {
    connects: AtomicUsize,
    entered: tokio::sync::Semaphore,
}
impl PausedTransport {
    fn new() -> Arc<Self> { Arc::new(Self { connects: AtomicUsize::new(0), entered: tokio::sync::Semaphore::new(0) }) }
}
impl DaemonTransportClient for PausedTransport {
    fn request(&self, _: Value, _: Option<u64>) -> BoxFuture<Result<DaemonResponse, String>> { Box::pin(async { Ok(DaemonResponse::ok(json!({}))) }) }
    fn on_message(&self, _: Arc<dyn Fn(DaemonOutbound) + Send + Sync>) -> Box<dyn Fn() + Send + Sync> { Box::new(|| {}) }
    fn on_close(&self, _: Arc<dyn Fn(String) + Send + Sync>) -> Box<dyn Fn() + Send + Sync> { Box::new(|| {}) }
    fn supports_server_capability(&self, _: &str) -> bool { false }
    fn hello_socket_path(&self) -> Option<String> { None }
    fn is_connected(&self) -> bool { false }
    fn enable_request_recovery(&self) {}
    fn close(&self) {}
    fn connect(&self, _: u64) -> BoxFuture<Result<(), String>> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        self.entered.add_permits(1);
        // The test uses a shared transport so the gate outlives this future.
        Box::pin(async { Ok(()) })
    }
    fn wait_for_hello(&self, _: u64) -> BoxFuture<Result<(), String>> { Box::pin(async { Ok(()) }) }
    fn reconnect(&self, _: u64) -> BoxFuture<Result<(), String>> { Box::pin(async { Ok(()) }) }
    fn disconnect_for_reconnect(&self, _: &str) {}
    fn reset_transport_for_reconnect(&self) {}
    fn has_direct_transport(&self) -> bool { true }
    fn control_plane_transport(self: Arc<Self>) -> Arc<dyn DaemonTransportClient> { self }
}

#[tokio::test]
async fn backlog_reconnect_joiners_share_owner_and_cancel_result() {
    let transport = PausedTransport::new();
    let connection = Arc::new(DaemonAgentConnection::new(transport.clone(), "active".into(), Default::default()));
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let listener_gate = gate.clone();
    let _listener = connection.subscribe(Arc::new(move |event| {
        let _ = sent.send(event);
        let gate = listener_gate.clone();
        Box::pin(async move { let permit = gate.acquire().await.unwrap(); permit.forget(); })
    }));
    let owner = { let connection = connection.clone(); tokio::spawn(async move { connection.reconnect("first".into()).await }) };
    received.recv().await.unwrap();
    let joiner = { let connection = connection.clone(); tokio::spawn(async move { connection.reconnect("second".into()).await }) };
    let attempt = connection.reconnect_in_flight.lock().unwrap().clone().unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while attempt.result.receiver_count() == 0 { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert_eq!(transport.connects.load(Ordering::SeqCst), 0);
    attempt.cancel.cancel();
    assert!(tokio::time::timeout(Duration::from_secs(1), owner).await.unwrap().unwrap().is_err());
    assert!(tokio::time::timeout(Duration::from_secs(1), joiner).await.unwrap().unwrap().is_err());
    assert!(connection.reconnect_in_flight.lock().unwrap().is_none());
    _listener();
    gate.add_permits(10);
    connection.reconnect("later failure".into()).await.unwrap();
    assert_eq!(transport.connects.load(Ordering::SeqCst), 1);
}
