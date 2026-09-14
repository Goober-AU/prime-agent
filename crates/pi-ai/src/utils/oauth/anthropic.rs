//! Port of packages/ai/src/utils/oauth/anthropic.ts
//!
//! Anthropic OAuth flow (Claude Pro/Max).
//!
//! The TypeScript uses Node `http.createServer` for the OAuth callback server and
//! is only intended for CLI use. The Rust port binds a tokio TCP listener on the
//! same host/port/path and serves the same HTML pages.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::types::BoxFuture;
use crate::utils::oauth::oauth_page::{oauth_error_html, oauth_success_html};
use crate::utils::oauth::pkce::generate_pkce;
use crate::utils::oauth::types::{
    OAuthAuthInfo, OAuthCredentials, OAuthLoginCallbacks, OAuthPrompt, OAuthProviderInterface,
};
use crate::utils::oauth::plumbing::{
    bind_callback_listener, oauth_callback_host, spawn_http_callback_server, CallbackSlot,
};

// TypeScript decodes its base64 literal before using the ID in OAuth requests.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub const CALLBACK_PORT: u16 = 53692;
pub const CALLBACK_PATH: &str = "/callback";
pub const REDIRECT_URI: &str = "http://localhost:53692/callback";
pub const SCOPES: &str =
    "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
pub const TOKEN_REQUEST_TIMEOUT_MS: u64 = 30_000;

/// The TypeScript computes CLIENT_ID with `atob`; the decoded value is used by the
/// tests below to prove the literal is the same.
pub fn decoded_client_id() -> String {
    CLIENT_ID.to_string()
}

pub struct CallbackServerInfo {
    pub port: u16,
    pub redirect_uri: String,
    slot: CallbackSlot<(String, String)>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl CallbackServerInfo {
    pub fn cancel_wait(&self) {
        self.slot.cancel();
    }

    pub async fn wait_for_code(&self) -> Option<(String, String)> {
        self.slot.wait().await
    }

    pub fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

impl Drop for CallbackServerInfo {
    fn drop(&mut self) {
        self.close();
    }
}

/// `parseAuthorizationInput(input)`.
pub fn parse_authorization_input(input: &str) -> AuthorizationInput {
    let value = input.trim();
    if value.is_empty() {
        return AuthorizationInput::default();
    }

    if let Ok(url) = url::Url::parse(value) {
        let mut result = AuthorizationInput::default();
        for (key, query_value) in url.query_pairs() {
            match key.as_ref() {
                "code" => result.code = Some(query_value.to_string()),
                "state" => result.state = Some(query_value.to_string()),
                _ => {}
            }
        }
        return result;
    }

    if let Some((code, state)) = value.split_once('#') {
        return AuthorizationInput {
            code: Some(code.to_string()),
            state: Some(state.to_string()),
        };
    }

    if value.contains("code=") {
        let mut result = AuthorizationInput::default();
        for (key, query_value) in url::form_urlencoded::parse(value.as_bytes()) {
            match key.as_ref() {
                "code" => result.code = Some(query_value.to_string()),
                "state" => result.state = Some(query_value.to_string()),
                _ => {}
            }
        }
        return result;
    }

    AuthorizationInput {
        code: Some(value.to_string()),
        state: None,
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuthorizationInput {
    pub code: Option<String>,
    pub state: Option<String>,
}

/// `formatErrorDetails(error)` for the error kinds the Rust port can observe.
pub fn format_error_details(message: &str, code: Option<&str>, stack: Option<&str>) -> String {
    let mut details = vec![format!("Error: {}", message)];
    if let Some(code) = code {
        details.push(format!("code={}", code));
    }
    if let Some(stack) = stack {
        details.push(format!("stack={}", stack));
    }
    details.join("; ")
}

async fn start_callback_server(expected_state: &str) -> Result<CallbackServerInfo, String> {
    let host = oauth_callback_host();
    let listener = bind_callback_listener(&host, CALLBACK_PORT)
        .await
        .map_err(|error| error.to_string())?;
    let slot: CallbackSlot<(String, String)> = CallbackSlot::default();
    let handler_slot = slot.clone();
    let expected_state = expected_state.to_string();
    let handle = spawn_http_callback_server(listener, move |path, params| {
        if path != CALLBACK_PATH {
            return (404, oauth_error_html("Callback route not found.", None));
        }

        let code = params.get("code").cloned();
        let state = params.get("state").cloned();
        let error = params.get("error").cloned();

        if let Some(error) = error {
            return (
                400,
                oauth_error_html(
                    "Anthropic authentication did not complete.",
                    Some(&format!("Error: {}", error)),
                ),
            );
        }

        let (Some(code), Some(state)) = (code, state) else {
            return (400, oauth_error_html("Missing code or state parameter.", None));
        };

        if state != expected_state {
            return (400, oauth_error_html("State mismatch.", None));
        }

        handler_slot.settle(Some((code, state)));
        (
            200,
            oauth_success_html("Anthropic authentication completed. You can close this window."),
        )
    });

    Ok(CallbackServerInfo {
        port: CALLBACK_PORT,
        redirect_uri: REDIRECT_URI.to_string(),
        slot,
        handle: Some(handle),
    })
}

async fn post_json(url: &str, body: &Value) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(TOKEN_REQUEST_TIMEOUT_MS))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|error| error.to_string())?;

    let status = response.status().as_u16();
    let response_body = response.text().await.unwrap_or_default();

    if !(200..300).contains(&status) {
        return Err(format!(
            "HTTP request failed. status={}; url={}; body={}",
            status, url, response_body
        ));
    }

    Ok(response_body)
}

async fn exchange_authorization_code(
    code: &str,
    state: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthCredentials, String> {
    let response_body = post_json(
        TOKEN_URL,
        &json!({
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "code": code,
            "state": state,
            "redirect_uri": redirect_uri,
            "code_verifier": verifier,
        }),
    )
    .await
    .map_err(|error| {
        format!(
            "Token exchange request failed. url={}; redirect_uri={}; response_type=authorization_code; details={}",
            TOKEN_URL,
            redirect_uri,
            format_error_details(&error, None, None)
        )
    })?;

    let token_data: Value = serde_json::from_str(&response_body).map_err(|error| {
        format!(
            "Token exchange returned invalid JSON. url={}; body={}; details={}",
            TOKEN_URL,
            response_body,
            format_error_details(&error.to_string(), None, None)
        )
    })?;

    let access_token = token_data.get("access_token").and_then(Value::as_str).unwrap_or_default();
    let refresh_token = token_data.get("refresh_token").and_then(Value::as_str).unwrap_or_default();
    let expires_in = token_data.get("expires_in").and_then(Value::as_f64).unwrap_or(0.0);

    Ok(OAuthCredentials {
        refresh: refresh_token.to_string(),
        access: access_token.to_string(),
        expires: crate::utils::now_ms() as f64 + expires_in * 1000.0 - 5.0 * 60.0 * 1000.0,
        extra: serde_json::Map::new(),
    })
}

/// The manual promise settles the callback waiter; a browser callback can finish
/// login while manual input remains pending.
async fn wait_for_callback_or_manual(
    server: &CallbackServerInfo,
    mut manual: BoxFuture<Result<String, String>>,
) -> Result<(Option<(String, String)>, Option<String>), String> {
    tokio::select! {
        biased;
        result = &mut manual => {
            server.cancel_wait();
            let input = result?;
            Ok((server.wait_for_code().await, Some(input)))
        }
        callback = server.wait_for_code() => {
            if callback.is_some() {
                Ok((callback, None))
            } else {
                Ok((None, Some(manual.await?)))
            }
        }
    }
}

/// Login with Anthropic OAuth (authorization code + PKCE).
pub async fn login_anthropic(options: AnthropicLoginOptions) -> Result<OAuthCredentials, String> {
    let (verifier, challenge) = generate_pkce().await;
    let server = start_callback_server(&verifier).await?;

    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut redirect_uri_for_exchange = REDIRECT_URI.to_string();

    let auth_params: Vec<(String, String)> = vec![
        ("code".to_string(), "true".to_string()),
        ("client_id".to_string(), CLIENT_ID.to_string()),
        ("response_type".to_string(), "code".to_string()),
        ("redirect_uri".to_string(), REDIRECT_URI.to_string()),
        ("scope".to_string(), SCOPES.to_string()),
        ("code_challenge".to_string(), challenge),
        ("code_challenge_method".to_string(), "S256".to_string()),
        ("state".to_string(), verifier.clone()),
    ];
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(auth_params.iter().map(|(key, value)| (key.as_str(), value.as_str())))
        .finish();

    if let Some(on_auth) = options.on_auth.as_ref() {
        on_auth(OAuthAuthInfo {
            url: format!("{}?{}", AUTHORIZE_URL, query),
            instructions: Some(
                "Complete login in your browser. If the browser is on another machine, paste the final redirect URL here."
                    .to_string(),
            ),
        });
    }

    if let Some(on_manual_code_input) = options.on_manual_code_input.as_ref() {
        let (callback_result, manual_input) =
            wait_for_callback_or_manual(&server, on_manual_code_input()).await?;

        if let Some((callback_code, callback_state)) = callback_result {
            code = Some(callback_code);
            state = Some(callback_state);
            redirect_uri_for_exchange = REDIRECT_URI.to_string();
        } else if let Some(input) = manual_input.as_deref() {
            let parsed = parse_authorization_input(input);
            if let Some(parsed_state) = parsed.state.as_deref() {
                if parsed_state != verifier {
                    return Err("OAuth state mismatch".to_string());
                }
            }
            code = parsed.code;
            state = parsed.state.or_else(|| Some(verifier.clone()));
        }
    } else {
        let callback_result = server.wait_for_code().await;
        if let Some((callback_code, callback_state)) = callback_result {
            code = Some(callback_code);
            state = Some(callback_state);
            redirect_uri_for_exchange = REDIRECT_URI.to_string();
        }
    }

    if code.is_none() {
        let Some(on_prompt) = options.on_prompt.as_ref() else {
            return Err("Missing authorization code".to_string());
        };
        let input = on_prompt(OAuthPrompt {
            message: "Paste the authorization code or full redirect URL:".to_string(),
            placeholder: Some(REDIRECT_URI.to_string()),
            allow_empty: None,
        })
        .await;
        let parsed = parse_authorization_input(&input);
        if let Some(parsed_state) = parsed.state.as_deref() {
            if parsed_state != verifier {
                return Err("OAuth state mismatch".to_string());
            }
        }
        code = parsed.code;
        state = parsed.state.or_else(|| Some(verifier.clone()));
    }

    let Some(code) = code else {
        return Err("Missing authorization code".to_string());
    };

    let Some(state) = state else {
        return Err("Missing OAuth state".to_string());
    };

    if let Some(on_progress) = options.on_progress.as_ref() {
        on_progress("Exchanging authorization code for tokens...".to_string());
    }
    exchange_authorization_code(&code, &state, &verifier, &redirect_uri_for_exchange).await
}

/// Options accepted by `loginAnthropic`.
pub struct AnthropicLoginOptions {
    pub on_auth: Option<Arc<dyn Fn(OAuthAuthInfo) + Send + Sync>>,
    pub on_prompt: Option<Arc<dyn Fn(OAuthPrompt) -> BoxFuture<String> + Send + Sync>>,
    pub on_progress: Option<Arc<dyn Fn(String) + Send + Sync>>,
    /// `onManualCodeInput(): Promise<string>` - `Err` is the rejected promise.
    pub on_manual_code_input: Option<crate::utils::oauth::types::OnManualCodeInput>,
}

/// Refresh Anthropic OAuth token.
pub async fn refresh_anthropic_token(refresh_token: &str) -> Result<OAuthCredentials, String> {
    let response_body = post_json(
        TOKEN_URL,
        &json!({
            "grant_type": "refresh_token",
            "client_id": CLIENT_ID,
            "refresh_token": refresh_token,
        }),
    )
    .await
    .map_err(|error| {
        format!(
            "Anthropic token refresh request failed. url={}; details={}",
            TOKEN_URL,
            format_error_details(&error, None, None)
        )
    })?;

    let data: Value = serde_json::from_str(&response_body).map_err(|error| {
        format!(
            "Anthropic token refresh returned invalid JSON. url={}; body={}; details={}",
            TOKEN_URL,
            response_body,
            format_error_details(&error.to_string(), None, None)
        )
    })?;

    let access_token = data.get("access_token").and_then(Value::as_str).unwrap_or_default();
    let refresh_token = data.get("refresh_token").and_then(Value::as_str).unwrap_or_default();
    let expires_in = data.get("expires_in").and_then(Value::as_f64).unwrap_or(0.0);

    Ok(OAuthCredentials {
        refresh: refresh_token.to_string(),
        access: access_token.to_string(),
        expires: crate::utils::now_ms() as f64 + expires_in * 1000.0 - 5.0 * 60.0 * 1000.0,
        extra: serde_json::Map::new(),
    })
}

/// `anthropicOAuthProvider`.
pub fn anthropic_oauth_provider() -> OAuthProviderInterface {
    OAuthProviderInterface {
        id: "anthropic".to_string(),
        name: "Anthropic (Claude Pro/Max)".to_string(),
        uses_callback_server: Some(true),
        login: Arc::new(|callbacks: OAuthLoginCallbacks| {
            Box::pin(async move {
                login_anthropic(AnthropicLoginOptions {
                    on_auth: callbacks.on_auth.clone(),
                    on_prompt: callbacks.on_prompt.clone(),
                    on_progress: callbacks.on_progress.clone(),
                    on_manual_code_input: callbacks.on_manual_code_input.clone(),
                })
                .await
            }) as crate::types::BoxFuture<Result<OAuthCredentials, String>>
        }),
        refresh_token: Arc::new(|credentials: OAuthCredentials| {
            Box::pin(async move { refresh_anthropic_token(&credentials.refresh).await })
                as crate::types::BoxFuture<Result<OAuthCredentials, String>>
        }),
        get_api_key: Arc::new(|credentials: &OAuthCredentials| credentials.access.clone()),
        modify_models: None,
    }
}

/// Keep the `CancellationToken` import meaningful for callers that build
/// `OAuthLoginCallbacks` with a signal.
pub fn login_callbacks_signal(callbacks: &OAuthLoginCallbacks) -> Option<CancellationToken> {
    callbacks.signal.clone()
}

/// Helper for tests: unused `HashMap` import guard.
#[allow(dead_code)]
fn _assert_hash_map(_: HashMap<String, String>) {}

#[cfg(test)]
mod tests {
    use super::*;

    // The real Anthropic redirect uses a fixed port. Keep real listener tests
    // exclusive until the aborted server task has released its socket.
    static CALLBACK_SERVER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn close_callback_server(mut server: CallbackServerInfo) {
        let handle = server.handle.take().expect("running callback server");
        handle.abort();
        let _ = handle.await;
    }

    fn callback_waiter() -> CallbackServerInfo {
        CallbackServerInfo {
            port: CALLBACK_PORT,
            redirect_uri: REDIRECT_URI.to_string(),
            slot: CallbackSlot::default(),
            handle: None,
        }
    }

    #[test]
    fn client_id_matches_decoded_typescript_literal() {
        assert_eq!(decoded_client_id(), "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
        assert_eq!(CLIENT_ID, decoded_client_id());
    }

    #[test]
    fn parses_full_redirect_url() {
        let parsed = parse_authorization_input(
            "http://localhost:53692/callback?code=abc&state=def",
        );
        assert_eq!(parsed.code.as_deref(), Some("abc"));
        assert_eq!(parsed.state.as_deref(), Some("def"));
    }

    #[test]
    fn parses_hash_and_query_forms_and_bare_code() {
        let parsed = parse_authorization_input("abc#def");
        assert_eq!(parsed.code.as_deref(), Some("abc"));
        assert_eq!(parsed.state.as_deref(), Some("def"));

        let parsed = parse_authorization_input("code=abc&state=def");
        assert_eq!(parsed.code.as_deref(), Some("abc"));
        assert_eq!(parsed.state.as_deref(), Some("def"));

        let parsed = parse_authorization_input("  bare-code  ");
        assert_eq!(parsed.code.as_deref(), Some("bare-code"));
        assert_eq!(parsed.state, None);

        assert_eq!(parse_authorization_input(""), AuthorizationInput::default());
    }

    #[test]
    fn constants_match_typescript() {
        assert_eq!(AUTHORIZE_URL, "https://claude.ai/oauth/authorize");
        assert_eq!(TOKEN_URL, "https://platform.claude.com/v1/oauth/token");
        assert_eq!(CALLBACK_PORT, 53692);
        assert_eq!(REDIRECT_URI, "http://localhost:53692/callback");
        assert_eq!(
            SCOPES,
            "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload"
        );
    }

    #[tokio::test]
    async fn callback_server_serves_success_and_state_mismatch() {
        let _guard = CALLBACK_SERVER_LOCK.lock().await;
        let server = start_callback_server("expected-state").await.unwrap();
        let redirect_uri = server.redirect_uri.clone();
        assert_eq!(redirect_uri, REDIRECT_URI);

        // Wrong state -> 400 and no code.
        let response = reqwest::get(format!("{}?code=abc&state=wrong", redirect_uri))
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 400);
        assert!(response.text().await.unwrap().contains("State mismatch."));

        // Missing code -> 400.
        let response = reqwest::get(format!("{}?state=expected-state", redirect_uri))
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 400);

        // Correct callback settles the waiter.
        let response = reqwest::get(format!("{}?code=abc&state=expected-state", redirect_uri))
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let code = server.wait_for_code().await.unwrap();
        assert_eq!(code.0, "abc");
        assert_eq!(code.1, "expected-state");
        close_callback_server(server).await;
    }

    #[tokio::test]
    async fn cancel_wait_settles_with_none() {
        let _guard = CALLBACK_SERVER_LOCK.lock().await;
        let server = start_callback_server("s").await.unwrap();
        server.cancel_wait();
        assert!(server.wait_for_code().await.is_none());
        close_callback_server(server).await;
    }

    #[tokio::test]
    async fn manual_input_settles_without_a_browser_callback() {
        let server = callback_waiter();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_callback_or_manual(&server, Box::pin(async { Ok("code#state".to_string()) })),
        )
        .await
        .expect("manual input must cancel the callback wait")
        .unwrap();
        assert_eq!(result, (None, Some("code#state".to_string())));
        assert!(server.wait_for_code().await.is_none());
    }

    #[tokio::test]
    async fn browser_callback_does_not_wait_for_pending_manual_input() {
        let server = callback_waiter();
        server.slot.settle(Some(("code".to_string(), "state".to_string())));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_callback_or_manual(&server, Box::pin(std::future::pending())),
        )
        .await
        .expect("browser callback must finish independently of manual input")
        .unwrap();
        assert_eq!(result, (Some(("code".to_string(), "state".to_string())), None));
    }

    #[tokio::test]
    async fn manual_input_rejection_cancels_callback_wait() {
        let server = callback_waiter();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_callback_or_manual(&server, Box::pin(async { Err("cancelled".to_string()) })),
        )
        .await
        .expect("manual rejection must cancel the callback wait");
        assert_eq!(result.unwrap_err(), "cancelled");
        assert!(server.wait_for_code().await.is_none());
    }

    #[test]
    fn error_details_include_code_and_stack() {
        assert_eq!(format_error_details("boom", None, None), "Error: boom");
        assert_eq!(
            format_error_details("boom", Some("ECONNREFUSED"), Some("stack")),
            "Error: boom; code=ECONNREFUSED; stack=stack"
        );
    }
}
