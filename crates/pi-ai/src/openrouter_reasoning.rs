//! Port of packages/ai/src/openrouter-reasoning.ts

use serde_json::Value;

use crate::types::{ThinkingLevelMap, THINKING_LEVELS};

#[derive(Debug, Clone, PartialEq)]
pub struct OpenRouterReasoningCapabilities {
    /// Exact local-to-provider effort map. null entries are unsupported.
    pub thinking_level_map: Option<ThinkingLevelMap>,
    /// Whether the model exposes effort selection, rather than only an enabled toggle.
    pub supports_reasoning_effort: bool,
    /// Whether the model rejects attempts to disable reasoning.
    pub mandatory: bool,
}

fn is_record(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

/// The model supports reasoning as an on/off capability, but does not expose
/// effort selection. Represent that as one generic active level in the UI.
fn enabled_only_capabilities(mandatory: bool) -> OpenRouterReasoningCapabilities {
    let mut thinking_level_map: ThinkingLevelMap = ThinkingLevelMap::new();
    if mandatory {
        thinking_level_map.insert("off".to_string(), None);
    }
    thinking_level_map.insert("minimal".to_string(), None);
    thinking_level_map.insert("low".to_string(), None);
    thinking_level_map.insert("medium".to_string(), None);
    thinking_level_map.insert("high".to_string(), Some("high".to_string()));
    thinking_level_map.insert("xhigh".to_string(), None);
    thinking_level_map.insert("max".to_string(), None);
    OpenRouterReasoningCapabilities {
        thinking_level_map: Some(thinking_level_map),
        supports_reasoning_effort: false,
        mandatory,
    }
}

/// `getOpenRouterReasoningCapabilities(model: unknown)`.
pub fn get_openrouter_reasoning_capabilities(
    model: &Value,
) -> Option<OpenRouterReasoningCapabilities> {
    if !is_record(model) {
        return None;
    }
    let object = model.as_object()?;
    let supported_parameters: Vec<&Value> = object
        .get("supported_parameters")
        .and_then(Value::as_array)
        .map(|values| values.iter().collect())
        .unwrap_or_default();
    if !supported_parameters
        .iter()
        .any(|parameter| parameter.as_str() == Some("reasoning"))
    {
        return None;
    }
    let reasoning = object.get("reasoning").filter(|value| is_record(value))?;
    let reasoning = reasoning.as_object()?;

    let mandatory = reasoning.get("mandatory").and_then(Value::as_bool) == Some(true);
    let raw_efforts = reasoning.get("supported_efforts");
    if matches!(raw_efforts, Some(Value::Null)) {
        let mut thinking_level_map: ThinkingLevelMap = ThinkingLevelMap::new();
        if mandatory {
            thinking_level_map.insert("off".to_string(), None);
        }
        for level in THINKING_LEVELS {
            thinking_level_map.insert(level.to_string(), Some(level.to_string()));
        }
        return Some(OpenRouterReasoningCapabilities {
            thinking_level_map: Some(thinking_level_map),
            supports_reasoning_effort: true,
            mandatory,
        });
    }

    if let Some(efforts) = raw_efforts.and_then(Value::as_array) {
        if !efforts.is_empty() {
            let supported_efforts: Vec<&str> = efforts
                .iter()
                .filter_map(Value::as_str)
                .filter(|effort| THINKING_LEVELS.contains(effort))
                .collect();
            if supported_efforts.is_empty() {
                return Some(enabled_only_capabilities(mandatory));
            }
            let mut thinking_level_map: ThinkingLevelMap = ThinkingLevelMap::new();
            if mandatory {
                thinking_level_map.insert("off".to_string(), None);
            }
            for level in THINKING_LEVELS {
                thinking_level_map.insert(
                    level.to_string(),
                    if supported_efforts.contains(&level) {
                        Some(level.to_string())
                    } else {
                        None
                    },
                );
            }
            return Some(OpenRouterReasoningCapabilities {
                thinking_level_map: Some(thinking_level_map),
                supports_reasoning_effort: true,
                mandatory,
            });
        }
    }

    Some(enabled_only_capabilities(mandatory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ignores_non_records_and_missing_parameters() {
        assert!(get_openrouter_reasoning_capabilities(&json!(null)).is_none());
        assert!(get_openrouter_reasoning_capabilities(&json!([1])).is_none());
        assert!(get_openrouter_reasoning_capabilities(&json!({"supported_parameters": ["reasoning"]})).is_none());
        assert!(get_openrouter_reasoning_capabilities(&json!({"supported_parameters": [], "reasoning": {}})).is_none());
    }

    #[test]
    fn null_supported_efforts_maps_every_level() {
        let model = json!({
            "supported_parameters": ["reasoning"],
            "reasoning": {"supported_efforts": null, "mandatory": true}
        });
        let capabilities = get_openrouter_reasoning_capabilities(&model).unwrap();
        assert!(capabilities.supports_reasoning_effort);
        assert!(capabilities.mandatory);
        let map = capabilities.thinking_level_map.unwrap();
        assert_eq!(map.get("off"), Some(&None));
        assert_eq!(map.get("max"), Some(&Some("max".to_string())));
    }

    #[test]
    fn explicit_efforts_mark_unsupported_levels_null() {
        let model = json!({
            "supported_parameters": ["reasoning"],
            "reasoning": {"supported_efforts": ["low", "high"], "mandatory": false}
        });
        let capabilities = get_openrouter_reasoning_capabilities(&model).unwrap();
        let map = capabilities.thinking_level_map.unwrap();
        assert_eq!(map.get("low"), Some(&Some("low".to_string())));
        assert_eq!(map.get("high"), Some(&Some("high".to_string())));
        assert_eq!(map.get("medium"), Some(&None));
        assert!(!map.contains_key("off"));
    }

    #[test]
    fn unknown_efforts_fall_back_to_enabled_only() {
        let model = json!({
            "supported_parameters": ["reasoning"],
            "reasoning": {"supported_efforts": ["bogus"]}
        });
        let capabilities = get_openrouter_reasoning_capabilities(&model).unwrap();
        assert!(!capabilities.supports_reasoning_effort);
        assert_eq!(
            capabilities.thinking_level_map.unwrap().get("high"),
            Some(&Some("high".to_string()))
        );

        let model = json!({
            "supported_parameters": ["reasoning"],
            "reasoning": {"supported_efforts": []}
        });
        let capabilities = get_openrouter_reasoning_capabilities(&model).unwrap();
        assert!(!capabilities.supports_reasoning_effort);
    }
}
