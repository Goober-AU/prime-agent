//! Port of packages/ai/src/index.ts
//!
//! `export * from ...` re-exports. The TypeScript also re-exports TypeBox
//! (`Static`, `TSchema`, `Type`), which has no Rust counterpart; tool parameter
//! schemas are `serde_json::Value` (see types.rs).

pub use crate::api_registry::*;
pub use crate::compaction::*;
pub use crate::env_api_keys::*;
pub use crate::log::*;
pub use crate::models::*;
pub use crate::prime_inference_model_catalog::*;

pub use crate::providers::faux::*;
pub use crate::providers::register_builtins::*;

pub use crate::session_resources::*;
pub use crate::stream::*;
pub use crate::types::*;

pub use crate::utils::diagnostics::*;
pub use crate::utils::event_stream::*;
pub use crate::utils::json_parse::*;
pub use crate::utils::overflow::*;
pub use crate::utils::stream_failure::*;
pub use crate::utils::typebox_helpers::*;
pub use crate::utils::validation::*;

// `export type { ... } from "./utils/oauth/types.js"` - type-only re-exports.
pub use crate::utils::oauth::types::{
    OAuthAuthInfo, OAuthCredentials, OAuthLoginCallbacks, OAuthPrompt, OAuthProvider, OAuthProviderId,
    OAuthProviderInfo, OAuthProviderInterface, OAuthSelectOption, OAuthSelectPrompt,
};

// `export type { BedrockOptions, ... }` and the other provider option types are
// re-exported by the providers module itself.
pub use crate::providers::amazon_bedrock::{BedrockOptions, BedrockThinkingDisplay};
pub use crate::providers::amazon_bedrock_responses::BedrockResponsesOptions;
pub use crate::providers::anthropic::{AnthropicEffort, AnthropicOptions, AnthropicThinkingDisplay};
pub use crate::providers::azure_openai_responses::AzureOpenAIResponsesOptions;
pub use crate::providers::google::GoogleOptions;
pub use crate::providers::google_shared::GoogleThinkingLevel;
pub use crate::providers::google_vertex::GoogleVertexOptions;
pub use crate::providers::mistral::MistralOptions;
pub use crate::providers::openai_codex_responses::{
    OpenAICodexResponsesOptions, OpenAICodexWebSocketDebugStats,
};
pub use crate::providers::openai_completions::OpenAICompletionsOptions;
pub use crate::providers::openai_responses::OpenAIResponsesOptions;

/// `export type { AssistantMessageEventStream } from "./utils/event-stream.js";`
pub use crate::utils::event_stream::AssistantMessageEventStream;
