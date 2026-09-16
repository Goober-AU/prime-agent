//! Port of packages/coding-agent/src/modes/telegram/api.ts

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::modes::telegram::store::{is_record, is_telegram_id, is_telegram_update_id, valid_bot_token};

/// `TelegramApiError`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{}", message)]
pub struct TelegramApiError {
    pub code: f64,
    pub retry_after: f64,
    pub message: String,
}

impl TelegramApiError {
    pub fn new(code: f64, retry_after: f64) -> Self {
        let message = if code == 401.0 {
            "Telegram rejected the bot token. Create a new token in BotFather and run /telegram setup."
                .to_string()
        } else if code == 409.0 {
            "Another Telegram poller or webhook is using this bot. Stop it or create a separate bot for Prime."
                .to_string()
        } else if code == 403.0 {
            "Telegram delivery was blocked. Open the bot chat and unblock the bot.".to_string()
        } else {
            format!("Telegram request failed ({code}).")
        };
        Self {
            code,
            retry_after,
            message,
        }
    }
}

/// `TelegramMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramMessage {
    pub message_id: f64,
    pub date: f64,
    pub from: TelegramUser,
    pub chat: TelegramChat,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramUser {
    pub id: f64,
    pub is_bot: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramChat {
    pub id: f64,
    #[serde(rename = "type")]
    pub type_: String,
}

/// `TelegramUpdate`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramUpdate {
    pub update_id: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,
}

/// JavaScript `String.length`: the number of UTF-16 code units.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// `privateMessage(update)`.
pub fn private_message(update: &Value) -> Option<TelegramMessage> {
    if !is_record(update) {
        return None;
    }
    let message = update.get("message")?;
    if !is_record(message) {
        return None;
    }
    let from = message.get("from")?;
    let chat = message.get("chat")?;
    if !is_record(from) || !is_record(chat) {
        return None;
    }
    let text = message.get("text");
    let text_ok = match text {
        None => true,
        // `text.length` in the reference counts UTF-16 code units, not code points.
        Some(Value::String(text)) => utf16_len(text) <= 16384,
        Some(_) => false,
    };
    if chat.get("type").and_then(Value::as_str) != Some("private")
        || from.get("is_bot").and_then(Value::as_bool) != Some(false)
        || !is_telegram_id(from.get("id").unwrap_or(&Value::Null))
        || chat.get("id") != from.get("id")
        || !is_telegram_id(message.get("message_id").unwrap_or(&Value::Null))
        || !message
            .get("date")
            .and_then(Value::as_f64)
            .map(|date| date.fract() == 0.0 && date.abs() < 9_007_199_254_740_992.0)
            .unwrap_or(false)
        || !text_ok
    {
        return None;
    }
    serde_json::from_value(message.clone()).ok()
}

/// `splitTelegramText(text, limit)`.
pub fn split_telegram_text(text: &str, limit: usize) -> Result<Vec<String>, String> {
    if limit < 2 || limit > 4096 {
        return Err("Invalid Telegram message limit.".to_string());
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut chunk = String::new();
    let mut chunk_units = 0usize;
    for character in text.chars() {
        // `chunk.length + character.length` in the reference: both are UTF-16 code units.
        let character_units = character.len_utf16();
        if chunk_units + character_units > limit {
            chunks.push(std::mem::take(&mut chunk));
            chunk_units = 0;
        }
        chunk.push(character);
        chunk_units += character_units;
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    Ok(chunks)
}

/// The `signal?: AbortSignal` argument of every `TelegramApi` request.
///
/// The port modelled the reference's optional signal as a boolean ("this request
/// carries a signal", which selects the longer signalled timeout). Keeping that
/// form convertible means every existing call site still reads the same while a
/// real `CancellationToken` — which can actually abort the in-flight request — is
/// accepted alongside it.
#[derive(Debug, Clone, Default)]
pub struct TelegramSignal(Option<CancellationToken>);

impl TelegramSignal {
    /// `AbortSignal.timeout(hasSignal ? 40_000 : 15_000)`.
    pub fn timeout_ms(&self) -> u64 {
        if self.0.is_some() {
            TELEGRAM_SIGNALLED_REQUEST_TIMEOUT_MS
        } else {
            TELEGRAM_REQUEST_TIMEOUT_MS
        }
    }

    /// The abort handle, when a real one was supplied.
    pub fn token(&self) -> Option<&CancellationToken> {
        self.0.as_ref()
    }
}

impl From<bool> for TelegramSignal {
    /// `true`: the reference passes a signal that is never aborted in this call.
    fn from(has_signal: bool) -> Self {
        Self(has_signal.then(CancellationToken::new))
    }
}

impl From<Option<CancellationToken>> for TelegramSignal {
    fn from(token: Option<CancellationToken>) -> Self {
        Self(token)
    }
}

impl From<CancellationToken> for TelegramSignal {
    fn from(token: CancellationToken) -> Self {
        Self(Some(token))
    }
}

/// The HTTP surface `TelegramApi.call` uses.
///
/// blocked_on: the port cannot depend on a Node `fetch`; the seam mirrors the
/// request the TypeScript issues (POST, JSON body, response bytes).
pub trait TelegramFetcher: Send + Sync {
    /// `fetcher(url, { method, headers, body, signal })`.
    ///
    /// The request is raced against its `AbortSignal` by the caller, so a cancelled
    /// request drops this future — the same effect as the reference's aborted fetch.
    fn fetch(
        &self,
        url: String,
        body: String,
        timeout_ms: u64,
    ) -> pi_ai::types::BoxFuture<Result<TelegramHttpResponse, String>>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct TelegramHttpResponse {
    pub ok: bool,
    pub status: f64,
    pub body: Vec<u8>,
}

/// `class TelegramApi`.
pub struct TelegramApi {
    token: String,
    base_url: String,
    fetcher: std::sync::Arc<dyn TelegramFetcher>,
}

pub const TELEGRAM_DEFAULT_BASE_URL: &str = "https://api.telegram.org";
const TELEGRAM_REQUEST_TIMEOUT_MS: u64 = 15_000;
const TELEGRAM_SIGNALLED_REQUEST_TIMEOUT_MS: u64 = 40_000;
const TELEGRAM_MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

impl TelegramApi {
    pub fn new(token: &str, base_url: &str, fetcher: std::sync::Arc<dyn TelegramFetcher>) -> Result<Self, String> {
        if !valid_bot_token(token) {
            return Err("Invalid Telegram bot token format.".to_string());
        }
        Ok(Self {
            token: token.to_string(),
            base_url: base_url.to_string(),
            fetcher,
        })
    }

    /// `redact(error)`.
    pub fn redact(&self, error: &str) -> String {
        let message = if error.is_empty() {
            "Telegram operation failed."
        } else {
            error
        };
        message.replace(&self.token, "[redacted]")
    }

    /// `call(method, body, signal)`.
    pub async fn call(
        &self,
        method: &str,
        body: Value,
        signal: impl Into<TelegramSignal>,
    ) -> Result<Value, TelegramCallError> {
        let signal = signal.into();
        let token = signal.token().cloned();
        let url = format!("{}/bot{}/{}", self.base_url, self.token, method);
        let body_text = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
        let timeout_ms = signal.timeout_ms();
        let request = self.fetcher.fetch(url, body_text, timeout_ms);
        let response = match match token.as_ref() {
            // `AbortSignal.any([signal, AbortSignal.timeout(...)])`: an abort ends the
            // in-flight request instead of waiting for the transport timeout.
            Some(token) => tokio::select! {
                result = request => result,
                _ = token.cancelled() => return Err(TelegramCallError::Aborted),
            },
            None => request.await,
        } {
            Ok(response) => response,
            Err(_) => {
                // `if (signal?.aborted) throw signal.reason`: only an actual abort is
                // reported as an abort. Everything else keeps the connectivity wording.
                if token.as_ref().is_some_and(CancellationToken::is_cancelled) {
                    return Err(TelegramCallError::Aborted);
                }
                return Err(TelegramCallError::Error(
                    "Cannot reach Telegram. Check this computer's internet connection.".to_string(),
                ));
            }
        };
        if response.body.len() > TELEGRAM_MAX_RESPONSE_BYTES {
            return Err(TelegramCallError::Error(
                "Telegram response was too large.".to_string(),
            ));
        }
        let Ok(data) = serde_json::from_slice::<Value>(&response.body) else {
            return Err(TelegramCallError::Error(
                "Telegram returned an invalid response.".to_string(),
            ));
        };
        if !is_record(&data) || !response.ok || data.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = data
                .get("error_code")
                .and_then(Value::as_f64)
                .unwrap_or(response.status);
            let retry = data
                .get("parameters")
                .and_then(|parameters| parameters.get("retry_after"))
                .and_then(Value::as_f64)
                .map(|retry| retry.clamp(0.0, 300.0))
                .unwrap_or(0.0);
            return Err(TelegramCallError::Api(TelegramApiError::new(code, retry)));
        }
        Ok(data.get("result").cloned().unwrap_or(Value::Null))
    }

    /// `identify(signal)`.
    pub async fn identify(&self, signal: impl Into<TelegramSignal>) -> Result<TelegramBotIdentity, TelegramCallError> {
        let result = self.call("getMe", Value::Object(Default::default()), signal).await?;
        static USERNAME: once_cell::sync::Lazy<regex::Regex> =
            once_cell::sync::Lazy::new(|| regex::Regex::new(r"^[A-Za-z0-9_]{1,64}$").expect("static regex"));
        let id = result.get("id").cloned().unwrap_or(Value::Null);
        let username = result.get("username").and_then(Value::as_str);
        if !is_telegram_id(&id)
            || result.get("is_bot").and_then(Value::as_bool) != Some(true)
            || username.map(|username| !USERNAME.is_match(username)).unwrap_or(true)
        {
            return Err(TelegramCallError::Error(
                "Telegram returned an invalid bot identity.".to_string(),
            ));
        }
        Ok(TelegramBotIdentity {
            id: id.as_f64().unwrap_or_default(),
            username: username.unwrap_or_default().to_string(),
        })
    }

    /// `requirePolling(signal)`.
    pub async fn require_polling(&self, signal: impl Into<TelegramSignal>) -> Result<(), TelegramCallError> {
        let result = self
            .call("getWebhookInfo", Value::Object(Default::default()), signal)
            .await?;
        let Some(url) = result.get("url").and_then(Value::as_str) else {
            return Err(TelegramCallError::Error(
                "Telegram returned invalid webhook information.".to_string(),
            ));
        };
        if !url.is_empty() {
            return Err(TelegramCallError::Error(
                "This bot already has a webhook. Disconnect its other integration or create a new bot for Prime in BotFather."
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// `updates(offset, signal)`.
    pub async fn updates(&self, offset: f64, signal: impl Into<TelegramSignal>) -> Result<Vec<TelegramUpdate>, TelegramCallError> {
        let result = self
            .call(
                "getUpdates",
                serde_json::json!({
                    "offset": offset,
                    "timeout": 25,
                    "limit": 50,
                    "allowed_updates": ["message"],
                }),
                signal,
            )
            .await?;
        let Some(updates) = result.as_array() else {
            return Err(TelegramCallError::Error(
                "Telegram returned invalid updates.".to_string(),
            ));
        };
        let all_valid = updates.iter().all(|update| {
            is_record(update) && is_telegram_update_id(update.get("update_id").unwrap_or(&Value::Null))
        });
        if !all_valid {
            return Err(TelegramCallError::Error(
                "Telegram returned invalid updates.".to_string(),
            ));
        }
        Ok(updates
            .iter()
            .filter_map(|update| serde_json::from_value(update.clone()).ok())
            .collect())
    }

    /// `send(chatId, text, signal)`.
    pub async fn send(
        &self,
        chat_id: f64,
        text: &str,
        signal: impl Into<TelegramSignal>,
    ) -> Result<(), TelegramCallError> {
        self.call(
            "sendMessage",
            serde_json::json!({
                "chat_id": chat_id,
                "text": text,
                "link_preview_options": { "is_disabled": true },
            }),
            signal,
        )
        .await
        .map(|_| ())
    }
}

/// `{ id: number; username: string }`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TelegramBotIdentity {
    pub id: f64,
    pub username: String,
}

/// `call()` failures: the API error, a transport error, or an aborted signal.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TelegramCallError {
    #[error(transparent)]
    Api(TelegramApiError),
    #[error("{0}")]
    Error(String),
    /// `throw signal.reason`.
    #[error("Telegram request aborted")]
    Aborted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_errors_use_the_documented_wording() {
        assert_eq!(
            TelegramApiError::new(401.0, 0.0).message,
            "Telegram rejected the bot token. Create a new token in BotFather and run /telegram setup."
        );
        assert_eq!(
            TelegramApiError::new(409.0, 0.0).message,
            "Another Telegram poller or webhook is using this bot. Stop it or create a separate bot for Prime."
        );
        assert_eq!(
            TelegramApiError::new(403.0, 0.0).message,
            "Telegram delivery was blocked. Open the bot chat and unblock the bot."
        );
        assert_eq!(TelegramApiError::new(500.0, 0.0).message, "Telegram request failed (500).");
    }

    #[test]
    fn private_messages_are_filtered_by_shape() {
        let update = serde_json::json!({
            "update_id": 3,
            "message": {
                "message_id": 1,
                "date": 1000,
                "from": {"id": 7, "is_bot": false},
                "chat": {"id": 7, "type": "private"},
                "text": "hi",
            },
        });
        let message = private_message(&update).unwrap();
        assert_eq!(message.message_id, 1.0);
        assert_eq!(message.text.as_deref(), Some("hi"));

        let group = serde_json::json!({
            "update_id": 3,
            "message": {
                "message_id": 1,
                "date": 1000,
                "from": {"id": 7, "is_bot": false},
                "chat": {"id": 7, "type": "group"},
            },
        });
        assert!(private_message(&group).is_none());

        let mismatched = serde_json::json!({
            "update_id": 3,
            "message": {
                "message_id": 1,
                "date": 1000,
                "from": {"id": 7, "is_bot": false},
                "chat": {"id": 8, "type": "private"},
            },
        });
        assert!(private_message(&mismatched).is_none());

        assert!(private_message(&serde_json::json!({"update_id": 1})).is_none());
    }

    #[test]
    fn text_splitting_counts_utf16_code_units() {
        assert_eq!(
            split_telegram_text("abcdef", 4).unwrap(),
            vec!["abcd".to_string(), "ef".to_string()]
        );
        assert_eq!(split_telegram_text("", 4).unwrap(), Vec::<String>::new());
        assert_eq!(split_telegram_text("ab", 2).unwrap(), vec!["ab".to_string()]);
        assert_eq!(
            split_telegram_text("x", 1).unwrap_err(),
            "Invalid Telegram message limit."
        );
        assert_eq!(
            split_telegram_text("x", 5000).unwrap_err(),
            "Invalid Telegram message limit."
        );
    }

    #[test]
    fn token_validation_gates_construction() {
        let fetcher = std::sync::Arc::new(NoopFetcher);
        assert!(TelegramApi::new("bad", TELEGRAM_DEFAULT_BASE_URL, fetcher).is_err());
    }

    struct NoopFetcher;

    impl TelegramFetcher for NoopFetcher {
        fn fetch(
            &self,
            _url: String,
            _body: String,
            _timeout_ms: u64,
        ) -> pi_ai::types::BoxFuture<Result<TelegramHttpResponse, String>> {
            Box::pin(async { Err("not used".to_string()) })
        }
    }

    #[tokio::test]
    async fn redaction_hides_the_token() {
        let api = TelegramApi::new(
            "123456789:AAAAAAAAAAAAAAAAAAAA",
            TELEGRAM_DEFAULT_BASE_URL,
            std::sync::Arc::new(NoopFetcher),
        )
        .unwrap();
        assert_eq!(
            api.redact("failed for 123456789:AAAAAAAAAAAAAAAAAAAA"),
            "failed for [redacted]"
        );
        assert_eq!(api.redact(""), "Telegram operation failed.");
    }
}
