//! Port of packages/coding-agent/src/core/kernel/boot-gate.ts.
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Notify;

use super::shared::{AbortSignal, KernelError};

// Above core count because boots are IO-bound (cold imports), but capped so a
// fan-out can't thrash the FS past the ready-handshake window.
const MAX_KERNEL_BOOT_CONCURRENCY: usize = 64;

fn default_kernel_boot_concurrency() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4);
    let cpus = if cpus == 0 { 4 } else { cpus };
    std::cmp::min(16, std::cmp::max(4, cpus * 2))
}

pub fn resolve_kernel_boot_concurrency() -> usize {
    let Ok(raw) = std::env::var("PRIME_AGENT_MAX_CONCURRENT_KERNEL_BOOTS") else {
        return default_kernel_boot_concurrency();
    };
    // `/^\d+$/`: ASCII digits only, and an empty string is not a match.
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
        return default_kernel_boot_concurrency();
    }
    // TS `Number.parseInt` parses into a double: a digit-only string beyond
    // i64 still parses (as a float) and clamps to the cap instead of falling
    // back to the default.
    let Ok(parsed) = raw.parse::<f64>() else {
        return default_kernel_boot_concurrency();
    };
    // A malformed or out-of-range value (incl. 0, e.g. "00") falls back to the
    // default rather than mis-bounding the gate or throwing at module load.
    if parsed < 1.0 {
        return default_kernel_boot_concurrency();
    }
    // An explicit override may exceed the auto default (that's its purpose), but is
    // still clamped by the FS-contention cap.
    std::cmp::min(MAX_KERNEL_BOOT_CONCURRENCY, parsed as usize)
}

/// Local stand-in for utils/semaphore.ts (`TODO(slice)`: needs
/// `pi-coding-agent::utils::semaphore::Semaphore`). Counting semaphore for
/// bounding async concurrency. FIFO: waiters acquire in the order they queued,
/// and an aborted waiter removes itself so it never consumes a slot.
pub struct KernelBootSemaphore {
    state: Mutex<SemaphoreState>,
}

struct SemaphoreState {
    available: usize,
    waiters: Vec<Arc<Waiter>>,
}

struct Waiter {
    outcome: Mutex<Option<Result<(), KernelError>>>,
    notify: Notify,
}

impl Waiter {
    fn outcome(&self) -> Option<Result<(), KernelError>> {
        self.outcome.lock().unwrap().clone()
    }

    fn settle(&self, outcome: Result<(), KernelError>) {
        let mut slot = self.outcome.lock().unwrap();
        if slot.is_none() {
            *slot = Some(outcome);
        }
        drop(slot);
        self.notify.notify_waiters();
    }
}

impl KernelBootSemaphore {
    pub fn new(permits: usize) -> Self {
        if permits < 1 {
            panic!("Semaphore permits must be a positive integer, got {permits}");
        }
        KernelBootSemaphore {
            state: Mutex::new(SemaphoreState {
                available: permits,
                waiters: Vec::new(),
            }),
        }
    }

    pub fn queue_length(&self) -> usize {
        self.state.lock().unwrap().waiters.len()
    }

    async fn acquire(&self, signal: Option<&AbortSignal>) -> Result<(), KernelError> {
        if let Some(signal) = signal {
            if signal.is_aborted() {
                return Err(signal.reason().unwrap_or_else(|| KernelError::new("aborted")));
            }
        }
        {
            let mut state = self.state.lock().unwrap();
            if state.available > 0 {
                state.available -= 1;
                return Ok(());
            }
        }

        // A queued waiter can be aborted before it gets a permit; on abort it
        // removes itself from the queue so it never consumes a slot.
        let waiter = Arc::new(Waiter {
            outcome: Mutex::new(None),
            notify: Notify::new(),
        });
        self.state.lock().unwrap().waiters.push(waiter.clone());

        loop {
            if let Some(outcome) = waiter.outcome() {
                return outcome;
            }
            let notified = waiter.notify.notified();
            if let Some(outcome) = waiter.outcome() {
                return outcome;
            }
            match signal {
                Some(signal) => {
                    tokio::select! {
                        biased;
                        _ = notified => {}
                        _ = signal.wait() => {
                            // A grant that already consumed a slot must be honored,
                            // otherwise the permit would leak.
                            if let Some(outcome) = waiter.outcome() {
                                return outcome;
                            }
                            self.remove_waiter(&waiter);
                            return Err(signal.reason().unwrap_or_else(|| KernelError::new("aborted")));
                        }
                    }
                }
                None => notified.await,
            }
        }
    }

    fn remove_waiter(&self, waiter: &Arc<Waiter>) {
        let mut state = self.state.lock().unwrap();
        if let Some(index) = state
            .waiters
            .iter()
            .position(|existing| Arc::ptr_eq(existing, waiter))
        {
            state.waiters.remove(index);
        }
    }

    fn release(&self) {
        let waiter = {
            let mut state = self.state.lock().unwrap();
            if state.waiters.is_empty() {
                state.available += 1;
                return;
            }
            state.waiters.remove(0)
        };
        waiter.settle(Ok(()));
    }

    /// Run `function` while holding a permit, releasing it even if `function`
    /// fails. Rejects without running if `signal` aborts while queued.
    pub async fn run<F, Fut, T>(&self, function: F, signal: Option<AbortSignal>) -> Result<T, KernelError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, KernelError>>,
    {
        self.acquire(signal.as_ref()).await?;
        let result = function().await;
        self.release();
        result
    }
}

// Resolved lazily on first boot so an env override set before the first kernel
// starts is honored - not just one set at import time.
static KERNEL_BOOT_SEMAPHORE: OnceLock<Arc<KernelBootSemaphore>> = OnceLock::new();

pub fn kernel_boot_semaphore() -> Arc<KernelBootSemaphore> {
    KERNEL_BOOT_SEMAPHORE
        .get_or_init(|| Arc::new(KernelBootSemaphore::new(resolve_kernel_boot_concurrency())))
        .clone()
}

pub async fn with_kernel_boot_permit<F, Fut, T>(boot: F, signal: Option<AbortSignal>) -> Result<T, KernelError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, KernelError>>,
{
    kernel_boot_semaphore().run(boot, signal).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrency_default_is_between_four_and_sixteen() {
        let value = default_kernel_boot_concurrency();
        assert!((4..=16).contains(&value));
    }

    #[test]
    fn env_override_is_parsed_or_falls_back() {
        let key = "PRIME_AGENT_MAX_CONCURRENT_KERNEL_BOOTS";
        let previous = std::env::var(key).ok();
        std::env::remove_var(key);
        assert_eq!(resolve_kernel_boot_concurrency(), default_kernel_boot_concurrency());

        std::env::set_var(key, "7");
        assert_eq!(resolve_kernel_boot_concurrency(), 7);
        std::env::set_var(key, "00");
        assert_eq!(resolve_kernel_boot_concurrency(), default_kernel_boot_concurrency());
        std::env::set_var(key, "999");
        assert_eq!(resolve_kernel_boot_concurrency(), MAX_KERNEL_BOOT_CONCURRENCY);
        std::env::set_var(key, "1x");
        assert_eq!(resolve_kernel_boot_concurrency(), default_kernel_boot_concurrency());

        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn permits_must_be_positive() {
        assert_eq!(KernelBootSemaphore::new(1).queue_length(), 0);
        let panicked = std::panic::catch_unwind(|| KernelBootSemaphore::new(0));
        assert!(panicked.is_err());
    }

    #[tokio::test]
    async fn run_releases_the_permit_after_an_error() {
        let semaphore = KernelBootSemaphore::new(1);
        let first = semaphore
            .run(|| async { Err::<(), KernelError>(KernelError::new("boom")) }, None)
            .await;
        assert!(first.is_err());
        let second = semaphore.run(|| async { Ok::<_, KernelError>(1) }, None).await;
        assert_eq!(second.unwrap(), 1);
    }

    #[tokio::test]
    async fn queued_waiters_acquire_in_queue_order() {
        let semaphore = Arc::new(KernelBootSemaphore::new(1));
        let order = Arc::new(Mutex::new(Vec::<usize>::new()));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

        let holder = {
            let semaphore = semaphore.clone();
            tokio::spawn(async move {
                semaphore
                    .run(
                        || async move {
                            let _ = release_rx.await;
                            Ok::<_, KernelError>(())
                        },
                        None,
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;

        let mut waiters = Vec::new();
        for index in 0..2usize {
            let semaphore = semaphore.clone();
            let order = order.clone();
            waiters.push(tokio::spawn(async move {
                semaphore
                    .run(
                        || async move {
                            order.lock().unwrap().push(index);
                            Ok::<_, KernelError>(())
                        },
                        None,
                    )
                    .await
            }));
            tokio::task::yield_now().await;
        }
        assert_eq!(semaphore.queue_length(), 2);

        release_tx.send(()).unwrap();
        holder.await.unwrap().unwrap();
        for waiter in waiters {
            waiter.await.unwrap().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![0, 1]);
    }

    #[tokio::test]
    async fn aborted_waiter_leaves_the_queue_without_a_permit() {
        let semaphore = Arc::new(KernelBootSemaphore::new(1));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let holder = {
            let semaphore = semaphore.clone();
            tokio::spawn(async move {
                semaphore
                    .run(
                        || async move {
                            let _ = release_rx.await;
                            Ok::<_, KernelError>(())
                        },
                        None,
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;

        let signal = AbortSignal::new();
        let queued = {
            let semaphore = semaphore.clone();
            let signal = signal.clone();
            tokio::spawn(async move {
                semaphore
                    .run(|| async { Ok::<_, KernelError>(()) }, Some(signal))
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert_eq!(semaphore.queue_length(), 1);

        signal.abort(Some(KernelError::new("cancelled")));
        let result = queued.await.unwrap();
        assert_eq!(result, Err(KernelError::new("cancelled")));
        assert_eq!(semaphore.queue_length(), 0);

        release_tx.send(()).unwrap();
        holder.await.unwrap().unwrap();
    }

    /// G-13: a digit-only override beyond i64 must clamp to the 64 cap like
    /// the TypeScript `Number.parseInt` (double) path, not fall back to the
    /// default.
    #[test]
    fn extreme_concurrency_override_clamps_like_typescript() {
        let key = "PRIME_AGENT_MAX_CONCURRENT_KERNEL_BOOTS";
        let previous = std::env::var(key).ok();
        std::env::set_var(key, "99999999999999999999");
        assert_eq!(resolve_kernel_boot_concurrency(), MAX_KERNEL_BOOT_CONCURRENCY);
        std::env::set_var(key, "18446744073709551616");
        assert_eq!(resolve_kernel_boot_concurrency(), MAX_KERNEL_BOOT_CONCURRENCY);
        // Negative controls keep the documented fallback semantics.
        std::env::set_var(key, "abc");
        assert_eq!(resolve_kernel_boot_concurrency(), default_kernel_boot_concurrency());
        std::env::set_var(key, "0");
        assert_eq!(resolve_kernel_boot_concurrency(), default_kernel_boot_concurrency());
        std::env::set_var(key, "64");
        assert_eq!(resolve_kernel_boot_concurrency(), 64);
        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}
