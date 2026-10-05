//! The Tailwind CSS standalone binary: where `serve` and `build` find it, and `smeltery tailwind:install` (also run by
//! `smeltery new`), which downloads one pinned release, checks its SHA-256 and keeps it in a per-user folder.
//!
//! Bumping Tailwind is a code change here: set [`VERSION`], copy the new `sha256sums.txt` lines of the release into
//! [`ASSETS`], update the version in `.github/workflows/ci.yml`, and regenerate the prebuilt CSS (D-206).

use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use sha2::{Digest as _, Sha256};

use crate::ui::{Badge, Ui};

/// The pinned Tailwind CSS release.
pub(crate) const VERSION: &str = "4.3.3";

/// Where the release files live; `{VERSION}` is appended as `v<VERSION>/`.
const RELEASES: &str = "https://github.com/tailwindlabs/tailwindcss/releases/download";

/// One release file and its SHA-256, from the release's `sha256sums.txt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Asset {
    pub(crate) name: &'static str,
    pub(crate) sha256: &'static str,
}

/// Every platform the pinned release has a standalone binary for.
pub(crate) const ASSETS: &[(&str, &str, bool, Asset)] = &[
    // (os, arch, musl, asset)
    (
        "linux",
        "aarch64",
        false,
        Asset {
            name: "tailwindcss-linux-arm64",
            sha256: "55fd0b241214eff3de1e8ee4f22796662f2d2e7a49bcfca7477cfd0bac398195",
        },
    ),
    (
        "linux",
        "aarch64",
        true,
        Asset {
            name: "tailwindcss-linux-arm64-musl",
            sha256: "71ea4be79c9de9827545682df3e040053fb535d37c71ed2cfdedf9385a0868e0",
        },
    ),
    (
        "linux",
        "x86_64",
        false,
        Asset {
            name: "tailwindcss-linux-x64",
            sha256: "dc61b3ac6b8c9ca874c0cc4c57b2409791a64c5540404ca5f5367360babc313a",
        },
    ),
    (
        "linux",
        "x86_64",
        true,
        Asset {
            name: "tailwindcss-linux-x64-musl",
            sha256: "a04d34ceacc8f52cbe8920ad846cdeb61d3d0021dba32db0d1f77c9d9fad7a6c",
        },
    ),
    (
        "macos",
        "aarch64",
        false,
        Asset {
            name: "tailwindcss-macos-arm64",
            sha256: "cdf646702987a743464dff4d9c60fd4480d1c1e73dd819a9a67f1078815dce9d",
        },
    ),
    (
        "macos",
        "x86_64",
        false,
        Asset {
            name: "tailwindcss-macos-x64",
            sha256: "7922e0953f2110c05976e3bf58f14e643d90427575e766b7d433f5f80cbee7e1",
        },
    ),
    (
        "windows",
        "x86_64",
        false,
        Asset {
            name: "tailwindcss-windows-x64.exe",
            sha256: "e0e260ce048014e9268f6237ff18f8ccf02cef521cbd0ae04e82c2cdf7aa3955",
        },
    ),
];

/// The release file for `os` / `arch` (`std::env::consts` names); `musl` picks the musl build on Linux.
pub(crate) fn asset_for(os: &str, arch: &str, musl: bool) -> Option<Asset> {
    let musl = musl && os == "linux";
    ASSETS
        .iter()
        .find(|(o, a, m, _)| *o == os && *a == arch && *m == musl)
        .map(|(_, _, _, asset)| *asset)
}

/// The release file for the platform this CLI was built for.
fn current_asset() -> Option<Asset> {
    asset_for(
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(target_env = "musl"),
    )
}

/// The installed binary's file name: the version is in the name, so a bump never reuses an old binary.
pub(crate) fn file_name() -> String {
    format!("tailwindcss-v{VERSION}{}", std::env::consts::EXE_SUFFIX)
}

/// The per-user folder for Smeltery's tools:
/// - Windows: `%LOCALAPPDATA%\smeltery\bin`
/// - macOS: `~/Library/Application Support/smeltery/bin`
/// - other: `$XDG_DATA_HOME/smeltery/bin`, else `~/.local/share/smeltery/bin`
pub(crate) fn user_bin_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let base = if cfg!(windows) {
        PathBuf::from(var("LOCALAPPDATA")?)
    } else if cfg!(target_os = "macos") {
        PathBuf::from(var("HOME")?)
            .join("Library")
            .join("Application Support")
    } else {
        match var("XDG_DATA_HOME").map(PathBuf::from) {
            Some(xdg) if xdg.is_absolute() => xdg,
            _ => PathBuf::from(var("HOME")?).join(".local").join("share"),
        }
    };
    Some(base.join("smeltery").join("bin"))
}

/// The installed binary, when `smeltery tailwind:install` (or `smeltery new`) put it there and it passes
/// [`verify_installed`]; otherwise `None`, with a warning when a binary is there but fails the checks.
fn user_binary() -> Option<PathBuf> {
    let dir = user_bin_dir()?;
    let asset = current_asset()?;
    // The user's data folder (`~/.local/share`, `$XDG_DATA_HOME`, ...) that `smeltery/bin` sits in.
    let base = dir.parent().and_then(Path::parent);
    match verify_installed(&dir, &file_name(), asset.sha256, base) {
        Ok(found) => found,
        Err(why) => {
            eprintln!(
                "smeltery: warning: not running {}: {why}; run `smeltery tailwind:install` to replace it",
                dir.join(file_name()).display()
            );
            None
        }
    }
}

/// Checks the per-user binary `dir/file_name` before it runs (S6-13, D-357): `Ok(None)` when there is none,
/// `Ok(Some(path))` when it is the pinned release, `Err(reason)` otherwise.
///
/// - On Unix the file, `dir` and every folder between `dir` and `base` (the user's data folder: `smeltery/`) must
///   belong to this user and not be writable by the group or others; `base` itself must belong to this user or root
///   and not be writable by others (group write is allowed there: with per-user groups a umask of 002 makes the
///   user's own folders group-writable). So no other account can swap the binary or a folder on its path
///   (`XDG_DATA_HOME` may point anywhere). Without `base` only `dir` is checked.
/// - Its SHA-256 must be `sha256`. Hashing 110 MB on every `serve` and `build` is avoidable: after a check passes, a
///   stamp file next to the binary records its size, modification time and hash, and the hash is computed again only
///   when the size or the time changed.
pub(crate) fn verify_installed(
    dir: &Path,
    file_name: &str,
    sha256: &str,
    base: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    let path = dir.join(file_name);
    if !path.is_file() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        private_to_me(&path)?;
        let mut folder = dir;
        loop {
            if Some(folder) == base {
                not_shared(folder)?;
                break;
            }
            private_to_me(folder)?;
            match folder.parent() {
                Some(parent) if base.is_some_and(|b| parent.starts_with(b)) => folder = parent,
                _ => break,
            }
        }
    }
    #[cfg(not(unix))]
    let _ = base;
    let stamp = dir.join(format!("{file_name}.verified"));
    let fingerprint = fingerprint(&path).map_err(|e| format!("cannot read it: {e}"))?;
    let expected = format!("{fingerprint} {sha256}\n");
    if std::fs::read_to_string(&stamp).is_ok_and(|s| s == expected) {
        return Ok(Some(path));
    }
    let actual = sha256_file(&path).map_err(|e| format!("cannot read it: {e}"))?;
    if actual != sha256 {
        return Err(format!(
            "its SHA-256 is {actual}, not the pinned release's {sha256}"
        ));
    }
    // Best effort: without the stamp the next run hashes again.
    let _ = write_stamp(&stamp, &expected);
    Ok(Some(path))
}

/// `<size> <modification time in nanoseconds>` of `path`.
fn fingerprint(path: &Path) -> std::io::Result<String> {
    let meta = std::fs::metadata(path)?;
    let modified = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    Ok(format!("{} {modified}", meta.len()))
}

/// Replaces the stamp file through a temporary file and a rename.
fn write_stamp(stamp: &Path, text: &str) -> std::io::Result<()> {
    let tmp = stamp.with_extension(format!("verified.{}.tmp", std::process::id()));
    let result = crate::files::create_new(&tmp, text.as_bytes(), crate::files::Mode::Private)
        .and_then(|()| std::fs::rename(&tmp, stamp));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `Err` when `path` is not owned by this process's user or is writable by its group or others.
#[cfg(unix)]
fn private_to_me(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let meta =
        std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let me = my_uid().map_err(|e| format!("cannot tell this user's id: {e}"))?;
    if meta.uid() != me {
        return Err(format!(
            "{} belongs to another user (uid {})",
            path.display(),
            meta.uid()
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!(
            "{} is writable by other users (mode {:o})",
            path.display(),
            meta.mode() & 0o777
        ));
    }
    Ok(())
}

/// `Err` when `path` belongs to neither this process's user nor root, or others may write to it.
#[cfg(unix)]
fn not_shared(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let meta =
        std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let me = my_uid().map_err(|e| format!("cannot tell this user's id: {e}"))?;
    if meta.uid() != me && meta.uid() != 0 {
        return Err(format!(
            "{} belongs to another user (uid {})",
            path.display(),
            meta.uid()
        ));
    }
    if meta.mode() & 0o002 != 0 {
        return Err(format!(
            "{} is writable by other users (mode {:o})",
            path.display(),
            meta.mode() & 0o777
        ));
    }
    Ok(())
}

/// Removes the group and other write bits of `path` when it has them (nothing when it does not exist).
#[cfg(unix)]
fn not_writable_by_others(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    let mode = meta.permissions().mode();
    if mode & 0o022 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & !0o022))
            .with_context(|| format!("cannot change the permissions of {}", path.display()))?;
    }
    Ok(())
}

/// The user id new files of this process get: the owner of a file it creates. (Without `unsafe` or a new dependency
/// there is no `geteuid`; a file in the temp folder is created and removed.)
#[cfg(unix)]
fn my_uid() -> std::io::Result<u32> {
    use std::os::unix::fs::MetadataExt as _;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let probe = std::env::temp_dir().join(format!(".smeltery-uid-{}-{nanos}", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe);
    let uid = file.and_then(|f| f.metadata()).map(|m| m.uid());
    let _ = std::fs::remove_file(&probe);
    uid
}

/// The Tailwind binary `serve` and `build` run: `TAILWIND_BIN`, then the per-user install, then `tailwindcss` on
/// `PATH` (absolute entries only, see [`path_candidates`]).
pub(crate) fn find(
    env_bin: Option<OsString>,
    user_bin: Option<PathBuf>,
    path: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(bin) = env_bin.filter(|b| !b.is_empty()) {
        return Some(PathBuf::from(bin));
    }
    if let Some(bin) = user_bin {
        return Some(bin);
    }
    path_candidates(&path?).into_iter().find(|p| p.is_file())
}

/// Where `tailwindcss` may be on `path`, in order. Empty, `.` and other relative entries are skipped: they name the
/// current folder, which is the app (S6-13), and a repository could ship its own `tailwindcss` there.
pub(crate) fn path_candidates(path: &std::ffi::OsStr) -> Vec<PathBuf> {
    let exe = format!("tailwindcss{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(&exe))
        .collect()
}

/// [`find`] with this process's environment.
pub(crate) fn from_env() -> Option<PathBuf> {
    find(
        std::env::var_os("TAILWIND_BIN"),
        user_binary(),
        std::env::var_os("PATH"),
    )
}

/// How to get Tailwind, for warnings and hints.
pub(crate) const HOW_TO_INSTALL: &str = "run `smeltery tailwind:install`, or set TAILWIND_BIN to a downloaded \
     standalone binary; on PATH it must be named `tailwindcss`";

/// What [`Installer::install`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Installed {
    /// Downloaded and verified into this path.
    Downloaded(PathBuf),
    /// A verified binary was already there.
    Reused(PathBuf),
}

impl Installed {
    pub(crate) fn path(&self) -> &Path {
        match self {
            Installed::Downloaded(p) | Installed::Reused(p) => p,
        }
    }
}

/// The largest download accepted: the release binaries are 80-113 MB, so this leaves room for growth and stops an
/// endpoint that streams without end before it fills the disk.
pub(crate) const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
// The largest v4.3.3 binary (Windows x64) is 112,503,296 bytes; keep room above it.
const _: () = assert!(MAX_DOWNLOAD_BYTES > 2 * 112_503_296);

/// Part files left by an interrupted download are removed once they are this old.
const STALE_PART: Duration = Duration::from_secs(60 * 60);

/// Downloads one release file into a folder and verifies it.
#[derive(Debug, Clone)]
pub(crate) struct Installer {
    /// The release folder URL, without a trailing `/`.
    pub(crate) base_url: String,
    pub(crate) dir: PathBuf,
    pub(crate) asset_name: String,
    pub(crate) sha256: String,
    pub(crate) file_name: String,
    /// Connecting may take this long.
    pub(crate) connect_timeout: Duration,
    /// Each read of the response (headers, then every body chunk) may wait this long.
    pub(crate) read_timeout: Duration,
    /// The download fails past this many bytes.
    pub(crate) max_bytes: u64,
}

impl Installer {
    /// The pinned release for this platform into the per-user folder.
    ///
    /// # Errors
    /// The platform has no standalone binary, or no per-user folder can be found.
    pub(crate) fn for_user() -> anyhow::Result<Self> {
        let Some(asset) = current_asset() else {
            bail!(
                "Tailwind CSS has no standalone binary for {}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
        };
        let Some(dir) = user_bin_dir() else {
            bail!("no per-user folder for the Tailwind binary (LOCALAPPDATA or HOME is not set)");
        };
        Ok(Self {
            base_url: format!("{RELEASES}/v{VERSION}"),
            dir,
            asset_name: asset.name.to_owned(),
            sha256: asset.sha256.to_owned(),
            file_name: file_name(),
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(30),
            max_bytes: MAX_DOWNLOAD_BYTES,
        })
    }

    /// The installed binary's path.
    pub(crate) fn target(&self) -> PathBuf {
        self.dir.join(&self.file_name)
    }

    /// Reuses a verified binary, or downloads the release file to a temporary file, checks its SHA-256 and renames
    /// it into place (executable on Unix). A file that fails the check is deleted, and so are part files of
    /// interrupted downloads older than an hour.
    ///
    /// # Errors
    /// The download fails (connection, HTTP status, timeout, size, a refused redirect) or the checksum does not
    /// match.
    pub(crate) fn install(&self) -> anyhow::Result<Installed> {
        let target = self.target();
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("cannot create {}", self.dir.display()))?;
        // `serve` and `build` refuse a folder or binary others can write to (`verify_installed`); a umask of 002
        // makes both group-writable, so they are tightened here.
        #[cfg(unix)]
        {
            // The `smeltery` folder above `bin` is Smeltery's too (`verify_installed` checks it).
            let ours = self
                .dir
                .parent()
                .filter(|p| p.file_name().is_some_and(|n| n == "smeltery"));
            for path in [Some(self.dir.as_path()), ours, Some(target.as_path())]
                .into_iter()
                .flatten()
            {
                not_writable_by_others(path)?;
            }
        }
        if target.is_file() && sha256_file(&target).is_ok_and(|h| h == self.sha256) {
            self.record_verified(&target);
            return Ok(Installed::Reused(target));
        }
        self.remove_stale_parts(STALE_PART);
        let url = format!("{}/{}", self.base_url, self.asset_name);
        let part = self
            .dir
            .join(format!("{}.{}.part", self.file_name, std::process::id()));
        let result = self.download(&url, &part);
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        result?;
        if let Err(e) = std::fs::rename(&part, &target) {
            let _ = std::fs::remove_file(&part);
            bail!("cannot move the download to {}: {e}", target.display());
        }
        self.record_verified(&target);
        Ok(Installed::Downloaded(target))
    }

    /// Writes the stamp [`verify_installed`] reads for a binary whose hash was just checked, so the next `serve` or
    /// `build` does not hash it again. Best effort: without the stamp it is hashed once more.
    fn record_verified(&self, target: &Path) {
        if let Ok(fingerprint) = fingerprint(target) {
            let stamp = self.dir.join(format!("{}.verified", self.file_name));
            let _ = write_stamp(&stamp, &format!("{fingerprint} {}\n", self.sha256));
        }
    }

    /// Deletes `<file_name>.<pid>.part` files not modified for `age` (a download that was killed).
    pub(crate) fn remove_stale_parts(&self, age: Duration) {
        let prefix = format!("{}.", self.file_name);
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with(&prefix) && name.ends_with(".part")) {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|elapsed| elapsed >= age);
            if old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    fn download(&self, url: &str, part: &Path) -> anyhow::Result<()> {
        let client = client(&self.base_url, self.connect_timeout, self.read_timeout)?;
        let mut response = client
            .get(url)
            .send()
            .with_context(|| format!("cannot download {url}"))?;
        let status = response.status();
        if !status.is_success() {
            bail!("cannot download {url}: HTTP {status}");
        }
        let too_big = || {
            anyhow::anyhow!(
                "cannot download {url}: it is larger than {} MiB",
                self.max_bytes / (1024 * 1024)
            )
        };
        if response
            .content_length()
            .is_some_and(|n| n > self.max_bytes)
        {
            return Err(too_big());
        }
        let mut file = std::fs::File::create(part)
            .with_context(|| format!("cannot write {}", part.display()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut total: u64 = 0;
        loop {
            let n = response
                .read(&mut buf)
                .with_context(|| format!("cannot download {url}"))?;
            if n == 0 {
                break;
            }
            total = total.saturating_add(n as u64);
            if total > self.max_bytes {
                return Err(too_big());
            }
            let chunk = buf.get(..n).unwrap_or_default();
            hasher.update(chunk);
            file.write_all(chunk)
                .with_context(|| format!("cannot write {}", part.display()))?;
        }
        let actual = hex(&hasher.finalize());
        if actual != self.sha256 {
            bail!(
                "the download of {url} failed its SHA-256 check (expected {}, got {actual}); the file was not kept",
                self.sha256
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        file.sync_all()?;
        Ok(())
    }
}

/// Whether a redirect to `scheme://host` is followed, for a download from `base_url`. From an `https://` base: only
/// `https` to `github.com` or a `*.githubusercontent.com` host (where GitHub serves release files, e.g.
/// `release-assets.githubusercontent.com`). From another base (the tests' local server): only the same host.
pub(crate) fn redirect_allowed(base_url: &str, scheme: &str, host: &str) -> bool {
    match base_url.strip_prefix("https://") {
        Some(_) => {
            scheme == "https" && (host == "github.com" || host.ends_with(".githubusercontent.com"))
        }
        None => {
            let base_host = base_url
                .split("://")
                .nth(1)
                .and_then(|rest| rest.split(['/', ':']).next())
                .unwrap_or_default();
            host == base_host
        }
    }
}

/// A blocking HTTPS client: rustls with ring and the platform's certificate verifier, as Watchfire's HTTP client.
/// For an `https://` base it refuses plain HTTP, and every redirect must pass [`redirect_allowed`] (at most 5).
fn client(
    base_url: &str,
    connect: Duration,
    read: Duration,
) -> anyhow::Result<reqwest::blocking::Client> {
    use rustls_platform_verifier::BuilderVerifierExt as _;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("TLS setup")?
        .with_platform_verifier()
        .context("TLS setup")?
        .with_no_client_auth();
    let base = base_url.to_owned();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        let url = attempt.url();
        let (scheme, host) = (
            url.scheme().to_owned(),
            url.host_str().unwrap_or_default().to_owned(),
        );
        if attempt.previous().len() >= 5 {
            attempt.error("too many redirects")
        } else if redirect_allowed(&base, &scheme, &host) {
            attempt.follow()
        } else {
            attempt.error(format!("refused a redirect to {scheme}://{host}"))
        }
    });
    reqwest::blocking::Client::builder()
        .tls_backend_preconfigured(tls)
        .https_only(base_url.starts_with("https://"))
        .redirect(policy)
        .user_agent(concat!("smeltery/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(connect)
        // Blocking reqwest applies this to every read, so a slow but moving download is not cut off.
        .timeout(read)
        .build()
        .context("HTTP client setup")
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(buf.get(..n).unwrap_or_default());
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Installs with `installer`, printing progress; `Err` carries the reason. Used by `tailwind:install` and `new`.
pub(crate) fn install_with_progress(installer: &Installer, ui: Ui) -> anyhow::Result<Installed> {
    let text = format!("Downloading Tailwind CSS v{VERSION} (about 110 MB)");
    if ui.styled() {
        let spinner = ui.spinner(&text);
        let result = installer.install();
        spinner.finish();
        result
    } else {
        if !installer.target().is_file() {
            println!("{text}...");
        }
        installer.install()
    }
}

/// `smeltery tailwind:install`.
pub(crate) fn run(ui: Ui) -> anyhow::Result<ExitCode> {
    let installer = Installer::for_user()?;
    let installed = install_with_progress(&installer, ui)?;
    let message = match &installed {
        Installed::Downloaded(p) => format!("Tailwind CSS v{VERSION} installed at {}", p.display()),
        Installed::Reused(p) => {
            format!(
                "Tailwind CSS v{VERSION} is already installed at {}",
                p.display()
            )
        }
    };
    if ui.styled() {
        println!("{}", ui.badged(Badge::Done, &message));
    } else {
        println!("{message}");
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests;
