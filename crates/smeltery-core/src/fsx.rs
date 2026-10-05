//! File-system helpers for files the framework creates itself (logs, cache entries, the SQLite file): private
//! permissions on Unix, and the checks that keep a process running as root from following paths another user
//! controls.

use std::io;
use std::path::Path;

/// Create `dir` and its missing parents. On Unix every folder created gets `mode` (minus the umask, which can
/// only remove bits), so the framework's own folders are never readable by every local user.
pub(crate) fn create_dir_all(dir: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(mode)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::create_dir_all(dir)
    }
}

/// `options` with the Unix permission bits of a new file set to `mode` (no effect elsewhere).
pub(crate) fn file_mode(
    options: &mut std::fs::OpenOptions,
    mode: u32,
) -> &mut std::fs::OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(mode)
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        options
    }
}

/// Whether this process runs as root (effective user id 0).
#[cfg(unix)]
pub(crate) fn is_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// The user ids a root process trusts with the folders it writes into: root alone.
#[cfg(unix)]
pub(crate) const ROOT_ONLY: &[u32] = &[0];

/// Resolve the folder `dir` the way the kernel would, refusing it unless every folder on the way (and every
/// symlink followed) belongs to one of `trusted` and no folder is writable by group or others (a sticky folder
/// such as `/tmp` is allowed: there nobody can replace another user's entries). Missing folders are created
/// with `create` (`None`: a missing folder is an error).
///
/// A path that passes can only be changed by a `trusted` user, so opening a file in it afterwards cannot be
/// redirected by anyone else (no check-then-use race).
///
/// # Errors
/// A folder or link is not trusted, a component is not a folder, too many links, or an I/O error.
#[cfg(unix)]
pub(crate) fn trusted_dir(
    dir: &Path,
    trusted: &[u32],
    create: Option<u32>,
) -> io::Result<std::path::PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    use std::path::{Component, PathBuf};

    fn push_components(stack: &mut Vec<OsString>, path: &Path) {
        for c in path.components().rev() {
            match c {
                Component::Normal(name) => stack.push(name.to_owned()),
                Component::ParentDir => stack.push(OsString::from("..")),
                Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
            }
        }
    }
    let refuse = |path: &Path, why: &str| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} {why}", path.display()),
        )
    };
    let check_dir = |path: &Path, meta: &std::fs::Metadata| -> io::Result<()> {
        if !meta.is_dir() {
            return Err(refuse(path, "is not a folder"));
        }
        if !trusted.contains(&meta.uid()) {
            return Err(refuse(path, &format!("belongs to user id {}", meta.uid())));
        }
        let writable_by_others = meta.mode() & 0o022 != 0;
        let sticky = meta.mode() & 0o1000 != 0;
        if writable_by_others && !sticky {
            return Err(refuse(path, "is writable by group or others"));
        }
        Ok(())
    };

    let absolute = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(dir)
    };
    let mut pending = Vec::new();
    push_components(&mut pending, &absolute);
    let mut current = PathBuf::from("/");
    check_dir(&current, &std::fs::symlink_metadata(&current)?)?;
    let mut links = 0;
    while let Some(name) = pending.pop() {
        if name == ".." {
            current.pop();
            continue;
        }
        let next = current.join(&name);
        let meta = match std::fs::symlink_metadata(&next) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let Some(mode) = create else { return Err(e) };
                // `current` is trusted, so the new folder cannot be swapped by anyone else.
                std::fs::DirBuilder::new().mode(mode).create(&next)?;
                std::fs::symlink_metadata(&next)?
            }
            Err(e) => return Err(e),
        };
        if meta.file_type().is_symlink() {
            if !trusted.contains(&meta.uid()) {
                return Err(refuse(
                    &next,
                    &format!("is a link of user id {}", meta.uid()),
                ));
            }
            links += 1;
            if links > 40 {
                return Err(refuse(&next, "leads through too many links"));
            }
            let target = std::fs::read_link(&next)?;
            if target.is_absolute() {
                current = PathBuf::from("/");
            }
            push_components(&mut pending, &target);
            continue;
        }
        check_dir(&next, &meta)?;
        current = next;
    }
    Ok(current)
}

/// Tests: a temp folder only its owner may write to, whatever the umask (Ubuntu gives users `002`).
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
pub(crate) fn private_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// Tests: `create_dir_all`, then mode `0700` on the folder (not masked by the umask).
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
pub(crate) fn private_mkdir(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    /// Root and this test's own user: what root trusts, with this user standing in for root.
    #[allow(clippy::unwrap_used)]
    fn me() -> Vec<u32> {
        let probe = tempfile::NamedTempFile::new().unwrap();
        vec![0, probe.as_file().metadata().unwrap().uid()]
    }

    #[test]
    fn a_private_tree_is_trusted_and_missing_folders_are_created() {
        let dir = private_tempdir();
        let logs = dir.path().join("storage/logs");
        let resolved = trusted_dir(&logs, &me(), Some(0o750)).unwrap();
        assert!(resolved.is_dir());
        let mode = std::fs::metadata(&resolved).unwrap().permissions().mode();
        assert_eq!(mode & 0o027, 0, "{mode:o}");
    }

    #[test]
    fn a_folder_others_can_write_is_refused() {
        let dir = private_tempdir();
        let storage = dir.path().join("storage");
        std::fs::create_dir(&storage).unwrap();
        std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o777)).unwrap();
        let err = trusted_dir(&storage.join("logs"), &me(), Some(0o750)).unwrap_err();
        assert!(
            err.to_string().contains("writable by group or others"),
            "{err}"
        );
        assert!(
            !storage.join("logs").exists(),
            "nothing was created behind it"
        );
    }

    #[test]
    fn a_link_in_a_shared_folder_cannot_redirect_the_path() {
        let dir = private_tempdir();
        let elsewhere = dir.path().join("elsewhere");
        private_mkdir(&elsewhere);
        let storage = dir.path().join("storage");
        private_mkdir(&storage);
        std::os::unix::fs::symlink(&elsewhere, storage.join("logs")).unwrap();
        // While `storage` is private, the link (made by a trusted user) is followed.
        assert_eq!(
            trusted_dir(&storage.join("logs"), &me(), None).unwrap(),
            elsewhere.canonicalize().unwrap()
        );
        // Group-writable `storage`: another user could have placed that link.
        std::fs::set_permissions(&storage, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(trusted_dir(&storage.join("logs"), &me(), None).is_err());
    }

    #[test]
    fn a_folder_of_another_user_is_refused() {
        // Trust only a user id nobody has: the temp folder (or `/`) belongs to someone else.
        let dir = tempfile::tempdir().unwrap();
        let err = trusted_dir(dir.path(), &[u32::MAX - 7], None).unwrap_err();
        assert!(err.to_string().contains("belongs to user id"), "{err}");
    }
}
