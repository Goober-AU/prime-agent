//! Port of packages/coding-agent/src/core/tools/file-mutation-queue.ts

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::oneshot;

type QueueEntry = Option<oneshot::Sender<()>>;

fn file_mutation_queues() -> &'static Mutex<HashMap<String, Arc<Mutex<QueueEntry>>>> {
    static QUEUES: OnceLock<Mutex<HashMap<String, Arc<Mutex<QueueEntry>>>>> = OnceLock::new();
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

    // The chained queue node: this task waits for the previous holder, then
    // publishes its own "release" sender so the next waiter can attach.
    let entry: Arc<Mutex<QueueEntry>> = Arc::new(Mutex::new(None));
    let current_queue = {
        let mut queues = file_mutation_queues().lock().expect("mutation queue lock");
        let previous = queues.get(&key).cloned();
        queues.insert(key.clone(), entry.clone());
        previous
    };

    let (release_tx, release_rx) = oneshot::channel::<()>();
    *entry.lock().expect("queue entry lock") = Some(release_tx);

    // `await currentQueue` - wait until the previously chained queue settles.
    if let Some(previous) = current_queue {
        let previous_release = previous.lock().expect("queue entry lock").take();
        if let Some(previous_release) = previous_release {
            let _ = previous_release.send(());
        }
    }

    let result = run().await;

    // finally: releaseNext(); drop the key when it still points at this node.
    let mut queues = file_mutation_queues().lock().expect("mutation queue lock");
    let still_current = queues
        .get(&key)
        .map(|current| Arc::ptr_eq(current, &entry))
        .unwrap_or(false);
    if still_current {
        queues.remove(&key);
    }
    drop(queues);

    drop(release_rx);
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
        assert!(!file_mutation_queues().lock().expect("lock").contains_key(&key));
        assert_eq!(AtomicUsize::new(0).load(Ordering::SeqCst), 0);
    }
}
