//! Port of packages/coding-agent/src/core/footer-data-provider.ts

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::utils::child_process::{exec_file_hidden, spawn_sync_hidden, SpawnOptions};
use crate::utils::fs_watch::{
    close_watcher, watch_with_error_handler, FsWatcher, FS_WATCH_RETRY_DELAY_MS,
};
use crate::utils::git::{find_git_paths, GitPaths};

/// Ask git for the current branch. Returns null on detached HEAD or if git is unavailable.
fn resolve_branch_with_git_sync(repo_dir: &str) -> Option<String> {
    let mut options = SpawnOptions {
        cwd: Some(repo_dir.to_string()),
        capture_stdout: true,
        ..Default::default()
    };
    options.capture_stderr = false;
    let result = spawn_sync_hidden(
        "git",
        &[
            "--no-optional-locks".to_string(),
            "symbolic-ref".to_string(),
            "--quiet".to_string(),
            "--short".to_string(),
            "HEAD".to_string(),
        ],
        options,
    );
    let Ok(result) = result else {
        return None;
    };
    let branch = if result.status.success() {
        String::from_utf8_lossy(&result.stdout).trim().to_string()
    } else {
        String::new()
    };
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

/// Ask git for the current branch asynchronously. Returns null on detached HEAD or if git is unavailable.
async fn resolve_branch_with_git_async(repo_dir: &str) -> Option<String> {
    let repo_dir = repo_dir.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let mut options = SpawnOptions {
            cwd: Some(repo_dir),
            capture_stdout: true,
            ..Default::default()
        };
        options.capture_stderr = false;
        exec_file_hidden(
            "git",
            &[
                "--no-optional-locks".to_string(),
                "symbolic-ref".to_string(),
                "--quiet".to_string(),
                "--short".to_string(),
                "HEAD".to_string(),
            ],
            options,
        )
    })
    .await;
    let Ok(Ok(output)) = result else {
        return None;
    };
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

/// Provides git branch and extension statuses - data not otherwise accessible to extensions.
/// Token stats, model info available via ctx.sessionManager and ctx.model.
pub struct FooterDataProvider {
    cwd: Mutex<String>,
    extension_statuses: Mutex<BTreeMap<String, String>>,
    /// `undefined` = not resolved yet, `Some(None)` = resolved to null.
    cached_branch: Mutex<Option<Option<String>>>,
    git_paths: Mutex<Option<Option<GitPaths>>>,
    head_watcher: Mutex<Option<FsWatcher>>,
    reftable_watcher: Mutex<Option<FsWatcher>>,
    reftable_tables_list_watcher: Mutex<Option<FsWatcher>>,
    reftable_tables_list_path: Mutex<Option<String>>,
    branch_change_callbacks: Mutex<BTreeMap<usize, Arc<dyn Fn() + Send + Sync>>>,
    next_branch_change_callback_id: AtomicUsize,
    available_provider_count: AtomicUsize,
    refresh_timer: Mutex<Option<tokio::task::JoinHandle<()>>>,
    git_watcher_retry_timer: Mutex<Option<tokio::task::JoinHandle<()>>>,
    refresh_in_flight: AtomicBool,
    refresh_pending: AtomicBool,
    disposed: AtomicBool,
    /// Set once so `schedule_refresh` can start the debounce timer.
    runtime: Mutex<Option<tokio::runtime::Handle>>,
    /// Weak self handle used by the debounce and retry timers.
    self_handle: Mutex<Option<Arc<FooterDataProvider>>>,
}

pub const WATCH_DEBOUNCE_MS: u64 = 500;

impl FooterDataProvider {
    pub fn new(cwd: &str) -> Self {
        let provider = FooterDataProvider {
            cwd: Mutex::new(cwd.to_string()),
            extension_statuses: Mutex::new(BTreeMap::new()),
            cached_branch: Mutex::new(None),
            git_paths: Mutex::new(None),
            head_watcher: Mutex::new(None),
            reftable_watcher: Mutex::new(None),
            reftable_tables_list_watcher: Mutex::new(None),
            reftable_tables_list_path: Mutex::new(None),
            branch_change_callbacks: Mutex::new(BTreeMap::new()),
            next_branch_change_callback_id: AtomicUsize::new(1),
            available_provider_count: AtomicUsize::new(0),
            refresh_timer: Mutex::new(None),
            git_watcher_retry_timer: Mutex::new(None),
            refresh_in_flight: AtomicBool::new(false),
            refresh_pending: AtomicBool::new(false),
            disposed: AtomicBool::new(false),
            runtime: Mutex::new(tokio::runtime::Handle::try_current().ok()),
            self_handle: Mutex::new(None),
        };
        {
            let mut git_paths = provider.git_paths.lock().unwrap();
            *git_paths = Some(find_git_paths(cwd));
        }
        provider.setup_git_watcher();
        provider
    }

    /// Current git branch, null if not in repo, "detached" if detached HEAD
    pub fn get_git_branch(&self) -> Option<String> {
        let mut cached = self.cached_branch.lock().unwrap();
        if cached.is_none() {
            *cached = Some(self.resolve_git_branch_sync());
        }
        cached.clone().flatten()
    }

    /// Extension status texts set via ctx.ui.setStatus()
    pub fn get_extension_statuses(&self) -> BTreeMap<String, String> {
        self.extension_statuses.lock().unwrap().clone()
    }

    /// Subscribe to git branch changes. Returns unsubscribe function.
    pub fn on_branch_change(&self, callback: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        let id = self
            .next_branch_change_callback_id
            .fetch_add(1, Ordering::SeqCst);
        self.branch_change_callbacks
            .lock()
            .unwrap()
            .insert(id, callback);
        let callbacks = self.branch_change_callbacks.clone();
        Unsubscribe(Arc::new(move || {
            callbacks.lock().unwrap().remove(&id);
        }))
    }

    /// Internal: set extension status
    pub fn set_extension_status(&self, key: &str, text: Option<&str>) {
        let mut statuses = self.extension_statuses.lock().unwrap();
        match text {
            None => {
                statuses.remove(key);
            }
            Some(text) => {
                statuses.insert(key.to_string(), text.to_string());
            }
        }
    }

    /// Internal: clear extension statuses
    pub fn clear_extension_statuses(&self) {
        self.extension_statuses.lock().unwrap().clear();
    }

    /// Number of unique providers with available models (for footer display)
    pub fn get_available_provider_count(&self) -> usize {
        self.available_provider_count.load(Ordering::SeqCst)
    }

    /// Internal: update available provider count
    pub fn set_available_provider_count(&self, count: usize) {
        self.available_provider_count.store(count, Ordering::SeqCst);
    }

    pub fn set_cwd(&self, cwd: &str) {
        if self.cwd.lock().unwrap().as_str() == cwd {
            return;
        }

        *self.cwd.lock().unwrap() = cwd.to_string();
        if let Some(handle) = self.refresh_timer.lock().unwrap().take() {
            handle.abort();
        }
        self.clear_git_watchers();
        *self.cached_branch.lock().unwrap() = None;
        *self.git_paths.lock().unwrap() = Some(find_git_paths(cwd));
        self.setup_git_watcher();
        self.notify_branch_change();
    }

    /// Internal: cleanup
    pub fn dispose(&self) {
        self.disposed.store(true, Ordering::SeqCst);
        if let Some(handle) = self.refresh_timer.lock().unwrap().take() {
            handle.abort();
        }
        self.clear_git_watchers();
        self.branch_change_callbacks.lock().unwrap().clear();
    }
}

/// `() => void` unsubscribe handle returned by `onBranchChange`.
#[derive(Clone)]
pub struct Unsubscribe(Arc<dyn Fn() + Send + Sync>);

impl Unsubscribe {
    pub fn call(&self) {
        (self.0)();
    }
}

impl std::fmt::Debug for Unsubscribe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Unsubscribe")
    }
}

impl FooterDataProvider {
    fn notify_branch_change(&self) {
        let callbacks: Vec<Arc<dyn Fn() + Send + Sync>> = self
            .branch_change_callbacks
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for callback in callbacks {
            callback();
        }
    }

    fn schedule_refresh(&self) {
        if self.disposed.load(Ordering::SeqCst) {
            return;
        }
        let mut timer = self.refresh_timer.lock().unwrap();
        if timer.is_some() {
            return;
        }
        if self.refresh_in_flight.load(Ordering::SeqCst) {
            self.refresh_pending.store(true, Ordering::SeqCst);
            return;
        }
        let Some(handle) = self.runtime.lock().unwrap().clone() else {
            return;
        };
        let provider = self.self_ref();
        *timer = Some(handle.spawn(async move {
            tokio::time::sleep(Duration::from_millis(WATCH_DEBOUNCE_MS)).await;
            if let Some(provider) = provider {
                *provider.refresh_timer.lock().unwrap() = None;
                provider.refresh_git_branch_async().await;
            }
        }));
    }

    /// Weak handle used by the debounce timer so the provider can be dropped.
    fn self_ref(&self) -> Option<Arc<FooterDataProvider>> {
        self.self_handle.lock().unwrap().clone()
    }

    async fn refresh_git_branch_async(&self) {
        if self.disposed.load(Ordering::SeqCst) {
            return;
        }
        if self.refresh_in_flight.swap(true, Ordering::SeqCst) {
            self.refresh_pending.store(true, Ordering::SeqCst);
            return;
        }

        let next_branch = self.resolve_git_branch_async().await;
        if self.disposed.load(Ordering::SeqCst) {
            self.refresh_in_flight.store(false, Ordering::SeqCst);
            return;
        }
        let changed = {
            let mut cached = self.cached_branch.lock().unwrap();
            let changed = cached.is_some() && cached.clone().flatten() != next_branch;
            *cached = Some(next_branch);
            changed
        };
        self.refresh_in_flight.store(false, Ordering::SeqCst);
        if changed {
            self.notify_branch_change();
        }
        if self.refresh_pending.swap(false, Ordering::SeqCst)
            && !self.disposed.load(Ordering::SeqCst)
        {
            self.schedule_refresh();
        }
    }

    fn resolve_git_branch_sync(&self) -> Option<String> {
        let git_paths = self.git_paths.lock().unwrap().clone().flatten()?;
        let Ok(content) = std::fs::read_to_string(&git_paths.head_path) else {
            return None;
        };
        let content = content.trim().to_string();
        if let Some(branch) = content.strip_prefix("ref: refs/heads/") {
            if branch == ".invalid" {
                return resolve_branch_with_git_sync(&git_paths.repo_dir)
                    .or_else(|| Some("detached".to_string()));
            }
            return Some(branch.to_string());
        }
        Some("detached".to_string())
    }

    async fn resolve_git_branch_async(&self) -> Option<String> {
        let git_paths = match self.git_paths.lock().unwrap().clone().flatten() {
            Some(paths) => paths,
            None => return None,
        };
        let Ok(content) = std::fs::read_to_string(&git_paths.head_path) else {
            return None;
        };
        let content = content.trim().to_string();
        if let Some(branch) = content.strip_prefix("ref: refs/heads/") {
            if branch == ".invalid" {
                return resolve_branch_with_git_async(&git_paths.repo_dir)
                    .await
                    .or_else(|| Some("detached".to_string()));
            }
            return Some(branch.to_string());
        }
        Some("detached".to_string())
    }

    fn clear_git_watchers(&self) {
        close_watcher(self.head_watcher.lock().unwrap().as_ref());
        *self.head_watcher.lock().unwrap() = None;
        close_watcher(self.reftable_watcher.lock().unwrap().as_ref());
        *self.reftable_watcher.lock().unwrap() = None;
        close_watcher(self.reftable_tables_list_watcher.lock().unwrap().as_ref());
        *self.reftable_tables_list_watcher.lock().unwrap() = None;
        *self.reftable_tables_list_path.lock().unwrap() = None;
        if let Some(handle) = self.git_watcher_retry_timer.lock().unwrap().take() {
            handle.abort();
        }
    }

    fn schedule_git_watcher_retry(&self) {
        if self.disposed.load(Ordering::SeqCst) {
            return;
        }
        let mut timer = self.git_watcher_retry_timer.lock().unwrap();
        if timer.is_some() {
            return;
        }
        let Some(handle) = self.runtime.lock().unwrap().clone() else {
            return;
        };
        let provider = self.self_ref();
        *timer = Some(handle.spawn(async move {
            tokio::time::sleep(Duration::from_millis(FS_WATCH_RETRY_DELAY_MS)).await;
            if let Some(provider) = provider {
                *provider.git_watcher_retry_timer.lock().unwrap() = None;
                provider.setup_git_watcher();
            }
        }));
    }

    fn handle_git_watcher_error(&self) {
        self.clear_git_watchers();
        self.schedule_git_watcher_retry();
    }

    fn setup_git_watcher(&self) {
        self.clear_git_watchers();
        // Node's `fs.watch` starts synchronously; the Rust watcher needs a tokio
        // runtime to poll in. Without one, watchers are skipped (the cached branch
        // read path is unaffected).
        if self.runtime.lock().unwrap().is_none() {
            return;
        }
        let Some(git_paths) = self.git_paths.lock().unwrap().clone().flatten() else {
            return;
        };

        // Watch the directory containing HEAD, not HEAD itself.
        // Git uses atomic writes (write temp, rename over HEAD), which changes the inode.
        // fs.watch on a file stops working after the inode changes.
        let provider = self.self_ref();
        let refresh_provider = provider.clone();
        let error_provider = provider.clone();
        let head_dir = std::path::Path::new(&git_paths.head_path)
            .parent()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default();
        let head_watcher = watch_with_error_handler(
            &head_dir,
            move || {
                if let Some(provider) = &refresh_provider {
                    provider.schedule_refresh();
                }
            },
            move || {
                if let Some(provider) = &error_provider {
                    provider.handle_git_watcher_error();
                }
            },
        );
        *self.head_watcher.lock().unwrap() = head_watcher;
        if self.head_watcher.lock().unwrap().is_none() {
            return;
        }

        // In reftable repos, branch switches update files in the reftable directory
        // instead of HEAD. Watch it separately so the footer picks up those changes.
        let reftable_dir = std::path::Path::new(&git_paths.common_git_dir).join("reftable");
        if reftable_dir.exists() {
            let refresh_provider = provider.clone();
            let error_provider = provider.clone();
            let reftable_watcher = watch_with_error_handler(
                &reftable_dir.to_string_lossy(),
                move || {
                    if let Some(provider) = &refresh_provider {
                        provider.schedule_refresh();
                    }
                },
                move || {
                    if let Some(provider) = &error_provider {
                        provider.handle_git_watcher_error();
                    }
                },
            );
            *self.reftable_watcher.lock().unwrap() = reftable_watcher;
            if self.reftable_watcher.lock().unwrap().is_none() {
                return;
            }

            let tables_list_path = reftable_dir.join("tables.list");
            if tables_list_path.exists() {
                *self.reftable_tables_list_path.lock().unwrap() =
                    Some(tables_list_path.to_string_lossy().to_string());
                let refresh_provider = provider.clone();
                let error_provider = provider.clone();
                let tables_watcher = watch_with_error_handler(
                    &tables_list_path.to_string_lossy(),
                    move || {
                        if let Some(provider) = &refresh_provider {
                            provider.schedule_refresh();
                        }
                    },
                    move || {
                        if let Some(provider) = &error_provider {
                            provider.handle_git_watcher_error();
                        }
                    },
                );
                *self.reftable_tables_list_watcher.lock().unwrap() = tables_watcher;
            }
        }
    }
}

/// Read-only view for extensions - excludes setExtensionStatus, setAvailableProviderCount and dispose
pub trait ReadonlyFooterDataProvider: Send + Sync {
    fn get_git_branch(&self) -> Option<String>;
    fn get_extension_statuses(&self) -> BTreeMap<String, String>;
    fn get_available_provider_count(&self) -> usize;
    fn on_branch_change(&self, callback: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe;
}

impl ReadonlyFooterDataProvider for FooterDataProvider {
    fn get_git_branch(&self) -> Option<String> {
        FooterDataProvider::get_git_branch(self)
    }

    fn get_extension_statuses(&self) -> BTreeMap<String, String> {
        FooterDataProvider::get_extension_statuses(self)
    }

    fn get_available_provider_count(&self) -> usize {
        FooterDataProvider::get_available_provider_count(self)
    }

    fn on_branch_change(&self, callback: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        FooterDataProvider::on_branch_change(self, callback)
    }
}

impl FooterDataProvider {
    /// Builds a provider that timers can keep alive (`createFooterDataProvider`).
    pub fn new_shared(cwd: &str) -> Arc<FooterDataProvider> {
        let provider = Arc::new(FooterDataProvider::new(cwd));
        *provider.self_handle.lock().unwrap() = Some(provider.clone());
        provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_repo(dir: &std::path::Path) -> bool {
        let status = Command::new("git")
            .args(["init", "--quiet", "-b", "main"])
            .current_dir(dir)
            .status();
        matches!(status, Ok(status) if status.success())
    }

    #[test]
    fn missing_repo_reports_no_branch_and_detached_heads_report_detached() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FooterDataProvider::new_shared(dir.path().to_str().unwrap());
        assert!(provider.get_git_branch().is_none());
        assert_eq!(provider.get_available_provider_count(), 0);
        provider.set_available_provider_count(4);
        assert_eq!(provider.get_available_provider_count(), 4);
        provider.dispose();
    }

    #[test]
    fn branch_is_read_from_head_and_updates_are_notified() {
        let dir = tempfile::tempdir().unwrap();
        if !init_repo(dir.path()) {
            return; // git is unavailable in this environment
        }
        let provider = FooterDataProvider::new_shared(dir.path().to_str().unwrap());
        assert_eq!(provider.get_git_branch().as_deref(), Some("main"));

        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let unsubscribe = provider.on_branch_change(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        // A new cwd resets the cached branch and notifies subscribers.
        provider.set_cwd(dir.path().to_str().unwrap());
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        provider.set_cwd(nested.to_str().unwrap());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider.get_git_branch().as_deref(), Some("main"));
        unsubscribe.call();
        provider.dispose();
    }

    #[test]
    fn extension_statuses_are_ordered_and_clearable() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FooterDataProvider::new_shared(dir.path().to_str().unwrap());
        provider.set_extension_status("b", Some("two"));
        provider.set_extension_status("a", Some("one"));
        let statuses = provider.get_extension_statuses();
        let keys: Vec<&String> = statuses.keys().collect();
        assert_eq!(keys, vec!["a", "b"]);
        provider.set_extension_status("a", None);
        assert!(!provider.get_extension_statuses().contains_key("a"));
        provider.clear_extension_statuses();
        assert!(provider.get_extension_statuses().is_empty());
        provider.dispose();
    }

    #[test]
    fn disposed_providers_stop_scheduling() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FooterDataProvider::new_shared(dir.path().to_str().unwrap());
        provider.dispose();
        provider.schedule_refresh();
        assert!(provider.refresh_timer.lock().unwrap().is_none());
        assert!(provider.self_ref().is_some());
    }

    #[test]
    fn detached_head_reports_detached() {
        let dir = tempfile::tempdir().unwrap();
        if !init_repo(dir.path()) {
            return;
        }
        std::fs::write(dir.path().join(".git/HEAD"), b"0123456789abcdef\n").unwrap();
        let provider = FooterDataProvider::new_shared(dir.path().to_str().unwrap());
        assert_eq!(provider.get_git_branch().as_deref(), Some("detached"));
        provider.dispose();
    }
}
