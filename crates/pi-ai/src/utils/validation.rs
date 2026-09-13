//! Port of packages/ai/src/utils/validation.ts
//!
//! The TypeScript validates with TypeBox (`Compile` / `Value.Convert`). Rust has
//! no TypeBox; the port keeps the observable behaviour:
//! - `Value.Convert`-style primitive conversion (`convert_json_schema_value`)
//! - the JSON-Schema coercion rules (`coerce_with_json_schema`)
//! - validation against a JSON Schema subset with AJV-compatible type checks
//! - the exact error strings `Tool "<name>" not found` and
//!   `Validation failed for tool "<name>":\n<errors>\n\nReceived arguments:\n<json>`
//!
//! A TypeBox schema is represented by the JSON Schema it serialises to, so the
//! `hasTypeBoxMetadata` branch (symbol marker) does not exist in Rust and the
//! JSON-Schema coercion path is always used.

use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value};

use crate::types::{Tool, ToolCall};

fn is_record(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

fn is_json_schema_object(value: &Value) -> bool {
    is_record(value)
}

fn get_schema_types(schema: &Map<String, Value>) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(value)) => vec![value.clone()],
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn matches_json_type(value: &Value, schema_type: &str) -> bool {
    match schema_type {
        "number" => value.is_number(),
        "integer" => match value {
            Value::Number(number) => number
                .as_f64()
                .map(|number| number.fract() == 0.0)
                .unwrap_or(false),
            _ => false,
        },
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => is_record(value),
        _ => false,
    }
}

/// `Value.Convert(schema, value)` for the primitive conversions used by tool
/// schemas: strings that look like numbers become numbers, `"true"`/`"false"`
/// become booleans, and numbers become strings.
pub fn convert_json_schema_value(value: &mut Value, schema: &Value) {
    let Some(schema) = schema.as_object() else {
        return;
    };
    let types = get_schema_types(schema);
    if types.is_empty() {
        return;
    }
    let already_matches = types
        .iter()
        .any(|schema_type| matches_json_type(value, schema_type));
    if already_matches {
        // Still convert nested object/array members.
    } else {
        for schema_type in &types {
            if let Some(converted) = convert_primitive_by_type(value, schema_type) {
                *value = converted;
                break;
            }
        }
    }

    if types.iter().any(|schema_type| schema_type == "object") {
        if let Value::Object(object) = value {
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (key, property_schema) in properties {
                    if let Some(property_value) = object.get_mut(key) {
                        convert_json_schema_value(property_value, property_schema);
                    }
                }
            }
        }
    }

    if types.iter().any(|schema_type| schema_type == "array") {
        if let Value::Array(items) = value {
            match schema.get("items") {
                Some(Value::Array(item_schemas)) => {
                    for (index, item) in items.iter_mut().enumerate() {
                        if let Some(item_schema) = item_schemas.get(index) {
                            convert_json_schema_value(item, item_schema);
                        }
                    }
                }
                Some(item_schema) if is_json_schema_object(item_schema) => {
                    for item in items.iter_mut() {
                        convert_json_schema_value(item, item_schema);
                    }
                }
                _ => {}
            }
        }
    }
}

/// `Value.Convert` primitive conversion for one schema type.
fn convert_primitive_by_type(value: &Value, schema_type: &str) -> Option<Value> {
    match schema_type {
        "number" | "integer" => match value {
            Value::String(text) => {
                if text.trim().is_empty() {
                    return None;
                }
                let parsed: f64 = text.parse().ok()?;
                if schema_type == "integer" && parsed.fract() != 0.0 {
                    return None;
                }
                Some(serde_json::Number::from_f64(parsed).map(Value::Number)?)
            }
            Value::Bool(flag) => Some(Value::Number(if *flag { 1.into() } else { 0.into() })),
            _ => None,
        },
        "boolean" => match value {
            Value::String(text) if text == "true" => Some(Value::Bool(true)),
            Value::String(text) if text == "false" => Some(Value::Bool(false)),
            Value::Number(number) if number.as_f64() == Some(1.0) => Some(Value::Bool(true)),
            Value::Number(number) if number.as_f64() == Some(0.0) => Some(Value::Bool(false)),
            _ => None,
        },
        "string" => match value {
            Value::Number(_) | Value::Bool(_) => Some(Value::String(match value {
                Value::Number(number) => number.to_string(),
                Value::Bool(flag) => flag.to_string(),
                _ => unreachable!(),
            })),
            _ => None,
        },
        "null" => match value {
            Value::String(text) if text.is_empty() => Some(Value::Null),
            Value::Number(number) if number.as_f64() == Some(0.0) => Some(Value::Null),
            Value::Bool(false) => Some(Value::Null),
            _ => None,
        },
        _ => None,
    }
}

fn coerce_primitive_by_type(value: &Value, schema_type: &str) -> Value {
    match schema_type {
        "number" => match value {
            Value::Null => Value::Number(0.into()),
            Value::String(text) if !text.trim().is_empty() => match text.parse::<f64>() {
                Ok(parsed) if parsed.is_finite() => serde_json::Number::from_f64(parsed)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                _ => value.clone(),
            },
            Value::Bool(flag) => Value::Number(if *flag { 1.into() } else { 0.into() }),
            _ => value.clone(),
        },
        "integer" => match value {
            Value::Null => Value::Number(0.into()),
            Value::String(text) if !text.trim().is_empty() => match text.parse::<f64>() {
                Ok(parsed) if parsed.fract() == 0.0 && parsed.is_finite() => {
                    serde_json::Number::from_f64(parsed)
                        .map(Value::Number)
                        .unwrap_or(Value::Null)
                }
                _ => value.clone(),
            },
            Value::Bool(flag) => Value::Number(if *flag { 1.into() } else { 0.into() }),
            _ => value.clone(),
        },
        "boolean" => match value {
            Value::Null => Value::Bool(false),
            Value::String(text) if text == "true" => Value::Bool(true),
            Value::String(text) if text == "false" => Value::Bool(false),
            Value::Number(number) if number.as_f64() == Some(1.0) => Value::Bool(true),
            Value::Number(number) if number.as_f64() == Some(0.0) => Value::Bool(false),
            _ => value.clone(),
        },
        "string" => match value {
            Value::Null => Value::String(String::new()),
            Value::Number(number) => Value::String(number.to_string()),
            Value::Bool(flag) => Value::String(flag.to_string()),
            _ => value.clone(),
        },
        "null" => match value {
            Value::String(text) if text.is_empty() => Value::Null,
            Value::Number(number) if number.as_f64() == Some(0.0) => Value::Null,
            Value::Bool(false) => Value::Null,
            _ => value.clone(),
        },
        _ => value.clone(),
    }
}

fn apply_schema_object_coercion(value: &mut Map<String, Value>, schema: &Map<String, Value>) {
    let properties = schema.get("properties").and_then(Value::as_object);
    let defined_keys: Vec<String> = properties
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default();

    if let Some(properties) = properties {
        for (key, property_schema) in properties {
            if !value.contains_key(key) {
                continue;
            }
            let coerced = coerce_with_json_schema(&value[key], property_schema);
            value.insert(key.clone(), coerced);
        }
    }

    if let Some(additional) = schema.get("additionalProperties") {
        if is_json_schema_object(additional) {
            let keys: Vec<String> = value.keys().cloned().collect();
            for key in keys {
                if defined_keys.contains(&key) {
                    continue;
                }
                let coerced = coerce_with_json_schema(&value[&key], additional);
                value.insert(key, coerced);
            }
        }
    }
}

fn apply_schema_array_coercion(value: &mut Vec<Value>, schema: &Map<String, Value>) {
    match schema.get("items") {
        Some(Value::Array(item_schemas)) => {
            for (index, item) in value.iter_mut().enumerate() {
                let Some(item_schema) = item_schemas.get(index) else {
                    continue;
                };
                *item = coerce_with_json_schema(item, item_schema);
            }
        }
        Some(item_schema) if is_json_schema_object(item_schema) => {
            for item in value.iter_mut() {
                *item = coerce_with_json_schema(item, item_schema);
            }
        }
        _ => {}
    }
}

fn coerce_with_union_schema(value: &Value, schemas: &[Value]) -> Value {
    for schema in schemas {
        let candidate = value.clone();
        let coerced = coerce_with_json_schema(&candidate, schema);
        if check_json_schema(&coerced, schema) {
            return coerced;
        }
    }
    value.clone()
}

fn coerce_with_json_schema(value: &Value, schema: &Value) -> Value {
    let Some(schema_object) = schema.as_object() else {
        return value.clone();
    };
    let mut next_value = value.clone();

    if let Some(all_of) = schema_object.get("allOf").and_then(Value::as_array) {
        for nested in all_of {
            next_value = coerce_with_json_schema(&next_value, nested);
        }
    }

    if let Some(any_of) = schema_object.get("anyOf").and_then(Value::as_array) {
        next_value = coerce_with_union_schema(&next_value, any_of);
    }

    if let Some(one_of) = schema_object.get("oneOf").and_then(Value::as_array) {
        next_value = coerce_with_union_schema(&next_value, one_of);
    }

    let schema_types = get_schema_types(schema_object);
    let matches_union_member = schema_types.len() > 1
        && schema_types
            .iter()
            .any(|schema_type| matches_json_type(&next_value, schema_type));
    if !schema_types.is_empty() && !matches_union_member {
        for schema_type in &schema_types {
            let candidate = coerce_primitive_by_type(&next_value, schema_type);
            if candidate != next_value {
                next_value = candidate;
                break;
            }
        }
    }

    if schema_types
        .iter()
        .any(|schema_type| schema_type == "object")
    {
        if let Value::Object(object) = &next_value {
            let mut object = object.clone();
            apply_schema_object_coercion(&mut object, schema_object);
            next_value = Value::Object(object);
        }
    }

    if schema_types
        .iter()
        .any(|schema_type| schema_type == "array")
    {
        if let Value::Array(items) = &next_value {
            let mut items = items.clone();
            apply_schema_array_coercion(&mut items, schema_object);
            next_value = Value::Array(items);
        }
    }

    next_value
}

/// `Compile(schema)` - the cached JSON Schema validator.
///
/// The TypeScript caches compiled validators in a `WeakMap` keyed by the schema
/// object. Rust keys the cache by the serialized schema instead.
fn validator_cache() -> &'static Mutex<Vec<(String, Arc<JsonSchemaValidator>)>> {
    static CACHE: OnceLock<Mutex<Vec<(String, Arc<JsonSchemaValidator>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

#[derive(Debug, Clone, PartialEq)]
pub struct JsonSchemaError {
    pub keyword: String,
    pub instance_path: String,
    pub message: String,
    pub params: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct JsonSchemaValidator {
    schema: Value,
}

pub fn get_validator(schema: &Value) -> Arc<JsonSchemaValidator> {
    let key = serde_json::to_string(schema).unwrap_or_default();
    {
        let cache = validator_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((_, validator)) = cache.iter().find(|(cached_key, _)| *cached_key == key) {
            return validator.clone();
        }
    }
    let validator = Arc::new(JsonSchemaValidator {
        schema: schema.clone(),
    });
    let mut cache = validator_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.push((key, validator.clone()));
    validator
}

impl JsonSchemaValidator {
    /// `validator.Check(value)`.
    pub fn check(&self, value: &Value) -> bool {
        let mut errors = Vec::new();
        validate_against_schema(value, &self.schema, "", &mut errors);
        errors.is_empty()
    }

    /// `validator.Errors(value)`.
    pub fn errors(&self, value: &Value) -> Vec<JsonSchemaError> {
        let mut errors = Vec::new();
        validate_against_schema(value, &self.schema, "", &mut errors);
        errors
    }
}

fn check_json_schema(value: &Value, schema: &Value) -> bool {
    get_validator(schema).check(value)
}

fn validate_against_schema(
    value: &Value,
    schema: &Value,
    instance_path: &str,
    errors: &mut Vec<JsonSchemaError>,
) {
    let Some(schema_object) = schema.as_object() else {
        return;
    };

    let schema_types = get_schema_types(schema_object);
    if !schema_types.is_empty()
        && !schema_types
            .iter()
            .any(|schema_type| matches_json_type(value, schema_type))
    {
        errors.push(JsonSchemaError {
            keyword: "type".to_string(),
            instance_path: instance_path.to_string(),
            message: format!(
                "Expected {}",
                schema_types
                    .iter()
                    .map(|schema_type| format!("\"{}\"", schema_type))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            params: Map::new(),
        });
        return;
    }

    if let Some(enum_values) = schema_object.get("enum").and_then(Value::as_array) {
        if !enum_values.contains(value) {
            errors.push(JsonSchemaError {
                keyword: "enum".to_string(),
                instance_path: instance_path.to_string(),
                message: "Expected a value from the enum".to_string(),
                params: Map::new(),
            });
        }
    }

    if let Some(Value::Object(properties)) = schema_object.get("properties") {
        if let Value::Object(value_object) = value {
            for (key, property_schema) in properties {
                if let Some(property_value) = value_object.get(key) {
                    let child_path = format!("{}/{}", instance_path, key);
                    validate_against_schema(property_value, property_schema, &child_path, errors);
                }
            }
        }
    }

    if let Some(required) = schema_object.get("required").and_then(Value::as_array) {
        if let Value::Object(value_object) = value {
            for key in required.iter().filter_map(Value::as_str) {
                if !value_object.contains_key(key) {
                    let mut params = Map::new();
                    params.insert(
                        "requiredProperties".to_string(),
                        Value::Array(vec![Value::String(key.to_string())]),
                    );
                    errors.push(JsonSchemaError {
                        keyword: "required".to_string(),
                        instance_path: instance_path.to_string(),
                        message: format!("Expected required property \"{}\"", key),
                        params,
                    });
                }
            }
        }
    }

    if let Some(items_schema) = schema_object.get("items") {
        if let Value::Array(items) = value {
            match items_schema {
                Value::Array(item_schemas) => {
                    for (index, item) in items.iter().enumerate() {
                        if let Some(item_schema) = item_schemas.get(index) {
                            let child_path = format!("{}/{}", instance_path, index);
                            validate_against_schema(item, item_schema, &child_path, errors);
                        }
                    }
                }
                item_schema => {
                    for (index, item) in items.iter().enumerate() {
                        let child_path = format!("{}/{}", instance_path, index);
                        validate_against_schema(item, item_schema, &child_path, errors);
                    }
                }
            }
        }
    }

    for keyword in ["anyOf", "oneOf"] {
        if let Some(schemas) = schema_object.get(keyword).and_then(Value::as_array) {
            let matches = schemas
                .iter()
                .filter(|candidate| get_validator(candidate).check(value))
                .count();
            let valid = if keyword == "oneOf" {
                matches == 1
            } else {
                matches >= 1
            };
            if !valid {
                errors.push(JsonSchemaError {
                    keyword: keyword.to_string(),
                    instance_path: instance_path.to_string(),
                    message: format!("Expected to match {} schema", keyword),
                    params: Map::new(),
                });
            }
        }
    }

    if let Some(all_of) = schema_object.get("allOf").and_then(Value::as_array) {
        for nested in all_of {
            validate_against_schema(value, nested, instance_path, errors);
        }
    }
}

fn format_validation_path(error: &JsonSchemaError) -> String {
    if error.keyword == "required" {
        let required_property = error
            .params
            .get("requiredProperties")
            .and_then(Value::as_array)
            .and_then(|properties| properties.first())
            .and_then(Value::as_str);
        if let Some(required_property) = required_property {
            let base_path = error
                .instance_path
                .trim_start_matches('/')
                .replace('/', ".");
            return if base_path.is_empty() {
                required_property.to_string()
            } else {
                format!("{}.{}", base_path, required_property)
            };
        }
    }
    let path = error
        .instance_path
        .trim_start_matches('/')
        .replace('/', ".");
    if path.is_empty() {
        "root".to_string()
    } else {
        path
    }
}

/// Finds a tool by name and validates the tool call arguments against its schema.
pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> Result<Value, String> {
    let tool = tools.iter().find(|tool| tool.name == tool_call.name);
    let Some(tool) = tool else {
        return Err(format!("Tool \"{}\" not found", tool_call.name));
    };
    validate_tool_arguments(tool, tool_call)
}

/// Validates tool call arguments against the tool's schema.
pub fn validate_tool_arguments(tool: &Tool, tool_call: &ToolCall) -> Result<Value, String> {
    let mut args = Value::Object(tool_call.arguments.clone());
    convert_json_schema_value(&mut args, &tool.parameters);

    let validator = get_validator(&tool.parameters);
    if is_json_schema_object(&tool.parameters) {
        let coerced = coerce_with_json_schema(&args, &tool.parameters);
        if coerced != args {
            if is_record(&args) && is_record(&coerced) {
                args = coerced;
            } else {
                return Ok(if validator.check(&coerced) {
                    coerced
                } else {
                    args
                });
            }
        }
    }

    if validator.check(&args) {
        return Ok(args);
    }

    let errors = validator
        .errors(&args)
        .iter()
        .map(|error| format!("  - {}: {}", format_validation_path(error), error.message))
        .collect::<Vec<_>>()
        .join("\n");
    let errors = if errors.is_empty() {
        "Unknown validation error".to_string()
    } else {
        errors
    };

    let received = serde_json::to_string_pretty(&Value::Object(tool_call.arguments.clone()))
        .unwrap_or_default();
    Err(format!(
        "Validation failed for tool \"{}\":\n{}\n\nReceived arguments:\n{}",
        tool_call.name, errors, received
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_with_schema(schema: Value) -> Tool {
        Tool {
            name: "echo".to_string(),
            description: "Echo tool".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"value": schema},
                "required": ["value"]
            }),
        }
    }

    fn tool_call(value: Value) -> ToolCall {
        let mut arguments = Map::new();
        arguments.insert("value".to_string(), value);
        ToolCall::new("tool-1", "echo", arguments)
    }

    #[test]
    fn coerces_serialized_plain_json_schemas() {
        let passing = [
            (json!({"type": "number"}), json!("42"), json!(42.0)),
            (json!({"type": "number"}), json!(true), json!(1)),
            (json!({"type": "number"}), json!(null), json!(0)),
            (json!({"type": "integer"}), json!("42"), json!(42.0)),
            (json!({"type": "boolean"}), json!("true"), json!(true)),
            (json!({"type": "boolean"}), json!("false"), json!(false)),
            (json!({"type": "boolean"}), json!(1), json!(true)),
            (json!({"type": "boolean"}), json!(0), json!(false)),
            (json!({"type": "string"}), json!(null), json!("")),
            (json!({"type": "string"}), json!(true), json!("true")),
            (json!({"type": "null"}), json!(""), json!(null)),
            (json!({"type": "null"}), json!(0), json!(null)),
            (json!({"type": "null"}), json!(false), json!(null)),
            (
                json!({"type": ["number", "string"]}),
                json!("1"),
                json!("1"),
            ),
            (
                json!({"type": ["boolean", "number"]}),
                json!("1"),
                json!(1.0),
            ),
        ];
        for (schema, input, expected) in passing {
            let tool = tool_with_schema(schema);
            let result = validate_tool_arguments(&tool, &tool_call(input.clone())).unwrap();
            assert_eq!(result, json!({"value": expected}), "input {}", input);
        }
    }

    #[test]
    fn rejects_invalid_coercions() {
        let failing = [
            (json!({"type": "boolean"}), json!("1")),
            (json!({"type": "boolean"}), json!("0")),
            (json!({"type": "null"}), json!("null")),
            (json!({"type": "integer"}), json!("42.1")),
        ];
        for (schema, input) in failing {
            let tool = tool_with_schema(schema);
            let error = validate_tool_arguments(&tool, &tool_call(input.clone())).unwrap_err();
            assert!(error.starts_with("Validation failed"), "input {}", input);
        }
    }

    #[test]
    fn missing_tool_reports_typescript_message() {
        let tools: Vec<Tool> = Vec::new();
        let error = validate_tool_call(&tools, &tool_call(json!(1))).unwrap_err();
        assert_eq!(error, "Tool \"echo\" not found");
    }

    #[test]
    fn error_message_lists_paths_and_received_arguments() {
        let tool = Tool {
            name: "echo".to_string(),
            description: String::new(),
            parameters: json!({
                "type": "object",
                "properties": {"count": {"type": "number"}},
                "required": ["count"]
            }),
        };
        let mut arguments = Map::new();
        arguments.insert("other".to_string(), json!("x"));
        let error =
            validate_tool_arguments(&tool, &ToolCall::new("id", "echo", arguments)).unwrap_err();
        assert!(error.starts_with("Validation failed for tool \"echo\":"));
        assert!(error.contains("  - count:"));
        assert!(error.contains("\n\nReceived arguments:\n{\n  \"other\": \"x\"\n}"));
    }

    #[test]
    fn nested_required_error_uses_dotted_path() {
        let tool = Tool {
            name: "echo".to_string(),
            description: String::new(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "outer": {
                        "type": "object",
                        "properties": {"inner": {"type": "string"}},
                        "required": ["inner"]
                    }
                }
            }),
        };
        let mut arguments = Map::new();
        arguments.insert("outer".to_string(), json!({}));
        let error =
            validate_tool_arguments(&tool, &ToolCall::new("id", "echo", arguments)).unwrap_err();
        assert!(error.contains("  - outer.inner:"), "{}", error);
    }

    #[test]
    fn convert_json_schema_value_coerces_primitives_and_nested_values() {
        let schema = json!({
            "type": "object",
            "properties": {
                "count": {"type": "number"},
                "flag": {"type": "boolean"},
                "name": {"type": "string"},
                "items": {"type": "array", "items": {"type": "number"}}
            }
        });
        let mut value = json!({
            "count": "42",
            "flag": "true",
            "name": 7,
            "items": ["1", "2"]
        });
        convert_json_schema_value(&mut value, &schema);
        assert_eq!(
            value,
            json!({"count": 42.0, "flag": true, "name": "7", "items": [1.0, 2.0]})
        );
    }
}
