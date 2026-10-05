//! Credentials without a session ([`verify_credentials`]), password changes ([`Auth::set_password`],
//! [`password_changed`]), what runs when a user's credentials change ([`CredentialListener`], [`AuthEvent`] on the
//! PubSub topic [`EVENTS_TOPIC`]), the [`SecondFactor`] and [`LoginPolicy`] seams and password confirmation
//! ([`Auth::confirm_password`], the `password.confirm` middleware).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::response::{IntoResponse, Response};
use http::StatusCode;
use serde::{Deserialize, Serialize};

use super::{
    Auth, AuthUser, Authenticatable, CredentialKind, DUMMY_HASH, Principal, hash_password,
    normalize_email, session_binding, throttle_ip, throttle_network, verify_password,
};
use crate::app::{App, AppBuilder, BoxFuture};
use crate::cache::{RateLimit, RateLimiter};
use crate::db::Record;
use crate::error::{Error, Result};
use crate::middleware::{Next, Request};
use crate::session::{AUTH_HASH_KEY, Queued};

/// The PubSub topic [`AuthEvent`]s are published on.
pub const EVENTS_TOPIC: &str = "auth";

/// The session key of the last password confirmation (Unix seconds).
pub(crate) const CONFIRMED_AT_KEY: &str = "_auth.confirmed_at";

/// Session keys with these prefixes belong to one sign-in: they are removed at every sign-in and sign-out.
pub(crate) const RESERVED_PREFIXES: &[&str] = &["_auth.", "_temper."];

/// Password confirmations a signed-in user may try in a minute.
pub const CONFIRM_MAX_ATTEMPTS: u32 = 5;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---- verify_credentials ---------------------------------------------------------------------

/// Check an email and password without signing anyone in: the user when they match, `Ok(None)` otherwise. The
/// check [`Auth::attempt`] signs in with, for endpoints without a session (issuing an API token, a second
/// step before the sign-in).
///
/// `client` is the client address ([`ClientInfo`](crate::http::ClientInfo) / [`Auth::ip`]). Each call counts before
/// the account is looked up and the password checked, against the login budgets: thirty a minute from one client
/// whatever the address, five a minute for one address from one client (an IPv6 client by its /64) and twenty per
/// five minutes for one address from one network (an IPv4 /24, an IPv6 /48), for addresses with and without an
/// account alike. An unknown address is checked against a dummy hash of the same cost, so the answer takes as long.
/// A match resets the address's budgets and gives its hit back to the client's. The budgets live in the process's
/// memory and are shared with [`Auth::attempt`].
///
/// # Errors
/// Too many attempts ([`TooManyAttempts`](super::TooManyAttempts) as an [`Error`]: 429 for JSON clients),
/// too many password checks waiting (503), no user model, or a database failure.
pub async fn verify_credentials(
    app: &App,
    client: &str,
    email: &str,
    password: &str,
) -> Result<Option<AuthUser>> {
    let address = normalize_email(email);
    let ip = throttle_ip(client);
    let key = format!("{address}|{ip}");
    let network = format!("{address}|{}", throttle_network(client));
    // Counted before the account is looked up and the slow password check, each under one lock: a burst of
    // parallel guesses gets exactly the allowed number through, and the answer is the same whether the address
    // has an account.
    app.client_throttle().try_hit(&ip)?;
    app.throttle().try_hit(&key)?;
    app.account_throttle().try_hit(&network)?;
    let provider = app.user_provider().ok_or_else(super::no_user_model)?;
    let user = provider
        .find_by_email(&app.db()?, email)
        .await?
        .map(|(user, _)| user);
    let hash = user.as_ref().map_or(DUMMY_HASH, |u| u.hash()).to_owned();
    let ok = verify_password(password, &hash).await? && user.is_some();
    match user {
        Some(user) if ok => {
            app.client_throttle().refund(&ip);
            app.throttle().clear(&key);
            app.account_throttle().clear(&network);
            Ok(Some(AuthUser::new(user)))
        }
        _ => Ok(None),
    }
}

// ---- lookups ---------------------------------------------------------------------------------

/// The user whose stored address is `email` (after [`normalize_email`]; a row counts only when its stored address
/// equals the typed one apart from ASCII letter case), through the registered user model `U`.
///
/// # Errors
/// No user model, `U` is not the registered model, or the query fails.
pub async fn find_by_email<U: Authenticatable>(app: &App, email: &str) -> Result<Option<U>> {
    let provider = app.user_provider().ok_or_else(super::no_user_model)?;
    match provider.find_by_email(&app.db()?, email).await? {
        None => Ok(None),
        Some((user, _)) => AuthUser::new(user)
            .downcast::<U>()
            .map(Some)
            .ok_or_else(|| {
                Error::internal(format!(
                    "`{}` is not the user model registered with `.auth::<…>()`",
                    std::any::type_name::<U>()
                ))
            }),
    }
}

/// A serde `deserialize_with` function for email form fields: the text trimmed and in lower case, before the
/// validation rules run, so ` Ada@Example.com` and `ada@example.com` are one account for registration, sign-in
/// and resets.
///
/// ```
/// #[derive(serde::Deserialize)]
/// struct LoginForm {
///     #[serde(deserialize_with = "smeltery_core::auth::deserialize_email")]
///     email: String,
/// }
///
/// let form: LoginForm = serde_json::from_str(r#"{"email": " Ada@Example.COM "}"#).unwrap();
/// assert_eq!(form.email, "ada@example.com");
/// ```
///
/// # Errors
/// The value is not a string.
pub fn deserialize_email<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let email = <String as Deserialize>::deserialize(deserializer)?;
    Ok(email.trim().to_lowercase())
}

/// The column of the model `U` named `name` (e.g. `"email"`), to read or write it through the model's entity.
///
/// # Errors
/// `U` has no such column.
pub fn model_column<U: Record>(name: &str) -> Result<<U::Entity as sea_orm::EntityTrait>::Column> {
    super::column::<U>(name)
}

// ---- listeners and events --------------------------------------------------------------------

/// Why a user's credentials changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CredentialChange {
    /// A password reset ([`passwords::reset`](super::passwords::reset)).
    Reset,
    /// A new password ([`Auth::set_password`], [`password_changed`]).
    Changed,
    /// [`Auth::logout_other_devices`].
    OtherDevicesLoggedOut,
    /// [`end_credentials`]: the app ended the user's credentials without a password change (a suspension).
    Ended,
}

/// What a [`CredentialListener`] is told.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CredentialsChanged {
    /// The user.
    pub user_id: i64,
    /// Why.
    pub why: CredentialChange,
    /// The credential of the request that made the change ([`Principal::key`]), which stays valid; `None` when no
    /// credential stays (a reset).
    pub except: Option<String>,
    /// For a [`CredentialChange::Reset`]: the reset marked a previously unverified address verified (the reset link
    /// proved control of it). A listener then removes every login method and second factor added while the address
    /// was unverified (someone who registered the address first may have added them). `false` otherwise.
    pub was_unverified: bool,
}

/// Runs when a user's credentials change, before the request that changed them answers (a guard crate deletes the
/// user's tokens here). Register with [`AppBuilder::credential_listener`].
///
/// ```
/// use smeltery_core::auth::{CredentialListener, CredentialsChanged};
/// use smeltery_core::{App, BoxFuture, Result};
///
/// struct Audit;
///
/// impl CredentialListener for Audit {
///     fn credentials_changed<'a>(&'a self, _app: &'a App, change: &'a CredentialsChanged)
///         -> BoxFuture<'a, Result<()>> {
///         Box::pin(async move {
///             tracing::info!(user_id = change.user_id, why = ?change.why, "credentials changed");
///             Ok(())
///         })
///     }
/// }
///
/// fn build(app: smeltery_core::AppBuilder) -> smeltery_core::AppBuilder {
///     app.credential_listener(Audit)
/// }
/// # let _ = build;
/// ```
pub trait CredentialListener: Send + Sync + 'static {
    /// Handle the change. An error fails the request (the password change itself is stored already) and is logged
    /// with the user id; the other listeners still run and the [`AuthEvent`] is still published.
    fn credentials_changed<'a>(
        &'a self,
        app: &'a App,
        change: &'a CredentialsChanged,
    ) -> BoxFuture<'a, Result<()>>;
}

impl AppBuilder {
    /// Register a [`SecondFactor`] (a two-factor implementation); [`App::second_factor`] returns it.
    pub fn second_factor(self, second_factor: impl SecondFactor) -> Self {
        self.service::<Arc<dyn SecondFactor>>(Arc::new(second_factor))
    }
}

impl App {
    /// Whether the app requires verified addresses ([`AppBuilder::verify_email`]).
    pub fn verifies_email(&self) -> bool {
        self.verifier().is_some()
    }

    /// The registered [`SecondFactor`], if any.
    pub fn second_factor(&self) -> Option<Arc<dyn SecondFactor>> {
        self.service::<Arc<dyn SecondFactor>>()
            .map(|s| Arc::clone(&*s))
    }
}

/// An authentication event, published on the PubSub topic [`EVENTS_TOPIC`] (`auth`) when credentials end, so other
/// processes and crates (open sockets, caches) can act on it. Delivery is at most once (PubSub's rule).
///
/// As JSON: `{"type":"revoked","user_id":7,"key":"web:session:…"}` or
/// `{"type":"revoked_all","user_id":7,"kind":"every","except":"web:session:…"}`. Keys are [`Principal::key`]'s:
/// `<guard>:session:<binding>` or `<guard>:token:<id>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthEvent {
    /// One credential ended (a session signed out, a token revoked).
    Revoked {
        /// The user.
        user_id: i64,
        /// The credential ([`Principal::key`]).
        key: String,
    },
    /// Every credential of the user of `kind` ended, except `except` when set.
    RevokedAll {
        /// The user.
        user_id: i64,
        /// Which kind of credential.
        kind: CredentialKind,
        /// The credential that stays ([`Principal::key`]).
        except: Option<String>,
    },
}

impl AuthEvent {
    /// Whether this event ends the credential `key` ([`Principal::key`]: `web:session:<binding>`,
    /// `hallmark:token:12`) of user `user_id`: a [`Revoked`](Self::Revoked) naming that key, or a
    /// [`RevokedAll`](Self::RevokedAll) of the user whose kind covers the key's kind and whose `except` is another key.
    ///
    /// ```
    /// use smeltery_core::auth::{AuthEvent, CredentialKind};
    ///
    /// let logout = AuthEvent::Revoked { user_id: 7, key: "web:session:ab12".into() };
    /// assert!(logout.ends(7, "web:session:ab12"));
    /// assert!(!logout.ends(7, "web:session:cd34"));
    /// let others = AuthEvent::RevokedAll {
    ///     user_id: 7,
    ///     kind: CredentialKind::Sessions,
    ///     except: Some("web:session:ab12".into()),
    /// };
    /// assert!(others.ends(7, "web:session:cd34"));
    /// assert!(!others.ends(7, "web:session:ab12"));
    /// assert!(!others.ends(7, "hallmark:token:3"));
    /// assert!(!others.ends(8, "web:session:cd34"));
    /// ```
    pub fn ends(&self, user_id: i64, key: &str) -> bool {
        match self {
            Self::Revoked {
                user_id: user,
                key: ended,
            } => *user == user_id && ended == key,
            Self::RevokedAll {
                user_id: user,
                kind,
                except,
            } => {
                if *user != user_id || except.as_deref() == Some(key) {
                    return false;
                }
                let of_kind = key.split(':').nth(1);
                match kind {
                    CredentialKind::Sessions => of_kind == Some("session"),
                    CredentialKind::Tokens => of_kind == Some("token"),
                    CredentialKind::Every => true,
                }
            }
        }
    }
}

/// Publish `event` on [`EVENTS_TOPIC`]: the subscribers in this process at once, the app's other processes through
/// the PubSub driver.
///
/// # Errors
/// The app has no PubSub, or the driver failed (the subscribers in this process got it).
pub async fn publish_event(app: &App, event: &AuthEvent) -> Result<()> {
    let pubsub =
        crate::pubsub::PubSub::of(app).ok_or_else(|| Error::internal("the app has no PubSub"))?;
    pubsub.publish_reserved(EVENTS_TOPIC, event).await
}

/// Publish without failing the caller: the credential change is done; a lost event is logged.
async fn announce(app: &App, event: AuthEvent) {
    if let Err(e) = publish_event(app, &event).await {
        tracing::warn!(error = %e, "an auth event could not be sent to the app's other processes");
    }
}

/// A user's password (and with it every credential bound to it) changed: run the listeners, then publish
/// `RevokedAll { Every, except }`.
/// Every listener runs even after one fails (a failing audit listener must never keep a token-deleting one from
/// running), and the event is always published; then the first error is returned.
pub(crate) async fn changed(
    app: &App,
    user_id: i64,
    why: CredentialChange,
    except: Option<String>,
    was_unverified: bool,
) -> Result<()> {
    let change = CredentialsChanged {
        user_id,
        why,
        except: except.clone(),
        was_unverified,
    };
    let mut first_error = None;
    for listener in app.credential_listeners() {
        if let Err(e) = listener.credentials_changed(app, &change).await {
            tracing::error!(user_id, error = %e, "a credential listener failed after a password change");
            first_error.get_or_insert(e);
        }
    }
    announce(
        app,
        AuthEvent::RevokedAll {
            user_id,
            kind: CredentialKind::Every,
            except,
        },
    )
    .await;
    first_error.map_or(Ok(()), Err)
}

/// One session signed out.
pub(crate) async fn session_ended(app: &App, user_id: i64, key: String) {
    announce(app, AuthEvent::Revoked { user_id, key }).await;
}

/// Replace user `user_id`'s remember token with a random one nobody holds (only its SHA-256 is stored), through the
/// registered user model: no remember-me cookie of the user signs in any more. Sessions are not touched.
///
/// # Errors
/// No user model or database, or the update fails.
pub async fn cycle_remember_token(app: &App, user_id: i64) -> Result<()> {
    let provider = app.user_provider().ok_or_else(super::no_user_model)?;
    let cycled = crate::crypto::sha256_hex(&crate::crypto::random_token(60)?);
    provider
        .set_remember_token(&app.db()?, user_id, Some(cycled))
        .await
}

/// After app code wrote a new password for user `user_id` itself (an admin form, a console command): end the
/// user's remember-me cookies, run the [`CredentialListener`]s ([`CredentialChange::Changed`]) and publish
/// [`AuthEvent::RevokedAll`]. `except` is the credential of the request that made the change and stays valid, when
/// there is one, and must be a principal of this user. The user's sessions, `except`'s included, end on their next
/// request anyway (they are bound to the old password hash); [`Auth::set_password`] keeps the current one. Check the
/// current password (or a fresh confirmation) before keeping a credential: a stolen credential that changes the
/// password would otherwise keep itself.
///
/// # Errors
/// `except` is another user's principal (nothing changes), no user model or database, a query fails, or a listener
/// fails.
pub async fn password_changed(app: &App, user_id: i64, except: Option<&Principal>) -> Result<()> {
    if let Some(other) = except.filter(|p| p.user_id != user_id) {
        return Err(Error::internal(format!(
            "password_changed for user {user_id} cannot keep a credential of user {}",
            other.user_id
        )));
    }
    // No step skips the next (as `end_credentials`): the listeners and the event run even when the remember-token
    // write failed; then the first error is returned.
    let remember = cycle_remember_token(app, user_id).await;
    let listeners = changed(
        app,
        user_id,
        CredentialChange::Changed,
        except.map(Principal::key),
        false,
    )
    .await;
    remember.and(listeners)
}

/// Sign user `user_id` out everywhere without a password change (a suspended account): add one to the user's
/// `credentials_epoch` (every open web session of the user is signed out on its next request; sign-ins never change
/// it), replace the remember token (no remember-me cookie signs in again), run the [`CredentialListener`]s with
/// [`CredentialChange::Ended`] (a token crate deletes every token of the user there) and publish
/// [`AuthEvent::RevokedAll`] (`Every`, no exception: open sockets close). Pair it with a [`LoginPolicy`] that refuses
/// the user, or the next sign-in gives new credentials.
///
/// # Errors
/// No user model or database, a query fails, or a listener fails (every listener still runs and the event is still
/// published). A user model without the `credentials_epoch` column (or whose
/// [`Authenticatable::credentials_epoch`] does not read it) gets an error after everything else ran: open sessions
/// were not ended.
pub async fn end_credentials(app: &App, user_id: i64) -> Result<()> {
    let provider = app.user_provider().ok_or_else(super::no_user_model)?;
    let db = app.db()?;
    // The remember token first, then the epoch: a remember-me restore that read the row before the new token
    // carries the old epoch and ends on its next request; one that reads it after no longer matches. (The other
    // order lets a restore between the steps match the old token and bind to the new epoch.)
    // No step skips the next: a failure is kept, the listeners run and the event goes out, then the first error
    // is returned (a transient error must never leave tokens and sockets alive).
    let remember = cycle_remember_token(app, user_id).await;
    let sessions = match provider.bump_epoch(&db, user_id).await {
        Ok(_) => match provider.find_by_id(&db, user_id).await {
            Ok(Some(user)) if user.epoch().is_none() => Err(Error::internal(concat!(
                "end_credentials: the user model's `credentials_epoch()` returns None after the column was ",
                "increased; implement it to read the `credentials_epoch` column, or open sessions stay signed in",
            ))),
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        },
        Err(e) => Err(Error::internal(format!(
            "end_credentials could not end open sessions (add a nullable integer `credentials_epoch` column and \
             `Authenticatable::credentials_epoch`): {e}"
        ))),
    };
    let listeners = changed(app, user_id, CredentialChange::Ended, None, false).await;
    for (step, result) in [
        ("the remember token", &remember),
        ("open sessions", &sessions),
    ] {
        if let Err(e) = result {
            tracing::error!(user_id, error = %e, "end_credentials could not end {step}");
        }
    }
    remember.and(sessions).and(listeners)
}

// ---- login completion --------------------------------------------------------------------------

/// What finishes a sign-in after the user is known (a password matched, or another crate proved the user: a
/// social login): an authentication crate implements it (a second-factor step, events, the intended page) and
/// registers it with [`AppBuilder::login_completion`]; other crates call [`App::login_completion`] instead of
/// [`Auth::login`] when one is registered, so every way in meets the same steps.
///
/// ```
/// use smeltery_core::auth::{Auth, AuthUser, LoginCompletion};
/// use smeltery_core::http::{HeaderMap, IntoResponse, Redirect};
/// use smeltery_core::session::Session;
/// use smeltery_core::{App, BoxFuture, Response, Result};
///
/// struct Plain;
///
/// impl LoginCompletion for Plain {
///     fn complete<'a>(&'a self, app: &'a App, auth: &'a Auth, _session: &'a Session, _headers: &'a HeaderMap,
///         user: AuthUser, remember: bool) -> BoxFuture<'a, Result<Response>> {
///         Box::pin(async move {
///             auth.login_user(&user, remember).await?;
///             Ok(auth.intended(&app.settings().auth_home).into_response())
///         })
///     }
/// }
///
/// fn build(app: smeltery_core::AppBuilder) -> smeltery_core::AppBuilder {
///     app.login_completion(Plain)
/// }
/// # let _ = (build, Redirect::to("/"));
/// ```
pub trait LoginCompletion: Send + Sync + 'static {
    /// Finish signing in `user` for this request (`auth` and `session` are the request's, `headers` its headers):
    /// the response to send.
    fn complete<'a>(
        &'a self,
        app: &'a App,
        auth: &'a Auth,
        session: &'a crate::session::Session,
        headers: &'a http::HeaderMap,
        user: AuthUser,
        remember: bool,
    ) -> BoxFuture<'a, Result<axum::response::Response>>;
}

impl AppBuilder {
    /// Register the [`LoginCompletion`]; [`App::login_completion`] returns it.
    pub fn login_completion(self, completion: impl LoginCompletion) -> Self {
        self.service::<Arc<dyn LoginCompletion>>(Arc::new(completion))
    }
}

impl App {
    /// The registered [`LoginCompletion`], if any.
    pub fn login_completion(&self) -> Option<Arc<dyn LoginCompletion>> {
        self.service::<Arc<dyn LoginCompletion>>()
            .map(|s| Arc::clone(&*s))
    }
}

impl Auth {
    /// Sign in `user` (an [`AuthUser`] from [`Auth::validate`], [`verify_credentials`], [`App::find_user`] or
    /// [`AuthUser::of`]). Unlike [`Auth::login`] it works when a [`LoginCompletion`] is registered: it is how the
    /// completion (and only the completion) signs the user in.
    ///
    /// # Errors
    /// Storing the remember token fails.
    pub async fn login_user(&self, user: &AuthUser, remember: bool) -> Result<()> {
        self.login_dyn(Arc::clone(user.dyn_user()), remember).await
    }
}

// ---- second factor ---------------------------------------------------------------------------

/// A second authentication factor (a one-time code): implemented by a two-factor crate and registered with
/// [`AppBuilder::second_factor`]; endpoints that sign in or issue credentials ask it after the password (and after
/// [`App::check_login`]).
/// [`Auth::attempt`] and [`Auth::login`] never ask it: a sign-in flow that supports a second factor checks the
/// password with [`Auth::validate`], asks [`SecondFactor::required`] / [`SecondFactor::verify`] and signs in only
/// after the code.
pub trait SecondFactor: Send + Sync + 'static {
    /// Whether `user` must give a code.
    fn required<'a>(&'a self, app: &'a App, user: &'a AuthUser) -> BoxFuture<'a, Result<bool>>;

    /// Check `code` for `user` (counted and single use, as the implementation defines): valid, invalid, or over the
    /// account's budget (then the code was not checked).
    fn verify<'a>(
        &'a self,
        app: &'a App,
        user: &'a AuthUser,
        code: &'a str,
    ) -> BoxFuture<'a, Result<SecondFactorVerdict>>;
}

/// What [`SecondFactor::verify`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecondFactorVerdict {
    /// The code is right (and now used up).
    Valid,
    /// Wrong, already used, or unreadable.
    Invalid,
    /// The account's code budget is spent: the code was not checked.
    TooManyAttempts {
        /// Seconds until the budget allows another code.
        retry_after: u64,
    },
}

impl SecondFactorVerdict {
    /// Whether the code was accepted.
    pub fn is_valid(self) -> bool {
        self == Self::Valid
    }

    /// `Ok(())` for a valid code; otherwise the answer an endpoint returns: `invalid_message` on `field` (422), or
    /// for a spent budget a 429 with `Retry-After` and "Too many attempts…" on `field`.
    ///
    /// ```
    /// use smeltery_core::auth::SecondFactorVerdict;
    ///
    /// assert!(SecondFactorVerdict::Valid.into_result("code", "The code is invalid.").is_ok());
    /// let err = SecondFactorVerdict::Invalid.into_result("code", "The code is invalid.").unwrap_err();
    /// assert_eq!(err.status().as_u16(), 422);
    /// let err = SecondFactorVerdict::TooManyAttempts { retry_after: 30 }
    ///     .into_result("code", "The code is invalid.")
    ///     .unwrap_err();
    /// assert_eq!(err.status().as_u16(), 429);
    /// ```
    ///
    /// # Errors
    /// The verdict is not [`SecondFactorVerdict::Valid`].
    pub fn into_result(self, field: &str, invalid_message: &str) -> Result<()> {
        match self {
            Self::Valid => Ok(()),
            Self::Invalid => Err(Error::validation(field, invalid_message)),
            Self::TooManyAttempts { retry_after } => Err(Error::Validation(Box::new(
                crate::validation::Invalid::too_many(
                    field,
                    format!("Too many attempts. Please try again in {retry_after} seconds."),
                    retry_after,
                ),
            ))),
        }
    }
}

// ---- login policy ------------------------------------------------------------------------------

/// A rule every new sign-in meets, without a session: "may this user sign in now?" (a suspended account, an IP
/// rule). Register it with [`AppBuilder::login_policy`]. Every endpoint that signs in or issues credentials asks
/// [`App::check_login`] after the password and before the second factor: [`Auth::attempt`], an authentication
/// crate's login steps, its two-factor challenge and its registration, a token endpoint. A remember-me cookie
/// meets it too: a refused restore stays a guest, and the cookie and the user's remember token are replaced.
///
/// Policies gate new sign-ins, token issuance and remember-me restores only. Refusing a user does not end what they
/// already hold: an open web session, issued API tokens, open sockets. To cut those, call [`end_credentials`]
/// (open sessions through the `credentials_epoch` column, remember token, tokens, sockets).
///
/// ```
/// use smeltery_core::auth::{AuthUser, LoginDecision, LoginPolicy};
/// use smeltery_core::http::StatusCode;
/// use smeltery_core::{App, BoxFuture, Result};
///
/// struct NotSuspended;
///
/// impl LoginPolicy for NotSuspended {
///     fn check<'a>(&'a self, _app: &'a App, user: &'a AuthUser) -> BoxFuture<'a, Result<LoginDecision>> {
///         Box::pin(async move {
///             if user.id() == 13 {
///                 return Ok(LoginDecision::refuse(StatusCode::FORBIDDEN, "This account is suspended."));
///             }
///             Ok(LoginDecision::Allow)
///         })
///     }
/// }
///
/// fn build(app: smeltery_core::AppBuilder) -> smeltery_core::AppBuilder {
///     app.login_policy(NotSuspended)
/// }
/// # let _ = build;
/// ```
pub trait LoginPolicy: Send + Sync + 'static {
    /// Whether `user`, whose password (or other proof) is already checked, may sign in now.
    fn check<'a>(
        &'a self,
        app: &'a App,
        user: &'a AuthUser,
    ) -> BoxFuture<'a, Result<LoginDecision>>;
}

/// What a [`LoginPolicy`] decided.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoginDecision {
    /// The user may sign in.
    Allow,
    /// The user may not: answer `status` with `message` (shown to the user, on the `email` field).
    Refuse {
        /// The status (403 for a suspended account, for example). JSON clients get it; browsers are sent back
        /// with the message.
        status: StatusCode,
        /// The message for the user.
        message: String,
    },
}

impl LoginDecision {
    /// A refusal with `status` and `message`.
    pub fn refuse(status: StatusCode, message: impl Into<String>) -> Self {
        Self::Refuse {
            status,
            message: message.into(),
        }
    }
}

/// The registered policies, in registration order.
#[derive(Clone, Default)]
struct LoginPolicies(Vec<Arc<dyn LoginPolicy>>);

impl AppBuilder {
    /// Add a [`LoginPolicy`]. Several may be added; [`App::check_login`] asks them in the order they were added, and
    /// the first refusal wins.
    pub fn login_policy(self, policy: impl LoginPolicy) -> Self {
        let mut policies = self
            .registered_service::<LoginPolicies>()
            .cloned()
            .unwrap_or_default();
        policies.0.push(Arc::new(policy));
        self.service(policies)
    }
}

impl App {
    /// Ask every [`LoginPolicy`] whether `user` may sign in now (no policy: yes). Every endpoint that signs in or
    /// issues credentials calls it after the password check and before the second factor.
    ///
    /// # Errors
    /// A refusal: [`Error::Validation`] with the policy's status, and its message as the summary and on `email`
    /// (JSON clients get the status; browsers are sent back with the message). Or a policy's own error.
    pub async fn check_login(&self, user: &AuthUser) -> Result<()> {
        if let LoginDecision::Refuse { status, message } = self.login_decision(user).await? {
            tracing::info!(
                user_id = user.id(),
                status = status.as_u16(),
                "a login policy refused a sign-in"
            );
            let mut errors = crate::validation::ValidationErrors::new();
            errors.add("email", message.clone());
            let mut invalid =
                crate::validation::Invalid::new(errors, crate::validation::Input::new());
            invalid.status = status;
            invalid.message = message;
            return Err(Error::Validation(Box::new(invalid)));
        }
        Ok(())
    }

    /// The first refusal of the registered policies, else [`LoginDecision::Allow`].
    pub(crate) async fn login_decision(&self, user: &AuthUser) -> Result<LoginDecision> {
        if let Some(policies) = self.service::<LoginPolicies>() {
            for policy in &policies.0 {
                let decision = policy.check(self, user).await?;
                if matches!(decision, LoginDecision::Refuse { .. }) {
                    return Ok(decision);
                }
            }
        }
        Ok(LoginDecision::Allow)
    }
}

// ---- Auth: validate, set_password, confirmation -----------------------------------------------

/// The budget of [`Auth::confirm_password`], one per app.
struct ConfirmLimiter(RateLimiter);

/// "Too many password confirmations…": 429 for JSON clients, a redirect back with the message on `password`.
fn too_many_confirmations(retry_after: u64) -> Error {
    let message = format!("Too many attempts. Please try again in {retry_after} seconds.");
    Error::Validation(Box::new(crate::validation::Invalid::too_many(
        "password",
        message,
        retry_after,
    )))
}

impl Auth {
    /// [`verify_credentials`] for this request's client: the user when the email and password match, without
    /// signing in.
    ///
    /// # Errors
    /// As [`verify_credentials`].
    pub async fn validate(&self, email: &str, password: &str) -> Result<Option<AuthUser>> {
        verify_credentials(self.app(), self.ip(), email, password).await
    }

    /// Change the signed-in user's password and keep this session signed in: the password is hashed with
    /// argon2id and stored by the user's id, this session is bound to the new hash (every other session of the
    /// user ends on its next request), the remember token is replaced (no remember-me cookie signs in any more,
    /// this device's is removed), the [`CredentialListener`]s run ([`CredentialChange::Changed`], except this
    /// session) and [`AuthEvent::RevokedAll`] is published. `Ok(false)` (nothing changes) when nobody is signed in.
    /// Check the current password before calling it.
    ///
    /// # Errors
    /// No user model, a query fails, hashing fails (503 when too many hashes wait), or a listener fails.
    pub async fn set_password(&self, new_password: &str) -> Result<bool> {
        let Some(user) = self.dyn_user().await? else {
            return Ok(false);
        };
        let id = user.id();
        let hash = hash_password(new_password).await?;
        self.provider()?
            .set_column(&self.app().db()?, id, "password", Some(hash.clone()))
            .await?;
        self.session()
            .insert(AUTH_HASH_KEY, session_binding(&hash, user.epoch()));
        self.forget_user();
        // No step skips the next (as `end_credentials`): a failed remember-token write is kept, the listeners
        // still run and the event still goes out, then the first error is returned.
        let remember = cycle_remember_token(self.app(), id).await;
        self.session().queue(Queued::Remove {
            name: super::remember_cookie(self.app()),
        });
        let except = super::principal::session_key(&self.session().binding());
        let listeners = changed(
            self.app(),
            id,
            CredentialChange::Changed,
            Some(except),
            false,
        )
        .await;
        remember.and(listeners).map(|()| true)
    }

    /// Confirm the signed-in user's password (before a sensitive action): `Ok(true)` when it matches, and the
    /// session remembers the time (`password_confirmed_within`, the `password.confirm` middleware). Each call
    /// counts against five a minute per user before the password is checked.
    ///
    /// # Errors
    /// More than five calls a minute (429 for JSON clients, a redirect back with the message on `password` for
    /// forms), no user model, a cache or query failure, or too many password checks waiting (503).
    pub async fn confirm_password(&self, password: &str) -> Result<bool> {
        let Some(user) = self.dyn_user().await? else {
            return Ok(false);
        };
        self.count_password_check(user.id()).await?;
        if !verify_password(password, user.hash()).await? {
            return Ok(false);
        }
        self.session().insert(CONFIRMED_AT_KEY, now_secs());
        Ok(true)
    }

    /// Count one password check of a signed-in user against [`CONFIRM_MAX_ATTEMPTS`] a minute (shared by
    /// [`confirm_password`](Self::confirm_password) and [`logout_other_devices`](Self::logout_other_devices)),
    /// before the hash: over the budget, 429.
    pub(crate) async fn count_password_check(&self, user_id: i64) -> Result<()> {
        let limiter = self.app().service_or_insert_with(|| {
            ConfirmLimiter(RateLimiter::new(
                "auth.confirm-password",
                CONFIRM_MAX_ATTEMPTS,
                Duration::from_secs(60),
            ))
        });
        if let RateLimit::Limited { retry_after, .. } = limiter
            .0
            .hit(self.app(), &format!("user:{user_id}"))
            .await?
        {
            return Err(too_many_confirmations(retry_after));
        }
        Ok(())
    }

    /// Whether this session confirmed the password ([`confirm_password`](Self::confirm_password)) within `within`.
    pub fn password_confirmed_within(&self, within: Duration) -> bool {
        self.check()
            && self
                .session()
                .get::<u64>(CONFIRMED_AT_KEY)
                .is_some_and(|at| now_secs().saturating_sub(at) < within.as_secs())
    }
}

/// The `password.confirm` middleware: a session that confirmed its password within `AUTH_PASSWORD_TIMEOUT` passes;
/// otherwise JSON clients and API routes get 423 `{"message": "Password confirmation required."}`, and a page visit
/// is remembered (as the `auth` middleware does) and sent with a 303 to the route named `password.confirm` (else
/// `/user/confirm-password`).
pub(crate) async fn require_confirmed_password(req: Request, next: Next) -> Response {
    let app = req.extensions().get::<App>().cloned();
    let auth = req.extensions().get::<Auth>().cloned();
    if let (Some(app), Some(auth)) = (&app, &auth)
        && auth.password_confirmed_within(app.settings().password_timeout)
    {
        return next.run(req).await;
    }
    let Some(auth) = auth.filter(|_| !crate::error::wants_json(req.headers())) else {
        return (
            StatusCode::LOCKED,
            axum::Json(serde_json::json!({ "message": "Password confirmation required." })),
        )
            .into_response();
    };
    if super::is_page_visit(&req)
        && let Some(path) = req.uri().path_and_query()
    {
        auth.set_intended(path.as_str());
    }
    let target = app
        .and_then(|app| app.url("password.confirm", &[]).ok())
        .unwrap_or_else(|| "/user/confirm-password".to_owned());
    axum::response::Redirect::to(&target).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_serialize_with_a_type_tag() {
        let event = AuthEvent::RevokedAll {
            user_id: 7,
            kind: CredentialKind::Every,
            except: Some("web:session:ab".into()),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({"type": "revoked_all", "user_id": 7, "kind": "every", "except": "web:session:ab"})
        );
        let back: AuthEvent = serde_json::from_value(serde_json::json!({
            "type": "revoked", "user_id": 7, "key": "hallmark:token:3"
        }))
        .unwrap();
        assert_eq!(
            back,
            AuthEvent::Revoked {
                user_id: 7,
                key: "hallmark:token:3".into()
            }
        );
    }
}
