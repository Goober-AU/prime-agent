//! Port of packages/ai/src/providers/transform-messages.ts
use crate::compaction::compaction_matches_model;
use crate::types::{
	AssistantMessage, ContentBlock, ImageOrTextContent, InputModality, Message, Model, TextContent, ToolCall,
	ToolResultMessage, UserContent,
};

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str = "(tool image omitted: model does not support images)";

fn replace_images_with_placeholder(content: &[ImageOrTextContent], placeholder: &str) -> Vec<TextContent> {
	let mut result: Vec<TextContent> = Vec::new();
	let mut previous_was_placeholder = false;

	for block in content {
		if let ImageOrTextContent::Image(_) = block {
			if !previous_was_placeholder {
				result.push(TextContent::new(placeholder));
			}
			previous_was_placeholder = true;
			continue;
		}

		let text = match block {
			ImageOrTextContent::Text(text) => text.clone(),
			ImageOrTextContent::Image(_) => unreachable!(),
		};
		previous_was_placeholder = text.text == placeholder;
		result.push(text);
	}

	result
}

fn downgrade_unsupported_images(messages: Vec<Message>, model: &Model) -> Vec<Message> {
	if model.input.iter().any(|m| matches!(m, InputModality::Image)) {
		return messages;
	}

	messages
		.into_iter()
		.map(|msg| match msg {
			Message::User(mut user) => {
				if let UserContent::Blocks(blocks) = &user.content {
					let replaced = replace_images_with_placeholder(blocks, NON_VISION_USER_IMAGE_PLACEHOLDER);
					user.content = UserContent::Blocks(replaced.into_iter().map(ImageOrTextContent::Text).collect());
				}
				Message::User(user)
			}
			Message::ToolResult(mut result) => {
				let replaced = replace_images_with_placeholder(&result.content, NON_VISION_TOOL_IMAGE_PLACEHOLDER);
				result.content = replaced.into_iter().map(ImageOrTextContent::Text).collect();
				Message::ToolResult(result)
			}
			other => other,
		})
		.collect()
}

/// Normalize tool call ID for cross-provider compatibility.
/// OpenAI Responses API generates IDs that are 450+ chars with special characters like `|`.
/// Anthropic APIs require IDs matching ^[a-zA-Z0-9_-]+$ (max 64 chars).
///
/// TS: `transformMessages(messages, model, normalizeToolCallId?)`
///
/// The TypeScript throws when a user message carries a compaction checkpoint for another model.
/// Provider code must use [`try_transform_messages`] and route the error through its own catch
/// block; this wrapper keeps the throwing shape for direct callers.
pub fn transform_messages(
	messages: Vec<Message>,
	model: &Model,
	normalize_tool_call_id: Option<&dyn Fn(&str, &Model, &AssistantMessage) -> String>,
) -> Vec<Message> {
	match try_transform_messages(messages, model, normalize_tool_call_id) {
		Ok(messages) => messages,
		Err(error) => panic!("{error}"),
	}
}

/// [`transform_messages`] with the TypeScript `throw` turned into `Err`.
pub fn try_transform_messages(
	messages: Vec<Message>,
	model: &Model,
	normalize_tool_call_id: Option<&dyn Fn(&str, &Model, &AssistantMessage) -> String>,
) -> Result<Vec<Message>, String> {
	use std::collections::HashMap;

	let mut tool_call_id_map: HashMap<String, String> = HashMap::new();
	let image_aware_messages = downgrade_unsupported_images(messages, model);

	let mut transformed: Vec<Message> = Vec::with_capacity(image_aware_messages.len());
	for msg in image_aware_messages {
		let msg = match msg {
			Message::User(user) => {
				if let Some(provider_context) = &user.provider_context {
					if !compaction_matches_model(provider_context, model) {
						return Err("Compaction checkpoint belongs to another model or provider; rebuild context from the session transcript".to_string());
					}
				}
				Message::User(user)
			}
			Message::ToolResult(mut result) => {
				if let Some(normalized_id) = tool_call_id_map.get(&result.tool_call_id) {
					if *normalized_id != result.tool_call_id {
						result.tool_call_id = normalized_id.clone();
					}
				}
				Message::ToolResult(result)
			}
			Message::Assistant(assistant_msg) => {
				let is_same_model = assistant_msg.provider == model.provider
					&& assistant_msg.api == model.api
					&& assistant_msg.model == model.id;

				let mut transformed_content: Vec<ContentBlock> = Vec::new();
				for block in assistant_msg.content.iter() {
					match block {
						ContentBlock::Thinking(thinking) => {
							// Redacted thinking is opaque encrypted content, only valid for the same model.
							// Drop it for cross-model to avoid API errors.
							if thinking.redacted == Some(true) {
								if is_same_model {
									transformed_content.push(block.clone());
								}
								continue;
							}
							// For same model: keep thinking blocks with signatures (needed for replay)
							// even if the thinking text is empty (OpenAI encrypted reasoning)
							if is_same_model && thinking.thinking_signature.is_some() {
								transformed_content.push(block.clone());
								continue;
							}
							// Skip empty thinking blocks, convert others to plain text
							if thinking.thinking.is_empty() || thinking.thinking.trim().is_empty() {
								continue;
							}
							if is_same_model {
								transformed_content.push(block.clone());
								continue;
							}
							transformed_content.push(ContentBlock::Text(TextContent::new(thinking.thinking.clone())));
						}
						ContentBlock::Text(text) => {
							transformed_content.push(ContentBlock::Text(text.clone()));
						}
						ContentBlock::ToolCall(tool_call) => {
							let mut normalized_tool_call = tool_call.clone();

							if !is_same_model && tool_call.thought_signature.is_some() {
								normalized_tool_call.thought_signature = None;
							}

							if !is_same_model {
								if let Some(normalize) = normalize_tool_call_id {
									let normalized_id = normalize(&tool_call.id, model, &assistant_msg);
									if normalized_id != tool_call.id {
										tool_call_id_map.insert(tool_call.id.clone(), normalized_id.clone());
										normalized_tool_call.id = normalized_id;
									}
								}
							}

							transformed_content.push(ContentBlock::ToolCall(normalized_tool_call));
						}
					}
				}

				Message::Assistant(AssistantMessage {
					content: transformed_content,
					..assistant_msg
				})
			}
		};
		transformed.push(msg);
	}

	// This preserves thinking signatures and satisfies API requirements
	let mut result: Vec<Message> = Vec::new();
	let mut pending_tool_calls: Vec<ToolCall> = Vec::new();
	let mut existing_tool_result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

	fn insert_synthetic_tool_results(
		result: &mut Vec<Message>,
		pending_tool_calls: &mut Vec<ToolCall>,
		existing_tool_result_ids: &mut std::collections::HashSet<String>,
	) {
		if !pending_tool_calls.is_empty() {
			for tc in pending_tool_calls.iter() {
				if !existing_tool_result_ids.contains(&tc.id) {
					result.push(Message::tool_result(ToolResultMessage::new(
						tc.id.clone(),
						tc.name.clone(),
						vec![ImageOrTextContent::Text(TextContent::new("No result provided"))],
						true,
						crate::utils::now_ms(),
					)));
				}
			}
			pending_tool_calls.clear();
			existing_tool_result_ids.clear();
		}
	}

	for msg in transformed.into_iter() {
		match msg {
			Message::Assistant(assistant_msg) => {
				insert_synthetic_tool_results(&mut result, &mut pending_tool_calls, &mut existing_tool_result_ids);

				// Skip errored/aborted assistant messages entirely.
				// These are incomplete turns that shouldn't be replayed:
				// - May have partial content (reasoning without message, incomplete tool calls)
				// - Replaying them can cause API errors (e.g., OpenAI "reasoning without following item")
				// - The model should retry from the last valid state
				if assistant_msg.stop_reason == "error" || assistant_msg.stop_reason == "aborted" {
					continue;
				}

				let tool_calls: Vec<ToolCall> = assistant_msg
					.content
					.iter()
					.filter_map(|b| match b {
						ContentBlock::ToolCall(tc) => Some(tc.clone()),
						_ => None,
					})
					.collect();
				if !tool_calls.is_empty() {
					pending_tool_calls = tool_calls;
					existing_tool_result_ids.clear();
				}

				result.push(Message::Assistant(assistant_msg));
			}
			Message::ToolResult(result_msg) => {
				if !pending_tool_calls.iter().any(|tool_call| tool_call.id == result_msg.tool_call_id) {
					continue;
				}
				existing_tool_result_ids.insert(result_msg.tool_call_id.clone());
				result.push(Message::ToolResult(result_msg));
			}
			Message::User(user) => {
				insert_synthetic_tool_results(&mut result, &mut pending_tool_calls, &mut existing_tool_result_ids);
				result.push(Message::User(user));
			}
		}
	}

	insert_synthetic_tool_results(&mut result, &mut pending_tool_calls, &mut existing_tool_result_ids);

	Ok(result)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::types::{ImageContent, ThinkingContent, Usage, UserMessage};

	fn model_with_input(input: Vec<InputModality>) -> Model {
		Model {
			id: "test-model".to_string(),
			provider: "test-provider".to_string(),
			api: "test-api".to_string(),
			input,
			..Default::default()
		}
	}

	fn text_model() -> Model {
		model_with_input(vec![InputModality::Text])
	}

	fn assistant(model: &str, content: Vec<ContentBlock>, stop_reason: &str) -> Message {
		Message::assistant(AssistantMessage {
			content,
			api: "test-api".to_string(),
			provider: "test-provider".to_string(),
			model: model.to_string(),
			usage: Usage::zero(),
			stop_reason: stop_reason.to_string(),
			timestamp: 1,
			..Default::default()
		})
	}

	fn tool_call(id: &str, name: &str) -> ContentBlock {
		ContentBlock::ToolCall(ToolCall::new(id, name, serde_json::Map::new()))
	}

	fn user_text(text: &str) -> Message {
		Message::user(UserMessage::new(UserContent::Text(text.to_string()), 0))
	}

	fn tool_result(id: &str) -> Message {
		Message::tool_result(ToolResultMessage::new(
			id,
			"tool",
			vec![ImageOrTextContent::Text(TextContent::new("ok"))],
			false,
			0,
		))
	}

	#[test]
	fn replaces_user_images_for_non_vision_model() {
		let user = Message::user(UserMessage::new(
			UserContent::Blocks(vec![
				ImageOrTextContent::Image(ImageContent::new("AAAA", "image/png")),
				ImageOrTextContent::Image(ImageContent::new("BBBB", "image/png")),
				ImageOrTextContent::Text(TextContent::new("after")),
			]),
			0,
		));
		let out = transform_messages(vec![user], &text_model(), None);
		let blocks = match &out[0] {
			Message::User(u) => match &u.content {
				UserContent::Blocks(blocks) => blocks.clone(),
				_ => panic!("expected blocks"),
			},
			_ => panic!("expected user"),
		};
		assert_eq!(blocks.len(), 3);
		match &blocks[0] {
			ImageOrTextContent::Text(t) => assert_eq!(t.text, NON_VISION_USER_IMAGE_PLACEHOLDER),
			_ => panic!("expected text"),
		}
		match &blocks[1] {
			ImageOrTextContent::Text(t) => assert_eq!(t.text, "after"),
			_ => panic!("expected text"),
		}
	}

	#[test]
	fn keeps_images_for_vision_model() {
		let user = Message::user(UserMessage::new(
			UserContent::Blocks(vec![ImageOrTextContent::Image(ImageContent::new("AAAA", "image/png"))]),
			0,
		));
		let out = transform_messages(vec![user], &model_with_input(vec![InputModality::Text, InputModality::Image]), None);
		match &out[0] {
			Message::User(u) => match &u.content {
				UserContent::Blocks(blocks) => assert!(matches!(blocks[0], ImageOrTextContent::Image(_))),
				_ => panic!("expected blocks"),
			},
			_ => panic!("expected user"),
		}
	}

	#[test]
	fn inserts_synthetic_tool_result_when_missing() {
		let out = transform_messages(
			vec![user_text("hi"), assistant("test-model", vec![tool_call("call-1", "read")], "toolUse")],
			&text_model(),
			None,
		);
		assert_eq!(out.len(), 3);
		match &out[2] {
			Message::ToolResult(r) => {
				assert_eq!(r.tool_call_id, "call-1");
				assert_eq!(r.tool_name, "read");
				assert!(r.is_error);
				assert_eq!(r.content.len(), 1);
				match &r.content[0] {
					ImageOrTextContent::Text(t) => assert_eq!(t.text, "No result provided"),
					_ => panic!("expected text"),
				}
			}
			_ => panic!("expected synthetic tool result"),
		}
	}

	#[test]
	fn keeps_real_tool_result_and_drops_orphans() {
		let out = transform_messages(
			vec![
				assistant("test-model", vec![tool_call("call-1", "read")], "toolUse"),
				tool_result("call-1"),
				tool_result("orphan"),
			],
			&text_model(),
			None,
		);
		assert_eq!(out.len(), 2);
		match &out[1] {
			Message::ToolResult(r) => assert_eq!(r.tool_call_id, "call-1"),
			_ => panic!("expected tool result"),
		}
	}

	#[test]
	fn drops_error_and_aborted_assistant_messages() {
		let out = transform_messages(
			vec![
				user_text("hi"),
				assistant("test-model", vec![ContentBlock::Text(TextContent::new("partial"))], "error"),
				assistant("test-model", vec![ContentBlock::Text(TextContent::new("partial"))], "aborted"),
			],
			&text_model(),
			None,
		);
		assert_eq!(out.len(), 1);
	}

	#[test]
	fn converts_thinking_to_text_for_other_model() {
		let mut thinking = ThinkingContent::new("deep thought");
		thinking.thinking_signature = Some("sig".to_string());
		let out = transform_messages(
			vec![assistant("other-model", vec![ContentBlock::Thinking(thinking)], "stop")],
			&text_model(),
			None,
		);
		match &out[0] {
			Message::Assistant(a) => match &a.content[0] {
				ContentBlock::Text(t) => assert_eq!(t.text, "deep thought"),
				_ => panic!("expected text block"),
			},
			_ => panic!("expected assistant"),
		}
	}

	#[test]
	fn keeps_signed_thinking_for_same_model_even_when_empty() {
		let mut thinking = ThinkingContent::new("");
		thinking.thinking_signature = Some("sig".to_string());
		let out = transform_messages(
			vec![assistant("test-model", vec![ContentBlock::Thinking(thinking)], "stop")],
			&text_model(),
			None,
		);
		match &out[0] {
			Message::Assistant(a) => {
				assert_eq!(a.content.len(), 1);
				assert!(matches!(a.content[0], ContentBlock::Thinking(_)));
			}
			_ => panic!("expected assistant"),
		}
	}

	#[test]
	fn drops_redacted_thinking_for_other_model() {
		let mut thinking = ThinkingContent::new("");
		thinking.thinking_signature = Some("encrypted".to_string());
		thinking.redacted = Some(true);
		let out = transform_messages(
			vec![assistant("other-model", vec![ContentBlock::Thinking(thinking)], "stop")],
			&text_model(),
			None,
		);
		match &out[0] {
			Message::Assistant(a) => assert!(a.content.is_empty()),
			_ => panic!("expected assistant"),
		}
	}

	#[test]
	fn normalizes_tool_call_ids_across_models_and_rewrites_results() {
		let normalize = |id: &str, _model: &Model, _source: &AssistantMessage| format!("norm-{id}");
		let out = transform_messages(
			vec![
				assistant("other-model", vec![tool_call("call-1", "read")], "toolUse"),
				tool_result("call-1"),
			],
			&text_model(),
			Some(&normalize),
		);
		match &out[0] {
			Message::Assistant(a) => match &a.content[0] {
				ContentBlock::ToolCall(tc) => assert_eq!(tc.id, "norm-call-1"),
				_ => panic!("expected tool call"),
			},
			_ => panic!("expected assistant"),
		}
		match &out[1] {
			Message::ToolResult(r) => assert_eq!(r.tool_call_id, "norm-call-1"),
			_ => panic!("expected tool result"),
		}
	}

	#[test]
	fn does_not_normalize_for_same_model() {
		let normalize = |id: &str, _model: &Model, _source: &AssistantMessage| format!("norm-{id}");
		let out = transform_messages(
			vec![assistant("test-model", vec![tool_call("call-1", "read")], "toolUse")],
			&text_model(),
			Some(&normalize),
		);
		match &out[0] {
			Message::Assistant(a) => match &a.content[0] {
				ContentBlock::ToolCall(tc) => assert_eq!(tc.id, "call-1"),
				_ => panic!("expected tool call"),
			},
			_ => panic!("expected assistant"),
		}
	}

	#[test]
	fn strips_thought_signature_for_other_model() {
		let mut tc = ToolCall::new("call-1", "read", serde_json::Map::new());
		tc.thought_signature = Some("sig".to_string());
		let out = transform_messages(
			vec![assistant("other-model", vec![ContentBlock::ToolCall(tc)], "toolUse")],
			&text_model(),
			None,
		);
		match &out[0] {
			Message::Assistant(a) => match &a.content[0] {
				ContentBlock::ToolCall(tc) => assert_eq!(tc.thought_signature, None),
				_ => panic!("expected tool call"),
			},
			_ => panic!("expected assistant"),
		}
	}
}
