//! Port of packages/ai/src/providers/google-shared.ts
//!
//! Shared utilities for Google Generative AI and Vertex providers.

use serde_json::{Map, Value};

use crate::providers::transform_messages::try_transform_messages;
use crate::types::{Context, ImageOrTextContent, InputModality, Model, StopReason, TextContent, ThinkingBudgets, Tool};
use crate::utils::sanitize_unicode::sanitize_surrogates;

/// TS: `type GoogleApiType = "google-generative-ai" | "google-vertex"`.
pub type GoogleApiType = String;
pub const GOOGLE_API_TYPE_GENERATIVE_AI: &str = "google-generative-ai";
pub const GOOGLE_API_TYPE_VERTEX: &str = "google-vertex";

/// Thinking level values accepted by Gemini 3 models.
pub type GoogleThinkingLevel = String;
pub const THINKING_LEVEL_UNSPECIFIED: &str = "THINKING_LEVEL_UNSPECIFIED";
pub const THINKING_LEVEL_MINIMAL: &str = "MINIMAL";
pub const THINKING_LEVEL_LOW: &str = "LOW";
pub const THINKING_LEVEL_MEDIUM: &str = "MEDIUM";
pub const THINKING_LEVEL_HIGH: &str = "HIGH";

/// TS: `type GoogleBudgetThinkingLevel = "minimal" | "low" | "medium" | "high"`.
pub type GoogleBudgetThinkingLevel = String;

/// TS: `getGoogleThinkingBudget(modelId, effort, customBudgets?)`.
pub fn get_google_thinking_budget(
	model_id: &str,
	effort: &GoogleBudgetThinkingLevel,
	custom_budgets: Option<&ThinkingBudgets>,
) -> f64 {
	if let Some(budget) = custom_budgets.and_then(|budgets| thinking_budget_for_level(budgets, effort)) {
		return budget;
	}

	if model_id.contains("2.5-pro") {
		return budget_for(&[("minimal", 128.0), ("low", 2048.0), ("medium", 8192.0), ("high", 32768.0)], effort);
	}

	if model_id.contains("2.5-flash-lite") {
		return budget_for(&[("minimal", 512.0), ("low", 2048.0), ("medium", 8192.0), ("high", 24576.0)], effort);
	}

	if model_id.contains("2.5-flash") {
		return budget_for(&[("minimal", 128.0), ("low", 2048.0), ("medium", 8192.0), ("high", 24576.0)], effort);
	}

	-1.0
}

/// TS: `customBudgets?.[effort] !== undefined` - only the four budget levels.
fn thinking_budget_for_level(budgets: &ThinkingBudgets, effort: &str) -> Option<f64> {
	match effort {
		"minimal" => budgets.minimal,
		"low" => budgets.low,
		"medium" => budgets.medium,
		"high" => budgets.high,
		_ => None,
	}
}

fn budget_for(table: &[(&str, f64)], effort: &str) -> f64 {
	table
		.iter()
		.find(|(level, _)| *level == effort)
		.map(|(_, budget)| *budget)
		.unwrap_or(0.0)
}

/// Determines whether a streamed Gemini `Part` should be treated as "thinking".
///
/// Protocol note (Gemini / Vertex AI thought signatures):
/// - `thought: true` is the definitive marker for thinking content (thought summaries).
/// - `thoughtSignature` is an encrypted representation of the model's internal thought process
///   used to preserve reasoning context across multi-turn interactions.
/// - `thoughtSignature` can appear on ANY part type (text, functionCall, etc.) - it does NOT
///   indicate the part itself is thinking content.
/// - For non-functionCall responses, the signature appears on the last part for context replay.
/// - When persisting/replaying model outputs, signature-bearing parts must be preserved as-is;
///   do not merge/move signatures across parts.
///
/// See: https://ai.google.dev/gemini-api/docs/thought-signatures
///
/// TS: `isThinkingPart(part: Pick<Part, "thought" | "thoughtSignature">)`.
pub fn is_thinking_part(part: &Value) -> bool {
	part.get("thought").and_then(Value::as_bool) == Some(true)
}

/// Retain thought signatures during streaming.
///
/// Some backends only send `thoughtSignature` on the first delta for a given part/block; later deltas may omit it.
/// This helper preserves the last non-empty signature for the current block.
///
/// Note: this does NOT merge or move signatures across distinct response parts. It only prevents
/// a signature from being overwritten with `undefined` within the same streamed block.
///
/// TS: `retainThoughtSignature(existing, incoming)`.
pub fn retain_thought_signature(existing: Option<&str>, incoming: Option<&str>) -> Option<String> {
	if let Some(incoming) = incoming {
		if !incoming.is_empty() {
			return Some(incoming.to_string());
		}
	}
	existing.map(str::to_string)
}

/// Thought signatures must be base64 for Google APIs (TYPE_BYTES).
/// TS: `const base64SignaturePattern = /^[A-Za-z0-9+/]+={0,2}$/`.
fn is_base64_signature_pattern(signature: &str) -> bool {
	let bytes = signature.as_bytes();
	let mut padding = 0usize;
	while padding < bytes.len() && bytes[bytes.len() - 1 - padding] == b'=' {
		padding += 1;
	}
	if padding > 2 {
		return false;
	}
	let body = &signature[..signature.len() - padding];
	if body.is_empty() {
		return false;
	}
	body.bytes()
		.all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
}

/// TS: `isValidThoughtSignature(signature)`.
fn is_valid_thought_signature(signature: Option<&str>) -> bool {
	let Some(signature) = signature else {
		return false;
	};
	if signature.is_empty() {
		return false;
	}
	if signature.len() % 4 != 0 {
		return false;
	}
	is_base64_signature_pattern(signature)
}

/// Retains a thought signature only for the originating provider/model and when it is valid base64.
/// TS: `resolveThoughtSignature(isSameProviderAndModel, signature)`.
fn resolve_thought_signature(is_same_provider_and_model: bool, signature: Option<&str>) -> Option<String> {
	if is_same_provider_and_model && is_valid_thought_signature(signature) {
		signature.map(str::to_string)
	} else {
		None
	}
}

/// Whether this Google API model requires tool-call IDs on function calls and responses.
/// TS: `requiresToolCallId(modelId)`.
pub fn requires_tool_call_id(model_id: &str) -> bool {
	model_id.starts_with("claude-") || model_id.starts_with("gpt-oss-")
}

/// TS: `getGeminiMajorVersion(modelId)` - `undefined` becomes `None`.
fn get_gemini_major_version(model_id: &str) -> Option<i64> {
	let lower = model_id.to_lowercase();
	let rest = lower.strip_prefix("gemini")?;
	let rest = rest.strip_prefix("-live").unwrap_or(rest);
	let rest = rest.strip_prefix('-')?;
	let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
	if digits.is_empty() {
		return None;
	}
	digits.parse::<i64>().ok()
}

/// TS: `supportsMultimodalFunctionResponse(modelId)`.
fn supports_multimodal_function_response(model_id: &str) -> bool {
	match get_gemini_major_version(model_id) {
		Some(major) => major >= 3,
		None => true,
	}
}


/// Converts internal context to Google `Content[]`, preserving replayable signatures only when protocol-valid.
///
/// TS: `convertMessages<T extends GoogleApiType>(model, context)`.
///
/// `transformMessages` can throw (`Compaction checkpoint belongs to another model or
/// provider; ...`), so the port returns `Result` and the caller routes the message
/// through the same catch path as the TypeScript.
pub fn convert_messages(model: &Model, context: &Context) -> Result<Vec<Value>, String> {
	let mut contents: Vec<Value> = Vec::new();
	let model_id = model.id.clone();
	let normalize = move |id: &str, _model: &Model, _source: &crate::types::AssistantMessage| {
		if !requires_tool_call_id(&model_id) {
			return id.to_string();
		}
		let replaced: String = id
			.chars()
			.map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
			.collect();
		replaced.chars().take(64).collect()
	};

	let transformed_messages = try_transform_messages(context.messages.clone(), model, Some(&normalize))?;

	for msg in transformed_messages {
		match msg {
			crate::types::Message::User(user) => match &user.content {
				crate::types::UserContent::Text(text) => {
					contents.push(content_value(
						"user",
						vec![part_text(&sanitize_surrogates(text))],
					));
				}
				crate::types::UserContent::Blocks(blocks) => {
					let parts: Vec<Value> = blocks
						.iter()
						.map(|item| match item {
							ImageOrTextContent::Text(text) => part_text(&sanitize_surrogates(&text.text)),
							ImageOrTextContent::Image(image) => part_inline_data(&image.mime_type, &image.data),
						})
						.collect();
					if parts.is_empty() {
						continue;
					}
					contents.push(content_value("user", parts));
				}
			},
			crate::types::Message::Assistant(assistant_msg) => {
				let mut parts: Vec<Value> = Vec::new();
				let is_same_provider_and_model =
					assistant_msg.provider == model.provider && assistant_msg.model == model.id;

				for block in assistant_msg.content.iter() {
					match block {
						crate::types::ContentBlock::Text(text) => {
							if text.text.is_empty() || text.text.trim().is_empty() {
								continue;
							}
							let thought_signature = resolve_thought_signature(
								is_same_provider_and_model,
								text.text_signature.as_deref(),
							);
							let mut part = Map::new();
							part.insert("text".to_string(), Value::String(sanitize_surrogates(&text.text)));
							if let Some(thought_signature) = thought_signature {
								part.insert("thoughtSignature".to_string(), Value::String(thought_signature));
							}
							parts.push(Value::Object(part));
						}
						crate::types::ContentBlock::Thinking(thinking) => {
							if thinking.thinking.is_empty() || thinking.thinking.trim().is_empty() {
								continue;
							}
							// Only keep as thinking block if same provider AND same model
							// Otherwise convert to plain text (no tags to avoid model mimicking them)
							if is_same_provider_and_model {
								let thought_signature = resolve_thought_signature(
									is_same_provider_and_model,
									thinking.thinking_signature.as_deref(),
								);
								let mut part = Map::new();
								part.insert("thought".to_string(), Value::Bool(true));
								part.insert(
									"text".to_string(),
									Value::String(sanitize_surrogates(&thinking.thinking)),
								);
								if let Some(thought_signature) = thought_signature {
									part.insert("thoughtSignature".to_string(), Value::String(thought_signature));
								}
								parts.push(Value::Object(part));
							} else {
								parts.push(part_text(&sanitize_surrogates(&thinking.thinking)));
							}
						}
						crate::types::ContentBlock::ToolCall(tool_call) => {
							let thought_signature = resolve_thought_signature(
								is_same_provider_and_model,
								tool_call.thought_signature.as_deref(),
							);
							let mut function_call = Map::new();
							function_call.insert("name".to_string(), Value::String(tool_call.name.clone()));
							function_call
								.insert("args".to_string(), Value::Object(tool_call.arguments.clone()));
							if requires_tool_call_id(&model.id) {
								function_call.insert("id".to_string(), Value::String(tool_call.id.clone()));
							}
							let mut part = Map::new();
							part.insert("functionCall".to_string(), Value::Object(function_call));
							if let Some(thought_signature) = thought_signature {
								part.insert("thoughtSignature".to_string(), Value::String(thought_signature));
							}
							parts.push(Value::Object(part));
						}
					}
				}

				if parts.is_empty() {
					continue;
				}
				contents.push(content_value("model", parts));
			}
			crate::types::Message::ToolResult(result_msg) => {
				let text_content: Vec<&TextContent> = result_msg
					.content
					.iter()
					.filter_map(|c| match c {
						ImageOrTextContent::Text(text) => Some(text),
						ImageOrTextContent::Image(_) => None,
					})
					.collect();
				let text_result = text_content
					.iter()
					.map(|c| c.text.clone())
					.collect::<Vec<_>>()
					.join("\n");
				let image_content: Vec<&crate::types::ImageContent> = if model
					.input
					.iter()
					.any(|m| matches!(m, InputModality::Image))
				{
					result_msg
						.content
						.iter()
						.filter_map(|c| match c {
							ImageOrTextContent::Image(image) => Some(image),
							ImageOrTextContent::Text(_) => None,
						})
						.collect()
				} else {
					Vec::new()
				};

				let has_text = !text_result.is_empty();
				let has_images = !image_content.is_empty();

				// Gemini 3+ models support multimodal function responses with images nested inside
				// functionResponse.parts. Claude and other non-Gemini models behind Cloud Code Assist /
				// Gemini < 3 still needs a separate user image turn.
				let model_supports_multimodal_function_response = supports_multimodal_function_response(&model.id);

				// Use "output" key for success, "error" key for errors as per SDK documentation
				let response_value = if has_text {
					sanitize_surrogates(&text_result)
				} else if has_images {
					"(see attached image)".to_string()
				} else {
					String::new()
				};

				let image_parts: Vec<Value> = image_content
					.iter()
					.map(|image_block| part_inline_data(&image_block.mime_type, &image_block.data))
					.collect();

				let include_id = requires_tool_call_id(&model.id);
				let mut response = Map::new();
				let mut response_payload = Map::new();
				if result_msg.is_error {
					response_payload.insert("error".to_string(), Value::String(response_value));
				} else {
					response_payload.insert("output".to_string(), Value::String(response_value));
				}
				response.insert("name".to_string(), Value::String(result_msg.tool_name.clone()));
				response.insert("response".to_string(), Value::Object(response_payload));
				if has_images && model_supports_multimodal_function_response {
					response.insert("parts".to_string(), Value::Array(image_parts.clone()));
				}
				if include_id {
					response.insert("id".to_string(), Value::String(result_msg.tool_call_id.clone()));
				}
				let mut function_response_part = Map::new();
				function_response_part.insert("functionResponse".to_string(), Value::Object(response));
				let function_response_part = Value::Object(function_response_part);

				// Cloud Code Assist API requires all function responses to be in a single user turn.
				// Check if the last content is already a user turn with function responses and merge.
				let merge_into_last = contents.last().map_or(false, |last_content| {
					last_content.get("role").and_then(Value::as_str) == Some("user")
						&& last_content
							.get("parts")
							.and_then(Value::as_array)
							.map_or(false, |parts| parts.iter().any(|p| p.get("functionResponse").is_some()))
				});
				if merge_into_last {
					if let Some(last_content) = contents.last_mut() {
						if let Some(parts) = last_content.get_mut("parts").and_then(Value::as_array_mut) {
							parts.push(function_response_part);
						}
					}
				} else {
					contents.push(content_value("user", vec![function_response_part]));
				}

				// For Gemini < 3, add images in a separate user message
				if has_images && !model_supports_multimodal_function_response {
					let mut parts: Vec<Value> = vec![part_text("Tool result image:")];
					parts.extend(image_parts);
					contents.push(content_value("user", parts));
				}
			}
		}
	}

	Ok(contents)
}

/// TS: `{ role, parts }` object literal - key order is observable.
fn content_value(role: &str, parts: Vec<Value>) -> Value {
	let mut content = Map::new();
	content.insert("role".to_string(), Value::String(role.to_string()));
	content.insert("parts".to_string(), Value::Array(parts));
	Value::Object(content)
}

/// TS: `{ text }`.
fn part_text(text: &str) -> Value {
	let mut part = Map::new();
	part.insert("text".to_string(), Value::String(text.to_string()));
	Value::Object(part)
}

/// TS: `{ inlineData: { mimeType, data } }`.
fn part_inline_data(mime_type: &str, data: &str) -> Value {
	let mut inline_data = Map::new();
	inline_data.insert("mimeType".to_string(), Value::String(mime_type.to_string()));
	inline_data.insert("data".to_string(), Value::String(data.to_string()));
	let mut part = Map::new();
	part.insert("inlineData".to_string(), Value::Object(inline_data));
	Value::Object(part)
}

/// TS: `JSON_SCHEMA_META_DECLARATIONS`.
pub const JSON_SCHEMA_META_DECLARATIONS: [&str; 8] = [
	"$schema",
	"$id",
	"$anchor",
	"$dynamicAnchor",
	"$vocabulary",
	"$comment",
	"$defs",
	// pre-draft-2019-09 equivalent of $defs
	"definitions",
];

/// TS: `sanitizeForOpenApi(schema)`.
fn sanitize_for_open_api(schema: &Value) -> Value {
	match schema {
		Value::Object(object) => {
			let mut result = Map::new();
			for (key, value) in object {
				if JSON_SCHEMA_META_DECLARATIONS.contains(&key.as_str()) {
					continue;
				}
				result.insert(key.clone(), sanitize_for_open_api(value));
			}
			Value::Object(result)
		}
		other => other.clone(),
	}
}

/// Convert tools to Gemini function declarations format.
///
/// By default uses `parametersJsonSchema` which supports full JSON Schema (including
/// anyOf, oneOf, const, etc.). Set `useParameters` to true to use the legacy `parameters`
/// field instead (OpenAPI 3.03 Schema). This is needed for Cloud Code Assist with Claude
/// models, where the API translates `parameters` into Anthropic's `input_schema`.
///
/// TS: `convertTools(tools, useParameters = false)`.
pub fn convert_tools(tools: &[Tool], use_parameters: bool) -> Option<Vec<Value>> {
	if tools.is_empty() {
		return None;
	}
	let function_declarations: Vec<Value> = tools
		.iter()
		.map(|tool| {
			let mut declaration = Map::new();
			declaration.insert("name".to_string(), Value::String(tool.name.clone()));
			declaration.insert("description".to_string(), Value::String(tool.description.clone()));
			if use_parameters {
				declaration.insert("parameters".to_string(), sanitize_for_open_api(&tool.parameters));
			} else {
				declaration.insert("parametersJsonSchema".to_string(), tool.parameters.clone());
			}
			Value::Object(declaration)
		})
		.collect();
	let mut group = Map::new();
	group.insert("functionDeclarations".to_string(), Value::Array(function_declarations));
	Some(vec![Value::Object(group)])
}

/// TS: `FunctionCallingConfigMode` enum values.
pub const FUNCTION_CALLING_CONFIG_MODE_UNSPECIFIED: &str = "MODE_UNSPECIFIED";
pub const FUNCTION_CALLING_CONFIG_MODE_AUTO: &str = "AUTO";
pub const FUNCTION_CALLING_CONFIG_MODE_ANY: &str = "ANY";
pub const FUNCTION_CALLING_CONFIG_MODE_NONE: &str = "NONE";

/// Converts the generic tool-choice mode to Google's function-calling mode.
/// TS: `mapToolChoice(choice)`.
pub fn map_tool_choice(choice: &str) -> String {
	match choice {
		"auto" => FUNCTION_CALLING_CONFIG_MODE_AUTO.to_string(),
		"none" => FUNCTION_CALLING_CONFIG_MODE_NONE.to_string(),
		"any" => FUNCTION_CALLING_CONFIG_MODE_ANY.to_string(),
		_ => FUNCTION_CALLING_CONFIG_MODE_AUTO.to_string(),
	}
}

/// TS: `FinishReason` enum values.
pub const FINISH_REASON_STOP: &str = "STOP";
pub const FINISH_REASON_MAX_TOKENS: &str = "MAX_TOKENS";
pub const FINISH_REASON_BLOCKLIST: &str = "BLOCKLIST";
pub const FINISH_REASON_PROHIBITED_CONTENT: &str = "PROHIBITED_CONTENT";
pub const FINISH_REASON_SPII: &str = "SPII";
pub const FINISH_REASON_SAFETY: &str = "SAFETY";
pub const FINISH_REASON_IMAGE_SAFETY: &str = "IMAGE_SAFETY";
pub const FINISH_REASON_IMAGE_PROHIBITED_CONTENT: &str = "IMAGE_PROHIBITED_CONTENT";
pub const FINISH_REASON_IMAGE_RECITATION: &str = "IMAGE_RECITATION";
pub const FINISH_REASON_IMAGE_OTHER: &str = "IMAGE_OTHER";
pub const FINISH_REASON_RECITATION: &str = "RECITATION";
pub const FINISH_REASON_FINISH_REASON_UNSPECIFIED: &str = "FINISH_REASON_UNSPECIFIED";
pub const FINISH_REASON_OTHER: &str = "OTHER";
pub const FINISH_REASON_LANGUAGE: &str = "LANGUAGE";
pub const FINISH_REASON_MALFORMED_FUNCTION_CALL: &str = "MALFORMED_FUNCTION_CALL";
pub const FINISH_REASON_UNEXPECTED_TOOL_CALL: &str = "UNEXPECTED_TOOL_CALL";
pub const FINISH_REASON_NO_IMAGE: &str = "NO_IMAGE";

/// Converts Google finish reasons to the shared stop-reason protocol.
///
/// TS: `mapStopReason(reason: FinishReason)` - the exhaustive `never` branch throws
/// `Unhandled stop reason: ${reason}`, so the port returns `Result`.
pub fn map_stop_reason(reason: &str) -> Result<StopReason, String> {
	match reason {
		FINISH_REASON_STOP => Ok("stop".to_string()),
		FINISH_REASON_MAX_TOKENS => Ok("length".to_string()),
		FINISH_REASON_BLOCKLIST
		| FINISH_REASON_PROHIBITED_CONTENT
		| FINISH_REASON_SPII
		| FINISH_REASON_SAFETY
		| FINISH_REASON_IMAGE_SAFETY
		| FINISH_REASON_IMAGE_PROHIBITED_CONTENT
		| FINISH_REASON_IMAGE_RECITATION
		| FINISH_REASON_IMAGE_OTHER
		| FINISH_REASON_RECITATION
		| FINISH_REASON_FINISH_REASON_UNSPECIFIED
		| FINISH_REASON_OTHER
		| FINISH_REASON_LANGUAGE
		| FINISH_REASON_MALFORMED_FUNCTION_CALL
		| FINISH_REASON_UNEXPECTED_TOOL_CALL
		| FINISH_REASON_NO_IMAGE => Ok("error".to_string()),
		other => Err(format!("Unhandled stop reason: {}", other)),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::types::{
		AssistantMessage, ContentBlock, ImageContent, Message, ModelCost, ThinkingContent, ToolCall, UserContent,
		UserMessage,
	};
	use serde_json::json;

	fn model(id: &str, provider: &str) -> Model {
		let mut model = Model::new(id, id, GOOGLE_API_TYPE_GENERATIVE_AI, provider, "");
		model.input = vec![InputModality::Text, InputModality::Image];
		model.cost = ModelCost::zero();
		model
	}

	fn assistant(model: &Model, content: Vec<ContentBlock>) -> Message {
		Message::assistant(AssistantMessage {
			content,
			api: model.api.clone(),
			provider: model.provider.clone(),
			model: model.id.clone(),
			stop_reason: "stop".to_string(),
			..Default::default()
		})
	}

	#[test]
	fn thinking_budget_uses_custom_budgets_first() {
		let budgets = ThinkingBudgets {
			minimal: Some(7.0),
			low: None,
			medium: None,
			high: None,
		};
		assert_eq!(
			get_google_thinking_budget("gemini-2.5-pro", &"minimal".to_string(), Some(&budgets)),
			7.0
		);
	}

	#[test]
	fn thinking_budget_matches_model_tables() {
		assert_eq!(get_google_thinking_budget("gemini-2.5-pro", &"high".to_string(), None), 32768.0);
		assert_eq!(get_google_thinking_budget("gemini-2.5-pro", &"minimal".to_string(), None), 128.0);
		assert_eq!(get_google_thinking_budget("gemini-2.5-flash-lite", &"minimal".to_string(), None), 512.0);
		assert_eq!(get_google_thinking_budget("gemini-2.5-flash-lite", &"high".to_string(), None), 24576.0);
		assert_eq!(get_google_thinking_budget("gemini-2.5-flash", &"minimal".to_string(), None), 128.0);
		assert_eq!(get_google_thinking_budget("gemini-2.5-flash", &"medium".to_string(), None), 8192.0);
		assert_eq!(get_google_thinking_budget("gemini-2.0-flash", &"high".to_string(), None), -1.0);
	}

	#[test]
	fn is_thinking_part_requires_thought_true() {
		assert!(is_thinking_part(&json!({"thought": true})));
		assert!(!is_thinking_part(&json!({"thought": false})));
		assert!(!is_thinking_part(&json!({"thoughtSignature": "AAAA"})));
		assert!(!is_thinking_part(&json!({})));
	}

	#[test]
	fn retain_thought_signature_keeps_existing_on_empty_incoming() {
		assert_eq!(retain_thought_signature(Some("a"), Some("b")), Some("b".to_string()));
		assert_eq!(retain_thought_signature(Some("a"), Some("")), Some("a".to_string()));
		assert_eq!(retain_thought_signature(Some("a"), None), Some("a".to_string()));
		assert_eq!(retain_thought_signature(None, None), None);
	}

	#[test]
	fn requires_tool_call_id_only_for_claude_and_gpt_oss() {
		assert!(requires_tool_call_id("claude-sonnet-4"));
		assert!(requires_tool_call_id("gpt-oss-120b"));
		assert!(!requires_tool_call_id("gemini-3-pro"));
	}

	#[test]
	fn map_stop_reason_maps_known_values_and_errors_otherwise() {
		assert_eq!(map_stop_reason(FINISH_REASON_STOP).unwrap(), "stop");
		assert_eq!(map_stop_reason(FINISH_REASON_MAX_TOKENS).unwrap(), "length");
		assert_eq!(map_stop_reason(FINISH_REASON_SAFETY).unwrap(), "error");
		assert_eq!(map_stop_reason(FINISH_REASON_NO_IMAGE).unwrap(), "error");
		assert_eq!(map_stop_reason("WAT").unwrap_err(), "Unhandled stop reason: WAT");
	}

	#[test]
	fn map_tool_choice_defaults_to_auto() {
		assert_eq!(map_tool_choice("auto"), "AUTO");
		assert_eq!(map_tool_choice("none"), "NONE");
		assert_eq!(map_tool_choice("any"), "ANY");
		assert_eq!(map_tool_choice("other"), "AUTO");
	}

	#[test]
	fn convert_tools_returns_undefined_for_no_tools() {
		assert_eq!(convert_tools(&[], false), None);
	}

	#[test]
	fn convert_tools_uses_json_schema_by_default_and_parameters_on_request() {
		let tool = Tool {
			name: "read".to_string(),
			description: "Read a file".to_string(),
			parameters: json!({"$schema": "x", "type": "object", "properties": {"$defs": {"a": 1}}}),
		};
		let default = convert_tools(std::slice::from_ref(&tool), false).unwrap();
		assert_eq!(
			default[0]["functionDeclarations"][0],
			json!({
				"name": "read",
				"description": "Read a file",
				"parametersJsonSchema": {"$schema": "x", "type": "object", "properties": {"$defs": {"a": 1}}}
			})
		);

		let legacy = convert_tools(std::slice::from_ref(&tool), true).unwrap();
		assert_eq!(
			legacy[0]["functionDeclarations"][0],
			json!({
				"name": "read",
				"description": "Read a file",
				"parameters": {"type": "object", "properties": {}}
			})
		);
	}

	#[test]
	fn convert_messages_maps_user_text_and_images() {
		let model = model("gemini-3-pro", "google");
		let context = Context::new(
			None,
			vec![Message::user(UserMessage::new(
				UserContent::Blocks(vec![
					ImageOrTextContent::Text(TextContent::new("look")),
					ImageOrTextContent::Image(ImageContent::new("AAAA", "image/png")),
				]),
				0,
			))],
			None,
		);
		let contents = convert_messages(&model, &context).unwrap();
		assert_eq!(
			contents[0],
			json!({"role": "user", "parts": [{"text": "look"}, {"inlineData": {"mimeType": "image/png", "data": "AAAA"}}]})
		);
	}

	#[test]
	fn convert_messages_skips_empty_user_block_list() {
		let model = model("gemini-3-pro", "google");
		let context = Context::new(
			None,
			vec![Message::user(UserMessage::new(UserContent::Blocks(Vec::new()), 0))],
			None,
		);
		assert!(convert_messages(&model, &context).unwrap().is_empty());
	}

	#[test]
	fn convert_messages_maps_assistant_text_thinking_and_tool_calls() {
		let model = model("gemini-3-pro", "google");
		let mut text = TextContent::new("hello");
		text.text_signature = Some("QUJD".to_string());
		let mut thinking = ThinkingContent::new("hmm");
		thinking.thinking_signature = Some("QUJD".to_string());
		let mut tool_call = ToolCall::new("call-1", "read", json!({"path": "a"}).as_object().unwrap().clone());
		tool_call.thought_signature = Some("QUJD".to_string());
		let context = Context::new(
			None,
			vec![assistant(
				&model,
				vec![
					ContentBlock::Text(text),
					ContentBlock::Thinking(thinking),
					ContentBlock::ToolCall(tool_call),
				],
			)],
			None,
		);
		let contents = convert_messages(&model, &context).unwrap();
		assert_eq!(
			contents[0],
			json!({
				"role": "model",
				"parts": [
					{"text": "hello", "thoughtSignature": "QUJD"},
					{"thought": true, "text": "hmm", "thoughtSignature": "QUJD"},
					{"functionCall": {"name": "read", "args": {"path": "a"}}, "thoughtSignature": "QUJD"}
				]
			})
		);
	}

	#[test]
	fn convert_messages_drops_invalid_signatures_and_blank_blocks() {
		let model = model("gemini-3-pro", "google");
		let mut text = TextContent::new("   ");
		text.text_signature = Some("not-base64!".to_string());
		let mut thinking = ThinkingContent::new("keep");
		thinking.thinking_signature = Some("QUJD".to_string());
		let other_model = self::model("gemini-3-flash", "google");
		let context = Context::new(
			None,
			vec![assistant(
				&other_model,
				vec![ContentBlock::Text(text), ContentBlock::Thinking(thinking)],
			)],
			None,
		);
		let contents = convert_messages(&model, &context).unwrap();
		assert_eq!(contents[0], json!({"role": "model", "parts": [{"text": "keep"}]}));
	}

	#[test]
	fn convert_messages_adds_tool_call_id_for_claude_models() {
		let model = model("claude-sonnet-4", "google");
		let tool_call = ToolCall::new("call:1|2", "read", Map::new());
		let context = Context::new(
			None,
			vec![assistant(&model, vec![ContentBlock::ToolCall(tool_call)])],
			None,
		);
		let contents = convert_messages(&model, &context).unwrap();
		assert_eq!(
			contents[0],
			json!({"role": "model", "parts": [{"functionCall": {"name": "read", "args": {}, "id": "call:1|2"}}]})
		);
	}

	#[test]
	fn convert_messages_merges_function_responses_into_one_user_turn() {
		let model = model("gemini-3-pro", "google");
		let result_one = crate::types::ToolResultMessage::new(
			"call-1",
			"read",
			vec![ImageOrTextContent::Text(TextContent::new("one"))],
			false,
			0,
		);
		let result_two = crate::types::ToolResultMessage::new(
			"call-2",
			"read",
			vec![ImageOrTextContent::Text(TextContent::new("two"))],
			true,
			0,
		);
		let context = Context::new(
			None,
			vec![Message::tool_result(result_one), Message::tool_result(result_two)],
			None,
		);
		let contents = convert_messages(&model, &context).unwrap();
		assert_eq!(contents.len(), 1);
		assert_eq!(
			contents[0],
			json!({
				"role": "user",
				"parts": [
					{"functionResponse": {"name": "read", "response": {"output": "one"}}},
					{"functionResponse": {"name": "read", "response": {"error": "two"}}}
				]
			})
		);
	}

	#[test]
	fn convert_messages_nests_images_for_gemini_three_and_splits_for_gemini_two() {
		let result = crate::types::ToolResultMessage::new(
			"call-1",
			"read",
			vec![ImageOrTextContent::Image(ImageContent::new("AAAA", "image/png"))],
			false,
			0,
		);
		let gemini_three = model("gemini-3-pro", "google");
		let contents = convert_messages(
			&gemini_three,
			&Context::new(None, vec![Message::tool_result(result.clone())], None),
		)
		.unwrap();
		assert_eq!(
			contents[0],
			json!({
				"role": "user",
				"parts": [{
					"functionResponse": {
						"name": "read",
						"response": {"output": "(see attached image)"},
						"parts": [{"inlineData": {"mimeType": "image/png", "data": "AAAA"}}]
					}
				}]
			})
		);

		let gemini_two = model("gemini-2.5-flash", "google");
		let contents = convert_messages(
			&gemini_two,
			&Context::new(None, vec![Message::tool_result(result)], None),
		)
		.unwrap();
		assert_eq!(contents.len(), 2);
		assert_eq!(
			contents[1],
			json!({
				"role": "user",
				"parts": [
					{"text": "Tool result image:"},
					{"inlineData": {"mimeType": "image/png", "data": "AAAA"}}
				]
			})
		);
	}

	#[test]
	fn convert_messages_omits_images_when_model_has_no_image_input() {
		let mut model = model("gemini-3-pro", "google");
		model.input = vec![InputModality::Text];
		let result = crate::types::ToolResultMessage::new(
			"call-1",
			"read",
			vec![ImageOrTextContent::Image(ImageContent::new("AAAA", "image/png"))],
			false,
			0,
		);
		let contents = convert_messages(&model, &Context::new(None, vec![Message::tool_result(result)], None)).unwrap();
		assert_eq!(
			contents[0],
			json!({"role": "user", "parts": [{"functionResponse": {"name": "read", "response": {"output": "(see attached image)"}}}]})
		);
	}
}
