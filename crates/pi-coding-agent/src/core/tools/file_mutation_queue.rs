//! Port of packages/coding-agent/src/core/tools/file-mutation-queue.ts

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::watch;

/// One link of the per-file promise chain of file-mutation-queue.ts:21-28.
///
/// TS stores `chainedQueue = currentQueue.then(() => nextQueue)` (line 27) in the map, so a
/// caller's `await currentQueue` (line 30) settles only after the previous holder's `fn()`
/// returned and its `finally` ran `releaseNext()` (line 34). `released` carries exactly that
/// "previous mutation COMPLETED" fact, and `previous` keeps the link this node was chained
/// onto so a cancelled waiter still hands its successor the whole chain.
struct QueueNode {
    /// `nextQueue` (file-mutation-queue.ts:24-26): pending until the holder releases.
    released: watch::Sender<bool>,
    /// `currentQueue` (file-mutation-queue.ts:21) this node was chained onto.
    previous: Option<Arc<QueueNode>>,
}

impl QueueNode {
    fn new(previous: Option<Arc<QueueNode>>) -> Self {
        Self {
            released: watch::channel(false).0,
            previous,
        }
    }

    /// `releaseNext()` (file-mutation-queue.ts:34).
    fn release(&self) {
        // `send_replace` stores the value even when no receiver exists yet; plain `send` fails
        // there and would leave later `subscribe()` calls observing `false` forever.
        let _ = self.released.send_replace(true);
    }

    fn is_released(&self) -> bool {
        *self.released.borrow()
    }

    /// The link of this chain that is still holding the file: the newest not-yet-released link
    /// behind `self`. TS can never lose that link - a caller waiting at line 30 always resumes and
    /// runs its `fn()` - so its map always describes the tail of the live chain (lines 27-28). A
    /// Rust waiter can be dropped while it waits, so it hands this live tail on instead of leaving
    /// the next caller with no chain to wait for.
    fn live_tail(&self) -> Option<Arc<QueueNode>> {
        let mut cursor = self.previous.as_ref();
        while let Some(current) = cursor {
            if !current.is_released() {
                return Some(current.clone());
            }
            cursor = current.previous.as_ref();
        }
        None
    }
}

/// Awaits `chainedQueue` (file-mutation-queue.ts:27) of `node`: the entire chain `node` belongs
/// to has settled, i.e. every link up to and including `node` has released. Links that already
/// released resolve immediately.
async fn await_chained_settled(node: &Arc<QueueNode>) {
    let mut chain: Vec<&Arc<QueueNode>> = Vec::new();
    let mut cursor = Some(node);
    while let Some(current) = cursor {
        chain.push(current);
        cursor = current.previous.as_ref();
    }
    // `currentQueue.then(() => nextQueue)`: the oldest link settles first.
    for link in chain.iter().rev() {
        let mut released = link.released.subscribe();
        let _ = released.wait_for(|released| *released).await;
    }
}

/// `finally { releaseNext(); if (fileMutationQueues.get(key) === chainedQueue) delete }`
/// (file-mutation-queue.ts:33-38). A drop guard is used because a Rust future can be dropped
/// (cancelled) in the middle of `run().await`, where TS still reaches its `finally`; without it
/// a cancelled mutation would leave every later same-file caller waiting forever.
struct QueueReleaseGuard {
    key: String,
    node: Arc<QueueNode>,
}

impl Drop for QueueReleaseGuard {
    fn drop(&mut self) {
        self.node.release();
        let live_tail = self.node.live_tail();
        let mut queues = file_mutation_queues().lock().expect("mutation queue lock");
        let still_current = queues
            .get(&self.key)
            .map(|current| Arc::ptr_eq(current, &self.node))
            .unwrap_or(false);
        if still_current {
            match live_tail {
                // An earlier link is still running (this mutation was cancelled while it waited), so
                // keep that chain in the map like TS does; deleting it would let the next caller
                // start its read-modify-write while the file is still held.
                Some(live_tail) => {
                    queues.insert(self.key.clone(), live_tail);
                }
                None => {
                    queues.remove(&self.key);
                }
            }
        }
    }
}

fn file_mutation_queues() -> &'static Mutex<HashMap<String, Arc<QueueNode>>> {
    static QUEUES: OnceLock<Mutex<HashMap<String, Arc<QueueNode>>>> = OnceLock::new();
    QUEUES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn get_mutation_queue_key(file_path: &str) -> String {
    let resolved_path = PathBuf::from(file_path);
    let absolute = if resolved_path.is_absolute() {
        resolved_path
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(resolved_path),
            Err(_) => resolved_path,
        }
    };
    match std::fs::canonicalize(&absolute) {
        Ok(canonical) => canonical.to_string_lossy().into_owned(),
        Err(_) => absolute.to_string_lossy().into_owned(),
    }
}

/// Serialize file mutation operations targeting the same file.
/// Operations for different files still run in parallel.
pub async fn with_file_mutation_queue<T, F, Fut>(file_path: &str, run: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let key = get_mutation_queue_key(file_path);

    // `currentQueue = fileMutationQueues.get(key)` then `fileMutationQueues.set(key,
    // chainedQueue)` (file-mutation-queue.ts:21,27-28): this node becomes the chain the next
    // caller attaches to, and it is published before we start waiting.
    //
    // The guard is built inside the same critical section, before the node is published, so a
    // published node always has a releaser: no same-file caller can be stranded by a panic in
    // this window.
    let (guard, current_queue) = {
        let mut queues = file_mutation_queues().lock().expect("mutation queue lock");
        let current_queue = queues.get(&key).cloned();
        let node = Arc::new(QueueNode::new(current_queue.clone()));
        let guard = QueueReleaseGuard {
            key,
            node: node.clone(),
        };
        queues.insert(guard.key.clone(), node);
        (guard, current_queue)
    };

    // `await currentQueue` (file-mutation-queue.ts:30): the previous mutation has fully settled -
    // including its own `fn()` - before ours runs. Same-file read-modify-write operations must
    // never overlap, or one edit is lost.
    if let Some(current_queue) = current_queue {
        await_chained_settled(&current_queue).await;
    }

    let result = run().await;

    // finally (file-mutation-queue.ts:33-37): `releaseNext()`, then drop the key only while it
    // still points at this node.
    drop(guard);
    result
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test]
    async fn serializes_operations_for_the_same_file() {
        let order = Arc::new(Mutex::new(Vec::<usize>::new()));
        let mut handles = Vec::new();
        for index in 0..4 {
            let order = order.clone();
            handles.push(tokio::spawn(async move {
                with_file_mutation_queue("queue-target.txt", || async move {
                    order.lock().expect("order lock").push(index);
                    tokio::task::yield_now().await;
                })
                .await;
            }));
        }
        for handle in handles {
            handle.await.expect("join");
        }
        assert_eq!(order.lock().expect("order lock").len(), 4);
    }

    /// Regression test for file-mutation-queue.ts:30 plus the line-34 `finally` release: the
    /// second caller must wait until the first caller's `fn()` has COMPLETED, not merely until
    /// the previous holder was scheduled. Two parallel `edit` calls on one file otherwise
    /// interleave their read-modify-write and one edit is silently lost.
    #[tokio::test]
    async fn overlapping_mutations_on_the_same_file_do_not_interleave() {
        let path = "queue-overlap-target.txt";
        // `(mutation index, entered)` - `entered == false` marks the end of that mutation.
        let events = Arc::new(Mutex::new(Vec::<(usize, bool)>::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for index in 0..2usize {
            let events = events.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            handles.push(tokio::spawn(async move {
                with_file_mutation_queue(path, || async move {
                    {
                        let mut events = events.lock().expect("events lock");
                        events.push((index, true));
                        let running = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(running, Ordering::SeqCst);
                    }
                    // Inside the critical section: a sibling that reaches its own closure now
                    // would be interleaving with this file mutation.
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;

                    active.fetch_sub(1, Ordering::SeqCst);
                    events.lock().expect("events lock").push((index, false));
                })
                .await;
            }));
        }
        for handle in handles {
            handle.await.expect("join");
        }

        let events = events.lock().expect("events lock").clone();
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "two same-file mutations ran at the same time: {events:?}"
        );
        assert_eq!(
            events.len(),
            4,
            "expected enter/exit for both mutations: {events:?}"
        );

        // Strict nesting: every mutation must run to completion before the next one starts.
        let mut open: Option<usize> = None;
        for (index, entered) in events {
            if entered {
                assert!(
                    open.is_none(),
                    "mutation {index} started while mutation {open:?} was still running"
                );
                open = Some(index);
            } else {
                assert_eq!(open, Some(index), "mutation {index} finished out of order");
                open = None;
            }
        }
        assert!(open.is_none(), "a mutation never released its successor");
    }

    /// The release signal must be latched: a holder that already finished can still be awaited by a
    /// successor that read the node from the map first (file-mutation-queue.ts:21 then line 30), so
    /// an unlatched signal would hang that successor forever.
    #[tokio::test]
    async fn an_already_released_holder_does_not_block_its_successor() {
        let path = "queue-latched-release.txt";
        let node = Arc::new(QueueNode::new(None));
        node.release();
        file_mutation_queues()
            .lock()
            .expect("lock")
            .insert(get_mutation_queue_key(path), node);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            with_file_mutation_queue(path, || async { 5usize }),
        )
        .await;
        assert_eq!(
            result.expect("a successor must not wait on an already released holder"),
            5
        );
    }

    /// A cancelled mutation must still release its successor. file-mutation-queue.ts:33-38 reaches
    /// `finally { releaseNext() }` even when `fn()` rejects, and a dropped Rust future has to behave
    /// the same way, or every later mutation on that file waits forever.
    #[tokio::test]
    async fn a_cancelled_mutation_still_releases_the_next_one() {
        let path = "queue-cancel-target.txt";
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            // Holds the file and never finishes; the enclosing timeout cancels this mutation.
            with_file_mutation_queue(path, || async { std::future::pending::<()>().await }),
        )
        .await;
        assert!(cancelled.is_err(), "the first mutation should be cancelled");

        let next = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            with_file_mutation_queue(path, || async { 9usize }),
        )
        .await;
        assert_eq!(
            next.expect("a cancelled mutation must not strand later mutations on the same file"),
            9
        );
    }

    /// A mutation cancelled while it WAITS must not let its successor slip past the still-running
    /// holder. TS reaches `finally` (file-mutation-queue.ts:33-38) for such a waiter and the holder
    /// is still inside `fn()`, so the successor's `await currentQueue` (line 30) still has to cover
    /// the whole earlier chain.
    #[tokio::test]
    async fn a_waiter_cancelled_while_waiting_does_not_unblock_its_successor() {
        let path = "queue-wait-cancel-target.txt";
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let middle_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let holder_active = active.clone();
        let holder_max = max_active.clone();
        let holder = tokio::spawn(async move {
            with_file_mutation_queue(path, || async move {
                let running = holder_active.fetch_add(1, Ordering::SeqCst) + 1;
                holder_max.fetch_max(running, Ordering::SeqCst);
                let _ = gate_rx.await;
                holder_active.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        });

        let middle_ran_flag = middle_ran.clone();
        let middle = tokio::spawn(async move {
            with_file_mutation_queue(path, || async move {
                middle_ran_flag.store(true, Ordering::SeqCst);
            })
            .await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        middle.abort();

        let next_active = active.clone();
        let next_max = max_active.clone();
        let next = tokio::spawn(async move {
            with_file_mutation_queue(path, || async move {
                let running = next_active.fetch_add(1, Ordering::SeqCst) + 1;
                next_max.fetch_max(running, Ordering::SeqCst);
                next_active.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = gate_tx.send(());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), holder).await;
        let next_result = tokio::time::timeout(std::time::Duration::from_secs(5), next).await;

        assert!(
            !middle_ran.load(Ordering::SeqCst),
            "the cancelled waiter must not run"
        );
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "a successor overlapped the holder after a cancelled waiter released early"
        );
        assert!(
            next_result.is_ok(),
            "the successor must still run after the holder finishes"
        );
    }

    #[tokio::test]
    async fn returns_the_closure_value() {
        let value = with_file_mutation_queue("queue-value.txt", || async { 7usize }).await;
        assert_eq!(value, 7);
    }

    #[tokio::test]
    async fn drops_the_key_after_the_last_operation() {
        let path = "queue-cleanup.txt";
        with_file_mutation_queue(path, || async {}).await;
        let key = get_mutation_queue_key(path);
        assert!(!file_mutation_queues()
            .lock()
            .expect("lock")
            .contains_key(&key));
        assert_eq!(AtomicUsize::new(0).load(Ordering::SeqCst), 0);
    }
}
