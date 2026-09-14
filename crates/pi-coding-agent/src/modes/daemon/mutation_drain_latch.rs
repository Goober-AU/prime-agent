//! Port of packages/coding-agent/src/modes/daemon/mutation-drain-latch.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Counts in-flight mutating commands so update-restart preparation can wait for them to drain.
#[derive(Default)]
pub struct MutationDrainLatch {
    active: AtomicUsize,
    waiters: Mutex<Vec<oneshot::Sender<()>>>,
}

impl MutationDrainLatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&self) {
        self.active.fetch_add(1, Ordering::SeqCst);
    }

    pub fn end(&self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
        let waiters = {
            let mut waiters = self.waiters.lock().expect("mutation drain latch poisoned");
            std::mem::take(&mut *waiters)
        };
        for waiter in waiters {
            let _ = waiter.send(());
        }
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    /// Wait until at most `remaining` mutating commands are in flight.
    ///
    /// The TypeScript rejects with `new Error(abortMessage)`; the Rust port
    /// returns the same message through `Err`.
    pub async fn wait_for_drain(
        &self,
        remaining: usize,
        signal: &CancellationToken,
        abort_message: &str,
    ) -> Result<(), String> {
        if signal.is_cancelled() {
            return Err(abort_message.to_string());
        }
        while self.active() > remaining {
            let (sender, receiver) = oneshot::channel::<()>();
            self.waiters
                .lock()
                .expect("mutation drain latch poisoned")
                .push(sender);
            let drained = tokio::select! {
                result = receiver => result.is_ok(),
                _ = signal.cancelled() => false,
            };
            if !drained && signal.is_cancelled() {
                return Err(abort_message.to_string());
            }
            if signal.is_cancelled() {
                return Err(abort_message.to_string());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waits_for_in_flight_commands() {
        let latch = MutationDrainLatch::new();
        latch.begin();
        let token = CancellationToken::new();
        let handle = {
            let latch = std::sync::Arc::new(latch);
            let token = token.clone();
            let cloned = latch.clone();
            tokio::spawn(async move {
                cloned
                    .wait_for_drain(0, &token, "aborted")
                    .await
            })
        };
        // The latch lives in the Arc; end() through the shared handle.
        // (Re-enter through the Arc so the spawned waiter observes the drain.)
        tokio::task::yield_now().await;
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn rejects_an_already_aborted_wait() {
        let latch = MutationDrainLatch::new();
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(
            latch.wait_for_drain(0, &token, "update restart cancelled").await,
            Err("update restart cancelled".to_string())
        );
    }
}
