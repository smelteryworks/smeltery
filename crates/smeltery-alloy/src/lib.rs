#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod page;
mod props;
mod protocol;
pub mod testing;
mod vite;

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};
use smeltery_core::session::Session;
use smeltery_core::view::{AlloyRenderer, RequestHost, view_with_status};
use smeltery_core::{App, AppBuilder, Error, Result};
use smeltery_mold::Template;

pub use page::{Component, Page, render};
pub use props::Props;
pub use protocol::SharedCtx;

use protocol::{ExternalLocation, Inner};

/// How Alloy is set up in `bootstrap/app.rs`: the root template, the Vite entries, shared props and options.
///
/// ```
/// use smeltery::alloy::{Alloy, AlloyExt as _, Props, SharedCtx};
///
/// #[derive(smeltery::Mold, Default)]
/// #[mold("app")]
/// struct Root {}
///
/// async fn shared(ctx: SharedCtx) -> smeltery::Result<Props> {
///     Ok(Props::new().with("app", serde_json::json!({ "name": ctx.app().settings().name })))
/// }
///
/// fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
///     app.alloy(
///         Alloy::new()
///             .root::<Root>()
///             .entries(["resources/js/app.tsx"])
///             .share(shared),
///     )
/// }
/// ```
pub struct Alloy {
    root: Option<(protocol::RootFn, &'static str, &'static str)>,
    entries: Vec<String>,
    share: Option<protocol::ShareFn>,
    version: Option<protocol::VersionFn>,
    all_errors: bool,
    encrypt_history: bool,
    build_dir: String,
    hot_file: PathBuf,
}

impl std::fmt::Debug for Alloy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Alloy")
            .field("root", &self.root.as_ref().map(|(_, name, _)| *name))
            .field("entries", &self.entries)
            .field("build_dir", &self.build_dir)
            .field("hot_file", &self.hot_file)
            .finish_non_exhaustive()
    }
}

impl Default for Alloy {
    fn default() -> Self {
        Self::new()
    }
}

impl Alloy {
    /// Alloy with the defaults: entry `resources/js/app.tsx`, build directory `build` (`public/build/manifest.json`),
    /// hot file `storage/framework/vite.hot`, the first validation message per field, no shared props. A root
    /// template is required ([`Alloy::root`]).
    pub fn new() -> Self {
        Self {
            root: None,
            entries: vec!["resources/js/app.tsx".to_owned()],
            share: None,
            version: None,
            all_errors: false,
            encrypt_history: false,
            build_dir: "build".to_owned(),
            hot_file: PathBuf::from("storage/framework/vite.hot"),
        }
    }

    /// The Mold template of the first visit (usually `resources/views/app.mold.html` with `@vite`, `@alloyHead` and
    /// `@alloy`): `#[derive(Mold, Default)] #[mold("app")] struct Root {}`.
    #[must_use]
    pub fn root<T: Template + Default + Send + 'static>(mut self) -> Self {
        let make: protocol::RootFn = Arc::new(|status| view_with_status(status, T::default()));
        self.root = Some((make, T::NAME, std::any::type_name::<T>()));
        self
    }

    /// The Vite entries a bare `@vite` renders (default `["resources/js/app.tsx"]`).
    #[must_use]
    pub fn entries<I, S>(mut self, entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.entries = entries.into_iter().map(Into::into).collect();
        self
    }

    /// Props every page gets, computed per request by `share` (an `async fn(SharedCtx) -> Result<Props>`). A page
    /// prop with the same key wins; the page object's `sharedProps` lists the shared keys.
    #[must_use]
    pub fn share<F, Fut>(mut self, share: F) -> Self
    where
        F: Fn(SharedCtx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Props>> + Send + 'static,
    {
        self.share = Some(Arc::new(move |ctx| Box::pin(share(ctx))));
        self
    }

    /// The asset version from `f` instead of the hash of the Vite manifest. A visit whose `X-Inertia-Version`
    /// differs gets a full page load.
    #[must_use]
    pub fn version<F>(mut self, f: F) -> Self
    where
        F: Fn(&App) -> String + Send + Sync + 'static,
    {
        self.version = Some(Arc::new(f));
        self
    }

    /// Every validation message per field in `props.errors` (arrays), instead of the first one.
    #[must_use]
    pub fn all_errors(mut self) -> Self {
        self.all_errors = true;
        self
    }

    /// Have the client encrypt every page's history state (`encryptHistory`; per page with
    /// [`Page::encrypt_history`]). Needs a secure context: HTTPS, or `localhost` / `127.0.0.1`.
    #[must_use]
    pub fn encrypt_history(mut self) -> Self {
        self.encrypt_history = true;
        self
    }

    /// The build directory under `public/` (default `build`): the manifest is `public/<dir>/manifest.json` and asset
    /// URLs start with `/<dir>/`. Letters, digits and `_ . / @ -` without `.` or `..` segments; anything else makes
    /// the app fail to boot.
    #[must_use]
    pub fn build_dir(mut self, dir: impl Into<String>) -> Self {
        self.build_dir = dir.into();
        self
    }

    /// The file holding the Vite dev server's URL while it runs, relative to the app root (default
    /// `storage/framework/vite.hot`). Only debug builds read it, never under `APP_ENV=production`, and only an
    /// `http(s)` URL on `127.0.0.1`, `localhost` or `[::1]` is used.
    #[must_use]
    pub fn hot_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.hot_file = path.into();
        self
    }
}

/// Installs Alloy on an app: `use smeltery::alloy::AlloyExt as _;` then `app.alloy(Alloy::new().root::<Root>())`.
pub trait AlloyExt: Sized {
    /// Answer Alloy pages on every web route (the Inertia protocol, as a web middleware inside the session stack),
    /// render `@alloy`, `@alloyHead` and `@vite` in views, set the `XSRF-TOKEN` cookie Inertia's client sends back
    /// as `X-XSRF-TOKEN`, and name `X-Inertia` in every web response's `Vary` header. When the server starts
    /// (`serve`, not console commands or `work`), logs where the assets come from; a release build without
    /// `public/build/manifest.json` logs an error then.
    fn alloy(self, alloy: Alloy) -> Self;
}

impl AlloyExt for AppBuilder {
    fn alloy(self, alloy: Alloy) -> Self {
        let Some((root, root_name, root_type)) = alloy.root else {
            return self.on_boot(|_| async {
                Err(Error::internal(
                    "`.alloy(…)` needs a root template: `Alloy::new().root::<Root>()`",
                ))
            });
        };
        if let Err(e) = vite::check_build_dir(&alloy.build_dir) {
            return self.on_boot(move |_| async move { Err(Error::internal(e)) });
        }
        let inner = Arc::new(Inner {
            root,
            root_name,
            share: alloy.share,
            version: alloy.version,
            all_errors: alloy.all_errors,
            encrypt_history: alloy.encrypt_history,
            vite: vite::Vite::new(alloy.entries, alloy.build_dir, alloy.hot_file),
            checked: Mutex::new(std::collections::HashSet::new()),
        });
        let renderer: Arc<dyn AlloyRenderer> = Arc::new(Renderer(inner.clone()));
        let middleware = inner.clone();
        let booted = inner.clone();
        let serving = inner.clone();
        self.service(inner)
            .service(renderer)
            .xsrf_cookie()
            .vary_web_responses("X-Inertia")
            .web_middleware(move |req, next| protocol::handle(middleware.clone(), req, next))
            .on_boot(move |app| boot(booted, app, root_type))
            .on_serve(move |app| assets_check(serving, app))
    }
}

/// The boot check: the root template's file in debug builds.
async fn boot(inner: Arc<Inner>, app: App, root_type: &'static str) -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let app2 = app.clone();
    // File access off the async workers.
    let template = tokio::task::spawn_blocking(move || {
        let template = app2
            .views()
            .views_dir()
            .join(format!("{}.mold.html", inner.root_name));
        (!template.is_file()).then_some(template)
    })
    .await
    .map_err(|e| Error::internal(format!("the Alloy boot check failed: {e}")))?;
    if let Some(template) = template {
        tracing::warn!(
            root = root_type,
            template = %template.display(),
            "the Alloy root template is missing"
        );
    }
    Ok(())
}

/// The asset check when the server starts (`serve` only, not console commands or `work`, review R2): logs where
/// the assets come from; a release build without the Vite manifest logs an error before the first request (A5).
async fn assets_check(inner: Arc<Inner>, app: App) -> Result<()> {
    let checked = inner.clone();
    let app2 = app.clone();
    // File access off the async workers.
    let (hot, manifest) = tokio::task::spawn_blocking(move || {
        let root = &app2.settings().root;
        let mode = protocol::mode(&app2);
        (
            checked.vite.hot_url(root, mode),
            checked.vite.manifest(root, mode),
        )
    })
    .await
    .map_err(|e| Error::internal(format!("the Alloy asset check failed: {e}")))?;
    let root = &app.settings().root;
    let manifest_path = inner.vite.manifest_path(root);
    match (hot, manifest) {
        (Some(url), _) => tracing::info!(%url, "Alloy assets: the Vite dev server"),
        (None, Ok(Some(_))) => {
            tracing::info!(manifest = %manifest_path.display(), "Alloy assets: the Vite build")
        }
        (None, Err(e)) => {
            tracing::error!(error = %e, "Alloy assets: the Vite manifest cannot be read")
        }
        (None, Ok(None)) if app.settings().env == "testing" => {}
        (None, Ok(None)) if cfg!(debug_assertions) => tracing::warn!(
            manifest = %manifest_path.display(),
            "Alloy assets: none yet; run `npm install` and `smeltery serve` (or `npm run build`)"
        ),
        (None, Ok(None)) => tracing::error!(
            manifest = %manifest_path.display(),
            "Alloy assets: the Vite manifest is missing; pages render without their scripts. Run `npm run build` \
             and deploy public/build/"
        ),
    }
    Ok(())
}

/// `@alloy`, `@alloyHead` and `@vite` for the app's views.
struct Renderer(Arc<Inner>);

impl AlloyRenderer for Renderer {
    fn page(&self, host: &RequestHost, id: &str) -> std::result::Result<String, String> {
        protocol::page_element(host.page_payload(), id)
    }

    fn vite(&self, host: &RequestHost, entries: &[&str]) -> std::result::Result<String, String> {
        let app = host.app();
        self.0
            .vite
            .tags(&app.settings().root, entries, protocol::mode(app))
    }
}

/// The app's current asset version (what `X-Inertia-Version` must equal), `None` when the app has no Alloy. Reads
/// the Vite manifest: blocking file access.
pub fn version(app: &App) -> Option<String> {
    app.service::<Arc<Inner>>().map(|inner| inner.version(app))
}

/// Leave the single-page app: an Inertia visit gets `409` with `X-Inertia-Location` (the client loads the URL
/// itself), any other request a `303` redirect. For OAuth screens, downloads, other apps. Never pass user input: use
/// `Back` for "return to" links.
///
/// ```
/// use smeltery::alloy::{self, Location};
///
/// async fn billing() -> Location {
///     alloy::location("https://billing.example.test/portal")
/// }
/// ```
pub fn location(url: impl Into<String>) -> Location {
    Location { url: url.into() }
}

/// The answer of [`location`].
#[derive(Clone, Debug)]
pub struct Location {
    url: String,
}

impl IntoResponse for Location {
    fn into_response(self) -> Response {
        let Ok(value) = HeaderValue::from_str(&self.url) else {
            return Error::internal(format!("`{}` is not a valid location", self.url))
                .into_response();
        };
        let mut response = StatusCode::SEE_OTHER.into_response();
        response.headers_mut().insert(header::LOCATION, value);
        response.extensions_mut().insert(ExternalLocation(self.url));
        response
    }
}

/// Clear the client's history state with the next page (`clearHistory`), e.g. on logout so the back button cannot
/// show the signed-in pages' props. Call it after `session.invalidate()`, which drops session data.
pub fn clear_history(session: &Session) {
    session.insert(protocol::CLEAR_HISTORY_KEY, true);
}
