//! Port of packages/coding-agent/src/core/memory (module index).
pub mod evidence;
pub mod jobs;
pub mod project;
pub mod search;
pub mod server;
pub mod service;
pub mod sharing;
pub mod store;

// ---------------------------------------------------------------------------
// Internal plumbing.
//
// `store.ts`, `jobs.ts`, `sharing.ts`, `project.ts` and `server.ts` take their
// mutual exclusion from the third-party `proper-lockfile` package
// (`lockSync(dir)` / `await lock(dir, {...})`), which is not part of the mapped
// port tree. The behaviour those call sites depend on - one holder per
// directory, a stale window, bounded retries - is implemented here and stays
// private to the memory module.
// ---------------------------------------------------------------------------

/// Retry loop shape of `proper-lockfile`'s options object.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MemoryLockRetries {
	/// Stale lock age in milliseconds.
	pub stale_ms: u64,
	/// Number of retries after the first attempt.
	pub retries: u32,
	pub min_timeout_ms: u64,
	pub max_timeout_ms: u64,
}

pub(crate) fn lock_path_for_dir(dir: &str) -> String {
	format!("{dir}.lock")
}

/// Held lock. Releasing removes the lock entry, like `proper-lockfile`'s release.
pub(crate) struct MemoryLock {
	path: String,
}

impl Drop for MemoryLock {
	fn drop(&mut self) {
		let _ = std::fs::remove_file(&self.path);
	}
}

fn lock_is_stale(path: &str, stale_ms: u64) -> bool {
	if stale_ms == 0 {
		return false;
	}
	match std::fs::metadata(path).and_then(|metadata| metadata.modified()) {
		Ok(modified) => match modified.elapsed() {
			Ok(age) => age > std::time::Duration::from_millis(stale_ms),
			Err(_) => false,
		},
		Err(_) => false,
	}
}

fn try_lock_once(dir: &str, stale_ms: u64) -> Result<MemoryLock, bool> {
	let path = lock_path_for_dir(dir);
	let mut options = std::fs::OpenOptions::new();
	options.write(true).create_new(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		options.mode(0o600);
	}
	match options.open(&path) {
		Ok(mut file) => {
			use std::io::Write;
			let _ = write!(file, "{}\n", std::process::id());
			Ok(MemoryLock { path })
		}
		Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
			if lock_is_stale(&path, stale_ms) {
				let _ = std::fs::remove_file(&path);
			}
			Err(false)
		}
		Err(_) => Err(true),
	}
}

fn retry_delay(retries: MemoryLockRetries, attempt: u32) -> u64 {
	(retries.min_timeout_ms + attempt as u64 * 20).max(1).min(retries.max_timeout_ms.max(1))
}

/// `await lock(dir, {...})`: bounded retries, then the same failure the
/// TypeScript observes as an `ELOCKED` rejection.
pub(crate) async fn acquire_lock(dir: &str, retries: MemoryLockRetries) -> Result<MemoryLock, String> {
	let mut attempt: u32 = 0;
	loop {
		match try_lock_once(dir, retries.stale_ms) {
			Ok(lock) => return Ok(lock),
			Err(fatal) => {
				if fatal {
					return Err(format!("Failed to acquire lock for {dir}"));
				}
				if attempt >= retries.retries {
					return Err(format!("Lock file is already being held: {}", lock_path_for_dir(dir)));
				}
				tokio::time::sleep(std::time::Duration::from_millis(retry_delay(retries, attempt))).await;
				attempt += 1;
			}
		}
	}
}

/// `lockSync(dir)`: the blocking form used by constructor-time writes.
pub(crate) fn acquire_lock_sync(dir: &str, retries: MemoryLockRetries) -> Result<MemoryLock, String> {
	let mut attempt: u32 = 0;
	loop {
		match try_lock_once(dir, retries.stale_ms) {
			Ok(lock) => return Ok(lock),
			Err(fatal) => {
				if fatal {
					return Err(format!("Failed to acquire lock for {dir}"));
				}
				if attempt >= retries.retries {
					return Err(format!("Lock file is already being held: {}", lock_path_for_dir(dir)));
				}
				std::thread::sleep(std::time::Duration::from_millis(retry_delay(retries, attempt)));
				attempt += 1;
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn acquires_and_releases_a_directory_lock() {
		let dir = std::env::temp_dir().join(format!("prime-memory-lock-{}", uuid::Uuid::new_v4()));
		std::fs::create_dir_all(&dir).unwrap();
		let target = dir.to_string_lossy().to_string();
		{
			let _lock = acquire_lock_sync(
				&target,
				MemoryLockRetries {
					stale_ms: 30_000,
					retries: 0,
					min_timeout_ms: 0,
					max_timeout_ms: 0,
				},
			)
			.expect("lock");
			assert!(std::path::Path::new(&lock_path_for_dir(&target)).exists());
		}
		assert!(!std::path::Path::new(&lock_path_for_dir(&target)).exists());
		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn reports_a_held_lock_after_the_retry_budget() {
		let dir = std::env::temp_dir().join(format!("prime-memory-lock-{}", uuid::Uuid::new_v4()));
		std::fs::create_dir_all(&dir).unwrap();
		let target = dir.to_string_lossy().to_string();
		let held = acquire_lock_sync(
			&target,
			MemoryLockRetries {
				stale_ms: 30_000,
				retries: 0,
				min_timeout_ms: 0,
				max_timeout_ms: 0,
			},
		)
		.expect("lock");
		let error = acquire_lock_sync(
			&target,
			MemoryLockRetries {
				stale_ms: 30_000,
				retries: 0,
				min_timeout_ms: 0,
				max_timeout_ms: 0,
			},
		)
		.expect_err("held");
		assert!(error.starts_with("Lock file is already being held:"));
		drop(held);
		std::fs::remove_dir_all(&dir).ok();
	}
}
