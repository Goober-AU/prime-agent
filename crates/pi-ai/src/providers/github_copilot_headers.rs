//! Port of packages/ai/src/providers/github-copilot-headers.ts
use crate::types::{ImageOrTextContent, Message, UserContent};
use indexmap::IndexMap;

/// TS: `inferCopilotInitiator(messages)`
pub fn infer_copilot_initiator(messages: &[Message]) -> String {
	match messages.last() {
		Some(Message::Assistant(_)) | Some(Message::ToolResult(_)) => "agent".to_string(),
		_ => "user".to_string(),
	}
}

/// TS: `hasCopilotVisionInput(messages)`
pub fn has_copilot_vision_input(messages: &[Message]) -> bool {
	messages.iter().any(|msg| match msg {
		Message::User(user) => match &user.content {
			UserContent::Text(_) => false,
			UserContent::Blocks(blocks) => blocks.iter().any(|c| matches!(c, ImageOrTextContent::Image(_))),
		},
		Message::ToolResult(result) => result
			.content
			.iter()
			.any(|c| matches!(c, ImageOrTextContent::Image(_))),
		Message::Assistant(_) => false,
	})
}

/// Parameters of TS: `buildCopilotDynamicHeaders({ messages, hasImages })`
pub struct CopilotDynamicHeaderParams<'a> {
	pub messages: &'a [Message],
	pub has_images: bool,
}

/// TS: `buildCopilotDynamicHeaders(params)`
pub fn build_copilot_dynamic_headers(params: CopilotDynamicHeaderParams<'_>) -> IndexMap<String, String> {
	let mut headers: IndexMap<String, String> = IndexMap::new();
	headers.insert("X-Initiator".to_string(), infer_copilot_initiator(params.messages));
	headers.insert("Openai-Intent".to_string(), "conversation-edits".to_string());

	if params.has_images {
		headers.insert("Copilot-Vision-Request".to_string(), "true".to_string());
	}

	headers
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::types::{
		AssistantMessage, ContentBlock, ImageContent, TextContent, ToolResultMessage, Usage, UserMessage,
	};

	fn user_message(text: &str) -> Message {
		Message::user(UserMessage::new(UserContent::Text(text.to_string()), 0))
	}

	fn assistant_message() -> Message {
		Message::assistant(AssistantMessage {
			content: vec![ContentBlock::Text(TextContent::new("hi"))],
			usage: Usage::zero(),
			..Default::default()
		})
	}

	fn image_block() -> ImageOrTextContent {
		ImageOrTextContent::Image(ImageContent::new("AAAA", "image/png"))
	}

	#[test]
	fn initiator_is_user_for_empty_and_user_last() {
		assert_eq!(infer_copilot_initiator(&[]), "user");
		assert_eq!(infer_copilot_initiator(&[user_message("hi")]), "user");
	}

	#[test]
	fn initiator_is_agent_when_last_message_is_not_user() {
		assert_eq!(infer_copilot_initiator(&[user_message("hi"), assistant_message()]), "agent");
	}

	#[test]
	fn vision_input_detected_in_user_blocks_only() {
		let with_image = Message::user(UserMessage::new(UserContent::Blocks(vec![image_block()]), 0));
		assert!(has_copilot_vision_input(&[with_image]));
		assert!(!has_copilot_vision_input(&[user_message("text only")]));
	}

	#[test]
	fn dynamic_headers_include_vision_flag_when_images_present() {
		let headers = build_copilot_dynamic_headers(CopilotDynamicHeaderParams {
			messages: &[user_message("hi")],
			has_images: true,
		});
		assert_eq!(
			headers.keys().cloned().collect::<Vec<_>>(),
			vec!["X-Initiator", "Openai-Intent", "Copilot-Vision-Request"]
		);
		assert_eq!(headers.get("Copilot-Vision-Request").map(String::as_str), Some("true"));
	}

	#[test]
	fn dynamic_headers_omit_vision_flag_without_images() {
		let headers = build_copilot_dynamic_headers(CopilotDynamicHeaderParams {
			messages: &[],
			has_images: false,
		});
		assert_eq!(headers.keys().cloned().collect::<Vec<_>>(), vec!["X-Initiator", "Openai-Intent"]);
		assert_eq!(headers.get("Openai-Intent").map(String::as_str), Some("conversation-edits"));
	}

	#[test]
	fn tool_result_content_counts_as_vision_input() {
		let result = ToolResultMessage::new("id", "tool", vec![image_block()], false, 0);
		assert!(has_copilot_vision_input(&[Message::tool_result(result)]));
	}
}
