//! The application: settings, services, routes, and the builder that wires them.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};

use axum::extract::FromRequestParts;
use tokio_util::sync::CancellationToken;

use crate::auth::verification::{EmailVerifier, ModelVerifier};
use crate::auth::{Authenticatable, ModelProvider, MustVerifyEmail, Throttle, UserProvider};
use crate::config::Settings;
use crate::console::Commands;
use crate::db::migration::Migrator;
use crate::db::seed::Seeders;
use crate::db::{Db, DbOptions, PrimaryKeyOf, Record};
use crate::error::{Error, Result};
use crate::http::TrustedProxies;
use crate::middleware::{BoxedMiddleware, ErasedMiddleware, Family, Middleware};
use crate::routing::{RouteInfo, Router};

/// A boxed, sendable future, as boot hooks return.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

type BootHook = Box<dyn FnOnce(App) -> BoxFuture<'static, Result<()>> + Send>;
type StartHook = Box<dyn FnOnce(App) -> BoxFuture<'static, Result<Background>> + Send>;
type ServeHook = Box<dyn FnOnce(App) -> BoxFuture<'static, Result<()>> + Send>;

/// Background work started by an [`AppBuilder::on_start`] hook (e.g. Watchfire's agents).
///
/// It wraps a future that completes once the work has fully stopped after the app's
/// [shutdown token](App::shutdown_token) was cancelled. `serve` and `work` wait for it within the
/// app's shutdown budget (`SHUTDOWN_TIMEOUT`).
pub struct Background {
    done: BoxFuture<'static, ()>,
}

impl std::fmt::Debug for Background {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Background").finish_non_exhaustive()
    }
}

impl Background {
    /// Background work that is finished when `done` completes.
    pub fn new(done: impl Future<Output = ()> + Send + 'static) -> Self {
        Self {
            done: Box::pin(done),
        }
    }

    /// Wait until the work has stopped. The work itself should run in spawned tasks: this
    /// future only waits for them.
    pub async fn wait(self) {
        self.done.await;
    }
}
type RouteFn = Box<dyn FnOnce(&mut Router) + Send>;

/// The running application: settings, services and the route table.
///
/// Cheap to clone (an `Arc`). Handlers receive it by naming it as an argument; everything
/// else (providers, commands, tests) gets it from [`AppBuilder::build`].
///
/// ```
/// use smeltery_core::App;
///
/// #[derive(Clone)]
/// struct Greeting(String);
///
/// async fn hello(app: App) -> String {
///     app.service::<Greeting>().map(|g| g.0.clone()).unwrap_or_default()
/// }
/// ```
#[derive(Clone)]
pub struct App {
    inner: Arc<Inner>,
}

struct Inner {
    settings: Settings,
    services: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    routes: Vec<RouteInfo>,
    names: HashMap<String, String>,
    shutdown: CancellationToken,
    views: smeltery_mold::Engine,
    migrator: Migrator,
    seeders: Seeders,
    web: Option<crate::session::web::WebConfig>,
    key_error: Option<String>,
    users: Option<Arc<dyn UserProvider>>,
    verifier: Option<Arc<dyn EmailVerifier>>,
    guards: Vec<Arc<dyn crate::auth::Guard>>,
    credential_listeners: Vec<Arc<dyn crate::auth::CredentialListener>>,
    auth_model: Option<TypeId>,
    throttle: Throttle,
    account_throttle: Throttle,
    client_throttle: Throttle,
    verify_throttle: Throttle,
    reset_throttle: Throttle,
    dont_flash: Vec<String>,
    tasks: tokio_util::task::TaskTracker,
    force_csrf: AtomicBool,
    start: Mutex<Vec<StartHook>>,
    serve: Mutex<Vec<ServeHook>>,
    serving: AtomicBool,
    web_only: AtomicBool,
    trusted: TrustedProxies,
    xsrf: bool,
    web_vary: Vec<&'static str>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("name", &self.inner.settings.name)
            .field("routes", &self.inner.routes.len())
            .finish_non_exhaustive()
    }
}

impl App {
    /// The framework settings.
    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    /// A registered service (or config struct) by type.
    pub fn service<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        let services = self
            .inner
            .services
            .read()
            .unwrap_or_else(|e| e.into_inner());
        services
            .get(&TypeId::of::<T>())
            .and_then(|s| Arc::clone(s).downcast::<T>().ok())
    }

    /// A config struct registered with [`AppBuilder::config`]. Same as [`App::service`].
    pub fn config<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.service::<T>()
    }

    /// Register (or replace) a service while the app runs, e.g. from a boot hook.
    pub fn insert_service<T: Send + Sync + 'static>(&self, service: T) {
        let mut services = self
            .inner
            .services
            .write()
            .unwrap_or_else(|e| e.into_inner());
        services.insert(TypeId::of::<T>(), Arc::new(service));
    }

    /// The service of type `T`, created with `make` and registered when there is none yet.
    /// Concurrent callers all get the instance registered first (`make` runs outside the lock,
    /// so it may read other services).
    pub(crate) fn service_or_insert_with<T: Send + Sync + 'static>(
        &self,
        make: impl FnOnce() -> T,
    ) -> Arc<T> {
        if let Some(found) = self.service::<T>() {
            return found;
        }
        let made = Arc::new(make());
        let mut services = self
            .inner
            .services
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(found) = services
            .get(&TypeId::of::<T>())
            .and_then(|s| Arc::clone(s).downcast::<T>().ok())
        {
            return found;
        }
        services.insert(
            TypeId::of::<T>(),
            Arc::clone(&made) as Arc<dyn Any + Send + Sync>,
        );
        made
    }

    /// Every route, in registration order.
    pub fn routes(&self) -> &[RouteInfo] {
        &self.inner.routes
    }

    /// The URL path of a named route, with `{param}` placeholders filled from `params`.
    /// Parameters that are not in the path become the query string.
    ///
    /// ```
    /// # async fn demo() -> smeltery_core::Result<()> {
    /// use smeltery_core::{AppBuilder, config::Settings};
    ///
    /// async fn show() {}
    /// let app = AppBuilder::new(Settings::from_env())
    ///     .routes(|r| { r.get("/posts/{post}", show).name("posts.show"); })
    ///     .build()
    ///     .await?
    ///     .app;
    /// assert_eq!(app.url("posts.show", &[("post", "7"), ("tab", "a b")])?, "/posts/7?tab=a+b");
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// An unknown route name, or a placeholder without a value.
    pub fn url(&self, name: &str, params: &[(&str, &str)]) -> Result<String> {
        let path = self
            .inner
            .names
            .get(name)
            .ok_or_else(|| Error::internal(format!("no route named `{name}`")))?;
        crate::routing::fill_path(path, params)
    }

    /// The Mold engine reading this app's templates from `<root>/resources/views` (used to render views in
    /// debug builds, with hot reload).
    pub fn views(&self) -> &smeltery_mold::Engine {
        &self.inner.views
    }

    /// The registered migrations (see [`AppBuilder::migrations`]).
    pub fn migrator(&self) -> &Migrator {
        &self.inner.migrator
    }

    /// The registered seeders (see [`AppBuilder::seeders`]).
    pub fn seeders(&self) -> &Seeders {
        &self.inner.seeders
    }

    pub(crate) fn web_config(&self) -> Option<&crate::session::web::WebConfig> {
        self.inner.web.as_ref()
    }

    /// Why the server cannot start (APP_KEY), if it cannot.
    pub(crate) fn key_error(&self) -> Option<&str> {
        self.inner.key_error.as_deref()
    }

    pub(crate) fn user_provider(&self) -> Option<Arc<dyn UserProvider>> {
        self.inner.users.clone()
    }

    /// The registered guards, in registration order ([`AppBuilder::guard`]; `web` from [`AppBuilder::auth`]).
    pub(crate) fn guards(&self) -> &[Arc<dyn crate::auth::Guard>] {
        &self.inner.guards
    }

    /// The registered credential listeners ([`AppBuilder::credential_listener`]).
    pub(crate) fn credential_listeners(&self) -> &[Arc<dyn crate::auth::CredentialListener>] {
        &self.inner.credential_listeners
    }

    /// The type of the user model registered with [`AppBuilder::auth`], if any (compare with
    /// `TypeId::of::<User>()`).
    pub fn auth_model(&self) -> Option<TypeId> {
        self.inner.auth_model
    }

    /// Whether a stateless guard (one that reads only headers, such as a bearer-token guard) is registered.
    pub fn has_stateless_guard(&self) -> bool {
        self.inner.guards.iter().any(|g| g.stateless())
    }

    pub(crate) fn throttle(&self) -> &Throttle {
        &self.inner.throttle
    }

    /// Login attempts per address and client network.
    pub(crate) fn account_throttle(&self) -> &Throttle {
        &self.inner.account_throttle
    }

    /// Login attempts per client, whatever the address.
    pub(crate) fn client_throttle(&self) -> &Throttle {
        &self.inner.client_throttle
    }

    /// Password reset links per address.
    pub(crate) fn reset_throttle(&self) -> &Throttle {
        &self.inner.reset_throttle
    }

    /// Field names never flashed as old input ([`AppBuilder::dont_flash`]).
    pub(crate) fn dont_flash(&self) -> &[String] {
        &self.inner.dont_flash
    }

    /// Work the app runs in the background for a request (a password reset mail); `serve` and
    /// `work` wait for it within the shutdown budget.
    pub(crate) fn tasks(&self) -> &tokio_util::task::TaskTracker {
        &self.inner.tasks
    }

    /// Run `task` as work the app owns: `serve` and `work` (and an app console command) wait for it within the
    /// shutdown budget after the [shutdown token](App::shutdown_token) is cancelled, so the task must end on that
    /// token. Framework crates use it for their long-running tasks (Sparks' relay of other processes' pushes).
    /// Needs a Tokio runtime.
    ///
    /// A task spawned after `serve` / `work` finished waiting is not awaited; a task of an app dropped without
    /// [`shutdown`](App::shutdown) is not aborted (it runs until its token or the runtime ends); a panic in the task
    /// is not reported anywhere.
    pub fn spawn_owned(&self, task: impl Future<Output = ()> + Send + 'static) {
        self.inner.tasks.spawn(task);
    }

    /// The email verification of the user model, when the app requires it
    /// ([`AppBuilder::verify_email`]).
    pub(crate) fn verifier(&self) -> Option<Arc<dyn EmailVerifier>> {
        self.inner.verifier.clone()
    }

    /// Verification mails per user.
    pub(crate) fn verify_throttle(&self) -> &Throttle {
        &self.inner.verify_throttle
    }

    /// The proxies whose `X-Forwarded-*` headers this app believes (`TRUSTED_PROXIES`). The
    /// HTTP stack uses them for every request's [`ClientInfo`](crate::http::ClientInfo); a
    /// router served outside that stack can call
    /// [`ClientInfo::resolve`](crate::http::ClientInfo::resolve) with them.
    pub fn trusted_proxies(&self) -> &TrustedProxies {
        &self.inner.trusted
    }

    /// CSRF is checked everywhere except under `APP_ENV=testing`, unless a test turned it on.
    /// Whether web responses carry the `XSRF-TOKEN` cookie ([`AppBuilder::xsrf_cookie`]).
    pub(crate) fn xsrf_cookie(&self) -> bool {
        self.inner.xsrf
    }

    /// The headers every web response names in `Vary` ([`AppBuilder::vary_web_responses`]).
    pub(crate) fn web_vary(&self) -> &[&'static str] {
        &self.inner.web_vary
    }

    pub(crate) fn csrf_enabled(&self) -> bool {
        self.inner.settings.env != "testing" || self.inner.force_csrf.load(Ordering::Relaxed)
    }

    pub(crate) fn force_csrf(&self) {
        self.inner.force_csrf.store(true, Ordering::Relaxed);
    }

    /// Cancelled when the app shuts down; background work watches it.
    pub fn shutdown_token(&self) -> &CancellationToken {
        &self.inner.shutdown
    }

    /// Ask the app to shut down (the server stops accepting and drains).
    pub fn shutdown(&self) {
        self.inner.shutdown.cancel();
    }

    /// Whether this process serves the app's HTTP routes (`serve`), as opposed to `work` or a
    /// console command. Start hooks read it to decide what to serve themselves.
    pub fn serves_http(&self) -> bool {
        self.inner.serving.load(Ordering::Relaxed)
    }

    pub(crate) fn mark_serving(&self) {
        self.inner.serving.store(true, Ordering::Relaxed);
    }

    /// Run the [`AppBuilder::on_serve`] hooks, once (later calls run nothing).
    pub(crate) async fn run_serve_hooks(&self) -> Result<()> {
        let hooks = std::mem::take(
            &mut *self
                .inner
                .serve
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for hook in hooks {
            hook(self.clone()).await?;
        }
        Ok(())
    }

    /// Whether [`AppBuilder::on_start`] hooks are registered and not started yet.
    pub fn has_background(&self) -> bool {
        !self
            .inner
            .start
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    /// Drop the [`AppBuilder::on_start`] hooks without running them, so this process runs no
    /// background work: `serve --no-agents` (a web-only process next to a `work` process).
    pub fn skip_background(&self) {
        self.set_web_only();
        self.inner
            .start
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Mark this process as web-only: when it serves HTTP, its background work runs in another process (`work`).
    /// [`skip_background`](Self::skip_background) (`serve --no-agents`) marks it, and Watchfire does with
    /// `WATCHFIRE_IN_SERVE=false`, and Anvil with `ANVIL_IN_SERVE=false` (the sockets are in an `anvil` process).
    /// Under `PUBSUB_DRIVER=auto` such a process uses the shared
    /// [PubSub](crate::pubsub) driver.
    pub fn set_web_only(&self) {
        self.inner.web_only.store(true, Ordering::Relaxed);
    }

    /// Whether [`set_web_only`](Self::set_web_only) was called.
    pub fn is_web_only(&self) -> bool {
        self.inner.web_only.load(Ordering::Relaxed)
    }

    /// Run the [`AppBuilder::on_start`] hooks (once; later calls start nothing) and return
    /// their background work as one, or `None` when there is none. `serve` and `work` call it;
    /// console commands and tests do not, so they run without background work.
    ///
    /// After the hooks it starts the app's [PubSub](crate::pubsub) (once), whose driver under
    /// `PUBSUB_DRIVER=auto` follows what this process runs: `serve` with its background work, a web-only `serve`
    /// ([`set_web_only`](Self::set_web_only)) or `work`.
    ///
    /// # Errors
    /// A hook fails (the work started by earlier hooks keeps running until the shutdown token
    /// is cancelled), or the configured `PUBSUB_DRIVER` cannot start.
    pub async fn start_background(&self) -> Result<Option<Background>> {
        let hooks = std::mem::take(
            &mut *self
                .inner
                .start
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        // An explicit PUBSUB_DRIVER that cannot work stops the start before any background work runs.
        crate::pubsub::check(self)?;
        let mut started = Vec::with_capacity(hooks.len());
        for hook in hooks {
            started.push(hook(self.clone()).await?);
        }
        // After the hooks: Watchfire marks a web-only process in its own (`WATCHFIRE_IN_SERVE=false`).
        let role = match (self.serves_http(), self.is_web_only()) {
            (true, false) => crate::pubsub::Role::Serve,
            (true, true) => crate::pubsub::Role::WebOnly,
            (false, _) => crate::pubsub::Role::Work,
        };
        crate::pubsub::start(self, role).await?;
        if started.is_empty() {
            return Ok(None);
        }
        // The work itself runs in tasks the hooks spawned; these futures only wait for it, so
        // waiting one after the other takes as long as the slowest.
        Ok(Some(Background::new(async move {
            for background in started {
                background.wait().await;
            }
        })))
    }
}

/// A handle to an [`App`] that does not keep it alive: services stored inside the app hold
/// this instead of an `App`, so the app is freed when its last `App` handle is dropped.
#[derive(Clone)]
pub struct WeakApp {
    inner: Weak<Inner>,
}

impl std::fmt::Debug for WeakApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeakApp")
            .field("alive", &(self.inner.strong_count() > 0))
            .finish()
    }
}

impl WeakApp {
    /// The app, while it is alive.
    pub fn upgrade(&self) -> Option<App> {
        self.inner.upgrade().map(|inner| App { inner })
    }
}

impl App {
    /// A [`WeakApp`] for this app (for services stored inside it).
    ///
    /// ```
    /// # async fn demo() -> smeltery_core::Result<()> {
    /// use smeltery_core::{AppBuilder, config::Settings};
    ///
    /// let app = AppBuilder::new(Settings::from_env()).build().await?.app;
    /// let weak = app.downgrade();
    /// assert!(weak.upgrade().is_some());
    /// drop(app);
    /// assert!(weak.upgrade().is_none());
    /// # Ok(())
    /// # }
    /// ```
    pub fn downgrade(&self) -> WeakApp {
        WeakApp {
            inner: Arc::downgrade(&self.inner),
        }
    }
}

impl FromRequestParts<App> for App {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        _parts: &mut http::request::Parts,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        Ok(app.clone())
    }
}

/// Collects settings, services, routes and middleware, then [`build`](Self::build)s the
/// [`App`]. `bootstrap/app.rs` in a generated app is one function over this builder.
///
/// ```
/// use smeltery_core::{AppBuilder, config::Settings};
///
/// #[derive(Clone)]
/// struct AppConfig {
///     name: String,
/// }
///
/// async fn home() -> &'static str {
///     "home"
/// }
///
/// fn build(app: AppBuilder) -> AppBuilder {
///     app.config(AppConfig { name: "Demo".into() })
///         .routes(|r| {
///             r.get("/", home).name("home");
///         })
/// }
/// # let _ = build(AppBuilder::new(Settings::from_env()));
/// ```
pub struct AppBuilder {
    settings: Settings,
    services: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
    route_fns: Vec<(Kind, String, RouteFn)>,
    aliases: HashMap<String, ErasedMiddleware>,
    families: Vec<(String, Family)>,
    guards: Vec<Arc<dyn crate::auth::Guard>>,
    credential_listeners: Vec<Arc<dyn crate::auth::CredentialListener>>,
    model_listeners: Vec<Arc<dyn crate::db::ModelListener>>,
    /// A guard named `web` given to [`AppBuilder::guard`]: the name is core's (the session).
    custom_web_guard: bool,
    /// `.auth::<U>()` was called with two different models.
    second_auth_model: Option<&'static str>,
    global: Vec<ErasedMiddleware>,
    web: Vec<ErasedMiddleware>,
    xsrf: bool,
    web_vary: Vec<&'static str>,
    boot: Vec<BootHook>,
    start: Vec<StartHook>,
    serve: Vec<ServeHook>,
    migrator: Migrator,
    seeders: Seeders,
    commands: Commands,
    serve_commands: Vec<ServeCommand>,
    users: Option<(TypeId, Arc<dyn UserProvider>)>,
    verifier: Option<(TypeId, Arc<dyn EmailVerifier>)>,
    health: bool,
    dont_flash: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Web,
    Api,
}

impl std::fmt::Debug for AppBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppBuilder")
            .field("settings", &self.settings)
            .field("route_groups", &self.route_fns.len())
            .field("middleware", &self.aliases.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// The result of [`AppBuilder::build`]: the app and its HTTP service.
#[derive(Debug)]
#[non_exhaustive]
pub struct Built {
    /// The app.
    pub app: App,
    /// The HTTP router, with every framework layer applied.
    pub router: axum::Router,
}

/// A command registered with [`AppBuilder::serve_command`].
pub(crate) struct ServeCommand {
    pub(crate) name: &'static str,
    pub(crate) about: &'static str,
    #[allow(clippy::type_complexity)]
    pub(crate) run:
        Box<dyn FnOnce(Built, crate::console::Args) -> BoxFuture<'static, Result<()>> + Send>,
}

impl AppBuilder {
    /// A builder with these settings and nothing registered.
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            services: HashMap::new(),
            route_fns: Vec::new(),
            aliases: HashMap::new(),
            families: vec![(
                "throttle".to_owned(),
                Arc::new(crate::rate_limit::family) as Family,
            )],
            guards: Vec::new(),
            credential_listeners: Vec::new(),
            model_listeners: Vec::new(),
            custom_web_guard: false,
            second_auth_model: None,
            global: Vec::new(),
            web: Vec::new(),
            xsrf: false,
            web_vary: Vec::new(),
            boot: Vec::new(),
            start: Vec::new(),
            serve: Vec::new(),
            migrator: Migrator::new(),
            seeders: Seeders::new(),
            commands: Commands::new(),
            serve_commands: Vec::new(),
            users: None,
            verifier: None,
            health: true,
            dont_flash: Vec::new(),
        }
    }

    /// Leave out the framework's health route, `GET /up` (see [`AppBuilder::build`]).
    pub fn without_health_route(mut self) -> Self {
        self.health = false;
        self
    }

    /// The settings the app will run with.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Change the settings before building, e.g. in tests.
    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// Register a config struct, read in handlers with [`Config<T>`](crate::config::Config)
    /// and elsewhere with [`App::config`].
    pub fn config<T: Send + Sync + 'static>(self, config: T) -> Self {
        self.service(config)
    }

    /// Register a service, read with [`App::service`].
    pub fn service<T: Send + Sync + 'static>(mut self, service: T) -> Self {
        self.services.insert(TypeId::of::<T>(), Arc::new(service));
        self
    }

    /// A service registered on this builder so far (crate code that appends to a list kept as a service).
    pub(crate) fn registered_service<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.services
            .get(&TypeId::of::<T>())
            .and_then(|s| s.downcast_ref::<T>())
    }

    /// Add browser routes (`routes/web.rs`).
    pub fn routes(mut self, routes: impl FnOnce(&mut Router) + Send + 'static) -> Self {
        self.route_fns
            .push((Kind::Web, String::new(), Box::new(routes)));
        self
    }

    /// Add API routes (`routes/api.rs`): every path is prefixed with `/api`.
    pub fn api_routes(mut self, routes: impl FnOnce(&mut Router) + Send + 'static) -> Self {
        self.route_fns
            .push((Kind::Api, "/api".to_owned(), Box::new(routes)));
        self
    }

    /// Add API routes (no sessions, no CSRF check, like [`AppBuilder::api_routes`]) under
    /// `prefix` instead of `/api`, e.g. a framework crate's `/_watchfire/api`.
    pub fn api_routes_at(
        mut self,
        prefix: &str,
        routes: impl FnOnce(&mut Router) + Send + 'static,
    ) -> Self {
        self.route_fns
            .push((Kind::Api, prefix.to_owned(), Box::new(routes)));
        self
    }

    /// Register route middleware under an alias, used as `.middleware("alias")` on routes
    /// and groups.
    ///
    /// ```
    /// use smeltery_core::middleware::{Next, Request};
    /// use smeltery_core::{AppBuilder, Response};
    ///
    /// async fn add_header(req: Request, next: Next) -> Response {
    ///     let mut res = next.run(req).await;
    ///     res.headers_mut().insert("x-demo", "1".parse().unwrap());
    ///     res
    /// }
    ///
    /// fn build(app: AppBuilder) -> AppBuilder {
    ///     app.middleware("demo", add_header)
    /// }
    /// ```
    pub fn middleware(mut self, alias: impl Into<String>, middleware: impl Middleware) -> Self {
        self.aliases
            .insert(alias.into(), ErasedMiddleware::new(middleware));
        self
    }

    /// Register a middleware family: aliases `<prefix>:<arguments>` (like `throttle:5,1` or `auth:web`) whose
    /// middleware `make` builds per route from the arguments and the route ("METHODS /pattern", e.g.
    /// `POST /login`). `make` runs while the app builds, once per route that uses the alias; an error from it
    /// (invalid arguments) fails [`build`](Self::build). Core registers the families `throttle` and `auth`.
    /// [`build`](Self::build) fails when two families share a prefix, when a prefix is empty or holds `:`, or when
    /// a plain alias starts with `<prefix>:`.
    ///
    /// ```
    /// use smeltery_core::middleware::{BoxedMiddleware, Next, Request};
    /// use smeltery_core::{AppBuilder, Error};
    ///
    /// fn build(app: AppBuilder) -> AppBuilder {
    ///     // `role:admin`, `role:editor`: only the named role passes.
    ///     app.middleware_family("role", |args, _route| {
    ///         if args.is_empty() {
    ///             return Err(Error::internal("write `role:<name>`"));
    ///         }
    ///         let role = args.to_owned();
    ///         Ok(BoxedMiddleware::new(move |req: Request, next: Next| {
    ///             let role = role.clone();
    ///             async move {
    ///                 let _ = role; // check the user's role here
    ///                 next.run(req).await
    ///             }
    ///         }))
    ///     })
    /// }
    /// # let _ = build;
    /// ```
    pub fn middleware_family<F>(mut self, prefix: impl Into<String>, make: F) -> Self
    where
        F: Fn(&str, &str) -> Result<BoxedMiddleware> + Send + Sync + 'static,
    {
        let make: Family = Arc::new(move |args: &str, route: &str| make(args, route).map(|m| m.0));
        self.families.push((prefix.into(), make));
        self
    }

    /// Register a guard: it resolves a request's [`Principal`](crate::auth::Principal) for the `auth:<name>`
    /// middleware, the [`Authenticated`](crate::auth::Authenticated) extractor and
    /// [`auth::authenticate`](crate::auth::authenticate). [`AppBuilder::auth`] registers core's `web` guard (the
    /// signed-in session). [`build`](Self::build) fails for two guards with one name, a name that is not
    /// lowercase ASCII letters, digits, `_` and `-`, or the name `web` (core's).
    pub fn guard(mut self, guard: impl crate::auth::Guard) -> Self {
        if guard.name() == crate::auth::WEB_GUARD {
            self.custom_web_guard = true;
        }
        self.guards.push(Arc::new(guard));
        self
    }

    /// Register a [`CredentialListener`](crate::auth::CredentialListener): it runs, in registration order, when a
    /// user's password changes (a reset, [`Auth::set_password`](crate::auth::Auth::set_password),
    /// [`Auth::logout_other_devices`](crate::auth::Auth::logout_other_devices),
    /// [`auth::password_changed`](crate::auth::password_changed)), before that request answers.
    pub fn credential_listener(mut self, listener: impl crate::auth::CredentialListener) -> Self {
        self.credential_listeners.push(Arc::new(listener));
        self
    }

    /// Register a [`ModelListener`](crate::db::ModelListener): it is told, in registration order, about every
    /// successful [`Record`] `create`, `update` (one that changed something) and `delete` on the app's database
    /// (see [`db::ModelEvent`](crate::db::ModelEvent)). An app without listeners does no listener work at all.
    pub fn model_listener(mut self, listener: impl crate::db::ModelListener) -> Self {
        self.model_listeners.push(Arc::new(listener));
        self
    }

    /// Run middleware on every request, in registration order (the first registered runs
    /// first).
    pub fn global_middleware(mut self, middleware: impl Middleware) -> Self {
        self.global.push(ErasedMiddleware::new(middleware));
        self
    }

    /// Run middleware on every web route (`routes/web.rs`), inside the session stack: after the
    /// session is loaded and the CSRF token checked, before the session is saved, outside the
    /// route's own middleware. Several run in registration order (the first registered runs
    /// first). API routes never run them.
    ///
    /// Unlike [`global_middleware`](Self::global_middleware), it can read and change the
    /// [`Session`](crate::session::Session) (in the request extensions) and the change is kept.
    /// Frontend crates use it: Alloy's Inertia protocol handling runs here.
    ///
    /// ```
    /// use smeltery_core::middleware::{Next, Request};
    /// use smeltery_core::session::Session;
    /// use smeltery_core::{AppBuilder, Response};
    ///
    /// async fn count_visits(req: Request, next: Next) -> Response {
    ///     if let Some(session) = req.extensions().get::<Session>() {
    ///         let visits = session.get::<u64>("visits").unwrap_or(0);
    ///         session.insert("visits", visits + 1);
    ///     }
    ///     next.run(req).await
    /// }
    ///
    /// fn build(app: AppBuilder) -> AppBuilder {
    ///     app.web_middleware(count_visits)
    /// }
    /// ```
    pub fn web_middleware(mut self, middleware: impl Middleware) -> Self {
        self.web.push(ErasedMiddleware::new(middleware));
        self
    }

    /// Set the `XSRF-TOKEN` cookie on every web response and accept its value back in the
    /// `X-XSRF-TOKEN` header, what Inertia's client does on every request (Alloy turns it on).
    ///
    /// The cookie holds the session's CSRF token masked with a fresh pad per response (like
    /// `@csrf`); it is readable by the page's JavaScript (not `HttpOnly`), `SameSite=Lax`,
    /// `Secure` when `APP_URL` is https, and lasts for the browser session. Off by default.
    pub fn xsrf_cookie(mut self) -> Self {
        self.xsrf = true;
        self
    }

    /// Never flash these fields back as old input after a failed validation, on top of the fields
    /// that look like secrets: names holding `password`,
    /// `passwd`, `passphrase`, `passcode`, `secret`, `token`, `apikey`, `privatekey`,
    /// `accesskey`, `cardnumber`, `creditcard`, `securitycode`, `twofactor` or `recoverycode`
    /// once everything but letters and digits is left out, and names with one of the words
    /// `key`, `pin`, `otp`, `totp`, `mfa`, `2fa`, `tfa`, `auth`, `card`, `cvv`, `cvc`, `cvv2`,
    /// `iban`, `ssn`, `answer` or `recovery` (split at anything but letters and digits and at
    /// camelCase; letter case ignored).
    ///
    /// ```
    /// use smeltery_core::AppBuilder;
    ///
    /// fn build(app: AppBuilder) -> AppBuilder {
    ///     app.dont_flash(&["nickname", "date_of_birth"])
    /// }
    /// ```
    pub fn dont_flash(mut self, fields: &[&str]) -> Self {
        self.dont_flash
            .extend(fields.iter().map(|field| (*field).to_owned()));
        self
    }

    /// Name `header` in the `Vary` header of every web response, the session stack's own
    /// answers included (the CSRF failure, the redirect back after a failed validation), so a
    /// cache never serves an answer meant for another value of that request header. Alloy adds
    /// `X-Inertia`: a page answers JSON or HTML depending on it.
    pub fn vary_web_responses(mut self, header: &'static str) -> Self {
        if !self.web_vary.iter().any(|h| h.eq_ignore_ascii_case(header)) {
            self.web_vary.push(header);
        }
        self
    }

    /// Register migrations: `database/migrations/mod.rs`'s `register` function.
    pub fn migrations(mut self, register: impl FnOnce(&mut Migrator)) -> Self {
        register(&mut self.migrator);
        self
    }

    /// Register seeders: `database/seeders/mod.rs`'s `register` function.
    pub fn seeders(mut self, register: impl FnOnce(&mut Seeders)) -> Self {
        register(&mut self.seeders);
        self
    }

    /// Register console commands: `app/commands/mod.rs`'s `register` function.
    pub fn commands(mut self, register: impl FnOnce(&mut Commands)) -> Self {
        register(&mut self.commands);
        self
    }

    /// Use `U` (the app's user model) for authentication: `Auth` finds users by id and by
    /// their `email` column and stores remember-me tokens in `remember_token`. Also registers
    /// the `web` guard (the signed-in session) and the `auth`, `guest`, `verified` and
    /// `password.confirm` middleware aliases (`verified` lets every signed-in user, but no guest,
    /// through until [`verify_email`](Self::verify_email) is called; `password.confirm` lets a
    /// session through that confirmed its password within `AUTH_PASSWORD_TIMEOUT`, see
    /// [`Auth::confirm_password`](crate::auth::Auth::confirm_password)).
    pub fn auth<U>(mut self) -> Self
    where
        U: Record
            + Authenticatable
            + sea_orm::FromQueryResult
            + sea_orm::ModelTrait<Entity = <U as Record>::Entity>,
        PrimaryKeyOf<U>: From<i64>,
    {
        if self
            .users
            .as_ref()
            .is_some_and(|(model, _)| *model != TypeId::of::<U>())
        {
            self.second_auth_model = Some(std::any::type_name::<U>());
        }
        self.users = Some((TypeId::of::<U>(), Arc::new(ModelProvider::<U>::new())));
        if !self
            .guards
            .iter()
            .any(|g| g.name() == crate::auth::WEB_GUARD)
        {
            self.guards.insert(0, Arc::new(crate::auth::WebGuard));
        }
        self.middleware("auth", crate::auth::require_auth)
            .middleware("guest", crate::auth::require_guest)
            .middleware("verified", crate::auth::verification::require_verified)
            .middleware("password.confirm", crate::auth::require_confirmed_password)
    }

    /// Require verified email addresses from `U`, the user model registered with
    /// [`auth`](Self::auth), which implements
    /// [`MustVerifyEmail`](crate::auth::MustVerifyEmail): the `verified` middleware then sends
    /// users whose `email_verified_at` is empty to the `verification.notice` route, and
    /// [`EmailVerificationRequest`](crate::auth::EmailVerificationRequest) sets the column (see
    /// [`crate::auth::verification`]).
    ///
    /// ```
    /// # extern crate smeltery_core as smeltery;
    /// # use smeltery::db::prelude::*;
    /// # #[sea_orm::model]
    /// # #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    /// # #[sea_orm(table_name = "users")]
    /// # pub struct Model {
    /// #     #[sea_orm(primary_key)]
    /// #     pub id: i64,
    /// #     pub email: String,
    /// #     pub email_verified_at: Option<DateTimeUtc>,
    /// #     pub password: String,
    /// #     pub remember_token: Option<String>,
    /// # }
    /// # impl ActiveModelBehavior for ActiveModel {}
    /// # impl smeltery::auth::Authenticatable for Model {
    /// #     fn auth_id(&self) -> i64 { self.id }
    /// #     fn password_hash(&self) -> &str { &self.password }
    /// #     fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
    /// # }
    /// # use Model as User;
    /// impl smeltery::auth::MustVerifyEmail for User {
    ///     fn email(&self) -> &str { &self.email }
    ///     fn email_verified_at(&self) -> Option<DateTimeUtc> { self.email_verified_at }
    /// }
    ///
    /// fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    ///     app.auth::<User>().verify_email::<User>()
    /// }
    /// # fn main() {}
    /// ```
    ///
    /// [`build`](Self::build) fails when `U` is not the model given to `auth`.
    pub fn verify_email<U>(mut self) -> Self
    where
        U: Record + MustVerifyEmail + sea_orm::FromQueryResult,
        PrimaryKeyOf<U>: From<i64>,
    {
        self.verifier = Some((TypeId::of::<U>(), Arc::new(ModelVerifier::<U>::new())));
        self.middleware("verified", crate::auth::verification::require_verified)
    }

    /// The registered console commands.
    pub(crate) fn take_commands(&mut self) -> Commands {
        std::mem::take(&mut self.commands)
    }

    /// The registered [`serve_command`](Self::serve_command)s.
    pub(crate) fn take_serve_commands(&mut self) -> Vec<ServeCommand> {
        std::mem::take(&mut self.serve_commands)
    }

    /// Register a console command that runs a server of its own from the built app, as `serve` does: `run` gets
    /// the [`Built`] app (its router with every framework layer) and the words after the command name, and
    /// usually ends in [`serve_on`](crate::serve_on) on a listener of its own. Anvil's `anvil` command (a process
    /// that serves only the WebSocket endpoint) is one.
    ///
    /// The console builds the app only for this command. A name the framework uses (`serve`, `migrate` …) is
    /// never run; a serve command wins over an app command of the same name. When `run` serves through
    /// [`serve_on`](crate::serve_on), the [`on_serve`](Self::on_serve) hooks run in that process too, so a hook must
    /// tolerate a process that serves only part of the app.
    pub fn serve_command<F, Fut>(mut self, name: &'static str, about: &'static str, run: F) -> Self
    where
        F: FnOnce(Built, crate::console::Args) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.serve_commands.push(ServeCommand {
            name,
            about,
            run: Box::new(move |built, args| Box::pin(run(built, args))),
        });
        self
    }

    /// Run an async hook while the app builds, after routes are collected and before the
    /// server starts. Hooks run in registration order; one failing aborts the build.
    ///
    /// Use it to open connections and register services with [`App::insert_service`].
    pub fn on_boot<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce(App) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.boot.push(Box::new(move |app| Box::pin(hook(app))));
        self
    }

    /// Start background work when the app runs: `serve` (with the HTTP server) and `work`
    /// (without it) run these hooks after the build, in registration order. Console commands
    /// and `TestApp` do not run them.
    ///
    /// The hook returns a [`Background`] that completes once its work has stopped after the
    /// app's [shutdown token](App::shutdown_token) is cancelled; the app waits for it within
    /// its shutdown budget. Watchfire (`.agents(...)`) is built on this hook.
    pub fn on_start<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce(App) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Background>> + Send + 'static,
    {
        self.start.push(Box::new(move |app| Box::pin(hook(app))));
        self
    }

    /// Run an async hook when the app starts serving HTTP (`serve`), after the build and before
    /// the first request, in registration order; one failing stops the server from starting.
    /// `work`, console commands and `TestApp` never run these hooks. Unlike
    /// [`on_start`](Self::on_start) hooks they run under `serve --no-agents` too.
    ///
    /// Use it for start-up checks that only matter to a web server, such as Alloy's log line
    /// about where the Vite assets come from.
    pub fn on_serve<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce(App) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.serve.push(Box::new(move |app| Box::pin(hook(app))));
        self
    }

    /// Collect the routes, connect the database (when `DATABASE_URL` is set), create the
    /// [`App`], run the boot hooks and build the HTTP stack.
    ///
    /// The framework adds one route of its own: the health check `GET /up`, which answers
    /// `200` with the text `OK` and `Cache-Control: no-store`, outside sessions and CSRF. An
    /// app that declares `GET /up` itself keeps its own route;
    /// [`without_health_route`](Self::without_health_route) leaves it out.
    ///
    /// # Errors
    /// An invalid or duplicate route, an unknown middleware alias, a duplicate route
    /// name, an invalid `TRUSTED_PROXIES` entry or `FRAME_OPTIONS` value, a
    /// [`verify_email`](Self::verify_email) model
    /// that is not the [`auth`](Self::auth) model or a `verification.verify` route without
    /// `{id}` and `{hash}`, a database that cannot be reached, or a
    /// failing boot hook.
    pub async fn build(mut self) -> Result<Built> {
        let mut collected = Vec::new();
        for (kind, prefix, routes) in self.route_fns {
            let mut router = Router::new(kind, &prefix);
            routes(&mut router);
            collected.extend(router.into_routes());
        }
        if self.health {
            crate::routing::add_health_route(&mut collected);
        }
        if let Some(model) = self.second_auth_model {
            return Err(Error::internal(format!(
                "`.auth::<…>()` is called with two different user models (the second is `{model}`): an app has one"
            )));
        }
        if self.custom_web_guard {
            return Err(Error::internal(
                "the guard name `web` belongs to core's session guard: give the guard another name",
            ));
        }
        let guards = check_guards(&self.guards)?;
        let families = families(self.families, &self.aliases, &guards)?;
        let table = crate::routing::RouteTable::new(collected, &self.aliases, &families)?;
        if let Some((verified, _)) = &self.verifier
            && self.users.as_ref().map(|(model, _)| model) != Some(verified)
        {
            return Err(Error::internal(
                "`.verify_email::<User>()` needs the same model in `.auth::<User>()`",
            ));
        }
        // `EmailVerificationRequest` reads `id` and `hash` from the path: a route without them
        // would answer every link with 403.
        if self.verifier.is_some()
            && let Some(path) = table.names().get("verification.verify")
            && !(path.contains("{id}") && path.contains("{hash}"))
        {
            return Err(Error::internal(format!(
                "the route `verification.verify` ({path}) needs the parameters {{id}} and {{hash}}, \
                 e.g. /email/verify/{{id}}/{{hash}}"
            )));
        }
        let trusted =
            TrustedProxies::parse(&self.settings.trusted_proxies).map_err(Error::internal)?;
        crate::auth::configure_hashing(self.settings.hash_concurrency, self.settings.hash_queue);

        // Web routes run inside sessions, which are encrypted with APP_KEY: the server of an
        // app with web routes refuses to start without a usable key (console commands such
        // as `migrate` or `route:list` still work).
        let driver = crate::session::store::Driver::from_settings(&self.settings.session_driver)?;
        let (web, key_error) = match crate::crypto::Keys::from_settings(&self.settings) {
            Ok(keys) => (Some(crate::session::web::WebConfig { keys, driver }), None),
            Err(e) if table.has_web() => (None, Some(e.to_string())),
            Err(_) => (None, None),
        };

        if !self.settings.database_url.is_empty() {
            let options = DbOptions::default()
                .pool_max(self.settings.db_pool_max)
                .connect_timeout(self.settings.db_connect_timeout)
                .root(self.settings.root.clone());
            let db = Db::connect_with(&self.settings.database_url, options)
                .await?
                .with_listeners(std::mem::take(&mut self.model_listeners));
            self.services.insert(TypeId::of::<Db>(), Arc::new(db));
        }

        let pubsub = crate::pubsub::PubSub::new(&self.settings)?;
        self.services
            .insert(TypeId::of::<crate::pubsub::PubSub>(), Arc::new(pubsub));

        let views_dir = self.settings.views_dir();
        // `Template::render` outside the web stack uses the global engine; point it at this app. In a process
        // with several apps (tests) the first one wins there, while the web stack always uses `App::views`.
        let _ = smeltery_mold::set_global_views_dir(views_dir.clone());
        let app = App {
            inner: Arc::new(Inner {
                views: smeltery_mold::Engine::new(views_dir),
                settings: self.settings,
                services: RwLock::new(self.services),
                routes: table.infos(),
                names: table.names(),
                shutdown: CancellationToken::new(),
                migrator: self.migrator,
                seeders: self.seeders,
                web,
                key_error,
                auth_model: self.users.as_ref().map(|(model, _)| *model),
                users: self.users.map(|(_, users)| users),
                verifier: self.verifier.map(|(_, verifier)| verifier),
                guards,
                credential_listeners: self.credential_listeners,
                throttle: Throttle::new(crate::auth::MAX_ATTEMPTS),
                account_throttle: Throttle::with_window(
                    crate::auth::ACCOUNT_MAX_ATTEMPTS,
                    crate::auth::ACCOUNT_WINDOW,
                ),
                client_throttle: Throttle::new(crate::auth::CLIENT_MAX_ATTEMPTS),
                verify_throttle: Throttle::new(crate::auth::verification::MAX_SENDS),
                reset_throttle: Throttle::with_window(1, crate::auth::passwords::RESEND_INTERVAL),
                dont_flash: self.dont_flash,
                tasks: tokio_util::task::TaskTracker::new(),
                force_csrf: AtomicBool::new(false),
                start: Mutex::new(self.start),
                serve: Mutex::new(self.serve),
                serving: AtomicBool::new(false),
                web_only: AtomicBool::new(false),
                trusted,
                xsrf: self.xsrf,
                web_vary: self.web_vary,
            }),
        };
        if let Some(db) = app.service::<Db>() {
            db.set_listener_owner(&app);
        }
        for hook in self.boot {
            hook(app.clone()).await?;
        }
        let router = crate::server::http_stack(&app, table, &self.global, &self.web)?;
        Ok(Built { app, router })
    }
}

/// The guards, checked: plain names, each once.
fn check_guards(
    guards: &[Arc<dyn crate::auth::Guard>],
) -> Result<Vec<Arc<dyn crate::auth::Guard>>> {
    let mut seen = std::collections::HashSet::new();
    for guard in guards {
        let name = guard.name();
        if !crate::auth::valid_guard_name(name) {
            return Err(Error::internal(format!(
                "the guard name `{name}` is invalid: use lowercase ASCII letters, digits, `_` and `-`"
            )));
        }
        if !seen.insert(name) {
            return Err(Error::internal(format!("two guards are named `{name}`")));
        }
    }
    Ok(guards.to_vec())
}

/// The middleware families by prefix, with core's `auth` family over `guards`, checked.
fn families(
    registered: Vec<(String, Family)>,
    aliases: &HashMap<String, ErasedMiddleware>,
    guards: &[Arc<dyn crate::auth::Guard>],
) -> Result<HashMap<String, Family>> {
    let guards: Arc<[Arc<dyn crate::auth::Guard>]> = guards.into();
    let auth: Family =
        Arc::new(move |args: &str, _route: &str| crate::auth::auth_family(&guards, args));
    let mut out: HashMap<String, Family> = HashMap::new();
    for (prefix, make) in registered.into_iter().chain([("auth".to_owned(), auth)]) {
        if prefix.is_empty() || prefix.contains(':') {
            return Err(Error::internal(format!(
                "the middleware family prefix `{prefix}` is invalid: it must be non-empty and hold no `:`"
            )));
        }
        let colon = format!("{prefix}:");
        if let Some(alias) = aliases.keys().find(|a| a.starts_with(&colon)) {
            return Err(Error::internal(format!(
                "the middleware alias `{alias}` clashes with the middleware family `{prefix}:`"
            )));
        }
        if out.insert(prefix.clone(), make).is_some() {
            return Err(Error::internal(format!(
                "two middleware families use the prefix `{prefix}:`"
            )));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn services_round_trip_and_boot_hooks_run() {
        #[derive(Debug, PartialEq)]
        struct Name(&'static str);
        #[derive(Debug, PartialEq)]
        struct Booted(bool);

        let built = AppBuilder::new(Settings::from_env())
            .service(Name("x"))
            .on_boot(|app| async move {
                app.insert_service(Booted(true));
                Ok(())
            })
            .build()
            .await
            .unwrap();
        assert_eq!(*built.app.service::<Name>().unwrap(), Name("x"));
        assert_eq!(*built.app.service::<Booted>().unwrap(), Booted(true));
        assert!(built.app.service::<String>().is_none());
    }

    #[tokio::test]
    async fn start_hooks_run_once_and_only_when_asked() {
        use std::sync::atomic::AtomicU32;
        let started = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&started);
        let app = AppBuilder::new(Settings::from_env())
            .on_start(move |app| async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(Background::new(async move {
                    app.shutdown_token().cancelled().await;
                }))
            })
            .build()
            .await
            .unwrap()
            .app;
        assert_eq!(started.load(Ordering::SeqCst), 0, "building does not start");
        assert!(app.has_background());
        let background = app.start_background().await.unwrap().unwrap();
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert!(!app.has_background());
        assert!(app.start_background().await.unwrap().is_none(), "only once");
        app.shutdown();
        background.wait().await;

        let plain = AppBuilder::new(Settings::from_env())
            .build()
            .await
            .unwrap()
            .app;
        assert!(plain.start_background().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn failing_boot_hook_fails_build() {
        let result = AppBuilder::new(Settings::from_env())
            .on_boot(|_| async { Err(Error::internal("no db")) })
            .build()
            .await;
        assert!(result.is_err());
    }
}
