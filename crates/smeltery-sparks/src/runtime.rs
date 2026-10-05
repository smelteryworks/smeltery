//! The registry ([`Sparks`]), mounting, rendering (with nested components) and updating instances.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::task::{Context, Poll};
use std::time::Duration;

use smeltery_core::auth::Auth;
use smeltery_core::html::escape;
use smeltery_core::session::Session;
use smeltery_core::validation::{ValidationErrors, rules::label};
use smeltery_core::view::{RequestHost, SparkRenderer};
use smeltery_core::{App, BoxFuture, Error, Result};
use smeltery_mold::{Host, Value};

use crate::broadcast::Broadcast;
use crate::component::{Guard, Spark, UploadRule};
use crate::snapshot::{Children, Memo, Opened, expired, seal};
use crate::{SparkCtx, upload};

/// The components of an app, registered in `app/sparks/mod.rs`:
///
/// ```
/// # use smeltery::prelude::*;
/// # mod counter {
/// #     use smeltery::prelude::*;
/// #     use serde::{Deserialize, Serialize};
/// #     #[derive(Serialize, Deserialize, Default, Spark)]
/// #     #[spark(name = "counter")]
/// #     pub struct Counter {
/// #         pub count: i64,
/// #         #[spark(model)]
/// #         pub step: i64,
/// #     }
/// #     #[actions]
/// #     impl Counter {}
/// # }
/// pub fn register(s: &mut Sparks) {
///     s.add::<counter::Counter>();
///     // smeltery:sparks
/// }
/// # fn main() {}
/// ```
pub struct Sparks {
    entries: BTreeMap<&'static str, Arc<dyn ErasedSpark>>,
    duplicates: Vec<&'static str>,
    upload_ttl: Duration,
    limits: Limits,
}

/// The limits of the Sparks routes (set on [`Sparks`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// Components in one update request.
    pub(crate) max_components: usize,
    /// Action calls for one component in one update request.
    pub(crate) max_calls: usize,
    /// How long a snapshot is accepted after it was issued; `None` is the session lifetime.
    pub(crate) snapshot_ttl: Option<Duration>,
    /// Upload requests one session may make per `upload_window`.
    pub(crate) upload_files: u32,
    /// Bytes one session may upload per `upload_window`.
    pub(crate) upload_bytes: u64,
    /// The upload quota's window.
    pub(crate) upload_window: Duration,
    /// Upload requests one client address may make per `upload_window`.
    pub(crate) upload_address_files: u32,
    /// Bytes one client address may upload per `upload_window`.
    pub(crate) upload_address_bytes: u64,
    /// Open `GET /_sparks/stream` connections per process.
    pub(crate) max_streams: usize,
    /// `GET /_sparks/stream` requests one client address may make per minute.
    pub(crate) stream_opens: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_components: 1,
            max_calls: 50,
            snapshot_ttl: None,
            upload_files: 30,
            upload_bytes: 100 * 1024 * 1024,
            upload_window: Duration::from_secs(10 * 60),
            upload_address_files: 120,
            upload_address_bytes: 400 * 1024 * 1024,
            max_streams: 1000,
            stream_opens: 60,
        }
    }
}

impl std::fmt::Debug for Sparks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sparks")
            .field("components", &self.entries.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Sparks {
    pub(crate) fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            duplicates: Vec::new(),
            upload_ttl: Duration::from_secs(24 * 60 * 60),
            limits: Limits::default(),
        }
    }

    /// Register component `T` under its name (`T::NAME`). Two components with one name fail the app's build.
    pub fn add<T: Spark>(&mut self) -> &mut Self {
        if self
            .entries
            .insert(T::NAME, Arc::new(Entry::<T>(PhantomData)))
            .is_some()
        {
            self.duplicates.push(T::NAME);
        }
        self
    }

    /// How long an upload token stays valid after the upload (default 24 hours).
    pub fn upload_ttl(&mut self, ttl: Duration) -> &mut Self {
        self.upload_ttl = ttl;
        self
    }

    /// The most components one update request may carry (default 1: the client runtime sends one component per
    /// request). A request with more answers 413 before any snapshot is opened.
    pub fn max_components(&mut self, n: usize) -> &mut Self {
        self.limits.max_components = n.max(1);
        self
    }

    /// The most action calls one component may carry in one update request (default 50: calls queued while a
    /// request is in flight go out together). A request with more answers 413 before anything runs.
    pub fn max_calls(&mut self, n: usize) -> &mut Self {
        self.limits.max_calls = n;
        self
    }

    /// How long a snapshot is accepted after the server issued it (default: the session lifetime,
    /// `SESSION_LIFETIME`). Every response issues a fresh snapshot; an older one answers 419 and the page reloads.
    pub fn snapshot_ttl(&mut self, ttl: Duration) -> &mut Self {
        self.limits.snapshot_ttl = Some(ttl);
        self
    }

    /// The upload quota of one session: at most `files` upload requests and `bytes` bytes per `per` (default 30
    /// files and 100 MiB per 10 minutes). Past it `POST /_sparks/upload` answers 429 without reading the file.
    pub fn upload_quota(&mut self, files: u32, bytes: u64, per: Duration) -> &mut Self {
        self.limits.upload_files = files;
        self.limits.upload_bytes = bytes;
        self.limits.upload_window = per;
        self
    }

    /// The upload quota of one client address (`ClientInfo::ip`, which honours `TRUSTED_PROXIES`), in the window of
    /// [`Sparks::upload_quota`]: at most `files` upload requests and `bytes` bytes (default 120 files and 400 MiB).
    /// A new session is one page load away, so this bounds what one client can store. Past it the upload answers
    /// 429 without reading the file. Requests without a known address count only against the session quota.
    pub fn upload_address_quota(&mut self, files: u32, bytes: u64) -> &mut Self {
        self.limits.upload_address_files = files;
        self.limits.upload_address_bytes = bytes;
        self
    }

    /// The most open `GET /_sparks/stream` connections of this process (default 1000). Past it the stream answers
    /// 503 and the browser's `EventSource` retries later.
    pub fn max_streams(&mut self, n: usize) -> &mut Self {
        self.limits.max_streams = n;
        self
    }

    /// How many `GET /_sparks/stream` requests one client address (`ClientInfo::ip`, which honours
    /// `TRUSTED_PROXIES`; an IPv6 client by its /64) may make per minute (default 60, counted in process memory).
    /// Past it the stream answers 429 before reading the session or asking any channel rule. Visitors behind one NAT
    /// or proxy address share this budget (each page load opens one stream): raise it for such audiences.
    pub fn stream_opens_per_minute(&mut self, n: u32) -> &mut Self {
        self.limits.stream_opens = n;
        self
    }

    /// The registered component names, sorted.
    pub fn names(&self) -> Vec<&'static str> {
        self.entries.keys().copied().collect()
    }

    pub(crate) fn duplicates(&self) -> &[&'static str] {
        &self.duplicates
    }
}

/// The installed Sparks: a service of the app (`Arc<Runtime>`) and its view renderer.
pub(crate) struct Runtime {
    /// Behind a lock so framework crates can add their components at boot ([`crate::extend`]).
    entries: std::sync::RwLock<BTreeMap<&'static str, Arc<dyn ErasedSpark>>>,
    pub(crate) broadcast: Broadcast,
    pub(crate) upload_ttl: Duration,
    pub(crate) limits: Limits,
    /// Unix seconds of the last temp-upload cleanup.
    pub(crate) last_cleanup: AtomicU64,
    /// Each session's upload use in the current window, by session binding.
    pub(crate) uploads: std::sync::Mutex<std::collections::HashMap<String, upload::Use>>,
    /// Open stream connections.
    pub(crate) streams: Arc<std::sync::atomic::AtomicUsize>,
    /// Stream opens per client address this minute.
    pub(crate) stream_opens: crate::broadcast::OpenBudget,
}

impl Runtime {
    pub(crate) fn new(sparks: Sparks, broadcast: Broadcast) -> Self {
        Self {
            entries: std::sync::RwLock::new(sparks.entries),
            broadcast,
            upload_ttl: sparks.upload_ttl,
            limits: sparks.limits,
            last_cleanup: AtomicU64::new(0),
            uploads: std::sync::Mutex::new(std::collections::HashMap::new()),
            streams: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            stream_opens: crate::broadcast::OpenBudget::default(),
        }
    }

    /// How long a snapshot is accepted after it was issued.
    pub(crate) fn snapshot_ttl(&self, app: &App) -> Duration {
        self.limits
            .snapshot_ttl
            .unwrap_or_else(|| app.settings().session_lifetime)
    }

    /// The component registered as `name`.
    pub(crate) fn entry(&self, name: &str) -> Option<Arc<dyn ErasedSpark>> {
        self.entries
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    /// Add `sparks`' components; the names already taken.
    pub(crate) fn extend(&self, sparks: Sparks) -> Vec<&'static str> {
        let mut entries = self
            .entries
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut taken = sparks.duplicates;
        for (name, entry) in sparks.entries {
            if entries.insert(name, entry).is_some() {
                taken.push(name);
            }
        }
        taken
    }

    /// Check every registered component against the app at boot: listeners need `#[spark(stream)]`, an installed
    /// channel authorizer and the state fields their channels name.
    pub(crate) fn check(&self, app: &App) -> Result<()> {
        let entries = self
            .entries
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.values().try_for_each(|entry| entry.boot_check(app))
    }

    pub(crate) fn of(app: &App) -> Result<Arc<Self>> {
        app.service::<Arc<Self>>()
            .map(|r| Arc::clone(&*r))
            .ok_or_else(|| Error::internal("Sparks are not installed"))
    }
}

impl SparkRenderer for Runtime {
    fn render(
        &self,
        host: &RequestHost,
        name: &str,
        props: &Value,
    ) -> std::result::Result<String, String> {
        let entry = self.entry(name).ok_or_else(|| {
            format!("unknown Spark `{name}`: register it with `s.add::<…>()` in app/sparks/mod.rs")
        })?;
        let env = Env {
            app: host.app().clone(),
            runtime: self,
            session: host.data().session_handle().cloned(),
            auth: host.data().auth().cloned(),
        };
        let props = props_map(props).map_err(|e| e.to_string())?;
        let id = new_id().map_err(|e| e.to_string())?;
        entry
            .mount(&env, host, props, id)
            .map_err(|e| format!("Spark `{name}`: {e}"))
    }

    fn scripts(&self, host: &RequestHost) -> String {
        crate::assets::scripts_tag(host.csrf_token())
    }
}

/// What mounting and rendering need from the request.
pub(crate) struct Env<'a> {
    pub(crate) app: App,
    pub(crate) runtime: &'a Runtime,
    pub(crate) session: Option<Session>,
    pub(crate) auth: Option<Auth>,
}

/// One instance's part of an update request, with the request's session.
pub(crate) struct UpdateRequest {
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) app: App,
    pub(crate) session: Session,
    pub(crate) auth: Auth,
    pub(crate) opened: Opened,
    pub(crate) updates: serde_json::Map<String, serde_json::Value>,
    pub(crate) calls: Vec<Call>,
}

/// One call of an action, as the client sent it.
pub(crate) struct Call {
    pub(crate) method: String,
    pub(crate) params: Vec<serde_json::Value>,
}

/// A component type with its type erased.
pub(crate) trait ErasedSpark: Send + Sync {
    fn uploads(&self) -> &'static [UploadRule];

    /// The boot check of the component's declarations ([`Runtime::check`]).
    fn boot_check(&self, app: &App) -> Result<()>;

    /// Check an update's fields and calls against the allow-lists and guards, before anything runs.
    fn check(
        &self,
        updates: &serde_json::Map<String, serde_json::Value>,
        calls: &[Call],
        auth: &Auth,
    ) -> Result<()>;

    /// Mount and render a new instance. Runs on a blocking thread (the `mount` hook is driven there).
    fn mount(
        &self,
        env: &Env<'_>,
        base: &dyn Host,
        props: serde_json::Map<String, serde_json::Value>,
        id: String,
    ) -> Result<String>;

    /// Apply updates and calls to an instance and render it: the response entry for it.
    fn update(&self, request: UpdateRequest) -> BoxFuture<'static, Result<serde_json::Value>>;
}

struct Entry<T>(PhantomData<fn() -> T>);

impl<T: Spark> ErasedSpark for Entry<T> {
    fn uploads(&self) -> &'static [UploadRule] {
        T::UPLOADS
    }

    fn boot_check(&self, app: &App) -> Result<()> {
        let listeners = <T as crate::Actions>::LISTENERS;
        if listeners.is_empty() {
            return Ok(());
        }
        if !T::STREAM {
            return Err(Error::internal(format!(
                "Spark `{}` declares `#[on]` listeners: add `stream` to its `#[spark(...)]`",
                T::NAME
            )));
        }
        if app.channel_authorizer().is_none() {
            return Err(crate::listen::no_authorizer(T::NAME));
        }
        if app.cache().store_name() == "null" {
            // The `null` store keeps nothing: every listen message would be refused as "already ran".
            return Err(Error::internal(format!(
                "Spark `{}` declares `#[on]` listeners, which need a cache store: CACHE_STORE=null keeps nothing",
                T::NAME
            )));
        }
        let state = state_value(&T::default())?;
        for listener in listeners {
            if let Some(missing) = crate::listen::fields(listener.channel())
                .into_iter()
                .find(|f| state.get(*f).is_none())
            {
                return Err(Error::internal(format!(
                    "Spark `{}`: the listener `{}` names `{{{missing}}}`, which is not a field of the state",
                    T::NAME,
                    listener.method()
                )));
            }
        }
        Ok(())
    }

    fn check(
        &self,
        updates: &serde_json::Map<String, serde_json::Value>,
        calls: &[Call],
        auth: &Auth,
    ) -> Result<()> {
        check_request::<T>(updates, calls, auth)
    }

    fn mount(
        &self,
        env: &Env<'_>,
        base: &dyn Host,
        props: serde_json::Map<String, serde_json::Value>,
        id: String,
    ) -> Result<String> {
        let mut state = T::default();
        apply_props(&mut state, &props)?;
        let mut ctx = SparkCtx::new(
            env.app.clone(),
            env.session.clone(),
            env.auth.clone(),
            id.clone(),
            T::NAME,
            props,
        );
        let mut errors = ValidationErrors::new();
        match drive(state.mount_hook(&mut ctx))? {
            Ok(()) => {}
            Err(Error::Validation(failed)) => errors = failed.errors,
            Err(e) => return Err(e),
        }
        match drive(state.rendering_hook(&mut ctx))? {
            Ok(()) => {}
            Err(Error::Validation(failed)) => merge(&mut errors, &failed.errors),
            Err(e) => return Err(e),
        }
        let stream = if T::STREAM && drive(state.stream_hook(&mut ctx))?? {
            let value = state_value(&state)?;
            let listens = drive(crate::listen::grants::<T>(
                &env.app,
                &value,
                env.auth.as_ref(),
            ))??;
            Some(crate::broadcast::stream_token(
                &env.app,
                T::NAME,
                &id,
                env.runtime.snapshot_ttl(&env.app),
                crate::broadcast::Viewer {
                    session: env.session.as_ref(),
                    auth: env.auth.as_ref(),
                },
                listens,
            )?)
        } else {
            None
        };
        let (html, children) = render(&state, env, base, &errors, &BTreeMap::new())?;
        let memo = Memo::new(
            id,
            T::NAME,
            children,
            env.session.as_ref(),
            env.auth.as_ref(),
        );
        let snapshot = seal(&env.app, state_value(&state)?, &memo)?;
        Ok(wrap(&memo.id, T::NAME, &snapshot, &html, stream.as_deref()))
    }

    fn update(&self, request: UpdateRequest) -> BoxFuture<'static, Result<serde_json::Value>> {
        Box::pin(update::<T>(request))
    }
}

async fn update<T: Spark>(request: UpdateRequest) -> Result<serde_json::Value> {
    let UpdateRequest {
        runtime,
        app,
        session,
        auth,
        opened,
        updates,
        calls,
    } = request;
    let name = T::NAME;
    // Everything the client asks for is checked before anything runs (the request handler checked every
    // component already; checking here too keeps this function safe on its own).
    check_request::<T>(&updates, &calls, &auth)?;
    let mut state: T = serde_json::from_value(opened.data).map_err(|_| {
        tracing::warn!(
            component = name,
            "Sparks snapshot no longer fits the component"
        );
        expired()
    })?;
    let id = opened.memo.id;
    let mut ctx = SparkCtx::new(
        app.clone(),
        Some(session.clone()),
        Some(auth.clone()),
        id.clone(),
        name,
        serde_json::Map::new(),
    );
    // Listen messages are checked before anything of this component runs (signature, expiry, order, declared
    // listener, the viewer's access to the channel); whether the state names the channel is decided just before each
    // runs, after the request's updates and earlier calls.
    let plans = plan_listens::<T>(&app, &auth, (&id, opened.memo.l), &calls).await?;
    // The last listen message this instance ran or skipped by design: what the new snapshot's `memo.l` says.
    let mut last_listen = opened.memo.l;
    // A model write that would set a struct is refused before anything of this component runs: tried on a copy.
    let mut trial: T = serde_json::from_value(state_value(&state)?).map_err(|_| expired())?;
    for (field, value) in &updates {
        if T::UPLOADS.iter().any(|u| u.field() == field) {
            continue;
        }
        for (path, value) in model_writes(field, value.clone()) {
            if apply_update(&mut trial, &path, value, false) == Err(Rejected::Object) {
                return Err(not_model(name, &path, "a struct set through a model value"));
            }
        }
    }
    drop(trial);
    let mut errors = ValidationErrors::new();
    'run: {
        for (field, value) in updates {
            let is_upload = T::UPLOADS.iter().any(|u| u.field() == field);
            let writes = match T::UPLOADS.iter().find(|u| u.field() == field) {
                // The server's own `TemporaryUpload` from a verified token: written whole.
                Some(rule) => match upload::resolve(&app, &session, name, rule, value)? {
                    upload::Resolved::Value(v) => vec![(field.clone(), v)],
                    upload::Resolved::Error(message) => {
                        errors.add(field, message);
                        continue;
                    }
                },
                None => model_writes(&field, value),
            };
            for (path, value) in writes {
                match apply_update(&mut state, &path, value, is_upload) {
                    Ok(()) => {}
                    Err(Rejected::Object) => {
                        return Err(not_model(name, &path, "a struct set through a model value"));
                    }
                    Err(Rejected::Invalid) => errors.add(
                        path.clone(),
                        format!("The {} field is invalid.", label(&path)),
                    ),
                }
            }
            match state.updated_hook(&mut ctx, &field).await {
                Ok(()) => {}
                Err(Error::Validation(failed)) => {
                    merge(&mut errors, &failed.errors);
                    break 'run;
                }
                Err(e) => return Err(e),
            }
        }
        for (call, plan) in calls.into_iter().zip(plans) {
            let ran = match plan {
                Plan::Refresh => continue,
                Plan::Call => state.call(&call.method, call.params, &mut ctx).await,
                Plan::Listen(listen) => {
                    // The channel the state names NOW (after this request's updates and earlier calls).
                    let current = state_value(&state)?;
                    let methods: Vec<&'static str> = listen
                        .candidates
                        .iter()
                        .filter(|(_, template)| {
                            crate::listen::resolve(template, &current).as_deref()
                                == Some(listen.channel.as_str())
                        })
                        .map(|(method, _)| *method)
                        .collect();
                    if methods.is_empty() {
                        tracing::debug!(
                            component = name,
                            "Sparks listen skipped: the state names another channel now"
                        );
                        last_listen = listen.seq;
                        continue;
                    }
                    // Once per message, whatever snapshot carries it (shared cache store: across processes too).
                    crate::listen::claim(&app, &id, listen.seq, name).await?;
                    last_listen = listen.seq;
                    let mut ran = Ok(());
                    for method in methods {
                        ran = state.listen(method, listen.payload.clone(), &mut ctx).await;
                        if ran.is_err() {
                            break;
                        }
                    }
                    ran
                }
            };
            match ran {
                Ok(()) => {}
                Err(Error::Validation(failed)) => {
                    merge(&mut errors, &failed.errors);
                    break 'run;
                }
                Err(e) => return Err(e),
            }
        }
    }
    match state.rendering_hook(&mut ctx).await {
        Ok(()) => {}
        Err(Error::Validation(failed)) => merge(&mut errors, &failed.errors),
        Err(e) => return Err(e),
    }
    let stream = if T::STREAM && state.stream_hook(&mut ctx).await? {
        let listens = crate::listen::grants::<T>(&app, &state_value(&state)?, Some(&auth)).await?;
        Some(crate::broadcast::stream_token(
            &app,
            name,
            &id,
            runtime.snapshot_ttl(&app),
            crate::broadcast::Viewer {
                session: Some(&session),
                auth: Some(&auth),
            },
            listens,
        )?)
    } else {
        None
    };
    if let Some(target) = ctx.effects.refused.take() {
        return Err(Error::internal(format!(
            "Spark `{name}`: refused to redirect to `{target}`: `ctx.redirect` takes a path on this site (`/posts`) \
             or an APP_URL address, `ctx.redirect_away` an http(s) URL"
        )));
    }
    let effects = std::mem::take(&mut ctx.effects);
    drop(ctx);
    let data = state_value(&state)?;
    let old_children = opened.memo.children;
    let render_app = app.clone();
    let memo_session = session.clone();
    let memo_auth = auth.clone();
    // Rendering reads template files in debug builds and may mount children: keep it off the async workers.
    let rendered = tokio::task::spawn_blocking(move || {
        let env = Env {
            app: render_app,
            runtime: &runtime,
            session: Some(session.clone()),
            auth: Some(auth.clone()),
        };
        let base = UpdateHost {
            app: &env.app,
            csrf: session.csrf_token()?,
            authenticated: auth.check(),
            session: &session,
        };
        render(&state, &env, &base, &errors, &old_children)
    })
    .await
    .map_err(|e| Error::internal(format!("rendering a Spark panicked: {e}")))??;
    let (html, children) = rendered;
    let mut memo = Memo::new(id, name, children, Some(&memo_session), Some(&memo_auth));
    memo.l = last_listen;
    let snapshot = seal(&app, data, &memo)?;
    let html = wrap(&memo.id, name, &snapshot, &html, stream.as_deref());
    Ok(serde_json::json!({
        "id": memo.id,
        "snapshot": snapshot,
        "html": html,
        "effects": effects,
    }))
}

/// What one call of an update request does.
enum Plan {
    /// `$refresh`: nothing (the component re-renders).
    Refresh,
    /// An action.
    Call,
    /// `$listen`: a checked listen message.
    Listen(PlannedListen),
}

/// A checked listen message: the listeners that take its event on a channel of its shape, run when the state names
/// its channel at that moment.
struct PlannedListen {
    /// `(method, channel template)` of the declared listeners.
    candidates: Vec<(&'static str, &'static str)>,
    channel: String,
    payload: serde_json::Value,
    seq: u64,
}

/// Check the `$listen` calls of an update against the instance (before anything runs): each must be a listen
/// message the server signed for instance `id`, unexpired, newer than `last` (`memo.l`) and than the request's
/// earlier ones, for a declared listener taking its event on a channel of that shape, and the viewer must still be
/// allowed to receive that channel. The plan of every call.
async fn plan_listens<T: Spark>(
    app: &App,
    auth: &Auth,
    (id, mut last): (&str, u64),
    calls: &[Call],
) -> Result<Vec<Plan>> {
    use crate::listen::{ListenCall, fits, refused};
    let name = T::NAME;
    let mut plans = Vec::with_capacity(calls.len());
    for call in calls {
        if call.method == "$refresh" {
            plans.push(Plan::Refresh);
            continue;
        }
        if call.method != "$listen" {
            plans.push(Plan::Call);
            continue;
        }
        let message = ListenCall::parse(&call.params)?;
        if !message.verify(app, id) {
            return Err(refused(name, "not signed for this instance, or expired"));
        }
        if message.seq <= last {
            return Err(refused(name, "already ran"));
        }
        last = message.seq;
        let mut candidates: Vec<(&'static str, &'static str)> = Vec::new();
        for listener in <T as crate::Actions>::LISTENERS {
            if listener.event() == message.event
                && fits(listener.channel(), &message.channel)
                && !candidates
                    .iter()
                    .any(|(m, t)| *m == listener.method() && *t == listener.channel())
            {
                candidates.push((listener.method(), listener.channel()));
            }
        }
        if candidates.is_empty() {
            return Err(refused(name, "not a declared listener"));
        }
        let authorizer = app
            .channel_authorizer()
            .ok_or_else(|| crate::listen::no_authorizer(name))?;
        match authorizer
            .authorize(app, &message.channel, Some(auth))
            .await
        {
            Ok(true) => {}
            Ok(false) => return Err(refused(name, "the viewer may not receive the channel")),
            Err(e) if !e.status().is_server_error() => {
                return Err(refused(name, "the viewer may not receive the channel"));
            }
            Err(e) => return Err(e),
        }
        let payload: serde_json::Value = serde_json::from_str(&message.data)
            .map_err(|_| Error::bad_request("the event data is not JSON"))?;
        plans.push(Plan::Listen(PlannedListen {
            candidates,
            channel: message.channel,
            payload,
            seq: message.seq,
        }));
    }
    Ok(plans)
}

/// 403 for an update the component does not accept.
fn not_model(name: &str, field: &str, why: &str) -> Error {
    tracing::warn!(component = name, field = %field, reason = why, "Sparks update rejected: not a model field");
    Error::http(
        http::StatusCode::FORBIDDEN,
        format!("`{field}` is not a model field of `{name}`"),
    )
}

/// Whether `value` holds a JSON object at any depth.
fn holds_object(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(_) => true,
        serde_json::Value::Array(items) => items.iter().any(holds_object),
        _ => false,
    }
}

/// Check everything an update asks for, before anything runs: the fields it sets (model fields, the listed keys of
/// a `model(fields = …)` field, upload fields), the actions it calls and their guards.
///
/// A model value never carries a JSON object, so a struct (or map) field cannot be filled with keys the view
/// never binds (mass assignment); a struct field is written key by key, only the keys its
/// `#[spark(model(fields = "…"))]` lists (`form.title`, or `form` with an object of listed keys).
pub(crate) fn check_request<T: Spark>(
    updates: &serde_json::Map<String, serde_json::Value>,
    calls: &[Call],
    auth: &Auth,
) -> Result<()> {
    let name = T::NAME;
    for (field, value) in updates {
        if T::UPLOADS.iter().any(|u| u.field() == field) {
            continue;
        }
        if T::MODEL.contains(&field.as_str()) {
            if holds_object(value) {
                return Err(not_model(name, field, "an object for a model field"));
            }
            continue;
        }
        // `form` with an object: each key must be listed as `form.<key>`.
        let listed = |key: &str| T::MODEL.contains(&format!("{field}.{key}").as_str());
        match value {
            serde_json::Value::Object(map)
                if !field.contains('.')
                    && T::MODEL.iter().any(|m| m.starts_with(&format!("{field}."))) =>
            {
                for (key, v) in map {
                    if !listed(key) || holds_object(v) {
                        return Err(not_model(
                            name,
                            &format!("{field}.{key}"),
                            "an unlisted key",
                        ));
                    }
                }
            }
            _ => return Err(not_model(name, field, "not listed")),
        }
    }
    for call in calls {
        if call.method == "$refresh" {
            continue;
        }
        if call.method == "$listen" {
            if <T as crate::Actions>::LISTENERS.is_empty() {
                tracing::warn!(component = name, "Sparks call rejected: no listeners");
                return Err(crate::__private::unknown_action(&call.method));
            }
            continue;
        }
        let Some(action) = <T as crate::Actions>::ACTIONS
            .iter()
            .find(|a| a.name() == call.method)
        else {
            tracing::warn!(component = name, method = %call.method, "Sparks call rejected: not an action");
            return Err(crate::__private::unknown_action(&call.method));
        };
        for guard in action.guards() {
            match guard {
                Guard::Auth if !auth.check() => {
                    tracing::warn!(component = name, method = %call.method, "Sparks call rejected: guard auth");
                    return Err(Error::unauthorized());
                }
                Guard::Guest if auth.check() => {
                    tracing::warn!(component = name, method = %call.method, "Sparks call rejected: guard guest");
                    return Err(Error::forbidden());
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn merge(into: &mut ValidationErrors, from: &ValidationErrors) {
    for (field, messages) in from.iter() {
        for m in messages {
            into.add(field, m.clone());
        }
    }
}

/// The state as a JSON object.
fn state_value<T: Spark>(state: &T) -> Result<serde_json::Value> {
    let value = serde_json::to_value(state)?;
    if !value.is_object() {
        return Err(Error::internal(format!(
            "Spark `{}` must serialize to a JSON object",
            T::NAME
        )));
    }
    Ok(value)
}

/// Write the props named like fields into the state.
fn apply_props<T: Spark>(
    state: &mut T,
    props: &serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    let serde_json::Value::Object(mut fields) = state_value(state)? else {
        return Ok(());
    };
    let mut touched = false;
    for (k, v) in props {
        if let Some(slot) = fields.get_mut(k) {
            *slot = v.clone();
            touched = true;
        }
    }
    if touched {
        *state = serde_json::from_value(serde_json::Value::Object(fields)).map_err(|e| {
            Error::internal(format!(
                "the props do not fit the fields of `{}`: {e}",
                T::NAME
            ))
        })?;
    }
    Ok(())
}

/// The writes one checked update makes: `field` itself, or for `form` with an object of listed keys, one write
/// per key (`form.title`, `form.body`).
fn model_writes(field: &str, value: serde_json::Value) -> Vec<(String, serde_json::Value)> {
    match value {
        serde_json::Value::Object(map) => map
            .into_iter()
            .map(|(key, v)| (format!("{field}.{key}"), v))
            .collect(),
        other => vec![(field.to_owned(), other)],
    }
}

/// Why a model write was not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rejected {
    /// The value fits no form of the field: a validation message.
    Invalid,
    /// The write would set a struct (an object) through a model value: refused (403).
    Object,
}

/// The value at `field` / `field.key` of a state object.
fn value_at<'a>(
    fields: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    key: Option<&str>,
) -> Option<&'a serde_json::Value> {
    let v = fields.get(field)?;
    match key {
        None => Some(v),
        Some(key) => v.as_object()?.get(key),
    }
}

/// Set `path` (`field`, or `field.key` inside an object field) to `value`, trying the value's coerced forms
/// (text from inputs: `"2"`, `"on"`, `""`).
///
/// A model value never sets a struct: when the target holds an object now, or would hold one after the write
/// (serde fills a struct from a JSON array too: `["hi", 999]`), the write is [`Rejected::Object`]. Only `null` may
/// replace an object (clearing an `Option`).
pub(crate) fn apply_update<T: Spark>(
    state: &mut T,
    path: &str,
    value: serde_json::Value,
    allow_object: bool,
) -> std::result::Result<(), Rejected> {
    let Ok(serde_json::Value::Object(mut fields)) = serde_json::to_value(&*state) else {
        return Err(Rejected::Invalid);
    };
    let (field, key) = match path.split_once('.') {
        Some((field, key)) => (field, Some(key)),
        None => (path, None),
    };
    match (fields.get(field), key) {
        (None, _) => return Err(Rejected::Invalid),
        // A key of an object field: the field must hold an object with that key now (a `None` form has no keys
        // to set).
        (Some(serde_json::Value::Object(inner)), Some(key)) if inner.contains_key(key) => {}
        (Some(_), None) => {}
        (Some(_), Some(_)) => return Err(Rejected::Invalid),
    }
    if !allow_object && !value.is_null() && value_at(&fields, field, key).is_some_and(holds_object)
    {
        return Err(Rejected::Object);
    }
    for candidate in candidates(value) {
        match key {
            None => {
                fields.insert(field.to_owned(), candidate);
            }
            Some(key) => {
                if let Some(serde_json::Value::Object(inner)) = fields.get_mut(field) {
                    inner.insert(key.to_owned(), candidate);
                }
            }
        }
        if let Ok(next) = serde_json::from_value::<T>(serde_json::Value::Object(fields.clone())) {
            // What the struct made of the value: a struct (an object) is refused, whatever the value's shape.
            let after = serde_json::to_value(&next).map_err(|_| Rejected::Invalid)?;
            let set = after
                .as_object()
                .and_then(|after| value_at(after, field, key))
                .is_some_and(holds_object);
            if set && !allow_object {
                return Err(Rejected::Object);
            }
            *state = next;
            return Ok(());
        }
    }
    Err(Rejected::Invalid)
}

/// The forms a client value may take: as sent, then what its text spells.
fn candidates(value: serde_json::Value) -> Vec<serde_json::Value> {
    let mut out = Vec::with_capacity(3);
    if let serde_json::Value::String(text) = &value {
        let t = text.trim();
        if let Ok(i) = t.parse::<i64>() {
            out.push(serde_json::Value::from(i));
        } else if let Ok(u) = t.parse::<u64>() {
            out.push(serde_json::Value::from(u));
        } else if let Some(f) = t.parse::<f64>().ok().filter(|f| f.is_finite()) {
            out.push(serde_json::Value::from(f));
        }
        match t {
            "true" | "on" | "1" => out.push(serde_json::Value::Bool(true)),
            "false" | "off" | "0" | "" => out.push(serde_json::Value::Bool(false)),
            _ => {}
        }
        if t.is_empty() {
            out.push(serde_json::Value::Null);
        }
    }
    out.insert(0, value);
    out
}

/// Run a hook's future: at once when it does not wait, else on the current Tokio runtime (renders run on
/// blocking threads, where that is allowed).
fn drive<F: Future>(future: F) -> Result<F::Output> {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
        return Ok(output);
    }
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| Error::internal("a Spark's mount hook needs the Tokio runtime"))?;
    Ok(handle.block_on(future))
}

/// Render `state`'s view with its own errors; returns the HTML and the children it mounted or kept.
fn render<T: Spark>(
    state: &T,
    env: &Env<'_>,
    base: &dyn Host,
    errors: &ValidationErrors,
    old_children: &Children,
) -> Result<(String, Children)> {
    let host = ComponentHost {
        base,
        env,
        errors,
        old_children,
        children: RefCell::new(BTreeMap::new()),
        counter: Cell::new(0),
    };
    let html = state
        .render_view(env.app.views(), &host)
        .map_err(|e| Error::internal(format!("template error: {e}")))?;
    Ok((html, host.children.into_inner()))
}

/// The root element around a component's view.
pub(crate) fn wrap(
    id: &str,
    name: &str,
    snapshot: &str,
    html: &str,
    stream: Option<&str>,
) -> String {
    format!(
        "<div wire:id=\"{}\" wire:name=\"{}\" wire:snapshot=\"{}\"{}>{html}</div>",
        escape(id),
        escape(name),
        escape(snapshot),
        stream
            .map(|t| format!(" wire:stream=\"{}\"", escape(t)))
            .unwrap_or_default()
    )
}

/// The host a component's view renders with: its own errors, nested `@spark`, the rest from the page.
struct ComponentHost<'a> {
    base: &'a dyn Host,
    env: &'a Env<'a>,
    errors: &'a ValidationErrors,
    old_children: &'a Children,
    children: RefCell<Children>,
    counter: Cell<usize>,
}

impl Host for ComponentHost<'_> {
    fn csrf_token(&self) -> Option<&str> {
        self.base.csrf_token()
    }
    fn authenticated(&self) -> bool {
        self.base.authenticated()
    }
    fn errors(&self, field: &str) -> &[String] {
        self.errors.get(field)
    }
    fn session(&self, key: &str) -> Option<String> {
        self.base.session(key)
    }
    fn route(
        &self,
        name: &str,
        params: &[(String, String)],
    ) -> std::result::Result<String, String> {
        self.base.route(name, params)
    }
    fn spark(&self, name: &str, props: &Value) -> std::result::Result<String, String> {
        let props = props_map(props).map_err(|e| e.to_string())?;
        let key = match props.get("key") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => {
                let n = self.counter.get();
                self.counter.set(n + 1);
                format!("{name}-{n}")
            }
        };
        if let Some((id, old_name)) = self.old_children.get(&key)
            && old_name == name
        {
            self.children
                .borrow_mut()
                .insert(key, (id.clone(), name.to_owned()));
            return Ok(format!("<div wire:id=\"{}\"></div>", escape(id)));
        }
        let entry = self.env.runtime.entry(name).ok_or_else(|| {
            format!("unknown Spark `{name}`: register it with `s.add::<…>()` in app/sparks/mod.rs")
        })?;
        let id = new_id().map_err(|e| e.to_string())?;
        let html = entry
            .mount(self.env, self.base, props, id.clone())
            .map_err(|e| format!("Spark `{name}`: {e}"))?;
        self.children
            .borrow_mut()
            .insert(key, (id, name.to_owned()));
        Ok(html)
    }
    fn sparks_scripts(&self) -> String {
        self.base.sparks_scripts()
    }
}

/// The page-level host of an update request: the live session and sign-in.
struct UpdateHost<'a> {
    app: &'a App,
    csrf: String,
    authenticated: bool,
    session: &'a Session,
}

impl Host for UpdateHost<'_> {
    fn csrf_token(&self) -> Option<&str> {
        Some(&self.csrf)
    }
    fn authenticated(&self) -> bool {
        self.authenticated
    }
    fn session(&self, key: &str) -> Option<String> {
        if key.starts_with('_') {
            return None;
        }
        match self.session.get::<serde_json::Value>(key)? {
            serde_json::Value::String(s) => Some(s),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }
    fn route(
        &self,
        name: &str,
        params: &[(String, String)],
    ) -> std::result::Result<String, String> {
        let params: Vec<(&str, &str)> = params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        self.app.url(name, &params).map_err(|e| e.to_string())
    }
    fn sparks_scripts(&self) -> String {
        crate::assets::scripts_tag(Some(&self.csrf))
    }
}

/// `@spark` props as a JSON object.
fn props_map(props: &Value) -> Result<serde_json::Map<String, serde_json::Value>> {
    match serde_json::to_value(props)? {
        serde_json::Value::Object(map) => Ok(map),
        serde_json::Value::Null => Ok(serde_json::Map::new()),
        _ => Err(Error::internal(
            "`@spark` props must be a map: @spark(\"name\", { key: value })",
        )),
    }
}

/// A new instance id: 20 characters `[A-Za-z0-9]` from the OS random source.
pub(crate) fn new_id() -> Result<String> {
    crate::upload::random_token(20)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_cover_input_text() {
        assert_eq!(
            candidates(serde_json::json!("2")),
            vec![serde_json::json!("2"), serde_json::json!(2)]
        );
        assert_eq!(
            candidates(serde_json::json!("")),
            vec![
                serde_json::json!(""),
                serde_json::json!(false),
                serde_json::Value::Null
            ]
        );
        assert_eq!(
            candidates(serde_json::json!("on")),
            vec![serde_json::json!("on"), serde_json::json!(true)]
        );
        assert_eq!(candidates(serde_json::json!(3)), vec![serde_json::json!(3)]);
        assert_eq!(
            wrap("a", "c", "{\"x\":1}", "<p>", Some("t.s")),
            "<div wire:id=\"a\" wire:name=\"c\" wire:snapshot=\"{&quot;x&quot;:1}\" wire:stream=\"t.s\"><p></div>"
        );
    }

    #[test]
    fn drive_runs_ready_futures_without_a_runtime() {
        assert_eq!(drive(async { 7 }).unwrap(), 7);
        let err = drive(async {
            tokio::task::yield_now().await;
            1
        })
        .unwrap_err();
        assert!(err.to_string().contains("Tokio runtime"), "{err}");
    }

    #[test]
    fn invalid_carries_messages() {
        let mut errors = ValidationErrors::new();
        errors.add("a", "bad");
        assert!(matches!(crate::ctx::invalid(errors), Error::Validation(_)));
    }
}
