//! Port of packages/coding-agent/src/modes/telegram/worker.ts

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use pi_agent_core::types::AgentMessage;
use pi_ai::types::BoxFuture;

use crate::modes::agent_connection::daemon_agent_connection::{
    DaemonAgentConnection, DaemonAgentConnectionOptions, DaemonEventCursor as ConnectionEventCursor,
    DaemonOutbound as ConnectionOutbound, DaemonResponse as ConnectionResponse, DaemonSessionSnapshot,
    DaemonSessionStreamKind, DaemonSessionSummary as ConnectionSessionSummary, DaemonTransportClient,
};
use crate::modes::daemon::daemon_protocol::{DaemonCommand};
use crate::modes::daemon::daemon_client::{DaemonHello};
use crate::modes::daemon::daemon_client::{
    DaemonClient, DaemonClientError, DaemonClientMessageListener, DaemonClientRequestOptions,
};
use crate::modes::daemon::daemon_protocol::{
    DaemonEventCursor, DaemonOutbound as ProtocolOutbound, DaemonSessionClosedReason,
};
use crate::modes::daemon::daemon_session_list::SessionSummary;
use crate::modes::telegram::api::{TelegramApi, TelegramApiError, TelegramCallError};
use crate::modes::telegram::bridge::{accept_telegram_pairing, TelegramBridge};
use crate::modes::telegram::manager::{
    telegram_stop_requested, TelegramFileLock, TelegramLockError, TELEGRAM_WORKER_LOCK_OPTIONS,
};
use crate::modes::telegram::store::{
    is_record, TelegramConnectionSettings, TelegramDelivery, TelegramState, TelegramStore, TelegramWorkerStatus,
};

/// `fetch` - the daemon-independent default fetcher for `new TelegramApi(token)`.
///
/// Private to this module: `core/extensions/builtin/telegram.ts` owns its own
/// fetcher for the `/telegram` command path.
struct ReqwestTelegramFetcher;

impl crate::modes::telegram::api::TelegramFetcher for ReqwestTelegramFetcher {
    fn fetch(
        &self,
        url: String,
        body: String,
        timeout_ms: u64,
    ) -> BoxFuture<Result<crate::modes::telegram::api::TelegramHttpResponse, String>> {
        Box::pin(async move {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_millis(timeout_ms))
                .build()
                .map_err(|error| error.to_string())?;
            let response = client
                .post(&url)
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16() as f64;
            let ok = response.status().is_success();
            let body = response.bytes().await.map_err(|error| error.to_string())?.to_vec();
            Ok(crate::modes::telegram::api::TelegramHttpResponse { ok, status, body })
        })
    }
}

/// `new TelegramApi(settings.botToken)`.
fn default_create_api(token: &str) -> Result<TelegramApi, String> {
    TelegramApi::new(
        token,
        crate::modes::telegram::api::TELEGRAM_DEFAULT_BASE_URL,
        Arc::new(ReqwestTelegramFetcher),
    )
}

/// `randomUUID()`.
fn random_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// `Date.now()`.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `process.pid`.
fn process_pid() -> f64 {
    std::process::id() as f64
}

/// `await delay(ms, undefined, { signal })`: resolves early on abort.
async fn delay_ms(ms: u64, controller: &CancellationToken) {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
        _ = controller.cancelled() => {}
    }
}

/// `error instanceof TelegramApiError` for the worker's error branches.
fn telegram_api_error(error: &TelegramCallError) -> Option<&TelegramApiError> {
    match error {
        TelegramCallError::Api(api_error) => Some(api_error),
        _ => None,
    }
}

/// `error instanceof TelegramApiError && (error.code === 401 || error.code === 409)`.
fn is_fatal_api_error(error: &TelegramCallError) -> bool {
    telegram_api_error(error)
        .map(|api_error| api_error.code == 401.0 || api_error.code == 409.0)
        .unwrap_or(false)
}

/// `error instanceof TelegramApiError && error.retryAfter ? error.retryAfter * 1000 : fallback`.
fn retry_delay_ms(error: &TelegramCallError, fallback: f64) -> f64 {
    telegram_api_error(error)
        .map(|api_error| if api_error.retry_after != 0.0 { api_error.retry_after * 1000.0 } else { fallback })
        .unwrap_or(fallback)
}
