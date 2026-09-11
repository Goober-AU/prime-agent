//! Port of packages/coding-agent/src/bun/register-bedrock.ts

use std::sync::Arc;

use pi_ai::providers::amazon_bedrock::{
    stream_bedrock, stream_simple_bedrock, BedrockOptions,
};
use pi_ai::providers::amazon_bedrock_responses::{
    stream_bedrock_responses, stream_simple_bedrock_responses,
};
use pi_ai::providers::register_builtins::{
    set_bedrock_provider_module, BedrockProviderModule, BedrockResponsesProviderModule,
};
use pi_ai::types::{Context, Model, StreamOptions};

/// Port of `bedrockProviderModule` from `@earendil-works/pi-ai/bedrock-provider`.
pub fn bedrock_provider_module() -> BedrockProviderModule {
    BedrockProviderModule {
        responses: Some(BedrockResponsesProviderModule {
            stream_bedrock_responses: Arc::new(
                |model: &Model, context: &Context, options: Option<&StreamOptions>| {
                    stream_bedrock_responses(model, context, options)
                },
            ),
            stream_simple_bedrock_responses: Arc::new(
                |model: &Model, context: &Context, options: Option<&StreamOptions>| {
                    stream_simple_bedrock_responses(model, context, options)
                },
            ),
        }),
        stream_bedrock: Arc::new(|model: &Model, context: &Context, options: Option<&StreamOptions>| {
            stream_bedrock(model, context, BedrockOptions::from_base(options.unwrap_or(&StreamOptions::default())))
        }),
        stream_simple_bedrock: Arc::new(
            |model: &Model, context: &Context, options: Option<&StreamOptions>| {
                stream_simple_bedrock(model, context, options)
            },
        ),
    }
}

/// `setBedrockProviderModule(bedrockProviderModule)`.
pub fn register_bedrock() {
    set_bedrock_provider_module(bedrock_provider_module());
}
