//! Port of packages/coding-agent/src/core/prompt-admission.ts

use std::fmt;

use tokio_util::sync::CancellationToken;

/// The TypeScript `PromptAdmissionCancelledError` message literal.
pub const PROMPT_ADMISSION_CANCELLED_MESSAGE: &str = "Prompt admission was cancelled.";

/// The TypeScript `name` assigned to the error instance.
pub const PROMPT_ADMISSION_CANCELLED_NAME: &str = "PromptAdmissionCancelledError";

/// `class PromptAdmissionCancelledError extends Error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptAdmissionCancelledError {
    pub name: String,
    pub message: String,
}

impl PromptAdmissionCancelledError {
    pub fn new() -> Self {
        Self {
            name: PROMPT_ADMISSION_CANCELLED_NAME.to_string(),
            message: PROMPT_ADMISSION_CANCELLED_MESSAGE.to_string(),
        }
    }
}

impl Default for PromptAdmissionCancelledError {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for PromptAdmissionCancelledError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PromptAdmissionCancelledError {}

/// `throwIfPromptAdmissionCancelled(signal)`.
///
/// `AbortSignal` maps to a `CancellationToken` (see docs/PORT-RULES.md).
pub fn throw_if_prompt_admission_cancelled(
    signal: Option<&CancellationToken>,
) -> Result<(), PromptAdmissionCancelledError> {
    if signal.map(|signal| signal.is_cancelled()).unwrap_or(false) {
        return Err(PromptAdmissionCancelledError::new());
    }
    Ok(())
}

/// `waitForPromptAdmission(promise, signal)`.
///
/// Await `work` unless `signal` aborts first. Cancelling here always observes
/// the supplied work's rejection so a cancelled admission never leaks an
/// unhandled rejection: in Rust the pending future is dropped, and this task is
/// also kept alive so a spawned producer finishes instead of being forgotten.
pub async fn wait_for_prompt_admission<T, F>(
    work: F,
    signal: Option<CancellationToken>,
) -> Result<T, PromptAdmissionCancelledError>
where
    F: std::future::Future<Output = T> + Send,
{
    let Some(signal) = signal else {
        return Ok(work.await);
    };
    if signal.is_cancelled() {
        // `void promise.catch(() => {})`: the work still runs to completion, its
        // result is simply discarded.
        tokio::spawn(async move {
            let _ = work.await;
        });
        return Err(PromptAdmissionCancelledError::new());
    }
    tokio::select! {
        value = work => Ok(value),
        _ = signal.cancelled() => Err(PromptAdmissionCancelledError::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_error_carries_the_typescript_name_and_message() {
        let error = PromptAdmissionCancelledError::new();
        assert_eq!(error.name, "PromptAdmissionCancelledError");
        assert_eq!(error.to_string(), "Prompt admission was cancelled.");
    }

    #[test]
    fn throw_if_cancelled_ignores_absent_and_live_signals() {
        assert!(throw_if_prompt_admission_cancelled(None).is_ok());
        let live = CancellationToken::new();
        assert!(throw_if_prompt_admission_cancelled(Some(&live)).is_ok());
        live.cancel();
        assert_eq!(
            throw_if_prompt_admission_cancelled(Some(&live)).unwrap_err().message,
            "Prompt admission was cancelled."
        );
    }

    #[tokio::test]
    async fn pre_aborted_admission_rejects_and_still_observes_the_work() {
        let signal = CancellationToken::new();
        signal.cancel();
        let result = wait_for_prompt_admission(async { "late" }, Some(signal)).await;
        assert!(result.is_err());
        // Let the observed work settle; an unobserved panic would fail the test.
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn mid_wait_abort_rejects_with_the_typed_error() {
        let signal = CancellationToken::new();
        let abort = signal.clone();
        let future = wait_for_prompt_admission(std::future::pending::<()>(), Some(signal));
        abort.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), future).await;
        assert!(matches!(result, Ok(Err(_))));
    }

    #[tokio::test]
    async fn work_settles_normally_without_a_signal_and_with_a_live_signal() {
        assert_eq!(wait_for_prompt_admission(async { "value" }, None).await.unwrap(), "value");
        let live = CancellationToken::new();
        assert_eq!(
            wait_for_prompt_admission(async { "value" }, Some(live)).await.unwrap(),
            "value"
        );
    }
}
