//! The runtime engine (with hot reload), the [`Host`] seam and the [`Template`] trait.

use crate::ast::Resolved;
use crate::{Error, Value, interp, resolve};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Request data a template can reach: `@csrf`, `@auth`/`@guest`, `@error`, `old()`, `route()`, `@spark`,
/// `@sparksScripts`, `@alloy`, `@alloyHead` and `@vite`.
///
/// The web layer implements it per request; every method has a default, so [`NoHost`] (and tests) implement only
/// what they need.
pub trait Host {
    /// The CSRF token of the current session; `@csrf` and `csrf_token()` fail without one.
    fn csrf_token(&self) -> Option<&str> {
        None
    }
    /// Whether a user is signed in (`@auth` / `@guest`).
    fn authenticated(&self) -> bool {
        false
    }
    /// Validation errors for `field` (`@error`).
    fn errors(&self, _field: &str) -> &[String] {
        &[]
    }
    /// The previously submitted value of `field` (`old("field")`).
    fn old(&self, _field: &str) -> Option<&str> {
        None
    }
    /// A session value as text (`session("key")`), such as a flashed status message.
    fn session(&self, _key: &str) -> Option<String> {
        None
    }
    /// The URL of the named route with its parameters (`route("name", { … })`).
    fn route(&self, name: &str, _params: &[(String, String)]) -> Result<String, String> {
        Err(format!(
            "cannot build the URL of route `{name}`: routing is not available here"
        ))
    }
    /// The HTML of Spark `name` mounted with `props` (`@spark`).
    fn spark(&self, _name: &str, _props: &Value) -> Result<String, String> {
        Err("Sparks are not enabled".to_owned())
    }
    /// The script tag of the Sparks client runtime (`@sparksScripts`).
    fn sparks_scripts(&self) -> String {
        String::new()
    }
    /// The HTML of the Alloy page element with root id `id` (`@alloy`, `@alloy("id")`).
    ///
    /// `id` is the template's string literal, already checked to be letters, digits, `-` and `_`. The returned HTML
    /// is written unescaped: the host escapes everything it embeds (the page JSON in particular).
    fn alloy_page(&self, _id: &str) -> Result<String, String> {
        Err(ALLOY_NOT_ENABLED.to_owned())
    }
    /// The head tags of the Alloy page (`@alloyHead`); written unescaped. Empty by default.
    fn alloy_head(&self) -> String {
        String::new()
    }
    /// The Vite asset tags for `entries` (`@vite("a", …)`), or for the configured entries when `entries` is empty
    /// (a bare `@vite`).
    ///
    /// The entries are the template's non-empty string literals as written, not validated or escaped by Mold. The
    /// returned HTML is written unescaped, so the host must escape (or reject) anything it embeds in attributes and
    /// must never build a path outside the project from an entry.
    fn vite(&self, _entries: &[&str]) -> Result<String, String> {
        Err(ALLOY_NOT_ENABLED.to_owned())
    }
}

/// The error of `@alloy` and `@vite` when the host has no Alloy integration.
const ALLOY_NOT_ENABLED: &str = "Alloy is not enabled: call `.alloy(…)` in bootstrap/app.rs";

/// A [`Host`] with no request: no token, nobody signed in, no errors, no old input.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHost;

impl Host for NoHost {}

/// A template bound to its data, usually through `#[derive(Mold)]`.
///
/// `render` uses the runtime engine (hot reload) in debug builds and the compiled code in release builds; both
/// produce the same bytes.
pub trait Template {
    /// The template name, such as `posts/index`.
    const NAME: &'static str;

    /// Renders with the runtime engine from the template files.
    fn render_runtime(&self, host: &dyn Host) -> Result<String, Error>;

    /// Renders with the runtime engine `engine` (the web stack passes the app's engine, which reads
    /// `<root>/resources/views`). The default ignores `engine` and calls [`Template::render_runtime`];
    /// `#[derive(Mold)]` implements it.
    fn render_runtime_with(&self, engine: &Engine, host: &dyn Host) -> Result<String, Error> {
        let _ = engine;
        self.render_runtime(host)
    }

    /// Renders with the code generated at compile time.
    fn render_compiled(&self, host: &dyn Host) -> Result<String, Error>;

    /// Renders in the mode of the build: runtime with `debug_assertions`, compiled otherwise.
    fn render(&self, host: &dyn Host) -> Result<String, Error> {
        if cfg!(debug_assertions) {
            self.render_runtime(host)
        } else {
            self.render_compiled(host)
        }
    }
}

/// The runtime template engine: parses, resolves and caches templates from a views directory, and re-reads them
/// when a file they were built from changes (a `stat` per file per render, no watcher thread).
#[derive(Debug)]
pub struct Engine {
    views_dir: PathBuf,
    display_dir: PathBuf,
    cache: Mutex<HashMap<String, Arc<Resolved>>>,
}

impl Engine {
    /// An engine reading `<views_dir>/<name>.mold.html`.
    pub fn new(views_dir: impl Into<PathBuf>) -> Self {
        let views_dir = views_dir.into();
        let display_dir = display_path(&views_dir);
        Self {
            views_dir,
            display_dir,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The directory templates are read from.
    pub fn views_dir(&self) -> &Path {
        &self.views_dir
    }

    /// The engine of the app, reading from [`views_dir()`].
    pub fn global() -> &'static Engine {
        static GLOBAL: OnceLock<Engine> = OnceLock::new();
        GLOBAL.get_or_init(|| Engine::new(views_dir()))
    }

    /// Renders template `name` (such as `posts/index`) with `data` (a map, usually from [`crate::to_value`]).
    pub fn render(&self, name: &str, data: &Value, host: &dyn Host) -> Result<String, Error> {
        let resolved = self.resolved(name)?;
        interp::render(&resolved, data, host)
    }

    /// The resolved template, from the cache when none of its files changed.
    fn resolved(&self, name: &str) -> Result<Arc<Resolved>, Error> {
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned();
        if let Some(r) = cached
            && r.files.iter().all(|f| {
                std::fs::metadata(&f.path)
                    .and_then(|m| m.modified())
                    .ok()
                    .is_some_and(|t| Some(t) == f.mtime)
            })
        {
            return Ok(r);
        }
        let r = Arc::new(resolve::resolve_dir(
            &self.views_dir,
            &self.display_dir,
            name,
        )?);
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned(), r.clone());
        Ok(r)
    }
}

static GLOBAL_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Sets the views directory of [`Engine::global`]. Returns `false` when it was already set or the global engine is
/// already in use.
pub fn set_global_views_dir(path: impl Into<PathBuf>) -> bool {
    GLOBAL_DIR.set(path.into()).is_ok()
}

/// The app's views directory: the one given to [`set_global_views_dir`], else `$SMELTERY_ROOT/resources/views`,
/// else `./resources/views`.
pub fn views_dir() -> PathBuf {
    GLOBAL_DIR
        .get_or_init(|| {
            std::env::var_os("SMELTERY_ROOT")
                .map_or_else(|| PathBuf::from("."), PathBuf::from)
                .join("resources")
                .join("views")
        })
        .clone()
}

/// `dir` relative to the working directory when it lies below it, without a leading `./`.
fn display_path(dir: &Path) -> PathBuf {
    let rel = std::env::current_dir()
        .ok()
        .and_then(|cwd| dir.strip_prefix(cwd).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| dir.to_path_buf());
    rel.strip_prefix(".").map(Path::to_path_buf).unwrap_or(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    #[test]
    fn hot_reload_on_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("page.mold.html");
        fs::write(&file, "v1 {{ x }}").unwrap();
        let engine = Engine::new(dir.path());
        let data = Value::Map(vec![("x".into(), Value::Int(1))]);
        assert_eq!(engine.render("page", &data, &NoHost).unwrap(), "v1 1");
        fs::write(&file, "v2 {{ x }}").unwrap();
        // Force a different mtime even on file systems with coarse timestamps.
        let f = fs::File::options().write(true).open(&file).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        drop(f);
        assert_eq!(engine.render("page", &data, &NoHost).unwrap(), "v2 1");
    }

    #[test]
    fn included_file_changes_reload_too() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("page.mold.html"), "[@include(\"part\")]").unwrap();
        let part = dir.path().join("part.mold.html");
        fs::write(&part, "a").unwrap();
        let engine = Engine::new(dir.path());
        assert_eq!(engine.render("page", &Value::Null, &NoHost).unwrap(), "[a]");
        fs::write(&part, "b").unwrap();
        let f = fs::File::options().write(true).open(&part).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        drop(f);
        assert_eq!(engine.render("page", &Value::Null, &NoHost).unwrap(), "[b]");
    }

    #[test]
    fn display_dir_is_relative() {
        assert_eq!(
            display_path(Path::new("./resources/views")),
            PathBuf::from("resources/views")
        );
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(display_path(&cwd.join("x")), PathBuf::from("x"));
    }

    #[test]
    fn default_host_methods() {
        assert!(NoHost.csrf_token().is_none());
        assert!(!NoHost.authenticated());
        assert!(NoHost.errors("x").is_empty());
        assert!(NoHost.route("home", &[]).is_err());
        assert_eq!(
            NoHost.spark("c", &Value::Null).unwrap_err(),
            "Sparks are not enabled"
        );
        assert_eq!(NoHost.sparks_scripts(), "");
        let off = "Alloy is not enabled: call `.alloy(…)` in bootstrap/app.rs";
        assert_eq!(NoHost.alloy_page("app").unwrap_err(), off);
        assert_eq!(NoHost.vite(&[]).unwrap_err(), off);
        assert_eq!(NoHost.vite(&["a.ts"]).unwrap_err(), off);
        assert_eq!(NoHost.alloy_head(), "");
    }
}
