//! Port of packages/coding-agent/src/utils/sleep.ts

use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Sleep helper that respects an abort signal (port of `sleep(ms, signal)`).
///
/// The TypeScript rejects with `new Error("Aborted")`; the Rust port reports the
/// same message through `Err`.
pub async fn sleep(ms: u64, signal: Option<&CancellationToken>) -> Result<(), std::io::Error> {
    if let Some(signal) = signal {
        if signal.is_cancelled() {
            return Err(std::io::Error::other("Aborted"));
        }
    }

    match signal {
        None => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(())
        }
        Some(signal) => {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
                _ = signal.cancelled() => Err(std::io::Error::other("Aborted")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sleeps_without_a_signal() {
        assert!(sleep(1, None).await.is_ok());
    }

    #[tokio::test]
    async fn rejects_when_already_aborted() {
        let token = CancellationToken::new();
        token.cancel();
        let error = sleep(10_000, Some(&token)).await.unwrap_err();
        assert_eq!(error.to_string(), "Aborted");
    }

    #[tokio::test]
    async fn aborts_while_waiting() {
        let token = CancellationToken::new();
        let cloned = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            cloned.cancel();
        });
        let error = sleep(10_000, Some(&token)).await.unwrap_err();
        assert_eq!(error.to_string(), "Aborted");
    }
}
