//! Port of packages/ai/src/utils/oauth/openai-codex.ts
//!
//! OpenAI Codex (ChatGPT OAuth) flow.
//!
//! The TypeScript uses Node `crypto.randomBytes` and `http` for the OAuth
//! callback. The Rust port uses `rand` and a tokio TCP listener with the same
//! host/port/path and the same HTML pages.

use std::collections::HashMap;
use std::sync::Arc;

use rand::RngCore;
use serde_json::Value;

use crate::types::{BoxFuture, Model};
use crate::utils::oauth::oauth_page::{oauth_error_html, oauth_success_html};
use crate::utils::oauth::pkce::generate_pkce;
use crate::utils::oauth::types::{
    OAuthAuthInfo, OAuthCredentials, OAuthLoginCallbacks, OAuthPrompt, OAuthProviderInterface,
};
use crate::utils::oauth::plumbing::{
    bind_callback_listener, decode_base64, oauth_callback_host, spawn_http_callback_server, CallbackSlot,
};

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
pub const SCOPE: &str = "openid profile email offline_access";
pub const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
pub const CALLBACK_PORT: u16 = 1455;
pub const CALLBACK_PATH: &str = "/auth/callback";

#[derive(Debug, Clone, PartialEq)]
pub enum TokenResult {
    Success {
        access: String,
        refresh: String,
        expires: f64,
    },
    Failed {
        message: String,
        status: Option<u16>,
    },
}

/// `createState()` - 16 random bytes as hex.
pub fn create_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{:02x}", byte)).collect()
}

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

/// `decodeJwt(token)`.
pub fn decode_jwt(token: &str) -> Option<Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = parts[1];
    let decoded = decode_base64(payload);
    serde_json::from_str(&decoded).ok()
}

async fn exchange_authorization_code(
    code: &str,
    verifier: &str,
    redirect_uri: Option<&str>,
) -> TokenResult {
    let redirect_uri = redirect_uri.unwrap_or(REDIRECT_URI);
    let client = match reqwest::Client::builder().build() {
        Ok(client) => client,
        Err(error) => {
            return TokenResult::Failed {
                message: format!("OpenAI Codex token exchange error: {}", error),
                status: None,
            }
        }
    };
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code", code)
        .append_pair("code_verifier", verifier)
        .append_pair("redirect_uri", redirect_uri)
        .finish();
    let response = match client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return TokenResult::Failed {
                message: format!("OpenAI Codex token exchange error: {}", error),
                status: None,
            }
        }
    };

    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return TokenResult::Failed {
            status: Some(status.as_u16()),
            message: format!(
                "OpenAI Codex token exchange failed ({}): {}",
                status.as_u16(),
                if text.is_empty() {
                    status.canonical_reason().unwrap_or_default().to_string()
                } else {
                    text
                }
            ),
        };
    }

    let json: Value = match response.json().await {
        Ok(json) => json,
        Err(error) => {
            return TokenResult::Failed {
                message: format!("OpenAI Codex token exchange response missing fields: {}", error),
                status: None,
            }
        }
    };

    let (Some(access_token), Some(refresh_token), Some(expires_in)) = (
        json.get("access_token").and_then(Value::as_str),
        json.get("refresh_token").and_then(Value::as_str),
        json.get("expires_in").and_then(Value::as_f64),
    ) else {
        return TokenResult::Failed {
            message: format!(
                "OpenAI Codex token exchange response missing fields: {}",
                json
            ),
            status: None,
        };
    };

    TokenResult::Success {
        access: access_token.to_string(),
        refresh: refresh_token.to_string(),
        expires: crate::utils::now_ms() as f64 + expires_in * 1000.0,
    }
}

async fn refresh_access_token(refresh_token: &str) -> TokenResult {
    let client = match reqwest::Client::builder().build() {
        Ok(client) => client,
        Err(error) => {
            return TokenResult::Failed {
                message: format!("OpenAI Codex token refresh error: {}", error),
                status: None,
            }
        }
    };
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", refresh_token)
        .append_pair("client_id", CLIENT_ID)
        .finish();
    let response = match client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return TokenResult::Failed {
                message: format!("OpenAI Codex token refresh error: {}", error),
                status: None,
            }
        }
    };

    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return TokenResult::Failed {
            status: Some(status.as_u16()),
            message: format!(
                "OpenAI Codex token refresh failed ({}): {}",
                status.as_u16(),
                if text.is_empty() {
                    status.canonical_reason().unwrap_or_default().to_string()
                } else {
                    text
                }
            ),
        };
    }

    let json: Value = match response.json().await {
        Ok(json) => json,
        Err(error) => {
            return TokenResult::Failed {
                message: format!("OpenAI Codex token refresh response missing fields: {}", error),
                status: None,
            }
        }
    };

    let (Some(access_token), Some(refresh_token), Some(expires_in)) = (
        json.get("access_token").and_then(Value::as_str),
        json.get("refresh_token").and_then(Value::as_str),
        json.get("expires_in").and_then(Value::as_f64),
    ) else {
        return TokenResult::Failed {
            message: format!("OpenAI Codex token refresh response missing fields: {}", json),
            status: None,
        };
    };

    TokenResult::Success {
        access: access_token.to_string(),
        refresh: refresh_token.to_string(),
        expires: crate::utils::now_ms() as f64 + expires_in * 1000.0,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthorizationFlow {
    pub verifier: String,
    pub state: String,
    pub url: String,
}

pub async fn create_authorization_flow(originator: Option<&str>) -> AuthorizationFlow {
    let originator = originator.unwrap_or("pi");
    let (verifier, challenge) = generate_pkce().await;
    let state = create_state();

    let mut url = url::Url::parse(AUTHORIZE_URL).expect("authorize url");
    {
        let mut params = url.query_pairs_mut();
        params.append_pair("response_type", "code");
        params.append_pair("client_id", CLIENT_ID);
        params.append_pair("redirect_uri", REDIRECT_URI);
        params.append_pair("scope", SCOPE);
        params.append_pair("code_challenge", &challenge);
        params.append_pair("code_challenge_method", "S256");
        params.append_pair("state", &state);
        params.append_pair("id_token_add_organizations", "true");
        params.append_pair("codex_cli_simplified_flow", "true");
        params.append_pair("originator", originator);
    }

    AuthorizationFlow {
        verifier,
        state,
        url: url.to_string(),
    }
}

pub struct OAuthServerInfo {
    slot: CallbackSlot<String>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

/// Settles the callback slot with `None` when the callback-server task stops
/// without delivering a code, so `waitForCode()` can never be left pending.
///
/// TypeScript reference: a Node server that fails emits `error`, and that
/// handler calls `settleWait?.(null)` (packages/ai/src/utils/oauth/openai-codex.ts:274);
/// the fallback server it resolves exposes `waitForCode: async () => null`
/// (openai-codex.ts:284). So in the TypeScript a server that stops resolves the
/// wait with `null` instead of hanging it. This guard gives the Rust port the
/// same guarantee for every exit path of the spawned server task: a normal
/// return (`listener.accept()` failed), a panic, or `OAuthServerInfo::close()`.
///
/// `cancel()` never loses a delivered code: `CallbackSlot::settle` keeps the
/// first value, so this only converts a would-be hang into a resolution.
struct SettleSlotOnDrop(CallbackSlot<String>);

impl Drop for SettleSlotOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Aborts the wrapped task when dropped, so aborting the guard task
/// (`OAuthServerInfo::close()`) still stops the server task.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Wrap the spawned callback-server task in a guard that settles `slot` when the
/// server stops, and that stops the server when the guard is aborted or dropped.
///
/// Both guards are built before the task is spawned, so they are dropped even
/// when the task is aborted before its first poll.
fn guard_callback_server(
    slot: CallbackSlot<String>,
    server: tokio::task::JoinHandle<()>,
) -> tokio::task::JoinHandle<()> {
    let settle = SettleSlotOnDrop(slot);
    let cascade = AbortOnDrop(server.abort_handle());
    tokio::spawn(async move {
        let _settle = settle;
        let _cascade = cascade;
        let _ = server.await;
    })
}

impl OAuthServerInfo {
    pub fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
        // `server.close()` (openai-codex.ts:266) stops the server, and the
        // `error` path of the TypeScript resolves the wait with `null`
        // (openai-codex.ts:274, :284). The Rust port settles the slot here as
        // well, so no exit path can leave a waiter pending forever. A code that
        // was already delivered is kept, because `CallbackSlot::settle` keeps
        // the first value.
        self.slot.cancel();
    }

    pub fn cancel_wait(&self) {
        self.slot.cancel();
    }

    pub async fn wait_for_code(&self) -> Option<String> {
        self.slot.wait().await
    }
}

impl Drop for OAuthServerInfo {
    fn drop(&mut self) {
        self.close();
    }
}

async fn start_local_oauth_server(state: &str) -> Result<OAuthServerInfo, String> {
    let host = oauth_callback_host();
    let listener = match bind_callback_listener(&host, CALLBACK_PORT).await {
        Ok(listener) => listener,
        Err(_) => {
            // packages/ai/src/utils/oauth/openai-codex.ts:273-285: the Node server
            // emits "error" when the listen on port 1455 fails, that handler calls
            // `settleWait?.(null)` (openai-codex.ts:274) and then resolves a server
            // whose `waitForCode: async () => null` (openai-codex.ts:284). The wait
            // therefore resolves with null immediately, and `loginOpenAICodex`
            // falls through to the paste prompt at openai-codex.ts:378-388.
            let slot: CallbackSlot<String> = CallbackSlot::default();
            slot.cancel();
            return Ok(OAuthServerInfo { slot, handle: None });
        }
    };

    let slot: CallbackSlot<String> = CallbackSlot::default();
    let handler_slot = slot.clone();
    let expected_state = state.to_string();
    let handle = spawn_http_callback_server(listener, move |path, params| {
        if path != CALLBACK_PATH {
            return (404, oauth_error_html("Callback route not found.", None));
        }
        if params.get("state").map(String::as_str) != Some(expected_state.as_str()) {
            return (400, oauth_error_html("State mismatch.", None));
        }
        let Some(code) = params.get("code").cloned() else {
            return (400, oauth_error_html("Missing authorization code.", None));
        };
        handler_slot.settle(Some(code));
        (
            200,
            oauth_success_html("OpenAI authentication completed. You can close this window."),
        )
    });
    let handle = guard_callback_server(slot.clone(), handle);

    Ok(OAuthServerInfo {
        slot,
        handle: Some(handle),
    })
}

fn get_account_id(access_token: &str) -> Option<String> {
    let payload = decode_jwt(access_token)?;
    let account_id = payload
        .get(JWT_CLAIM_PATH)
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)?;
    if account_id.is_empty() {
        None
    } else {
        Some(account_id.to_string())
    }
}

/// Options accepted by `loginOpenAICodex`.
pub struct OpenAICodexLoginOptions {
    pub on_auth: Option<Arc<dyn Fn(OAuthAuthInfo) + Send + Sync>>,
    pub on_prompt: Option<Arc<dyn Fn(OAuthPrompt) -> BoxFuture<String> + Send + Sync>>,
    pub on_progress: Option<Arc<dyn Fn(String) + Send + Sync>>,
    /// Races with browser callback - whichever completes first wins.
    pub on_manual_code_input: Option<crate::utils::oauth::types::OnManualCodeInput>,
    pub originator: Option<String>,
}

/// Login with OpenAI Codex OAuth.
pub async fn login_openai_codex(options: OpenAICodexLoginOptions) -> Result<OAuthCredentials, String> {
    let flow = create_authorization_flow(options.originator.as_deref()).await;
    let mut server = start_local_oauth_server(&flow.state).await?;

    if let Some(on_auth) = options.on_auth.as_ref() {
        on_auth(OAuthAuthInfo {
            url: flow.url.clone(),
            instructions: Some(
                "A browser window should open. Complete login to finish.".to_string(),
            ),
        });
    }

    let mut code: Option<String> = None;
    if let Some(on_manual_code_input) = options.on_manual_code_input.as_ref() {
        // Race between browser callback and manual input.
        let slot = server.slot.clone();
        let on_manual_code_input = on_manual_code_input.clone();
        let manual_promise = tokio::spawn(async move {
            let result = on_manual_code_input().await;
            // Both `.then` and `.catch` call `server.cancelWait()`.
            slot.cancel();
            result
        });

        let callback_result = server.wait_for_code().await;

        let mut manual_code: Option<String> = None;
        let mut manual_error: Option<String> = None;
        if callback_result.is_none() {
            // The manual promise won the race (or was cancelled).
            match manual_promise.await {
                Ok(Ok(input)) => manual_code = Some(input),
                Ok(Err(error)) => manual_error = Some(error),
                Err(_) => {}
            }
        } else {
            manual_promise.abort();
        }

        // If manual input was cancelled, throw that error.
        if let Some(error) = manual_error {
            return Err(error);
        }

        if let Some(callback_code) = callback_result {
            // Browser callback won.
            code = Some(callback_code);
        } else if let Some(input) = manual_code.as_deref() {
            // Manual input won (or callback timed out and user had entered code).
            let parsed = parse_authorization_input(input);
            if let Some(state) = parsed.state.as_deref() {
                if state != flow.state {
                    return Err("State mismatch".to_string());
                }
            }
            code = parsed.code;
        }
    } else {
        code = server.wait_for_code().await;
    }

    // Fallback to onPrompt if still no code.
    if code.is_none() {
        let Some(on_prompt) = options.on_prompt.as_ref() else {
            return Err("Missing authorization code".to_string());
        };
        let input = on_prompt(OAuthPrompt {
            message: "Paste the authorization code (or full redirect URL):".to_string(),
            placeholder: None,
            allow_empty: None,
        })
        .await;
        let parsed = parse_authorization_input(&input);
        if let Some(state) = parsed.state.as_deref() {
            if state != flow.state {
                return Err("State mismatch".to_string());
            }
        }
        code = parsed.code;
    }

    let Some(code) = code else {
        return Err("Missing authorization code".to_string());
    };

    let token_result = exchange_authorization_code(&code, &flow.verifier, None).await;
    let (access, refresh, expires) = match token_result {
        TokenResult::Success {
            access,
            refresh,
            expires,
        } => (access, refresh, expires),
        TokenResult::Failed { message, .. } => return Err(message),
    };

    let Some(account_id) = get_account_id(&access) else {
        return Err("Failed to extract accountId from token".to_string());
    };

    let mut extra = serde_json::Map::new();
    extra.insert("accountId".to_string(), Value::String(account_id));

    server.close();
    Ok(OAuthCredentials {
        access,
        refresh,
        expires,
        extra,
    })
}

/// Refresh OpenAI Codex OAuth token.
pub async fn refresh_openai_codex_token(refresh_token: &str) -> Result<OAuthCredentials, String> {
    let result = refresh_access_token(refresh_token).await;
    let (access, refresh, expires) = match result {
        TokenResult::Success {
            access,
            refresh,
            expires,
        } => (access, refresh, expires),
        TokenResult::Failed { message, .. } => return Err(message),
    };

    let Some(account_id) = get_account_id(&access) else {
        return Err("Failed to extract accountId from token".to_string());
    };

    let mut extra = serde_json::Map::new();
    extra.insert("accountId".to_string(), Value::String(account_id));

    Ok(OAuthCredentials {
        access,
        refresh,
        expires,
        extra,
    })
}

/// `openaiCodexOAuthProvider`.
pub fn openai_codex_oauth_provider() -> OAuthProviderInterface {
    OAuthProviderInterface {
        id: "openai-codex".to_string(),
        name: "ChatGPT Plus/Pro (Codex Subscription)".to_string(),
        uses_callback_server: Some(true),
        login: Arc::new(|callbacks: OAuthLoginCallbacks| {
            Box::pin(async move {
                login_openai_codex(OpenAICodexLoginOptions {
                    on_auth: callbacks.on_auth.clone(),
                    on_prompt: callbacks.on_prompt.clone(),
                    on_progress: callbacks.on_progress.clone(),
                    on_manual_code_input: callbacks.on_manual_code_input.clone(),
                    originator: None,
                })
                .await
            }) as crate::types::BoxFuture<Result<OAuthCredentials, String>>
        }),
        refresh_token: Arc::new(|credentials: OAuthCredentials| {
            Box::pin(async move { refresh_openai_codex_token(&credentials.refresh).await })
                as crate::types::BoxFuture<Result<OAuthCredentials, String>>
        }),
        get_api_key: Arc::new(|credentials: &OAuthCredentials| credentials.access.clone()),
        modify_models: None,
    }
}

/// Helper for tests: unused `HashMap`/`Model` import guards.
#[allow(dead_code)]
fn _assert_unused(_: HashMap<String, String>, _: Option<Model>) {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    /// The OAuth callback tests all bind the real `CALLBACK_PORT`, so they must
    /// not run at the same time.
    static CALLBACK_PORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Bound for the interactive assertions: the flows must resolve quickly, and
    /// a hang must fail the test instead of blocking it.
    const FLOW_BOUND: Duration = Duration::from_secs(5);

    #[test]
    fn state_is_32_hex_chars() {
        let state = create_state();
        assert_eq!(state.len(), 32);
        assert!(state.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(state, create_state());
    }

    #[test]
    fn parses_authorization_inputs() {
        let parsed = parse_authorization_input(
            "http://localhost:1455/auth/callback?code=abc&state=def",
        );
        assert_eq!(parsed.code.as_deref(), Some("abc"));
        assert_eq!(parsed.state.as_deref(), Some("def"));

        let parsed = parse_authorization_input("abc#def");
        assert_eq!(parsed.code.as_deref(), Some("abc"));
        assert_eq!(parsed.state.as_deref(), Some("def"));

        let parsed = parse_authorization_input("code=abc&state=def");
        assert_eq!(parsed.code.as_deref(), Some("abc"));

        let parsed = parse_authorization_input("bare");
        assert_eq!(parsed.code.as_deref(), Some("bare"));
        assert_eq!(parsed.state, None);

        assert_eq!(parse_authorization_input("   "), AuthorizationInput::default());
    }

    #[test]
    fn decode_jwt_reads_payload_and_account_id() {
        // {"https://api.openai.com/auth":{"chatgpt_account_id":"acc-1"}}
        let payload = "eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjLTEifX0";
        let token = format!("header.{}.signature", payload);
        let decoded = decode_jwt(&token).unwrap();
        assert_eq!(decoded[JWT_CLAIM_PATH]["chatgpt_account_id"], "acc-1");
        assert_eq!(get_account_id(&token).as_deref(), Some("acc-1"));

        assert!(decode_jwt("not-a-jwt").is_none());
        assert!(get_account_id("not-a-jwt").is_none());
    }

    #[test]
    fn constants_match_typescript() {
        assert_eq!(CLIENT_ID, "app_EMoamEEZ73f0CkXaXp7hrann");
        assert_eq!(AUTHORIZE_URL, "https://auth.openai.com/oauth/authorize");
        assert_eq!(TOKEN_URL, "https://auth.openai.com/oauth/token");
        assert_eq!(REDIRECT_URI, "http://localhost:1455/auth/callback");
        assert_eq!(SCOPE, "openid profile email offline_access");
        assert_eq!(JWT_CLAIM_PATH, "https://api.openai.com/auth");
        assert_eq!(CALLBACK_PORT, 1455);
        assert_eq!(CALLBACK_PATH, "/auth/callback");
    }

    #[tokio::test]
    async fn authorization_flow_sets_expected_query_params() {
        let flow = create_authorization_flow(None).await;
        assert_eq!(flow.state.len(), 32);
        assert_eq!(flow.verifier.len(), 43);
        assert!(flow.url.starts_with(AUTHORIZE_URL));
        assert!(flow.url.contains("code_challenge_method=S256"));
        assert!(flow.url.contains("originator=pi"));
        assert!(flow.url.contains("codex_cli_simplified_flow=true"));
        assert!(flow.url.contains("id_token_add_organizations=true"));

        let custom = create_authorization_flow(Some("other")).await;
        assert!(custom.url.contains("originator=other"));
    }

    #[tokio::test]
    async fn local_server_validates_state_and_returns_code() {
        let _guard = CALLBACK_PORT_LOCK.lock().await;
        let state = create_state();
        let mut server = start_local_oauth_server(&state).await.unwrap();

        let response = reqwest::get(format!(
            "http://localhost:{}{}?code=abc&state=wrong",
            CALLBACK_PORT, CALLBACK_PATH
        ))
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 400);
        assert!(response.text().await.unwrap().contains("State mismatch."));

        let response = reqwest::get(format!(
            "http://localhost:{}{}?code=abc&state={}",
            CALLBACK_PORT, CALLBACK_PATH, state
        ))
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(server.wait_for_code().await.as_deref(), Some("abc"));
        server.close();
    }

    /// Occupies `CALLBACK_PORT` so the OAuth callback server cannot bind it,
    /// which is the state the TypeScript `server.on("error", ...)` handler
    /// (packages/ai/src/utils/oauth/openai-codex.ts:273-285) covers.
    struct CallbackPortLock {
        listener: Option<tokio::net::TcpListener>,
    }

    impl CallbackPortLock {
        async fn take() -> Self {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", CALLBACK_PORT))
                .await
                .expect("bind the callback port for the bind-failure test");
            Self {
                listener: Some(listener),
            }
        }
    }

    impl Drop for CallbackPortLock {
        fn drop(&mut self) {
            drop(self.listener.take());
        }
    }

    /// Reads the `state` query parameter of the authorization URL that
    /// `loginOpenAICodex` reports through `onAuth` (the URL is the only way the
    /// test can learn the state the flow generated).
    fn state_from_auth_info(info: &OAuthAuthInfo) -> String {
        let url = url::Url::parse(&info.url).expect("authorization url");
        url.query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.to_string())
            .expect("authorization url carries a state")
    }

    #[tokio::test]
    async fn bind_failure_settles_the_slot_so_the_wait_resolves() {
        let _guard = CALLBACK_PORT_LOCK.lock().await;
        let port_lock = CallbackPortLock::take().await;

        let server = start_local_oauth_server(&create_state()).await.unwrap();
        // openai-codex.ts:274 `settleWait?.(null)` + :284 `waitForCode: async () => null`.
        let waited = tokio::time::timeout(FLOW_BOUND, server.wait_for_code())
            .await
            .expect("bind failure must settle the callback slot; the wait hung");
        assert_eq!(waited, None);
        drop(port_lock);
    }

    #[tokio::test]
    async fn bind_failure_falls_through_to_the_paste_prompt() {
        let _guard = CALLBACK_PORT_LOCK.lock().await;
        let port_lock = CallbackPortLock::take().await;

        // Mirrors `if (!code) { const input = await options.onPrompt(...) }`
        // (packages/ai/src/utils/oauth/openai-codex.ts:378-388). A hang here means
        // the flow never reached the prompt.
        let auth_state: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        let prompt_state_reply = auth_state.clone();
        let auth_state_clone = auth_state.clone();
        let options = OpenAICodexLoginOptions {
            on_auth: Some(Arc::new(move |info: OAuthAuthInfo| {
                *auth_state_clone.lock().unwrap() = Some(state_from_auth_info(&info));
            })),
            on_prompt: Some(Arc::new(move |prompt: OAuthPrompt| {
                let reply = prompt_state_reply.clone();
                Box::pin(async move {
                    assert_eq!(
                        prompt.message,
                        "Paste the authorization code (or full redirect URL):"
                    );
                    let state = reply
                        .lock()
                        .unwrap()
                        .clone()
                        .expect("onAuth ran before the paste prompt");
                    format!("pasted-code#{}", state)
                }) as crate::types::BoxFuture<String>
            })),
            on_progress: None,
            on_manual_code_input: None,
            originator: None,
        };

        let login = tokio::time::timeout(FLOW_BOUND, login_openai_codex(options))
            .await
            .expect("bind failure must reach the paste prompt instead of hanging");
        // The paste prompt returned a code, so the flow continues to the token
        // exchange, which needs the network. Reaching that exchange proves the
        // callback wait resolved and the prompt was used.
        let error = login.expect_err("no live token endpoint in this test");
        assert!(
            error.starts_with("OpenAI Codex token exchange"),
            "flow did not consume the pasted code: {}",
            error
        );
        drop(port_lock);
    }

    #[tokio::test]
    async fn bind_failure_does_not_skip_the_manual_code_input() {
        let _guard = CALLBACK_PORT_LOCK.lock().await;
        let port_lock = CallbackPortLock::take().await;

        let manual_seen = Arc::new(AtomicBool::new(false));
        let manual_seen_clone = manual_seen.clone();
        let auth_state: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
        let auth_state_clone = auth_state.clone();
        let options = OpenAICodexLoginOptions {
            on_auth: Some(Arc::new(move |info: OAuthAuthInfo| {
                *auth_state_clone.lock().unwrap() = Some(state_from_auth_info(&info));
            })),
            on_prompt: Some(Arc::new(|_prompt: OAuthPrompt| {
                Box::pin(async move { "should-not-be-prompted".to_string() })
                    as crate::types::BoxFuture<String>
            })),
            on_progress: None,
            on_manual_code_input: Some(Arc::new(move || {
                let seen = manual_seen_clone.clone();
                Box::pin(async move {
                    seen.store(true, Ordering::SeqCst);
                    Ok("manual-code".to_string())
                }) as crate::types::BoxFuture<Result<String, String>>
            })),
            originator: None,
        };

        let login = tokio::time::timeout(FLOW_BOUND, login_openai_codex(options))
            .await
            .expect("bind failure must not hang the manual-input race");
        assert!(manual_seen.load(Ordering::SeqCst));
        let error = login.expect_err("no live token endpoint in this test");
        assert!(
            error.starts_with("OpenAI Codex token exchange"),
            "flow did not consume the manual code: {}",
            error
        );
        drop(port_lock);
    }

    #[tokio::test]
    async fn stopped_callback_server_settles_the_wait() {
        let _guard = CALLBACK_PORT_LOCK.lock().await;
        let state = create_state();
        let mut server = start_local_oauth_server(&state).await.unwrap();

        // The server task can no longer serve the callback, so the wait must
        // resolve with `null` (openai-codex.ts:274/:284) instead of hanging.
        server.close();
        let waited = tokio::time::timeout(FLOW_BOUND, server.wait_for_code())
            .await
            .expect("a stopped callback server must settle the wait");
        assert_eq!(waited, None);
    }

    #[tokio::test]
    async fn close_keeps_a_code_that_was_already_delivered() {
        let _guard = CALLBACK_PORT_LOCK.lock().await;
        let state = create_state();
        let mut server = start_local_oauth_server(&state).await.unwrap();

        let response = reqwest::get(format!(
            "http://localhost:{}{}?code=kept&state={}",
            CALLBACK_PORT, CALLBACK_PATH, state
        ))
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 200);

        server.close();
        let waited = tokio::time::timeout(FLOW_BOUND, server.wait_for_code())
            .await
            .expect("the wait must resolve");
        assert_eq!(waited.as_deref(), Some("kept"));
    }


    #[test]
    fn provider_identity() {
        let provider = openai_codex_oauth_provider();
        assert_eq!(provider.id, "openai-codex");
        assert_eq!(provider.name, "ChatGPT Plus/Pro (Codex Subscription)");
        assert_eq!(provider.uses_callback_server, Some(true));
    }
}
