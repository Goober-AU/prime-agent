//! Port of packages/coding-agent/src/core/model-registry.ts
//!
//! Model registry - manages built-in and custom models, provides API key resolution.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::api_registry::ApiStreamSimpleFunction;
use pi_ai::types::{
    Api, Compat, Context, Model, ModelCost, NativeCompactionCapability, SimpleStreamOptions, ThinkingLevelMap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::core::auth_storage::{
    register_oauth_provider, resolve_config_value_or_throw, resolve_config_value_uncached,
    resolve_headers_or_throw, AuthSourceToken, AuthStatus, AuthStorage,
};
use crate::core::prime_inference_auth::{FetchFn, HttpRequest, PRIME_INFERENCE_PROVIDER_ID};
use crate::core::prime_inference_model_catalog::{
    build_prime_inference_models, merge_prime_inference_models, parse_prime_inference_model_catalog,
    read_cached_prime_inference_models, refresh_prime_inference_models,
};
use crate::core::prime_inference_models::{
    fetch_authorized_private_prime_inference_models, get_private_prime_inference_models,
    is_private_prime_inference_model,
};
use crate::core::provider_display_names::built_in_provider_display_names;

// ---------------------------------------------------------------------------
// pi-ai boundary: models.ts `getProviders()` / `getModels(provider)`
// ---------------------------------------------------------------------------

/// `getProviders()` from packages/ai/src/models.ts.
fn get_providers() -> Vec<String> {
    pi_ai::models_generated::models().keys().cloned().collect()
}

/// `getModels(provider)` from packages/ai/src/models.ts.
fn get_models(provider: &str) -> Vec<Model> {
    pi_ai::models_generated::models_for_provider(provider)
        .map(|provider_models| provider_models.values().cloned().collect())
        .unwrap_or_default()
}

/// `resetApiProviders()` from packages/ai/src/api-registry.ts.
fn reset_api_providers() {
    pi_ai::api_registry::clear_api_providers();
}

// ---------------------------------------------------------------------------
// Schema types (typebox -> serde structs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDefinition {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelCost>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_compaction: Option<NativeCompactionCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Compat>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<PartialModelCost>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Compat>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PartialModelCost {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(rename = "cacheRead", skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(rename = "cacheWrite", skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Compat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<ModelDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_overrides: Option<IndexMap<String, ModelOverride>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelsConfig {
    pub providers: IndexMap<String, ProviderConfig>,
}

// ---------------------------------------------------------------------------
// models.json schema validation (typebox Compile(ModelsConfigSchema) equivalent)
// ---------------------------------------------------------------------------

fn validation_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{}.{}", path, key)
    }
}

fn expect_string(value: &Value, path: &str, errors: &mut Vec<String>, min_length: usize) -> Option<String> {
    match value.as_str() {
        Some(text) if text.chars().count() >= min_length => Some(text.to_string()),
        Some(_) => {
            errors.push(format!("  - {}: Expected string length greater or equal to {}", path, min_length));
            None
        }
        None => {
            errors.push(format!("  - {}: Expected string", path));
            None
        }
    }
}

fn expect_number(value: &Value, path: &str, errors: &mut Vec<String>) -> Option<f64> {
    match value.as_f64() {
        Some(number) if number.is_finite() => Some(number),
        _ => {
            errors.push(format!("  - {}: Expected number", path));
            None
        }
    }
}

fn expect_integer(value: &Value, path: &str, minimum: f64, errors: &mut Vec<String>) {
    match value.as_f64() {
        Some(number) if number.is_finite() && number.fract() == 0.0 && number >= minimum => {}
        _ => errors.push(format!("  - {}: Expected integer greater or equal to {}", path, minimum)),
    }
}

fn expect_boolean(value: &Value, path: &str, errors: &mut Vec<String>) {
    if !value.is_boolean() {
        errors.push(format!("  - {}: Expected boolean", path));
    }
}

fn expect_string_record(value: &Value, path: &str, errors: &mut Vec<String>) {
    match value.as_object() {
        Some(object) => {
            for (key, entry) in object {
                if !entry.is_string() {
                    errors.push(format!("  - {}: Expected string", validation_path(path, key)));
                }
            }
        }
        None => errors.push(format!("  - {}: Expected object", path)),
    }
}

fn validate_thinking_level_map(value: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(object) = value.as_object() else {
        errors.push(format!("  - {}: Expected object", path));
        return;
    };
    for (key, entry) in object {
        if !entry.is_null() && !entry.is_string() {
            errors.push(format!(
                "  - {}: Expected union",
                validation_path(path, key)
            ));
        }
    }
}

fn validate_compat(value: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(object) = value.as_object() else {
        errors.push(format!("  - {}: Expected union", path));
        return;
    };
    for key in [
        "supportsStore",
        "supportsDeveloperRole",
        "supportsReasoningEffort",
        "supportsUsageInStreaming",
        "requiresToolResultName",
        "requiresAssistantAfterToolResult",
        "requiresThinkingAsText",
        "requiresReasoningContentOnAssistantMessages",
        "supportsStrictMode",
        "supportsLongCacheRetention",
        "sendSessionIdHeader",
        "supportsEagerToolInputStreaming",
    ] {
        if let Some(entry) = object.get(key) {
            expect_boolean(entry, &validation_path(path, key), errors);
        }
    }
    if let Some(entry) = object.get("maxTokensField") {
        let valid = matches!(
            entry.as_str(),
            Some("max_completion_tokens") | Some("max_tokens")
        );
        if !valid {
            errors.push(format!(
                "  - {}: Expected union",
                validation_path(path, "maxTokensField")
            ));
        }
    }
    if let Some(entry) = object.get("thinkingFormat") {
        let valid = matches!(
            entry.as_str(),
            Some("openai")
                | Some("openrouter")
                | Some("deepseek")
                | Some("zai")
                | Some("qwen")
                | Some("qwen-chat-template")
        );
        if !valid {
            errors.push(format!(
                "  - {}: Expected union",
                validation_path(path, "thinkingFormat")
            ));
        }
    }
    if let Some(entry) = object.get("cacheControlFormat") {
        if entry.as_str() != Some("anthropic") {
            errors.push(format!(
                "  - {}: Expected literal",
                validation_path(path, "cacheControlFormat")
            ));
        }
    }
    if let Some(entry) = object.get("openRouterRouting") {
        match entry.as_object() {
            Some(routing) => {
                for key in ["allow_fallbacks", "require_parameters", "zdr", "enforce_distillable_text"] {
                    if let Some(inner) = routing.get(key) {
                        expect_boolean(inner, &validation_path(&validation_path(path, "openRouterRouting"), key), errors);
                    }
                }
                if let Some(inner) = routing.get("data_collection") {
                    let valid = matches!(inner.as_str(), Some("deny") | Some("allow"));
                    if !valid {
                        errors.push(format!(
                            "  - {}: Expected union",
                            validation_path(&validation_path(path, "openRouterRouting"), "data_collection")
                        ));
                    }
                }
            }
            None => errors.push(format!(
                "  - {}: Expected object",
                validation_path(path, "openRouterRouting")
            )),
        }
    }
    if let Some(entry) = object.get("vercelGatewayRouting") {
        if !entry.is_object() {
            errors.push(format!(
                "  - {}: Expected object",
                validation_path(path, "vercelGatewayRouting")
            ));
        }
    }
}

fn validate_native_compaction_schema(value: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(object) = value.as_object() else {
        errors.push(format!("  - {}: Expected object", path));
        return;
    };
    for key in ["protocol", "provider", "model", "endpoint", "apiVersion"] {
        match object.get(key) {
            None => errors.push(format!("  - {}: Expected required property", validation_path(path, key))),
            Some(entry) => {
                if key == "protocol" && entry.as_str() != Some("openai-responses-compact-v1") {
                    errors.push(format!("  - {}: Expected literal", validation_path(path, key)));
                } else if key == "apiVersion" && entry.as_str() != Some("v1") {
                    errors.push(format!("  - {}: Expected literal", validation_path(path, key)));
                } else if !entry.is_string() {
                    errors.push(format!("  - {}: Expected string", validation_path(path, key)));
                }
            }
        }
    }
    if let Some(entry) = object.get("enabled") {
        expect_boolean(entry, &validation_path(path, "enabled"), errors);
    } else {
        errors.push(format!("  - {}: Expected required property", validation_path(path, "enabled")));
    }
    match object.get("validation") {
        Some(entry) => {
            let valid = matches!(
                entry.as_str(),
                Some("unverified") | Some("documentation-verified") | Some("live-verified")
            );
            if !valid {
                errors.push(format!("  - {}: Expected union", validation_path(path, "validation")));
            }
        }
        None => errors.push(format!(
            "  - {}: Expected required property",
            validation_path(path, "validation")
        )),
    }
}

fn validate_model_definition(value: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(object) = value.as_object() else {
        errors.push(format!("  - {}: Expected object", path));
        return;
    };
    match object.get("id") {
        None => errors.push(format!("  - {}: Expected required property", validation_path(path, "id"))),
        Some(entry) => {
            expect_string(entry, &validation_path(path, "id"), errors, 1);
        }
    }
    for key in ["name", "api", "baseUrl"] {
        if let Some(entry) = object.get(key) {
            expect_string(entry, &validation_path(path, key), errors, 1);
        }
    }
    if let Some(entry) = object.get("reasoning") {
        expect_boolean(entry, &validation_path(path, "reasoning"), errors);
    }
    if let Some(entry) = object.get("thinkingLevelMap") {
        validate_thinking_level_map(entry, &validation_path(path, "thinkingLevelMap"), errors);
    }
    if let Some(entry) = object.get("input") {
        match entry.as_array() {
            Some(items) => {
                for (index, item) in items.iter().enumerate() {
                    let valid = matches!(item.as_str(), Some("text") | Some("image"));
                    if !valid {
                        errors.push(format!(
                            "  - {}: Expected union",
                            validation_path(&validation_path(path, "input"), &index.to_string())
                        ));
                    }
                }
            }
            None => errors.push(format!("  - {}: Expected array", validation_path(path, "input"))),
        }
    }
    if let Some(entry) = object.get("cost") {
        match entry.as_object() {
            Some(cost) => {
                for key in ["input", "output", "cacheRead", "cacheWrite"] {
                    match cost.get(key) {
                        None => errors.push(format!(
                            "  - {}: Expected required property",
                            validation_path(&validation_path(path, "cost"), key)
                        )),
                        Some(inner) => {
                            expect_number(inner, &validation_path(&validation_path(path, "cost"), key), errors);
                        }
                    }
                }
            }
            None => errors.push(format!("  - {}: Expected object", validation_path(path, "cost"))),
        }
    }
    if let Some(entry) = object.get("contextWindow") {
        expect_number(entry, &validation_path(path, "contextWindow"), errors);
    }
    if let Some(entry) = object.get("maxInputTokens") {
        expect_integer(entry, &validation_path(path, "maxInputTokens"), 1.0, errors);
    }
    if let Some(entry) = object.get("maxTokens") {
        expect_number(entry, &validation_path(path, "maxTokens"), errors);
    }
    if let Some(entry) = object.get("nativeCompaction") {
        validate_native_compaction_schema(entry, &validation_path(path, "nativeCompaction"), errors);
    }
    if let Some(entry) = object.get("headers") {
        expect_string_record(entry, &validation_path(path, "headers"), errors);
    }
    if let Some(entry) = object.get("compat") {
        validate_compat(entry, &validation_path(path, "compat"), errors);
    }
}

fn validate_model_override(value: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(object) = value.as_object() else {
        errors.push(format!("  - {}: Expected object", path));
        return;
    };
    if let Some(entry) = object.get("name") {
        expect_string(entry, &validation_path(path, "name"), errors, 1);
    }
    if let Some(entry) = object.get("reasoning") {
        expect_boolean(entry, &validation_path(path, "reasoning"), errors);
    }
    if let Some(entry) = object.get("thinkingLevelMap") {
        validate_thinking_level_map(entry, &validation_path(path, "thinkingLevelMap"), errors);
    }
    if let Some(entry) = object.get("input") {
        match entry.as_array() {
            Some(items) => {
                for (index, item) in items.iter().enumerate() {
                    let valid = matches!(item.as_str(), Some("text") | Some("image"));
                    if !valid {
                        errors.push(format!(
                            "  - {}: Expected union",
                            validation_path(&validation_path(path, "input"), &index.to_string())
                        ));
                    }
                }
            }
            None => errors.push(format!("  - {}: Expected array", validation_path(path, "input"))),
        }
    }
    if let Some(entry) = object.get("cost") {
        match entry.as_object() {
            Some(cost) => {
                for key in ["input", "output", "cacheRead", "cacheWrite"] {
                    if let Some(inner) = cost.get(key) {
                        expect_number(inner, &validation_path(&validation_path(path, "cost"), key), errors);
                    }
                }
            }
            None => errors.push(format!("  - {}: Expected object", validation_path(path, "cost"))),
        }
    }
    if let Some(entry) = object.get("contextWindow") {
        expect_number(entry, &validation_path(path, "contextWindow"), errors);
    }
    if let Some(entry) = object.get("maxInputTokens") {
        expect_integer(entry, &validation_path(path, "maxInputTokens"), 1.0, errors);
    }
    if let Some(entry) = object.get("maxTokens") {
        expect_number(entry, &validation_path(path, "maxTokens"), errors);
    }
    if let Some(entry) = object.get("headers") {
        expect_string_record(entry, &validation_path(path, "headers"), errors);
    }
    if let Some(entry) = object.get("compat") {
        validate_compat(entry, &validation_path(path, "compat"), errors);
    }
}

/// `Compile(ModelsConfigSchema)` equivalent: collect `  - <path>: <message>` lines.
pub fn validate_models_config_schema(value: &Value) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    let Some(root) = value.as_object() else {
        errors.push("  - root: Expected object".to_string());
        return errors;
    };
    let Some(providers) = root.get("providers") else {
        errors.push("  - providers: Expected required property".to_string());
        return errors;
    };
    let Some(providers) = providers.as_object() else {
        errors.push("  - providers: Expected object".to_string());
        return errors;
    };
    for (provider_name, provider) in providers {
        let path = provider_name.clone();
        let Some(provider) = provider.as_object() else {
            errors.push(format!("  - {}: Expected object", path));
            continue;
        };
        for key in ["name", "baseUrl", "apiKey", "api"] {
            if let Some(entry) = provider.get(key) {
                expect_string(entry, &validation_path(&path, key), &mut errors, 1);
            }
        }
        if let Some(entry) = provider.get("headers") {
            expect_string_record(entry, &validation_path(&path, "headers"), &mut errors);
        }
        if let Some(entry) = provider.get("compat") {
            validate_compat(entry, &validation_path(&path, "compat"), &mut errors);
        }
        if let Some(entry) = provider.get("authHeader") {
            expect_boolean(entry, &validation_path(&path, "authHeader"), &mut errors);
        }
        if let Some(entry) = provider.get("models") {
            match entry.as_array() {
                Some(items) => {
                    for (index, item) in items.iter().enumerate() {
                        validate_model_definition(
                            item,
                            &validation_path(&validation_path(&path, "models"), &index.to_string()),
                            &mut errors,
                        );
                    }
                }
                None => errors.push(format!("  - {}: Expected array", validation_path(&path, "models"))),
            }
        }
        if let Some(entry) = provider.get("modelOverrides") {
            match entry.as_object() {
                Some(overrides) => {
                    for (model_id, override_value) in overrides {
                        validate_model_override(
                            override_value,
                            &validation_path(&validation_path(&path, "modelOverrides"), model_id),
                            &mut errors,
                        );
                    }
                }
                None => errors.push(format!(
                    "  - {}: Expected object",
                    validation_path(&path, "modelOverrides")
                )),
            }
        }
    }
    errors
}

// ---------------------------------------------------------------------------
// Config helpers
// ---------------------------------------------------------------------------

/// Strip `//` line comments and trailing commas from JSON, leaving string
/// literals untouched.
pub fn strip_json_comments(input: &str) -> String {
    // Pass 1: drop `//` comments that are outside string literals.
    let mut without_comments = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '"' {
            without_comments.push(ch);
            index += 1;
            while index < chars.len() {
                let inner = chars[index];
                without_comments.push(inner);
                if inner == '\\' {
                    index += 1;
                    if index < chars.len() {
                        without_comments.push(chars[index]);
                        index += 1;
                    }
                    continue;
                }
                index += 1;
                if inner == '"' {
                    break;
                }
            }
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'/') {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        without_comments.push(ch);
        index += 1;
    }

    // Pass 2: drop a trailing comma before `}` or `]` that is outside a string.
    let chars: Vec<char> = without_comments.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '"' {
            out.push(ch);
            index += 1;
            while index < chars.len() {
                let inner = chars[index];
                out.push(inner);
                if inner == '\\' {
                    index += 1;
                    if index < chars.len() {
                        out.push(chars[index]);
                        index += 1;
                    }
                    continue;
                }
                index += 1;
                if inner == '"' {
                    break;
                }
            }
            continue;
        }
        if ch == ',' {
            let mut lookahead = index + 1;
            while lookahead < chars.len() && chars[lookahead].is_whitespace() {
                lookahead += 1;
            }
            if matches!(chars.get(lookahead), Some('}') | Some(']')) {
                index += 1;
                continue;
            }
        }
        out.push(ch);
        index += 1;
    }
    out
}

fn normalized_http_endpoint(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    let trimmed_path = url.path().trim_end_matches('/').to_string();
    let mut normalized = url.clone();
    normalized.set_path(&trimmed_path);
    let text = normalized.to_string();
    Some(text.strip_suffix('/').unwrap_or(&text).to_string())
}

/// The `modelDef` half of `validateNativeCompactionCapability` (both the
/// models.json `ModelDefinition` and the `registerProvider` model shape).
struct NativeCompactionModelRef<'a> {
    id: &'a str,
    api: Option<&'a str>,
    base_url: Option<&'a str>,
    capability: Option<&'a NativeCompactionCapability>,
}

/// The `providerConfig` half of `validateNativeCompactionCapability`.
struct NativeCompactionProviderRef<'a> {
    api: Option<&'a str>,
    base_url: Option<&'a str>,
}

impl ProviderConfig {
    fn native_compaction_provider_ref(&self) -> NativeCompactionProviderRef<'_> {
        NativeCompactionProviderRef {
            api: self.api.as_deref(),
            base_url: self.base_url.as_deref(),
        }
    }
}

impl ModelDefinition {
    fn native_compaction_model_ref(&self) -> NativeCompactionModelRef<'_> {
        NativeCompactionModelRef {
            id: &self.id,
            api: self.api.as_deref(),
            base_url: self.base_url.as_deref(),
            capability: self.native_compaction.as_ref(),
        }
    }
}

/// `validateNativeCompactionCapability(providerName, modelDef, providerConfig)`.
fn validate_native_compaction_capability(
    provider_name: &str,
    model_def: NativeCompactionModelRef<'_>,
    provider_config: NativeCompactionProviderRef<'_>,
) -> Result<(), String> {
    let Some(capability) = model_def.capability else {
        return Ok(());
    };
    let label = format!(
        "Provider {}, model {}: nativeCompaction",
        provider_name, model_def.id
    );
    if provider_name != "azure-openai-managed" || model_def.id != "gpt-6-astra" {
        return Err(format!(
            "{} is allowlisted only for azure-openai-managed/gpt-6-astra.",
            label
        ));
    }
    let api = model_def.api.or(provider_config.api);
    if api != Some("openai-responses") {
        return Err(format!("{} requires api \"openai-responses\".", label));
    }
    if capability.provider != provider_name || capability.model != model_def.id {
        return Err(format!(
            "{} provider/model ownership does not match its enclosing model.",
            label
        ));
    }
    if capability.enabled && capability.validation != pi_ai::types::NativeCompactionValidation::LiveVerified {
        return Err(format!(
            "{} cannot be enabled until validation is \"live-verified\".",
            label
        ));
    }
    let base_url = normalized_http_endpoint(model_def.base_url.or(provider_config.base_url).unwrap_or(""));
    let endpoint = normalized_http_endpoint(&capability.endpoint);
    let base_url_path_ok = base_url
        .as_deref()
        .and_then(|base| url::Url::parse(base).ok())
        .map(|url| url.path() == "/azure-openai/v1")
        .unwrap_or(false);
    if base_url.is_none() || endpoint.is_none() || !base_url_path_ok {
        return Err(format!(
            "{} requires a safe /azure-openai/v1 gateway base URL.",
            label
        ));
    }
    let expected = normalized_http_endpoint(&format!("{}/responses/compact", base_url.unwrap_or_default()));
    if endpoint != expected {
        return Err(format!(
            "{}.endpoint must equal the model base URL plus /responses/compact.",
            label
        ));
    }
    Ok(())
}

/// `interface ProviderOverride`.
#[derive(Debug, Clone, Default)]
struct ProviderOverride {
    base_url: Option<String>,
    compat: Option<Compat>,
}

#[derive(Debug, Clone, Default)]
struct ProviderRequestConfig {
    api_key: Option<String>,
    headers: Option<IndexMap<String, String>>,
    auth_header: Option<bool>,
}

#[derive(Clone)]
struct ProviderRequestAuthSource {
    source: String,
    configured: bool,
    label: Option<String>,
    identity_fingerprint: String,
    value_fingerprint: Option<String>,
    resolve_value_fingerprint: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
}

impl std::fmt::Debug for ProviderRequestAuthSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderRequestAuthSource")
            .field("source", &self.source)
            .field("configured", &self.configured)
            .field("label", &self.label)
            .field("identity_fingerprint", &self.identity_fingerprint)
            .field("value_fingerprint", &self.value_fingerprint)
            .finish()
    }
}

/// `type ResolvedRequestAuth`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedRequestAuth {
    pub ok: bool,
    pub api_key: Option<String>,
    pub headers: Option<IndexMap<String, String>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelCatalogSnapshot {
    pub models: Vec<Model>,
    pub configured_providers: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct CustomModelsResult {
    models: Vec<Model>,
    /// Providers with baseUrl/headers/apiKey overrides for built-in models.
    overrides: IndexMap<String, ProviderOverride>,
    /// Per-model overrides: provider -> modelId -> override.
    model_overrides: IndexMap<String, IndexMap<String, ModelOverride>>,
    error: Option<String>,
}

fn empty_custom_models_result(error: Option<String>) -> CustomModelsResult {
    CustomModelsResult {
        models: Vec::new(),
        overrides: IndexMap::new(),
        model_overrides: IndexMap::new(),
        error,
    }
}

/// `mergeCompat(baseCompat, overrideCompat)`.
///
/// The TypeScript does `{ ...base, ...override }` on the (already narrowed)
/// compat objects, then re-merges the two nested routing objects field by field.
pub fn merge_compat(base_compat: Option<&Compat>, override_compat: Option<&Compat>) -> Option<Compat> {
    let Some(override_compat) = override_compat else {
        return base_compat.cloned();
    };

    let base_value = base_compat
        .and_then(|compat| serde_json::to_value(compat).ok())
        .unwrap_or(Value::Object(Map::new()));
    let override_value = serde_json::to_value(override_compat).unwrap_or(Value::Object(Map::new()));

    let mut merged = match (base_value, override_value) {
        (Value::Object(base), Value::Object(over)) => {
            let mut merged = base;
            for (key, value) in over {
                merged.insert(key, value);
            }
            Value::Object(merged)
        }
        (_, over) => over,
    };

    if let Value::Object(object) = &mut merged {
        let base_object = base_compat
            .and_then(|compat| serde_json::to_value(compat).ok())
            .and_then(|value| match value {
                Value::Object(object) => Some(object),
                _ => None,
            })
            .unwrap_or_default();
        let override_object = serde_json::to_value(override_compat)
            .ok()
            .and_then(|value| match value {
                Value::Object(object) => Some(object),
                _ => None,
            })
            .unwrap_or_default();
        for key in ["openRouterRouting", "vercelGatewayRouting"] {
            let base_routing = base_object.get(key).cloned();
            let override_routing = override_object.get(key).cloned();
            if base_routing.is_some() || override_routing.is_some() {
                let mut routing = match base_routing {
                    Some(Value::Object(object)) => object,
                    _ => Map::new(),
                };
                if let Some(Value::Object(over)) = override_routing {
                    for (inner_key, inner_value) in over {
                        routing.insert(inner_key, inner_value);
                    }
                }
                object.insert(key.to_string(), Value::Object(routing));
            }
        }
    }

    serde_json::from_value(merged).ok()
}

/// Deep merge a model override into a model.
/// Handles nested objects (cost, compat) by merging rather than replacing.
pub fn apply_model_override(model: &Model, override_value: &ModelOverride) -> Model {
    let mut result = model.clone();

    if let Some(name) = &override_value.name {
        result.name = name.clone();
    }
    if let Some(reasoning) = override_value.reasoning {
        result.reasoning = reasoning;
    }
    if let Some(thinking_level_map) = &override_value.thinking_level_map {
        let mut merged = model.thinking_level_map.clone().unwrap_or_default();
        for (key, value) in thinking_level_map {
            merged.insert(key.clone(), value.clone());
        }
        result.thinking_level_map = Some(merged);
    }
    if let Some(input) = &override_value.input {
        result.input = input
            .iter()
            .filter_map(|value| match value.as_str() {
                "text" => Some(pi_ai::types::InputModality::Text),
                "image" => Some(pi_ai::types::InputModality::Image),
                _ => None,
            })
            .collect();
    }
    if let Some(context_window) = override_value.context_window {
        result.context_window = context_window;
    }
    if let Some(max_input_tokens) = override_value.max_input_tokens {
        result.max_input_tokens = Some(max_input_tokens);
    }
    if let Some(max_tokens) = override_value.max_tokens {
        result.max_tokens = max_tokens;
    }

    if let Some(cost) = &override_value.cost {
        result.cost = ModelCost {
            input: cost.input.unwrap_or(model.cost.input),
            output: cost.output.unwrap_or(model.cost.output),
            cache_read: cost.cache_read.unwrap_or(model.cost.cache_read),
            cache_write: cost.cache_write.unwrap_or(model.cost.cache_write),
        };
    }

    result.compat = merge_compat(model.compat.as_ref(), override_value.compat.as_ref());

    result
}

fn read_openai_codex_account_id(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let parsed: Value = serde_json::from_slice(&decoded).ok()?;
    let account_id = parsed
        .as_object()?
        .get("https://api.openai.com/auth")?
        .as_object()?
        .get("chatgpt_account_id")?;
    let account_id = account_id.as_str()?;
    if account_id.is_empty() {
        None
    } else {
        Some(account_id.to_string())
    }
}

/// The Codex backend gates its model catalog on the reported client version: it
/// answers HTTP 200 with a catalog that grows as the version rises, so a low
/// version yields a silently empty or partial list rather than an error.
///
/// Shipping a new Codex model takes two edits, and both are required:
/// 1. Add the model to `codexModels` in `packages/ai/scripts/generate-models.ts`
///    and regenerate.
/// 2. Raise this constant to a Codex CLI release whose catalog includes that
///    model.
///
/// Catalog behaviour measured 2026-08-13; see #702.
const OPENAI_CODEX_CLIENT_VERSION: &str = "0.153.4";

fn openai_codex_models_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    let path = if let Some(prefix) = normalized.strip_suffix("/codex/responses") {
        format!("{}/models", prefix)
    } else if normalized.ends_with("/codex") {
        format!("{}/models", normalized)
    } else {
        format!("{}/codex/models", normalized)
    };
    match url::Url::parse(&path) {
        Ok(mut url) => {
            url.query_pairs_mut()
                .append_pair("client_version", OPENAI_CODEX_CLIENT_VERSION);
            url.to_string()
        }
        Err(_) => path,
    }
}

fn read_openai_codex_model_ids(value: &Value) -> Result<HashSet<String>, String> {
    let object = value.as_object().ok_or_else(|| "Invalid OpenAI Codex model catalog".to_string())?;
    let models = object
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| "Invalid OpenAI Codex model catalog".to_string())?;
    let mut ids = HashSet::new();
    for model in models {
        let Some(object) = model.as_object() else {
            continue;
        };
        if let Some(slug) = object.get("slug").and_then(Value::as_str) {
            ids.insert(slug.to_string());
        }
    }
    Ok(ids)
}

const PRIVATE_PRIME_AUTHORIZATION_CACHE_FILE: &str = "prime-inference-private-models.json";
const PRIVATE_PRIME_AUTHORIZATION_CACHE_TTL_MS: i64 = 5 * 60_000;
const PRIVATE_PRIME_BACKGROUND_REFRESH_TIMEOUT_MS: u64 = 3_000;

#[derive(Debug, Clone, Default)]
struct PrivatePrimeAuthorizationCache {
    fingerprint: String,
    models: Vec<Model>,
    refreshed_at: i64,
}

fn private_prime_authorization_fingerprint(api_key: &str, team_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    hasher.update(b"\0");
    hasher.update(team_id.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn is_offline_mode_enabled() -> bool {
    match std::env::var("PI_OFFLINE") {
        Err(_) => false,
        Ok(value) => {
            if value.is_empty() {
                return false;
            }
            value == "1" || value.to_lowercase() == "true" || value.to_lowercase() == "yes"
        }
    }
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

/// `getAgentDir()` from config.ts (other slice).
fn get_agent_dir() -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".prime")
        .join("agent")
        .to_string_lossy()
        .to_string()
}

/// `interface ProviderConfigInput` for `registerProvider`.
#[derive(Clone, Default)]
pub struct ProviderConfigInput {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api: Option<Api>,
    /// `streamSimple?: (model, context, options?) => AssistantMessageEventStream`
    pub stream_simple: Option<
        Arc<dyn Fn(&Model, &Context, Option<&SimpleStreamOptions>) -> EventStream + Send + Sync>,
    >,
    pub headers: Option<IndexMap<String, String>>,
    pub auth_header: Option<bool>,
    /// OAuth provider for /login support (`Omit<OAuthProviderInterface, "id">`).
    pub oauth: Option<ProviderOAuthInput>,
    pub models: Option<Vec<ModelDefinition>>,
}

impl std::fmt::Debug for ProviderConfigInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderConfigInput")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api", &self.api)
            .field("auth_header", &self.auth_header)
            .finish_non_exhaustive()
    }
}

/// `Omit<OAuthProviderInterface, "id">`.
#[derive(Clone)]
pub struct ProviderOAuthInput {
    pub name: String,
    pub login: Arc<
        dyn Fn(pi_ai::utils::oauth::types::OAuthLoginCallbacks) -> pi_ai::types::BoxFuture<pi_ai::utils::oauth::types::OAuthCredentials>
            + Send
            + Sync,
    >,
    pub uses_callback_server: Option<bool>,
    pub refresh_token: Arc<
        dyn Fn(pi_ai::utils::oauth::types::OAuthCredentials) -> pi_ai::types::BoxFuture<pi_ai::utils::oauth::types::OAuthCredentials>
            + Send
            + Sync,
    >,
    pub get_api_key: Arc<dyn Fn(&pi_ai::utils::oauth::types::OAuthCredentials) -> String + Send + Sync>,
    pub modify_models: Option<Arc<dyn Fn(Vec<Model>, &pi_ai::utils::oauth::types::OAuthCredentials) -> Vec<Model> + Send + Sync>>,
}

impl std::fmt::Debug for ProviderOAuthInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderOAuthInput")
            .field("name", &self.name)
            .field("uses_callback_server", &self.uses_callback_server)
            .finish_non_exhaustive()
    }
}

/// Model registry - loads and manages models, resolves API keys via AuthStorage.
pub struct ModelRegistry {
    auth_storage: AuthStorage,
    models_json_path: Option<String>,
    models: Vec<Model>,
    provider_request_configs: IndexMap<String, ProviderRequestConfig>,
    stale_provider_request_auth_sources: HashMap<String, Vec<AuthSourceToken>>,
    last_provider_auth_source_tokens: HashMap<String, AuthSourceToken>,
    model_request_headers: IndexMap<String, IndexMap<String, String>>,
    registered_providers: IndexMap<String, ProviderConfigInput>,
    authorized_private_prime_inference_model_ids: HashSet<String>,
    authorized_private_prime_inference_models: Vec<Model>,
    authorized_private_prime_inference_team_id: Option<String>,
    explicit_private_prime_inference_model_ids: HashSet<String>,
    openai_codex_models_cache: Option<OpenAiCodexModelsCache>,
    background_private_prime_authorization: Option<BackgroundAuthorization>,
    live_prime_inference_models: Option<Vec<Model>>,
    load_error: Option<String>,
    /// Re-register dynamic OAuth providers (e.g. user MCP servers) after refresh()
    /// resets the registry.
    on_oauth_providers_reset: Option<Arc<dyn Fn() + Send + Sync>>,
    entitlement_refresh_chain: Arc<tokio::sync::Mutex<()>>,
    fetch_fn: Option<FetchFn>,
}

#[derive(Debug, Clone, Default)]
struct OpenAiCodexModelsCache {
    auth_fingerprint: String,
    model_ids: HashSet<String>,
    refreshed_at: i64,
}

#[derive(Debug, Clone, Default)]
struct BackgroundAuthorization {
    fingerprint: String,
    in_flight: bool,
}

impl ModelRegistry {
    fn new(auth_storage: AuthStorage, models_json_path: Option<String>) -> Self {
        let mut registry = Self {
            auth_storage,
            models_json_path,
            models: Vec::new(),
            provider_request_configs: IndexMap::new(),
            stale_provider_request_auth_sources: HashMap::new(),
            last_provider_auth_source_tokens: HashMap::new(),
            model_request_headers: IndexMap::new(),
            registered_providers: IndexMap::new(),
            authorized_private_prime_inference_model_ids: HashSet::new(),
            authorized_private_prime_inference_models: Vec::new(),
            authorized_private_prime_inference_team_id: None,
            explicit_private_prime_inference_model_ids: HashSet::new(),
            openai_codex_models_cache: None,
            background_private_prime_authorization: None,
            live_prime_inference_models: None,
            load_error: None,
            on_oauth_providers_reset: None,
            entitlement_refresh_chain: Arc::new(tokio::sync::Mutex::new(())),
            fetch_fn: None,
        };
        registry.load_models();
        registry
    }

    pub fn create(auth_storage: AuthStorage, models_json_path: Option<String>) -> Self {
        let path = models_json_path.unwrap_or_else(|| {
            Path::new(&get_agent_dir())
                .join("models.json")
                .to_string_lossy()
                .to_string()
        });
        Self::new(auth_storage, Some(path))
    }

    pub fn in_memory(auth_storage: AuthStorage) -> Self {
        Self::new(auth_storage, None)
    }

    pub fn set_fetch_fn(&mut self, fetch_fn: Option<FetchFn>) {
        self.fetch_fn = fetch_fn;
    }

    pub fn set_on_oauth_providers_reset(&mut self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.on_oauth_providers_reset = Some(hook);
    }

    /// Reload models from disk (built-in + custom from models.json).
    pub fn refresh(&mut self) {
        self.provider_request_configs.clear();
        self.model_request_headers.clear();
        self.last_provider_auth_source_tokens.clear();
        self.explicit_private_prime_inference_model_ids.clear();
        self.load_error = None;

        // Credentials may have been written by another process (e.g. the UI
        // process saving a login while the session lives in the daemon).
        self.auth_storage.reload();
        let team_id = self
            .auth_storage
            .get_provider_headers(PRIME_INFERENCE_PROVIDER_ID)
            .and_then(|headers| headers.get("X-Prime-Team-ID").cloned());
        // Direct refreshes must preserve same-team stale recovery, but invalidate
        // changed auth immediately.
        let auth_status = self.auth_storage.get_auth_status(PRIME_INFERENCE_PROVIDER_ID);
        if auth_status.source.as_deref() != Some("stale")
            || team_id.is_none()
            || team_id != self.authorized_private_prime_inference_team_id
        {
            self.authorized_private_prime_inference_model_ids.clear();
            self.authorized_private_prime_inference_models = Vec::new();
            self.authorized_private_prime_inference_team_id = None;
        }
        reset_api_providers();
        crate::core::auth_storage::reset_oauth_providers();
        // reset drops everything but model-provider built-ins; re-add MCP integrations
        // (built-in catalog + this session's user-declared servers via the hook).
        crate::core::auth_storage::register_builtin_mcp_oauth_providers();
        if let Some(hook) = &self.on_oauth_providers_reset {
            hook();
        }

        self.reload_models_after_catalog_change();
    }

    fn reload_models_after_catalog_change(&mut self) {
        self.load_models();
        self.reapply_registered_providers();
    }

    fn reapply_registered_providers(&mut self) {
        let providers: Vec<(String, ProviderConfigInput)> = self
            .registered_providers
            .iter()
            .map(|(name, config)| (name.clone(), config.clone()))
            .collect();
        for (provider_name, config) in providers {
            self.apply_provider_config(&provider_name, &config);
        }
    }

    fn prime_inference_catalog_cache_path(&self) -> Option<String> {
        self.models_json_path.as_ref().map(|path| {
            Path::new(path)
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("prime-inference-models-cache.json")
                .to_string_lossy()
                .to_string()
        })
    }

    fn bundled_prime_inference_models(&self) -> Vec<Model> {
        get_models(PRIME_INFERENCE_PROVIDER_ID)
    }

    /// Get any error from loading models.json (`None` if no error).
    pub fn get_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }
}
