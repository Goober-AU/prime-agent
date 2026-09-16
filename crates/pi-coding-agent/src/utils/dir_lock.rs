//! Port of packages/coding-agent/src/utils/dir-lock.ts

use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirLockAttempt {
    Acquired,
    Held,
    Reclaimed,
}

impl DirLockAttempt {
    pub fn as_str(self) -> &'static str {
        match self {
            DirLockAttempt::Acquired => "acquired",
            DirLockAttempt::Held => "held",
            DirLockAttempt::Reclaimed => "reclaimed",
        }
    }
}

/// link(2)-published lock file: born with its owner content, EEXIST the only
/// collision signal; stale locks are renamed aside, verified, then deleted or
/// restored. Directories at the lock path are preserved and require owner intervention.
const CANDIDATE_SWEEP_AGE_MS: u128 = 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatIdentity {
    pub(crate) dev: u64,
    pub(crate) ino: u64,
    is_dir: bool,
}

// The candidate prefix can never match the lock; the age gate spares mid-publish rivals.
fn sweep_abandoned_candidates(lock_path: &str) {
    let path = Path::new(lock_path);
    let Some(directory) = path.parent() else {
        return;
    };
    let prefix = format!(
        "{}.candidate-",
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default()
    );
    let cutoff = std::time::SystemTime::now().checked_sub(std::time::Duration::from_millis(
        CANDIDATE_SWEEP_AGE_MS as u64,
    ));

    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(&prefix) {
            continue;
        }
        // Litter collection only.
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let is_old = match cutoff {
            Some(cutoff) => modified < cutoff,
            None => false,
        };
        if is_old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Node's `statSync(path, { bigint: true })` identity plus the directory flag.
pub(crate) fn stat_identity(path: &Path) -> Option<StatIdentity> {
    let file = open_lock(path).ok()?;
    file_identity(&file)
}

#[cfg(unix)]
pub(crate) fn file_identity(file: &std::fs::File) -> Option<StatIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().ok()?;
    Some(StatIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        is_dir: metadata.is_dir(),
    })
}

#[cfg(windows)]
pub(crate) fn file_identity(file: &std::fs::File) -> Option<StatIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // The owned File keeps the handle and file index alive through reclamation.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return None;
    }
    Some(StatIdentity {
        dev: u64::from(info.dwVolumeSerialNumber),
        ino: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        is_dir: info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
    })
}

pub(crate) fn open_lock(path: &Path) -> std::io::Result<std::fs::File> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "Refusing to reclaim a symbolic link as a lock",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

pub fn try_acquire_dir_lock<F, Fut>(
    lock_path: &str,
    owner_alive: F,
) -> impl Future<Output = std::io::Result<DirLockAttempt>> + Send
where
    F: Fn(Option<i32>) -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = bool> + Send,
{
    let lock_path = lock_path.to_string();
    async move {
        // A directory can contain shared registry state or an old lock whose
        // owner is still publishing its marker. Neither is safe to reclaim.
        if std::fs::symlink_metadata(&lock_path)
            .is_ok_and(|m| m.is_dir() || m.file_type().is_symlink())
        {
            return Ok(DirLockAttempt::Held);
        }
        sweep_abandoned_candidates(&lock_path);
        acquire_attempt(&lock_path, owner_alive, true).await
    }
}

/// The retry after a swept candidate recurses, so the future is boxed like the
/// TypeScript's async recursion.
fn acquire_attempt<'a, F, Fut>(
    lock_path: &'a str,
    owner_alive: F,
    retry_on_swept_candidate: bool,
) -> Pin<Box<dyn Future<Output = std::io::Result<DirLockAttempt>> + Send + 'a>>
where
    F: Fn(Option<i32>) -> Fut + Copy + Send + Sync + 'a,
    Fut: Future<Output = bool> + Send + 'a,
{
    Box::pin(async move {
        let token = format!("{}-{}", std::process::id(), uuid::Uuid::new_v4());
        let temp_path = format!("{}.candidate-{}", lock_path, token);

        write_owner_file(&temp_path, std::process::id())?;

        let result = async {
            match link_onto(&temp_path, lock_path) {
                Ok(()) => return Ok(DirLockAttempt::Acquired),
                Err(link_error) => {
                    // NFS can report failure for a link that landed: nlink 2 means it published.
                    let mut candidate_swept = false;
                    let mut rechecked_nlink: Option<u64> = None;
                    match std::fs::metadata(&temp_path) {
                        Ok(metadata) => rechecked_nlink = Some(link_count(&metadata)),
                        Err(stat_error) => {
                            // Only a definite ENOENT means the candidate was swept.
                            if stat_error.kind() != std::io::ErrorKind::NotFound {
                                return Err(link_error);
                            }
                            candidate_swept = true;
                        }
                    }
                    if rechecked_nlink == Some(2) {
                        return Ok(DirLockAttempt::Acquired);
                    }
                    if candidate_swept && retry_on_swept_candidate {
                        return acquire_attempt(lock_path, owner_alive, false).await;
                    }

                    if !is_already_exists(&link_error) {
                        return Err(link_error);
                    }
                }
            }

            // One immutable dev+ino capture keys every later decision about the judged lock.
            let pinned = match open_lock(Path::new(lock_path)) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(DirLockAttempt::Reclaimed)
                }
                Err(_) => return Ok(DirLockAttempt::Held),
            };
            let Some(captured) = file_identity(&pinned) else {
                return Ok(DirLockAttempt::Held);
            };
            if captured.ino == 0 {
                // Some Windows filesystems report no stable file index: identity unavailable.
                return Ok(DirLockAttempt::Held);
            }

            let result = judge_and_reclaim(lock_path, owner_alive, captured, &token).await;
            drop(pinned);
            result
        }
        .await;

        // Cleanup only: a leaked candidate must never mask a settled acquisition.
        let _ = std::fs::remove_file(&temp_path);
        result
    })
}

#[cfg(unix)]
fn link_count(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink()
}

#[cfg(not(unix))]
fn link_count(_metadata: &std::fs::Metadata) -> u64 {
    // NTFS does not expose POSIX link counts; the EEXIST path is the only signal.
    1
}

fn is_already_exists(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::AlreadyExists || error.raw_os_error() == Some(17)
}

fn write_owner_file(temp_path: &str, pid: u32) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(temp_path)?;
    write!(file, "{}\n", pid)?;
    Ok(())
}

fn link_onto(from: &str, to: &str) -> std::io::Result<()> {
    std::fs::hard_link(from, to)
}

async fn judge_and_reclaim<F, Fut>(
    lock_path: &str,
    owner_alive: F,
    captured: StatIdentity,
    token: &str,
) -> std::io::Result<DirLockAttempt>
where
    F: Fn(Option<i32>) -> Fut + Copy + Send + Sync,
    Fut: Future<Output = bool> + Send,
{
    // A registry, session directory, or unexpected legacy payload is durable
    // state, not a disposable lock. Never rename or recursively delete it.
    if captured.is_dir {
        return Ok(DirLockAttempt::Held);
    }
    let judged = read_owner_raw(lock_path, captured.is_dir);
    if judged == OwnerRead::Unreadable {
        // A transient read failure may hide a LIVE lock: never judge it stale.
        return Ok(DirLockAttempt::Held);
    }
    let owner_pid = match &judged {
        OwnerRead::Absent => None,
        OwnerRead::Content(content) => strict_pid(Some(content)),
        OwnerRead::Unreadable => unreachable!(),
    };
    if owner_alive(owner_pid).await {
        return Ok(DirLockAttempt::Held);
    }
    if stat_identity(Path::new(lock_path)) != Some(captured) {
        return Ok(DirLockAttempt::Held);
    }

    let aside_path = format!("{}.stale-{}", lock_path, token);
    match std::fs::rename(lock_path, &aside_path) {
        Ok(()) => {}
        Err(reclaim_error) => {
            // ENOENT: a racing reclaimer moved it first.
            if reclaim_error.kind() == std::io::ErrorKind::NotFound {
                return Ok(DirLockAttempt::Reclaimed);
            }
            // A lost-reply rename: if the lock path is gone, something moved - fall to verify.
            if stat_identity(Path::new(lock_path)).is_some() {
                return Err(reclaim_error);
            }
        }
    }

    let aside = stat_identity(Path::new(&aside_path));
    if let Some(aside) = aside {
        if aside.dev == captured.dev && aside.ino == captured.ino {
            // Only a pinned regular file can enter this path. Never recursively
            // remove a directory, even if a caller chose the wrong lock path.
            if aside.is_dir {
                return Ok(DirLockAttempt::Held);
            }
            std::fs::remove_file(&aside_path)?;
            return Ok(DirLockAttempt::Reclaimed);
        }
    }

    // Not the judged lock: preserve directories and restore files only through
    // a no-clobber hard link. Any failure leaves the replacement intact aside.
    match aside {
        Some(aside) if aside.is_dir => {
            // An unexpected replacement is preserved at its unique aside path.
            // rename() could overwrite a new owner's directory on Unix.
        }
        _ => {
            if std::fs::hard_link(&aside_path, lock_path).is_ok() {
                let _ = std::fs::remove_file(&aside_path);
            }
        }
    }
    Ok(DirLockAttempt::Held)
}

#[derive(Debug, PartialEq, Eq)]
enum OwnerRead {
    Content(String),
    Absent,
    Unreadable,
}

// "absent" is safely stale territory; "unreadable" may be a LIVE lock (transient EPERM/EBUSY).
fn read_owner_raw(path: &str, legacy_dir: bool) -> OwnerRead {
    let target: PathBuf = if legacy_dir {
        Path::new(path).join("pid")
    } else {
        PathBuf::from(path)
    };
    match std::fs::read_to_string(&target) {
        Ok(content) => OwnerRead::Content(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => OwnerRead::Absent,
        Err(_) => OwnerRead::Unreadable,
    }
}

// kill(0)/kill(-n) probe our own process group: only an exact positive integer owns.
pub fn strict_pid(raw: Option<&str>) -> Option<i32> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() || !trimmed.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    match trimmed.parse::<i32>() {
        Ok(parsed) if parsed > 0 => Some(parsed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        path
    }

    #[tokio::test]
    async fn acquires_a_free_lock() {
        let dir = temp_dir();
        let lock = dir.join("session.lock");
        let attempt = try_acquire_dir_lock(lock.to_str().unwrap(), |_| async { false })
            .await
            .unwrap();
        assert_eq!(attempt, DirLockAttempt::Acquired);
        assert!(lock.exists());
        assert_eq!(
            std::fs::read_to_string(&lock).unwrap(),
            format!("{}\n", std::process::id())
        );
    }

    #[tokio::test]
    async fn reports_held_when_the_owner_is_alive() {
        let dir = temp_dir();
        let lock = dir.join("session.lock");
        std::fs::write(&lock, format!("{}\n", std::process::id())).unwrap();
        let attempt =
            try_acquire_dir_lock(lock.to_str().unwrap(), |pid| async move { pid.is_some() })
                .await
                .unwrap();
        assert_eq!(attempt, DirLockAttempt::Held);
        assert!(lock.exists());
    }

    #[tokio::test]
    async fn reclaims_a_stale_lock() {
        let dir = temp_dir();
        let lock = dir.join("session.lock");
        std::fs::write(&lock, "999999\n").unwrap();
        let attempt = try_acquire_dir_lock(lock.to_str().unwrap(), |_| async { false })
            .await
            .unwrap();
        assert_eq!(attempt, DirLockAttempt::Reclaimed);
        assert!(!lock.exists());
    }

    #[tokio::test]
    async fn reclaims_a_legacy_directory_lock_without_an_owner() {
        let dir = temp_dir();
        let lock = dir.join("session.lock");
        std::fs::create_dir_all(&lock).unwrap();
        // `readOwnerRaw(lockPath, isDir)` reads <lock>/pid: ENOENT is "absent", so
        // an ownerless legacy directory is judged stale and reclaimed.
        let attempt = try_acquire_dir_lock(lock.to_str().unwrap(), |_| async { false })
            .await
            .unwrap();
        assert_eq!(attempt, DirLockAttempt::Reclaimed);
        assert!(!lock.exists());
    }

    #[tokio::test]
    async fn treats_a_legacy_directory_lock_with_a_live_owner_as_held() {
        let dir = temp_dir();
        let lock = dir.join("session.lock");
        std::fs::create_dir_all(&lock).unwrap();
        std::fs::write(lock.join("pid"), format!("{}\n", std::process::id())).unwrap();
        let attempt =
            try_acquire_dir_lock(lock.to_str().unwrap(), |pid| async move { pid.is_some() })
                .await
                .unwrap();
        assert_eq!(attempt, DirLockAttempt::Held);
        assert!(lock.exists());
    }

    #[test]
    fn strict_pid_only_accepts_positive_integers() {
        assert_eq!(strict_pid(Some(" 42 ")), Some(42));
        assert_eq!(strict_pid(Some("0")), None);
        assert_eq!(strict_pid(Some("-3")), None);
        assert_eq!(strict_pid(Some("abc")), None);
        assert_eq!(strict_pid(Some("")), None);
        assert_eq!(strict_pid(None), None);
    }

    #[tokio::test]
    async fn sweeps_only_old_candidates() {
        let dir = temp_dir();
        let lock = dir.join("session.lock");
        let fresh = dir.join("session.lock.candidate-fresh");
        let old = dir.join("session.lock.candidate-old");
        std::fs::write(&fresh, "1\n").unwrap();
        std::fs::write(&old, "1\n").unwrap();
        let old_time = std::time::SystemTime::now()
            - std::time::Duration::from_millis(CANDIDATE_SWEEP_AGE_MS as u64 + 1000);
        if let Ok(file) = std::fs::File::options().write(true).open(&old) {
            let _ = file.set_times(std::fs::FileTimes::new().set_modified(old_time));
        }

        let attempt = try_acquire_dir_lock(lock.to_str().unwrap(), |_| async { false })
            .await
            .unwrap();
        assert_eq!(attempt, DirLockAttempt::Acquired);
        assert!(fresh.exists());
        assert!(!old.exists());
    }
}
