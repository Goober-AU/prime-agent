//! T12 — Telegram end-to-end with local fake transport (validation suite, owner: telegram).
//!
//! Findings under test: H-06 (worker env sanitization), H-07 (Unicode chunking, outbox
//! progress), H-10 (poll cancellation and error identity), H-11 (management-lock renewal
//! and ownership). Regression controls: pairing/steer round trip, native worker launch.
//!
//! Isolation (V00, `work/CONVENTIONS.md`): every case writes only under
//! `work/state-roots/telegram/<case>/` (given by `PARITY_TELEGRAM_STATE_ROOT`), uses a
//! synthetic bot token, a private pipe name and never touches a production pipe, port,
//! profile or service. No live Telegram traffic: the fake transport is in-process.
//!
//! TEST-ONLY FIXTURE: the H-06 case spawns `src/bin/telegram_worker_probe.rs`, a
//! validation-only launcher that dumps its environment and then continues into the real
//! native worker entry. Nothing in `src/` references that binary.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use pi_ai::types::BoxFuture;
use pi_coding_agent::modes::agent_connection::daemon_agent_connection::{
    DaemonAgentConnection, DaemonAgentConnectionOptions, DaemonOutbound, DaemonResponse, DaemonTransportClient,
};
use pi_coding_agent::modes::agent_connection::types::{AgentConnectionQueueState, AgentConnectionState};
use pi_coding_agent::modes::telegram::api::{
    private_message, split_telegram_text, TelegramApi, TelegramFetcher, TelegramHttpResponse, TelegramUpdate,
};
use pi_coding_agent::modes::telegram::bridge::TelegramBridge;
use pi_coding_agent::modes::telegram::manager::{
    start_telegram_worker, TelegramFileLock, TelegramWorkerLaunch, TELEGRAM_WORKER_LOCK_OPTIONS,
    with_telegram_management,
};
use pi_coding_agent::modes::telegram::store::{create_pairing, TelegramConnectionSettings, TelegramStore};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// V00 isolation helpers
// ---------------------------------------------------------------------------

/// `work/state-roots/telegram/<case>` — mandatory; never the default temp dir.
fn case_root(case: &str) -> std::path::PathBuf {
    let base = std::env::var("PARITY_TELEGRAM_STATE_ROOT")
        .expect("PARITY_TELEGRAM_STATE_ROOT must point at work/state-roots/telegram (V00)");
    let path = std::path::Path::new(&base).join(case);
    if path.exists() {
        std::fs::remove_dir_all(&path).expect("clean the private case root");
    }
    std::fs::create_dir_all(&path).expect("create the private case root");
    path
}

fn assert_private(root: &std::path::Path) {
    let private = std::env::var("PARITY_TELEGRAM_STATE_ROOT").unwrap();
    let private = std::path::Path::new(&private).canonicalize().unwrap();
    let canonical = root.canonicalize().unwrap();
    assert!(
        canonical.starts_with(&private),
        "V00: writable state escaped the private root: {}",
        canonical.display()
    );
    assert!(
        !canonical.to_string_lossy().contains(".prime"),
        "V00: no private state may live under .prime"
    );
}

const SYNTHETIC_TOKEN: &str = "123456789:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn synthetic_settings(agent_dir: &str, cwd: &str) -> TelegramConnectionSettings {
    TelegramConnectionSettings {
        version: 1.0,
        enabled: true,
        bot_token: SYNTHETIC_TOKEN.to_string(),
        bot_id: 7.0,
        bot_username: "parity_test_bot".to_string(),
        daemon_socket: format!(
            "\\\\.\\pipe\\optimus-parity-validation-20260915-t12-{}-baseline",
            agent_dir.trim_matches(|c| c == '/' || c == '\\').replace(['/', '\\'], "-")
        ),
        cwd: cwd.to_string(),
        session_id: "t12-session".to_string(),
        session_file: None,
        paired_user_id: Some(42.0),
        pairing: None,
    }
}

fn write_settings(store: &TelegramStore, settings: &TelegramConnectionSettings) {
    store
        .write("connection.json", &serde_json::to_value(settings).unwrap())
        .expect("write synthetic connection settings");
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

// ---------------------------------------------------------------------------
// H-07 — chunking contract (reference: api.ts:51-64, `chunk.length` is UTF-16)
// ---------------------------------------------------------------------------

fn reference_split(text: &str, limit: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    for character in text.chars() {
        if utf16_len(&chunk) + utf16_len(&character.to_string()) > limit {
            chunks.push(std::mem::take(&mut chunk));
        }
        chunk.push(character);
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

const TELEGRAM_SERVER_LIMIT: usize = 4096;
const SEND_LIMIT: usize = 4000;

fn assert_chunks_match_reference(label: &str, text: &str) {
    let produced = split_telegram_text(text, SEND_LIMIT).unwrap();
    let reference = reference_split(text, SEND_LIMIT);
    assert_eq!(
        produced.len(),
        reference.len(),
        "{label}: chunk count differs from the reference contract ({} vs {})",
        produced.len(),
        reference.len()
    );
    assert_eq!(
        produced.iter().map(|c| utf16_len(c)).collect::<Vec<_>>(),
        reference.iter().map(|c| utf16_len(c)).collect::<Vec<_>>(),
        "{label}: chunk boundaries differ from the reference contract (UTF-16 unit lengths per chunk)"
    );
    for chunk in &produced {
        assert!(
            utf16_len(chunk) <= SEND_LIMIT,
            "{label}: chunk exceeds the 4000 UTF-16 unit limit ({} units)",
            utf16_len(chunk)
        );
    }
    assert_eq!(produced.concat(), text, "{label}: concatenation must preserve the input");
}

#[test]
fn unicode_chunks_match_the_reference_contract() {
    assert_chunks_match_reference("ascii", &"abcdefghij".repeat(900));
    // BMP, 1 UTF-16 unit but 2 UTF-8 bytes: the mixed unit in the port doubles the work.
    assert_chunks_match_reference("cyrillic", &"телеграм".repeat(900));
    assert_chunks_match_reference("e-acute", &"é".repeat(8000));
    // Supplementary plane: 2 UTF-16 units, 4 UTF-8 bytes.
    assert_chunks_match_reference("emoji", &"\u{1F600}".repeat(5000));
    assert_chunks_match_reference("combining-newlines", &"e\u{0301}line\n".repeat(600));
    assert_chunks_match_reference("boundary", &format!("{}{}", "a".repeat(SEND_LIMIT - 1), "😀".repeat(3)));
}

#[test]
fn emitted_chunks_stay_within_the_telegram_server_limit() {
    for (label, text) in [
        ("emoji", "\u{1F600}".repeat(5000)),
        ("cyrillic", "телеграм".repeat(900)),
        ("mixed", "😀телеграм\n".repeat(500)),
    ] {
        let chunks = split_telegram_text(&text, SEND_LIMIT).unwrap();
        let worst = chunks.iter().map(|chunk| utf16_len(chunk)).max().unwrap_or(0);
        assert!(
            worst <= TELEGRAM_SERVER_LIMIT,
            "{label}: a produced chunk holds {worst} UTF-16 units, above Telegram's {TELEGRAM_SERVER_LIMIT} limit"
        );
    }
}

/// The `state.json` length validation must use the same unit as the reference.
#[test]
fn outbox_length_validation_matches_the_reference_unit() {
    let root = case_root("h07_outbox_length");
    assert_private(&root);
    let store = TelegramStore::new(root.to_str().unwrap());
    std::fs::create_dir_all(&store.directory).unwrap();
    // 3000 emoji = 6000 UTF-16 units: the reference rejects this record (length > 4000).
    let record = json!({
        "botId": 7,
        "offset": 0,
        "outbox": [{"id": "x", "chatId": 42, "text": "\u{1F600}".repeat(3000)}],
        "inbox": [],
    });
    store.write("state.json", &record).unwrap();
    let reference_rejects = utf16_len(record["outbox"][0]["text"].as_str().unwrap()) > 4000;
    let rust_accepts = store.state(7.0).is_ok();
    assert_eq!(
        reference_rejects, !rust_accepts,
        "outbox length validation disagrees with the reference unit (UTF-16 code units)"
    );
}

// ---------------------------------------------------------------------------
// Fake transports
// ---------------------------------------------------------------------------

/// Serves `sendMessage` and enforces the Telegram limit in UTF-16 units.
struct LimitedServer {
    limit: usize,
    sent: Arc<Mutex<Vec<String>>>,
}

impl LimitedServer {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self { limit, sent: Arc::new(Mutex::new(Vec::new())) })
    }
}

impl TelegramFetcher for LimitedServer {
    fn fetch(&self, url: String, body: String, _timeout_ms: u64) -> BoxFuture<Result<TelegramHttpResponse, String>> {
        let method = url.rsplit('/').next().unwrap_or_default().to_string();
        let limit = self.limit;
        let sent = self.sent.clone();
        Box::pin(async move {
            assert_eq!(method, "sendMessage", "the fake endpoint only serves sendMessage");
            let payload: Value = serde_json::from_str(&body).unwrap();
            let text = payload["text"].as_str().unwrap().to_string();
            if utf16_len(&text) > limit {
                return Ok(TelegramHttpResponse {
                    ok: false,
                    status: 400.0,
                    body: serde_json::to_vec(&json!({
                        "ok": false,
                        "error_code": 400,
                        "description": "Bad Request: message is too long",
                    }))
                    .unwrap(),
                });
            }
            sent.lock().unwrap().push(text);
            Ok(TelegramHttpResponse {
                ok: true,
                status: 200.0,
                body: serde_json::to_vec(&json!({"ok": true, "result": {"message_id": 1}})).unwrap(),
            })
        })
    }
}

/// Answers `setMyCommands`/`sendChatAction`; the long poll only ends after 30s.
struct HangingPollServer {
    polls_started: Arc<AtomicUsize>,
    polls_finished: Arc<AtomicUsize>,
}

impl TelegramFetcher for HangingPollServer {
    fn fetch(&self, url: String, _body: String, _t: u64) -> BoxFuture<Result<TelegramHttpResponse, String>> {
        let method = url.rsplit('/').next().unwrap_or_default().to_string();
        let started = self.polls_started.clone();
        let finished = self.polls_finished.clone();
        Box::pin(async move {
            if method == "getUpdates" {
                started.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(30)).await;
                finished.fetch_add(1, Ordering::SeqCst);
                return Ok(TelegramHttpResponse {
                    ok: true,
                    status: 200.0,
                    body: br#"{"ok":true,"result":[]}"#.to_vec(),
                });
            }
            Ok(TelegramHttpResponse {
                ok: true,
                status: 200.0,
                body: br#"{"ok":true,"result":true}"#.to_vec(),
            })
        })
    }
}

struct ErrorFetcher(&'static str);

impl TelegramFetcher for ErrorFetcher {
    fn fetch(&self, _: String, _: String, _: u64) -> BoxFuture<Result<TelegramHttpResponse, String>> {
        let message = self.0;
        Box::pin(async move { Err(message.to_string()) })
    }
}

struct StatusFetcher(f64, &'static str);

impl TelegramFetcher for StatusFetcher {
    fn fetch(&self, _: String, _: String, _: u64) -> BoxFuture<Result<TelegramHttpResponse, String>> {
        let body = serde_json::to_vec(&json!({
            "ok": false,
            "error_code": self.0,
            "description": self.1,
        }))
        .unwrap();
        let status = self.0;
        Box::pin(async move { Ok(TelegramHttpResponse { ok: false, status, body }) })
    }
}

struct GarbageFetcher;

impl TelegramFetcher for GarbageFetcher {
    fn fetch(&self, _: String, _: String, _: u64) -> BoxFuture<Result<TelegramHttpResponse, String>> {
        Box::pin(async move { Ok(TelegramHttpResponse { ok: true, status: 200.0, body: b"not json".to_vec() }) })
    }
}

fn api_for(fetcher: Arc<dyn TelegramFetcher>) -> TelegramApi {
    TelegramApi::new(SYNTHETIC_TOKEN, "https://fake.invalid", fetcher).unwrap()
}

// ---------------------------------------------------------------------------
// Shared in-process daemon transport
// ---------------------------------------------------------------------------

struct IdleTransport {
    state: AgentConnectionState,
    requests: Mutex<Vec<Value>>,
}

impl DaemonTransportClient for IdleTransport {
    fn request(&self, command: Value, _: Option<u64>) -> BoxFuture<Result<DaemonResponse, String>> {
        let data = match command["type"].as_str().unwrap() {
            "get_connection_state" => serde_json::to_value(&self.state).unwrap(),
            "abort_and_clear_queue" => serde_json::to_value(AgentConnectionQueueState::default()).unwrap(),
            "prompt" => Value::Null,
            other => panic!("unexpected daemon request: {other}"),
        };
        self.requests.lock().unwrap().push(command);
        Box::pin(async move { Ok(DaemonResponse::ok(data)) })
    }
    fn on_message(&self, _: Arc<dyn Fn(DaemonOutbound) + Send + Sync>) -> Box<dyn Fn() + Send + Sync> {
        Box::new(|| {})
    }
    fn on_close(&self, _: Arc<dyn Fn(String) + Send + Sync>) -> Box<dyn Fn() + Send + Sync> {
        Box::new(|| {})
    }
    fn supports_server_capability(&self, _: &str) -> bool {
        false
    }
    fn hello_socket_path(&self) -> Option<String> {
        None
    }
    fn is_connected(&self) -> bool {
        true
    }
    fn enable_request_recovery(&self) {}
    fn close(&self) {}
    fn connect(&self, _: u64) -> BoxFuture<Result<(), String>> {
        panic!("no socket connections in this test")
    }
    fn wait_for_hello(&self, _: u64) -> BoxFuture<Result<(), String>> {
        panic!("no socket connections in this test")
    }
    fn reconnect(&self, _: u64) -> BoxFuture<Result<(), String>> {
        panic!("no socket connections in this test")
    }
    fn disconnect_for_reconnect(&self, _: &str) {}
    fn reset_transport_for_reconnect(&self) {}
    fn control_plane_transport(self: Arc<Self>) -> Arc<dyn DaemonTransportClient> {
        self
    }
}

fn bridge_with(
    root: &std::path::Path,
    fetcher: Arc<dyn TelegramFetcher>,
    streaming: bool,
) -> (Arc<TelegramBridge>, TelegramStore, Arc<IdleTransport>) {
    let store = TelegramStore::new(root.to_str().unwrap());
    let settings = synthetic_settings(root.to_str().unwrap(), root.to_str().unwrap());
    write_settings(&store, &settings);
    let api = Arc::new(TelegramApi::new(&settings.bot_token, "https://fake.invalid", fetcher).unwrap());
    let transport = Arc::new(IdleTransport {
        state: AgentConnectionState {
            session_id: "t12-session".into(),
            cwd: root.to_string_lossy().into_owned(),
            is_streaming: streaming,
            ..Default::default()
        },
        requests: Mutex::new(Vec::new()),
    });
    let connection = Arc::new(DaemonAgentConnection::new(
        transport.clone(),
        "t12-session".into(),
        DaemonAgentConnectionOptions::default(),
    ));
    let bridge = Arc::new(
        TelegramBridge::new(
            TelegramStore::new(root.to_str().unwrap()),
            settings,
            api,
            connection,
            Arc::new(|_| {}),
        )
        .unwrap(),
    );
    (bridge, store, transport)
}

/// H-07 reproduction: supplementary-plane text must not block the outbox head.
#[tokio::test]
async fn unicode_chunks_and_outbox_progress() {
    let root = case_root("h07_outbox_progress");
    assert_private(&root);
    let server = LimitedServer::new(TELEGRAM_SERVER_LIMIT);
    let (bridge, store, _) = bridge_with(&root, server.clone(), false);

    let payload = "\u{1F600}".repeat(3000); // 6000 UTF-16 units
    bridge.enqueue_reply(&payload).unwrap();
    let flush = tokio::time::timeout(Duration::from_secs(20), bridge.flush_replies(false)).await;
    assert!(flush.is_ok(), "flush_replies must not hang on an oversized chunk");
    let flush_failed = flush.unwrap().is_err();

    // Primary observable: the outbox must make progress. An oversized chunk that the
    // server rejects leaves the head in place and blocks every later reply.
    let state = store.state(7.0).unwrap();
    let delivered = server.sent.lock().unwrap().clone();
    assert!(
        state.outbox.is_empty(),
        "the outbox head must be delivered and removed; {} item(s) remain (server accepted {} chunk(s), flush reported an error: {flush_failed})",
        state.outbox.len(),
        delivered.len()
    );
    assert_eq!(
        delivered.concat(),
        payload,
        "the delivered text must equal the queued text exactly once"
    );
    for text in delivered.iter() {
        assert!(utf16_len(text) <= TELEGRAM_SERVER_LIMIT);
    }
}

// ---------------------------------------------------------------------------
// H-10 — cancellation and error identity
// ---------------------------------------------------------------------------

/// H-10 reproduction: cancelling must end the in-flight long poll promptly.
///
/// Await the bridge in place while a watcher cancels its in-flight request,
/// mirroring the worker's top-level run loop.
#[tokio::test]
async fn cancel_poll_stops_the_in_flight_request() {
    let root = case_root("h10_cancel_poll");
    assert_private(&root);
    let polls_started = Arc::new(AtomicUsize::new(0));
    let polls_finished = Arc::new(AtomicUsize::new(0));
    let server = Arc::new(HangingPollServer {
        polls_started: polls_started.clone(),
        polls_finished: polls_finished.clone(),
    });
    let (bridge, _store, _) = bridge_with(&root, server, false);

    let token = CancellationToken::new();
    let watcher = {
        let token = token.clone();
        let started = polls_started.clone();
        tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_secs(5);
            while started.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            token.cancel();
        })
    };

    let started_at = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(10), bridge.run(Some(token.clone()))).await;
    watcher.await.unwrap();
    assert_eq!(polls_started.load(Ordering::SeqCst), 1, "the fake long poll must be in flight");
    assert!(
        outcome.is_ok(),
        "cancelling the bridge must end the in-flight poll; it was still pending after {:?}",
        started_at.elapsed()
    );
    assert_eq!(
        polls_finished.load(Ordering::SeqCst),
        0,
        "the in-flight request must be abandoned, not completed after the caller cancelled"
    );
}

/// H-10 reproduction: transport errors keep their own identity on the signalled path.
#[tokio::test]
async fn transport_errors_keep_their_own_identity() {
    let network = api_for(Arc::new(ErrorFetcher("connection reset")));
    assert_eq!(
        network.identify(false).await.unwrap_err().to_string(),
        "Cannot reach Telegram. Check this computer's internet connection.",
        "a network failure must stay a connectivity error"
    );

    // The worker always signals its requests; a genuine network failure must not be
    // relabelled as an abort.
    let signalled = api_for(Arc::new(ErrorFetcher("dns failure")));
    assert_eq!(
        signalled.identify(true).await.unwrap_err().to_string(),
        "Cannot reach Telegram. Check this computer's internet connection.",
        "a signalled request must not turn a network error into an abort"
    );

    let api_error = api_for(Arc::new(StatusFetcher(429.0, "Too Many Requests")));
    assert_eq!(
        api_error.send(42.0, "hi", false).await.unwrap_err().to_string(),
        "Telegram request failed (429).",
        "an HTTP/API failure must stay an API error"
    );

    let garbage = api_for(Arc::new(GarbageFetcher));
    assert_eq!(
        garbage.identify(false).await.unwrap_err().to_string(),
        "Telegram returned an invalid response."
    );
}

// ---------------------------------------------------------------------------
// H-11 — management-lock renewal and ownership
// ---------------------------------------------------------------------------

fn lock_age_ms(lock_path: &str) -> u128 {
    let modified = std::fs::metadata(lock_path).unwrap().modified().unwrap();
    SystemTime::now()
        .duration_since(modified)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0)
}

// The management action is held past the 10s stale threshold on purpose.
const STALE_HOLD_MS: u64 = 13_000;

/// H-11 reproduction: `with_telegram_management` must keep its lock fresh.
#[tokio::test]
async fn management_lock_renews_without_callback() {
    let root = case_root("h11_management_lock");
    assert_private(&root);
    let store = TelegramStore::new(root.to_str().unwrap());
    write_settings(&store, &synthetic_settings(root.to_str().unwrap(), root.to_str().unwrap()));
    let target = store.path("management");
    let lock_path = format!("{target}.lock");

    let result = with_telegram_management(&store, {
        let target = target.clone();
        let lock_path = lock_path.clone();
        move || {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(STALE_HOLD_MS)).await;
                let age = lock_age_ms(&lock_path);
                assert!(
                    age < TELEGRAM_WORKER_LOCK_OPTIONS.stale as u128,
                    "the management lock was {age}ms old after {STALE_HOLD_MS}ms of work: a heartbeat \
                     must refresh it below the {}ms stale threshold",
                    TELEGRAM_WORKER_LOCK_OPTIONS.stale
                );
                // The contender must not be able to steal a live lock.
                let contender = TelegramFileLock::acquire(&target, TELEGRAM_WORKER_LOCK_OPTIONS, None, None).await;
                assert!(
                    contender.is_err(),
                    "a live management lock must not be stealable by a contender"
                );
                Ok(())
            })
        }
    })
    .await;
    result.expect("management action must run");
    assert!(!std::path::Path::new(&lock_path).exists(), "release must remove the management lock");
}

/// H-11 reproduction, ownership half: a holder whose lock was stolen must notice the
/// compromise and must not delete the new holder's lock directory on release.
///
/// This mirrors `proper-lockfile`: `setLockAsCompromised` deletes the in-process registry
/// entry, so a later `release()` fails with `ENOTACQUIRED` instead of removing the path
/// (`node_modules/proper-lockfile/lib/lockfile.js:185-201,276-293`).
#[tokio::test]
async fn compromised_management_lock_is_forgotten_and_not_deleted_on_release() {
    let root = case_root("h11_management_compromise");
    assert_private(&root);
    let store = TelegramStore::new(root.to_str().unwrap());
    write_settings(&store, &synthetic_settings(root.to_str().unwrap(), root.to_str().unwrap()));
    let target = store.path("management");
    let lock_path = format!("{target}.lock");

    let compromised = Arc::new(AtomicUsize::new(0));
    let flag = compromised.clone();
    let lock = TelegramFileLock::acquire(
        &target,
        TELEGRAM_WORKER_LOCK_OPTIONS,
        None,
        Some(Arc::new(move || {
            flag.fetch_add(1, Ordering::SeqCst);
        })),
    )
    .await
    .unwrap();

    // Another process takes the lock over.
    std::fs::remove_dir_all(&lock_path).unwrap();
    std::fs::create_dir_all(&lock_path).unwrap();
    std::fs::write(format!("{lock_path}/owner.txt"), "second-holder").unwrap();

    // The heartbeat must notice within a few update periods.
    let deadline = Instant::now() + Duration::from_secs(8);
    while compromised.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        compromised.load(Ordering::SeqCst) > 0,
        "the heartbeat must detect that the lock directory is no longer the one it acquired"
    );

    lock.release();
    assert!(
        std::path::Path::new(&lock_path).exists(),
        "release must not delete a lock directory the holder no longer owns"
    );
    let _ = std::fs::remove_dir_all(&lock_path);
}

/// Control: the worker lock already passes a compromised callback.
#[tokio::test]
async fn worker_lock_renews_and_survives_the_stale_threshold() {
    let root = case_root("h11_worker_lock");
    assert_private(&root);
    let store = TelegramStore::new(root.to_str().unwrap());
    std::fs::create_dir_all(&store.directory).unwrap();
    let target = store.path("worker");
    let compromised = Arc::new(AtomicUsize::new(0));
    let flag = compromised.clone();
    let lock = TelegramFileLock::acquire(
        &target,
        TELEGRAM_WORKER_LOCK_OPTIONS,
        None,
        Some(Arc::new(move || {
            flag.fetch_add(1, Ordering::SeqCst);
        })),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(STALE_HOLD_MS)).await;
    let age = lock_age_ms(&format!("{target}.lock"));
    assert!(
        age < TELEGRAM_WORKER_LOCK_OPTIONS.stale as u128,
        "the worker lock was {age}ms old: its heartbeat must refresh it"
    );
    assert_eq!(compromised.load(Ordering::SeqCst), 0, "a live worker lock must not be compromised");
    lock.release();
}

/// Control: release stops its own heartbeat and removes the lock it holds.
#[tokio::test]
async fn release_removes_the_lock_it_holds() {
    let root = case_root("h11_release_removes_own_lock");
    assert_private(&root);
    let target = root.join("worker").to_string_lossy().to_string();
    let lock_path = format!("{target}.lock");
    let lock = TelegramFileLock::acquire(&target, TELEGRAM_WORKER_LOCK_OPTIONS, None, None)
        .await
        .unwrap();
    assert!(std::path::Path::new(&lock_path).exists());
    lock.release();
    assert!(!std::path::Path::new(&lock_path).exists(), "release must remove the lock it holds");
}

// ---------------------------------------------------------------------------
// H-06 — native worker environment sanitization
// ---------------------------------------------------------------------------

const PROHIBITED_PREFIXES: [&str; 2] = ["PRIME_AGENT_INTERNAL_", "PRIME_AGENT_SESSION_LEASE"];
const PROHIBITED_NAMES: [&str; 2] = ["PRIME_AGENT_ORPHAN_PROCESS_JOURNAL", "PRIME_AGENT_INTERACTIVE_SELF_UPDATE"];

fn probe_binary_path() -> String {
    let exe = std::env::current_exe().unwrap();
    let mut dir = exe.parent().unwrap().to_path_buf();
    if dir.ends_with("deps") {
        dir = dir.parent().unwrap().to_path_buf();
    }
    let name = if cfg!(windows) { "telegram_worker_probe.exe" } else { "telegram_worker_probe" };
    dir.join(name).to_string_lossy().to_string()
}

/// H-06 reproduction: the child must not inherit the parent's internal role/token/lease
/// variables, and it must run the real native worker entry.
#[tokio::test]
async fn native_worker_environment_is_sanitized() {
    let root = case_root("h06_worker_env");
    assert_private(&root);
    let agent_dir = root.join("agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let store = TelegramStore::new(agent_dir.to_str().unwrap());
    write_settings(&store, &synthetic_settings(agent_dir.to_str().unwrap(), agent_dir.to_str().unwrap()));

    let probe = probe_binary_path();
    assert!(
        std::path::Path::new(&probe).exists(),
        "the probe launcher must be built first (cargo build --bin telegram_worker_probe): {probe}"
    );

    // The parent carries exactly the variables the reference deletes before spawning.
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    for (name, value) in [
        ("PRIME_AGENT_INTERNAL_DAEMON_WORKER_ROLE", "daemon-worker"),
        ("PRIME_AGENT_INTERNAL_DAEMON_WORKER_TOKEN", "synthetic-token"),
        ("PRIME_AGENT_SESSION_LEASE_OWNER", "synthetic-owner"),
        ("PRIME_AGENT_ORPHAN_PROCESS_JOURNAL", "1"),
        ("PRIME_AGENT_INTERACTIVE_SELF_UPDATE", "1"),
        ("PARITY_TELEGRAM_SENTINEL_KEEP", "kept"),
    ] {
        env.retain(|(key, _)| key != name);
        env.push((name.to_string(), value.to_string()));
    }
    let dump = agent_dir.join("child_env.json");
    env.retain(|(key, _)| key != "PARITY_TELEGRAM_ENV_DUMP");
    env.push(("PARITY_TELEGRAM_ENV_DUMP".to_string(), dump.to_string_lossy().to_string()));
    env.retain(|(key, _)| key != "PARITY_TELEGRAM_DISABLE_SETTINGS");
    env.push(("PARITY_TELEGRAM_DISABLE_SETTINGS".to_string(), "1".to_string()));

    let launch = TelegramWorkerLaunch {
        is_bun_binary: false,
        from_source: false,
        package_dir: root.to_string_lossy().to_string(),
        exec_path: probe.clone(),
        exec_argv: Vec::new(),
        env,
    };

    // The spawn path under test is the production one; its startup result is not the
    // assertion here (the child disables the connection and exits quickly).
    let _ = tokio::time::timeout(Duration::from_secs(30), start_telegram_worker(&store, &launch)).await;

    let deadline = Instant::now() + Duration::from_secs(20);
    while !dump.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(dump.exists(), "the native worker child must have written its environment dump");
    let dumped: Value = serde_json::from_str(&std::fs::read_to_string(&dump).unwrap()).unwrap();
    let child_pid = dumped["__probe_pid"].as_u64().expect("the dump records the child pid");
    assert!(
        dumped["__probe_argv"].as_str().unwrap_or_default().contains(&agent_dir.to_string_lossy().to_string()),
        "the child must be invoked as the native worker with the agent directory"
    );

    let mut inherited = Vec::new();
    for key in dumped.as_object().unwrap().keys() {
        if PROHIBITED_PREFIXES.iter().any(|prefix| key.starts_with(prefix)) || PROHIBITED_NAMES.contains(&key.as_str())
        {
            inherited.push(key.clone());
        }
    }
    assert!(
        inherited.is_empty(),
        "the Telegram worker child inherited prohibited variables: {inherited:?}"
    );
    assert_eq!(
        dumped["PARITY_TELEGRAM_SENTINEL_KEEP"].as_str(),
        Some("kept"),
        "only the prohibited names may be removed; ordinary variables must survive"
    );
    assert!(
        !dumped
            .as_object()
            .unwrap()
            .keys()
            .any(|key| key.to_ascii_uppercase().contains("TOKEN")),
        "no token-named variable must reach the child (or the dump) unredacted"
    );

    // The real worker entry ran inside that child: it wrote its own status file.
    let mut saw_child_status = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(Some(status)) = store.status() {
            if status.pid == child_pid as f64
                && pi_coding_agent::modes::telegram::store::TELEGRAM_WORKER_PHASES.contains(&status.phase.as_str())
            {
                saw_child_status = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        saw_child_status,
        "the child must run the real native worker entry (worker.json written by pid {child_pid})"
    );
}

// ---------------------------------------------------------------------------
// Regression control — pairing + steer round trip
// ---------------------------------------------------------------------------

/// `bridge_with` variant for an unpaired store, so the pairing flow is under test.
fn bridge_unpaired(root: &std::path::Path, streaming: bool) -> (Arc<TelegramBridge>, TelegramStore, Arc<IdleTransport>, String) {
    let store = TelegramStore::new(root.to_str().unwrap());
    let (code, pairing) = create_pairing(
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
    );
    let mut settings = synthetic_settings(root.to_str().unwrap(), root.to_str().unwrap());
    settings.paired_user_id = None;
    settings.pairing = Some(pairing);
    write_settings(&store, &settings);
    let api = Arc::new(TelegramApi::new(SYNTHETIC_TOKEN, "https://fake.invalid", Arc::new(GarbageFetcher)).unwrap());
    let transport = Arc::new(IdleTransport {
        state: AgentConnectionState {
            session_id: "t12-session".into(),
            cwd: root.to_string_lossy().into_owned(),
            is_streaming: streaming,
            ..Default::default()
        },
        requests: Mutex::new(Vec::new()),
    });
    let connection = Arc::new(DaemonAgentConnection::new(
        transport.clone(),
        "t12-session".into(),
        DaemonAgentConnectionOptions::default(),
    ));
    let bridge = Arc::new(
        TelegramBridge::new(
            TelegramStore::new(root.to_str().unwrap()),
            settings,
            api,
            connection,
            Arc::new(|_| {}),
        )
        .unwrap(),
    );
    (bridge, store, transport, code)
}

fn update_for(id: f64, sender: f64, text: &str) -> TelegramUpdate {
    TelegramUpdate {
        update_id: id,
        message: Some(json!({
            "message_id": id + 1.0,
            "date": 1000,
            "from": {"id": sender, "is_bot": false},
            "chat": {"id": sender, "type": "private"},
            "text": text,
        })),
    }
}

/// Regression control for the repaired pairing and steer behaviour.
#[tokio::test]
async fn pairing_and_steer_roundtrip() {
    let root = case_root("control_pairing_steer");
    assert_private(&root);
    let (bridge, store, transport, code) = bridge_unpaired(&root, true);

    // Unauthorized sender: ignored, no daemon traffic.
    bridge.accept(&update_for(1.0, 99.0, "hello")).await.unwrap();
    // Wrong pairing code: rejected.
    bridge.accept(&update_for(2.0, 42.0, "/start wrong-code")).await.unwrap();
    assert!(transport.requests.lock().unwrap().is_empty(), "an unpaired sender must not reach the session");
    assert_eq!(store.settings().unwrap().unwrap().paired_user_id, None);

    // Correct pairing code: paired and acknowledged.
    bridge.accept(&update_for(3.0, 42.0, &format!("/start {code}"))).await.unwrap();
    assert_eq!(store.settings().unwrap().unwrap().paired_user_id, Some(42.0));
    assert!(
        store.state(7.0).unwrap().outbox.iter().any(|reply| reply.text.starts_with("Connected to Prime")),
        "a successful pairing must queue the acknowledgement"
    );

    // Ordinary text now steers exactly once.
    bridge.accept(&update_for(4.0, 42.0, "Use the updated requirement")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), bridge.wait_for_dispatch()).await.unwrap();
    let requests = transport.requests.lock().unwrap();
    let prompts: Vec<&Value> = requests.iter().filter(|request| request["type"] == "prompt").collect();
    assert_eq!(prompts.len(), 1, "exactly one prompt per message");
    assert_eq!(prompts[0]["message"], "Use the updated requirement");
    assert_eq!(prompts[0]["streamingBehavior"], "steer");
    assert_eq!(prompts[0]["queueIfBusy"], true);
    assert_eq!(prompts[0]["source"], "interactive");
}

#[test]
fn private_message_filter_keeps_its_contract() {
    let good = json!({
        "update_id": 3,
        "message": {"message_id": 1, "date": 1000, "from": {"id": 7, "is_bot": false},
                    "chat": {"id": 7, "type": "private"}, "text": "hi"},
    });
    assert!(private_message(&good).is_some());
    let mut group = good.clone();
    group["message"]["chat"]["type"] = json!("group");
    assert!(private_message(&group).is_none());
    let mut bot = good.clone();
    bot["message"]["from"]["is_bot"] = json!(true);
    assert!(private_message(&bot).is_none());
    let mut oversized = good.clone();
    oversized["message"]["text"] = json!("😀".repeat(9000)); // 18000 UTF-16 units
    assert!(
        private_message(&oversized).is_none(),
        "a text above 16384 UTF-16 units must be rejected"
    );
    assert!(private_message(&json!({"update_id": 3})).is_none(), "an update without a message is ignored");
}

#[test]
fn synthetic_settings_never_target_a_live_endpoint() {
    let settings = synthetic_settings("C:/private/agent", "C:/private/cwd");
    assert!(settings.daemon_socket.contains("optimus-parity-validation-20260915"));
    assert!(!settings.daemon_socket.contains("prime-agent-daemon"));
    assert!(!settings.daemon_socket.contains("optimus-rust-test-20260915"));
    assert_eq!(settings.bot_id, 7.0);
}
