//! Serialized Promise continuations without recursively polling predecessor work.

use super::SharedVoidFuture;
use futures::FutureExt;
use std::future::Future;
use std::sync::Mutex;

pub(super) fn enqueue<F>(tail: &Mutex<SharedVoidFuture>, task: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let (completed, completion) = tokio::sync::oneshot::channel::<()>();
    let next: SharedVoidFuture = async move {
        completion
            .await
            .map_err(|_| "Serialized session task ended before completion".to_string())
    }
    .boxed()
    .shared();
    let previous = {
        let mut tail = tail.lock().unwrap();
        std::mem::replace(&mut *tail, next)
    };

    // The shared tail contains only a completion receiver, never `task` or its
    // predecessor. A waiter cannot recursively poll or drop the entire backlog.
    // A panic/cancel drops the sender, allowing the next task to run just like
    // TypeScript's .then(task, task) after a rejected predecessor.
    tokio::spawn(
        async move {
            let _ = previous.await;
            task.await;
            let _ = completed.send(());
        }
        .boxed(),
    );
}
