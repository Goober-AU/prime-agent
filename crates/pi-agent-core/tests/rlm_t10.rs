//! T10 lane (owner rlm-agentcore): D-03 `Agent::wait_for_idle` lost-wakeup race and
//! D-14 tool-execution update delivery.
//!
//! Every test drives a real production entry point: `Agent::prompt`/`Agent::wait_for_idle`
//! for D-03, and the `agent_loop` event stream (the transport the UI consumes) for D-14.
//!
//! D-03 is covered by the in-crate `rlm_t10_tests` module in `src/agent.rs`, because the
//! lost-wakeup window is only reachable through a crate-private test seam.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_agent_core::agent_loop::agent_loop;
use pi_agent_core::types::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentState, AgentTool, AgentToolResult,
    ToolExecutionMode,
};
use pi_ai::providers::faux::{
    faux_assistant_message, register_faux_provider, FauxAssistantMessageOptions, FauxResponseStep,
};
use pi_agent_core::types::ContentBlock as ToolContentBlock;
use pi_ai::types::{ContentBlock, ToolCall, UserMessage, UserContent};
use serde_json::json;
use tokio::sync::Semaphore;
use tokio::time::timeout;

/// The awaited-value budget: far above any scheduling delay, far below any watchdog.
const WAIT_BUDGET: Duration = Duration::from_millis(250);
/// Bounded safety net so a blocked seam can never hang the test binary.
const SEAM_BUDGET: Duration = Duration::from_secs(5);
fn user_message_vec() -> Vec<AgentMessage> {
    vec![AgentMessage::from(UserMessage {
        role: "user".to_string(),
        content: UserContent::Text("go".to_string()),
        provider_context: None,
        timestamp: 1,
    })]
}

/// D-14: a gated tool emits two updates while it is unfinished, then a final result. The real
/// `agent_loop` stream is the transport: both updates must arrive, in order, before the tool
/// result and the final output, and nothing may be duplicated or lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_progress_arrives_before_completion() {
    let provider = register_faux_provider(None);
    provider.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            ContentBlock::ToolCall(ToolCall::new("call-1", "gated", serde_json::Map::new())).into(),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some("toolUse".to_string()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message("complete".into(), None)),
    ]);

    let observed_by_tool = Arc::new(Mutex::new(Vec::<usize>::new()));
    let recorder = observed_by_tool.clone();
    let tool = AgentTool {
        name: "gated".to_string(),
        description: "emits two updates before finishing".to_string(),
        parameters: json!({"type": "object", "properties": {}}),
        label: "Gated".to_string(),
        prepare_arguments: None,
        execute: Arc::new(move |_, _, _, on_update| {
            let on_update = on_update.expect("the loop must pass an update callback");
            let recorder = recorder.clone();
            Box::pin(async move {
                for step in 1..=2usize {
                    recorder.lock().unwrap().push(step);
                    on_update(AgentToolResult::new(
                        vec![ToolContentBlock::text(format!("update-{step}"))],
                        json!({ "step": step }),
                    ));
                    tokio::task::yield_now().await;
                }
                Ok(AgentToolResult::new(
                    vec![ToolContentBlock::text("final")],
                    json!({}),
                ))
            })
        }),
        execution_mode: Some(ToolExecutionMode::Sequential),
    };

    let context = AgentContext {
        system_prompt: String::new(),
        messages: Vec::new(),
        tools: Some(vec![tool]),
    };
    let mut config = AgentLoopConfig::new(provider.get_model());
    config.tool_execution = Some(ToolExecutionMode::Sequential);
    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stream = agent_loop(user_message_vec(), context, config, None, None);
    let drain = tokio::spawn({
        let order = order.clone();
        async move {
            while let Some(event) = stream.next().await {
                let label = match &event {
                    AgentEvent::ToolExecutionUpdate { partial_result, .. } => {
                        let text = partial_result
                            .content
                            .first()
                            .and_then(ToolContentBlock::as_text)
                            .unwrap_or("")
                            .to_string();
                        format!("update:{text}")
                    }
                    AgentEvent::ToolExecutionEnd { is_error, .. } => format!("end:error={is_error}"),
                    AgentEvent::MessageEnd { message } => format!("message_end:{}", message.role()),
                    AgentEvent::AgentEnd { .. } => "agent_end".to_string(),
                    other => format!("other:{}", other.type_name()),
                };
                order.lock().unwrap().push(label);
            }
        }
    });
    timeout(SEAM_BUDGET, drain)
        .await
        .expect("the loop never finished")
        .unwrap();

    let order = order.lock().unwrap().clone();
    let updates: Vec<&str> = order
        .iter()
        .filter(|label| label.starts_with("update:"))
        .map(String::as_str)
        .collect();
    assert_eq!(
        updates,
        vec!["update:update-1", "update:update-2"],
        "the transport must observe both updates exactly once and in order; observed {order:?}"
    );
    let end_index = order
        .iter()
        .position(|label| label.starts_with("end:"))
        .expect("tool_execution_end must be emitted");
    let last_update_index = order
        .iter()
        .rposition(|label| label.starts_with("update:"))
        .unwrap();
    assert!(
        last_update_index < end_index,
        "updates must arrive before completion; observed {order:?}"
    );
    provider.unregister();
}

/// D-14: progress publishers that all call the update callback while the tool is unfinished must
/// not lose a delivery. A single 8-publisher fanout drops an update only about 70% of the time on
/// the baseline, so the oracle fans out 32 publishers over 5 independent rounds: the baseline must
/// fail a round in almost every run (measured rate is recorded separately), while a lossless
/// implementation passes every round.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_tool_updates_are_not_silently_dropped() {
    const PUBLISHERS: usize = 32;
    const ROUNDS: usize = 5;
    let mut shortfalls = Vec::new();
    for round in 0..ROUNDS {
        let provider = register_faux_provider(None);
        provider.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                ContentBlock::ToolCall(ToolCall::new("call-1", "fanout", serde_json::Map::new())).into(),
                Some(FauxAssistantMessageOptions {
                    stop_reason: Some("toolUse".to_string()),
                    ..Default::default()
                }),
            )),
            FauxResponseStep::Message(faux_assistant_message("complete".into(), None)),
        ]);

        let delivered = Arc::new(AtomicUsize::new(0));
        let counter = delivered.clone();
        let tool = AgentTool {
            name: "fanout".to_string(),
            description: "concurrent progress publishers".to_string(),
            parameters: json!({"type": "object", "properties": {}}),
            label: "Fanout".to_string(),
            prepare_arguments: None,
            execute: Arc::new(move |_, _, _, on_update| {
                let on_update = on_update.expect("the loop must pass an update callback");
                Box::pin(async move {
                    let start = Arc::new(Semaphore::new(0));
                    let mut handles = Vec::new();
                    for index in 0..PUBLISHERS {
                        let on_update = on_update.clone();
                        let start = start.clone();
                        handles.push(tokio::spawn(async move {
                            start.acquire().await.unwrap().forget();
                            on_update(AgentToolResult::new(
                                vec![ToolContentBlock::text(format!("u{index}"))],
                                json!({ "index": index }),
                            ));
                        }));
                    }
                    // Release every publisher at the same rendezvous so the update callback runs
                    // concurrently on all worker threads.
                    start.add_permits(PUBLISHERS);
                    for handle in handles {
                        handle.await.unwrap();
                    }
                    Ok(AgentToolResult::new(vec![ToolContentBlock::text("final")], json!({})))
                })
            }),
            execution_mode: Some(ToolExecutionMode::Sequential),
        };

        let context = AgentContext {
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: Some(vec![tool]),
        };
        let mut config = AgentLoopConfig::new(provider.get_model());
        config.tool_execution = Some(ToolExecutionMode::Sequential);
        let stream = agent_loop(user_message_vec(), context, config, None, None);
        let drain = tokio::spawn({
            let delivered = delivered.clone();
            async move {
                while let Some(event) = stream.next().await {
                    if matches!(event, AgentEvent::ToolExecutionUpdate { .. }) {
                        delivered.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        });
        timeout(SEAM_BUDGET, drain)
            .await
            .expect("the loop never finished")
            .unwrap();
        provider.unregister();
        let seen = delivered.load(Ordering::SeqCst);
        if seen != PUBLISHERS {
            shortfalls.push((round, seen));
        }
    }
    assert!(
        shortfalls.is_empty(),
        "every accepted tool update must reach the transport in every round; rounds with a \
         shortfall (round, delivered) = {shortfalls:?} out of {ROUNDS} rounds of {PUBLISHERS} publishers"
    );
}

/// D-14: an update published while the tool is still unfinished must reach the transport while
/// the tool is still unfinished. The TypeScript loop starts `emit(...)` at publish time
/// (`updateEvents.push(Promise.resolve(emit(...)))`), so a client can render progress during a
/// long tool. This is an interleaving requirement, not a latency measurement: the tool waits
/// for the transport to acknowledge update 1 before it publishes update 2.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_progress_is_observable_while_the_tool_is_unfinished() {
    let provider = register_faux_provider(None);
    provider.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            ContentBlock::ToolCall(ToolCall::new("call-1", "gated", serde_json::Map::new())).into(),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some("toolUse".to_string()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message("complete".into(), None)),
    ]);

    let acknowledged = Arc::new(Semaphore::new(0));
    let streamed_while_unfinished = Arc::new(AtomicUsize::new(0));
    let tool_flag = streamed_while_unfinished.clone();
    let tool_ack = acknowledged.clone();
    let tool = AgentTool {
        name: "gated".to_string(),
        description: "waits for its first update to reach the transport".to_string(),
        parameters: json!({"type": "object", "properties": {}}),
        label: "Gated".to_string(),
        prepare_arguments: None,
        execute: Arc::new(move |_, _, _, on_update| {
            let on_update = on_update.expect("the loop must pass an update callback");
            let tool_ack = tool_ack.clone();
            let tool_flag = tool_flag.clone();
            Box::pin(async move {
                on_update(AgentToolResult::new(
                    vec![ToolContentBlock::text("update-1")],
                    json!({ "step": 1 }),
                ));
                // Bounded wait: the transport must observe update 1 while this tool is running.
                let deadline = Duration::from_secs(2);
                if timeout(deadline, tool_ack.acquire()).await.is_ok() {
                    tool_flag.store(1, Ordering::SeqCst);
                }
                on_update(AgentToolResult::new(
                    vec![ToolContentBlock::text("update-2")],
                    json!({ "step": 2 }),
                ));
                Ok(AgentToolResult::new(vec![ToolContentBlock::text("final")], json!({})))
            })
        }),
        execution_mode: Some(ToolExecutionMode::Sequential),
    };

    let context = AgentContext {
        system_prompt: String::new(),
        messages: Vec::new(),
        tools: Some(vec![tool]),
    };
    let mut config = AgentLoopConfig::new(provider.get_model());
    config.tool_execution = Some(ToolExecutionMode::Sequential);
    let stream = agent_loop(user_message_vec(), context, config, None, None);
    let drain = tokio::spawn({
        let acknowledged = acknowledged.clone();
        async move {
            while let Some(event) = stream.next().await {
                if let AgentEvent::ToolExecutionUpdate { partial_result, .. } = &event {
                    let text = partial_result
                        .content
                        .first()
                        .and_then(ToolContentBlock::as_text)
                        .unwrap_or("");
                    if text == "update-1" {
                        acknowledged.add_permits(1);
                    }
                }
            }
        }
    });
    timeout(SEAM_BUDGET, drain)
        .await
        .expect("the loop never finished")
        .unwrap();
    provider.unregister();
    assert_eq!(
        streamed_while_unfinished.load(Ordering::SeqCst),
        1,
        "the transport did not observe the tool update while the tool was still unfinished"
    );
}

/// D-14 order contract: the transport must observe concurrent publishers in publish order.
/// TypeScript calls `emit(...)` synchronously inside each publisher callback, so emission order
/// equals publish order; a task per update would be allowed to invert it. The publishers publish
/// from index order 0..8 into the ordered queue before releasing each other, so any other order
/// means the port reordered or dropped a required update.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_tool_updates_keep_publish_order() {
    const PUBLISHERS: usize = 8;
    let provider = register_faux_provider(None);
    provider.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            ContentBlock::ToolCall(ToolCall::new("call-1", "ordered", serde_json::Map::new())).into(),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some("toolUse".to_string()),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message("complete".into(), None)),
    ]);

    let published: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = published.clone();
    let tool = AgentTool {
        name: "ordered".to_string(),
        description: "publishes 0..8 in a deterministic order".to_string(),
        parameters: json!({"type": "object", "properties": {}}),
        label: "Ordered".to_string(),
        prepare_arguments: None,
        execute: Arc::new(move |_, _, _, on_update| {
            let on_update = on_update.expect("the loop must pass an update callback");
            let recorder = recorder.clone();
            Box::pin(async move {
                // Publish strictly in order from this task; each publish returns before the next,
                // so the callback invocation order is 0,1,2,...,7.
                for index in 0..PUBLISHERS {
                    recorder.lock().unwrap().push(index);
                    on_update(AgentToolResult::new(
                        vec![ToolContentBlock::text(format!("u{index}"))],
                        json!({ "index": index }),
                    ));
                    tokio::task::yield_now().await;
                }
                Ok(AgentToolResult::new(vec![ToolContentBlock::text("final")], json!({})))
            })
        }),
        execution_mode: Some(ToolExecutionMode::Sequential),
    };

    let context = AgentContext {
        system_prompt: String::new(),
        messages: Vec::new(),
        tools: Some(vec![tool]),
    };
    let mut config = AgentLoopConfig::new(provider.get_model());
    config.tool_execution = Some(ToolExecutionMode::Sequential);
    let observed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stream = agent_loop(user_message_vec(), context, config, None, None);
    let drain = tokio::spawn({
        let observed = observed.clone();
        async move {
            while let Some(event) = stream.next().await {
                if let AgentEvent::ToolExecutionUpdate { partial_result, .. } = &event {
                    let text = partial_result
                        .content
                        .first()
                        .and_then(ToolContentBlock::as_text)
                        .unwrap_or("")
                        .to_string();
                    observed.lock().unwrap().push(text);
                }
            }
        }
    });
    timeout(SEAM_BUDGET, drain)
        .await
        .expect("the loop never finished")
        .unwrap();
    provider.unregister();
    let expected: Vec<String> = (0..PUBLISHERS).map(|index| format!("u{index}")).collect();
    let seen = observed.lock().unwrap().clone();
    assert_eq!(
        seen, expected,
        "the transport must observe every update in publish order; published {:?}, observed {:?}",
        published.lock().unwrap(),
        seen
    );
}
