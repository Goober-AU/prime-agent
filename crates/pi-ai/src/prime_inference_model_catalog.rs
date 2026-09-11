//! Port of packages/ai/src/prime-inference-model-catalog.ts

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PrimeInferenceCatalogEntry {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub input: f64,
    pub output: f64,
    #[serde(rename = "cacheRead", skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(rename = "cacheWrite", skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    #[serde(rename = "contextWindow", skip_serializing_if = "Option::is_none")]
    pub context_window: Option<f64>,
    #[serde(rename = "maxTokens", skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParseCatalogOptions {
    pub allow_empty: Option<bool>,
}

fn is_record(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

fn non_negative_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0),
        _ => None,
    }
}

fn positive_integer(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number
            .as_f64()
            .filter(|value| value.is_finite() && value.fract() == 0.0 && *value > 0.0),
        _ => None,
    }
}

/// The `[\u0000-\u001f\u007f-\u009f]` character class used by the TypeScript.
fn has_control_characters(text: &str) -> bool {
    text.chars()
        .any(|c| (c as u32) <= 0x1f || (0x7f..=0x9f).contains(&(c as u32)))
}

fn strip_control_characters(text: &str) -> String {
    text.chars()
        .filter(|c| !((*c as u32) <= 0x1f || (0x7f..=0x9f).contains(&(*c as u32))))
        .collect()
}

pub fn is_private_prime_inference_model_id(model_id: &str) -> bool {
    let normalized_id = model_id.to_lowercase();
    normalized_id.starts_with("internal/")
        || normalized_id.starts_with("dev/")
        || normalized_id.contains(':')
}

/// `parsePrimeInferenceModelCatalog(value, options?)`.
pub fn parse_prime_inference_model_catalog(
    value: &Value,
    options: Option<&ParseCatalogOptions>,
) -> Result<Vec<PrimeInferenceCatalogEntry>, String> {
    let allow_empty = options.and_then(|options| options.allow_empty).unwrap_or(false);
    let Some(object) = value.as_object() else {
        return Err("Invalid Prime Inference model catalog".to_string());
    };
    let Some(data) = object.get("data").and_then(Value::as_array) else {
        return Err("Invalid Prime Inference model catalog".to_string());
    };

    let mut models: Vec<PrimeInferenceCatalogEntry> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in data {
        let Some(item) = item.as_object().filter(|_| is_record(item)) else {
            continue;
        };
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        if id.is_empty() || id.chars().count() > 1_024 {
            continue;
        }
        if has_control_characters(id) {
            continue;
        }
        if seen.contains(id) {
            return Err(format!("Duplicate Prime Inference model {}", id));
        }
        let pricing = item
            .get("pricing")
            .filter(|value| is_record(value))
            .and_then(Value::as_object);
        let input = non_negative_number(pricing.and_then(|pricing| pricing.get("input_usd_per_mtok")));
        let output = non_negative_number(pricing.and_then(|pricing| pricing.get("output_usd_per_mtok")));
        let (Some(input), Some(output)) = (input, output) else {
            continue;
        };

        let name = match item.get("display_name") {
            Some(Value::String(display_name)) => strip_control_characters(display_name).trim().to_string(),
            _ => String::new(),
        };
        let specs = item
            .get("specs")
            .filter(|value| is_record(value))
            .and_then(Value::as_object);
        let modalities = specs
            .and_then(|specs| specs.get("modalities"))
            .filter(|value| is_record(value))
            .and_then(Value::as_object);
        let input_modalities = modalities
            .and_then(|modalities| modalities.get("input"))
            .and_then(Value::as_array)
            .filter(|values| values.iter().all(Value::is_string));
        let output_modalities = modalities
            .and_then(|modalities| modalities.get("output"))
            .and_then(Value::as_array)
            .filter(|values| values.iter().all(Value::is_string));
        let context_window = positive_integer(specs.and_then(|specs| specs.get("context_window")));
        let max_tokens = positive_integer(specs.and_then(|specs| specs.get("max_output_tokens")));
        let reasoning = specs
            .and_then(|specs| specs.get("supports_reasoning"))
            .and_then(Value::as_bool);
        let has_specs = context_window.is_some()
            && max_tokens.is_some()
            && reasoning.is_some()
            && input_modalities.is_some()
            && output_modalities.is_some();
        let cache_read = non_negative_number(pricing.and_then(|pricing| pricing.get("cache_read_usd_per_mtok")));
        let cache_write =
            non_negative_number(pricing.and_then(|pricing| pricing.get("cache_write_usd_per_mtok")));

        seen.insert(id.to_string());
        let mut entry = PrimeInferenceCatalogEntry {
            id: id.to_string(),
            name: if name.is_empty() { None } else { Some(name) },
            input,
            output,
            cache_read,
            cache_write,
            context_window: None,
            max_tokens: None,
            vision: None,
            reasoning: None,
        };
        if has_specs {
            let context_window = context_window.expect("checked by has_specs");
            let max_tokens = max_tokens.expect("checked by has_specs");
            entry.context_window = Some(context_window);
            entry.max_tokens = Some(max_tokens.min(context_window));
            entry.vision = Some(
                input_modalities
                    .expect("checked by has_specs")
                    .iter()
                    .any(|modality| modality.as_str() == Some("image")),
            );
            entry.reasoning = reasoning;
        }
        models.push(entry);
    }
    if models.is_empty() && !allow_empty {
        return Err("Prime Inference model catalog is empty".to_string());
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> Value {
        json!({
            "data": [
                {
                    "id": "openai/gpt-5",
                    "display_name": " GPT-5 ",
                    "pricing": {"input_usd_per_mtok": 1.25, "output_usd_per_mtok": 10.0},
                    "specs": {
                        "context_window": 400000,
                        "max_output_tokens": 500000,
                        "supports_reasoning": true,
                        "modalities": {"input": ["text", "image"], "output": ["text"]}
                    }
                },
                {
                    "id": "no-pricing",
                    "pricing": {"input_usd_per_mtok": 1.0}
                }
            ]
        })
    }

    #[test]
    fn parses_entries_and_clamps_max_tokens() {
        let models = parse_prime_inference_model_catalog(&catalog(), None).unwrap();
        assert_eq!(models.len(), 1);
        let entry = &models[0];
        assert_eq!(entry.id, "openai/gpt-5");
        assert_eq!(entry.name.as_deref(), Some("GPT-5"));
        assert_eq!(entry.input, 1.25);
        assert_eq!(entry.output, 10.0);
        assert_eq!(entry.context_window, Some(400000.0));
        assert_eq!(entry.max_tokens, Some(400000.0));
        assert_eq!(entry.vision, Some(true));
        assert_eq!(entry.reasoning, Some(true));
        assert_eq!(entry.cache_read, None);
    }

    #[test]
    fn rejects_invalid_or_empty_catalogs() {
        assert!(parse_prime_inference_model_catalog(&json!(null), None).is_err());
        assert!(parse_prime_inference_model_catalog(&json!({}), None).is_err());
        assert!(parse_prime_inference_model_catalog(&json!({"data": []}), None).is_err());
        assert!(
            parse_prime_inference_model_catalog(&json!({"data": []}), Some(&ParseCatalogOptions { allow_empty: Some(true) }))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_duplicate_ids() {
        let value = json!({
            "data": [
                {"id": "a", "pricing": {"input_usd_per_mtok": 0, "output_usd_per_mtok": 0}},
                {"id": "a", "pricing": {"input_usd_per_mtok": 0, "output_usd_per_mtok": 0}}
            ]
        });
        assert_eq!(
            parse_prime_inference_model_catalog(&value, None).unwrap_err(),
            "Duplicate Prime Inference model a"
        );
    }

    #[test]
    fn skips_ids_with_control_characters_and_bad_shapes() {
        let value = json!({
            "data": [
                {"id": "bad\u{0001}id", "pricing": {"input_usd_per_mtok": 0, "output_usd_per_mtok": 0}},
                {"id": 42, "pricing": {"input_usd_per_mtok": 0, "output_usd_per_mtok": 0}},
                {"id": "good", "pricing": {"input_usd_per_mtok": 0, "output_usd_per_mtok": 0}}
            ]
        });
        let models = parse_prime_inference_model_catalog(&value, None).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "good");
    }

    #[test]
    fn private_model_ids() {
        assert!(is_private_prime_inference_model_id("internal/foo"));
        assert!(is_private_prime_inference_model_id("DEV/foo"));
        assert!(is_private_prime_inference_model_id("vendor:model"));
        assert!(!is_private_prime_inference_model_id("openai/gpt-5"));
    }
}
