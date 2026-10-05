//! File writes that never follow a planted link and never leave a half-written file (D-352).
//!
//! - [`create_new`] creates a file that must not exist yet: `O_CREAT | O_EXCL`, so any existing entry (a file, a
//!   directory, a symlink, a dangling symlink) makes it fail instead of writing through it.
//! - [`replace`] rewrites an existing file through a synced temporary file in the same folder and a rename, keeping
//!   the file's permissions; a crash or a full disk leaves the old contents.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Who may read a file [`create_new`] makes (on Unix; elsewhere the platform default applies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The default for new files (`0o666` minus the umask).
    Default,
    /// Owner only (`0o600`): `.env`, which holds `APP_KEY`.
    Private,
}

/// Creates `path` with `contents`; fails when anything exists at `path`, a dangling symlink included.
pub(crate) fn create_new(path: &Path, contents: &[u8], mode: Mode) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        if mode == Mode::Private {
            options.mode(0o600);
        }
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    Ok(())
}

/// The file a write to `path` changes: `path`, or the file a symlink at `path` points to (the link stays).
fn target_of(path: &Path) -> std::io::Result<PathBuf> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        std::fs::canonicalize(path)
    } else {
        Ok(path.to_path_buf())
    }
}

/// Replaces the existing file `path` with `contents`: a temporary file next to it (created with `create_new`, given
/// the old file's permissions, synced), then a rename over it. A symlink at `path` is followed and kept. When the
/// temporary file would get another owner than the old file (Unix), the file is rewritten in place instead, so its
/// owner stays.
pub(crate) fn replace(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let target = target_of(path)?;
    let old = std::fs::metadata(&target)?;
    let dir = target.parent().unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.smeltery-tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        if !same_owner(&file, &old) {
            return Ok(false);
        }
        file.set_permissions(old.permissions())?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, &target).map(|()| true)
    })();
    if !matches!(result, Ok(true)) {
        let _ = std::fs::remove_file(&tmp);
    }
    match result {
        Ok(true) => Ok(()),
        Ok(false) => std::fs::write(&target, contents),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn same_owner(file: &std::fs::File, old: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    file.metadata()
        .is_ok_and(|new| new.uid() == old.uid() && new.gid() == old.gid())
}

#[cfg(not(unix))]
fn same_owner(_file: &std::fs::File, _old: &std::fs::Metadata) -> bool {
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap_or_else(|e| unreachable!("tempdir: {e}"))
    }

    /// A file symlink at `link` to `target`; `false` (with a note) where the OS refuses one (Windows without
    /// Developer Mode or admin rights).
    pub(crate) fn symlink_file(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(target, link);
        match made {
            Ok(()) => true,
            Err(e) => {
                eprintln!("SKIPPED: cannot create a symlink here ({e})");
                false
            }
        }
    }

    #[test]
    fn create_new_refuses_any_existing_entry() {
        let dir = tmp();
        let path = dir.path().join("a.rs");
        assert!(create_new(&path, b"one", Mode::Default).is_ok());
        assert!(create_new(&path, b"two", Mode::Default).is_err());
        assert_eq!(std::fs::read_to_string(&path).ok().as_deref(), Some("one"));
    }

    /// A dangling symlink is not "missing": writing through it would create or truncate the file it names.
    #[test]
    fn create_new_does_not_follow_a_dangling_symlink() {
        let dir = tmp();
        let victim = dir.path().join("victim.txt");
        let link = dir.path().join("post.rs");
        if !symlink_file(&victim, &link) {
            return;
        }
        assert!(!link.exists(), "a dangling link: exists() is false");
        assert!(create_new(&link, b"generated", Mode::Default).is_err());
        assert!(!victim.exists(), "nothing was written through the link");
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tmp();
        let path = dir.path().join(".env");
        assert!(create_new(&path, b"APP_KEY=x\n", Mode::Private).is_ok());
        let mode = std::fs::metadata(&path)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or_default();
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn replace_rewrites_and_leaves_no_temporary_file() {
        let dir = tmp();
        let path = dir.path().join("web.rs");
        assert!(std::fs::write(&path, "old").is_ok());
        assert!(replace(&path, b"new").is_ok());
        assert_eq!(std::fs::read_to_string(&path).ok().as_deref(), Some("new"));
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .map(|r| {
                r.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(names, ["web.rs"]);
        assert!(replace(&dir.path().join("missing.rs"), b"x").is_err());
    }

    #[test]
    fn replace_writes_through_a_symlink_and_keeps_it() {
        let dir = tmp();
        let real = dir.path().join("real.rs");
        assert!(std::fs::write(&real, "old").is_ok());
        let link = dir.path().join("mod.rs");
        if !symlink_file(&real, &link) {
            return;
        }
        assert!(replace(&link, b"new").is_ok());
        assert!(std::fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()));
        assert_eq!(std::fs::read_to_string(&real).ok().as_deref(), Some("new"));
    }

    #[cfg(unix)]
    #[test]
    fn replace_keeps_permissions_and_symlinks() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tmp();
        let real = dir.path().join("real.rs");
        assert!(std::fs::write(&real, "old").is_ok());
        assert!(std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).is_ok());
        let link = dir.path().join("mod.rs");
        assert!(symlink_file(&real, &link));
        assert!(replace(&link, b"new").is_ok());
        assert!(
            std::fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()),
            "the link stays a link"
        );
        assert_eq!(std::fs::read_to_string(&real).ok().as_deref(), Some("new"));
        let mode = std::fs::metadata(&real)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or_default();
        assert_eq!(mode, 0o640);
    }
}
