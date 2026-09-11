//! Port of packages/coding-agent/src/utils/semaphore.ts

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum SemaphoreError {
    #[error("Semaphore permits must be a positive integer, got {0}")]
    InvalidPermits(i64),
    #[error("{0}")]
    Aborted(String),
}

struct Waiter {
    id: u64,
    sender: oneshot::Sender<()>,
}

struct State {
    available: usize,
    waiters: VecDeque<Waiter>,
}

/// Counting semaphore for bounding async concurrency. FIFO: waiters acquire in
/// the order they queued.
pub struct Semaphore {
    permits: usize,
    state: Mutex<State>,
    next_waiter_id: AtomicU64,
}

impl Semaphore {
    /// The TypeScript also rejects non-integer permits; Rust callers cannot
    /// express a fractional permit, so only the non-positive check remains.
    pub fn new(permits: i64) -> Result<Self, SemaphoreError> {
        if permits < 1 {
            return Err(SemaphoreError::InvalidPermits(permits));
        }
        Ok(Self {
            permits: permits as usize,
            state: Mutex::new(State {
                available: permits as usize,
                waiters: VecDeque::new(),
            }),
            next_waiter_id: AtomicU64::new(0),
        })
    }

    pub fn permits(&self) -> usize {
        self.permits
    }

    pub fn queue_length(&self) -> usize {
        self.state.lock().expect("semaphore state").waiters.len()
    }

    fn aborted_error() -> SemaphoreError {
        // `signal.reason ?? new Error("aborted")`: a cancelled token has no
        // reason payload, so the TypeScript default text is used.
        SemaphoreError::Aborted("aborted".to_string())
    }

    async fn acquire(&self, signal: Option<&CancellationToken>) -> Result<(), SemaphoreError> {
        if let Some(signal) = signal {
            if signal.is_cancelled() {
                return Err(Self::aborted_error());
            }
        }

        {
            let mut state = self.state.lock().expect("semaphore state");
            if state.available > 0 {
                state.available -= 1;
                return Ok(());
            }
        }

        // A queued waiter can be aborted before it gets a permit; on abort it
        // removes itself from the queue so it never consumes a slot.
        let (sender, mut receiver) = oneshot::channel::<()>();
        let id = self.next_waiter_id.fetch_add(1, Ordering::SeqCst);
        {
            let mut state = self.state.lock().expect("semaphore state");
            state.waiters.push_back(Waiter { id, sender });
        }

        let Some(signal) = signal else {
            let _ = receiver.await;
            return Ok(());
        };

        tokio::select! {
            result = &mut receiver => {
                let _ = result;
                Ok(())
            }
            _ = signal.cancelled() => {
                let removed = {
                    let mut state = self.state.lock().expect("semaphore state");
                    match state.waiters.iter().position(|waiter| waiter.id == id) {
                        Some(index) => {
                            state.waiters.remove(index);
                            true
                        }
                        None => false,
                    }
                };
                if !removed {
                    // release() already popped this waiter and is handing it the
                    // permit; take it back so the slot is not lost.
                    if receiver.await.is_ok() {
                        self.release();
                    }
                }
                Err(Self::aborted_error())
            }
        }
    }

    fn release(&self) {
        loop {
            let next = {
                let mut state = self.state.lock().expect("semaphore state");
                state.waiters.pop_front()
            };
            match next {
                Some(waiter) => {
                    if waiter.sender.send(()).is_ok() {
                        return;
                    }
                    // The waiter is gone; keep the permit in circulation.
                    continue;
                }
                None => {
                    let mut state = self.state.lock().expect("semaphore state");
                    state.available += 1;
                    return;
                }
            }
        }
    }

    /// Run `f` while holding a permit, releasing it even if `f` panics or fails.
    /// Rejects without running if `signal` aborts while queued.
    pub async fn run<F, Fut, T>(&self, f: F, signal: Option<&CancellationToken>) -> Result<T, SemaphoreError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        self.acquire(signal).await?;
        let guard = PermitGuard { semaphore: self };
        let result = f().await;
        drop(guard);
        Ok(result)
    }
}

struct PermitGuard<'a> {
    semaphore: &'a Semaphore,
}

impl Drop for PermitGuard<'_> {
    fn drop(&mut self) {
        self.semaphore.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[tokio::test]
    async fn rejects_non_positive_permit_counts() {
        assert!(Semaphore::new(0).is_err());
        assert!(Semaphore::new(-1).is_err());
        assert_eq!(
            Semaphore::new(0).unwrap_err().to_string(),
            "Semaphore permits must be a positive integer, got 0"
        );
        assert!(Semaphore::new(1).is_ok());
    }

    #[tokio::test]
    async fn never_exceeds_the_permit_count_under_heavy_fan_out() {
        let limit = 3usize;
        let total = 50usize;
        let semaphore = std::sync::Arc::new(Semaphore::new(limit as i64).unwrap());
        let active = std::sync::Arc::new(AtomicUsize::new(0));
        let peak = std::sync::Arc::new(AtomicUsize::new(0));
        let completed = std::sync::Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..total {
            let semaphore = semaphore.clone();
            let active = active.clone();
            let peak = peak.clone();
            let completed = completed.clone();
            handles.push(tokio::spawn(async move {
                semaphore
                    .run(
                        || async move {
                            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(now, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(1)).await;
                            active.fetch_sub(1, Ordering::SeqCst);
                            completed.fetch_add(1, Ordering::SeqCst);
                        },
                        None,
                    )
                    .await
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        assert!(peak.load(Ordering::SeqCst) <= limit);
        assert_eq!(completed.load(Ordering::SeqCst), total);
    }

    #[tokio::test]
    async fn releases_the_permit_when_the_task_panics() {
        let semaphore = Semaphore::new(1).unwrap();
        let result = tokio::spawn(async move {
            let _ = semaphore
                .run(|| async { panic!("boom") }, None)
                .await;
        })
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn hands_permits_to_waiters_in_fifo_order() {
        let semaphore = std::sync::Arc::new(Semaphore::new(1).unwrap());
        let order = std::sync::Arc::new(Mutex::new(Vec::<usize>::new()));
        let (gate_sender, gate_receiver) = oneshot::channel::<()>();

        let holder = {
            let semaphore = semaphore.clone();
            tokio::spawn(async move {
                semaphore
                    .run(
                        || async move {
                            let _ = gate_receiver.await;
                        },
                        None,
                    )
                    .await
                    .unwrap();
            })
        };

        let mut waiters = Vec::new();
        for index in 0..3usize {
            let semaphore = semaphore.clone();
            let order = order.clone();
            waiters.push(tokio::spawn(async move {
                semaphore
                    .run(
                        || async move {
                            order.lock().unwrap().push(index);
                        },
                        None,
                    )
                    .await
                    .unwrap();
            }));
        }

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(semaphore.queue_length(), 3);
        let _ = gate_sender.send(());
        holder.await.unwrap();
        for waiter in waiters {
            waiter.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn a_queued_waiter_aborts_without_consuming_a_permit() {
        let semaphore = std::sync::Arc::new(Semaphore::new(1).unwrap());
        let (gate_sender, gate_receiver) = oneshot::channel::<()>();

        let holder = {
            let semaphore = semaphore.clone();
            tokio::spawn(async move {
                semaphore
                    .run(
                        || async move {
                            let _ = gate_receiver.await;
                        },
                        None,
                    )
                    .await
                    .unwrap();
            })
        };

        let token = CancellationToken::new();
        let queued = {
            let semaphore = semaphore.clone();
            let token = token.clone();
            tokio::spawn(async move { semaphore.run(|| async { "ran" }, Some(&token)).await })
        };

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(semaphore.queue_length(), 1);
        token.cancel();
        assert!(queued.await.unwrap().is_err());
        assert_eq!(semaphore.queue_length(), 0);

        let _ = gate_sender.send(());
        holder.await.unwrap();
        assert_eq!(semaphore.run(|| async { "ok" }, None).await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn rejects_immediately_when_the_signal_is_already_aborted() {
        let semaphore = Semaphore::new(1).unwrap();
        let token = CancellationToken::new();
        token.cancel();
        assert!(semaphore.run(|| async { "ran" }, Some(&token)).await.is_err());
        assert_eq!(semaphore.run(|| async { "ok" }, None).await.unwrap(), "ok");
    }
}
