//! The component traits: [`Spark`] (state + view, from `#[derive(Spark)]`) and [`Actions`] (from `#[actions]`).

use serde::Serialize;
use serde::de::DeserializeOwned;
use smeltery_core::{BoxFuture, Result};
use smeltery_mold::{Engine, Host};

use crate::SparkCtx;

/// A live component: its state (the struct's fields) and its Mold view. `#[derive(Spark)]` implements it.
///
/// The state travels to the browser in a signed snapshot and comes back with every request, so it must
/// serialize to a JSON object; the visitor can read it (it is signed, not encrypted), so secrets never go into
/// component fields.
pub trait Spark: Serialize + DeserializeOwned + Default + Actions + Send + Sync + 'static {
    /// The component's name, as `@spark("name")` uses it.
    const NAME: &'static str;
    /// What `wire:model` may set: the names of `#[spark(model)]` fields, and `field.key` for each key listed by
    /// `#[spark(model(fields = "…"))]`.
    const MODEL: &'static [&'static str];
    /// The upload fields (`#[spark(upload(...))]`).
    const UPLOADS: &'static [UploadRule];
    /// Whether the component receives `Broadcast` pushes (`#[spark(stream)]`).
    const STREAM: bool;

    /// Render the view: the runtime engine `engine` (hot reload) in debug builds, the compiled template in
    /// release builds.
    ///
    /// # Errors
    /// A template error.
    fn render_view(
        &self,
        engine: &Engine,
        host: &dyn Host,
    ) -> std::result::Result<String, smeltery_mold::Error>;
}

/// The callable actions and the hooks of a component. `#[actions]` on the component's impl block implements it.
#[diagnostic::on_unimplemented(
    message = "`{Self}` has no `#[actions]` impl block",
    note = "add `#[smeltery::actions] impl {Self} {{}}` (it may be empty)"
)]
pub trait Actions: Send {
    /// The actions the page may call.
    const ACTIONS: &'static [ActionInfo];

    /// Run action `method` with `params`.
    fn call<'a>(
        &'a mut self,
        method: &'a str,
        params: Vec<serde_json::Value>,
        ctx: &'a mut SparkCtx,
    ) -> BoxFuture<'a, Result<()>>;

    /// The `mount` hook: runs once, when the component is first rendered.
    fn mount_hook<'a>(&'a mut self, ctx: &'a mut SparkCtx) -> BoxFuture<'a, Result<()>> {
        let _ = ctx;
        Box::pin(std::future::ready(Ok(())))
    }

    /// The `rendering` hook: runs before every render (the first one, after `mount`, and each one after an
    /// update request's updates and calls, `$refresh` included), e.g. to load fresh data into the state.
    fn rendering_hook<'a>(&'a mut self, ctx: &'a mut SparkCtx) -> BoxFuture<'a, Result<()>> {
        let _ = ctx;
        Box::pin(std::future::ready(Ok(())))
    }

    /// The `can_stream` hook of a `#[spark(stream)]` component: runs before every render (after `rendering`) and
    /// decides whether this visitor's page may subscribe to the component's pushes on `GET /_sparks/stream`. Only a
    /// render where it returns `true` carries a stream token (the default: every visitor the component renders for).
    fn stream_hook<'a>(&'a mut self, ctx: &'a mut SparkCtx) -> BoxFuture<'a, Result<bool>> {
        let _ = ctx;
        Box::pin(std::future::ready(Ok(true)))
    }

    /// The `updated` hook: runs after `wire:model` set `field`.
    fn updated_hook<'a>(
        &'a mut self,
        ctx: &'a mut SparkCtx,
        field: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        let _ = (ctx, field);
        Box::pin(std::future::ready(Ok(())))
    }

    /// The listeners: methods marked `#[on("anvil:<channel>", "<event>")]`, which run when that event is broadcast
    /// on that channel. They are not actions: the page cannot call them by name.
    const LISTENERS: &'static [ListenerInfo] = &[];

    /// Run listener `method` with the event's data `payload`.
    fn listen<'a>(
        &'a mut self,
        method: &'a str,
        payload: serde_json::Value,
        ctx: &'a mut SparkCtx,
    ) -> BoxFuture<'a, Result<()>> {
        let _ = (payload, ctx);
        Box::pin(std::future::ready(Err(crate::__private::unknown_action(
            method,
        ))))
    }
}

/// A listener: a method that runs when `event` is broadcast on the channel `channel` names
/// (`#[on("anvil:private-orders.{order_id}", "OrderShipped")]`).
#[derive(Clone, Copy, Debug)]
pub struct ListenerInfo {
    method: &'static str,
    channel: &'static str,
    event: &'static str,
}

impl ListenerInfo {
    /// Method `method` listens for `event` on the channels `channel` names (without the `anvil:` prefix;
    /// `{field}` stands for the value of that state field).
    pub const fn new(method: &'static str, channel: &'static str, event: &'static str) -> Self {
        Self {
            method,
            channel,
            event,
        }
    }

    /// The method name.
    pub fn method(&self) -> &'static str {
        self.method
    }

    /// The channel template (`private-orders.{order_id}`).
    pub fn channel(&self) -> &'static str {
        self.channel
    }

    /// The event name.
    pub fn event(&self) -> &'static str {
        self.event
    }
}

/// A requirement an action checks before it runs (`#[guard(auth)]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Guard {
    /// A signed-in user (otherwise 401).
    Auth,
    /// Nobody signed in (otherwise 403).
    Guest,
}

/// An action's name and guards.
#[derive(Clone, Copy, Debug)]
pub struct ActionInfo {
    name: &'static str,
    guards: &'static [Guard],
}

impl ActionInfo {
    /// An action named `name` with `guards`.
    pub const fn new(name: &'static str, guards: &'static [Guard]) -> Self {
        Self { name, guards }
    }

    /// The method name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The guards checked before it runs.
    pub fn guards(&self) -> &'static [Guard] {
        self.guards
    }
}

/// The rules of an upload field (`#[spark(upload(max = 2048, mimes = "png,jpg"))]`).
#[derive(Clone, Copy, Debug)]
pub struct UploadRule {
    field: &'static str,
    max_kb: u64,
    mimes: &'static [&'static str],
}

impl UploadRule {
    /// Field `field` takes files up to `max_kb` kilobytes with one of the extensions `mimes` (any when empty).
    pub const fn new(field: &'static str, max_kb: u64, mimes: &'static [&'static str]) -> Self {
        Self {
            field,
            max_kb,
            mimes,
        }
    }

    /// The field name.
    pub fn field(&self) -> &'static str {
        self.field
    }

    /// The largest file, in kilobytes (1 KB = 1024 bytes).
    pub fn max_kb(&self) -> u64 {
        self.max_kb
    }

    /// The allowed file extensions, lowercase; empty allows any.
    pub fn mimes(&self) -> &'static [&'static str] {
        self.mimes
    }

    /// Whether a file named `name` has an allowed extension.
    pub fn allows(&self, name: &str) -> bool {
        if self.mimes.is_empty() {
            return true;
        }
        let ext = crate::upload::extension(name);
        ext.is_some_and(|e| self.mimes.contains(&e.as_str()))
    }
}
