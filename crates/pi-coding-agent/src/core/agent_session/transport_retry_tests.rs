use super::*;
use pi_ai::types::{AssistantMessageEvent, ContentBlock, ToolCall};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_ai::utils::stream_failure::{record_stream_failure, StreamFailureError, StreamFailureInfo, ThrownStreamError};
use std::sync::atomic::AtomicUsize;

async fn assert_attempts(kind: &str, content: Vec<ContentBlock>, expected: usize) {
    let session = post_compaction_continuation_tests::test_session_with_credentials().await;
    session.settings_manager.lock().unwrap().set_retry_enabled(true);
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let kind = kind.to_string();
    session.agent.set_stream_fn(Arc::new(move |model, _, _| {
        let index = count.fetch_add(1, Ordering::SeqCst);
        let kind = kind.clone();
        let content = content.clone();
        Box::pin(async move {
            let stream = AssistantMessageEventStream::new();
            let mut message = AssistantMessage::new(model.api.clone(), model.provider.clone(), model.id.clone(), 0);
            stream.push(AssistantMessageEvent::Start { partial: message.clone() });
            if index == 0 {
                message.content = content;
                message.stop_reason = "error".into();
                message.error_message = Some("Isolated provider interruption".into());
                let failure = StreamFailureError::new("Isolated provider interruption", StreamFailureInfo {
                    kind, ..Default::default()
                });
                record_stream_failure(&model, &mut message, &ThrownStreamError::Failure(&failure));
                stream.push(AssistantMessageEvent::Error { reason: "error".into(), error: message });
            } else {
                message.content = vec![ContentBlock::Text(TextContent::new("retried"))];
                stream.push(AssistantMessageEvent::Done { reason: "stop".into(), message });
            }
            stream.end(None);
            stream
        })
    }));
    tokio::time::timeout(std::time::Duration::from_secs(8), session.prompt("isolated retry test", None))
        .await.expect("prompt bounded").expect("prompt settled");
    tokio::time::timeout(std::time::Duration::from_secs(8), session.wait_for_idle())
        .await.expect("idle bounded").expect("idle settled");
    let actual = attempts.load(Ordering::SeqCst);
    if expected == 1 {
        assert!(session.agent.state().messages.iter().any(|entry| matches!(entry,
            AgentMessage::Message(Message::Assistant(message)) if message.stop_reason == "error"
        )), "the interrupted response must remain visible instead of being dropped for replay");
    }
    session.dispose_async(Some(false)).await;
    assert_eq!(actual, expected, "automatic request count");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_websocket_send_never_replays_even_before_visible_output() {
    assert_attempts("request_interrupted", vec![], 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_error_after_partial_text_never_replays() {
    assert_attempts("server_error", vec![ContentBlock::Text(TextContent::new("already shown"))], 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_error_after_tool_proposal_never_replays() {
    assert_attempts("server_error", vec![ContentBlock::ToolCall(ToolCall::new("partial", "never_execute", Default::default()))], 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_pre_output_rate_limit_still_retries_once_then_succeeds() {
    assert_attempts("rate_limit", vec![], 2).await;
}

fn prepare_retry(session: &AgentSession) {
    session.settings_manager.lock().unwrap().set_retry_enabled(true);
    let message = AssistantMessage {
        stop_reason: "error".into(), error_message: Some("retry fixture failure".into()),
        ..Default::default()
    };
    session.create_retry_promise_for_agent_end(&AgentEvent::AgentEnd {
        messages: vec![AgentMessage::Message(Message::Assistant(message))],
    });
    assert!(session.is_retrying());
}

#[tokio::test]
async fn retry_waiters_share_pending_state_and_completion_never_resurrects_busy() {
    let session = post_compaction_continuation_tests::test_session_with_credentials().await;
    prepare_retry(&session);
    let mut first = Box::pin(session.wait_for_retry());
    let mut second = Box::pin(session.wait_for_retry());
    assert!(futures::poll!(first.as_mut()).is_pending());
    assert!(session.is_retrying(), "one waiter cannot consume shared retry state");
    assert!(futures::poll!(second.as_mut()).is_pending(), "both waiters must await settlement");
    session.resolve_retry();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(first, second);
    }).await.expect("both waiters settle");
    assert!(!session.is_retrying(), "completed waiters cannot reinsert ready promises");
    session.dispose_async(Some(false)).await;
}

#[tokio::test]
async fn settled_retry_waiter_cannot_clear_the_next_retry_generation() {
    let session = post_compaction_continuation_tests::test_session_with_credentials().await;
    prepare_retry(&session);
    let mut old = Box::pin(session.wait_for_retry());
    assert!(futures::poll!(old.as_mut()).is_pending());
    session.resolve_retry();
    prepare_retry(&session);
    tokio::time::timeout(std::time::Duration::from_secs(2), old).await.unwrap();
    assert!(session.is_retrying(), "old waiter cannot settle a new generation");
    session.resolve_retry();
    assert!(!session.is_retrying());
    session.dispose_async(Some(false)).await;
}
