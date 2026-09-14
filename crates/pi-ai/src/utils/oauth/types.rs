//! Port of packages/ai/src/utils/oauth/types.ts

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::types::Model;

/// `OAuthCredentials = { refresh: string; access: string; expires: number; [key: string]: unknown }`
///
/// The extra keys are preserved in `extra` (serde flatten), so e.g.
/// `enterpriseUrl` and `accountId` survive a round trip.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub refresh: String,
    pub access: String,
    pub expires: f64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

pub type OAuthProviderId = String;

/// @deprecated Use OAuthProviderId instead
pub type OAuthProvider = OAuthProviderId;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuthPrompt {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(rename = "allowEmpty", skip_serializing_if = "Option::is_none")]
    pub allow_empty: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuthAuthInfo {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuthSelectOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuthSelectPrompt {
    pub message: String,
    pub options: Vec<OAuthSelectOption>,
}

pub type OnAuth = Arc<dyn Fn(OAuthAuthInfo) + Send + Sync>;
pub type OnPrompt = Arc<dyn Fn(OAuthPrompt) -> crate::types::BoxFuture<String> + Send + Sync>;
pub type OnProgress = Arc<dyn Fn(String) + Send + Sync>;
pub type OnManualCodeInput =
    Arc<dyn Fn() -> crate::types::BoxFuture<Result<String, String>> + Send + Sync>;
pub type OnSelect =
    Arc<dyn Fn(OAuthSelectPrompt) -> crate::types::BoxFuture<Option<String>> + Send + Sync>;

/// `interface OAuthLoginCallbacks`
#[derive(Clone, Default)]
pub struct OAuthLoginCallbacks {
    pub on_auth: Option<OnAuth>,
    pub on_prompt: Option<OnPrompt>,
    pub on_progress: Option<OnProgress>,
    pub on_manual_code_input: Option<OnManualCodeInput>,
    /// Show an interactive selector and return the selected option id, or
    /// undefined on cancel.
    pub on_select: Option<OnSelect>,
    /// `signal?: AbortSignal`
    pub signal: Option<CancellationToken>,
}

/// `interface OAuthProviderInterface`
#[derive(Clone)]
pub struct OAuthProviderInterface {
    pub id: OAuthProviderId,
    pub name: String,
    /// Run the login flow, return credentials to persist.
    /// `Result::Err` is the TypeScript promise rejection.
    pub login:
        Arc<dyn Fn(OAuthLoginCallbacks) -> crate::types::BoxFuture<Result<OAuthCredentials, String>> + Send + Sync>,
    /// Whether login uses a local callback server and supports manual code input.
    pub uses_callback_server: Option<bool>,
    /// Refresh expired credentials, return updated credentials to persist.
    pub refresh_token: Arc<
        dyn Fn(OAuthCredentials) -> crate::types::BoxFuture<Result<OAuthCredentials, String>> + Send + Sync,
    >,
    /// Convert credentials to API key string for the provider.
    pub get_api_key: Arc<dyn Fn(&OAuthCredentials) -> String + Send + Sync>,
    /// Optional: modify models for this provider (e.g., update baseUrl).
    pub modify_models: Option<Arc<dyn Fn(Vec<Model>, &OAuthCredentials) -> Vec<Model> + Send + Sync>>,
}

/// @deprecated Use OAuthProviderInterface instead
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthProviderInfo {
    pub id: OAuthProviderId,
    pub name: String,
    pub available: bool,
}

/// Helper: `Model<Api>` is not generic in Rust, so `ApiModel` is `Model`.
pub type ApiModel = Model;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credentials_keep_extra_keys() {
        let credentials: OAuthCredentials = serde_json::from_value(json!({
            "refresh": "r",
            "access": "a",
            "expires": 1234,
            "enterpriseUrl": "company.ghe.com",
            "accountId": "acc"
        }))
        .unwrap();
        assert_eq!(credentials.refresh, "r");
        assert_eq!(credentials.expires, 1234.0);
        assert_eq!(
            credentials.extra.get("enterpriseUrl").and_then(|value| value.as_str()),
            Some("company.ghe.com")
        );
        let value = serde_json::to_value(&credentials).unwrap();
        assert_eq!(value["accountId"], json!("acc"));
    }

    #[test]
    fn prompt_uses_camel_case_allow_empty() {
        let prompt = OAuthPrompt {
            message: "m".to_string(),
            placeholder: Some("p".to_string()),
            allow_empty: Some(true),
        };
        assert_eq!(
            serde_json::to_value(&prompt).unwrap(),
            json!({"message": "m", "placeholder": "p", "allowEmpty": true})
        );
    }

    #[test]
    fn provider_info_round_trips() {
        let info = OAuthProviderInfo {
            id: "anthropic".to_string(),
            name: "Anthropic".to_string(),
            available: true,
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            json!({"id": "anthropic", "name": "Anthropic", "available": true})
        );
    }
}
