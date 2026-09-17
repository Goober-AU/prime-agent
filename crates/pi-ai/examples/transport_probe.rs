//! Explicit, bounded provider smoke test. Configuration (including credentials)
//! is read from stdin and never printed. Not run by the automated test suite.
use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pi_ai::providers::openai_responses::stream_simple_openai_responses;
use pi_ai::types::{
    AssistantMessageEvent, Context, Message, Model, SimpleStreamOptions, UserContent, UserMessage,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Configuration {
    model: Model,
    options: SimpleStreamOptions,
}

#[tokio::main]
async fn main() {
    let mut bytes = String::new();
    io::stdin()
        .take(128_000)
        .read_to_string(&mut bytes)
        .expect("probe configuration");
    let mut config: Configuration =
        serde_json::from_str(&bytes).expect("valid probe configuration");
    drop(bytes);
    let mut context = Context {
        messages: vec![Message::user(UserMessage::new(
            UserContent::Text("Reply with exactly OK.".into()),
            0,
        ))],
        ..Default::default()
    };
    for turn in 1..=2 {
        let started = Instant::now();
        let phases = Arc::new(Mutex::new(serde_json::Map::new()));
        let capture = phases.clone();
        config.options.stream.on_stream_observation = Some(Arc::new(move |stage| {
            capture
                .lock()
                .unwrap()
                .entry(stage.to_owned())
                .or_insert(json!(started.elapsed().as_secs_f64() * 1000.0));
        }));
        let status = Arc::new(Mutex::new(None));
        let capture = status.clone();
        config.options.stream.on_response = Some(Arc::new(move |response, _model| {
            *capture.lock().unwrap() = Some((
                response.status,
                response.headers.get("x-optimus-transport").cloned(),
            ));
            Box::pin(async {})
        }));
        let stream =
            stream_simple_openai_responses(&config.model, &context, Some(config.options.clone()));
        let result = tokio::time::timeout(Duration::from_secs(100), async {
            while let Some(event) = stream.next().await {
                match event {
                    AssistantMessageEvent::Done { message, .. } => return Some(message),
                    AssistantMessageEvent::Error { error, .. } => return Some(error),
                    _ => {}
                }
            }
            None
        })
        .await;
        let message = result.ok().flatten();
        println!(
            "{}",
            json!({"turn":turn,"elapsedMs":started.elapsed().as_secs_f64()*1000.0,
            "response":*status.lock().unwrap(),"phases":*phases.lock().unwrap(),
            "stop":message.as_ref().map(|m| &m.stop_reason),"usage":message.as_ref().map(|m| &m.usage),
            "error":message.as_ref().and_then(|m| m.error_message.as_deref())})
        );
        let Some(message) = message.filter(|m| m.stop_reason == "stop") else {
            std::process::exit(1)
        };
        context.messages.push(Message::assistant(message));
        context.messages.push(Message::user(UserMessage::new(
            UserContent::Text("Reply with exactly OK again.".into()),
            0,
        )));
    }
}
