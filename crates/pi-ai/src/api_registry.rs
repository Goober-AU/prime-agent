//! Port of packages/ai/src/api-registry.ts

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::compaction::CompactionOptions;
use crate::types::{
    Api, BoxFuture, CompactFunction, Context, Model, SimpleStreamOptions, StreamFunction,
    StreamOptions,
};
use crate::utils::event_stream::AssistantMessageEventStream;

pub type ApiStreamFunction =
    Arc<dyn Fn(&Model, &Context, Option<&StreamOptions>) -> AssistantMessageEventStream + Send + Sync>;

pub type ApiStreamSimpleFunction =
    Arc<dyn Fn(&Model, &Context, Option<&SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync>;

/// `interface ApiProvider<TApi, TOptions>`
pub struct ApiProvider {
    pub api: Api,
    pub stream: StreamFunction,
    pub stream_simple: StreamFunction,
    pub compact: Option<CompactFunction>,
    pub supports_compaction: Option<Arc<dyn Fn(&Model) -> bool + Send + Sync>>,
}

impl Clone for ApiProvider {
    fn clone(&self) -> Self {
        Self {
            api: self.api.clone(),
            stream: self.stream.clone(),
            stream_simple: self.stream_simple.clone(),
            compact: self.compact.clone(),
            supports_compaction: self.supports_compaction.clone(),
        }
    }
}

/// `interface ApiProviderInternal`
#[derive(Clone)]
pub struct ApiProviderInternal {
    pub api: Api,
    pub stream: ApiStreamFunction,
    pub stream_simple: ApiStreamSimpleFunction,
    pub compact: Option<CompactFunction>,
    pub supports_compaction: Option<Arc<dyn Fn(&Model) -> bool + Send + Sync>>,
}

#[derive(Clone)]
struct RegisteredApiProvider {
    provider: ApiProviderInternal,
    source_id: Option<String>,
}

fn api_provider_registry() -> &'static Mutex<HashMap<String, RegisteredApiProvider>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, RegisteredApiProvider>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_registry() -> std::sync::MutexGuard<'static, HashMap<String, RegisteredApiProvider>> {
    api_provider_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `function wrapStream(api, stream)`.
fn wrap_stream(api: Api, stream: StreamFunction) -> ApiStreamFunction {
    Arc::new(move |model: &Model, context: &Context, options: Option<&StreamOptions>| {
        if model.api != api {
            throw_mismatched_api(&model.api, &api, "Mismatched api");
        }
        stream(model, context, options)
    })
}

/// `function wrapStreamSimple(api, streamSimple)`.
fn wrap_stream_simple(api: Api, stream_simple: StreamFunction) -> ApiStreamSimpleFunction {
    Arc::new(
        move |model: &Model, context: &Context, options: Option<&SimpleStreamOptions>| {
            if model.api != api {
                throw_mismatched_api(&model.api, &api, "Mismatched api");
            }
            let options = options.map(|options| StreamOptions {
                temperature: options.stream.temperature,
                max_tokens: options.stream.max_tokens,
                signal: options.stream.signal.clone(),
                api_key: options.stream.api_key.clone(),
                transport: options.stream.transport.clone(),
                service_tier: options.stream.service_tier.clone(),
                cache_retention: options.stream.cache_retention.clone(),
                session_id: options.stream.session_id.clone(),
                on_payload: options.stream.on_payload.clone(),
                on_response: options.stream.on_response.clone(),
                on_usage_observation: options.stream.on_usage_observation.clone(),
                headers: options.stream.headers.clone(),
                timeout_ms: options.stream.timeout_ms,
                metadata: options.stream.metadata.clone(),
                extra: options.stream.extra.clone(),
            });
            stream_simple(model, context, options.as_ref())
        },
    )
}

/// The TypeScript `throw new Error(...)` inside the wrapper.
fn throw_mismatched_api(model_api: &str, api: &str, prefix: &str) -> ! {
    panic!("{}: {} expected {}", prefix, model_api, api)
}

/// `registerApiProvider(provider, sourceId?)`
pub fn register_api_provider(provider: ApiProvider, source_id: Option<String>) {
    let api = provider.api.clone();
    let internal = ApiProviderInternal {
        api: provider.api.clone(),
        stream: wrap_stream(provider.api.clone(), provider.stream.clone()),
        stream_simple: wrap_stream_simple(provider.api.clone(), provider.stream_simple.clone()),
        supports_compaction: provider.supports_compaction.clone(),
        compact: provider.compact.clone().map(|compact| {
            let api = provider.api.clone();
            let compact: CompactFunction = compact;
            Arc::new(
                move |model: &Model,
                      context: &Context,
                      options: Option<&CompactionOptions>|
                      -> BoxFuture<Option<crate::compaction::ProviderCompactionResult>> {
                    if model.api != api {
                        panic!("Mismatched compaction api: {}", model.api);
                    }
                    compact(model, context, options)
                },
            ) as CompactFunction
        }),
    };

    lock_registry().insert(
        api,
        RegisteredApiProvider {
            provider: internal,
            source_id,
        },
    );
}

pub fn get_api_provider(api: &str) -> Option<ApiProviderInternal> {
    lock_registry().get(api).map(|entry| entry.provider.clone())
}

pub fn get_api_providers() -> Vec<ApiProviderInternal> {
    lock_registry()
        .values()
        .map(|entry| entry.provider.clone())
        .collect()
}

pub fn unregister_api_providers(source_id: &str) {
    lock_registry().retain(|_, entry| entry.source_id.as_deref() != Some(source_id));
}

pub fn clear_api_providers() {
    lock_registry().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AssistantMessage, STOP_REASON_STOP};

    fn fake_stream(model_api: &str) -> StreamFunction {
        let model_api = model_api.to_string();
        Arc::new(move |model: &Model, _context: &Context, _options: Option<&StreamOptions>| {
            assert_eq!(model.api, model_api);
            let stream = AssistantMessageEventStream::new();
            stream.push(crate::types::AssistantMessageEvent::Done {
                reason: STOP_REASON_STOP.to_string(),
                message: AssistantMessage::default(),
            });
            stream
        })
    }

    fn provider(api: &str) -> ApiProvider {
        ApiProvider {
            api: api.to_string(),
            stream: fake_stream(api),
            stream_simple: fake_stream(api),
            compact: None,
            supports_compaction: None,
        }
    }

    #[test]
    fn register_get_and_clear() {
        clear_api_providers();
        register_api_provider(provider("openai-responses"), Some("src-a".to_string()));
        assert!(get_api_provider("openai-responses").is_some());
        assert_eq!(get_api_providers().len(), 1);

        register_api_provider(provider("anthropic-messages"), Some("src-b".to_string()));
        assert_eq!(get_api_providers().len(), 2);

        unregister_api_providers("src-a");
        assert!(get_api_provider("openai-responses").is_none());
        assert!(get_api_provider("anthropic-messages").is_some());

        clear_api_providers();
        assert!(get_api_providers().is_empty());
    }

    #[test]
    fn registry_is_last_write_wins_per_api() {
        clear_api_providers();
        register_api_provider(provider("openai-completions"), Some("first".to_string()));
        register_api_provider(provider("openai-completions"), Some("second".to_string()));
        assert_eq!(get_api_providers().len(), 1);
        unregister_api_providers("first");
        assert_eq!(get_api_providers().len(), 1, "second registration must survive");
        clear_api_providers();
    }

    #[test]
    fn wrapped_stream_returns_events_for_matching_api() {
        clear_api_providers();
        register_api_provider(provider("google-generative-ai"), None);
        let internal = get_api_provider("google-generative-ai").unwrap();
        let model = Model::new("m", "M", "google-generative-ai", "google", "https://example.test");
        let context = Context::default();
        let stream = (internal.stream)(&model, &context, None);
        assert!(stream.is_done());
        clear_api_providers();
    }

    #[test]
    #[should_panic(expected = "Mismatched api: openai-responses expected anthropic-messages")]
    fn wrapped_stream_panics_on_mismatched_api() {
        clear_api_providers();
        register_api_provider(provider("anthropic-messages"), None);
        let internal = get_api_provider("anthropic-messages").unwrap();
        let model = Model::new("m", "M", "openai-responses", "openai", "https://example.test");
        let context = Context::default();
        let _ = (internal.stream)(&model, &context, None);
    }
}
