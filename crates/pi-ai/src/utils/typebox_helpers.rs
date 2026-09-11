//! Port of packages/ai/src/utils/typebox-helpers.ts
//!
//! The TypeScript builds a TypeBox `TUnsafe` schema. Rust has no TypeBox, so the
//! helper returns the JSON Schema object that TypeBox would have produced.

use serde_json::{Map, Value};

/// Creates a string enum schema compatible with Google's API and other providers
/// that don't support anyOf/const patterns.
///
/// ```text
/// StringEnum(["add", "subtract"], { description: "The operation to perform" })
/// ```
pub fn string_enum(values: &[&str], options: Option<StringEnumOptions>) -> Value {
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("string".to_string()));
    schema.insert(
        "enum".to_string(),
        Value::Array(values.iter().map(|value| Value::String((*value).to_string())).collect()),
    );
    if let Some(options) = options {
        if let Some(description) = options.description {
            schema.insert("description".to_string(), Value::String(description));
        }
        if let Some(default) = options.default {
            schema.insert("default".to_string(), Value::String(default));
        }
    }
    Value::Object(schema)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StringEnumOptions {
    pub description: Option<String>,
    pub default: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_string_enum_schema() {
        assert_eq!(
            string_enum(&["add", "subtract"], None),
            json!({"type": "string", "enum": ["add", "subtract"]})
        );
    }

    #[test]
    fn adds_optional_description_and_default() {
        assert_eq!(
            string_enum(
                &["a"],
                Some(StringEnumOptions {
                    description: Some("d".to_string()),
                    default: Some("a".to_string()),
                })
            ),
            json!({"type": "string", "enum": ["a"], "description": "d", "default": "a"})
        );
    }
}
