//! Internal model completion plumbing shared by both TypeScript memory extractors.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::evidence::Evidence;
use super::jobs::ExtractionResult;
use super::service::MemoryService;
use crate::core::refinement::refinement::{
    plan_refinement, PlanRefinementRequest, RefineModel, RefineOptions, RefinementEdit,
    RefinementProposal,
};

/// Map the pi-ai `AssistantMessage` onto refinement's minimal local shape.
fn to_refinement_assistant(
    message: &pi_ai::types::AssistantMessage,
) -> crate::core::refinement::refinement::AssistantMessage {
    use crate::core::refinement::refinement::{
        AssistantContent, AssistantMessage as LocalAssistant, AssistantUsage, StopReason,
    };
    let content = message
        .content
        .iter()
        .filter_map(|block| match block {
            pi_ai::types::ContentBlock::Text(text) => Some(AssistantContent::Text {
                text: text.text.clone(),
            }),
            pi_ai::types::ContentBlock::Thinking(thinking) => Some(AssistantContent::Thinking {
                thinking: thinking.thinking.clone(),
            }),
            pi_ai::types::ContentBlock::ToolCall(_) => None,
        })
        .collect();
    let stop_reason = match message.stop_reason.as_str() {
        "length" => StopReason::Length,
        "toolUse" => StopReason::ToolUse,
        "error" => StopReason::Error,
        "aborted" => StopReason::Aborted,
        _ => StopReason::Stop,
    };
    LocalAssistant {
        content,
        usage: AssistantUsage {
            input: message.usage.input,
            output: message.usage.output,
            cache_read: message.usage.cache_read,
            cache_write: message.usage.cache_write,
        },
        stop_reason,
        error_message: message.error_message.clone(),
    }
}

/// `completeWithProviderRetry(() => completeSimple(model, context, options))`.
///
/// The TypeScript closes over `model`, `apiKey` and `headers` captured just
/// before the completion; the port takes the same captured values.
pub(crate) fn completion_fn_for(
    model: pi_ai::types::Model,
    api_key: String,
    headers: Option<HashMap<String, String>>,
    retry: Option<crate::core::refinement::refinement::ProviderRetryPolicy>,
) -> crate::core::refinement::refinement::CompletionFn {
    Arc::new(
        move |request: crate::core::refinement::refinement::RefinementCompletionRequest| {
            let model = model.clone();
            let api_key = api_key.clone();
            let headers = headers.clone();
            Box::pin(async move {
                let options = pi_ai::types::SimpleStreamOptions {
                    stream: pi_ai::types::StreamOptions {
                        max_tokens: Some(request.max_tokens),
                        api_key: Some(api_key),
                        headers: headers.clone().map(|headers| {
                            headers
                                .into_iter()
                                .collect::<indexmap::IndexMap<String, String>>()
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let context = pi_ai::types::Context {
                    system_prompt: Some(request.system_prompt),
                    messages: request
                        .messages
                        .iter()
                        .filter_map(|message| local_message_to_pi_ai(message))
                        .collect(),
                    tools: None,
                };
                let response = crate::core::provider_retry::complete_with_provider_retry(
                    || pi_ai::stream::complete_simple(&model, &context, Some(&options)),
                    crate::core::provider_retry::ProviderRetryExecutionOptions {
                        policy: retry.map(|policy| {
                            crate::core::provider_retry::ProviderRetryPolicy {
                                enabled: policy.enabled,
                                max_retries: f64::from(policy.max_retries),
                                base_delay_ms: policy.base_delay_ms,
                                max_retry_delay_ms: policy.max_retry_delay_ms,
                            }
                        }),
                        ..Default::default()
                    },
                )
                .await;
                to_refinement_assistant(&response)
            })
        },
    )
}

/// `[{ type: "text", text: prompt }]` as a pi-ai user message.
fn local_message_to_pi_ai(
    message: &crate::core::memory::evidence::AgentMessage,
) -> Option<pi_ai::types::Message> {
    let crate::core::memory::evidence::AgentMessage::User { content, timestamp } = message else {
        return None;
    };
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<String>>()
            .join(""),
        _ => String::new(),
    };
    Some(pi_ai::types::Message::User(pi_ai::types::UserMessage::new(
        pi_ai::types::UserContent::Text(text),
        *timestamp as i64,
    )))
}

/// `extractor` / `createMemoryHostHandlers`: count each planning and repair response.
pub(crate) async fn extract(
    memory: &MemoryService,
    records: Vec<Evidence>,
    model: pi_ai::types::Model,
    api_key: String,
    headers: Option<HashMap<String, String>>,
    mut options: RefineOptions,
) -> Result<ExtractionResult, String> {
    let complete = completion_fn_for(
        model.clone(),
        api_key.clone(),
        headers.clone(),
        options.retry,
    );
    let usage = Arc::new(Mutex::new((0.0, 0.0)));
    let recorded_usage = usage.clone();
    let complete: crate::core::refinement::refinement::CompletionFn = Arc::new(move |request| {
        let complete = complete.clone();
        let recorded_usage = recorded_usage.clone();
        Box::pin(async move {
            let response = complete(request).await;
            let mut usage = recorded_usage.lock().unwrap_or_else(|p| p.into_inner());
            usage.0 +=
                response.usage.input + response.usage.cache_read + response.usage.cache_write;
            usage.1 += response.usage.output;
            response
        })
    });
    options.evidence = Some(records);
    options.max_output_tokens = Some(memory.store.settings().max_extraction_tokens as f64);
    let state = memory.store.read()?.harness();
    let plan = plan_refinement(PlanRefinementRequest {
        messages: &[],
        state: &state,
        history: &[],
        model: RefineModel {
            max_tokens: model.max_tokens,
        },
        api_key,
        options,
        headers: headers.clone(),
        thinking_level: None,
        complete,
    })
    .await
    .map_err(|error| error.message)?;
    let edits: Vec<RefinementEdit> = plan
        .proposal
        .edits
        .into_iter()
        .filter(|edit| {
            edit.action == "create"
                && edit.kind == "memory"
                && edit
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("evidenceStatus"))
                    .and_then(Value::as_str)
                    == Some("cited")
        })
        .collect();
    let proposal = RefinementProposal {
        edits,
        ..plan.proposal
    };
    let (input, output) = *usage.lock().unwrap_or_else(|p| p.into_inner());
    Ok(ExtractionResult {
        proposal,
        input,
        output,
    })
}
