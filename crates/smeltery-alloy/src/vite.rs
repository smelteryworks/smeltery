//! Vite: the asset tags of `@vite` and the asset version, from the dev server's hot file (debug builds only) or the
//! build manifest `public/<build dir>/manifest.json`. No Node at runtime.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// One chunk of the Vite manifest (only the fields the tags need).
#[derive(Debug, Deserialize)]
struct Chunk {
    file: String,
    #[serde(default)]
    css: Vec<String>,
    #[serde(default)]
    imports: Vec<String>,
}

/// A parsed manifest with its version.
#[derive(Debug)]
pub(crate) struct Loaded {
    mtime: Option<SystemTime>,
    chunks: HashMap<String, Chunk>,
    /// The first 32 hex characters of SHA-256 over the manifest's bytes.
    pub(crate) version: String,
}

/// The build mode a call runs in; the public paths pass the real one, tests pass both.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mode {
    /// `debug_assertions`: the hot file is honoured and the manifest re-read when it changes.
    pub(crate) debug: bool,
    /// `APP_ENV=testing`: no assets render nothing instead of an error.
    pub(crate) testing: bool,
    /// `APP_ENV=production`: the hot file is ignored, in debug builds too.
    pub(crate) production: bool,
}

pub(crate) const NO_ASSETS: &str = "No Vite assets: run `npm install` and `smeltery serve` (or `npm run build`); \
     public/build/manifest.json is missing and the Vite dev server is not running";

/// The Vite configuration of an app and its manifest cache.
#[derive(Debug)]
pub(crate) struct Vite {
    pub(crate) entries: Vec<String>,
    pub(crate) build_dir: String,
    pub(crate) hot_file: PathBuf,
    cache: Mutex<Option<Arc<Loaded>>>,
    warned: AtomicBool,
    /// Whether an ignored hot file was logged already.
    hot_warned: AtomicBool,
}

impl Vite {
    pub(crate) fn new(entries: Vec<String>, build_dir: String, hot_file: PathBuf) -> Self {
        Self {
            entries,
            build_dir,
            hot_file,
            cache: Mutex::new(None),
            warned: AtomicBool::new(false),
            hot_warned: AtomicBool::new(false),
        }
    }

    pub(crate) fn manifest_path(&self, root: &Path) -> PathBuf {
        root.join("public")
            .join(&self.build_dir)
            .join("manifest.json")
    }

    /// The dev server's URL from the hot file: in debug builds only (a stale file can never point a release binary
    /// at a localhost script, D-281), never under `APP_ENV=production`, and only an `http(s)` URL on this machine
    /// (`127.0.0.1`, `localhost`, `[::1]`), so a written hot file cannot point every page's scripts at another host
    /// (S5-06). An ignored hot file is logged once.
    pub(crate) fn hot_url(&self, root: &Path, mode: Mode) -> Option<String> {
        if !mode.debug {
            return None;
        }
        let text = std::fs::read_to_string(root.join(&self.hot_file)).ok()?;
        let url = text.trim().trim_end_matches('/');
        let ignored = if mode.production {
            "APP_ENV=production"
        } else if !is_loopback_url(url) {
            "not an http(s) URL on 127.0.0.1, localhost or [::1]"
        } else {
            return Some(url.to_owned());
        };
        if !self.hot_warned.swap(true, Ordering::Relaxed) {
            let file = display(&root.join(&self.hot_file));
            tracing::warn!(
                %file,
                reason = ignored,
                "the Vite hot file is ignored"
            );
        }
        None
    }

    /// The manifest: read once and kept in release builds; re-read when its modification time changes in debug
    /// builds. `Ok(None)` when there is none.
    pub(crate) fn manifest(&self, root: &Path, mode: Mode) -> Result<Option<Arc<Loaded>>, String> {
        let path = self.manifest_path(root);
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(cached) = &cached
            && !mode.debug
        {
            return Ok(Some(cached.clone()));
        }
        let mtime = match std::fs::metadata(&path) {
            Ok(meta) => meta.modified().ok(),
            Err(_) => {
                *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = None;
                return Ok(None);
            }
        };
        if let Some(cached) = cached
            && cached.mtime.is_some()
            && cached.mtime == mtime
        {
            return Ok(Some(cached));
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("cannot read {}: {e}", display(&path))),
        };
        let chunks: HashMap<String, Chunk> = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{} is not a Vite manifest: {e}", display(&path)))?;
        let digest = Sha256::digest(&bytes);
        let version: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
        let loaded = Arc::new(Loaded {
            mtime,
            chunks,
            version,
        });
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = Some(loaded.clone());
        Ok(Some(loaded))
    }

    /// The version of the manifest already read, in release builds (no file access); `None` otherwise.
    pub(crate) fn cached_version(&self, mode: Mode) -> Option<String> {
        if mode.debug {
            return None;
        }
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|loaded| loaded.version.clone())
    }

    /// The asset version: `""` while the dev server runs or without a manifest.
    pub(crate) fn version(&self, root: &Path, mode: Mode) -> String {
        if self.hot_url(root, mode).is_some() {
            return String::new();
        }
        match self.manifest(root, mode) {
            Ok(Some(loaded)) => loaded.version.clone(),
            Ok(None) => String::new(),
            Err(e) => {
                tracing::error!(error = %e, "the Vite manifest cannot be read");
                String::new()
            }
        }
    }

    /// The tags of `@vite` for `entries` (the configured ones when empty).
    pub(crate) fn tags(&self, root: &Path, entries: &[&str], mode: Mode) -> Result<String, String> {
        let entries: Vec<&str> = if entries.is_empty() {
            self.entries.iter().map(String::as_str).collect()
        } else {
            entries.to_vec()
        };
        for entry in &entries {
            check_entry(entry)?;
        }
        if let Some(url) = self.hot_url(root, mode) {
            let url = escape(&url);
            let mut tags = vec![format!(
                "<script type=\"module\" src=\"{url}/@vite/client\"></script>"
            )];
            for entry in &entries {
                tags.push(format!(
                    "<script type=\"module\" src=\"{url}/{}\"></script>",
                    escape(entry)
                ));
            }
            return Ok(tags.join("\n"));
        }
        let Some(manifest) = self.manifest(root, mode)? else {
            if mode.testing {
                return Ok(String::new());
            }
            if mode.debug {
                return Err(NO_ASSETS.to_owned());
            }
            if !self.warned.swap(true, Ordering::Relaxed) {
                let manifest = display(&self.manifest_path(root));
                tracing::error!(
                    %manifest,
                    "{NO_ASSETS}; pages render without their scripts"
                );
            }
            return Ok(String::new());
        };
        // `check_build_dir` ran when the app was built; escaped all the same, like every other attribute value.
        let base = escape(&format!("/{}/", self.build_dir.trim_matches('/')));
        let mut css: Vec<&str> = Vec::new();
        let mut preload: Vec<&str> = Vec::new();
        let mut scripts: Vec<&str> = Vec::new();
        let mut visited: HashSet<&str> = HashSet::new();
        for entry in &entries {
            let chunk = manifest.chunks.get(*entry).ok_or_else(|| {
                format!(
                    "the Vite entry `{entry}` is not in {}: add it to the Vite config's inputs and run `npm run build`",
                    display(&self.manifest_path(root))
                )
            })?;
            css.extend(chunk.css.iter().map(String::as_str));
            walk_imports(
                &manifest.chunks,
                chunk,
                &mut visited,
                &mut css,
                &mut preload,
            );
            scripts.push(&chunk.file);
        }
        let mut seen = HashSet::new();
        let mut tags = Vec::new();
        for file in css {
            if seen.insert(file) {
                tags.push(format!(
                    "<link rel=\"stylesheet\" href=\"{base}{}\">",
                    escape(file)
                ));
            }
        }
        for file in preload {
            if !scripts.contains(&file) && seen.insert(file) {
                tags.push(format!(
                    "<link rel=\"modulepreload\" href=\"{base}{}\">",
                    escape(file)
                ));
            }
        }
        for file in scripts {
            if seen.insert(file) {
                tags.push(format!(
                    "<script type=\"module\" src=\"{base}{}\"></script>",
                    escape(file)
                ));
            }
        }
        Ok(tags.join("\n"))
    }
}

/// The CSS and the chunk files of everything `chunk` imports statically, depth first (Vite's backend-integration
/// algorithm); dynamic imports (the pages) are left out.
fn walk_imports<'m>(
    chunks: &'m HashMap<String, Chunk>,
    chunk: &'m Chunk,
    visited: &mut HashSet<&'m str>,
    css: &mut Vec<&'m str>,
    preload: &mut Vec<&'m str>,
) {
    for name in &chunk.imports {
        if !visited.insert(name) {
            continue;
        }
        if let Some(imported) = chunks.get(name) {
            preload.push(&imported.file);
            css.extend(imported.css.iter().map(String::as_str));
            walk_imports(chunks, imported, visited, css, preload);
        }
    }
}

/// Entries are project-relative paths of letters, digits and `_ . / @ -`, without `..`: they end up in URLs and
/// manifest lookups, never in a path outside the project.
fn check_entry(entry: &str) -> Result<(), String> {
    let ok = !entry.is_empty()
        && !entry.starts_with('/')
        && entry
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '@' | '-'))
        && !entry
            .split('/')
            .any(|segment| segment == ".." || segment.is_empty());
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid Vite entry `{entry}`: a project-relative path such as `resources/js/app.tsx` \
             (letters, digits, `_ . / @ -`, no `..`)"
        ))
    }
}

/// The build directory is a relative path under `public/` of letters, digits and `_ . / @ -`, without `.` / `..`
/// segments: it ends up in the manifest path and in every asset URL (S5-05).
pub(crate) fn check_build_dir(dir: &str) -> Result<(), String> {
    let trimmed = dir.trim_matches('/');
    let ok = !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '@' | '-'))
        && !trimmed
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..");
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid Alloy build directory `{}`: a folder under public/ such as `build` (letters, digits, \
             `_ . / @ -`, no `..`)",
            dir.escape_debug()
        ))
    }
}

/// Whether `url` is `http://` or `https://` on `127.0.0.1`, `localhost` or `[::1]`, with an optional port and path.
fn is_loopback_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (host, port) = if authority.starts_with('[') {
        match authority.find(']') {
            Some(end) => authority.split_at(end + 1),
            None => return false,
        }
    } else {
        match authority.find(':') {
            Some(i) => authority.split_at(i),
            None => (authority, ""),
        }
    };
    let port_ok = port.is_empty()
        || port.strip_prefix(':').is_some_and(|p| {
            !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit())
        });
    port_ok && ["127.0.0.1", "localhost", "[::1]"].contains(&host.to_ascii_lowercase().as_str())
}

/// Escape for a double-quoted HTML attribute.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn display(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use std::time::Duration;

    const DEBUG: Mode = Mode {
        debug: true,
        testing: false,
        production: false,
    };
    const RELEASE: Mode = Mode {
        debug: false,
        testing: false,
        production: false,
    };
    const TESTING: Mode = Mode {
        debug: true,
        testing: true,
        production: false,
    };
    /// A debug build deployed with `APP_ENV=production`.
    const DEBUG_PRODUCTION: Mode = Mode {
        debug: true,
        testing: false,
        production: true,
    };

    /// The shape Vite 8.3.2 writes (`build.manifest: "manifest.json"`), with a shared chunk carrying CSS.
    const MANIFEST: &str = r#"{
      "resources/js/app.tsx": {
        "file": "assets/app-1a2b.js", "src": "resources/js/app.tsx", "isEntry": true,
        "imports": ["_vendor-9z.js"], "dynamicImports": ["resources/js/pages/welcome.tsx"],
        "css": ["assets/app-3c4d.css"]
      },
      "resources/js/admin.tsx": {
        "file": "assets/admin-5e6f.js", "src": "resources/js/admin.tsx", "isEntry": true,
        "imports": ["_vendor-9z.js"]
      },
      "_vendor-9z.js": {"file": "assets/vendor-9z.js", "css": ["assets/vendor-7g.css"], "imports": ["_tiny.js"]},
      "_tiny.js": {"file": "assets/tiny.js"},
      "resources/js/pages/welcome.tsx": {
        "file": "assets/welcome-8h.js", "isDynamicEntry": true, "imports": ["resources/js/app.tsx"]
      }
    }"#;

    fn vite() -> Vite {
        Vite::new(
            vec!["resources/js/app.tsx".into()],
            "build".into(),
            PathBuf::from("storage/framework/vite.hot"),
        )
    }

    fn write_manifest(root: &Path, text: &str) {
        let dir = root.join("public/build");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), text).unwrap();
    }

    fn write_hot(root: &Path, url: &str) {
        let dir = root.join("storage/framework");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vite.hot"), url).unwrap();
    }

    #[test]
    fn built_tags_follow_the_manifest() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path(), MANIFEST);
        let v = vite();
        assert_eq!(
            v.tags(root.path(), &[], RELEASE).unwrap(),
            "<link rel=\"stylesheet\" href=\"/build/assets/app-3c4d.css\">\n\
             <link rel=\"stylesheet\" href=\"/build/assets/vendor-7g.css\">\n\
             <link rel=\"modulepreload\" href=\"/build/assets/vendor-9z.js\">\n\
             <link rel=\"modulepreload\" href=\"/build/assets/tiny.js\">\n\
             <script type=\"module\" src=\"/build/assets/app-1a2b.js\"></script>"
        );
        // Two entries share the vendor chunk: every tag once.
        assert_eq!(
            v.tags(
                root.path(),
                &["resources/js/app.tsx", "resources/js/admin.tsx"],
                DEBUG
            )
            .unwrap(),
            "<link rel=\"stylesheet\" href=\"/build/assets/app-3c4d.css\">\n\
             <link rel=\"stylesheet\" href=\"/build/assets/vendor-7g.css\">\n\
             <link rel=\"modulepreload\" href=\"/build/assets/vendor-9z.js\">\n\
             <link rel=\"modulepreload\" href=\"/build/assets/tiny.js\">\n\
             <script type=\"module\" src=\"/build/assets/app-1a2b.js\"></script>\n\
             <script type=\"module\" src=\"/build/assets/admin-5e6f.js\"></script>"
        );
        let err = v
            .tags(root.path(), &["resources/js/missing.tsx"], DEBUG)
            .unwrap_err();
        assert!(
            err.contains("`resources/js/missing.tsx` is not in"),
            "{err}"
        );
        assert!(err.contains("public/build/manifest.json"), "{err}");
    }

    #[test]
    fn entries_are_checked_and_file_names_escaped() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path(), r#"{"a.ts": {"file": "assets/a\"<b>.js"}}"#);
        let v = vite();
        assert_eq!(
            v.tags(root.path(), &["a.ts"], DEBUG).unwrap(),
            "<script type=\"module\" src=\"/build/assets/a&quot;&lt;b&gt;.js\"></script>"
        );
        for bad in [
            "../secret.ts",
            "/abs.ts",
            "a\"b.ts",
            "a<b.ts",
            "a//b.ts",
            "a b.ts",
            "",
        ] {
            let err = v.tags(root.path(), &[bad], DEBUG).unwrap_err();
            assert!(err.starts_with("invalid Vite entry"), "{bad}: {err}");
        }
        assert!(
            v.tags(root.path(), &["@scope/x-y_z.1.ts"], DEBUG).is_err(),
            "not in the manifest"
        );
    }

    #[test]
    fn the_hot_file_points_at_the_dev_server_in_debug_builds_only() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path(), MANIFEST);
        write_hot(root.path(), "http://127.0.0.1:5173\n");
        let v = vite();
        assert_eq!(
            v.tags(root.path(), &[], DEBUG).unwrap(),
            "<script type=\"module\" src=\"http://127.0.0.1:5173/@vite/client\"></script>\n\
             <script type=\"module\" src=\"http://127.0.0.1:5173/resources/js/app.tsx\"></script>"
        );
        assert_eq!(
            v.version(root.path(), DEBUG),
            "",
            "the dev server: no version"
        );
        // A release build ignores the hot file.
        assert!(
            v.tags(root.path(), &[], RELEASE)
                .unwrap()
                .contains("/build/assets/app-1a2b.js")
        );
        assert_eq!(v.version(root.path(), RELEASE).len(), 32);
        // Not a URL: ignored.
        write_hot(root.path(), "garbage");
        assert!(v.hot_url(root.path(), DEBUG).is_none());
    }

    #[test]
    fn the_hot_file_is_ignored_in_production_and_off_this_machine() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path(), MANIFEST);
        let v = vite();
        // A debug binary deployed with APP_ENV=production uses the build.
        write_hot(root.path(), "http://127.0.0.1:5173\n");
        assert!(v.hot_url(root.path(), DEBUG).is_some());
        assert_eq!(v.hot_url(root.path(), DEBUG_PRODUCTION), None);
        assert!(
            v.tags(root.path(), &[], DEBUG_PRODUCTION)
                .unwrap()
                .contains("/build/assets/app-1a2b.js")
        );
        assert_eq!(v.version(root.path(), DEBUG_PRODUCTION).len(), 32);
        for ok in [
            "http://127.0.0.1:5173",
            "http://localhost:5173/",
            "https://LOCALHOST:5173",
            "http://[::1]:5173",
            "http://localhost",
            "http://127.0.0.1:5173/base",
        ] {
            write_hot(root.path(), ok);
            assert!(v.hot_url(root.path(), DEBUG).is_some(), "{ok}");
        }
        for bad in [
            "http://evil.example:5173",
            "https://cdn.evil.example",
            "http://127.0.0.1.evil.example:5173",
            "http://127.0.0.1@evil.example",
            "http://localhost:5173@evil.example",
            "http://[::1].evil.example",
            "http://10.0.0.5:5173",
            "http://localhost:",
            "http://localhost:99999x",
            "//127.0.0.1:5173",
            "ftp://127.0.0.1",
        ] {
            write_hot(root.path(), bad);
            assert_eq!(v.hot_url(root.path(), DEBUG), None, "{bad}");
            assert!(
                v.tags(root.path(), &[], DEBUG)
                    .unwrap()
                    .contains("/build/assets/app-1a2b.js"),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_build_dir_is_a_plain_folder_and_escaped() {
        for ok in ["build", "/build/", "assets/build", "v1.2_x-y@z"] {
            assert!(check_build_dir(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/",
            "..",
            "../secret",
            "a/../b",
            "a//b",
            "./build",
            "bu\"ild",
            "b>ild",
            "b ild",
            "b\\ild",
            "caf\u{e9}",
        ] {
            let err = check_build_dir(bad).unwrap_err();
            assert!(
                err.starts_with("invalid Alloy build directory"),
                "{bad}: {err}"
            );
        }
        // The base is escaped in the tags all the same (a quote is a valid folder name on every platform).
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("public/a'b");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), MANIFEST).unwrap();
        let v = Vite::new(
            vec!["resources/js/app.tsx".into()],
            "a'b".into(),
            PathBuf::from("storage/framework/vite.hot"),
        );
        let tags = v.tags(root.path(), &[], RELEASE).unwrap();
        assert!(tags.contains("href=\"/a&#39;b/assets/"), "{tags}");
    }

    #[test]
    fn no_assets_is_an_error_in_debug_nothing_in_tests_and_release() {
        let root = tempfile::tempdir().unwrap();
        let v = vite();
        assert_eq!(v.tags(root.path(), &[], DEBUG).unwrap_err(), NO_ASSETS);
        assert_eq!(v.tags(root.path(), &[], TESTING).unwrap(), "");
        assert_eq!(v.tags(root.path(), &[], RELEASE).unwrap(), "");
        assert!(v.warned.load(Ordering::Relaxed), "logged once");
        assert_eq!(v.version(root.path(), RELEASE), "");
    }

    #[test]
    fn the_version_hashes_the_manifest_and_follows_changes_in_debug() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path(), MANIFEST);
        let v = vite();
        let first = v.version(root.path(), DEBUG);
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(v.version(root.path(), DEBUG), first, "stable");
        let expected: String = Sha256::digest(MANIFEST.as_bytes())
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(first, expected);

        write_manifest(root.path(), &MANIFEST.replace("1a2b", "ffff"));
        let file = std::fs::File::options()
            .write(true)
            .open(root.path().join("public/build/manifest.json"))
            .unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        drop(file);
        let second = v.version(root.path(), DEBUG);
        assert_ne!(second, first, "debug builds re-read a changed manifest");
        // Release builds keep the manifest they read first, and answer from it without file access.
        write_manifest(root.path(), &MANIFEST.replace("1a2b", "eeee"));
        assert_eq!(v.version(root.path(), RELEASE), second);
        assert_eq!(v.cached_version(RELEASE).as_deref(), Some(second.as_str()));
        assert_eq!(v.cached_version(DEBUG), None);
    }

    #[test]
    fn a_broken_manifest_is_an_error_naming_it() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path(), "{not json");
        let err = vite().tags(root.path(), &[], DEBUG).unwrap_err();
        assert!(
            err.contains("public/build/manifest.json is not a Vite manifest"),
            "{err}"
        );
    }
}
