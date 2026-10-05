//! Personal access tokens: creating, listing, revoking and pruning them ([`Tokens`]), what a token looks like
//! ([`AccessToken`], [`NewToken`], [`PlainToken`]) and the table ([`migrations`](crate::migrations)).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use sea_orm::prelude::{ChronoUtc, DateTimeUtc};
use sea_orm::sea_query::{Alias, Expr, ExprTrait, Order, Query};
use sea_orm::{ConnectionTrait, QueryResult, Value};
use serde::Serialize;
use smeltery_core::auth::{AuthEvent, AuthUser, Authenticatable, CredentialKind, publish_event};
use smeltery_core::crypto::{constant_time_eq, random_bytes, sha256_hex};
use smeltery_core::db::Db;
use smeltery_core::{App, Error, Result};

use crate::State;

/// The table tokens are stored in.
pub const TABLE: &str = "personal_access_tokens";

/// What every plain token starts with (secret scanners and log redaction find it by this).
pub const TOKEN_PREFIX: &str = "smt_";

/// The purpose of a token's password binding ([`AuthUser::binding`](smeltery_core::auth::AuthUser::binding)).
pub(crate) const BINDING_PURPOSE: &str = "hallmark.token";

/// Rows deleted per statement when pruning.
const PRUNE_BATCH: u64 = 500;

/// The longest token name, in characters.
pub const MAX_NAME_LEN: usize = 255;

/// The columns read for a token.
const COLUMNS: [&str; 9] = [
    "id",
    "user_id",
    "name",
    "token_hash",
    "binding",
    "abilities",
    "last_used_at",
    "expires_at",
    "created_at",
];

/// A new plain token: `smt_` + 64 lowercase hex characters (32 bytes from the OS random source).
pub(crate) fn generate() -> Result<String> {
    let bytes = random_bytes(32)?;
    let mut out = String::with_capacity(TOKEN_PREFIX.len() + 64);
    out.push_str(TOKEN_PREFIX);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    Ok(out)
}

/// Whether `token` has the shape of a plain token (checked before any database work).
pub(crate) fn well_formed(token: &str) -> bool {
    token.len() == TOKEN_PREFIX.len() + 64
        && token.starts_with(TOKEN_PREFIX)
        && token
            .bytes()
            .skip(TOKEN_PREFIX.len())
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A token as the app sees it: never its secret or its hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct AccessToken {
    /// The token's id.
    pub id: i64,
    /// The user it belongs to.
    pub user_id: i64,
    /// Its name ("Ada's phone", "build server").
    pub name: String,
    /// What it may do (`*` = everything). A stored list that fails the bounds reads as empty.
    pub abilities: Vec<String>,
    /// When a request last used it (written at most once a minute per token).
    pub last_used_at: Option<DateTimeUtc>,
    /// When it stops working: the earlier of its own expiry and its creation plus `HALLMARK_TOKEN_EXPIRATION`;
    /// `None` = never.
    pub expires_at: Option<DateTimeUtc>,
    /// When it was created.
    pub created_at: DateTimeUtc,
}

impl AccessToken {
    /// Whether the token grants `ability`: listed by exact name, or `*` listed.
    pub fn can(&self, ability: &str) -> bool {
        self.abilities.iter().any(|a| a == "*" || a == ability)
    }

    /// Whether it has expired at `now`.
    pub fn is_expired_at(&self, now: DateTimeUtc) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }
}

/// The plain text of a new token. It has no `Display` and no `Serialize`, and its `Debug` shows only
/// `PlainToken(smt_…)`, so it never lands in a log line or a JSON body by accident.
/// It has no `PartialEq` either: tokens are verified by their hash, in constant time, never compared as text.
#[derive(Clone)]
pub struct PlainToken(String);

impl PlainToken {
    /// The token text (`smt_…`): show it to the user once; it is not stored anywhere.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PlainToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PlainToken(smt_…)")
    }
}

/// A token just created: its plain text (shown once) and its stored data.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct NewToken {
    plain: PlainToken,
    token: AccessToken,
}

impl NewToken {
    /// The plain token (`smt_…`), to hand to the client once.
    pub fn plain_text(&self) -> &str {
        self.plain.expose()
    }

    /// The plain token, wrapped.
    pub fn plain(&self) -> &PlainToken {
        &self.plain
    }

    /// The token's stored data.
    pub fn token(&self) -> &AccessToken {
        &self.token
    }

    /// The answer of an endpoint that issues the token:
    /// `{"token":"smt_…","token_type":"Bearer","expires_at":…,"abilities":[…]}`. Send it with
    /// `Cache-Control: no-store`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "token": self.plain.expose(),
            "token_type": "Bearer",
            "expires_at": self.token.expires_at,
            "abilities": self.token.abilities,
        })
    }
}

/// A token row as stored (the guard's view).
pub(crate) struct Stored {
    pub(crate) token: AccessToken,
    pub(crate) token_hash: String,
    pub(crate) binding: String,
}

fn column(name: &str) -> Alias {
    Alias::new(name)
}

fn table() -> Alias {
    Alias::new(TABLE)
}

/// The earlier of `stored` and `created + max_age`.
fn effective_expiry(
    stored: Option<DateTimeUtc>,
    created: DateTimeUtc,
    max_age: Option<Duration>,
) -> Option<DateTimeUtc> {
    let by_age = max_age
        .and_then(|age| chrono_delta(age).and_then(|delta| created.checked_add_signed(delta)));
    match (stored, by_age) {
        (Some(a), Some(b)) => Some(std::cmp::min(a, b)),
        (a, b) => a.or(b),
    }
}

/// `duration` as chrono's delta (`None` when out of its range).
fn chrono_delta(duration: Duration) -> Option<sea_orm::sea_query::prelude::chrono::TimeDelta> {
    sea_orm::sea_query::prelude::chrono::TimeDelta::from_std(duration).ok()
}

/// `now - duration`, `None` when it underflows.
fn before(now: DateTimeUtc, duration: Duration) -> Option<DateTimeUtc> {
    chrono_delta(duration).and_then(|delta| now.checked_sub_signed(delta))
}

/// Read one token row.
fn read_row(row: &QueryResult, max_age: Option<Duration>) -> Result<Stored> {
    let id: i64 = row.try_get("", "id")?;
    let text: String = row.try_get("", "abilities")?;
    let abilities = crate::abilities::parse_stored(&text).unwrap_or_else(|| {
        // Fail closed: a row nobody wrote through `create` grants nothing.
        tracing::error!(
            token_id = id,
            "a stored token's abilities are invalid; the token grants no ability"
        );
        Vec::new()
    });
    let created_at: DateTimeUtc = row.try_get("", "created_at")?;
    let expires_at: Option<DateTimeUtc> = row.try_get("", "expires_at")?;
    Ok(Stored {
        token: AccessToken {
            id,
            user_id: row.try_get("", "user_id")?,
            name: row.try_get("", "name")?,
            abilities,
            last_used_at: row.try_get("", "last_used_at")?,
            expires_at: effective_expiry(expires_at, created_at, max_age),
            created_at,
        },
        token_hash: row.try_get("", "token_hash")?,
        binding: row.try_get("", "binding")?,
    })
}

/// Whether `name` is a valid token name: 1 to 255 characters, no control characters.
fn valid_name(name: &str) -> bool {
    let count = name.chars().count();
    (1..=MAX_NAME_LEN).contains(&count) && !name.chars().any(char::is_control)
}

/// The app's tokens: create, list, revoke and prune them. Get it with [`Tokens::of`] or as a handler argument.
///
/// ```no_run
/// use smeltery::hallmark::Tokens;
///
/// async fn demo(app: &smeltery::App, user_id: i64) -> smeltery::Result<()> {
///     let tokens = Tokens::of(app)?;
///     let new = tokens.create(user_id, "build server", &["deploy"], None).await?;
///     let plain: &str = new.plain_text(); // `smt_…`: shown once, never stored
///     # let _ = plain;
///     for token in tokens.list(user_id).await? {
///         println!("{} {}", token.id, token.name);
///     }
///     tokens.revoke(user_id, new.token().id).await?;
///     Ok(())
/// }
/// # let _ = demo;
/// ```
#[derive(Clone)]
pub struct Tokens {
    app: App,
    state: Arc<State>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("settings", &self.state.settings)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    /// The tokens of `app`.
    ///
    /// # Errors
    /// Hallmark is not installed (`.hallmark(...)` in `bootstrap/app.rs`).
    pub fn of(app: &App) -> Result<Self> {
        let state = app.service::<State>().ok_or_else(|| {
            Error::internal(
                "Hallmark is not installed: call `.hallmark(Hallmark::new())` in bootstrap/app.rs",
            )
        })?;
        Ok(Self {
            app: app.clone(),
            state,
        })
    }

    fn db(&self) -> Result<Db> {
        self.app.db()
    }

    /// Create a token for user `user_id` named `name` with `abilities` (`&["*"]` for every ability; `&[]` gives a
    /// token that may do nothing). `expires_at` `None` means now + `HALLMARK_TOKEN_EXPIRATION` (never with `0`); a
    /// later time is shortened to that. When the user holds `HALLMARK_MAX_TOKENS_PER_USER` tokens already, the
    /// least recently used ones are deleted in the same transaction (and their revocation published).
    ///
    /// # Errors
    /// An invalid name (1 to 255 characters, no control characters) or ability (400), no user with that id (400),
    /// no user model, or a query fails.
    pub async fn create(
        &self,
        user_id: i64,
        name: &str,
        abilities: &[&str],
        expires_at: Option<DateTimeUtc>,
    ) -> Result<NewToken> {
        if !valid_name(name) {
            return Err(Error::bad_request(format!(
                "a token name has 1 to {MAX_NAME_LEN} characters and no control characters"
            )));
        }
        let abilities = crate::abilities::checked(abilities)?;
        let user = self
            .app
            .find_user(user_id)
            .await?
            .ok_or_else(|| Error::bad_request(format!("there is no user {user_id}")))?;
        self.insert(&user, name, abilities, expires_at).await
    }

    /// [`create`](Self::create) for a user row the caller already read and judged (the token endpoint: the row its
    /// login policy and second factor saw). The token is bound to that row's password hash and credentials epoch, so
    /// when `end_credentials` or a password change ran after the read, the token ends at its first use.
    pub(crate) async fn create_for(
        &self,
        user: &AuthUser,
        name: &str,
        abilities: &[&str],
        expires_at: Option<DateTimeUtc>,
    ) -> Result<NewToken> {
        if !valid_name(name) {
            return Err(Error::bad_request(format!(
                "a token name has 1 to {MAX_NAME_LEN} characters and no control characters"
            )));
        }
        let abilities = crate::abilities::checked(abilities)?;
        self.insert(user, name, abilities, expires_at).await
    }

    /// Store a token for `user` (name and abilities checked already).
    async fn insert(
        &self,
        user: &AuthUser,
        name: &str,
        abilities: Vec<String>,
        expires_at: Option<DateTimeUtc>,
    ) -> Result<NewToken> {
        let user_id = user.id();
        let binding = user.credential_binding(BINDING_PURPOSE)?;
        let now = ChronoUtc::now();
        let max_age = self.state.settings.max_age();
        let latest =
            max_age.and_then(|age| chrono_delta(age).and_then(|d| now.checked_add_signed(d)));
        let expires_at = match (expires_at, latest) {
            (Some(asked), Some(latest)) => Some(std::cmp::min(asked, latest)),
            (asked, latest) => asked.or(latest),
        };
        let plain = generate()?;
        let hash = sha256_hex(&plain);
        let db = self.db()?;
        let conn = db.conn();
        let backend = conn.get_database_backend();
        // The count, the eviction and the insert under the write lock from the start (SQLite `BEGIN IMMEDIATE`):
        // concurrent creations wait for each other instead of failing with "database is locked".
        let txn = db.begin_write().await?;
        // The cap, in the transaction that inserts: the least recently used tokens make room.
        let cap = u64::from(self.state.settings.tokens_per_user());
        let held = Query::select()
            .expr(Expr::col(column("id")).count())
            .from(table())
            .and_where(Expr::col(column("user_id")).eq(user_id))
            .to_owned();
        let held: i64 = match txn.query_one_raw(backend.build(&held)).await? {
            Some(row) => row.try_get_by_index(0)?,
            None => 0,
        };
        let held = u64::try_from(held).unwrap_or(0);
        let mut evicted: Vec<i64> = Vec::new();
        if held >= cap {
            let oldest = Query::select()
                .column(column("id"))
                .from(table())
                .and_where(Expr::col(column("user_id")).eq(user_id))
                .order_by_expr(Expr::cust("COALESCE(last_used_at, created_at)"), Order::Asc)
                // On a tie the never-used token goes: MySQL timestamps have whole seconds, so a token used in
                // the second another was created ties with it, and by id alone the used one could be evicted.
                .order_by_expr(
                    Expr::cust("CASE WHEN last_used_at IS NULL THEN 0 ELSE 1 END"),
                    Order::Asc,
                )
                .order_by(column("id"), Order::Asc)
                .limit(held - cap + 1)
                .to_owned();
            for row in txn.query_all_raw(backend.build(&oldest)).await? {
                evicted.push(row.try_get("", "id")?);
            }
            if !evicted.is_empty() {
                let delete = Query::delete()
                    .from_table(table())
                    .and_where(Expr::col(column("id")).is_in(evicted.clone()))
                    .to_owned();
                txn.execute_raw(backend.build(&delete)).await?;
            }
        }
        let abilities_text = serde_json::to_string(&abilities)?;
        let mut insert = Query::insert();
        insert
            .into_table(table())
            .columns([
                column("user_id"),
                column("name"),
                column("token_hash"),
                column("binding"),
                column("abilities"),
                column("expires_at"),
                column("created_at"),
                column("updated_at"),
            ])
            .values([
                Value::from(user_id).into(),
                Value::from(name.to_owned()).into(),
                Value::from(hash.clone()).into(),
                Value::from(binding).into(),
                Value::from(abilities_text).into(),
                Value::from(expires_at).into(),
                Value::from(now).into(),
                Value::from(now).into(),
            ])
            .map_err(|e| Error::internal(e.to_string()))?;
        txn.execute_raw(backend.build(&insert)).await?;
        // Found back by its unique hash: portable across backends (no RETURNING on MySQL).
        let select = Query::select()
            .columns(COLUMNS.map(column))
            .from(table())
            .and_where(Expr::col(column("token_hash")).eq(hash))
            .to_owned();
        let row = txn
            .query_one_raw(backend.build(&select))
            .await?
            .ok_or_else(|| Error::internal("a token just stored was not found"))?;
        let stored = read_row(&row, max_age)?;
        txn.commit().await?;
        for id in evicted {
            announce(
                &self.app,
                AuthEvent::Revoked {
                    user_id,
                    key: token_key(id),
                },
            )
            .await;
        }
        Ok(NewToken {
            plain: PlainToken(plain),
            token: stored.token,
        })
    }

    /// The tokens of user `user_id`, newest first.
    ///
    /// # Errors
    /// A query fails.
    pub async fn list(&self, user_id: i64) -> Result<Vec<AccessToken>> {
        let db = self.db()?;
        let backend = db.conn().get_database_backend();
        let select = Query::select()
            .columns(COLUMNS.map(column))
            .from(table())
            .and_where(Expr::col(column("user_id")).eq(user_id))
            .order_by(column("id"), Order::Desc)
            .to_owned();
        let max_age = self.state.settings.max_age();
        db.conn()
            .query_all_raw(backend.build(&select))
            .await?
            .iter()
            .map(|row| read_row(row, max_age).map(|s| s.token))
            .collect()
    }

    /// Token `token_id` of user `user_id`, `None` when that user has no such token.
    ///
    /// # Errors
    /// A query fails.
    pub async fn find(&self, user_id: i64, token_id: i64) -> Result<Option<AccessToken>> {
        let db = self.db()?;
        let backend = db.conn().get_database_backend();
        let select = Query::select()
            .columns(COLUMNS.map(column))
            .from(table())
            .and_where(Expr::col(column("id")).eq(token_id))
            .and_where(Expr::col(column("user_id")).eq(user_id))
            .to_owned();
        match db.conn().query_one_raw(backend.build(&select)).await? {
            Some(row) => Ok(Some(read_row(&row, self.state.settings.max_age())?.token)),
            None => Ok(None),
        }
    }

    /// Delete token `token_id` when it belongs to user `user_id` (ids that come from a request reach only that
    /// user's tokens); `true` when one was deleted, and its revocation is published.
    ///
    /// # Errors
    /// A query fails.
    pub async fn revoke(&self, user_id: i64, token_id: i64) -> Result<bool> {
        let deleted = self
            .delete_where(user_id, Some(Expr::col(column("id")).eq(token_id)))
            .await?;
        if deleted > 0 {
            announce(
                &self.app,
                AuthEvent::Revoked {
                    user_id,
                    key: token_key(token_id),
                },
            )
            .await;
        }
        Ok(deleted > 0)
    }

    /// Delete every token of user `user_id`; the number deleted. The revocation is published when there was one.
    ///
    /// # Errors
    /// A query fails.
    pub async fn revoke_all(&self, user_id: i64) -> Result<u64> {
        let deleted = self.delete_where(user_id, None).await?;
        if deleted > 0 {
            announce(&self.app, revoked_all(user_id, None)).await;
        }
        Ok(deleted)
    }

    /// Delete every token of user `user_id` except `keep_id`; the number deleted.
    ///
    /// # Errors
    /// A query fails.
    pub async fn revoke_all_except(&self, user_id: i64, keep_id: i64) -> Result<u64> {
        let deleted = self
            .delete_where(user_id, Some(Expr::col(column("id")).ne(keep_id)))
            .await?;
        if deleted > 0 {
            announce(&self.app, revoked_all(user_id, Some(keep_id))).await;
        }
        Ok(deleted)
    }

    /// Delete user `user_id`'s tokens matching `extra` (all with `None`), without publishing.
    pub(crate) async fn delete_where(
        &self,
        user_id: i64,
        extra: Option<sea_orm::sea_query::SimpleExpr>,
    ) -> Result<u64> {
        let db = self.db()?;
        let backend = db.conn().get_database_backend();
        let mut delete = Query::delete();
        delete
            .from_table(table())
            .and_where(Expr::col(column("user_id")).eq(user_id));
        if let Some(extra) = extra {
            delete.and_where(extra);
        }
        Ok(db
            .conn()
            .execute_raw(backend.build(&delete))
            .await?
            .rows_affected())
    }

    /// Delete the tokens that expired (by their own expiry or by `HALLMARK_TOKEN_EXPIRATION` counted from their
    /// creation) at least `older_than` ago, 500 rows per statement; the number deleted. Expired tokens are refused
    /// whether or not they were pruned.
    ///
    /// # Errors
    /// A query fails.
    pub async fn prune_expired(&self, older_than: Duration) -> Result<u64> {
        let now = ChronoUtc::now();
        let Some(cutoff) = before(now, older_than) else {
            return Ok(0);
        };
        let by_age = self
            .state
            .settings
            .max_age()
            .and_then(|age| before(cutoff, age));
        let db = self.db()?;
        let backend = db.conn().get_database_backend();
        let mut total = 0;
        loop {
            let mut expired = Expr::col(column("expires_at"))
                .is_not_null()
                .and(Expr::col(column("expires_at")).lt(cutoff));
            if let Some(by_age) = by_age {
                expired = expired.or(Expr::col(column("created_at")).lt(by_age));
            }
            let select = Query::select()
                .column(column("id"))
                .from(table())
                .and_where(expired)
                .limit(PRUNE_BATCH)
                .to_owned();
            let mut ids: Vec<i64> = Vec::new();
            for row in db.conn().query_all_raw(backend.build(&select)).await? {
                ids.push(row.try_get("", "id")?);
            }
            if ids.is_empty() {
                break;
            }
            let full = ids.len() as u64 >= PRUNE_BATCH;
            let delete = Query::delete()
                .from_table(table())
                .and_where(Expr::col(column("id")).is_in(ids))
                .to_owned();
            total += db
                .conn()
                .execute_raw(backend.build(&delete))
                .await?
                .rows_affected();
            if !full {
                break;
            }
        }
        Ok(total)
    }

    /// The stored row of the plain token `plain` (looked up by its SHA-256, then re-compared in constant time).
    pub(crate) async fn lookup(&self, plain: &str) -> Result<Option<Stored>> {
        let hash = sha256_hex(plain);
        let db = self.db()?;
        let backend = db.conn().get_database_backend();
        let select = Query::select()
            .columns(COLUMNS.map(column))
            .from(table())
            .and_where(Expr::col(column("token_hash")).eq(hash.clone()))
            .to_owned();
        let Some(row) = db.conn().query_one_raw(backend.build(&select)).await? else {
            return Ok(None);
        };
        let stored = read_row(&row, self.state.settings.max_age())?;
        Ok(constant_time_eq(&stored.token_hash, &hash).then_some(stored))
    }

    /// Delete one token found invalid on use (expired, or its user's password changed) and publish its revocation.
    pub(crate) async fn end(&self, user_id: i64, token_id: i64) {
        let deleted = tokio::time::timeout(
            Duration::from_secs(2),
            self.delete_where(user_id, Some(Expr::col(column("id")).eq(token_id))),
        )
        .await;
        match deleted {
            Ok(Ok(n)) if n > 0 => {
                announce(
                    &self.app,
                    AuthEvent::Revoked {
                        user_id,
                        key: token_key(token_id),
                    },
                )
                .await;
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                tracing::warn!(token_id, error = %e, "an invalid token could not be deleted")
            }
            Err(_) => tracing::warn!(token_id, "deleting an invalid token timed out"),
        }
    }

    /// Bind token `token_id` of user `user_id` to the user's current password hash (after a password change that
    /// keeps it).
    pub(crate) async fn rebind(&self, user_id: i64, token_id: i64) -> Result<u64> {
        let Some(user) = self.app.find_user(user_id).await? else {
            return Ok(0);
        };
        let binding = user.credential_binding(BINDING_PURPOSE)?;
        let db = self.db()?;
        let backend = db.conn().get_database_backend();
        let update = Query::update()
            .table(table())
            .value(column("binding"), binding)
            .value(column("updated_at"), ChronoUtc::now())
            .and_where(Expr::col(column("id")).eq(token_id))
            .and_where(Expr::col(column("user_id")).eq(user_id))
            .to_owned();
        Ok(db
            .conn()
            .execute_raw(backend.build(&update))
            .await?
            .rows_affected())
    }

    /// Write `last_used_at = now` for token `id` unless it was written within the last minute (by any process).
    pub(crate) async fn touch(&self, id: i64) -> Result<u64> {
        let now = ChronoUtc::now();
        let recent = before(now, Duration::from_secs(60)).unwrap_or(now);
        let db = self.db()?;
        let backend = db.conn().get_database_backend();
        let update = Query::update()
            .table(table())
            .value(column("last_used_at"), now)
            .and_where(Expr::col(column("id")).eq(id))
            .and_where(
                Expr::col(column("last_used_at"))
                    .is_null()
                    .or(Expr::col(column("last_used_at")).lt(recent)),
            )
            .to_owned();
        Ok(db
            .conn()
            .execute_raw(backend.build(&update))
            .await?
            .rows_affected())
    }

    pub(crate) fn state(&self) -> &Arc<State> {
        &self.state
    }

    pub(crate) fn app(&self) -> &App {
        &self.app
    }
}

/// The key of this guard's token `id` (core's [`credential_key`](smeltery_core::auth::credential_key), the format
/// of [`Principal::key`](smeltery_core::auth::Principal::key)), what revocation events and `except` name it by.
pub(crate) fn token_key(id: i64) -> String {
    smeltery_core::auth::credential_key(crate::guard::GUARD, "token", &id.to_string())
}

/// `RevokedAll { Tokens }` for `user_id`, keeping `keep` when set.
fn revoked_all(user_id: i64, keep: Option<i64>) -> AuthEvent {
    AuthEvent::RevokedAll {
        user_id,
        kind: CredentialKind::Tokens,
        except: keep.map(token_key),
    }
}

/// Publish without failing the caller: the token is deleted already; a lost event is logged.
pub(crate) async fn announce(app: &App, event: AuthEvent) {
    if let Err(e) = publish_event(app, &event).await {
        tracing::warn!(error = %e, "a token revocation could not be published");
    }
}

impl axum::extract::FromRequestParts<App> for Tokens {
    type Rejection = Error;

    async fn from_request_parts(
        _parts: &mut http::request::Parts,
        app: &App,
    ) -> std::result::Result<Self, Self::Rejection> {
        Self::of(app)
    }
}

/// Token methods on the app's user model (every [`Authenticatable`] type has them).
///
/// ```no_run
/// use smeltery::hallmark::HasApiTokens as _;
///
/// async fn issue<U: smeltery::auth::Authenticatable>(app: &smeltery::App, user: &U) -> smeltery::Result<String> {
///     let new = user.create_token(app, "Ada's phone", &["orders:read"]).await?;
///     Ok(new.plain_text().to_owned())
/// }
/// ```
pub trait HasApiTokens: Authenticatable {
    /// [`Tokens::create`] for this user, with the default expiry.
    fn create_token<'a>(
        &'a self,
        app: &'a App,
        name: &'a str,
        abilities: &'a [&'a str],
    ) -> impl Future<Output = Result<NewToken>> + Send + 'a {
        async move {
            Tokens::of(app)?
                .create(self.auth_id(), name, abilities, None)
                .await
        }
    }

    /// [`Tokens::list`] for this user.
    fn tokens<'a>(
        &'a self,
        app: &'a App,
    ) -> impl Future<Output = Result<Vec<AccessToken>>> + Send + 'a {
        async move { Tokens::of(app)?.list(self.auth_id()).await }
    }

    /// [`Tokens::revoke_all`] for this user.
    fn revoke_tokens<'a>(&'a self, app: &'a App) -> impl Future<Output = Result<u64>> + Send + 'a {
        async move { Tokens::of(app)?.revoke_all(self.auth_id()).await }
    }
}

impl<U: Authenticatable> HasApiTokens for U {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn tokens_have_one_shape() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_ne!(a, b);
        assert!(well_formed(&a), "{a}");
        assert_eq!(a.len(), 68);
        assert!(!well_formed(&a.to_uppercase()));
        assert!(!well_formed(&a[..67]));
        assert!(!well_formed(&format!("{a}0")));
        assert!(!well_formed(&a.replacen("smt_", "smx_", 1)));
        assert!(!well_formed(&format!("smt_{}", "g".repeat(64))));
        assert!(!well_formed(""));
        // FIPS 180-2 test vector: the hash the table stores is plain SHA-256 hex.
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn plain_tokens_never_print() {
        let plain = PlainToken(generate().unwrap());
        let debug = format!("{plain:?}");
        assert_eq!(debug, "PlainToken(smt_…)");
        let new = NewToken {
            plain: plain.clone(),
            token: AccessToken {
                id: 1,
                user_id: 2,
                name: "n".into(),
                abilities: vec!["*".into()],
                last_used_at: None,
                expires_at: None,
                created_at: ChronoUtc::now(),
            },
        };
        let debug = format!("{new:?}");
        assert!(!debug.contains(plain.expose()), "{debug}");
        assert_eq!(new.to_json()["token"], plain.expose());
        assert_eq!(new.to_json()["token_type"], "Bearer");
    }

    #[test]
    fn names_are_bounded() {
        assert!(valid_name("Ada's iPhone"));
        assert!(valid_name(&"é".repeat(255)));
        assert!(!valid_name(""));
        assert!(!valid_name(&"a".repeat(256)));
        assert!(!valid_name("a\nb"));
        assert!(!valid_name("a\u{7f}"));
    }

    #[test]
    fn expiry_is_the_earlier_of_the_date_and_the_age() {
        let created = ChronoUtc::now();
        let day = Duration::from_secs(86_400);
        let age = Some(day * 10);
        let soon = created + chrono_delta(day).unwrap();
        let late = created + chrono_delta(day * 20).unwrap();
        assert_eq!(effective_expiry(Some(soon), created, age), Some(soon));
        assert_eq!(
            effective_expiry(Some(late), created, age),
            Some(created + chrono_delta(day * 10).unwrap())
        );
        assert_eq!(effective_expiry(None, created, None), None);
        assert_eq!(effective_expiry(Some(late), created, None), Some(late));
        assert_eq!(
            effective_expiry(None, created, Some(Duration::MAX)),
            None,
            "an age chrono cannot hold means no age limit, never a panic"
        );
    }
}
