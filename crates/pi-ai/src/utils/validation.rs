//! Port of packages/ai/src/utils/validation.ts
//!
//! The TypeScript validates with TypeBox (`Compile` / `Value.Convert`). Rust has
//! no TypeBox; the port keeps the observable behaviour:
//! - `Value.Convert`-style primitive conversion (`convert_json_schema_value`)
//! - the JSON-Schema coercion rules (`coerce_with_json_schema`)
//! - validation against the TypeBox-enforced JSON Schema keywords with
//!   AJV/TypeBox-compatible type checks (`additionalProperties`, `const`, `enum`,
//!   `items`, `maxItems`, `maxLength`, `minItems`, `minLength`, `pattern`,
//!   `patternProperties`, `properties`, `required`, and the `allOf`/`anyOf`/`oneOf`
//!   combinators). `format` is NOT ported; see the note in `validate_against_schema`.
//! - the exact error strings `Tool "<name>" not found` and
//!   `Validation failed for tool "<name>":\n<errors>\n\nReceived arguments:\n<json>`
//!
//! A TypeBox schema is represented by the JSON Schema it serialises to, so the
//! `hasTypeBoxMetadata` branch (symbol marker) does not exist in Rust and the
//! JSON-Schema coercion path is always used.

use std::sync::{Arc, Mutex, OnceLock};

use regex::Regex;
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

// ---------------------------------------------------------------------------
// TypeBox `Guard` helpers.
//
// `Guard.IsEqual` is JavaScript `===` and `Guard.IsDeepEqual`
// (node_modules/typebox/build/guard/guard.mjs:229) branches on the LEFT value:
// arrays compare as arrays, objects compare as objects, everything else compares
// with `===`. Because JavaScript `IsObject` is true for arrays too
// (guard.mjs:224 `DeepEqualObject` only checks `IsObject(right)` and then compares
// `Keys(...)` length), `{}` and `[]` compare equal, and `{"0": 1}` compares equal
// to `[1]`.
// ---------------------------------------------------------------------------

/// JavaScript `Guard.IsObject` (typebox/build/guard/guard.mjs:1499):
/// `typeof value === "object" && value !== null`, so arrays are objects too.
fn is_js_object(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Array(_))
}

/// `Guard.Keys` (guard.mjs:7721) is `Object.getOwnPropertyNames`, so an array also
/// contributes its `length` key.
fn js_property_keys(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => {
            let mut keys: Vec<String> = (0..items.len()).map(|index| index.to_string()).collect();
            keys.push("length".to_string());
            keys
        }
        Value::Object(object) => object.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// JavaScript `value[key]` for the keys produced by [`js_property_keys`], including
/// the array `length` property.
fn js_property(value: &Value, key: &str) -> Option<Value> {
    match value {
        Value::Array(items) => {
            if key == "length" {
                Some(Value::Number(items.len().into()))
            } else {
                key.parse::<usize>()
                    .ok()
                    .and_then(|index| items.get(index))
                    .cloned()
            }
        }
        Value::Object(object) => object.get(key).cloned(),
        _ => None,
    }
}

/// `Guard.IsEqual` (guard.mjs:68) - JavaScript `===`, where every JSON number is the
/// same primitive type, while `null`, `false` and `0` never equal one another.
fn is_strict_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => match (left.as_f64(), right.as_f64()) {
            (Some(left), Some(right)) => left == right,
            _ => left == right,
        },
        _ => left == right,
    }
}

/// `Guard.IsDeepEqual` (typebox/build/guard/guard.mjs:229). The reference branches on
/// the LEFT value: an array uses `DeepEqualArray` (guard.mjs:224), which requires the
/// right side to be an array of the same `length` with pairwise-deep-equal elements;
/// an object uses `DeepEqualObject` (guard.mjs:217), which first requires
/// `Guard.IsObject(right)` and then an equal `Keys(...)` count before comparing each
/// left key. Because `Keys` includes the array `length`, `{}` and `[]` are NOT equal
/// while `{ "0": 1, "length": 1 }` equals `[1]`; both facts were verified against the
/// reference `Guard.IsDeepEqual`.
fn deep_equal(left: &Value, right: &Value) -> bool {
    match left {
        Value::Array(left_items) => match right {
            Value::Array(right_items) => {
                left_items.len() == right_items.len()
                    && left_items
                        .iter()
                        .zip(right_items)
                        .all(|(left, right)| deep_equal(left, right))
            }
            _ => false,
        },
        Value::Object(_) => {
            if !is_js_object(right) {
                return false;
            }
            let left_keys = js_property_keys(left);
            let right_keys = js_property_keys(right);
            if left_keys.len() != right_keys.len() {
                return false;
            }
            left_keys.iter().all(|key| {
                right_keys.contains(key)
                    && match (js_property(left, key), js_property(right, key)) {
                        (Some(left), Some(right)) => deep_equal(&left, &right),
                        _ => false,
                    }
            })
        }
        _ => is_strict_equal(left, right),
    }
}

// ---------------------------------------------------------------------------
// TypeBox string length.
//
// `Guard.IsMinLength`/`Guard.IsMaxLength` (node_modules/typebox/build/guard/guard.mjs)
// count GRAPHEME CLUSTERS, not code points: `IsMinLengthFast`/`IsMaxLengthFast`
// (guard/string.mjs:147,165) fast-path pure-ASCII strings and otherwise delegate to
// `GraphemeCount` (guard/string.mjs:126) built on `NextGraphemeClusterIndex`
// (guard/string.mjs:64), which consumes combining marks, variation selectors,
// zero-width-joiner sequences and regional-indicator pairs.
//
// The reference walks UTF-16 code units, so the port keeps `Vec<u16>` and the same
// index arithmetic to stay exact for astral characters and lone surrogates.
// ---------------------------------------------------------------------------

/// JavaScript `String(number)` for the values `Locale.en_US` interpolates
/// (`typebox/build/system/locale/en_US.mjs`): `-0` prints `0`, `1` prints `1`, not `1.0`.
fn js_number_text(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e21 {
        return format!("{}", value as i64);
    }
    format!("{}", value)
}

fn is_between(value: u32, min: u32, max: u32) -> bool {
    value >= min && value <= max
}

fn is_zero_width_joiner(value: u32) -> bool {
    value == 0x200D
}

fn is_high_surrogate(value: u32) -> bool {
    is_between(value, 0xD800, 0xDBFF)
}

fn is_low_surrogate(value: u32) -> bool {
    is_between(value, 0xDC00, 0xDFFF)
}

fn is_regional_indicator(value: u32) -> bool {
    is_between(value, 0x1F1E6, 0x1F1FF)
}

fn is_variation_selector(value: u32) -> bool {
    is_between(value, 0xFE00, 0xFE0F)
}

fn is_combining_mark(value: u32) -> bool {
    is_between(value, 0x0300, 0x036F)
        || is_between(value, 0x1AB0, 0x1AFF)
        || is_between(value, 0x1DC0, 0x1DFF)
        || is_between(value, 0xFE20, 0xFE2F)
}

fn code_point_length(value: u32) -> usize {
    if value > 0xFFFF {
        2
    } else {
        1
    }
}

/// JavaScript `String.prototype.codePointAt` over UTF-16 code units.
fn code_point_at(units: &[u16], index: usize) -> u32 {
    let first = u32::from(units[index]);
    if is_high_surrogate(first) && index + 1 < units.len() {
        let second = u32::from(units[index + 1]);
        if is_low_surrogate(second) {
            return 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
        }
    }
    first
}

fn consume_modifiers(units: &[u16], mut index: usize) -> usize {
    while index < units.len() {
        let point = code_point_at(units, index);
        if is_combining_mark(point) || is_variation_selector(point) {
            index += code_point_length(point);
        } else {
            break;
        }
    }
    index
}

fn next_grapheme_cluster_index(units: &[u16], cluster_start: usize) -> usize {
    let start_code_point = code_point_at(units, cluster_start);
    let mut cluster_end = cluster_start + code_point_length(start_code_point);
    cluster_end = consume_modifiers(units, cluster_end);
    while cluster_end < units.len().saturating_sub(1) && units[cluster_end] == 0x200D {
        let next_code_point = code_point_at(units, cluster_end + 1);
        cluster_end += 1 + code_point_length(next_code_point);
        cluster_end = consume_modifiers(units, cluster_end);
    }
    if is_regional_indicator(start_code_point)
        && cluster_end < units.len()
        && is_regional_indicator(code_point_at(units, cluster_end))
    {
        cluster_end += code_point_length(code_point_at(units, cluster_end));
    }
    cluster_end
}

fn is_grapheme_code_unit(value: u16) -> bool {
    let value = u32::from(value);
    is_high_surrogate(value)
        || is_combining_mark(value)
        || is_variation_selector(value)
        || is_zero_width_joiner(value)
}

/// `String.IsMinLengthFast`. The limit stays an `f64` because TypeBox takes the raw
/// schema number, so fractional and negative limits behave as they do in JavaScript.
fn string_has_min_length(units: &[u16], min_length: f64) -> bool {
    if min_length == 0.0 {
        return true;
    }
    let mut index = 0usize;
    while index < units.len() {
        if is_grapheme_code_unit(units[index]) {
            let mut count = 0usize;
            let mut cluster = 0usize;
            while cluster < units.len() {
                cluster = next_grapheme_cluster_index(units, cluster);
                count += 1;
                if count as f64 >= min_length {
                    return true;
                }
            }
            return false;
        }
        index += 1;
        if index as f64 >= min_length {
            return true;
        }
    }
    false
}

/// `String.IsMaxLengthFast`.
fn string_has_max_length(units: &[u16], max_length: f64) -> bool {
    let mut index = 0usize;
    while index < units.len() {
        if is_grapheme_code_unit(units[index]) {
            let mut count = 0usize;
            let mut cluster = 0usize;
            while cluster < units.len() {
                cluster = next_grapheme_cluster_index(units, cluster);
                count += 1;
                if count as f64 > max_length {
                    return false;
                }
            }
            return true;
        }
        index += 1;
        if index as f64 > max_length {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// TypeBox `pattern` / `additionalProperties` patterns.
// ---------------------------------------------------------------------------

/// `GetPropertyKeyAsPattern` (typebox/build/schema/engine/additionalProperties.mjs:20):
/// escape the regular-expression metacharacters `.`, `*`, `+`, `?`, `^`, `$`, `{`,
/// `}`, `(`, `)`, `|`, `[`, `]`, `\\` (no `-` in the reference either), then anchor.
fn escape_property_key(key: &str) -> String {
    let mut escaped = String::with_capacity(key.len());
    for character in key.chars() {
        if matches!(
            character,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// `GetPropertiesPattern` (additionalProperties.mjs:23): the union of the
/// `patternProperties` keys (used verbatim) and the escaped `properties` keys, or a
/// pattern that matches nothing when the schema declares neither.
fn additional_properties_pattern(schema: &Map<String, Value>) -> Option<Regex> {
    let mut patterns: Vec<String> = Vec::new();
    if let Some(pattern_properties) = schema.get("patternProperties").and_then(Value::as_object) {
        for pattern in pattern_properties.keys() {
            // A `patternProperties` key can use JavaScript-only syntax (lookaround,
            // backreferences) that the Rust `regex` crate cannot compile. Such a key is
            // skipped individually so the remaining alternatives still work; the
            // consequence is limited to keys only that pattern would have matched.
            if Regex::new(pattern).is_ok() {
                patterns.push(pattern.clone());
            }
        }
    }
    patterns.extend(
        schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| {
                properties
                    .keys()
                    .map(|key| format!("^{}$", escape_property_key(key)))
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default(),
    );
    if patterns.is_empty() {
        // `GetPropertiesPattern` returns `(?!)` when neither keyword declares a pattern;
        // the caller then treats every key as additional.
        return None;
    }
    Regex::new(&format!("({})", patterns.join("|"))).ok()
}

/// `CheckPattern` (typebox/build/schema/engine/pattern.mjs:14) builds
/// `new RegExp(schema.pattern, "u")` and calls `.test(value)` - an unanchored search.
///
/// JavaScript-only syntax (lookaround, backreferences) cannot be compiled by the
/// Rust `regex` crate, which is the only regular-expression engine the port has, so
/// such a pattern is reported instead of being silently ignored.
fn matches_pattern(pattern: &str, text: &str) -> Result<bool, String> {
    match Regex::new(pattern) {
        Ok(regex) => Ok(regex.is_match(text)),
        Err(error) => Err(error.to_string()),
    }
}

/// The ported `CheckSchema`/`ErrorSchema` pair
/// (typebox/build/schema/engine/schema.mjs:249-292 and the matching `ErrorSchema`).
///
/// Evaluation order follows the reference: `type` short-circuits, then one flat
/// conjunction ordered object keywords, array keywords, string keywords, number
/// keywords, `const`, `enum`, `allOf`, `anyOf`, `oneOf`. Every keyword is guarded by
/// its instance type, exactly as the reference guards them
/// (`!G.IsObject(value) || ...`, `!G.IsArray(value) || ...`, `!G.IsString(value) || ...`).
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

    // ---- object keywords -------------------------------------------------
    // `G.IsObject(value) && !G.IsArray(value)` in the reference.
    if let Value::Object(value_object) = value {
        if let Some(required) = schema_object.get("required").and_then(Value::as_array) {
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

        // TypeBox `CheckAdditionalProperties`
        // (typebox/build/schema/engine/additionalProperties.mjs:80) runs for every object
        // instance, whatever `properties` says: a key the `GetPropertiesPattern` union
        // (additionalProperties.mjs:23) does not match must still satisfy
        // `schema.additionalProperties`. `IsAdditionalProperties`
        // (typebox/build/schema/types/additionalProperties.mjs:11) accepts only a boolean
        // or a schema object, and `false` is the strict case this port was missing.
        // TypeBox reports ONE error at the object's own path whose `params` lists the
        // offending keys; a failure inside an `additionalProperties` schema is folded
        // into that same error rather than reported per key.
        let declared = schema_object.get("additionalProperties");
        let is_declared_schema = matches!(declared, Some(Value::Bool(_)) | Some(Value::Object(_)));
        if is_declared_schema && !matches!(declared, Some(Value::Bool(true))) {
            let pattern = additional_properties_pattern(schema_object);
            let mut offending: Vec<Value> = Vec::new();
            for (key, property_value) in value_object {
                if pattern.as_ref().is_some_and(|regex| regex.is_match(key)) {
                    continue;
                }
                let accepted = match declared {
                    Some(Value::Object(additional_schema)) => {
                        let mut nested_errors = Vec::new();
                        validate_against_schema(
                            property_value,
                            &Value::Object(additional_schema.clone()),
                            "",
                            &mut nested_errors,
                        );
                        nested_errors.is_empty()
                    }
                    _ => false,
                };
                if !accepted {
                    offending.push(Value::String(key.clone()));
                }
            }
            if !offending.is_empty() {
                let mut params = Map::new();
                params.insert("additionalProperties".to_string(), Value::Array(offending));
                errors.push(JsonSchemaError {
                    keyword: "additionalProperties".to_string(),
                    instance_path: instance_path.to_string(),
                    // `Locale.en_US` line 5; `params.additionalProperties` carries the
                    // offending keys, as in `ErrorAdditionalProperties`
                    // (schema/engine/additionalProperties.mjs:91).
                    message: "must not have additional properties".to_string(),
                    params,
                });
            }
        }

        // TypeBox `CheckPatternProperties`
        // (typebox/build/schema/engine/patternProperties.mjs:23) validates every key a
        // `patternProperties` pattern matches against that pattern's schema; the
        // reference compiles `new RegExp(pattern, "u")` and reports sub-schema errors
        // at the child path, which is what this block does.
        if let Some(pattern_properties) = schema_object
            .get("patternProperties")
            .and_then(Value::as_object)
        {
            for (pattern, pattern_schema) in pattern_properties {
                for (key, property_value) in value_object {
                    if matches!(matches_pattern(pattern, key), Ok(true)) {
                        let child_path = format!("{}/{}", instance_path, key);
                        validate_against_schema(
                            property_value,
                            pattern_schema,
                            &child_path,
                            errors,
                        );
                    }
                }
            }
        }

        if let Some(Value::Object(properties)) = schema_object.get("properties") {
            for (key, property_schema) in properties {
                if let Some(property_value) = value_object.get(key) {
                    let child_path = format!("{}/{}", instance_path, key);
                    validate_against_schema(property_value, property_schema, &child_path, errors);
                }
            }
        }
    }

    // ---- array keywords --------------------------------------------------
    // Order matches the reference `ErrorSchema` (schema/engine/schema.mjs): `items`
    // runs before `maxItems`, then `minItems`.
    if let Value::Array(items) = value {
        if let Some(items_schema) = schema_object.get("items") {
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

        if let Some(max_items) = schema_object.get("maxItems").and_then(Value::as_f64) {
            if (items.len() as f64) > max_items {
                // `Locale.en_US` line 17.
                errors.push(JsonSchemaError {
                    keyword: "maxItems".to_string(),
                    instance_path: instance_path.to_string(),
                    message: format!(
                        "must not have more than {} items",
                        js_number_text(max_items)
                    ),
                    params: Map::new(),
                });
            }
        }

        if let Some(min_items) = schema_object.get("minItems").and_then(Value::as_f64) {
            if (items.len() as f64) < min_items {
                // `Locale.en_US` line 21.
                errors.push(JsonSchemaError {
                    keyword: "minItems".to_string(),
                    instance_path: instance_path.to_string(),
                    message: format!(
                        "must not have fewer than {} items",
                        js_number_text(min_items)
                    ),
                    params: Map::new(),
                });
            }
        }
    }

    // ---- string keywords -------------------------------------------------
    // `CheckSchema` guards these with `!G.IsString(value) || ...`
    // (schema/engine/schema.mjs:270-273), so a non-string instance ignores them.
    if let Value::String(text) = value {
        if let Some(max_length) = schema_object.get("maxLength").and_then(Value::as_f64) {
            if !string_has_max_length(&text.encode_utf16().collect::<Vec<u16>>(), max_length) {
                // `Locale.en_US` line 18.
                errors.push(JsonSchemaError {
                    keyword: "maxLength".to_string(),
                    instance_path: instance_path.to_string(),
                    message: format!(
                        "must not have more than {} characters",
                        js_number_text(max_length)
                    ),
                    params: Map::new(),
                });
            }
        }

        if let Some(min_length) = schema_object.get("minLength").and_then(Value::as_f64) {
            if !string_has_min_length(&text.encode_utf16().collect::<Vec<u16>>(), min_length) {
                // `Locale.en_US` line 22.
                errors.push(JsonSchemaError {
                    keyword: "minLength".to_string(),
                    instance_path: instance_path.to_string(),
                    message: format!(
                        "must not have fewer than {} characters",
                        js_number_text(min_length)
                    ),
                    params: Map::new(),
                });
            }
        }

        // `CheckPattern` (typebox/build/schema/engine/pattern.mjs:14) builds
        // `new RegExp(schema.pattern, "u")` and calls `.test(value)`: an unanchored
        // search. JavaScript-only syntax (lookaround, backreferences) cannot be
        // compiled by the Rust `regex` crate, which is the only regular-expression
        // engine the port has, so such a pattern is reported instead of skipped.
        if let Some(pattern) = schema_object.get("pattern").and_then(Value::as_str) {
            match matches_pattern(pattern, text) {
                Ok(true) => {}
                // `Locale.en_US` line 28.
                Ok(false) => errors.push(JsonSchemaError {
                    keyword: "pattern".to_string(),
                    instance_path: instance_path.to_string(),
                    message: format!("must match pattern \"{}\"", pattern),
                    params: Map::new(),
                }),
                Err(reason) => errors.push(JsonSchemaError {
                    keyword: "pattern".to_string(),
                    instance_path: instance_path.to_string(),
                    message: format!(
                        "must match pattern \"{}\" (unsupported by the Rust regex engine: {})",
                        pattern, reason
                    ),
                    params: Map::new(),
                }),
            }
        }
    }

    // NOT ported in this change: `format`. TypeBox `CheckFormat`
    // (typebox/build/schema/engine/format.mjs) consults the format registry
    // (typebox/build/format/_registry.mjs, whose `Reset()` at line 40 registers
    // `date-time`, `date`, `duration`, `email`, `hostname`, `idn-email`,
    // `idn-hostname`, `ipv4`, `ipv6`, `iri-reference`, `iri`,
    // `json-pointer-uri-fragment`, `json-pointer`, `regex`, `relative-json-pointer`,
    // `time`, `uri-reference`, `uri-template`, `uri`, `url`, `uuid`); an unregistered
    // format is accepted by TypeBox (`format/_registry.mjs` `Test` returns true).
    //
    // The missing owner symbol is that registry: ~34 KB of validators including the
    // IDNA/punycode tables in typebox/build/format/_idna.mjs, with no Rust counterpart
    // in the port. A `format` keyword therefore stays unchecked here, which keeps
    // VALID values accepted (matching TypeBox) but still accepts an invalid value of a
    // registered format. Rejecting every `format` value instead would break working
    // calls, so it is deliberately not done. Reported to the lead as UNRESOLVED.
    //
    // No tool schema in packages/{agent,ai,coding-agent}/src declares `format` today,
    // so the gap has no current in-tree trigger.

    // ---- value keywords --------------------------------------------------
    // `CheckConst` (schema/engine/const.mjs:15) is part of `CheckSchema` for every
    // instance type (schema/engine/schema.mjs:282) and runs before `enum`; `IsConst`
    // only requires the key to be present.
    if let Some(const_value) = schema_object.get("const") {
        if !deep_equal(value, const_value) {
            errors.push(JsonSchemaError {
                keyword: "const".to_string(),
                instance_path: instance_path.to_string(),
                // `Locale.en_US` line 8.
                message: "must be equal to constant".to_string(),
                params: Map::new(),
            });
        }
    }

    // `CheckEnum` (schema/engine/enum.mjs:20) compares with `Guard.IsEqual` for
    // primitive options and `Guard.IsDeepEqual` otherwise, exactly as `deep_equal` does.
    if let Some(enum_values) = schema_object.get("enum").and_then(Value::as_array) {
        if !enum_values.iter().any(|option| deep_equal(value, option)) {
            errors.push(JsonSchemaError {
                keyword: "enum".to_string(),
                instance_path: instance_path.to_string(),
                message: "Expected a value from the enum".to_string(),
                params: Map::new(),
            });
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

    /// `Guard.IsDeepEqual` (guard.mjs:229) treats an array's `length` as an own
    /// property via `Keys` (guard.mjs:7721), so `{}` and `[]` are NOT deeply equal and
    /// `{"0": 1, "length": 1}` IS equal to `[1]`. Verified against the reference
    /// `Guard.IsDeepEqual` in typebox 1.3.10.
    #[test]
    fn deep_equal_matches_typebox_guard_semantics() {
        assert!(!deep_equal(&json!({}), &json!([])));
        assert!(!deep_equal(&json!([]), &json!({})));
        assert!(deep_equal(&json!({"0": 1, "length": 1}), &json!([1])));
        assert!(!deep_equal(&json!({"0": 1}), &json!([1])));
        assert!(deep_equal(&json!(1), &json!(1.0)));
        assert!(!deep_equal(&json!(0), &json!(false)));
        assert!(!deep_equal(&json!(""), &json!(null)));
        assert!(deep_equal(
            &json!({"a": 1, "b": 2}),
            &json!({"b": 2, "a": 1})
        ));
        assert!(!deep_equal(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
        assert!(!deep_equal(&json!([1, 2]), &json!([2, 1])));
        assert!(deep_equal(
            &json!({"a": [1, {"b": 2}]}),
            &json!({"a": [1, {"b": 2}]})
        ));
    }

    /// `CheckConst` (typebox/build/schema/engine/const.mjs:15) is applied for every
    /// instance type by `CheckSchema` (schema/engine/schema.mjs:282), and
    /// `IsConst` (schema/types/const.mjs:10) only requires the key to exist.
    /// TypeBox message: `Locale.en_US` line 8.
    #[test]
    fn const_keyword_matches_typebox() {
        let accepted: [(Value, Value); 5] = [
            (json!({"const": "x"}), json!("x")),
            (json!({"const": 1}), json!(1.0)),
            (json!({"const": {"a": 1}}), json!({"a": 1})),
            (json!({"const": [1, 2]}), json!([1, 2])),
            (json!({"const": null}), json!(null)),
        ];
        for (schema, input) in accepted {
            let tool = tool_with_schema(schema.clone());
            let result = validate_tool_arguments(&tool, &tool_call(input.clone()))
                .unwrap_or_else(|error| panic!("schema {} input {} -> {}", schema, input, error));
            assert_eq!(result, json!({"value": input}), "schema {}", schema);
        }

        let rejected: [(Value, Value); 5] = [
            (json!({"const": "x"}), json!("y")),
            (json!({"const": 1}), json!(1.5)),
            (json!({"const": {"a": 1}}), json!({"a": 2})),
            (json!({"const": [1, 2]}), json!([2, 1])),
            (json!({"const": false}), json!(0)),
        ];
        for (schema, input) in rejected {
            let tool = tool_with_schema(schema.clone());
            let error = match validate_tool_arguments(&tool, &tool_call(input.clone())) {
                Ok(_) => panic!("schema {} accepted input {}", schema, input),
                Err(error) => error,
            };
            assert!(
                error.contains("  - value: must be equal to constant"),
                "schema {} input {} -> {}",
                schema,
                input,
                error
            );
        }
    }

    /// `CheckAdditionalProperties`
    /// (typebox/build/schema/engine/additionalProperties.mjs:80) rejects any key the
    /// `GetPropertiesPattern` union (additionalProperties.mjs:23) does not match when
    /// `additionalProperties` is `false`, and validates it against the schema when it
    /// is an object. TypeBox message: `Locale.en_US` line 5.
    #[test]
    fn additional_properties_false_rejects_extra_keys() {
        let schema = json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
            "additionalProperties": false
        });
        let tool = |schema: &Value| Tool {
            name: "edit".to_string(),
            description: String::new(),
            parameters: schema.clone(),
        };

        let accepted = json!({"path": "a.txt"});
        assert_eq!(
            validate_tool_arguments(
                &tool(&schema),
                &ToolCall::new("id", "edit", accepted.as_object().unwrap().clone())
            )
            .expect("declared key only"),
            accepted
        );

        let rejected = json!({"path": "a.txt", "extra": 1});
        let error = validate_tool_arguments(
            &tool(&schema),
            &ToolCall::new("id", "edit", rejected.as_object().unwrap().clone()),
        )
        .expect_err("extra key must be rejected");
        assert!(
            error.contains("  - root: must not have additional properties"),
            "{}",
            error
        );

        // Nested: the `edit` tool's `edits[].{additionalProperties:false}` (TS
        // packages/coding-agent/src/core/tools/edit.ts:44) must reject too.
        let nested = json!({
            "type": "object",
            "properties": {
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"oldText": {"type": "string"}},
                        "required": ["oldText"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["edits"]
        });
        let nested_accepted = json!({"edits": [{"oldText": "x"}]});
        assert!(validate_tool_arguments(
            &tool(&nested),
            &ToolCall::new("id", "edit", nested_accepted.as_object().unwrap().clone())
        )
        .is_ok());
        let nested_rejected = json!({"edits": [{"oldText": "x", "bogus": true}]});
        let error = validate_tool_arguments(
            &tool(&nested),
            &ToolCall::new("id", "edit", nested_rejected.as_object().unwrap().clone()),
        )
        .expect_err("nested extra key must be rejected");
        assert!(
            error.contains("  - edits.0: must not have additional"),
            "{}",
            error
        );
    }

    /// An `additionalProperties` SCHEMA validates each extra key; `true` accepts any
    /// extra key. Same reference site as the `false` case above.
    #[test]
    fn additional_properties_schema_and_true_are_honoured() {
        let with_schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a"],
            "additionalProperties": {"type": "number"}
        });
        let tool = Tool {
            name: "echo".to_string(),
            description: String::new(),
            parameters: with_schema,
        };
        let accepted = json!({"a": "x", "b": 2});
        assert_eq!(
            validate_tool_arguments(
                &tool,
                &ToolCall::new("id", "echo", accepted.as_object().unwrap().clone())
            )
            .expect("extra key of the declared type"),
            accepted
        );
        let rejected = json!({"a": "x", "b": "s"});
        let error = validate_tool_arguments(
            &tool,
            &ToolCall::new("id", "echo", rejected.as_object().unwrap().clone()),
        )
        .expect_err("extra key of the wrong type must be rejected");
        assert!(
            error.contains("  - root: must not have additional properties"),
            "{}",
            error
        );

        // `additionalProperties: true` (TS packages/coding-agent/src/core/tools/acp-mcp.ts:84).
        let truthy = Tool {
            name: "echo".to_string(),
            description: String::new(),
            parameters: json!({
                "type": "object",
                "properties": {"a": {"type": "string"}},
                "required": ["a"],
                "additionalProperties": true
            }),
        };
        let any_extra = json!({"a": "x", "b": {"c": [1]}});
        assert_eq!(
            validate_tool_arguments(
                &truthy,
                &ToolCall::new("id", "echo", any_extra.as_object().unwrap().clone())
            )
            .expect("additionalProperties true accepts extra keys"),
            any_extra
        );
    }

    /// `CheckMinLength`/`CheckMaxLength` (schema/engine/minLength.mjs:12,
    /// maxLength.mjs:12) count GRAPHEME CLUSTERS via
    /// `Guard.IsMinLength`/`IsMaxLength` (guard.mjs) and
    /// `String.IsMinLengthFast`/`IsMaxLengthFast` (guard/string.mjs:147,165). Messages:
    /// `Locale.en_US` lines 22 and 18.
    #[test]
    fn min_and_max_length_count_grapheme_clusters() {
        for (schema, accepted, rejected) in [
            (
                json!({"type": "string", "minLength": 3}),
                json!("abc"),
                json!("ab"),
            ),
            (
                json!({"type": "string", "maxLength": 2}),
                json!("ab"),
                json!("abc"),
            ),
            // `e` + COMBINING ACUTE is ONE grapheme cluster (guard/string.mjs:34).
            (
                json!({"type": "string", "minLength": 1}),
                json!("e\u{0301}"),
                json!(""),
            ),
            (
                json!({"type": "string", "maxLength": 1}),
                json!("e\u{0301}"),
                json!("ee"),
            ),
            // Astral character: one cluster, two UTF-16 code units.
            (
                json!({"type": "string", "maxLength": 1}),
                json!("\u{1F600}"),
                json!("\u{1F600}\u{1F600}"),
            ),
        ] {
            let tool = tool_with_schema(schema.clone());
            assert_eq!(
                validate_tool_arguments(&tool, &tool_call(accepted.clone()))
                    .unwrap_or_else(|error| panic!("{} rejected {}", schema, error)),
                json!({"value": accepted}),
                "schema {}",
                schema
            );
            let error = validate_tool_arguments(&tool, &tool_call(rejected.clone()))
                .expect_err("must be rejected");
            assert!(error.starts_with("Validation failed"), "{}", error);
        }

        let tool = tool_with_schema(json!({"type": "string", "minLength": 3}));
        let error = validate_tool_arguments(&tool, &tool_call(json!("ab"))).unwrap_err();
        assert!(
            error.contains("  - value: must not have fewer than 3 characters"),
            "{}",
            error
        );
        let tool = tool_with_schema(json!({"type": "string", "maxLength": 2}));
        let error = validate_tool_arguments(&tool, &tool_call(json!("abc"))).unwrap_err();
        assert!(
            error.contains("  - value: must not have more than 2 characters"),
            "{}",
            error
        );
    }

    /// `CheckPattern` (typebox/build/schema/engine/pattern.mjs:14) builds
    /// `new RegExp(schema.pattern, "u")` and calls `.test(...)`, an unanchored search.
    /// Message: `Locale.en_US` line 28.
    #[test]
    fn pattern_keyword_is_an_unanchored_search() {
        let anchored = tool_with_schema(json!({"type": "string", "pattern": "^a+$"}));
        assert_eq!(
            validate_tool_arguments(&anchored, &tool_call(json!("aa"))).expect("matches"),
            json!({"value": "aa"})
        );
        let error = validate_tool_arguments(&anchored, &tool_call(json!("b"))).unwrap_err();
        assert!(
            error.contains("  - value: must match pattern \"^a+$\""),
            "{}",
            error
        );

        // Unanchored: `RegExp.test` searches, so `"b"` matches `"abc"`.
        let search = tool_with_schema(json!({"type": "string", "pattern": "b"}));
        assert_eq!(
            validate_tool_arguments(&search, &tool_call(json!("abc"))).expect("searches"),
            json!({"value": "abc"})
        );
        // `\\d` is portable to the Rust `regex` crate.
        let digits = tool_with_schema(json!({"type": "string", "pattern": "\\d+"}));
        assert_eq!(
            validate_tool_arguments(&digits, &tool_call(json!("abc123"))).expect("digits"),
            json!({"value": "abc123"})
        );
    }

    /// `CheckMinItems`/`CheckMaxItems` (schema/engine/minItems.mjs:12,
    /// maxItems.mjs:12) compare `value.length`. Messages: `Locale.en_US` lines 21, 17.
    #[test]
    fn min_and_max_items_match_typebox() {
        let min =
            tool_with_schema(json!({"type": "array", "minItems": 2, "items": {"type": "number"}}));
        assert_eq!(
            validate_tool_arguments(&min, &tool_call(json!([1, 2]))).expect("enough items"),
            json!({"value": [1, 2]})
        );
        let error = validate_tool_arguments(&min, &tool_call(json!([1]))).unwrap_err();
        assert!(
            error.contains("  - value: must not have fewer than 2 items"),
            "{}",
            error
        );

        let max =
            tool_with_schema(json!({"type": "array", "maxItems": 1, "items": {"type": "number"}}));
        assert_eq!(
            validate_tool_arguments(&max, &tool_call(json!([1]))).expect("few enough items"),
            json!({"value": [1]})
        );
        let error = validate_tool_arguments(&max, &tool_call(json!([1, 2]))).unwrap_err();
        assert!(
            error.contains("  - value: must not have more than 1 items"),
            "{}",
            error
        );
    }

    /// The reference guards every keyword by instance type
    /// (`!G.IsString(value) || ...`, `!G.IsArray(value) || ...`,
    /// `!(G.IsObject(value) && !G.IsArray(value)) || ...` in
    /// schema/engine/schema.mjs:252-273), so a non-matching instance ignores them.
    #[test]
    fn type_guard_keywords_are_ignored_for_other_instances() {
        assert_eq!(
            validate_tool_arguments(
                &tool_with_schema(json!({"minLength": 3})),
                &tool_call(json!(5))
            )
            .expect("minLength ignores numbers"),
            json!({"value": 5})
        );
        assert_eq!(
            validate_tool_arguments(
                &tool_with_schema(json!({"minItems": 2})),
                &tool_call(json!("ab"))
            )
            .expect("minItems ignores strings"),
            json!({"value": "ab"})
        );
        assert_eq!(
            validate_tool_arguments(
                &tool_with_schema(json!({"additionalProperties": false})),
                &tool_call(json!("x"))
            )
            .expect("additionalProperties ignores non-objects"),
            json!({"value": "x"})
        );
        assert_eq!(
            validate_tool_arguments(
                &tool_with_schema(json!({"maxItems": 0})),
                &tool_call(json!({}))
            )
            .expect("maxItems ignores objects"),
            json!({"value": {}})
        );
    }

    /// `CheckPatternProperties` (schema/engine/patternProperties.mjs:23) validates the
    /// keys a `patternProperties` pattern matches against that pattern's schema.
    #[test]
    fn pattern_properties_validate_matching_keys() {
        let tool = tool_with_schema(json!({
            "type": "object",
            "patternProperties": {"^x": {"type": "number"}},
            "additionalProperties": false
        }));
        let accepted = json!({"xb": 2});
        assert_eq!(
            validate_tool_arguments(&tool, &tool_call(accepted.clone())).expect("matching pattern"),
            json!({"value": accepted})
        );
        let rejected = json!({"xb": "no"});
        let error = validate_tool_arguments(&tool, &tool_call(rejected)).unwrap_err();
        assert!(error.contains("  - value.xb:"), "{}", error);

        // `^x` does not match `yb`, so `additionalProperties: false` rejects it
        // (additionalProperties.mjs:23 keeps `patternProperties` keys verbatim).
        let error = validate_tool_arguments(&tool, &tool_call(json!({"yb": 2}))).unwrap_err();
        assert!(
            error.contains("  - value: must not have additional properties"),
            "{}",
            error
        );
    }

    /// `GetPropertiesPattern` (additionalProperties.mjs:20) escapes the declared
    /// property keys, so a key with regex metacharacters is still recognised.
    #[test]
    fn additional_properties_pattern_escapes_declared_keys() {
        let tool = tool_with_schema(json!({
            "type": "object",
            "properties": {"a.b": {"type": "string"}},
            "additionalProperties": false
        }));
        let accepted = json!({"a.b": "1"});
        assert_eq!(
            validate_tool_arguments(&tool, &tool_call(accepted.clone())).expect("escaped dot"),
            json!({"value": accepted})
        );
        let error = validate_tool_arguments(&tool, &tool_call(json!({"axb": "1"}))).unwrap_err();
        assert!(
            error.contains("must not have additional properties"),
            "{}",
            error
        );
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
