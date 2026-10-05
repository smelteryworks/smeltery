//! The `file` store: one file per key under `CACHE_PATH`
//! (`<dir>/<first 2 hex chars>/<sha256 of the key>`), locks under `<dir>/locks/`.
//!
//! A file holds the expiry (Unix ms, `0` = never) on the first line, the key as a JSON string
//! on the second (to tell hash collisions and prefixes apart), then the value. Writes go to a
//! temporary file that is renamed over the entry, so readers never see half a value.
//! Read-modify-write operations (`add`, `increment`, locks) run under a per-key lock: an
//! in-process mutex stripe plus a `<entry>.lock` file created with `O_EXCL`, which also
//! excludes other processes on the same machine. All file work runs on blocking threads.
//! On Windows, a file another handle is deleting answers `PermissionDenied` for a moment; the
//! store waits that out (briefly, see `retry_busy`) instead of failing.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{CacheStore, add_to, expires_at, live};
use crate::app::BoxFuture;
use crate::error::{Error, Result};

const STRIPES: usize = 64;
/// A lock file older than this belongs to a crashed process and is removed.
const STALE_LOCK: Duration = Duration::from_secs(30);
/// How long a read-modify-write waits for the per-key lock file.
const LOCK_WAIT: Duration = Duration::from_secs(10);
/// How long a single file operation retries Windows' "file is being deleted" answer (see [`busy`]).
const BUSY_WAIT: Duration = Duration::from_secs(1);

/// Whether `e` means another thread or process is deleting this very file right now. Windows answers every
/// open, create, rename or delete of a file in that delete-pending state with `ERROR_ACCESS_DENIED`
/// (`PermissionDenied`) until the deleting handle closes, a window of microseconds. Lock files are created and
/// removed constantly, so this is ordinary contention there, not a permission problem. Always `false` on Unix,
/// where a removed name is free at once.
fn busy(e: &std::io::Error) -> bool {
    cfg!(windows) && e.kind() == ErrorKind::PermissionDenied
}

/// Runs `op`, retrying a [`busy`] error with a short growing pause for at most [`BUSY_WAIT`]; any other result
/// (and a busy error that outlasts the wait) is returned as is. On Unix `op` runs once.
fn retry_busy<T>(mut op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let start = Instant::now();
    let mut pause = Duration::from_millis(1);
    loop {
        match op() {
            Err(e) if busy(&e) && start.elapsed() < BUSY_WAIT => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(16));
            }
            other => return other,
        }
    }
}

pub(crate) struct FileStore {
    dir: Arc<PathBuf>,
    stripes: Vec<tokio::sync::Mutex<()>>,
    /// `CACHE_MAX_VALUE_BYTES`: `get` refuses a larger entry file without reading it.
    max_value: usize,
    /// Tests: sweep every shard on every write (this store only).
    #[cfg(test)]
    clean_always: std::sync::atomic::AtomicBool,
}

/// Remove the expired entries of shard `shard` under `base`, each under its lock file (and only when it is still
/// expired then); entries another process holds are skipped. Returns how many went.
fn sweep_shard(base: &Path, shard: u8) -> usize {
    let Ok(files) = std::fs::read_dir(base.join(format!("{shard:02x}"))) else {
        return 0;
    };
    let expired = |path: &Path| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| parse(&text).map(|(_, _, expires)| !live(expires)))
            .unwrap_or(false)
    };
    let mut removed = 0;
    for file in files.flatten() {
        let path = file.path();
        // Entry files are bare hex names; skip `.lock` and `.tmp` files.
        if path.extension().is_some() || !expired(&path) {
            continue;
        }
        let Ok(_guard) = LockFile::acquire(&path) else {
            continue;
        };
        if expired(&path) && remove(&path).unwrap_or(false) {
            removed += 1;
        }
    }
    removed
}

impl FileStore {
    /// On about one write in a hundred, remove the expired entries and locks of one random shard (of 256): files
    /// are never expired by anything else.
    async fn maybe_sweep(&self) {
        #[cfg(test)]
        let all = self.clean_always.load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(test))]
        let all = false;
        if !all && !super::sometimes() {
            return;
        }
        let shards: Vec<u8> = if all {
            (0..=u8::MAX).collect()
        } else {
            crate::crypto::random_bytes(1).unwrap_or_default()
        };
        let dir = Arc::clone(&self.dir);
        let swept = Self::blocking(move || {
            Ok(shards
                .into_iter()
                .map(|s| sweep_shard(&dir, s) + sweep_shard(&dir.join("locks"), s))
                .sum::<usize>())
        })
        .await;
        if let Ok(n) = swept
            && n > 0
        {
            tracing::debug!(removed = n, "file cache: expired entries removed");
        }
    }

    /// The same store with this `CACHE_MAX_VALUE_BYTES`.
    pub(crate) fn max_value(mut self, bytes: usize) -> Self {
        self.max_value = bytes;
        self
    }

    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir: Arc::new(dir),
            stripes: (0..STRIPES).map(|_| tokio::sync::Mutex::new(())).collect(),
            max_value: super::DEFAULT_MAX_VALUE_BYTES,
            #[cfg(test)]
            clean_always: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn path(&self, locks: bool, key: &str) -> (PathBuf, u8) {
        let digest = Sha256::digest(key.as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let base = if locks {
            self.dir.join("locks")
        } else {
            self.dir.as_ref().clone()
        };
        let shard = hex.get(..2).unwrap_or("00").to_owned();
        (
            base.join(shard).join(hex),
            digest.first().copied().unwrap_or(0),
        )
    }

    /// Run `f` on a blocking thread.
    async fn blocking<T: Send + 'static>(
        f: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|e| Error::internal(format!("a file cache task failed: {e}")))?
    }

    /// Run `f` on a blocking thread while holding the key's lock (stripe + lock file).
    async fn guarded<T: Send + 'static>(
        &self,
        path: PathBuf,
        stripe: u8,
        f: impl FnOnce(&Path) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let mutex = self
            .stripes
            .get(usize::from(stripe) % STRIPES)
            .ok_or_else(|| Error::internal("no lock stripe"))?;
        let _held = mutex.lock().await;
        Self::blocking(move || {
            let _guard = LockFile::acquire(&path)?;
            f(&path)
        })
        .await
    }
}

/// Unix modes of what the store creates: entries and lock files for the app's user only.
const FILE_MODE: u32 = 0o600;
const DIR_MODE: u32 = 0o700;

/// Create `path` (an entry's temp file or a lock file) only when it does not exist, private on Unix.
fn create_new(path: &Path) -> std::io::Result<std::fs::File> {
    crate::fsx::file_mode(
        std::fs::OpenOptions::new().write(true).create_new(true),
        FILE_MODE,
    )
    .open(path)
}

/// Whether the file at `path` was last written more than [`STALE_LOCK`] ago.
fn is_stale(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > STALE_LOCK)
}

/// Run `f` while holding `<lock>.takeover`, the right to remove `<lock>` (a stale one, or one's own). Checking a
/// lock file and removing it are two steps; done by two waiters at once, the second removal could hit the lock
/// the first one had just created, and both would hold the key. `None` when the takeover file stayed taken for
/// [`BUSY_WAIT`].
fn with_takeover<T>(lock: &Path, f: impl FnOnce() -> T) -> Option<T> {
    let mut name = lock.as_os_str().to_owned();
    name.push(".takeover");
    let takeover = PathBuf::from(name);
    let start = Instant::now();
    loop {
        match create_new(&takeover) {
            Ok(_) => break,
            Err(e) if e.kind() == ErrorKind::AlreadyExists || busy(&e) => {
                // Only a process that died within the few microseconds of a takeover leaves this file.
                if is_stale(&takeover) {
                    let _ = std::fs::remove_file(&takeover);
                    continue;
                }
                if start.elapsed() > BUSY_WAIT {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return None,
        }
    }
    let out = f();
    let _ = retry_busy(|| std::fs::remove_file(&takeover));
    Some(out)
}

/// The `<entry>.lock` file, holding a random token of its holder; removed on drop while it still holds it.
struct LockFile {
    path: PathBuf,
    token: String,
}

impl LockFile {
    fn acquire(entry: &Path) -> Result<Self> {
        let path = entry.with_extension("lock");
        if let Some(parent) = path.parent() {
            crate::fsx::create_dir_all(parent, DIR_MODE)?;
        }
        let token = crate::crypto::random_token(24)?;
        let start = Instant::now();
        loop {
            match create_new(&path) {
                Ok(mut file) => {
                    // A failed write leaves a lock without a token: it is never removed as ours, and is taken over
                    // once stale.
                    let _ = file.write_all(token.as_bytes());
                    return Ok(Self { path, token });
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    if is_stale(&path) {
                        // Checked again under the takeover file: only one waiter removes it, and only while
                        // it is still the stale one.
                        with_takeover(&path, || {
                            if is_stale(&path) {
                                let _ = std::fs::remove_file(&path);
                            }
                        });
                        continue;
                    }
                    if start.elapsed() > LOCK_WAIT {
                        return Err(Error::internal(format!(
                            "the file cache entry stayed locked for {LOCK_WAIT:?}"
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                // Windows: the previous holder is removing its lock file this instant (see `busy`); the name is
                // free again once that delete completes, so wait like for a held lock. A denial that lasts the
                // whole wait is a real permission problem and is returned as one.
                Err(e) if busy(&e) => {
                    if start.elapsed() > LOCK_WAIT {
                        return Err(e.into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        // A holder slower than STALE_LOCK may have lost its lock to a takeover: never remove the new holder's.
        let ours = || {
            retry_busy(|| std::fs::read_to_string(&self.path)).is_ok_and(|held| held == self.token)
        };
        let removed = with_takeover(&self.path, || {
            if ours() {
                let _ = retry_busy(|| std::fs::remove_file(&self.path));
            }
        });
        if removed.is_none() && ours() {
            let _ = retry_busy(|| std::fs::remove_file(&self.path));
        }
    }
}

/// The entry in `path` for `key`: `(value, expires)`, live or not. A missing, corrupt or
/// foreign file is `None`.
fn read(path: &Path, key: &str) -> Result<Option<(String, Option<u64>)>> {
    // An unguarded read can meet an entry that `flush` is removing (Windows: `busy`).
    let text = match retry_busy(|| std::fs::read_to_string(path)) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        // Not UTF-8: a corrupt entry, treated as missing.
        Err(e) if e.kind() == ErrorKind::InvalidData => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(parse(&text)
        .and_then(|(stored, value, expires)| (stored == key).then(|| (value.to_owned(), expires))))
}

fn parse(text: &str) -> Option<(String, &str, Option<u64>)> {
    let mut parts = text.splitn(3, '\n');
    let expires: u64 = parts.next()?.parse().ok()?;
    let key: String = serde_json::from_str(parts.next()?).ok()?;
    let value = parts.next()?;
    Some((key, value, (expires != 0).then_some(expires)))
}

fn read_live(path: &Path, key: &str) -> Result<Option<(String, Option<u64>)>> {
    Ok(read(path, key)?.filter(|(_, expires)| live(*expires)))
}

fn write(path: &Path, key: &str, value: &str, expires: Option<u64>) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::internal("a file cache entry has no directory"))?;
    crate::fsx::create_dir_all(parent, DIR_MODE)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::internal("a file cache entry has no name"))?;
    let tmp = parent.join(format!("{name}.{}.tmp", crate::crypto::random_token(12)?));
    let written = (|| -> std::io::Result<()> {
        let mut file = create_new(&tmp)?;
        write!(
            file,
            "{}\n{}\n{value}",
            expires.unwrap_or(0),
            serde_json::to_string(key)?
        )?;
        drop(file);
        // The entry may be in the middle of a `flush` delete (Windows: `busy`).
        retry_busy(|| std::fs::rename(&tmp, path))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(written?)
}

fn remove(path: &Path) -> Result<bool> {
    match retry_busy(|| std::fs::remove_file(path)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Whether `name` is all lowercase hex digits and `len` long: the names of shard folders (2) and entries (64).
fn hex_name(name: &std::ffi::OsStr, len: usize) -> bool {
    name.to_str().is_some_and(|n| {
        n.len() == len && n.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Remove the entry files under `dir` whose key starts with `prefix` (and with none of `keep`), and every expired
/// one. Only files shaped like the store's own (`<2 hex>/<64 hex>`) that parse as entries are touched: a
/// `CACHE_PATH` pointed at another folder by mistake loses none of its files.
fn flush_dir(dir: &Path, prefix: &str, keep: &[String]) -> Result<()> {
    let shards = match std::fs::read_dir(dir) {
        Ok(shards) => shards,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for shard in shards.flatten() {
        if !hex_name(&shard.file_name(), 2) || !shard.path().is_dir() {
            continue;
        }
        for file in std::fs::read_dir(shard.path())?.flatten() {
            // Entry files are bare hex names; `.lock`, `.takeover` and `.tmp` files and anything else stay.
            if !hex_name(&file.file_name(), 64) {
                continue;
            }
            let path = file.path();
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let gone = parse(&text).is_some_and(|(key, _, expires)| {
                super::flushed(&key, prefix, keep) || !live(expires)
            });
            if gone {
                remove(&path)?;
            }
        }
    }
    Ok(())
}

impl CacheStore for FileStore {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        let (path, _) = self.path(false, key);
        let key = key.to_owned();
        Box::pin(async move {
            let limit = self.max_value;
            Self::blocking(move || {
                // The entry also holds its expiry and its key (JSON, at most 6 bytes a character): allow for both.
                let header = 64 + 6 * key.len();
                if let Ok(meta) = std::fs::metadata(&path)
                    && meta.len() > u64::try_from(limit.saturating_add(header)).unwrap_or(u64::MAX)
                {
                    return Err(super::too_large(meta.len(), limit));
                }
                Ok(read_live(&path, &key)?.map(|(value, _)| value))
            })
            .await
        })
    }

    fn put<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<()>> {
        let (path, _) = self.path(false, key);
        let (key, value) = (key.to_owned(), value.to_owned());
        Box::pin(async move {
            // Before the write: a caller's timeout during the sweep then fires before anything is stored.
            self.maybe_sweep().await;
            Self::blocking(move || write(&path, &key, &value, expires_at(ttl))).await
        })
    }

    fn add<'a>(
        &'a self,
        key: &'a str,
        value: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        let (path, stripe) = self.path(false, key);
        let (key, value) = (key.to_owned(), value.to_owned());
        Box::pin(async move {
            // Before the write: a caller's timeout during the sweep then fires before anything is stored, so an
            // `add` that stored its key is never reported as failed.
            self.maybe_sweep().await;
            self.guarded(path, stripe, move |path| {
                if read_live(path, &key)?.is_some() {
                    return Ok(false);
                }
                write(path, &key, &value, expires_at(ttl))?;
                Ok(true)
            })
            .await
        })
    }

    fn increment<'a>(&'a self, key: &'a str, by: i64) -> BoxFuture<'a, Result<i64>> {
        let (path, stripe) = self.path(false, key);
        let key = key.to_owned();
        Box::pin(async move {
            self.guarded(path, stripe, move |path| {
                let current = read_live(path, &key)?;
                let next = add_to(current.as_ref().map(|(v, _)| v.as_str()), by)?;
                let expires = current.and_then(|(_, e)| e);
                write(path, &key, &next.to_string(), expires)?;
                Ok(next)
            })
            .await
        })
    }

    fn forget<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<bool>> {
        let (path, stripe) = self.path(false, key);
        let key = key.to_owned();
        Box::pin(async move {
            self.guarded(path, stripe, move |path| {
                let was_live = read_live(path, &key)?.is_some();
                if read(path, &key)?.is_some() {
                    remove(path)?;
                }
                Ok(was_live)
            })
            .await
        })
    }

    fn flush<'a>(&'a self, prefix: &'a str, keep: &'a [String]) -> BoxFuture<'a, Result<()>> {
        let dir = Arc::clone(&self.dir);
        let prefix = prefix.to_owned();
        let keep = keep.to_vec();
        Box::pin(async move {
            Self::blocking(move || {
                flush_dir(&dir, &prefix, &keep)?;
                flush_dir(&dir.join("locks"), &prefix, &keep)
            })
            .await
        })
    }

    fn acquire_lock<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        let (path, stripe) = self.path(true, name);
        let (name, owner) = (name.to_owned(), owner.to_owned());
        Box::pin(async move {
            self.guarded(path, stripe, move |path| {
                if read_live(path, &name)?.is_some() {
                    return Ok(false);
                }
                write(path, &name, &owner, expires_at(ttl))?;
                Ok(true)
            })
            .await
        })
    }

    fn release_lock<'a>(&'a self, name: &'a str, owner: &'a str) -> BoxFuture<'a, Result<bool>> {
        let (path, stripe) = self.path(true, name);
        let (name, owner) = (name.to_owned(), owner.to_owned());
        Box::pin(async move {
            self.guarded(path, stripe, move |path| {
                let held = read_live(path, &name)?.is_some_and(|(o, _)| o == owner);
                if held {
                    remove(path)?;
                }
                Ok(held)
            })
            .await
        })
    }

    fn refresh_lock<'a>(
        &'a self,
        name: &'a str,
        owner: &'a str,
        ttl: Option<Duration>,
    ) -> BoxFuture<'a, Result<bool>> {
        let (path, stripe) = self.path(true, name);
        let (name, owner) = (name.to_owned(), owner.to_owned());
        Box::pin(async move {
            self.guarded(path, stripe, move |path| {
                let held = read_live(path, &name)?.is_some_and(|(o, _)| o == owner);
                if held {
                    write(path, &name, &owner, expires_at(ttl))?;
                }
                Ok(held)
            })
            .await
        })
    }

    fn force_release_lock<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<()>> {
        let (path, stripe) = self.path(true, name);
        Box::pin(async move {
            self.guarded(path, stripe, move |path| remove(path).map(|_| ()))
                .await
        })
    }

    fn lock_owner<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        let (path, _) = self.path(true, name);
        let name = name.to_owned();
        Box::pin(async move {
            Self::blocking(move || Ok(read_live(&path, &name)?.map(|(owner, _)| owner))).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|d| d.file_name().len() == 2 && d.path().is_dir())
            .map(|d| {
                std::fs::read_dir(d.path())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|f| f.path().extension().is_none())
                    .count()
            })
            .sum()
    }

    #[tokio::test]
    async fn get_refuses_a_large_entry_before_reading_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().to_path_buf()).max_value(100);
        store.put("big", &"x".repeat(500), None).await.unwrap();
        let err = store.get("big").await.unwrap_err();
        assert!(err.to_string().contains("CACHE_MAX_VALUE_BYTES"), "{err}");
        store.put("small", &"x".repeat(90), None).await.unwrap();
        assert!(store.get("small").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn flush_leaves_files_that_are_not_cache_entries() {
        // A CACHE_PATH pointed at `public/` by mistake.
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in [
            ("js/README", "not a cache entry"),
            (
                "js/app",
                "0\n\"key\"\nlooks like one, but the name is not a hash",
            ),
            ("ab/notes", "plain text"),
        ] {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        let store = FileStore::new(dir.path().to_path_buf());
        store.put("k", "1", None).await.unwrap();
        store.flush("", &[]).await.unwrap();
        assert_eq!(store.get("k").await.unwrap(), None);
        assert!(dir.path().join("js/README").exists());
        assert!(dir.path().join("js/app").exists());
        assert!(dir.path().join("ab/notes").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn entries_locks_and_folders_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("cache"));
        store.put("k", "1", None).await.unwrap();
        assert!(store.acquire_lock("l", "me", None).await.unwrap());
        let mut checked = 0;
        let mut stack = vec![dir.path().join("cache")];
        while let Some(path) = stack.pop() {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode & 0o077, 0, "{} is {mode:o}", path.display());
            checked += 1;
            if path.is_dir() {
                stack.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            }
        }
        assert!(checked >= 5, "{checked}");
    }

    /// Many waiters meet one stale lock file at once: still only one holds the key at a time.
    #[test]
    fn a_stale_lock_is_taken_over_by_one_waiter() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for round in 0..20 {
            let dir = tempfile::tempdir().unwrap();
            let entry = dir.path().join("ab").join("entry");
            std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
            let lock = entry.with_extension("lock");
            let stale = std::fs::File::create(&lock).unwrap();
            stale
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(120))
                .unwrap();
            drop(stale);
            let holders = Arc::new(AtomicUsize::new(0));
            let most = Arc::new(AtomicUsize::new(0));
            let start = Arc::new(std::sync::Barrier::new(12));
            let threads: Vec<_> = (0..12)
                .map(|_| {
                    let (entry, holders, most, start) = (
                        entry.clone(),
                        Arc::clone(&holders),
                        Arc::clone(&most),
                        Arc::clone(&start),
                    );
                    std::thread::spawn(move || {
                        start.wait();
                        let guard = LockFile::acquire(&entry).unwrap();
                        let now = holders.fetch_add(1, Ordering::SeqCst) + 1;
                        most.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(3));
                        holders.fetch_sub(1, Ordering::SeqCst);
                        drop(guard);
                    })
                })
                .collect();
            for t in threads {
                t.join().unwrap();
            }
            assert_eq!(most.load(Ordering::SeqCst), 1, "round {round}");
            assert!(!lock.exists(), "the last holder removed its lock");
        }
    }

    #[tokio::test]
    async fn writes_sweep_expired_entries_and_locks() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());
        for i in 0..5 {
            let key = format!("tick:{i}");
            assert!(
                store
                    .add(&key, "x", Some(Duration::from_millis(1)))
                    .await
                    .unwrap()
            );
        }
        assert!(
            store
                .acquire_lock("old-lock", "me", Some(Duration::from_millis(1)))
                .await
                .unwrap()
        );
        assert!(
            store
                .add("live", "x", Some(Duration::from_secs(60)))
                .await
                .unwrap()
        );
        assert_eq!(entries(dir.path()), 6);
        tokio::time::sleep(Duration::from_millis(20)).await;
        store
            .clean_always
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            store
                .add("tick:6", "x", Some(Duration::from_secs(60)))
                .await
                .unwrap()
        );
        assert_eq!(entries(dir.path()), 2, "the expired claims are gone");
        assert_eq!(
            entries(&dir.path().join("locks")),
            0,
            "and the expired lock"
        );
        assert_eq!(
            store.get("live").await.unwrap().as_deref(),
            Some("x"),
            "live entries stay"
        );
    }
}
