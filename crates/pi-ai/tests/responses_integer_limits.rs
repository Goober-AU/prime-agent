//! Integer token limits must survive the actual HTTP serializer, not only numeric comparison.
use std::time::Duration;

use pi_ai::providers::openai_responses::{build_params, stream_simple_openai_responses, OpenAIResponsesOptions};
use pi_ai::types::{Context, Message, Model, SimpleStreamOptions, StreamOptions, UserContent, UserMessage};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn model(provider: &str, base_url: &str) -> Model {
    let mut model = Model::new("gpt-5.6-sol", "Sol", "openai-responses", provider, base_url);
    model.reasoning = true;
    model.thinking_level_map = Some([("xhigh".into(), Some("xhigh".into()))].into_iter().collect());
    model.max_tokens = 128_000.0;
    model.context_window = 400_000.0;
    model
}

#[test]
fn responses_token_limit_serializes_as_integer_without_changing_other_numbers() {
    for provider in ["github-copilot", "openai", "azure-openai-managed"] {
        for limit in [1, 4096, 16_000, 32_000, 128_000] {
            let options = OpenAIResponsesOptions {
                stream: StreamOptions {
                    max_tokens: Some(limit as f64),
                    temperature: Some(0.25),
                    ..Default::default()
                },
                service_tier: Some(Some("default".into())),
                reasoning_effort: Some("xhigh".into()),
                ..Default::default()
            };
            let params = build_params(&model(provider, "http://127.0.0.1:1"), &Context::default(), Some(&options)).unwrap();
            let wire = serde_json::to_vec(&params).unwrap();
            let parsed: Value = serde_json::from_slice(&wire).unwrap();
            assert_eq!(parsed["max_output_tokens"].as_u64(), Some(limit), "{provider}: token limit was not an integer");
            assert_eq!(parsed["temperature"], json!(0.25));
            assert_eq!(parsed["reasoning"]["effort"], "xhigh");
            assert_eq!(parsed.get("service_tier").is_some(), provider != "github-copilot");
        }
    }
}

#[test]
fn responses_rejects_invalid_token_limits_without_saturating_or_rounding() {
    for limit in [-1.0, 1.5, f64::NAN, f64::INFINITY, u64::MAX as f64] {
        let options = OpenAIResponsesOptions {
            stream: StreamOptions { max_tokens: Some(limit), ..Default::default() },
            ..Default::default()
        };
        assert!(build_params(&model("github-copilot", "http://127.0.0.1:1"), &Context::default(), Some(&options)).is_err());
    }
    for limit in [None, Some(0.0)] {
        let options = OpenAIResponsesOptions {
            stream: StreamOptions { max_tokens: limit, ..Default::default() },
            ..Default::default()
        };
        assert!(!build_params(&model("openai", "http://127.0.0.1:1"), &Context::default(), Some(&options))
            .unwrap().contains_key("max_output_tokens"));
    }
}

async fn strict_integer_provider(listener: TcpListener, expected_limit: u64) {
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut received = Vec::new();
    let (header_end, content_length) = loop {
        let mut chunk = [0_u8; 4096];
        let size = socket.read(&mut chunk).await.unwrap();
        assert!(size > 0, "request ended before headers");
        received.extend_from_slice(&chunk[..size]);
        assert!(received.len() < 128 * 1024, "unbounded test request");
        if let Some(end) = received.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&received[..end]);
            assert!(header.starts_with("POST /responses HTTP/1.1"));
            let length = header.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
            }).unwrap();
            break (end + 4, length);
        }
    };
    assert!(content_length < 128 * 1024);
    while received.len() < header_end + content_length {
        let mut chunk = [0_u8; 4096];
        let size = socket.read(&mut chunk).await.unwrap();
        assert!(size > 0);
        received.extend_from_slice(&chunk[..size]);
    }
    let body: Value = serde_json::from_slice(&received[header_end..header_end + content_length]).unwrap();
    let valid = body["max_output_tokens"].as_u64() == Some(expected_limit);
    assert!(body.get("service_tier").is_none());
    assert_eq!(body["reasoning"]["effort"], "xhigh");
    let (status, content_type, response) = if valid {
        ("200 OK", "text/event-stream", concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"fake\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"fake\",\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0,\"total_tokens\":1}}}\n\n"
        ))
    } else {
        ("400 Bad Request", "application/json", "{\"error\":{\"message\":\"failed to parse request\"}}")
    };
    socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
}

#[tokio::test]
async fn responses_chat_and_summary_budgets_pass_strict_integer_http_endpoint() {
    // Default chat limit and the explicit summary limit both use streamSimple.
    for (requested_limit, expected_limit) in [(None, 32_000), (Some(16_000.0), 16_000)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(10), strict_integer_provider(listener, expected_limit)).await.unwrap();
        });
        let options = SimpleStreamOptions {
            stream: StreamOptions {
                api_key: Some("fake-test-key".into()),
                max_tokens: requested_limit,
                timeout_ms: Some(5000.0),
                service_tier: Some(Some("default".into())),
                ..Default::default()
            },
            reasoning: Some("xhigh".into()),
            ..Default::default()
        };
        let context = Context::new(Some("Test summary or chat".into()), vec![Message::user(UserMessage::new(UserContent::Text("Synthetic text only".into()), 0))], None);
        let stream = stream_simple_openai_responses(&model("github-copilot", &url), &context, Some(options));
        let result = tokio::time::timeout(Duration::from_secs(10), stream.result()).await.unwrap();
        server.await.unwrap();
        assert_eq!(result.stop_reason, "stop", "{}", result.error_message.unwrap_or_default());
    }
}
