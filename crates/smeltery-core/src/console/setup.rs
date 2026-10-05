//! The file-level setup commands: `key:generate` and `storage:link`.
//!
//! The app binary runs them as built-in commands, so a server needs no `smeltery` CLI; the
//! CLI calls the same functions. They work on files only and never build the app, so they
//! also work while `APP_KEY` is missing.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;

use crate::config::parse_env;
use crate::error::{Error, Result};

/// What `key:generate` prints when it keeps the existing key in production.
pub const KEY_KEPT_IN_PRODUCTION: &str = "The app runs in production (APP_ENV=production) and .env \
     already has an APP_KEY.\nA new key signs everyone out and invalidates encrypted cookies, Spark \
     state and the Watchfire API token.\nRun it again with --force to replace it.";

/// A new application key: `base64:` and 32 random bytes in standard base64.
///
/// ```
/// let key = smeltery_core::console::setup::generate_key().unwrap();
/// assert!(key.starts_with("base64:"));
/// assert_eq!(key.len(), "base64:".len() + 44);
/// ```
///
/// # Errors
/// The operating system has no random bytes to give.
pub fn generate_key() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| Error::internal(format!("cannot get random bytes: {e}")))?;
    Ok(format!(
        "base64:{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// Replace every `APP_KEY` line of the `.env` text `env` with `APP_KEY=<key>`, or append one
/// when there is none. Every other line is kept byte for byte, and the replaced line keeps its
/// own line ending (`\n` or `\r\n`).
///
/// ```
/// use smeltery_core::console::setup::set_app_key;
///
/// assert_eq!(set_app_key("APP_NAME=x\nAPP_KEY=\n", "k"), "APP_NAME=x\nAPP_KEY=k\n");
/// assert_eq!(set_app_key("APP_NAME=x", "k"), "APP_NAME=x\nAPP_KEY=k\n");
/// ```
pub fn set_app_key(env: &str, key: &str) -> String {
    let new_line = format!("APP_KEY={key}");
    let mut found = false;
    let mut out = String::with_capacity(env.len() + new_line.len() + 2);
    for line in env.split_inclusive('\n') {
        if is_key_line(line) {
            found = true;
            // A UTF-8 byte order mark before the first line stays in the file.
            if line.starts_with('\u{feff}') {
                out.push('\u{feff}');
            }
            out.push_str(&new_line);
            out.push_str(line_ending(line));
        } else {
            out.push_str(line);
        }
    }
    if !found {
        // Follow the file's own line endings when appending.
        let ending = if out.ends_with("\r\n") { "\r\n" } else { "\n" };
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(ending);
        }
        out.push_str(&new_line);
        out.push_str(ending);
    }
    out
}

/// Whether `line` assigns `APP_KEY` the way [`parse_env`] reads it (`APP_KEY=…`,
/// `APP_KEY = …`, `export APP_KEY=…`, or a bare `APP_KEY`).
fn is_key_line(line: &str) -> bool {
    let body = line.strip_prefix('\u{feff}').unwrap_or(line).trim();
    let body = body
        .strip_prefix("export ")
        .map(str::trim_start)
        .unwrap_or(body);
    match body.split_once('=') {
        Some((name, _)) => name.trim() == "APP_KEY",
        None => body == "APP_KEY",
    }
}

fn line_ending(line: &str) -> &'static str {
    if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

/// What [`write_app_key`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyWrite {
    /// The key was written into this `.env` file.
    Written(PathBuf),
    /// The key was written into this `.env` file in place, not by an atomic rename: the file
    /// belongs to another user and this process cannot give a new file that owner (print
    /// [`KEY_WRITTEN_IN_PLACE`]).
    WrittenInPlace(PathBuf),
    /// Production with an `APP_KEY` already in `.env` and no `force`: nothing was written
    /// (print [`KEY_KEPT_IN_PRODUCTION`]).
    KeptInProduction,
}

/// Write `key` as `APP_KEY` into `<root>/.env` (created when missing).
///
/// When `production` is true and `.env` already has a non-empty `APP_KEY`, nothing is written
/// unless `force` is true: a new key signs every user out.
///
/// The file is replaced atomically: the new text goes to a temporary file next to it (with the
/// old file's permissions and, on Unix, its owner and group; `0600` for a new `.env`), is
/// synced, and is renamed over it. When the owner cannot be kept (a file of another user,
/// written without the right to change owners), the file is rewritten in place instead, which
/// keeps its owner ([`KeyWrite::WrittenInPlace`]). When `.env` is a symlink, the file it points
/// to is replaced (or created, for a link to a missing file) and the link is kept.
///
/// # Errors
/// `.env` cannot be read or written.
pub fn write_app_key(root: &Path, key: &str, production: bool, force: bool) -> Result<KeyWrite> {
    let path = root.join(".env");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(Error::internal(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    let has_key = parse_env(&text)
        .iter()
        .any(|(k, v)| k == "APP_KEY" && !v.trim().is_empty());
    if production && has_key && !force {
        return Ok(KeyWrite::KeptInProduction);
    }
    let atomic = write_atomic(&path, &set_app_key(&text, key))
        .map_err(|e| Error::internal(format!("cannot write {}: {e}", path.display())))?;
    Ok(if atomic {
        KeyWrite::Written(path)
    } else {
        KeyWrite::WrittenInPlace(path)
    })
}

/// What `key:generate` adds after [`KeyWrite::WrittenInPlace`].
pub const KEY_WRITTEN_IN_PLACE: &str = ".env belongs to another user, so it was rewritten in place to keep its \
     owner (not replaced atomically).";

/// What `key:generate` prints after writing the key. When `APP_KEY` is also set in the process
/// environment, that value wins over `.env` (see [`env_value`](crate::config::env_value)), and the
/// message says so.
pub fn key_written_message() -> &'static str {
    if std::env::var_os("APP_KEY").is_some_and(|v| !v.is_empty()) {
        "APP_KEY written to .env. APP_KEY is also set in the process environment, which takes \
         precedence over .env: change it there for the new key to take effect."
    } else {
        "APP_KEY written to .env"
    }
}

/// The file a write to `path` changes: `path` itself, or the file a symlink at `path` points to
/// (also when that file does not exist yet).
fn write_target(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::canonicalize(path) {
            Ok(target) => Ok(target),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // A dangling link: create the file it names, relative to the link's folder.
                let link = std::fs::read_link(path)?;
                let dir = path.parent().unwrap_or(Path::new("."));
                let target = dir.join(link);
                if target.parent().is_some_and(Path::is_dir) {
                    Ok(target)
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!(
                            "{} is a symlink to {}, whose folder does not exist",
                            path.display(),
                            target.display()
                        ),
                    ))
                }
            }
            Err(e) => Err(e),
        },
        _ => Ok(path.to_path_buf()),
    }
}

/// Replace `path` with `contents` through a synced temporary file and a rename. Returns `false`
/// when the file was rewritten in place instead, because its owner could not be kept.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<bool> {
    let target = write_target(path)?;
    let dir = target.parent().unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".env".to_owned());
    let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
    let old = std::fs::metadata(&target).ok();
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        if let Some(old) = &old {
            if !keep_owner(&file, old) {
                return Ok(false);
            }
            file.set_permissions(old.permissions())?;
        }
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, &target).map(|()| true)
    })();
    if !matches!(result, Ok(true)) {
        let _ = std::fs::remove_file(&tmp);
    }
    match result {
        Ok(true) => Ok(true),
        // The owner cannot be kept: rewrite the file itself, which keeps its owner.
        Ok(false) => std::fs::write(&target, contents).map(|()| false),
        Err(e) => Err(e),
    }
}

/// Give `file` the owner and group of `old` (Unix). `false` when that is not allowed.
#[cfg(unix)]
fn keep_owner(file: &std::fs::File, old: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(new) = file.metadata() else {
        return false;
    };
    if new.uid() == old.uid() && new.gid() == old.gid() {
        return true;
    }
    std::os::unix::fs::fchown(file, Some(old.uid()), Some(old.gid())).is_ok()
}

/// Windows files have no Unix owner to keep.
#[cfg(not(unix))]
fn keep_owner(_file: &std::fs::File, _old: &std::fs::Metadata) -> bool {
    true
}

/// Whether the app in `root` runs in production: `APP_ENV` from the process environment, else
/// from `<root>/.env`, is `production` or missing (what [`Settings`](crate::config::Settings) reads,
/// for callers that have not loaded the settings, such as the `smeltery` CLI).
pub fn is_production_at(root: &Path) -> bool {
    let value = std::env::var("APP_ENV").ok().or_else(|| {
        let text = std::fs::read_to_string(root.join(".env")).ok()?;
        parse_env(&text)
            .into_iter()
            .rev()
            .find(|(k, _)| k == "APP_ENV")
            .map(|(_, v)| v)
    });
    // No trim: the same comparison as `Settings::is_production` (`parse_env` already trims).
    // A missing APP_ENV is production, as in `Settings::from_env`.
    value.is_none_or(|v| v == "production")
}

/// Link `<root>/public/storage` to `../storage/app/public` (a relative directory symlink), so
/// files in `storage/app/public` are served at `/storage/…`. Creates both folders when
/// missing.
///
/// On Windows, creating a symlink needs Developer Mode or an elevated shell.
///
/// Running it again when the link is there is not an error ([`StorageLink::AlreadyLinked`]).
///
/// # Errors
/// `public/storage` exists and is not a link to `storage/app/public`, or the link cannot be
/// created.
pub fn link_storage(root: &Path) -> Result<StorageLink> {
    #[cfg(unix)]
    if crate::fsx::is_root() {
        return link_storage_trusted(root, crate::fsx::ROOT_ONLY);
    }
    let link = root.join("public").join("storage");
    let public = root.join("storage").join("app").join("public");
    let target = Path::new("..").join("storage").join("app").join("public");
    if let Some(done) = existing_link(&link, &public, &target)? {
        return Ok(done);
    }
    std::fs::create_dir_all(&public)?;
    std::fs::create_dir_all(root.join("public"))?;
    symlink_dir(&target, &link).map_err(|e| Error::internal(link_error(&link, &e)))?;
    Ok(StorageLink::Linked)
}

/// What is at `link` already: `None` when nothing, [`StorageLink::AlreadyLinked`] for the link itself (also
/// while `storage/app/public` does not exist yet), an error for anything else.
fn existing_link(link: &Path, public: &Path, target: &Path) -> Result<Option<StorageLink>> {
    let Ok(meta) = link.symlink_metadata() else {
        return Ok(None);
    };
    let same = match (std::fs::canonicalize(link), std::fs::canonicalize(public)) {
        (Ok(a), Ok(b)) => a == b,
        _ => std::fs::read_link(link).is_ok_and(|t| t == target),
    };
    if same && meta.file_type().is_symlink() {
        return Ok(Some(StorageLink::AlreadyLinked));
    }
    Err(Error::internal(format!(
        "{} already exists and is not a link to storage/app/public",
        link.display()
    )))
}

/// `storage:link` run as root (`sudo`): the link goes into `public/` only when no other user controls that
/// folder or the folders above it, and `storage/app/public` is created only under the same condition. In the
/// usual server layout `storage/` belongs to the app user, so root leaves it alone (the app creates
/// `storage/app/public` itself with its first public upload) instead of creating folders through paths that
/// user could have pointed elsewhere with a symlink.
#[cfg(unix)]
fn link_storage_trusted(root: &Path, trusted: &[u32]) -> Result<StorageLink> {
    let refuse = |e: std::io::Error| {
        Error::internal(format!(
            "storage:link as root: {e}; create the link as the owner of public/ instead"
        ))
    };
    let public_dir =
        crate::fsx::trusted_dir(&root.join("public"), trusted, Some(0o755)).map_err(refuse)?;
    let link = public_dir.join("storage");
    let target = Path::new("..").join("storage").join("app").join("public");
    let storage_public = root.join("storage").join("app").join("public");
    if let Some(done) = existing_link(&link, &storage_public, &target)? {
        return Ok(done);
    }
    // Only where root alone controls the path; otherwise the folder is the app user's business.
    let _ = crate::fsx::trusted_dir(&storage_public, trusted, Some(0o755));
    symlink_dir(&target, &link).map_err(|e| Error::internal(link_error(&link, &e)))?;
    Ok(StorageLink::Linked)
}

/// What [`link_storage`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StorageLink {
    /// The link was created.
    Linked,
    /// `public/storage` already linked to `storage/app/public`; nothing changed.
    AlreadyLinked,
}

impl StorageLink {
    /// The line `storage:link` prints.
    pub fn message(self) -> &'static str {
        match self {
            StorageLink::Linked => "Linked public/storage to storage/app/public",
            StorageLink::AlreadyLinked => {
                "public/storage already links to storage/app/public; nothing changed"
            }
        }
    }
}

/// Windows' `ERROR_PRIVILEGE_NOT_HELD`: creating a symlink without Developer Mode or elevation.
const PRIVILEGE_NOT_HELD: i32 = 1314;

/// The message for a failed `storage:link`, naming the fix when Windows refused the privilege.
fn link_error(link: &Path, e: &std::io::Error) -> String {
    let mut message = format!("cannot create the link {}: {e}", link.display());
    if cfg!(windows) && e.raw_os_error() == Some(PRIVILEGE_NOT_HELD) {
        message
            .push_str("; creating a symlink on Windows needs Developer Mode or an elevated shell");
    }
    message
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(not(any(unix, windows)))]
fn symlink_dir(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlinks are not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_are_32_bytes_and_differ() {
        let a = generate_key().unwrap();
        let b = generate_key().unwrap();
        assert!(a.starts_with("base64:"));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(a.trim_start_matches("base64:"))
            .unwrap();
        assert_eq!(decoded.len(), 32);
        assert_ne!(a, b);
    }

    #[test]
    fn replaces_an_existing_key_line() {
        let env = "APP_NAME=x\nAPP_KEY=\nAPP_DEBUG=true\n";
        assert_eq!(
            set_app_key(env, "base64:k"),
            "APP_NAME=x\nAPP_KEY=base64:k\nAPP_DEBUG=true\n"
        );
        assert_eq!(
            set_app_key("APP_KEY=base64:old\n", "base64:new"),
            "APP_KEY=base64:new\n"
        );
        assert_eq!(set_app_key("export APP_KEY=old", "k"), "APP_KEY=k");
    }

    #[test]
    fn appends_when_missing() {
        assert_eq!(set_app_key("APP_NAME=x", "k"), "APP_NAME=x\nAPP_KEY=k\n");
        assert_eq!(set_app_key("APP_NAME=x\n", "k"), "APP_NAME=x\nAPP_KEY=k\n");
        assert_eq!(set_app_key("", "k"), "APP_KEY=k\n");
        // A key mentioned in a comment or another key is not the key line.
        assert_eq!(
            set_app_key("# APP_KEY=x\nMY_APP_KEY=y\n", "k"),
            "# APP_KEY=x\nMY_APP_KEY=y\nAPP_KEY=k\n"
        );
    }

    #[test]
    fn writes_into_env_and_creates_it() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join(".env");
        std::fs::write(&env, "APP_NAME=x\nAPP_KEY=\n").unwrap();
        assert_eq!(
            write_app_key(dir.path(), "base64:k", false, false).unwrap(),
            KeyWrite::Written(env.clone())
        );
        assert_eq!(
            std::fs::read_to_string(&env).unwrap(),
            "APP_NAME=x\nAPP_KEY=base64:k\n"
        );
        // Outside production an existing key is replaced.
        write_app_key(dir.path(), "base64:k2", false, false).unwrap();
        assert!(
            std::fs::read_to_string(&env)
                .unwrap()
                .contains("APP_KEY=base64:k2\n")
        );

        let fresh = tempfile::tempdir().unwrap();
        write_app_key(fresh.path(), "base64:k", true, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(fresh.path().join(".env")).unwrap(),
            "APP_KEY=base64:k\n"
        );
    }

    #[test]
    fn production_keeps_an_existing_key_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join(".env");
        let before = "APP_ENV=production\nAPP_KEY=base64:old\n";
        std::fs::write(&env, before).unwrap();
        assert_eq!(
            write_app_key(dir.path(), "base64:new", true, false).unwrap(),
            KeyWrite::KeptInProduction
        );
        assert_eq!(std::fs::read_to_string(&env).unwrap(), before);
        assert!(matches!(
            write_app_key(dir.path(), "base64:new", true, true).unwrap(),
            KeyWrite::Written(_)
        ));
        assert_eq!(
            std::fs::read_to_string(&env).unwrap(),
            "APP_ENV=production\nAPP_KEY=base64:new\n"
        );
        // An empty key in production is filled without --force.
        std::fs::write(&env, "APP_ENV=production\nAPP_KEY=\n").unwrap();
        assert!(matches!(
            write_app_key(dir.path(), "base64:k", true, false).unwrap(),
            KeyWrite::Written(_)
        ));
    }

    #[test]
    fn production_is_read_from_the_env_file() {
        // The process environment wins; skip when this test process has APP_ENV set.
        if std::env::var_os("APP_ENV").is_some() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // No `.env`, or one without APP_ENV: production, like `Settings`.
        assert!(is_production_at(dir.path()));
        std::fs::write(
            dir.path().join(".env"),
            "APP_NAME=x
",
        )
        .unwrap();
        assert!(is_production_at(dir.path()));
        std::fs::write(dir.path().join(".env"), "APP_ENV=local\n").unwrap();
        assert!(!is_production_at(dir.path()));
        std::fs::write(dir.path().join(".env"), "APP_ENV=production\n").unwrap();
        assert!(is_production_at(dir.path()));
    }

    #[test]
    fn link_storage_refuses_an_existing_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("public/storage")).unwrap();
        let err = link_storage(dir.path()).unwrap_err().to_string();
        assert!(
            err.contains("already exists and is not a link to storage/app/public"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn link_storage_creates_a_relative_link_and_accepts_a_second_run() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(link_storage(dir.path()).unwrap(), StorageLink::Linked);
        let link = dir.path().join("public/storage");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("../storage/app/public")
        );
        std::fs::write(dir.path().join("storage/app/public/a.txt"), "hi").unwrap();
        assert_eq!(std::fs::read_to_string(link.join("a.txt")).unwrap(), "hi");
        // A second run is not an error.
        assert_eq!(
            link_storage(dir.path()).unwrap(),
            StorageLink::AlreadyLinked
        );
        // A link somewhere else is.
        std::fs::remove_file(&link).unwrap();
        std::fs::create_dir(dir.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink("../elsewhere", &link).unwrap();
        assert!(link_storage(dir.path()).is_err());
    }

    /// Root and this test's own user, which stands in for root.
    #[cfg(unix)]
    fn trusted() -> Vec<u32> {
        use std::os::unix::fs::MetadataExt as _;
        let probe = tempfile::NamedTempFile::new().unwrap();
        vec![0, probe.as_file().metadata().unwrap().uid()]
    }

    #[cfg(unix)]
    #[test]
    fn as_root_storage_link_never_creates_folders_in_the_app_users_storage() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::fsx::private_tempdir();
        let storage = dir.path().join("storage");
        std::fs::create_dir(&storage).unwrap();
        // Writable by others: from root's side, a folder the app user controls (`app` could be a link to `/etc`).
        std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            link_storage_trusted(dir.path(), &trusted()).unwrap(),
            StorageLink::Linked
        );
        assert!(
            !storage.join("app").exists(),
            "nothing created under storage/"
        );
        let link = dir.path().join("public/storage");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("../storage/app/public")
        );
        // Again, with the folder still missing: the link is recognised.
        assert_eq!(
            link_storage_trusted(dir.path(), &trusted()).unwrap(),
            StorageLink::AlreadyLinked
        );
        assert_eq!(
            link_storage(dir.path()).unwrap(),
            StorageLink::AlreadyLinked
        );
    }

    #[cfg(unix)]
    #[test]
    fn as_root_storage_link_refuses_a_public_folder_others_control() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::fsx::private_tempdir();
        std::fs::create_dir(dir.path().join("public")).unwrap();
        std::fs::set_permissions(
            dir.path().join("public"),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        let err = link_storage_trusted(dir.path(), &trusted())
            .unwrap_err()
            .to_string();
        assert!(err.contains("writable by group or others"), "{err}");
        assert!(!dir.path().join("public/storage").exists());
        // In a private tree both folders and the link are made.
        let ok = crate::fsx::private_tempdir();
        assert_eq!(
            link_storage_trusted(ok.path(), &trusted()).unwrap(),
            StorageLink::Linked
        );
        assert!(ok.path().join("storage/app/public").is_dir());
    }

    /// On every OS where this process may create symlinks (Windows: Developer Mode or elevation).
    #[test]
    fn a_second_storage_link_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        match link_storage(dir.path()) {
            Ok(first) => assert_eq!(first, StorageLink::Linked),
            Err(e) if e.to_string().contains("Developer Mode") => {
                eprintln!("skipped: this Windows session may not create symlinks ({e})");
                return;
            }
            Err(e) => panic!("{e}"),
        }
        assert_eq!(
            link_storage(dir.path()).unwrap(),
            StorageLink::AlreadyLinked
        );
        assert!(
            StorageLink::AlreadyLinked
                .message()
                .contains("already links")
        );
    }

    #[test]
    fn a_key_line_after_a_bom_is_replaced_and_the_bom_kept() {
        assert_eq!(
            set_app_key("\u{feff}APP_KEY=old\nB=2\n", "k"),
            "\u{feff}APP_KEY=k\nB=2\n"
        );
        assert_eq!(
            set_app_key("\u{feff}A=1\n", "k"),
            "\u{feff}A=1\nAPP_KEY=k\n"
        );
    }

    #[test]
    fn the_key_line_keeps_its_crlf_ending() {
        assert_eq!(
            set_app_key("A=1\r\nAPP_KEY=\r\nB=2\r\n", "k"),
            "A=1\r\nAPP_KEY=k\r\nB=2\r\n"
        );
        // Appending follows the file's own endings.
        assert_eq!(set_app_key("A=1\r\n", "k"), "A=1\r\nAPP_KEY=k\r\n");
    }

    #[test]
    fn a_key_line_with_spaces_is_replaced_not_duplicated() {
        assert_eq!(set_app_key("APP_KEY = old\nB=2\n", "k"), "APP_KEY=k\nB=2\n");
        assert_eq!(set_app_key("  export APP_KEY =old\n", "k"), "APP_KEY=k\n");
    }

    #[test]
    fn production_ignores_surrounding_spaces_like_settings() {
        if std::env::var_os("APP_ENV").is_some() {
            return;
        }
        // `parse_env` trims values, so `.env` spaces do not matter.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "APP_ENV = production \n").unwrap();
        assert!(is_production_at(dir.path()));
    }

    #[test]
    fn the_write_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "A=1\n").unwrap();
        write_app_key(dir.path(), "k", false, false).unwrap();
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [".env"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_new_env_is_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        write_app_key(dir.path(), "k", false, false).unwrap();
        let mode = std::fs::metadata(dir.path().join(".env"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // An existing file keeps its permissions.
        std::fs::set_permissions(
            dir.path().join(".env"),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        write_app_key(dir.path(), "k2", false, false).unwrap();
        let mode = std::fs::metadata(dir.path().join(".env"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o640);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_env_stays_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::write(shared.join("env"), "A=1\nAPP_KEY=\n").unwrap();
        let app = dir.path().join("app");
        std::fs::create_dir(&app).unwrap();
        std::os::unix::fs::symlink("../shared/env", app.join(".env")).unwrap();
        write_app_key(&app, "k", false, false).unwrap();
        assert!(app.join(".env").symlink_metadata().unwrap().is_symlink());
        assert_eq!(
            std::fs::read_to_string(shared.join("env")).unwrap(),
            "A=1\nAPP_KEY=k\n"
        );
    }

    #[test]
    fn a_refused_windows_symlink_names_the_fix() {
        let link = Path::new("public/storage");
        let denied = std::io::Error::from_raw_os_error(PRIVILEGE_NOT_HELD);
        let message = link_error(link, &denied);
        assert!(message.starts_with("cannot create the link"), "{message}");
        assert_eq!(
            message.contains("Developer Mode or an elevated shell"),
            cfg!(windows),
            "{message}"
        );
        let other = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(!link_error(link, &other).contains("Developer Mode"));
    }

    #[cfg(unix)]
    #[test]
    fn the_owner_group_and_mode_are_kept() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join(".env");
        std::fs::write(&env, "A=1\n").unwrap();
        std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o640)).unwrap();
        // Give the file another of this user's groups when there is one, so a new file (which
        // gets the default group) would differ.
        let default_gid = std::fs::metadata(&env).unwrap().gid();
        let other_gid = std::process::Command::new("id")
            .arg("-G")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|ids| {
                ids.split_whitespace()
                    .filter_map(|g| g.parse::<u32>().ok())
                    .find(|g| *g != default_gid)
            });
        if let Some(gid) = other_gid {
            std::os::unix::fs::chown(&env, None, Some(gid)).unwrap();
        }
        let before = std::fs::metadata(&env).unwrap();
        assert!(matches!(
            write_app_key(dir.path(), "k", false, false).unwrap(),
            KeyWrite::Written(_)
        ));
        let after = std::fs::metadata(&env).unwrap();
        assert_eq!(
            (after.uid(), after.gid(), after.mode() & 0o777),
            (before.uid(), before.gid(), 0o640)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_env_symlink_creates_its_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("shared")).unwrap();
        let app = dir.path().join("app");
        std::fs::create_dir(&app).unwrap();
        std::os::unix::fs::symlink("../shared/env", app.join(".env")).unwrap();
        write_app_key(&app, "k", false, false).unwrap();
        assert!(app.join(".env").symlink_metadata().unwrap().is_symlink());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("shared/env")).unwrap(),
            "APP_KEY=k\n"
        );
        // A link into a missing folder is an error that names the link.
        std::fs::remove_file(app.join(".env")).unwrap();
        std::os::unix::fs::symlink("../nowhere/env", app.join(".env")).unwrap();
        let err = write_app_key(&app, "k", false, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("is a symlink to"), "{err}");
    }
}
