//! Port of packages/ai/src/bedrock-provider.ts

use std::sync::Arc;

use crate::providers::amazon_bedrock::{stream_bedrock, stream_simple_bedrock};
use crate::providers::amazon_bedrock_responses::{
    stream_bedrock_responses, stream_simple_bedrock_responses,
};
use crate::types::{Context, Model, StreamFunction, StreamOptions};
use crate::utils::event_stream::AssistantMessageEventStream;

/// `bedrockProviderModule` - the module object handed to
/// `setBedrockProviderModule` in register-builtins.ts.
pub struct BedrockProviderModule {
    pub stream_bedrock: StreamFunction,
    pub stream_simple_bedrock: StreamFunction,
    pub stream_bedrock_responses: StreamFunction,
    pub stream_simple_bedrock_responses: StreamFunction,
}

impl BedrockProviderModule {
    /// `module.responses.streamBedrockResponses` / `.streamSimpleBedrockResponses`.
    pub fn responses(&self) -> (StreamFunction, StreamFunction) {
        (
            self.stream_bedrock_responses.clone(),
            self.stream_simple_bedrock_responses.clone(),
        )
    }
}

/// `export const bedrockProviderModule`.
pub fn bedrock_provider_module() -> BedrockProviderModule {
    BedrockProviderModule {
        stream_bedrock: Arc::new(
            |model: &Model, context: &Context, options: Option<&StreamOptions>| {
                stream_bedrock(model, context, options)
            },
        ),
        stream_simple_bedrock: Arc::new(
            |model: &Model, context: &Context, options: Option<&StreamOptions>| {
                stream_simple_bedrock(model, context, options)
            },
        ),
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
    }
}

/// Keeps the module type usable where a raw event stream is expected.
#[allow(dead_code)]
fn _assert_stream_type(_: AssistantMessageEventStream) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_exposes_both_bedrock_apis() {
        let module = bedrock_provider_module();
        let (responses_stream, responses_simple) = module.responses();
        assert!(Arc::strong_count(&module.stream_bedrock) >= 1);
        assert!(Arc::strong_count(&module.stream_simple_bedrock) >= 1);
        assert!(Arc::strong_count(&responses_stream) >= 1);
        assert!(Arc::strong_count(&responses_simple) >= 1);
    }
}
