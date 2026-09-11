//! Port of packages/coding-agent/src/core/compaction/checkpoint.ts

use pi_ai::compaction::{is_compaction_checkpoint, ProviderCompactionCheckpoint};
use serde_json::Value;

fn is_legacy_checkpoint(details: &serde_json::Map<String, Value>) -> bool {
    details.get("strategy").and_then(Value::as_str) == Some("openai-responses-compaction-v2")
}

pub fn has_provider_checkpoint(details: &Value) -> bool {
    match details {
        Value::Object(object) => object.contains_key("providerCheckpoint") || is_legacy_checkpoint(object),
        _ => false,
    }
}

pub fn get_provider_checkpoint(details: &Value) -> Option<ProviderCompactionCheckpoint> {
    let Value::Object(object) = details else {
        return None;
    };
    if let Some(value) = object.get("providerCheckpoint") {
        return if is_compaction_checkpoint(value) {
            serde_json::from_value(value.clone()).ok()
        } else {
            None
        };
    }
    // Read older extension checkpoints without rewriting their opaque provider items.
    if !is_legacy_checkpoint(object) {
        return None;
    }
    let Some(Value::Array(items)) = object.get("compactedWindow") else {
        return None;
    };
    let has_encrypted_compaction = items.iter().any(|item| match item {
        Value::Object(item) => {
            item.get("type").and_then(Value::as_str) == Some("compaction")
                && matches!(item.get("encrypted_content"), Some(Value::String(value)) if !value.is_empty())
        }
        _ => false,
    });
    if !has_encrypted_compaction {
        return None;
    }

    // `JSON.stringify(items, replacer)`: data-URL images are replaced by "(image)"
    // and counted, so the estimated token count does not read the whole payload.
    let mut images = 0usize;
    let serialized = serialize_with_image_replacer(&Value::Array(items.clone()), &mut images);
    let checkpoint = serde_json::json!({
        "version": 1,
        "provider": object.get("provider").cloned().unwrap_or(Value::Null),
        "api": object.get("api").cloned().unwrap_or(Value::Null),
        "model": object.get("model").cloned().unwrap_or(Value::Null),
        "baseUrl": object.get("baseUrl").cloned().unwrap_or(Value::Null),
        "items": Value::Array(items.clone()),
        "estimatedTokens": ((serialized.chars().count() + 3) / 4) as f64 + (images as f64) * 1200.0,
    });
    if is_compaction_checkpoint(&checkpoint) {
        serde_json::from_value(checkpoint).ok()
    } else {
        None
    }
}

/// `JSON.stringify(value, replacer)`: keys "image_url"/"url" whose string value
/// starts with "data:image/" become "(image)" and increment `images`.
fn serialize_with_image_replacer(value: &Value, images: &mut usize) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string()),
        Value::Array(items) => {
            let rendered: Vec<String> = items
                .iter()
                .map(|item| serialize_with_image_replacer(item, images))
                .collect();
            format!("[{}]", rendered.join(","))
        }
        Value::Object(object) => {
            let rendered: Vec<String> = object
                .iter()
                .map(|(key, item)| {
                    let replaced = match item {
                        Value::String(text)
                            if (key == "image_url" || key == "url") && text.starts_with("data:image/") =>
                        {
                            *images += 1;
                            Some(Value::String("(image)".to_string()))
                        }
                        _ => None,
                    };
                    let value = replaced.unwrap_or_else(|| item.clone());
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                        serialize_with_image_replacer(&value, images)
                    )
                })
                .collect();
            format!("{{{}}}", rendered.join(","))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_provider_checkpoint_key() {
        assert!(has_provider_checkpoint(&json!({"providerCheckpoint": null})));
        assert!(has_provider_checkpoint(&json!({
            "strategy": "openai-responses-compaction-v2"
        })));
        assert!(!has_provider_checkpoint(&json!({"strategy": "other"})));
        assert!(!has_provider_checkpoint(&json!(null)));
        assert!(!has_provider_checkpoint(&json!("text")));
    }

    #[test]
    fn returns_undefined_for_non_objects_and_invalid_checkpoints() {
        assert!(get_provider_checkpoint(&json!(null)).is_none());
        assert!(get_provider_checkpoint(&json!({"providerCheckpoint": {"version": 2}})).is_none());
    }

    #[test]
    fn reads_legacy_checkpoints_with_encrypted_items() {
        let details = json!({
            "strategy": "openai-responses-compaction-v2",
            "provider": "openai",
            "api": "openai-responses",
            "model": "gpt-5",
            "baseUrl": "https://api.openai.com/v1",
            "compactedWindow": [
                {"type": "compaction", "encrypted_content": "abc"},
                {"type": "message", "content": [{"image_url": "data:image/png;base64,AAAA"}]}
            ],
        });
        let checkpoint = get_provider_checkpoint(&details).expect("checkpoint");
        assert_eq!(checkpoint.version, 1);
        assert_eq!(checkpoint.provider, "openai");
        assert_eq!(checkpoint.items.len(), 2);
        assert!(checkpoint.estimated_tokens >= 1200.0);
        assert!(checkpoint.endpoint.is_none());
    }

    #[test]
    fn legacy_checkpoints_without_encrypted_items_are_ignored() {
        let details = json!({
            "strategy": "openai-responses-compaction-v2",
            "provider": "openai",
            "api": "openai-responses",
            "model": "gpt-5",
            "baseUrl": "https://api.openai.com/v1",
            "compactedWindow": [{"type": "compaction", "encrypted_content": ""}],
        });
        assert!(get_provider_checkpoint(&details).is_none());
        let no_window = json!({"strategy": "openai-responses-compaction-v2"});
        assert!(get_provider_checkpoint(&no_window).is_none());
    }
}
