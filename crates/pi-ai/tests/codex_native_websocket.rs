//! Local wire regressions: no credentials, external endpoints, or model tokens.
use std::time::Duration;

use base64::Engine;
use futures::{SinkExt, StreamExt};
use pi_ai::providers::openai_codex_responses::{
    close_openai_codex_web_socket_sessions, get_openai_codex_web_socket_debug_stats,
    stream_openai_codex_responses, OpenAICodexResponsesOptions, JWT_CLAIM_PATH,
    OPENAI_BETA_RESPONSES_WEBSOCKETS,
};
use pi_ai::types::{
    AssistantMessage, Context, Message, Model, StreamOptions, UserContent, UserMessage,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::{
    handshake::server::{Request, Response},
    Message as Frame,
};
use tokio_util::sync::CancellationToken;

type ServerSocket = tokio_tungstenite::WebSocketStream<TcpStream>;

fn fixture(port: u16, session: &str) -> (Model, Context, OpenAICodexResponsesOptions) {
    let model = Model::new(
        "gpt-6-astra",
        "Astra fixture",
        "openai-codex-responses",
        "openai-codex",
        format!("http://127.0.0.1:{port}"),
    );
    let context = Context::new(Some("Local fixture".to_string()), vec![user("first")], None);
    let token = base64::engine::general_purpose::STANDARD
        .encode(json!({JWT_CLAIM_PATH: {"chatgpt_account_id": "fixture"}}).to_string());
    let options = OpenAICodexResponsesOptions {
        stream: StreamOptions {
            api_key: Some(format!("test.{token}.local")),
            session_id: Some(session.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    (model, context, options)
}

fn user(text: &str) -> Message {
    Message::user(UserMessage::new(UserContent::Text(text.to_string()), 1))
}

fn events(id: &str) -> Vec<Value> {
    vec![
        json!({"type":"response.created","response":{"id": id}}),
        json!({"type":"response.output_item.added","item":{"type":"message","id":"msg_1","content":[]}}),
        json!({"type":"response.content_part.added","part":{"type":"output_text","text":""}}),
        json!({"type":"response.output_text.delta","delta":"Hello"}),
        json!({"type":"response.output_item.done","item":{"type":"message","id":"msg_1","phase":"final_answer","content":[{"type":"output_text","text":"Hello"}]}}),
        json!({"type":"response.completed","response":{"id":id,"status":"completed","usage":{"input_tokens":5,"output_tokens":1,"total_tokens":6}}}),
    ]
}

async fn request(socket: &mut ServerSocket) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(frame.to_text().unwrap()).unwrap()
}

async fn respond(socket: &mut ServerSocket, id: &str) {
    // Back-to-back frames deliberately challenge ordering and listener installation.
    for event in events(id) {
        socket
            .feed(Frame::Text(event.to_string().into()))
            .await
            .unwrap();
    }
    socket.flush().await.unwrap();
}

async fn answer(
    model: &Model,
    context: &Context,
    options: OpenAICodexResponsesOptions,
) -> AssistantMessage {
    tokio::time::timeout(
        Duration::from_secs(8),
        stream_openai_codex_responses(model, context, Some(options)).result(),
    )
    .await
    .expect("provider must settle")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_default_transport_reuses_connection_and_sends_only_followup_delta() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (model, mut context, options) =
        fixture(listener.local_addr().unwrap().port(), "native-delta");
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket =
            tokio_tungstenite::accept_hdr_async(tcp, |request: &Request, response: Response| {
                assert_eq!(request.uri().path(), "/codex/responses");
                assert_eq!(
                    request.headers()["openai-beta"],
                    OPENAI_BETA_RESPONSES_WEBSOCKETS
                );
                assert_eq!(request.headers()["chatgpt-account-id"], "fixture");
                assert_eq!(request.headers()["session_id"], "native-delta");
                assert!(request.headers()["authorization"]
                    .to_str()
                    .unwrap()
                    .starts_with("Bearer test."));
                Ok(response)
            })
            .await
            .unwrap();
        let first = request(&mut socket).await;
        assert_eq!(first["type"], "response.create");
        assert_eq!(first["store"], false);
        assert!(first.get("previous_response_id").is_none());
        respond(&mut socket, "resp_first").await;
        let next = request(&mut socket).await;
        assert_eq!(next["previous_response_id"], "resp_first");
        assert_eq!(next["input"].as_array().unwrap().len(), 1);
        assert_eq!(next["input"][0]["role"], "user");
        respond(&mut socket, "resp_second").await;
        let changed = request(&mut socket).await;
        assert!(changed.get("previous_response_id").is_none());
        assert!(changed["input"].as_array().unwrap().len() > 1);
        respond(&mut socket, "resp_changed").await;
        let close = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(close, Frame::Close(_)));
    });
    let first = answer(&model, &context, options.clone()).await;
    assert_eq!(first.stop_reason, "stop", "{:?}", first.error_message);
    assert_eq!(first.content[0].as_text().unwrap().text, "Hello");
    context
        .messages
        .extend([Message::assistant(first), user("second")]);
    let second = answer(&model, &context, options.clone()).await;
    assert_eq!(second.stop_reason, "stop");
    context
        .messages
        .extend([Message::assistant(second), user("third")]);
    context.system_prompt = Some("Changed instructions require full replay".to_string());
    assert_eq!(answer(&model, &context, options).await.stop_reason, "stop");
    let stats = get_openai_codex_web_socket_debug_stats("native-delta").unwrap();
    assert_eq!(
        (
            stats.connections_created,
            stats.connections_reused,
            stats.delta_requests,
            stats.full_context_requests
        ),
        (1, 2, 1, 2)
    );
    assert_eq!(stats.sse_fallbacks, 0);
    close_openai_codex_web_socket_sessions(Some("native-delta"));
    server.await.unwrap();
}

async fn read_http(socket: &mut TcpStream) -> String {
    let mut request = Vec::new();
    loop {
        let mut byte = [0];
        socket.read_exact(&mut byte).await.unwrap();
        request.push(byte[0]);
        if request.ends_with(b"\r\n\r\n") {
            break;
        }
        assert!(request.len() < 32_768);
    }
    String::from_utf8(request).unwrap()
}

#[tokio::test]
async fn native_upgrade_rejection_falls_back_to_sse_and_remembers_session_choice() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (model, context, options) =
        fixture(listener.local_addr().unwrap().port(), "native-fallback");
    let server = tokio::spawn(async move {
        for index in 0..4 {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let header = read_http(&mut tcp).await;
            if index < 2 {
                assert!(header.starts_with("GET "));
                tcp.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            } else {
                assert!(header.starts_with("POST "), "SSE fallback must use POST");
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|length| length.trim().parse().unwrap())
                    })
                    .unwrap();
                let mut body = vec![0; length];
                tcp.read_exact(&mut body).await.unwrap();
                assert!(serde_json::from_slice::<Value>(&body)
                    .unwrap()
                    .get("previous_response_id")
                    .is_none());
                let data = events("resp_sse")
                    .into_iter()
                    .map(|event| format!("data: {event}\n\n"))
                    .collect::<String>();
                tcp.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len()).as_bytes()).await.unwrap();
            }
        }
    });
    for _ in 0..2 {
        assert_eq!(
            answer(&model, &context, options.clone()).await.stop_reason,
            "stop"
        );
    }
    let stats = get_openai_codex_web_socket_debug_stats("native-fallback").unwrap();
    assert_eq!(stats.sse_fallbacks, 2);
    assert_eq!(stats.websocket_failures, 1);
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_response_event_or_invalid_json_never_replays_over_either_transport() {
    for (case, payload) in [
        (
            "partial",
            json!({"type":"response.created","response":{"id":"started"}}).to_string(),
        ),
        ("unknown", json!({"extension_event":true}).to_string()),
        ("invalid", "not valid json".to_string()),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let session = format!("native-no-replay-{case}");
        let (model, context, options) = fixture(listener.local_addr().unwrap().port(), &session);
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            request(&mut socket).await;
            socket.send(Frame::Text(payload.into())).await.unwrap();
            // No graceful close: emulate a 1006 network failure immediately after an event.
            drop(socket);
            assert!(
                tokio::time::timeout(Duration::from_millis(300), listener.accept())
                    .await
                    .is_err(),
                "partial work was replayed"
            );
        });
        let output = answer(&model, &context, options).await;
        assert_eq!(output.stop_reason, "error", "case {case}");
        assert_eq!(
            get_openai_codex_web_socket_debug_stats(&session)
                .unwrap()
                .sse_fallbacks,
            0
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn native_cancellation_closes_the_active_socket_without_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (model, context, mut options) = fixture(
        listener.local_addr().unwrap().port(),
        "native-cancel-active",
    );
    let signal = CancellationToken::new();
    options.stream.signal = Some(signal.clone());
    let (started, observed) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
        request(&mut socket).await;
        started.send(()).unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(frame, Frame::Close(_)));
    });
    let stream = stream_openai_codex_responses(&model, &context, Some(options));
    observed.await.unwrap();
    signal.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .stop_reason,
        "aborted"
    );
    server.await.unwrap();
    assert_eq!(
        get_openai_codex_web_socket_debug_stats("native-cancel-active")
            .unwrap()
            .sse_fallbacks,
        0
    );
}

#[tokio::test]
async fn native_cancellation_interrupts_an_unanswered_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (model, context, mut options) = fixture(
        listener.local_addr().unwrap().port(),
        "native-cancel-handshake",
    );
    let signal = CancellationToken::new();
    options.stream.signal = Some(signal.clone());
    let (started, observed) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http(&mut socket).await;
        started.send(()).unwrap();
        let mut byte = [0];
        let read = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut byte))
            .await
            .unwrap();
        assert!(
            matches!(read, Ok(0) | Err(_)),
            "cancel must close TCP during handshake"
        );
    });
    let stream = stream_openai_codex_responses(&model, &context, Some(options));
    observed.await.unwrap();
    signal.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), stream.result())
            .await
            .unwrap()
            .stop_reason,
        "aborted"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn native_stale_cached_connection_reconnects_with_full_context() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (model, mut context, options) =
        fixture(listener.local_addr().unwrap().port(), "native-reconnect");
    let (closed, observed) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
        request(&mut socket).await;
        respond(&mut socket, "old-response").await;
        drop(socket);
        closed.send(()).unwrap();
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let body = request(&mut socket).await;
        assert!(
            body.get("previous_response_id").is_none(),
            "continuation belongs to the dead connection"
        );
        assert!(body["input"].as_array().unwrap().len() > 1);
        respond(&mut socket, "fresh-response").await;
        let _ = tokio::time::timeout(Duration::from_secs(3), socket.next()).await;
    });
    let first = answer(&model, &context, options.clone()).await;
    assert_eq!(first.stop_reason, "stop");
    observed.await.unwrap();
    context
        .messages
        .extend([Message::assistant(first), user("second")]);
    assert_eq!(answer(&model, &context, options).await.stop_reason, "stop");
    close_openai_codex_web_socket_sessions(Some("native-reconnect"));
    server.await.unwrap();
}

#[tokio::test]
async fn native_changed_endpoint_does_not_reuse_another_connection_or_checkpoint() {
    let first_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_url = format!("http://{}", second_listener.local_addr().unwrap());
    let (mut model, mut context, options) = fixture(
        first_listener.local_addr().unwrap().port(),
        "native-endpoint-change",
    );
    let first_server = tokio::spawn(async move {
        let (tcp, _) = first_listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
        request(&mut socket).await;
        respond(&mut socket, "first-endpoint").await;
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Frame::Close(_)
        ));
    });
    let second_server = tokio::spawn(async move {
        let (tcp, _) = second_listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let body = request(&mut socket).await;
        assert!(body.get("previous_response_id").is_none());
        assert!(body["input"].as_array().unwrap().len() > 1);
        respond(&mut socket, "second-endpoint").await;
        let _ = tokio::time::timeout(Duration::from_secs(3), socket.next()).await;
    });
    let first = answer(&model, &context, options.clone()).await;
    assert_eq!(first.stop_reason, "stop");
    context
        .messages
        .extend([Message::assistant(first), user("new endpoint")]);
    model.base_url = second_url;
    assert_eq!(answer(&model, &context, options).await.stop_reason, "stop");
    close_openai_codex_web_socket_sessions(Some("native-endpoint-change"));
    first_server.await.unwrap();
    second_server.await.unwrap();
}

#[tokio::test]
async fn native_request_timeout_closes_socket_and_does_not_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (model, context, mut options) =
        fixture(listener.local_addr().unwrap().port(), "native-timeout");
    options.stream.timeout_ms = Some(500.0);
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
        request(&mut socket).await;
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Frame::Close(_)
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_err()
        );
    });
    let output = answer(&model, &context, options).await;
    assert_eq!(output.stop_reason, "error");
    assert_eq!(output.error_message.as_deref(), Some("Request timed out"));
    server.await.unwrap();
    assert_eq!(
        get_openai_codex_web_socket_debug_stats("native-timeout")
            .unwrap()
            .sse_fallbacks,
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_busy_cached_connection_is_not_shared_between_concurrent_turns() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (model, context, options) = fixture(listener.local_addr().unwrap().port(), "native-busy");
    let (first_started, observed) = tokio::sync::oneshot::channel();
    let (release_first, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut first = tokio_tungstenite::accept_async(tcp).await.unwrap();
        request(&mut first).await;
        respond(&mut first, "warmup").await;
        request(&mut first).await;
        first_started.send(()).unwrap();
        let (tcp, _) = listener.accept().await.unwrap();
        let mut second = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let body = request(&mut second).await;
        assert!(body.get("previous_response_id").is_none());
        respond(&mut second, "independent").await;
        released.await.unwrap();
        respond(&mut first, "cached").await;
        let _ = tokio::time::timeout(Duration::from_secs(3), first.next()).await;
    });
    assert_eq!(
        answer(&model, &context, options.clone()).await.stop_reason,
        "stop"
    );
    let cached_turn = stream_openai_codex_responses(&model, &context, Some(options.clone()));
    observed.await.unwrap();
    let independent = answer(&model, &context, options).await;
    assert_eq!(independent.response_id.as_deref(), Some("independent"));
    release_first.send(()).unwrap();
    let cached = tokio::time::timeout(Duration::from_secs(3), cached_turn.result())
        .await
        .unwrap();
    assert_eq!(cached.response_id.as_deref(), Some("cached"));
    close_openai_codex_web_socket_sessions(Some("native-busy"));
    server.await.unwrap();
}
