//! [`SparkCtx`]: what actions and hooks reach besides the component's own state.

use serde::Serialize;
use serde::de::DeserializeOwned;
use smeltery_core::auth::Auth;
use smeltery_core::db::Db;
use smeltery_core::http::is_local_path;
use smeltery_core::session::Session;
use smeltery_core::validation::{Input, Invalid, Validate, ValidationContext, ValidationErrors};
use smeltery_core::{App, Error, Result};

/// The context of an action or hook: the app, the visitor's session and sign-in, the mount props, and the
/// effects to send back to the browser (redirect, browser events).
///
/// ```
/// # use smeltery::prelude::*;
/// # use smeltery::json;
/// # use serde::{Deserialize, Serialize};
/// # #[derive(Serialize, Deserialize, Default, Spark, Validate)]
/// # #[spark(name = "counter")]
/// # pub struct Counter {
/// #     pub count: i64,
/// #     #[spark(model)]
/// #     pub step: i64,
/// # }
/// #[actions]
/// impl Counter {
///     pub async fn save(&mut self, ctx: &mut SparkCtx) -> Result<()> {
///         ctx.validate(self).await?;
///         ctx.flash("status", "Saved.");
///         ctx.dispatch("saved", json!({ "count": self.count }));
///         Ok(())
///     }
/// }
/// # fn main() {}
/// ```
pub struct SparkCtx {
    app: App,
    session: Option<Session>,
    auth: Option<Auth>,
    id: String,
    name: &'static str,
    props: serde_json::Map<String, serde_json::Value>,
    pub(crate) effects: Effects,
}

impl std::fmt::Debug for SparkCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SparkCtx")
            .field("name", &self.name)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// What the browser does after an update.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Effects {
    pub(crate) redirect: Option<String>,
    pub(crate) dispatches: Vec<Dispatch>,
    /// A redirect target `redirect` refused: the update fails instead of answering.
    #[serde(skip)]
    pub(crate) refused: Option<String>,
}

/// `(scheme, authority)` of an absolute `http`/`https` URL made of visible ASCII without `\`.
fn absolute_http(url: &str) -> Option<(&'static str, &str)> {
    if !url.bytes().all(|b| b.is_ascii_graphic() && b != b'\\') {
        return None;
    }
    let lower = url
        .get(..8)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let (scheme, rest) = if lower.starts_with("https://") {
        ("https", url.get(8..)?)
    } else if lower.starts_with("http://") {
        ("http", url.get(7..)?)
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    (!authority.is_empty()).then_some((scheme, authority))
}

/// Whether `url` is an absolute URL with the scheme and host (and port) of `app_url`.
fn same_origin(url: &str, app_url: &str) -> bool {
    match (absolute_http(url), absolute_http(app_url)) {
        (Some((s1, a1)), Some((s2, a2))) => s1 == s2 && a1.eq_ignore_ascii_case(a2),
        _ => false,
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Dispatch {
    pub(crate) event: String,
    pub(crate) payload: serde_json::Value,
}

impl SparkCtx {
    pub(crate) fn new(
        app: App,
        session: Option<Session>,
        auth: Option<Auth>,
        id: String,
        name: &'static str,
        props: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self {
            app,
            session,
            auth,
            id,
            name,
            props,
            effects: Effects::default(),
        }
    }

    /// The app.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The database.
    ///
    /// # Errors
    /// The app has no database.
    pub fn db(&self) -> Result<Db> {
        self.app.db()
    }

    /// The visitor's sign-in (`None` when the page has no session, e.g. a view rendered from an API route).
    pub fn auth(&self) -> Option<&Auth> {
        self.auth.as_ref()
    }

    /// The signed-in user's id.
    pub fn user_id(&self) -> Option<i64> {
        self.auth.as_ref().and_then(Auth::id)
    }

    /// The visitor's session. During `mount` on a page render the session has already been saved, so changes
    /// made there are not kept; in actions they are.
    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    /// This instance's id (`wire:id`).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The component's name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// A prop from `@spark("name", { key: value })`, during `mount`.
    pub fn prop<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        self.props
            .get(name)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Send the browser to `url` after this update (instead of re-rendering the component).
    ///
    /// `url` is a path on this site (`/posts/3`: one leading `/`, visible ASCII, no `\`) or an absolute URL on
    /// `APP_URL`'s scheme and host. Any other target (`javascript:`, `//other.host`, another site) is refused: the
    /// update answers 500 and logs why, so a URL taken from data can never run script or leave the site. Use
    /// [`SparkCtx::redirect_away`] to send the browser to another site on purpose.
    pub fn redirect(&mut self, url: impl Into<String>) {
        let url = url.into();
        if is_local_path(&url) || same_origin(&url, &self.app.settings().url) {
            self.effects.redirect = Some(url);
        } else {
            self.effects.refused = Some(url);
        }
    }

    /// Send the browser to another site after this update: `url` is an absolute `http://` or `https://` URL
    /// (visible ASCII, no `\`). Any other target is refused like in [`SparkCtx::redirect`].
    pub fn redirect_away(&mut self, url: impl Into<String>) {
        let url = url.into();
        if absolute_http(&url).is_some() {
            self.effects.redirect = Some(url);
        } else {
            self.effects.refused = Some(url);
        }
    }

    /// Fire the browser event `event` with `payload` (`window` receives a `CustomEvent` whose `detail` is the
    /// payload) after this update.
    pub fn dispatch(&mut self, event: impl Into<String>, payload: impl Serialize) {
        let payload = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        self.effects.dispatches.push(Dispatch {
            event: event.into(),
            payload,
        });
    }

    /// Flash `value` under `key` to the session: `session("key")` shows it in this render and on the next page.
    pub fn flash(&mut self, key: &str, value: impl Serialize) {
        match &self.session {
            Some(session) => session.flash(key, value),
            None => tracing::warn!(key, "flash without a session (the page has no session)"),
        }
    }

    /// Check the component's `#[derive(Validate)]` rules. On failure the error carries the messages: return it
    /// with `?` and the component re-renders with them (`@error("field")` in its view).
    ///
    /// # Errors
    /// [`Error::Validation`] with every message, or a database error from the `unique` / `exists` rules.
    pub async fn validate<T: Validate + Serialize>(&self, component: &T) -> Result<()> {
        let input = input_of(component)?;
        let ctx = ValidationContext::new(&input, self.app.db().ok());
        let result = component.validate(&ctx).await;
        if let Some(error) = ctx.take_failure() {
            return Err(error);
        }
        result.map_err(invalid)
    }

    /// Fail with one validation message on `field` (shown by `@error("field")`).
    pub fn error(&self, field: &str, message: &str) -> Error {
        Error::validation(field, message)
    }
}

/// `Error::Validation` from `errors`.
pub(crate) fn invalid(errors: ValidationErrors) -> Error {
    Error::Validation(Box::new(Invalid::new(errors, Input::new())))
}

/// The component's fields as validation input: text as is, `null` as empty, other values as JSON.
fn input_of<T: Serialize>(component: &T) -> Result<Input> {
    let value = serde_json::to_value(component)?;
    let mut input = Input::new();
    if let serde_json::Value::Object(map) = value {
        for (k, v) in map {
            let text = match v {
                serde_json::Value::String(s) => s,
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            input.insert(k, text);
        }
    }
    Ok(input)
}
