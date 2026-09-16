//! Port of packages/coding-agent/src/core/memory (module index).
pub mod evidence;
pub(crate) mod extraction;
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

/// The OS guard survives long awaits and is released even if the process dies.
/// Its file is never unlinked: removing a locked inode would permit two holders.
#[derive(Debug)]
pub(crate) struct MemoryLock {
	path: String,
	owner: String,
	_guard: std::fs::File,
}

impl Drop for MemoryLock {
	fn drop(&mut self) {
		if std::fs::read_to_string(&self.path).is_ok_and(|owner| owner == self.owner) {
			let _ = std::fs::remove_file(&self.path);
		}
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
	let mut guard_options = std::fs::OpenOptions::new();
	guard_options.read(true).write(true).create(true).truncate(false);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		guard_options.mode(0o600);
	}
	#[cfg(windows)]
	{
		use std::os::windows::fs::OpenOptionsExt;
		use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
		guard_options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
	}
	let guard = guard_options.open(format!("{path}.guard")).map_err(|_| true)?;
	match guard.try_lock() {
		Ok(()) => {},
		Err(std::fs::TryLockError::WouldBlock) => return Err(false),
		Err(std::fs::TryLockError::Error(_)) => return Err(true),
	}
	// A v2 marker with an unlocked guard belongs to a crashed holder. Legacy
	// markers retain their stale window, and a live legacy PID is never evicted.
	match std::fs::read_to_string(&path) {
		Ok(owner) => {
			let legacy_live = owner.trim().parse::<i32>().ok().is_some_and(|pid| {
				pid > 0 && crate::utils::child_process::process_id_exists(pid)
			});
			if !owner.starts_with("optimus-memory-lock-v2:")
				&& (legacy_live || !lock_is_stale(&path, stale_ms))
			{
				return Err(false);
			}
			std::fs::remove_file(&path).map_err(|_| true)?;
		},
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
		Err(_) => return Err(true),
	}
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
			let owner = format!("optimus-memory-lock-v2:{}:{}\n", std::process::id(), uuid::Uuid::new_v4());
			if file.write_all(owner.as_bytes()).and_then(|_| file.sync_all()).is_err() {
				let _ = std::fs::remove_file(&path);
				return Err(true);
			}
			Ok(MemoryLock { path, owner, _guard: guard })
		}
		Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
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

	fn retry_options() -> MemoryLockRetries {
		MemoryLockRetries { stale_ms: 10, retries: 0, min_timeout_ms: 1, max_timeout_ms: 1 }
	}

	#[test]
	fn live_memory_lock_does_not_expire_and_release_preserves_replacement() {
		let root = tempfile::tempdir().unwrap();
		let target = root.path().join("memory").to_string_lossy().into_owned();
		let held = acquire_lock_sync(&target, retry_options()).unwrap();
		std::thread::sleep(std::time::Duration::from_millis(40));
		for _ in 0..3 {
			assert!(acquire_lock_sync(&target, retry_options()).is_err());
		}
		std::fs::write(lock_path_for_dir(&target), "replacement-owner").unwrap();
		drop(held);
		assert_eq!(std::fs::read_to_string(lock_path_for_dir(&target)).unwrap(), "replacement-owner");
	}

	#[test]
	fn memory_lock_recovers_crashed_marker_but_not_a_live_legacy_holder() {
		let root = tempfile::tempdir().unwrap();
		let target = root.path().join("memory").to_string_lossy().into_owned();
		std::fs::write(lock_path_for_dir(&target), format!("{}\n", std::process::id())).unwrap();
		std::thread::sleep(std::time::Duration::from_millis(40));
		assert!(acquire_lock_sync(&target, retry_options()).is_err());
		std::fs::write(lock_path_for_dir(&target), "optimus-memory-lock-v2:dead:fixture\n").unwrap();
		let held = acquire_lock_sync(&target, retry_options()).unwrap();
		drop(held);
		assert!(!std::path::Path::new(&lock_path_for_dir(&target)).exists());
		assert!(acquire_lock_sync(&target, retry_options()).is_ok());
	}

	#[test]
	fn memory_lock_child_process_fixture() {
		let Ok(target) = std::env::var("OPTIMUS_MEMORY_LOCK_TEST_TARGET") else { return; };
		let _held = acquire_lock_sync(&target, retry_options()).expect("child acquires lock");
		std::fs::write(format!("{target}.ready"), "ready").unwrap();
		// The parent kills only this owned child to exercise OS lock recovery.
		std::thread::sleep(std::time::Duration::from_secs(15));
	}

	#[test]
	fn memory_lock_excludes_other_process_and_recovers_after_crash() {
		struct OwnedChild(std::process::Child);
		impl Drop for OwnedChild {
			fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
		}
		let root = tempfile::tempdir().unwrap();
		let target = root.path().join("memory").to_string_lossy().into_owned();
		let mut command = std::process::Command::new(std::env::current_exe().unwrap());
		command.args(["--exact", "core::memory::tests::memory_lock_child_process_fixture", "--nocapture"])
			.env("OPTIMUS_MEMORY_LOCK_TEST_TARGET", &target);
		#[cfg(windows)]
		{
			use std::os::windows::process::CommandExt;
			command.creation_flags(0x08000000);
		}
		let mut child = OwnedChild(command.spawn().unwrap());
		let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
		while !std::path::Path::new(&format!("{target}.ready")).exists() {
			assert!(std::time::Instant::now() < deadline, "child readiness timeout");
			assert!(child.0.try_wait().unwrap().is_none(), "child exited before holding lock");
			std::thread::sleep(std::time::Duration::from_millis(10));
		}
		std::thread::sleep(std::time::Duration::from_millis(40));
		assert!(acquire_lock_sync(&target, retry_options()).is_err());
		child.0.kill().unwrap();
		child.0.wait().unwrap();
		assert!(acquire_lock_sync(&target, retry_options()).is_ok());
	}

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
