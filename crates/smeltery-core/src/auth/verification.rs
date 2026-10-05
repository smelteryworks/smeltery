//! Email verification: signed, expiring links that set `users.email_verified_at`, the
//! `verified` middleware, and resending with a throttle.
//!
//! An app opts in with [`MustVerifyEmail`] on its user model and
//! [`AppBuilder::verify_email`](crate::AppBuilder::verify_email) in `bootstrap/app.rs`. Without
//! them the `verified` middleware lets every signed-in user (never a guest) through and the
//! helpers here send nothing.
//!
//! A link is `APP_URL` + the route named `verification.verify` (else `/email/verify/{id}/{hash}`)
//! with `?expires=<unix seconds>&signature=<…>`: `hash` is the SHA-256 (hex) of the user's
//! current email and `signature` an HMAC-SHA256 of `id|hash|expires` under a key derived from
//! `APP_KEY` for this purpose ([`App::sign`]). A link therefore works only for that user, only
//! until it expires (`AUTH_VERIFICATION_EXPIRE`, 60 minutes by default), and no longer once the
//! email changes or `APP_KEY` is rotated. No token is stored.
//!
//! The routes a web app declares (the handlers are the app's own):
//!
//! ```
//! # extern crate smeltery_core as smeltery;
//! use smeltery::auth::{Auth, EmailVerificationRequest};
//! use smeltery::http::{Back, IntoResponse, Redirect};
//! use smeltery::session::Session;
//! use smeltery::{App, Response, Result};
//!
//! /// `GET /email/verify`: "check your inbox", or home when there is nothing to verify.
//! async fn notice(app: App, auth: Auth) -> Result<Response> {
//!     if auth.has_verified_email().await? {
//!         return Ok(Redirect::to(&app.settings().auth_home).into_response());
//!     }
//!     Ok("Check your inbox for the verification link.".into_response())
//! }
//!
//! /// `GET /email/verify/{id}/{hash}`: the link from the mail.
//! async fn verify(app: App, session: Session, request: EmailVerificationRequest) -> Result<Redirect> {
//!     request.fulfill().await?;
//!     session.flash("status", "Your email address is verified.");
//!     Ok(Redirect::to(&app.settings().auth_home))
//! }
//!
//! /// `POST /email/verification-notification`: send the link again.
//! async fn send(auth: Auth, session: Session, back: Back) -> Result<Redirect> {
//!     auth.resend_verification_email().await?;
//!     session.flash("status", "verification-link-sent");
//!     Ok(back.redirect())
//! }
//!
//! fn routes(r: &mut smeltery::routing::Router) {
//!     r.get("/email/verify", notice).name("verification.notice").middleware("auth");
//!     r.get("/email/verify/{id}/{hash}", verify)
//!         .name("verification.verify")
//!         .middleware("auth");
//!     r.post("/email/verification-notification", send)
//!         .name("verification.send")
//!         .middleware("auth");
//! }
//! # fn main() {}
//! ```
//!
//! The link goes to the [`VerificationNotifier`] service when one is installed (the mail crate's
//! `.mail()` installs one that sends a mail); otherwise it is written to the log at `info`
//! (target `smeltery::mail`) under `APP_ENV` `local` or `testing` only.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::response::{IntoResponse, Response};
use http::StatusCode;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, Iterable, PrimaryKeyToColumn, QueryFilter};

use super::{Auth, DynUser, column};
use crate::app::{App, BoxFuture};

use crate::db::{Db, Record, timestamp_value};
use crate::error::{Error, Result};
use crate::middleware::{Next, Request};

/// A user model whose email address must be verified: opted in with
/// [`AppBuilder::verify_email`](crate::AppBuilder::verify_email). Verifying sets the model's
/// `email_verified_at` column (a nullable `datetime`) to the current time.
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
/// impl smeltery::auth::MustVerifyEmail for Model {
///     fn email(&self) -> &str { &self.email }
///     fn email_verified_at(&self) -> Option<DateTimeUtc> { self.email_verified_at }
/// }
/// # fn main() {}
/// ```
pub trait MustVerifyEmail: super::Authenticatable {
    /// The email address links are sent to (the `email` column).
    fn email(&self) -> &str;
    /// When the address was verified (the `email_verified_at` column); `None` until then.
    fn email_verified_at(&self) -> Option<sea_orm::prelude::DateTimeUtc>;

    /// Whether the address is verified.
    fn has_verified_email(&self) -> bool {
        self.email_verified_at().is_some()
    }
}

/// Delivers email verification links: registered as the service
/// `Arc<dyn VerificationNotifier>` (`smeltery::mail` installs one that sends the
/// `VerifyEmail` mail). Without one the link is logged in local development only.
pub trait VerificationNotifier: Send + Sync + 'static {
    /// Send `url` (the verification link) to `email`.
    ///
    /// # Errors
    /// The delivery failed.
    fn send<'a>(&'a self, app: &'a App, email: &'a str, url: &'a str) -> BoxFuture<'a, Result<()>>;
}

/// Verification mails one user may ask for in a minute
/// ([`Auth::resend_verification_email`]).
pub const MAX_SENDS: u32 = 6;

/// The [`App::sign`] purpose of verification links.
const PURPOSE: &str = "email-verification";

/// The 403 for a link that is not valid for the signed-in user.
const INVALID_LINK: &str = "This verification link is invalid or has expired.";

/// [`MustVerifyEmail`] without its type, for the app.
pub(crate) trait EmailVerifier: Send + Sync {
    /// The user's email and whether it is verified; `None` when `user` is another model.
    fn state(&self, user: &dyn DynUser) -> Option<(String, bool)>;
    /// Set `email_verified_at` of user `id` when it is empty; `true` when it was.
    fn mark_verified<'a>(&'a self, db: &'a Db, id: i64) -> BoxFuture<'a, Result<bool>>;
    /// Empty `email_verified_at` of user `id`; `true` when it was set.
    fn mark_unverified<'a>(&'a self, db: &'a Db, id: i64) -> BoxFuture<'a, Result<bool>>;
}

pub(crate) struct ModelVerifier<U>(PhantomData<fn() -> U>);

impl<U> ModelVerifier<U> {
    pub(crate) fn new() -> Self {
        Self(PhantomData)
    }
}

impl<U: Record + MustVerifyEmail> EmailVerifier for ModelVerifier<U> {
    fn state(&self, user: &dyn DynUser) -> Option<(String, bool)> {
        user.as_any()
            .downcast_ref::<U>()
            .map(|u| (u.email().to_owned(), u.has_verified_email()))
    }

    fn mark_verified<'a>(&'a self, db: &'a Db, id: i64) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let now = sea_orm::prelude::ChronoUtc::now();
            let verified_at = column::<U>("email_verified_at")?;
            // The value in the column's own Rust type, like `Record::update` stamps.
            let at = timestamp_value::<U::Entity>(verified_at, now).ok_or_else(|| {
                Error::internal(
                    "the user model's `email_verified_at` must be an optional date-time \
                     (`Option<DateTimeUtc>`)",
                )
            })?;
            let key = <<U::Entity as EntityTrait>::PrimaryKey as Iterable>::iter()
                .next()
                .ok_or_else(|| Error::internal("the user model has no primary key"))?
                .into_column();
            // One conditional UPDATE: a second click (or two at once) leaves the first time.
            let mut update =
                <U::Entity as EntityTrait>::update_many().col_expr(verified_at, Expr::value(at));
            // `updated_at` moves too when the model has it: the row changed.
            if let Ok(updated_at) = column::<U>("updated_at")
                && let Some(at) = timestamp_value::<U::Entity>(updated_at, now)
            {
                update = update.col_expr(updated_at, Expr::value(at));
            }
            let result = update
                .filter(key.eq(id))
                .filter(verified_at.is_null())
                .exec(db.conn())
                .await?;
            Ok(result.rows_affected > 0)
        })
    }

    fn mark_unverified<'a>(&'a self, db: &'a Db, id: i64) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let verified_at = column::<U>("email_verified_at")?;
            let key = <<U::Entity as EntityTrait>::PrimaryKey as Iterable>::iter()
                .next()
                .ok_or_else(|| Error::internal("the user model has no primary key"))?
                .into_column();
            let mut update = <U::Entity as EntityTrait>::update_many().col_expr(
                verified_at,
                Expr::value(sea_orm::Value::ChronoDateTimeUtc(None)),
            );
            if let Ok(updated_at) = column::<U>("updated_at")
                && let Some(at) =
                    timestamp_value::<U::Entity>(updated_at, sea_orm::prelude::ChronoUtc::now())
            {
                update = update.col_expr(updated_at, Expr::value(at));
            }
            let result = update
                .filter(key.eq(id))
                .filter(verified_at.is_not_null())
                .exec(db.conn())
                .await?;
            Ok(result.rows_affected > 0)
        })
    }
}

/// Set user `id`'s `email_verified_at` to now unless it is set: `true` when this call verified the address.
///
/// # Errors
/// The app does not require verification ([`AppBuilder::verify_email`](crate::AppBuilder::verify_email)), there is
/// no database, or the update fails.
pub async fn mark_verified(app: &App, id: i64) -> Result<bool> {
    let verifier = app.verifier().ok_or_else(no_verifier)?;
    verifier.mark_verified(&app.db()?, id).await
}

/// Empty user `id`'s `email_verified_at` (after the address changed): `true` when it was set. The `verified`
/// middleware then turns the user away until a new link is used.
///
/// # Errors
/// The app does not require verification ([`AppBuilder::verify_email`](crate::AppBuilder::verify_email)), there is
/// no database, or the update fails.
pub async fn mark_unverified(app: &App, id: i64) -> Result<bool> {
    let verifier = app.verifier().ok_or_else(no_verifier)?;
    verifier.mark_unverified(&app.db()?, id).await
}

fn no_verifier() -> Error {
    Error::internal(
        "the app does not verify addresses: call `.verify_email::<User>()` in bootstrap/app.rs",
    )
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn signed_message(id: i64, hash: &str, expires: u64) -> String {
    format!("{id}|{hash}|{expires}")
}

/// The link for user `id` with this email, valid for `AUTH_VERIFICATION_EXPIRE`.
fn link(app: &App, id: i64, email: &str) -> Result<String> {
    let hash = crate::crypto::sha256_hex(email);
    let expires = now_secs().saturating_add(app.settings().verification_expire.as_secs());
    let signature = app.sign(PURPOSE, signed_message(id, &hash, expires).as_bytes())?;
    let id = id.to_string();
    let expires = expires.to_string();
    let params = [
        ("id", id.as_str()),
        ("hash", hash.as_str()),
        ("expires", expires.as_str()),
        ("signature", signature.as_str()),
    ];
    let path = match app.url("verification.verify", &params) {
        Ok(path) => path,
        Err(_) => {
            let query = serde_urlencoded::to_string([
                ("expires", expires.as_str()),
                ("signature", signature.as_str()),
            ])
            .map_err(Error::other)?;
            format!("/email/verify/{id}/{hash}?{query}")
        }
    };
    // Always APP_URL, never the request's Host header: a forged Host must not put an
    // attacker's domain into the mail.
    Ok(format!(
        "{}{path}",
        app.settings().url.trim_end_matches('/')
    ))
}

/// Whether the link parts are valid now for user `id` whose email is `email`.
fn valid_link(app: &App, id: i64, email: &str, hash: &str, expires: &str, signature: &str) -> bool {
    let Ok(expires) = expires.parse::<u64>() else {
        return false;
    };
    let current = crate::crypto::sha256_hex(email);
    crate::crypto::same(hash, &current)
        && now_secs() < expires
        && app.verify_signature(
            PURPOSE,
            signed_message(id, hash, expires).as_bytes(),
            signature,
        )
}

/// The verification link of `user`: `APP_URL`, the route named `verification.verify` (else
/// `/email/verify/{id}/{hash}`), `?expires=…&signature=…`. Valid for
/// `AUTH_VERIFICATION_EXPIRE` minutes (default 60) and only while the email stays the same.
///
/// # Errors
/// The app has no usable `APP_KEY`.
pub fn verification_url<U: MustVerifyEmail>(app: &App, user: &U) -> Result<String> {
    link(app, user.auth_id(), user.email())
}

/// Send `user` their verification link through the [`VerificationNotifier`] service. Without
/// one, the link is written to the log at `info` under `APP_ENV` `local` or `testing`; in any
/// other environment only a warning that no mail is set up is logged, never the link.
///
/// # Errors
/// No usable `APP_KEY`, or the notifier fails.
pub async fn send_verification_link<U: MustVerifyEmail>(app: &App, user: &U) -> Result<()> {
    deliver(app, user.email(), &verification_url(app, user)?).await
}

async fn deliver(app: &App, email: &str, url: &str) -> Result<()> {
    if let Some(notifier) = app.service::<Arc<dyn VerificationNotifier>>() {
        return notifier.send(app, email, url).await;
    }
    // The link signs the user's address as verified: a live credential outside development
    // (CLAUDE.md: secrets are never logged; D-197).
    if app.settings().is_local_development() {
        tracing::info!(target: "smeltery::mail", verification_url = %url, "email verification link");
    } else {
        tracing::warn!(
            target: "smeltery::mail",
            "an email verification link was requested, but no mail is set up (`.mail()` in bootstrap/app.rs), so no link was sent"
        );
    }
    Ok(())
}

fn model_mismatch() -> Error {
    Error::internal("the signed-in user is not the model given to `.verify_email::<User>()`")
}

impl Auth {
    /// The signed-in user's email and whether it is verified, when the app requires
    /// verification and someone is signed in.
    async fn verification_state(&self) -> Result<Option<(i64, String, bool)>> {
        let Some(verifier) = self.app().verifier() else {
            return Ok(None);
        };
        let Some(user) = self.dyn_user().await? else {
            return Ok(None);
        };
        let (email, verified) = verifier.state(&*user).ok_or_else(model_mismatch)?;
        Ok(Some((user.id(), email, verified)))
    }

    /// Whether the signed-in user may pass the `verified` middleware: `true` when their
    /// `email_verified_at` is set, or when the app does not require verification
    /// ([`AppBuilder::verify_email`](crate::AppBuilder::verify_email)); `false` for guests.
    /// It reads the user loaded once per request (like [`Auth::user`]).
    ///
    /// # Errors
    /// Loading the user fails.
    pub async fn has_verified_email(&self) -> Result<bool> {
        if !self.check() {
            return Ok(false);
        }
        Ok(self
            .verification_state()
            .await?
            .is_none_or(|(_, _, verified)| verified))
    }

    /// Issue the signed-in user's verification link and hand it to the notifier (see
    /// [`send_verification_link`]): `Ok(true)` when a link was issued. Nothing is issued, and
    /// `Ok(false)` returned, for a guest, a verified user, or an app that does not require
    /// verification. The user's row is read again first, so a handler that has just changed
    /// the email (and cleared `email_verified_at`) sends to the new address. A register
    /// handler calls it right after [`Auth::login`].
    ///
    /// # Errors
    /// Loading the user fails, there is no usable `APP_KEY`, or the notifier fails.
    pub async fn send_verification_email(&self) -> Result<bool> {
        self.forget_user();
        let Some((id, email, verified)) = self.verification_state().await? else {
            return Ok(false);
        };
        if verified {
            return Ok(false);
        }
        deliver(self.app(), &email, &link(self.app(), id, &email)?).await?;
        Ok(true)
    }

    /// [`Auth::send_verification_email`], at most [`MAX_SENDS`] (six) times a minute per user:
    /// beyond that it fails with "Too many verification emails. Please try again in N
    /// seconds.", which JSON clients get as 429 and web forms as a redirect back with the
    /// message on `email`. Every call by a signed-in user counts; the check and the count are
    /// one step, so concurrent requests cannot pass more than six times.
    ///
    /// # Errors
    /// Too many calls, or what [`Auth::send_verification_email`] fails with.
    pub async fn resend_verification_email(&self) -> Result<bool> {
        if let Some(id) = self.id() {
            self.app()
                .verify_throttle()
                .try_hit(&id.to_string())
                .map_err(|t| too_many(t.retry_after))?;
        }
        self.send_verification_email().await
    }
}

fn too_many(retry_after: u64) -> Error {
    let message =
        format!("Too many verification emails. Please try again in {retry_after} seconds.");
    Error::Validation(Box::new(crate::validation::Invalid::too_many(
        "email",
        message,
        retry_after,
    )))
}

/// The verification link of the request, checked, as a handler argument. It needs the user to
/// be signed in (put the route behind `auth`): it reads the route parameters `id` and `hash`
/// and the query's `expires` and `signature`, and answers 403 "This verification link is invalid or has expired." unless
/// the signature is valid, the link has not expired, `id` is the signed-in user and `hash`
/// matches their current email. [`fulfill`](Self::fulfill) then marks the email verified.
#[derive(Debug)]
pub struct EmailVerificationRequest {
    auth: Auth,
    id: i64,
}

impl EmailVerificationRequest {
    /// The verified user's id.
    pub fn user_id(&self) -> i64 {
        self.id
    }

    /// Set the user's `email_verified_at` to now, unless it is set already: `true` when this
    /// call verified the email, `false` when it was verified before (a link used twice).
    ///
    /// # Errors
    /// The update fails.
    pub async fn fulfill(&self) -> Result<bool> {
        let verifier = self.auth.app().verifier().ok_or_else(model_mismatch)?;
        let newly = verifier
            .mark_verified(&self.auth.app().db()?, self.id)
            .await?;
        self.auth.forget_user();
        Ok(newly)
    }
}

impl axum::extract::FromRequestParts<App> for EmailVerificationRequest {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        let auth = Auth::from_request_parts(parts, app).await?;
        if !auth.check() {
            return Err(Error::unauthorized());
        }
        let invalid = || Error::http(StatusCode::FORBIDDEN, INVALID_LINK);
        let params = axum::extract::Path::<HashMap<String, String>>::from_request_parts(parts, app)
            .await
            .map_err(|_| invalid())?
            .0;
        let query: HashMap<String, String> =
            form_urlencoded::parse(parts.uri.query().unwrap_or_default().as_bytes())
                .into_owned()
                .collect();
        let (Some(id), Some(hash), Some(expires), Some(signature)) = (
            params.get("id").and_then(|id| id.parse::<i64>().ok()),
            params.get("hash"),
            query.get("expires"),
            query.get("signature"),
        ) else {
            return Err(invalid());
        };
        let Some((user, email, _)) = auth.verification_state().await? else {
            return Err(invalid());
        };
        if user != id || !valid_link(app, id, &email, hash, expires, signature) {
            return Err(invalid());
        }
        Ok(Self { auth, id })
    }
}

/// The `verified` middleware: requests without a principal (a signed-in session, or a guard's principal stored by
/// an `auth:` middleware listed before it), and (when the app requires verification) principals whose user's
/// email is not verified, get a 303 to the route named `verification.notice` (or `/email/verify`); JSON clients
/// and API routes a 403 `{"error": "Your email address is not verified."}`. A guard's principal whose user row
/// is gone gets 401 `{"error": "Unauthenticated."}`.
pub(crate) async fn require_verified(req: Request, next: Next) -> Response {
    let app = req.extensions().get::<App>().cloned();
    // Guests never pass, whether or not the app requires verification; `has_verified_email` is
    // `false` for them and `true` for signed-in users of an app without `.verify_email`.
    // A signed-in session through `Auth`; any other principal (a guard's token) through its user.
    let auth = req.extensions().get::<Auth>().cloned().filter(Auth::check);
    let principal = req.extensions().get::<super::Principal>().cloned();
    let verified = match (auth, principal, &app) {
        (Some(auth), _, _) => auth.has_verified_email().await,
        (None, Some(principal), Some(app)) => match principal_verified(app, &principal).await {
            Ok(None) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({ "error": "Unauthenticated." })),
                )
                    .into_response();
            }
            other => other.map(|v| v.unwrap_or(false)),
        },
        _ => Ok(false),
    };
    let verified = match verified {
        Ok(verified) => verified,
        Err(e) => return e.into_response(),
    };
    if verified {
        return next.run(req).await;
    }
    // API routes (no session) always answer JSON.
    let api = req.extensions().get::<crate::session::Session>().is_none();
    if api || crate::error::wants_json(req.headers()) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({ "error": "Your email address is not verified." })),
        )
            .into_response();
    }
    let notice = app
        .and_then(|app| app.url("verification.notice", &[]).ok())
        .unwrap_or_else(|| "/email/verify".to_owned());
    axum::response::Redirect::to(&notice).into_response()
}

/// Whether the user of `principal` may pass `verified` (their email is verified, or the app does not require
/// it); `None` when the user's row is gone.
async fn principal_verified(app: &App, principal: &super::Principal) -> Result<Option<bool>> {
    let Some(user) = principal.auth_user(app).await? else {
        return Ok(None);
    };
    let Some(verifier) = app.verifier() else {
        return Ok(Some(true));
    };
    let (_, verified) = verifier
        .state(&**user.dyn_user())
        .ok_or_else(model_mismatch)?;
    Ok(Some(verified))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppBuilder;
    use crate::config::Settings;

    async fn app(expire_secs: u64) -> App {
        let mut builder = AppBuilder::new(Settings::from_env());
        builder.settings_mut().key = "0123456789abcdef0123456789abcdef".into();
        builder.settings_mut().url = "https://app.example/".into();
        builder.settings_mut().verification_expire = std::time::Duration::from_secs(expire_secs);
        builder.build().await.unwrap().app
    }

    fn parts(url: &str) -> (String, String, String, String) {
        let rest = url
            .strip_prefix("https://app.example/email/verify/")
            .unwrap();
        let (path, query) = rest.split_once('?').unwrap();
        let (id, hash) = path.split_once('/').unwrap();
        let query: HashMap<String, String> = form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        (
            id.to_owned(),
            hash.to_owned(),
            query["expires"].clone(),
            query["signature"].clone(),
        )
    }

    #[tokio::test]
    async fn links_bind_the_user_the_email_and_the_expiry() {
        let app = app(3600).await;
        let url = link(&app, 7, "ada@example.com").unwrap();
        assert!(!url.contains("ada@example.com"), "{url}");
        let (id, hash, expires, signature) = parts(&url);
        assert_eq!(id, "7");
        assert_eq!(hash, crate::crypto::sha256_hex("ada@example.com"));
        let ok = |id, email: &str, hash: &str, expires: &str, sig: &str| {
            valid_link(&app, id, email, hash, expires, sig)
        };
        assert!(ok(7, "ada@example.com", &hash, &expires, &signature));
        // Another user, a changed email, a longer expiry or another signature: invalid.
        assert!(!ok(8, "ada@example.com", &hash, &expires, &signature));
        assert!(!ok(7, "new@example.com", &hash, &expires, &signature));
        let other = crate::crypto::sha256_hex("new@example.com");
        assert!(!ok(7, "new@example.com", &other, &expires, &signature));
        let later = (expires.parse::<u64>().unwrap() + 3600).to_string();
        assert!(!ok(7, "ada@example.com", &hash, &later, &signature));
        assert!(!ok(7, "ada@example.com", &hash, "x", &signature));
        assert!(!ok(7, "ada@example.com", &hash, &expires, "forged"));
        // A signature for another purpose does not verify here.
        let foreign = app
            .sign(
                "other",
                signed_message(7, &hash, expires.parse().unwrap()).as_bytes(),
            )
            .unwrap();
        assert!(!ok(7, "ada@example.com", &hash, &expires, &foreign));
    }

    #[tokio::test]
    async fn links_expire() {
        let app = app(0).await;
        let (_, hash, expires, signature) = parts(&link(&app, 7, "ada@example.com").unwrap());
        assert!(!valid_link(
            &app,
            7,
            "ada@example.com",
            &hash,
            &expires,
            &signature
        ));
    }

    #[test]
    fn the_throttle_error_is_429_on_email() {
        let err = too_many(42);
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            err.to_string(),
            "Too many verification emails. Please try again in 42 seconds."
        );
    }
}
