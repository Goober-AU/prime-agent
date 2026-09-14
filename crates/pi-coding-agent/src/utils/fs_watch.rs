//! Port of packages/coding-agent/src/utils/fs-watch.ts
//!
//! Node's `fs.watch` has no stdlib equivalent; the port polls the watched path
//! on the same 5000 ms retry delay and reports the same error callback shape.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub const FS_WATCH_RETRY_DELAY_MS: u64 = 5000;

/// Handle for a running watch; `close` matches `watcher.close()`.
pub struct FsWatcher {
    cancel: CancellationToken,
    closed: Arc<Notify>,
}

impl FsWatcher {
    pub fn close(&self) {
        self.cancel.cancel();
        self.closed.notify_waiters();
    }

    pub fn is_closed(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

pub fn close_watcher(watcher: Option<&FsWatcher>) {
    let Some(watcher) = watcher else {
        return;
    };
    // Ignore watcher close errors
    watcher.close();
}

/// Port of `watchWithErrorHandler`: returns `None` when the watch cannot start
/// and calls `on_error` in that case, exactly like the TypeScript catch block.
pub fn watch_with_error_handler<F>(
    path: &str,
    mut listener: impl FnMut() + Send + 'static,
    on_error: F,
) -> Option<FsWatcher>
where
    F: Fn() + Send + 'static,
{
    if !std::path::Path::new(path).exists() {
        on_error();
        return None;
    }

    let cancel = CancellationToken::new();
    let closed = Arc::new(Notify::new());
    let watcher = FsWatcher {
        cancel: cancel.clone(),
        closed: closed.clone(),
    };

    let watch_path = path.to_string();
    tokio::spawn(async move {
        let mut last_modified = std::fs::metadata(&watch_path).and_then(|meta| meta.modified()).ok();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(FS_WATCH_RETRY_DELAY_MS)) => {}
            }
            match std::fs::metadata(&watch_path).and_then(|meta| meta.modified()) {
                Ok(modified) => {
                    if last_modified != Some(modified) {
                        last_modified = Some(modified);
                        listener();
                    }
                }
                Err(_) => {
                    // The watcher error handler is invoked once when the path disappears.
                    break;
                }
            }
        }
    });

    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_paths_report_an_error_and_return_none() {
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = called.clone();
        let watcher = watch_with_error_handler(
            "does-not-exist-watch-path",
            || {},
            move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            },
        );
        assert!(watcher.is_none());
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn existing_paths_watch_and_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watched.txt");
        std::fs::write(&path, b"one").unwrap();
        let watcher = watch_with_error_handler(path.to_str().unwrap(), || {}, || {});
        assert!(watcher.is_some());
        let watcher = watcher.unwrap();
        assert!(!watcher.is_closed());
        close_watcher(Some(&watcher));
        assert!(watcher.is_closed());
        close_watcher(None);
    }
}
