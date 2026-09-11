//! Port of packages/ai/src/providers/simple-options.ts
use crate::types::{
	Api, Model, ModelThinkingLevel, SimpleStreamOptions, StreamOptions, ThinkingBudgets, ThinkingLevel,
};

/// TS: `buildBaseOptions(model, options?, apiKey?)`
pub fn build_base_options(model: &Model, options: Option<&SimpleStreamOptions>, api_key: Option<&str>) -> StreamOptions {
	let mut base = StreamOptions::default();
	base.temperature = options.and_then(|o| o.stream.temperature);
	base.max_tokens = options
		.and_then(|o| o.stream.max_tokens)
		.or_else(|| if model.max_tokens > 0.0 { Some(model.max_tokens.min(32000.0)) } else { None });
	base.signal = options.and_then(|o| o.stream.signal.clone());
	base.api_key = match api_key {
		Some(key) if !key.is_empty() => Some(key.to_string()),
		_ => options.and_then(|o| o.stream.api_key.clone()),
	};
	base.transport = options.and_then(|o| o.stream.transport.clone());
	base.service_tier = options.and_then(|o| o.stream.service_tier.clone());
	base.cache_retention = options.and_then(|o| o.stream.cache_retention.clone());
	base.session_id = options.and_then(|o| o.stream.session_id.clone());
	base.headers = options.and_then(|o| o.stream.headers.clone());
	base.on_payload = options.and_then(|o| o.stream.on_payload.clone());
	base.on_response = options.and_then(|o| o.stream.on_response.clone());
	base.on_usage_observation = options.and_then(|o| o.stream.on_usage_observation.clone());
	base.timeout_ms = options.and_then(|o| o.stream.timeout_ms);
	base.metadata = options.and_then(|o| o.stream.metadata.clone());
	base
}

/// TS: `clampReasoning(effort)`
pub fn clamp_reasoning(effort: Option<&ThinkingLevel>) -> Option<ThinkingLevel> {
	match effort {
		Some(level) if level == "xhigh" || level == "max" => Some("high".to_string()),
		Some(level) => Some(level.clone()),
		None => None,
	}
}

/// Result of `adjustMaxTokensForThinking`.
#[derive(Debug, Clone, PartialEq)]
pub struct AdjustedMaxTokens {
	pub max_tokens: f64,
	pub thinking_budget: f64,
}

/// TS: `adjustMaxTokensForThinking(baseMaxTokens, modelMaxTokens, reasoningLevel, customBudgets?)`
pub fn adjust_max_tokens_for_thinking(
	base_max_tokens: f64,
	model_max_tokens: f64,
	reasoning_level: &ThinkingLevel,
	custom_budgets: Option<&ThinkingBudgets>,
) -> AdjustedMaxTokens {
	let mut budgets = ThinkingBudgets {
		minimal: Some(1024.0),
		low: Some(2048.0),
		medium: Some(8192.0),
		high: Some(16384.0),
	};
	if let Some(custom) = custom_budgets {
		if custom.minimal.is_some() {
			budgets.minimal = custom.minimal;
		}
		if custom.low.is_some() {
			budgets.low = custom.low;
		}
		if custom.medium.is_some() {
			budgets.medium = custom.medium;
		}
		if custom.high.is_some() {
			budgets.high = custom.high;
		}
	}

	let min_output_tokens = 1024.0;
	let level = clamp_reasoning(Some(reasoning_level)).expect("clamped level");
	let mut thinking_budget = match level.as_str() {
		"minimal" => budgets.minimal.unwrap_or(0.0),
		"low" => budgets.low.unwrap_or(0.0),
		"medium" => budgets.medium.unwrap_or(0.0),
		"high" => budgets.high.unwrap_or(0.0),
		_ => 0.0,
	};
	let max_tokens = (base_max_tokens + thinking_budget).min(model_max_tokens);

	if max_tokens <= thinking_budget {
		thinking_budget = (max_tokens - min_output_tokens).max(0.0);
	}

	AdjustedMaxTokens { max_tokens, thinking_budget }
}

/// Helper used by the providers: the model's thinking level map entry.
/// TS: `model.thinkingLevelMap?.[level]` (`null` marks a level as unsupported).
pub fn thinking_level_mapped(model: &Model, level: &ModelThinkingLevel) -> Option<Option<String>> {
	model.thinking_level_map.as_ref().and_then(|map| map.get(level).cloned())
}

/// Kept so the module always references `Api` the same way the TS module does.
pub fn _api_marker(_api: &Api) {}

#[cfg(test)]
mod tests {
	use super::*;

	fn model_with_max_tokens(max_tokens: f64) -> Model {
		Model {
			max_tokens,
			..Default::default()
		}
	}

	#[test]
	fn base_options_clamps_max_tokens_to_32000() {
		let model = model_with_max_tokens(100_000.0);
		let options = build_base_options(&model, None, None);
		assert_eq!(options.max_tokens, Some(32000.0));
	}

	#[test]
	fn base_options_keeps_smaller_model_max_tokens() {
		let model = model_with_max_tokens(4096.0);
		let options = build_base_options(&model, None, None);
		assert_eq!(options.max_tokens, Some(4096.0));
	}

	#[test]
	fn base_options_omits_max_tokens_when_model_reports_zero() {
		let model = model_with_max_tokens(0.0);
		let options = build_base_options(&model, None, None);
		assert_eq!(options.max_tokens, None);
	}

	#[test]
	fn base_options_prefers_explicit_api_key() {
		let model = model_with_max_tokens(1000.0);
		let mut simple = SimpleStreamOptions::default();
		simple.stream.api_key = Some("from-options".to_string());
		let options = build_base_options(&model, Some(&simple), Some("explicit"));
		assert_eq!(options.api_key.as_deref(), Some("explicit"));
		let options = build_base_options(&model, Some(&simple), None);
		assert_eq!(options.api_key.as_deref(), Some("from-options"));
	}

	#[test]
	fn clamp_reasoning_maps_xhigh_and_max_to_high() {
		assert_eq!(clamp_reasoning(Some(&"xhigh".to_string())), Some("high".to_string()));
		assert_eq!(clamp_reasoning(Some(&"max".to_string())), Some("high".to_string()));
		assert_eq!(clamp_reasoning(Some(&"low".to_string())), Some("low".to_string()));
		assert_eq!(clamp_reasoning(None), None);
	}

	#[test]
	fn adjust_max_tokens_uses_default_budgets() {
		let adjusted = adjust_max_tokens_for_thinking(8000.0, 64000.0, &"medium".to_string(), None);
		assert_eq!(adjusted.max_tokens, 16192.0);
		assert_eq!(adjusted.thinking_budget, 8192.0);
	}

	#[test]
	fn adjust_max_tokens_shrinks_budget_when_model_limit_hits() {
		let adjusted = adjust_max_tokens_for_thinking(1000.0, 4000.0, &"high".to_string(), None);
		assert_eq!(adjusted.max_tokens, 4000.0);
		assert_eq!(adjusted.thinking_budget, 2976.0);
	}

	#[test]
	fn adjust_max_tokens_respects_custom_budgets() {
		let custom = ThinkingBudgets {
			minimal: Some(10.0),
			low: Some(20.0),
			medium: Some(30.0),
			high: Some(40.0),
		};
		let adjusted = adjust_max_tokens_for_thinking(100.0, 100_000.0, &"low".to_string(), Some(&custom));
		assert_eq!(adjusted.max_tokens, 120.0);
		assert_eq!(adjusted.thinking_budget, 20.0);
	}

	#[test]
	fn thinking_level_map_distinguishes_absent_from_null() {
		let mut model = Model::default();
		model.thinking_level_map = Some(
			[("high".to_string(), Some("high-value".to_string())), ("max".to_string(), None)]
				.into_iter()
				.collect(),
		);
		assert_eq!(thinking_level_mapped(&model, &"high".to_string()), Some(Some("high-value".to_string())));
		assert_eq!(thinking_level_mapped(&model, &"max".to_string()), Some(None));
		assert_eq!(thinking_level_mapped(&model, &"low".to_string()), None);
	}
}
