//! Authentication: password hashing, the [`Auth`] extractor, the `auth`, `guest`
//! and `verified` middleware, remember-me cookies, login throttling, password resets and email
//! verification.
//!
//! The app's user model implements [`Authenticatable`] and is registered with
//! [`AppBuilder::auth`](crate::AppBuilder::auth):
//!
//! ```
//! # extern crate smeltery_core as smeltery;
//! # use smeltery::db::prelude::*;
//! # #[sea_orm::model]
//! # #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
//! # #[sea_orm(table_name = "users")]
//! # pub struct Model {
//! #     #[sea_orm(primary_key)]
//! #     pub id: i64,
//! #     pub password: String,
//! #     pub remember_token: Option<String>,
//! # }
//! # impl ActiveModelBehavior for ActiveModel {}
//! # mod app { pub mod models { pub use crate::Model as User; } }
//! impl smeltery::auth::Authenticatable for Model {
//!     fn auth_id(&self) -> i64 { self.id }
//!     fn password_hash(&self) -> &str { &self.password }
//!     fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
//! }
//!
//! # fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
//! // bootstrap/app.rs
//! app.auth::<crate::app::models::User>()
//! # }
//! # fn main() {}
//! ```

use std::any::Any;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use sea_orm::{ColumnTrait, EntityTrait, IdenStatic, Iterable, QueryFilter, Value};

use crate::app::{App, BoxFuture};
use crate::db::{Db, PrimaryKeyOf, Record};
use crate::error::{Error, Result};
use crate::middleware::{Next, Request};
use crate::session::{AUTH_HASH_KEY, AUTH_KEY, INTENDED_KEY, Queued, Session, TOKEN_KEY};

mod credentials;
pub mod passwords;
mod principal;
pub mod verification;

pub(crate) use credentials::require_confirmed_password;
pub use credentials::{
    AuthEvent, CONFIRM_MAX_ATTEMPTS, CredentialChange, CredentialListener, CredentialsChanged,
    EVENTS_TOPIC, LoginCompletion, LoginDecision, LoginPolicy, SecondFactor, SecondFactorVerdict,
    cycle_remember_token, deserialize_email, end_credentials, find_by_email, model_column,
    password_changed, publish_event, verify_credentials,
};
pub use principal::{
    Authenticated, Credential, CredentialKind, Guard, GuardSet, Principal, WEB_GUARD, authenticate,
    credential_key, unauthenticated_bearer,
};
pub(crate) use principal::{WebGuard, auth_family, valid_guard_name};
pub use verification::{EmailVerificationRequest, MustVerifyEmail, mark_unverified, mark_verified};

/// Hash a password with argon2id (default parameters, random salt) on a blocking thread.
///
/// At most `HASH_CONCURRENCY` hashes and checks run at once in the process; up to
/// `HASH_QUEUE` more wait for a turn, and beyond that the call fails at once with a 503.
///
/// # Errors
/// The hasher fails (no random source), or too many hashes are waiting already (503).
pub async fn hash_password(password: &str) -> Result<String> {
    let password = password.to_owned();
    hash_gate()
        .run(move || {
            let salt = crate::crypto::random_bytes(16)?;
            argon2::Argon2::default()
                .hash_password_with_salt(password.as_bytes(), &salt)
                .map(|hash| hash.to_string())
                .map_err(|e| Error::internal(format!("cannot hash the password: {e}")))
        })
        .await?
}

/// Check a password against a hash from [`hash_password`] (constant time, on a blocking
/// thread). A hash in an unknown format does not match. Shares [`hash_password`]'s limit on
/// concurrent work.
///
/// # Errors
/// The blocking task fails, or too many hashes are waiting already (503).
pub async fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let password = password.to_owned();
    let hash = hash.to_owned();
    hash_gate()
        .run(move || {
            argon2::Argon2::default()
                .verify_password(password.as_bytes(), hash.as_str())
                .is_ok()
        })
        .await
}

/// Bounds the argon2 work of the process: each hash takes about 19 MiB and a core for tens
/// of milliseconds, so unbounded logins (or registrations, or resets) of unknown users could
/// fill the memory and the blocking pool.
pub(crate) struct HashGate {
    permits: Arc<tokio::sync::Semaphore>,
    waiting: AtomicUsize,
    queue: usize,
}

/// Decrements the waiting count when a waiter leaves (with a permit, an error or cancelled).
struct Waiting<'a>(&'a AtomicUsize);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl HashGate {
    pub(crate) fn new(concurrency: usize, queue: usize) -> Self {
        Self {
            permits: Arc::new(tokio::sync::Semaphore::new(concurrency.max(1))),
            waiting: AtomicUsize::new(0),
            queue,
        }
    }

    /// Run `work` on the blocking pool once a permit is free. The permit moves into the
    /// blocking task, so it is held until the work ends even when the request is gone.
    pub(crate) async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T> {
        let permit = match Arc::clone(&self.permits).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let before = self.waiting.fetch_add(1, Ordering::SeqCst);
                let _waiting = Waiting(&self.waiting);
                if before >= self.queue {
                    tracing::warn!("too many password hashes are waiting; answering 503");
                    return Err(Error::http(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Service Unavailable",
                    ));
                }
                Arc::clone(&self.permits)
                    .acquire_owned()
                    .await
                    .map_err(|_| Error::internal("the password hashing limit is closed"))?
            }
        };
        tokio::task::spawn_blocking(move || {
            let out = work();
            drop(permit);
            out
        })
        .await
        .map_err(|_| Error::internal("password hashing panicked"))
    }
}

static HASH_GATE: OnceLock<HashGate> = OnceLock::new();

/// Set the process-wide hashing limit (`HASH_CONCURRENCY`, `HASH_QUEUE`); the first app built
/// in the process sets it.
pub(crate) fn configure_hashing(concurrency: usize, queue: usize) {
    let _ = HASH_GATE.set(HashGate::new(concurrency, queue));
}

fn hash_gate() -> &'static HashGate {
    HASH_GATE.get_or_init(|| {
        HashGate::new(
            crate::config::default_hash_concurrency(),
            crate::config::DEFAULT_HASH_QUEUE,
        )
    })
}

/// An email address as Smeltery compares and stores it: trimmed, ASCII letters in lowercase
/// (` Ada@Example.COM ` → `ada@example.com`). Other characters stay as they are.
///
/// ```
/// use smeltery_core::auth::normalize_email;
///
/// assert_eq!(normalize_email(" Ada@Example.COM "), "ada@example.com");
/// ```
pub fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

/// Whether the stored address `stored` is the typed address `typed`: equal after
/// [`normalize_email`] of the typed one and ASCII lowercase of the stored one. A database whose
/// collation also folds accents or other characters (MySQL's `utf8mb4_0900_ai_ci`) may return
/// a row for a look-alike address; this check refuses it.
pub(crate) fn email_matches(stored: &str, typed: &str) -> bool {
    stored.to_ascii_lowercase() == normalize_email(typed)
}

/// The addresses a lookup asks the database for: the normalized one, and the trimmed one as
/// typed when it differs (rows stored before addresses were normalized).
pub(crate) fn email_candidates(typed: &str) -> Vec<String> {
    let normalized = normalize_email(typed);
    let trimmed = typed.trim().to_owned();
    if trimmed == normalized {
        vec![normalized]
    } else {
        vec![normalized, trimmed]
    }
}

/// From the rows a lookup returned (with their stored addresses), the account `typed` names:
/// only rows that pass [`email_matches`]; the exact normalized address first, then the address
/// exactly as typed, then any other match.
pub(crate) fn pick_account<T>(rows: Vec<(T, String)>, typed: &str) -> Option<(T, String)> {
    let normalized = normalize_email(typed);
    let trimmed = typed.trim();
    let mut rows: Vec<(T, String)> = rows
        .into_iter()
        .filter(|(_, stored)| email_matches(stored, typed))
        .collect();
    let rank = |stored: &str| {
        if stored == normalized {
            0
        } else if stored == trimmed {
            1
        } else {
            2
        }
    };
    rows.sort_by_key(|(_, stored)| rank(stored));
    rows.into_iter().next()
}

/// The client address as throttles count it: an IPv6 client by its /64 (one customer's
/// network, which holds 2^64 addresses), anything else as it is.
pub(crate) fn throttle_ip(ip: &str) -> String {
    match ip.parse::<IpAddr>() {
        Ok(IpAddr::V6(v6)) if v6.to_ipv4_mapped().is_none() => {
            let [a, b, c, d, ..] = v6.segments();
            format!("{a:x}:{b:x}:{c:x}:{d:x}::/64")
        }
        Ok(IpAddr::V6(v6)) => v6
            .to_ipv4_mapped()
            .map_or_else(|| ip.to_owned(), |v4| v4.to_string()),
        _ => ip.to_owned(),
    }
}

/// The client network as the per-address login budget counts it: an IPv4 client by its /24, an
/// IPv6 client by its /48 (one site), anything else as it is. Guesses from one network share a
/// budget; clients on other networks keep theirs, so nobody can lock an account for everyone.
pub(crate) fn throttle_network(ip: &str) -> String {
    match ip.parse::<IpAddr>().map(|ip| ip.to_canonical()) {
        Ok(IpAddr::V4(v4)) => {
            let [a, b, c, _] = v4.octets();
            format!("{a}.{b}.{c}.0/24")
        }
        Ok(IpAddr::V6(v6)) => {
            let [a, b, c, ..] = v6.segments();
            format!("{a:x}:{b:x}:{c:x}::/48")
        }
        Err(_) => ip.to_owned(),
    }
}

/// What a signed-in session is bound to: a hash of the user's password hash and credentials epoch
/// ([`Authenticatable::credentials_epoch`]). A password change or reset, [`Auth::logout_other_devices`] or
/// [`end_credentials`] changes it, and every session made before then is signed out. Without an epoch (`None`) the
/// value is exactly the one before epochs existed, so those sessions stay valid.
pub(crate) fn session_binding(password_hash: &str, epoch: Option<i64>) -> String {
    match epoch {
        None => binding_of("session", password_hash),
        Some(epoch) => binding_of("session", &format!("{password_hash}|epoch:{epoch}")),
    }
}

/// The session binding of `user` now.
pub(crate) fn user_session_binding(user: &dyn DynUser) -> String {
    session_binding(user.hash(), user.epoch())
}

/// SHA-256 (hex) of `smeltery-<purpose>|<password hash>`: what a credential of `purpose` is bound to.
fn binding_of(purpose: &str, password_hash: &str) -> String {
    crate::crypto::sha256_hex(&format!("smeltery-{purpose}|{password_hash}"))
}

/// The app's user (the model registered with [`AppBuilder::auth`](crate::AppBuilder::auth)) with its type erased:
/// what guards and other framework crates hold. [`App::find_user`] loads one. The password hash never leaves it;
/// [`binding`](Self::binding) derives what a credential is bound to.
#[derive(Clone)]
pub struct AuthUser(Arc<dyn DynUser>);

impl std::fmt::Debug for AuthUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthUser")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

impl AuthUser {
    pub(crate) fn new(user: Arc<dyn DynUser>) -> Self {
        Self(user)
    }

    /// The erased handle of a loaded user of the app's model.
    pub fn of<U: Authenticatable>(user: &U) -> Self {
        Self(Arc::new(user.clone()))
    }

    /// The user's id.
    pub fn id(&self) -> i64 {
        self.0.id()
    }

    /// The user as the app's model `U`; `None` when `U` is another type.
    pub fn downcast<U: Authenticatable>(&self) -> Option<U> {
        self.0.as_any().downcast_ref::<U>().cloned()
    }

    /// What a credential of `purpose` is bound to: the SHA-256 (hex) of `smeltery-<purpose>|<password hash>`. It
    /// changes whenever the password hash changes, so a credential that stores it at issue and compares it on use
    /// (in constant time) stops working after any password change. Signed-in sessions use the purpose `session`;
    /// use a purpose of your own for anything else (framework crates: dotted, `hallmark.token`).
    ///
    /// # Errors
    /// `purpose` is empty or holds anything but lowercase ASCII letters, digits, `.`, `_` and `-` (a `|` would make
    /// the hashed text ambiguous).
    pub fn binding(&self, purpose: &str) -> Result<String> {
        let plain = !purpose.is_empty()
            && purpose
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
        if !plain {
            return Err(Error::internal(format!(
                "the binding purpose `{purpose}` is invalid: use lowercase ASCII letters, digits, `.`, `_` and `-`"
            )));
        }
        Ok(binding_of(purpose, self.0.hash()))
    }

    /// [`binding`](Self::binding) that also changes with the user's credentials epoch
    /// ([`Authenticatable::credentials_epoch`]): a credential that stores it at issue and compares it on use stops
    /// working after any password change and after [`end_credentials`], like a signed-in session. Without an epoch
    /// (`None`) it equals [`binding`](Self::binding).
    ///
    /// # Errors
    /// As [`binding`](Self::binding).
    pub fn credential_binding(&self, purpose: &str) -> Result<String> {
        let plain = self.binding(purpose)?;
        Ok(match self.0.epoch() {
            None => plain,
            Some(epoch) => binding_of(purpose, &format!("{}|epoch:{epoch}", self.0.hash())),
        })
    }

    pub(crate) fn dyn_user(&self) -> &Arc<dyn DynUser> {
        &self.0
    }
}

impl App {
    /// The user with id `id`, through the model registered with [`AppBuilder::auth`](crate::AppBuilder::auth);
    /// `None` when there is no such row.
    ///
    /// # Errors
    /// No user model is registered, there is no database, or the query fails.
    pub async fn find_user(&self, id: i64) -> Result<Option<AuthUser>> {
        let provider = self.user_provider().ok_or_else(no_user_model)?;
        Ok(provider
            .find_by_id(&self.db()?, id)
            .await?
            .map(AuthUser::new))
    }
}

fn no_user_model() -> Error {
    Error::internal("no user model is registered: call `.auth::<User>()` in bootstrap/app.rs")
}

/// The app's user model. `AppBuilder::auth::<User>()` registers it.
pub trait Authenticatable: Clone + Send + Sync + 'static {
    /// The primary key.
    fn auth_id(&self) -> i64;
    /// The argon2 hash of the password (the `password` column).
    fn password_hash(&self) -> &str;
    /// The stored remember-me token hash (the `remember_token` column).
    fn remember_token(&self) -> Option<&str>;

    /// The credentials epoch (a nullable integer column `credentials_epoch`): part of what a signed-in session is
    /// bound to. [`end_credentials`] adds one to it, which signs out every open session of the user on its next
    /// request; sign-ins never change it. `None` (the default, and a `NULL` column) binds sessions exactly as
    /// without the column, so a model without it keeps working and its sessions stay valid; [`end_credentials`]
    /// then fails after doing the rest, because it cannot end open sessions. A model with the column returns
    /// `self.credentials_epoch`.
    fn credentials_epoch(&self) -> Option<i64> {
        None
    }
}

/// [`Authenticatable`] without its type, for the user provider.
pub(crate) trait DynUser: Send + Sync {
    fn id(&self) -> i64;
    fn hash(&self) -> &str;
    fn remember(&self) -> Option<&str>;
    fn epoch(&self) -> Option<i64>;
    fn as_any(&self) -> &dyn Any;
}

impl<U: Authenticatable> DynUser for U {
    fn id(&self) -> i64 {
        self.auth_id()
    }
    fn hash(&self) -> &str {
        self.password_hash()
    }
    fn remember(&self) -> Option<&str> {
        self.remember_token()
    }
    fn epoch(&self) -> Option<i64> {
        self.credentials_epoch()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub(crate) type MaybeUser = Option<Arc<dyn DynUser>>;

/// A user with the address stored in their `email` column.
pub(crate) type UserWithEmail = Option<(Arc<dyn DynUser>, String)>;

/// Finds users and stores remember tokens for one model type.
pub(crate) trait UserProvider: Send + Sync {
    fn find_by_id<'a>(&'a self, db: &'a Db, id: i64) -> BoxFuture<'a, Result<MaybeUser>>;
    /// The user whose stored address is `email` (see [`pick_account`]), with that address.
    fn find_by_email<'a>(
        &'a self,
        db: &'a Db,
        email: &'a str,
    ) -> BoxFuture<'a, Result<UserWithEmail>>;
    /// Set the text column `name` (`remember_token`, `password`) of user `id`.
    fn set_column<'a>(
        &'a self,
        db: &'a Db,
        id: i64,
        name: &'static str,
        value: Option<String>,
    ) -> BoxFuture<'a, Result<()>>;
    fn set_remember_token<'a>(
        &'a self,
        db: &'a Db,
        id: i64,
        token: Option<String>,
    ) -> BoxFuture<'a, Result<()>> {
        self.set_column(db, id, "remember_token", token)
    }
    /// Add one to user `id`'s `credentials_epoch` (`NULL` counts as 0) in one `UPDATE`; whether a row changed.
    fn bump_epoch<'a>(&'a self, db: &'a Db, id: i64) -> BoxFuture<'a, Result<bool>>;
}

pub(crate) struct ModelProvider<U>(PhantomData<fn() -> U>);

impl<U> ModelProvider<U> {
    pub(crate) fn new() -> Self {
        Self(PhantomData)
    }
}

pub(crate) fn column<U: Record>(name: &str) -> Result<<U::Entity as EntityTrait>::Column> {
    <<U::Entity as EntityTrait>::Column as Iterable>::iter()
        .find(|c| IdenStatic::as_str(c) == name)
        .ok_or_else(|| {
            Error::internal(format!(
                "the user model `{}` has no `{name}` column",
                std::any::type_name::<U>()
            ))
        })
}

impl<U> UserProvider for ModelProvider<U>
where
    U: Record
        + Authenticatable
        + sea_orm::FromQueryResult
        + sea_orm::ModelTrait<Entity = <U as Record>::Entity>,
    PrimaryKeyOf<U>: From<i64>,
{
    fn find_by_id<'a>(&'a self, db: &'a Db, id: i64) -> BoxFuture<'a, Result<MaybeUser>> {
        Box::pin(async move {
            Ok(U::find(db, PrimaryKeyOf::<U>::from(id))
                .await?
                .map(|u| Arc::new(u) as Arc<dyn DynUser>))
        })
    }

    fn find_by_email<'a>(
        &'a self,
        db: &'a Db,
        email: &'a str,
    ) -> BoxFuture<'a, Result<UserWithEmail>> {
        Box::pin(async move {
            let col = column::<U>("email")?;
            let users = U::query()
                .filter(col.is_in(email_candidates(email)))
                .all(db.conn())
                .await?;
            let rows = users
                .into_iter()
                .filter_map(|u| {
                    let stored = match sea_orm::ModelTrait::get(&u, col) {
                        Value::String(Some(s)) => s,
                        _ => return None,
                    };
                    Some((u, stored))
                })
                .collect();
            Ok(pick_account(rows, email).map(|(u, s)| (Arc::new(u) as Arc<dyn DynUser>, s)))
        })
    }

    fn set_column<'a>(
        &'a self,
        db: &'a Db,
        id: i64,
        name: &'static str,
        token: Option<String>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let col = column::<U>(name)?;
            let Some(user) = U::find(db, PrimaryKeyOf::<U>::from(id)).await? else {
                return Ok(());
            };
            let mut set = Ok(());
            user.update(db, |m| {
                set = sea_orm::ActiveModelTrait::try_set(m, col, Value::from(token));
            })
            .await?;
            set.map_err(Error::from)
        })
    }

    fn bump_epoch<'a>(&'a self, db: &'a Db, id: i64) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            use sea_orm::PrimaryKeyToColumn as _;
            use sea_orm::sea_query::{Expr, ExprTrait};
            let epoch = column::<U>("credentials_epoch")?;
            let key = <<<U as Record>::Entity as EntityTrait>::PrimaryKey as Iterable>::iter()
                .next()
                .ok_or_else(|| Error::internal("the user model has no primary key"))?
                .into_column();
            // Atomic: parallel calls each add one (no read-modify-write).
            let result = <<U as Record>::Entity as EntityTrait>::update_many()
                .col_expr(epoch, Expr::col(epoch).if_null(0).add(1))
                .filter(key.eq(id))
                .exec(db.conn())
                .await?;
            Ok(result.rows_affected > 0)
        })
    }
}

/// The too-many-failed-logins error: "Too many login attempts. Please try again in N
/// seconds." As an [`Error`] it answers 429 to JSON clients and redirects back with the
/// message on `email` on web routes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooManyAttempts {
    /// Seconds until the next attempt is allowed.
    pub retry_after: u64,
}

impl std::fmt::Display for TooManyAttempts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Too many login attempts. Please try again in {} seconds.",
            self.retry_after
        )
    }
}

impl std::error::Error for TooManyAttempts {}

impl From<TooManyAttempts> for Error {
    fn from(t: TooManyAttempts) -> Self {
        Self::Validation(Box::new(crate::validation::Invalid::too_many(
            "email",
            t.to_string(),
            t.retry_after,
        )))
    }
}

/// At most `max` hits per key in a fixed window: login attempts per (email, client) and per
/// account, verification mails per user, reset links per address, requests per route for the
/// `throttle:` middleware. In process memory.
///
/// Bounded without letting a flood unblock a key: when it reaches its cap, expired entries
/// go, then the oldest keys that are not blocked; blocked keys stay until their window ends
/// (the map may then grow past the cap, and it waits until it has doubled before sweeping
/// again, so a flood costs amortized constant time per hit).
pub(crate) struct Throttle {
    max: u32,
    window: Duration,
    hits: Mutex<Hits>,
}

struct Hits {
    entries: HashMap<String, (u32, Instant)>,
    /// The size at which the next sweep runs.
    sweep_at: usize,
}

/// Login attempts per (email, client) a minute.
pub(crate) const MAX_ATTEMPTS: u32 = 5;
/// Login attempts per (email, client network: IPv4 /24, IPv6 /48) in [`ACCOUNT_WINDOW`],
/// counted the same for addresses with and without an account.
pub(crate) const ACCOUNT_MAX_ATTEMPTS: u32 = 20;
/// Login attempts per client (an IPv6 client by its /64) a minute, whatever the address;
/// successful sign-ins are given back.
pub(crate) const CLIENT_MAX_ATTEMPTS: u32 = 30;
/// The window of [`ACCOUNT_MAX_ATTEMPTS`].
pub(crate) const ACCOUNT_WINDOW: Duration = Duration::from_secs(5 * 60);
const WINDOW: Duration = Duration::from_secs(60);
pub(crate) const THROTTLE_CAP: usize = 10_000;

impl Throttle {
    /// `max` hits a minute.
    pub(crate) fn new(max: u32) -> Self {
        Self::with_window(max, WINDOW)
    }

    /// `max` hits per `window`.
    pub(crate) fn with_window(max: u32, window: Duration) -> Self {
        Self {
            max,
            window,
            hits: Mutex::new(Hits {
                entries: HashMap::new(),
                sweep_at: THROTTLE_CAP,
            }),
        }
    }

    fn blocked(&self, entry: Option<&(u32, Instant)>) -> std::result::Result<(), TooManyAttempts> {
        if let Some((count, start)) = entry {
            let elapsed = start.elapsed();
            if *count >= self.max && elapsed < self.window {
                let left = self.window.saturating_sub(elapsed).as_secs().max(1);
                return Err(TooManyAttempts { retry_after: left });
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn check(&self, key: &str) -> std::result::Result<(), TooManyAttempts> {
        let hits = self.hits.lock().unwrap_or_else(PoisonError::into_inner);
        self.blocked(hits.entries.get(key))
    }

    /// The hits left for `key` in its current window, without counting one; blocked when none are left.
    pub(crate) fn peek(&self, key: &str) -> std::result::Result<u32, TooManyAttempts> {
        let hits = self.hits.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = hits.entries.get(key);
        self.blocked(entry)?;
        let used = entry
            .filter(|(_, start)| start.elapsed() < self.window)
            .map_or(0, |(count, _)| *count);
        Ok(self.max.saturating_sub(used))
    }

    /// Count a hit for `key` unless it is over its limit, checked and counted under one lock,
    /// so concurrent callers can never pass more than `max` times a window. On success, the
    /// hits left in this window.
    pub(crate) fn try_hit(&self, key: &str) -> std::result::Result<u32, TooManyAttempts> {
        let mut hits = self.hits.lock().unwrap_or_else(PoisonError::into_inner);
        self.blocked(hits.entries.get(key))?;
        Ok(self.max.saturating_sub(self.count(&mut hits, key)))
    }

    #[cfg(test)]
    pub(crate) fn hit(&self, key: &str) {
        let mut hits = self.hits.lock().unwrap_or_else(PoisonError::into_inner);
        self.count(&mut hits, key);
    }

    /// Count a hit; the count in the current window.
    fn count(&self, hits: &mut Hits, key: &str) -> u32 {
        if !hits.entries.contains_key(key) && hits.entries.len() >= hits.sweep_at {
            self.sweep(hits);
        }
        let entry = hits
            .entries
            .entry(key.to_owned())
            .or_insert((0, Instant::now()));
        if entry.1.elapsed() >= self.window {
            *entry = (0, Instant::now());
        }
        entry.0 = entry.0.saturating_add(1);
        entry.0
    }

    /// Drop expired entries, then the oldest keys that are not blocked, down to three
    /// quarters of the cap (so the next sweep is a quarter of the cap away).
    fn sweep(&self, hits: &mut Hits) {
        let (max, window) = (self.max, self.window);
        hits.entries
            .retain(|_, (_, start)| start.elapsed() < window);
        let target = THROTTLE_CAP / 4 * 3;
        if hits.entries.len() > target {
            let mut open: Vec<(Instant, String)> = hits
                .entries
                .iter()
                .filter(|(_, (count, _))| *count < max)
                .map(|(key, (_, start))| (*start, key.clone()))
                .collect();
            open.sort_unstable();
            let excess = hits.entries.len() - target;
            for (_, key) in open.into_iter().take(excess) {
                hits.entries.remove(&key);
            }
        }
        // Only blocked keys left: grow, and sweep again when the map has doubled.
        hits.sweep_at = if hits.entries.len() < THROTTLE_CAP {
            THROTTLE_CAP
        } else {
            hits.entries.len().saturating_mul(2)
        };
    }

    /// Take back one hit of `key` in its current window (a successful sign-in).
    pub(crate) fn refund(&self, key: &str) {
        let mut hits = self.hits.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((count, start)) = hits.entries.get_mut(key)
            && start.elapsed() < self.window
        {
            *count = count.saturating_sub(1);
        }
    }

    pub(crate) fn clear(&self, key: &str) {
        self.hits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .remove(key);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.hits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .len()
    }
}

/// A hash `verify_password` can run against when the user does not exist, so a wrong email
/// takes as long as a wrong password.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c21lbHRlcnlzYWx0eHg$4xR3lTDgOT6q2hPJtGnVxZ4bk7iTsTmIhXQKqHLPtmM";

/// The remember-me cookie lives five years.
const REMEMBER_LIFETIME: Duration = Duration::from_secs(5 * 365 * 24 * 60 * 60);

/// The signed-in user of this request, as a handler argument (`auth: Auth`). Web routes only.
#[derive(Clone)]
pub struct Auth {
    app: App,
    session: Session,
    ip: String,
    cache: Arc<Mutex<Option<MaybeUser>>>,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

impl Auth {
    pub(crate) fn new(app: App, session: Session, ip: String) -> Self {
        Self {
            app,
            session,
            ip,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    /// The client address of the request after `TRUSTED_PROXIES` (see
    /// [`ClientInfo`](crate::http::ClientInfo)); `unknown` when the server did not pass one.
    pub fn ip(&self) -> &str {
        &self.ip
    }

    fn provider(&self) -> Result<Arc<dyn UserProvider>> {
        self.app.user_provider().ok_or_else(|| {
            Error::internal(
                "no user model is registered: call `.auth::<User>()` in bootstrap/app.rs",
            )
        })
    }

    /// Whether someone is signed in.
    pub fn check(&self) -> bool {
        self.id().is_some()
    }

    /// The signed-in user's id.
    pub fn id(&self) -> Option<i64> {
        self.session.auth_id()
    }

    /// The signed-in user (loaded once per request).
    ///
    /// # Errors
    /// The query fails, no user model is registered, or `U` is not the registered model.
    pub async fn user<U: Authenticatable>(&self) -> Result<Option<U>> {
        match self.dyn_user().await? {
            None => Ok(None),
            Some(user) => user
                .as_any()
                .downcast_ref::<U>()
                .cloned()
                .map(Some)
                .ok_or_else(|| {
                    Error::internal(format!(
                        "`{}` is not the user model registered with `.auth::<…>()`",
                        std::any::type_name::<U>()
                    ))
                }),
        }
    }

    /// The signed-in user without its type (loaded once per request).
    pub(crate) async fn dyn_user(&self) -> Result<MaybeUser> {
        let Some(id) = self.id() else {
            return Ok(None);
        };
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match cached {
            Some(user) => Ok(user),
            None => {
                let user = self.provider()?.find_by_id(&self.app.db()?, id).await?;
                *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = Some(user.clone());
                Ok(user)
            }
        }
    }

    /// The signed-in user without its type (loaded once per request, shared with [`Auth::user`]).
    ///
    /// # Errors
    /// The query fails or no user model is registered.
    pub async fn auth_user(&self) -> Result<Option<AuthUser>> {
        Ok(self.dyn_user().await?.map(AuthUser::new))
    }

    /// The sign-in of `session`, a session read with [`session::peek`](crate::session::peek) (no save, no cookie,
    /// no remember-me sign-in): for routes outside the web stack that must know who the visitor is. A signed-in
    /// session whose user changed their password, ended their credentials or no longer exists reads as signed out
    /// (the web stack signs it out on its next request). `ip` is the client address
    /// ([`ClientInfo`](crate::http::ClientInfo)).
    ///
    /// Use it to read (`id`, `user`, `check`); signing in or out through it changes nothing that is stored.
    ///
    /// ```
    /// use smeltery_core::App;
    /// use smeltery_core::auth::Auth;
    /// use smeltery_core::session;
    ///
    /// async fn who(app: App, headers: http::HeaderMap) -> smeltery_core::Result<Option<i64>> {
    ///     let Some(session) = session::peek(&app, &headers).await? else { return Ok(None) };
    ///     Ok(Auth::peek(&app, session, "unknown").await?.id())
    /// }
    /// # let _ = who;
    /// ```
    ///
    /// # Errors
    /// The user query failed.
    pub async fn peek(app: &App, session: Session, ip: impl Into<String>) -> Result<Self> {
        let auth = Self::new(app.clone(), session.clone(), ip.into());
        if !session_is_current(&auth).await? {
            // Only this in-memory copy: a peeked session is never stored.
            session.remove(AUTH_KEY);
            auth.forget_user();
        }
        Ok(auth)
    }

    /// Forget the loaded user, so the next call reads its row again (after it changed).
    pub(crate) fn forget_user(&self) {
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub(crate) fn app(&self) -> &App {
        &self.app
    }

    pub(crate) fn session(&self) -> &Session {
        &self.session
    }

    /// Sign in the user with this email and password. `Ok(false)` for wrong credentials.
    ///
    /// It never asks a registered [`SecondFactor`]: a flow with a second factor uses
    /// [`validate`](Self::validate) and [`login`](Self::login).
    ///
    /// The email is compared after [`normalize_email`] (trimmed, ASCII lowercase). Each
    /// attempt counts before the password is checked, against three budgets: thirty a minute
    /// from one client whatever the address, five a minute for one address from one client
    /// (an IPv6 client by its /64), and twenty per five minutes for one address from one
    /// network (an IPv4 /24, an IPv6 /48). They count addresses with and without an account
    /// alike, so a refusal tells nothing about which addresses have one, and guesses from other
    /// networks never lock out the account's owner. A successful sign-in resets the address's
    /// budgets and gives its hit back to the client's. A matching user then meets the app's
    /// [`LoginPolicy`]s ([`App::check_login`](crate::App::check_login)) before signing in.
    ///
    /// # Errors
    /// Too many attempts ([`TooManyAttempts`] as an [`Error`]), a login policy refuses the
    /// user, too many password checks waiting (503), or a database failure.
    pub async fn attempt(&self, email: &str, password: &str, remember: bool) -> Result<bool> {
        self.refuse_with_login_completion("attempt")?;
        // `validate` counts the budgets and checks the password; only a match signs in.
        match self.validate(email, password).await? {
            Some(user) => {
                self.app.check_login(&user).await?;
                self.login_dyn(Arc::clone(user.dyn_user()), remember)
                    .await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Sign in `user`: a new session id, a new CSRF token, the user's id in the session, and
    /// with `remember` a remember-me cookie.
    ///
    /// The session is bound to the user's password hash: when it changes (a password change or
    /// reset, [`logout_other_devices`](Self::logout_other_devices)), the session is signed out on
    /// its next request.
    ///
    /// When the app registers a [`LoginCompletion`] (a second factor, a sign-in
    /// pipeline), `login` and [`attempt`](Self::attempt) refuse with an error, so a controller cannot skip its
    /// steps: check the password with [`validate`](Self::validate) and hand the user to
    /// [`App::login_completion`](crate::App::login_completion); the completion itself signs in with
    /// [`login_user`](Self::login_user).
    ///
    /// # Errors
    /// A [`LoginCompletion`] is registered, or storing the remember token fails.
    pub async fn login<U: Authenticatable>(&self, user: &U, remember: bool) -> Result<()> {
        self.refuse_with_login_completion("login")?;
        self.login_dyn(Arc::new(user.clone()), remember).await
    }

    /// Fails when a [`LoginCompletion`] is registered: `method` would skip it.
    fn refuse_with_login_completion(&self, method: &str) -> Result<()> {
        if self.app.login_completion().is_some() {
            return Err(Error::internal(format!(
                "`Auth::{method}` would skip the app's sign-in steps (a LoginCompletion is registered): check the \
                 password with `auth.validate(…)` and call `app.login_completion()`; the completion signs in with \
                 `auth.login_user(…)`"
            )));
        }
        Ok(())
    }

    async fn login_dyn(&self, user: Arc<dyn DynUser>, remember: bool) -> Result<()> {
        start_signed_in_session(&self.session, user.id());
        if remember {
            let token = crate::crypto::random_token(60)?;
            let hashed = crate::crypto::sha256_hex(&token);
            self.provider()?
                .set_remember_token(&self.app.db()?, user.id(), Some(hashed))
                .await?;
            self.session.queue(Queued::Set {
                name: remember_cookie(&self.app),
                value: format!("{}|{token}", user.id()),
                max_age: REMEMBER_LIFETIME,
            });
        }
        self.session
            .insert(AUTH_HASH_KEY, user_session_binding(user.as_ref()));
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = Some(Some(user));
        Ok(())
    }

    /// Sign out this session: it is invalidated (a new id, a new CSRF token, no data; the
    /// database and file drivers delete the stored one), the remember-me cookie is removed and
    /// the user's remember token is replaced with a new random one, so no
    /// remember-me cookie of the user signs in any more. The user's sessions on other devices
    /// stay signed in (see [`logout_other_devices`](Self::logout_other_devices)). With
    /// `SESSION_DRIVER=cookie` a copy of this session's old cookie stays valid until it expires,
    /// since nothing is stored on the server.
    ///
    /// # Errors
    /// Storing the new remember token fails (the session is signed out all the same).
    pub async fn logout(&self) -> Result<()> {
        let signed_in = self
            .id()
            .map(|id| (id, principal::session_key(&self.session.binding())));
        // The session ends first, so a failing remember-token write can never leave it signed in.
        // Every key of this sign-in (`_auth.*`, `_temper.*`) goes with the session's data.
        self.session.invalidate();
        self.session.queue(Queued::Remove {
            name: remember_cookie(&self.app),
        });
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = Some(None);
        let Some((id, key)) = signed_in else {
            return Ok(());
        };
        credentials::session_ended(&self.app, id, key).await;
        if let Some(provider) = self.app.user_provider()
            && let Ok(db) = self.app.db()
        {
            // Only its hash is stored, and the token itself is thrown away: nothing can
            // present it.
            let cycled = crate::crypto::sha256_hex(&crate::crypto::random_token(60)?);
            provider.set_remember_token(&db, id, Some(cycled)).await?;
        }
        Ok(())
    }

    /// Sign out the user's sessions on every other device:
    /// `password` must be the user's current password; it is hashed again with a fresh salt and
    /// stored, so the hash the other sessions are bound to no longer matches, and this session is
    /// bound to the new one. The remember token is replaced too, so no remember-me cookie (this
    /// device's included, which is removed) signs a device in again. The
    /// [`CredentialListener`]s run ([`CredentialChange::OtherDevicesLoggedOut`], except this
    /// session) and [`AuthEvent::RevokedAll`] is published. `Ok(false)` (and nothing changes)
    /// when nobody is signed in or the password is wrong. Each call counts against the password
    /// confirmation budget (five a minute per user, see
    /// [`confirm_password`](Self::confirm_password)) before the password is checked.
    ///
    /// ```
    /// use smeltery_core::Result;
    /// use smeltery_core::auth::Auth;
    ///
    /// async fn sign_out_elsewhere(auth: Auth, password: String) -> Result<&'static str> {
    ///     Ok(if auth.logout_other_devices(&password).await? { "done" } else { "wrong password" })
    /// }
    /// ```
    ///
    /// # Errors
    /// No user model, a query fails, or hashing fails (503 when too many hashes wait).
    pub async fn logout_other_devices(&self, password: &str) -> Result<bool> {
        let Some(user) = self.dyn_user().await? else {
            return Ok(false);
        };
        // A stolen session must not make this an unlimited password oracle.
        self.count_password_check(user.id()).await?;
        if !verify_password(password, user.hash()).await? {
            return Ok(false);
        }
        let hash = hash_password(password).await?;
        self.provider()?
            .set_column(&self.app.db()?, user.id(), "password", Some(hash.clone()))
            .await?;
        self.session
            .insert(AUTH_HASH_KEY, session_binding(&hash, user.epoch()));
        self.forget_user();
        // No step skips the next (as `end_credentials`): a failed remember-token write is kept, the listeners
        // still run and the event still goes out, then the first error is returned.
        let remember = credentials::cycle_remember_token(&self.app, user.id()).await;
        self.session.queue(Queued::Remove {
            name: remember_cookie(&self.app),
        });
        let except = principal::session_key(&self.session.binding());
        let listeners = credentials::changed(
            &self.app,
            user.id(),
            CredentialChange::OtherDevicesLoggedOut,
            Some(except),
            false,
        )
        .await;
        remember.and(listeners).map(|()| true)
    }

    /// A 303 redirect to the page the `auth` middleware turned this visitor away from before
    /// they signed in, else to `default`; the remembered page is forgotten. The login handler
    /// answers with it after a successful [`attempt`](Self::attempt).
    ///
    /// The remembered page is always a path on this site (it starts with one `/`, not `//`, and
    /// holds only visible ASCII characters other than `\`); anything else in the session is
    /// ignored and `default` is used.
    ///
    /// ```
    /// use smeltery_core::App;
    /// use smeltery_core::auth::Auth;
    /// use smeltery_core::http::Redirect;
    ///
    /// async fn signed_in(app: App, auth: Auth) -> Redirect {
    ///     auth.intended(&app.settings().auth_home)
    /// }
    /// ```
    pub fn intended(&self, default: &str) -> axum::response::Redirect {
        let remembered = self.session.get::<String>(INTENDED_KEY);
        self.session.remove(INTENDED_KEY);
        let target = remembered
            .filter(|url| crate::http::is_local_path(url))
            .unwrap_or_else(|| default.to_owned());
        axum::response::Redirect::to(&target)
    }

    /// Remember `path` as the page [`intended`](Self::intended) sends the visitor to, as the
    /// `auth` middleware does for a guest. `false` (and nothing is remembered) unless `path` is
    /// a path on this site of at most 2048 bytes (see [`intended`](Self::intended)).
    pub fn set_intended(&self, path: &str) -> bool {
        let ok = path.len() <= MAX_INTENDED_LEN && crate::http::is_local_path(path);
        if ok {
            self.session.insert(INTENDED_KEY, path);
        }
        ok
    }
}

/// The longest remembered page (path and query): the session may live in a 4 KB cookie.
const MAX_INTENDED_LEN: usize = 2048;

/// Whether a guest's request is a page visit worth returning to after the login: a `GET`
/// that is not from a JSON client, not a background `fetch` / XHR, not a subresource (an
/// image, script, stylesheet or frame), not a prefetch and not an event stream. Inertia's
/// visits count as page visits.
///
/// Fetch Metadata decides where browsers send it: a page visit has `Sec-Fetch-Mode: navigate`
/// and `Sec-Fetch-Dest: document`; an Inertia visit is a `cors` fetch with `Sec-Fetch-Dest:
/// empty`. Clients without these headers (older browsers, tests) are judged by the rest.
fn is_page_visit(req: &Request) -> bool {
    let headers = req.headers();
    let has = |name: &str, needle: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| v.to_ascii_lowercase().contains(needle))
    };
    // A header that is absent, or whose every value is one of `allowed`.
    let only = |name: &str, allowed: &[&str]| {
        headers.get_all(name).iter().all(|v| {
            v.to_str()
                .is_ok_and(|v| allowed.iter().any(|a| v.trim().eq_ignore_ascii_case(a)))
        })
    };
    let inertia = crate::session::web::is_inertia(headers);
    let fetch_metadata = if inertia {
        only("sec-fetch-dest", &["empty"])
    } else {
        only("sec-fetch-mode", &["navigate"]) && only("sec-fetch-dest", &["document"])
    };
    req.method() == http::Method::GET
        && fetch_metadata
        && !crate::error::wants_json(headers)
        && !(has("x-requested-with", "xmlhttprequest") && !inertia)
        && !has("purpose", "prefetch")
        && !has("sec-purpose", "prefetch")
        && !has("accept", "text/event-stream")
}

/// `remember_<session cookie>`, with the `__Host-` prefix when cookies are `Secure`.
pub(crate) fn remember_cookie(app: &App) -> String {
    let settings = app.settings();
    let name = format!("remember_{}", settings.session_cookie);
    if settings.secure_cookies() {
        format!("__Host-{name}")
    } else {
        name
    }
}

/// Sign in from a valid remember-me cookie value (`id|token`); `false` when it does not
/// match (the caller then removes the cookie).
pub(crate) async fn login_from_remember(app: &App, session: &Session, value: &str) -> Result<bool> {
    let Some(provider) = app.user_provider() else {
        return Ok(false);
    };
    let Some((id, token)) = value.split_once('|') else {
        return Ok(false);
    };
    let Ok(id) = id.parse::<i64>() else {
        return Ok(false);
    };
    let Some(user) = provider.find_by_id(&app.db()?, id).await? else {
        return Ok(false);
    };
    let matches = user
        .remember()
        .is_some_and(|stored| crate::crypto::same(stored, &crate::crypto::sha256_hex(token)));
    if !matches {
        return Ok(false);
    }
    // A restore is a new sign-in: the login policies decide (a suspended account). A refusal stays a guest, the
    // cookie goes (the caller removes it on `false`) and the token is replaced, so no device's cookie comes back.
    // The request never asked to sign in, so the refusal is logged, not answered.
    let hash = user_session_binding(user.as_ref());
    if let LoginDecision::Refuse { status, .. } = app
        .login_decision(&AuthUser::new(Arc::clone(&user)))
        .await?
    {
        tracing::info!(
            user_id = id,
            status = status.as_u16(),
            "a login policy refused a remember-me sign-in"
        );
        credentials::cycle_remember_token(app, id).await?;
        return Ok(false);
    }
    start_signed_in_session(session, id);
    session.insert(AUTH_HASH_KEY, hash);
    Ok(true)
}

/// A sign-in: a new session id (against session fixation), a new CSRF token (a token a
/// visitor had before signing in may be known to someone else), a new absolute-lifetime
/// clock, the user's id, and none of the keys a sign-in owns (`_auth.*`, `_temper.*`: a password
/// confirmation, a pending second step) from before.
fn start_signed_in_session(session: &Session, id: i64) {
    session.regenerate();
    session.remove(TOKEN_KEY);
    session.remove_prefixed(credentials::RESERVED_PREFIXES);
    session.restart_clock();
    session.insert(AUTH_KEY, id);
}

/// Whether the signed-in session of `auth` still belongs to its user: the user exists and the
/// session's binding (see [`session_binding`]) matches their password hash. A guest session, or
/// an app without a user model, always does.
pub(crate) async fn session_is_current(auth: &Auth) -> Result<bool> {
    if auth.id().is_none() || auth.app.user_provider().is_none() {
        return Ok(true);
    }
    let Some(user) = auth.dyn_user().await? else {
        return Ok(false);
    };
    let expected = user_session_binding(user.as_ref());
    Ok(auth
        .session
        .get::<String>(AUTH_HASH_KEY)
        .is_some_and(|bound| crate::crypto::same(&bound, &expected)))
}

impl axum::extract::FromRequestParts<App> for Auth {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().cloned().ok_or_else(|| {
            Error::internal("`Auth` is available on web routes only (routes/web.rs)")
        })
    }
}

fn signed_in(req: &Request) -> bool {
    req.extensions().get::<Auth>().is_some_and(Auth::check)
}

/// The `auth` middleware: guests get a 303 to the `login` route (or `/login`), JSON clients
/// a 401. A guest's page visit is remembered for [`Auth::intended`].
pub(crate) async fn require_auth(req: Request, next: Next) -> Response {
    if signed_in(&req) {
        return next.run(req).await;
    }
    if is_page_visit(&req)
        && let Some(auth) = req.extensions().get::<Auth>()
        && let Some(path) = req.uri().path_and_query()
    {
        // The request's own path and query, never its Host: the target stays on this site.
        // A path the check refuses (`//x` through a catch-all route) or a long one is not
        // remembered, and an older remembered page is dropped.
        if !auth.set_intended(path.as_str()) {
            auth.session.remove(INTENDED_KEY);
        }
    }
    if crate::error::wants_json(req.headers()) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({ "error": "Unauthenticated." })),
        )
            .into_response();
    }
    let login = req
        .extensions()
        .get::<App>()
        .and_then(|app| app.url("login", &[]).ok())
        .unwrap_or_else(|| "/login".to_owned());
    axum::response::Redirect::to(&login).into_response()
}

/// The `guest` middleware: signed-in users get a 303 to `AUTH_HOME`.
pub(crate) async fn require_guest(req: Request, next: Next) -> Response {
    if !signed_in(&req) {
        return next.run(req).await;
    }
    let home = req.extensions().get::<App>().map_or_else(
        || "/dashboard".to_owned(),
        |app| app.settings().auth_home.clone(),
    );
    axum::response::Redirect::to(&home).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn argon2id_hash_and_verify() {
        let hash = hash_password("secret pass").await.unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("secret pass", &hash).await.unwrap());
        assert!(!verify_password("wrong", &hash).await.unwrap());
        assert!(!verify_password("x", "not a hash").await.unwrap());
        assert_ne!(hash, hash_password("secret pass").await.unwrap(), "salted");
        // The dummy hash parses, so unknown emails cost a full verification.
        assert!(!verify_password("x", DUMMY_HASH).await.unwrap());
        assert!(argon2::password_hash::phc::PasswordHash::new(DUMMY_HASH).is_ok());
    }

    #[test]
    fn throttle_blocks_after_five_and_stays_bounded() {
        let t = Throttle::new(MAX_ATTEMPTS);
        for _ in 0..5 {
            assert!(t.check("a|ip").is_ok());
            t.hit("a|ip");
        }
        let blocked = t.check("a|ip").unwrap_err();
        assert!((1..=60).contains(&blocked.retry_after));
        assert_eq!(
            blocked.to_string(),
            format!(
                "Too many login attempts. Please try again in {} seconds.",
                blocked.retry_after
            )
        );
        assert!(t.check("b|ip").is_ok());
        t.clear("a|ip");
        assert!(t.check("a|ip").is_ok());
        for i in 0..THROTTLE_CAP + 10 {
            t.hit(&format!("k{i}"));
        }
        assert!(t.len() <= THROTTLE_CAP);
        let err: Error = blocked.into();
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn try_hit_counts_atomically_under_a_parallel_burst() {
        let t = Arc::new(Throttle::new(6));
        let start = Arc::new(std::sync::Barrier::new(32));
        let passed: usize = (0..32)
            .map(|_| {
                let (t, start) = (Arc::clone(&t), Arc::clone(&start));
                std::thread::spawn(move || {
                    start.wait();
                    t.try_hit("7").is_ok()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum();
        assert_eq!(passed, 6);
        let blocked = t.try_hit("7").unwrap_err();
        assert!((1..=60).contains(&blocked.retry_after));
        assert!(t.try_hit("8").is_ok(), "another key has its own budget");
    }

    #[test]
    fn a_flood_of_new_keys_never_unblocks_a_blocked_one() {
        let t = Throttle::new(MAX_ATTEMPTS);
        for _ in 0..MAX_ATTEMPTS {
            t.try_hit("victim|ip").unwrap();
        }
        assert!(t.try_hit("victim|ip").is_err());
        // Enough junk keys to fill the map several times over.
        for i in 0..3 * THROTTLE_CAP {
            let _ = t.try_hit(&format!("junk{i}"));
        }
        assert!(t.try_hit("victim|ip").is_err(), "still blocked");
        assert!(t.len() <= THROTTLE_CAP + 1, "{}", t.len());

        // Blocked keys are never evicted; the map grows past its cap rather than dropping them.
        let full = Throttle::new(1);
        for i in 0..THROTTLE_CAP + 50 {
            full.try_hit(&format!("k{i}")).unwrap();
        }
        assert!(full.try_hit("k0").is_err() && full.try_hit("k10049").is_err());
        assert_eq!(full.len(), THROTTLE_CAP + 50);
    }

    #[test]
    fn throttles_use_their_own_window() {
        let t = Throttle::with_window(1, Duration::from_millis(50));
        t.try_hit("k").unwrap();
        assert_eq!(t.try_hit("k").unwrap_err().retry_after, 1);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(t.try_hit("k").unwrap(), 0, "a new window");
    }

    #[test]
    fn emails_match_only_up_to_ascii_case() {
        assert_eq!(normalize_email("  Ada@Example.COM\t"), "ada@example.com");
        assert!(email_matches("ada@example.com", " ADA@example.com "));
        assert!(email_matches("Ada@Example.com", "ada@example.com"));
        // What accent- or width-insensitive collations call equal is another address here.
        for typed in [
            "victim@gmaíl.com",
            "vıctim@gmail.com",
            "victim@ｇmail.com",
            "victim@gmail.com\u{200b}",
            "victim@gmail.co",
        ] {
            assert!(!email_matches("victim@gmail.com", typed), "{typed}");
        }
        assert_eq!(email_candidates("Ada@x.io "), ["ada@x.io", "Ada@x.io"]);
        assert_eq!(email_candidates("ada@x.io"), ["ada@x.io"]);
        let picked = pick_account(
            vec![
                (1, "Ada@x.io".to_owned()),
                (2, "ada@x.io".to_owned()),
                (3, "ádá@x.io".to_owned()),
            ],
            "ADA@x.io",
        );
        assert_eq!(
            picked,
            Some((2, "ada@x.io".to_owned())),
            "the normalized one first"
        );
        assert_eq!(
            pick_account(vec![(3, "ádá@x.io".to_owned())], "ada@x.io"),
            None
        );
    }

    #[test]
    fn ipv6_clients_are_throttled_by_their_64() {
        assert_eq!(
            throttle_ip("2001:db8:1:2:aaaa:bbbb:cccc:dddd"),
            "2001:db8:1:2::/64"
        );
        assert_eq!(
            throttle_ip("2001:db8:1:2::1"),
            throttle_ip("2001:db8:1:2:ffff::9")
        );
        assert_ne!(
            throttle_ip("2001:db8:1:2::1"),
            throttle_ip("2001:db8:1:3::1")
        );
        assert_eq!(throttle_ip("::ffff:192.0.2.7"), "192.0.2.7");
        assert_eq!(throttle_ip("192.0.2.7"), "192.0.2.7");
        assert_eq!(throttle_ip("unknown"), "unknown");
    }

    #[test]
    fn address_budgets_count_per_client_network() {
        assert_eq!(throttle_network("192.0.2.7"), "192.0.2.0/24");
        assert_eq!(throttle_network("::ffff:192.0.2.9"), "192.0.2.0/24");
        assert_ne!(throttle_network("192.0.2.7"), throttle_network("192.0.3.7"));
        assert_eq!(
            throttle_network("2001:db8:1:2:aaaa::1"),
            throttle_network("2001:db8:1:ffff::9")
        );
        assert_eq!(throttle_network("2001:db8:1:2::1"), "2001:db8:1::/48");
        assert_eq!(throttle_network("unknown"), "unknown");
    }

    #[test]
    fn a_refund_gives_one_hit_back() {
        let t = Throttle::new(2);
        assert!(t.try_hit("k").is_ok());
        t.refund("k");
        assert!(t.try_hit("k").is_ok());
        assert!(t.try_hit("k").is_ok());
        assert!(t.try_hit("k").is_err());
        t.refund("nobody");
        assert_eq!(t.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_hash_gate_bounds_concurrent_work_and_fails_fast_past_its_queue() {
        use std::sync::atomic::AtomicUsize;
        let gate = Arc::new(HashGate::new(2, 3));
        let running = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(tokio::sync::Barrier::new(20));
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..20 {
            let (gate, running, most, start) = (
                Arc::clone(&gate),
                Arc::clone(&running),
                Arc::clone(&most),
                Arc::clone(&start),
            );
            tasks.spawn(async move {
                start.wait().await;
                gate.run(move || {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(200));
                    running.fetch_sub(1, Ordering::SeqCst);
                })
                .await
            });
        }
        let mut done = 0;
        let mut busy = 0;
        while let Some(result) = tasks.join_next().await {
            match result.unwrap() {
                Ok(()) => done += 1,
                Err(e) => {
                    assert_eq!(e.status(), StatusCode::SERVICE_UNAVAILABLE);
                    busy += 1;
                }
            }
        }
        assert!(most.load(Ordering::SeqCst) <= 2, "at most two at once");
        assert_eq!(done + busy, 20);
        assert!(
            (2..=5).contains(&done),
            "two running and three waiting: {done}"
        );
        assert!(busy >= 15, "{busy}");
    }

    #[test]
    fn sessions_are_bound_to_the_password_hash() {
        let a = session_binding("$argon2id$hash-a", None);
        assert_eq!(a, session_binding("$argon2id$hash-a", None));
        assert_ne!(a, session_binding("$argon2id$hash-b", None));
        // The epoch changes it; each value differs from no epoch and from the others.
        let one = session_binding("$argon2id$hash-a", Some(1));
        assert_ne!(one, a);
        assert_ne!(one, session_binding("$argon2id$hash-a", Some(2)));
        assert_ne!(session_binding("$argon2id$hash-a", Some(0)), a);
        assert_eq!(a.len(), 64);
        assert!(!a.contains("hash-a"));
    }

    /// Sessions signed in before `AuthUser::binding` existed stay valid: the session purpose gives the old value.
    #[test]
    fn the_session_binding_is_unchanged() {
        // Without an epoch (a model without `credentials_epoch`, or a NULL column) the value is the one sessions
        // were signed in with before epochs existed.
        assert_eq!(
            session_binding("$argon2id$hash-a", None),
            "f125bb77c631ba561617409d803827f169b6beda4c33b18af82cce5ee2c4810f"
        );
        #[derive(Clone)]
        struct U;
        impl Authenticatable for U {
            fn auth_id(&self) -> i64 {
                7
            }
            fn password_hash(&self) -> &str {
                "$argon2id$hash-a"
            }
            fn remember_token(&self) -> Option<&str> {
                None
            }
        }
        let user = AuthUser::new(Arc::new(U));
        assert_eq!(
            user.binding("session").unwrap(),
            session_binding("$argon2id$hash-a", None)
        );
        assert_ne!(
            user.binding("hallmark.token").unwrap(),
            user.binding("session").unwrap()
        );
        for bad in ["", "a|b", "Token", "a b"] {
            assert!(user.binding(bad).is_err(), "{bad}");
        }
        // Without an epoch the credential binding is the plain one.
        assert_eq!(
            user.credential_binding("hallmark.token").unwrap(),
            user.binding("hallmark.token").unwrap()
        );
        assert!(user.credential_binding("a|b").is_err());
        assert_eq!(user.id(), 7);
        assert!(user.downcast::<U>().is_some());
        assert_eq!(format!("{user:?}"), "AuthUser { id: 7, .. }");
    }

    /// `end_credentials` changes the epoch: a credential bound with `credential_binding` no longer matches.
    #[test]
    fn credential_bindings_follow_the_epoch() {
        #[derive(Clone)]
        struct E(Option<i64>);
        impl Authenticatable for E {
            fn auth_id(&self) -> i64 {
                7
            }
            fn password_hash(&self) -> &str {
                "$argon2id$hash-a"
            }
            fn remember_token(&self) -> Option<&str> {
                None
            }
            fn credentials_epoch(&self) -> Option<i64> {
                self.0
            }
        }
        let of = |epoch| {
            AuthUser::new(Arc::new(E(epoch)))
                .credential_binding("hallmark.token")
                .unwrap()
        };
        let none = of(None);
        assert_eq!(
            none,
            AuthUser::new(Arc::new(E(None)))
                .binding("hallmark.token")
                .unwrap()
        );
        assert_ne!(of(Some(0)), none);
        assert_ne!(of(Some(1)), of(Some(2)));
        assert_eq!(of(Some(1)), of(Some(1)));
        assert_eq!(none.len(), 64);
    }
}
