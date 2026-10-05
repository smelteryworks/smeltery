//! Password resets: a one-hour token per account in `password_reset_tokens` (`user_id`, `token`
//! (its SHA-256), `created_at`), and the reset itself through the user model registered with
//! [`AppBuilder::auth`](crate::AppBuilder::auth).
//!
//! The account is found by its address after [`normalize_email`](super::normalize_email); the
//! token row is the account's (by its id, so no column collation can make two accounts share
//! one), the mail and the link use the address stored on the account, never the one typed into
//! the form, and the password is changed by the account's id.
//!
//! The reset link goes to the [`ResetNotifier`] service when one is installed (the mail crate's
//! `.mail()` installs one that sends a mail); otherwise it is written to the log (`info`, target
//! `smeltery::mail`).

use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query};
use sea_orm::{ConnectionTrait, Value};

use crate::app::{App, BoxFuture};
use crate::db::Db;
use crate::error::{Error, Result};

/// Delivers password reset links: registered as the service `Arc<dyn ResetNotifier>`
/// (`smeltery::mail` installs one that sends the reset mail). Without one the link is logged.
pub trait ResetNotifier: Send + Sync + 'static {
    /// Send `url` (the reset link) to `email`, the address stored on the account.
    ///
    /// # Errors
    /// The delivery failed.
    fn send<'a>(&'a self, app: &'a App, email: &'a str, url: &'a str) -> BoxFuture<'a, Result<()>>;
}

/// The token table.
pub const TOKENS_TABLE: &str = "password_reset_tokens";

/// How long a reset token is valid.
pub const TOKEN_LIFETIME: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// How often one address may be sent a reset link.
pub const RESEND_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Create a reset token for the account with address `email` and deliver the reset link
/// (`APP_URL` + the route named `password.reset` with its `{token}` + `?email=…`, else
/// `APP_URL/reset-password/{token}?email=…`) to the address stored on that account, through
/// the [`ResetNotifier`] service. Without one, the link is written to the log at `info` under
/// `APP_ENV` `local` or `testing`; in any other environment only a warning that no mail is
/// set up is logged, never the link.
///
/// `Ok(Some(user id))` when a link was issued, `Ok(None)` when not (an unknown address, or a link
/// went to the address within the last minute): for the caller's own records only, never for the
/// answer. The answer is the same whether or not the address has an account (the caller shows the
/// same message either way): an address without an account does nothing, at most one link a
/// minute goes to one address (further requests in that minute do nothing), and the mail is
/// sent in the background, so the response time does not tell whether a mail went out. A
/// failed delivery is logged, not returned. Under `APP_ENV=testing` the mail is sent before
/// the call returns, so tests can look at it.
///
/// # Errors
/// No database or user model, or a query fails.
pub async fn send_reset_link(app: &App, email: &str) -> Result<Option<i64>> {
    let db = app.db()?;
    let provider = app.user_provider().ok_or_else(|| {
        Error::internal("no user model is registered: call `.auth::<User>()` in bootstrap/app.rs")
    })?;
    // Counted for every address, with or without an account, so the limit reveals nothing.
    if app
        .reset_throttle()
        .try_hit(&super::normalize_email(email))
        .is_err()
    {
        return Ok(None);
    }
    let Some((user, stored)) = provider.find_by_email(&db, email).await? else {
        return Ok(None);
    };
    let token = store_token(&db, user.id(), Some(&stored)).await?;
    let path = match app.url("password.reset", &[("token", &token), ("email", &stored)]) {
        Ok(path) => path,
        Err(_) => {
            let query =
                serde_urlencoded::to_string([("email", stored.as_str())]).map_err(Error::other)?;
            format!("/reset-password/{token}?{query}")
        }
    };
    // Always APP_URL, never the request's Host: a forged Host must not put another domain into the mail.
    let url = format!("{}{path}", app.settings().url.trim_end_matches('/'));
    deliver(app, stored, url).await;
    Ok(Some(user.id()))
}

/// Hand the link to the notifier (in the background, outside `testing`), or log it.
async fn deliver(app: &App, email: String, url: String) {
    if let Some(notifier) = app.service::<std::sync::Arc<dyn ResetNotifier>>() {
        let owned = app.clone();
        let send = async move {
            if let Err(e) = notifier.send(&owned, &email, &url).await {
                tracing::warn!(target: "smeltery::mail", error = %e, "the password reset mail could not be sent");
            }
        };
        if app.settings().env == "testing" {
            send.await;
        } else {
            // Owned by the app's task tracker: `serve` and `work` wait for it on shutdown.
            app.tasks().spawn(send);
        }
        return;
    }
    // Without mail, a development app reads the link in its log. Anywhere else the link is a
    // live credential and must not reach the log (CLAUDE.md: secrets are never logged).
    if app.settings().is_local_development() {
        tracing::info!(target: "smeltery::mail", reset_url = %url, "password reset link");
    } else {
        tracing::warn!(
            target: "smeltery::mail",
            "a password reset was requested, but no mail is set up (`.mail()` in bootstrap/app.rs), so no link was sent"
        );
    }
}

/// Store a new token for the account with id `user_id` (replacing an older one) and return it
/// in clear.
///
/// # Errors
/// A query fails.
pub async fn create_token(db: &Db, user_id: i64) -> Result<String> {
    store_token(db, user_id, None).await
}

/// How many characters of a reset token name the address it was mailed to.
const ADDRESS_TAG_LEN: usize = 16;

/// The tag of `address` a mailed token ends with: the first 16 hex characters of its SHA-256.
fn address_tag(address: &str) -> String {
    let mut tag = crate::crypto::sha256_hex(&format!("smeltery-reset|{address}"));
    tag.truncate(ADDRESS_TAG_LEN);
    tag
}

/// A new 64-character token for `user_id`, stored as its SHA-256. A token mailed to `address` ends with the
/// address's tag (48 random characters + 16 hex): the whole token is hashed in the table, so nobody can change the
/// tag, and a reset can tell whether the token reached the account's current address.
async fn store_token(db: &Db, user_id: i64, address: Option<&str>) -> Result<String> {
    let token = match address {
        Some(address) => format!(
            "{}{}",
            crate::crypto::random_token(64 - ADDRESS_TAG_LEN)?,
            address_tag(address)
        ),
        None => crate::crypto::random_token(64)?,
    };
    let backend = db.conn().get_database_backend();
    let delete = Query::delete()
        .from_table(Alias::new(TOKENS_TABLE))
        .and_where(Expr::col(Alias::new("user_id")).eq(user_id))
        .to_owned();
    db.conn().execute_raw(backend.build(&delete)).await?;
    let mut insert = Query::insert();
    insert
        .into_table(Alias::new(TOKENS_TABLE))
        .columns([
            Alias::new("user_id"),
            Alias::new("token"),
            Alias::new("created_at"),
        ])
        .values([
            Value::from(user_id).into(),
            Value::from(crate::crypto::sha256_hex(&token)).into(),
            Value::from(sea_orm::prelude::ChronoUtc::now()).into(),
        ])
        .map_err(|e| Error::internal(e.to_string()))?;
    db.conn().execute_raw(backend.build(&insert)).await?;
    Ok(token)
}

/// Set a new password for the account with address `email` when `token` is its valid,
/// unexpired reset token: `Ok(Some(user id))` when the password was changed, `Ok(None)` for an
/// unknown address or a wrong, used or expired token (an expired one is deleted).
///
/// The account is found like [`send_reset_link`] finds it, through the user model registered with
/// [`AppBuilder::auth`](crate::AppBuilder::auth) (its table and its `email`, `password` and
/// `remember_token` columns), and changed by its id. The token is used up in one conditional
/// `DELETE` before the password is written, so two requests with one token change the password at
/// most once. The new password is hashed with argon2id; the user's remember token is cleared, and
/// the new password hash signs out every session of the user. When the app requires verified
/// addresses ([`AppBuilder::verify_email`](crate::AppBuilder::verify_email)) and the token was mailed
/// (by [`send_reset_link`]) to the address the account has now, the address is marked verified (the
/// link reached it); a token from [`create_token`] verifies nothing. Then the [`CredentialListener`](super::CredentialListener)s run
/// ([`CredentialChange::Reset`](super::CredentialChange::Reset)) and
/// [`AuthEvent::RevokedAll`](super::AuthEvent::RevokedAll) (every credential) is published.
///
/// # Errors
/// No database or user model, a query fails, hashing fails (503 when too many hashes wait), or a
/// credential listener or a write after the password fails (the password is changed already, and the listeners and
/// the event ran all the same).
pub async fn reset(app: &App, email: &str, token: &str, new_password: &str) -> Result<Option<i64>> {
    let db = app.db()?;
    let provider = app.user_provider().ok_or_else(super::no_user_model)?;
    let backend = db.conn().get_database_backend();
    let Some((user, current_address)) = provider.find_by_email(&db, email).await? else {
        return Ok(None);
    };
    let id = user.id();
    let select = Query::select()
        .columns([Alias::new("token"), Alias::new("created_at")])
        .from(Alias::new(TOKENS_TABLE))
        .and_where(Expr::col(Alias::new("user_id")).eq(id))
        .to_owned();
    let Some(row) = db.conn().query_one_raw(backend.build(&select)).await? else {
        return Ok(None);
    };
    let stored_hash: String = row.try_get("", "token")?;
    let created: sea_orm::prelude::DateTimeUtc = row.try_get("", "created_at")?;
    let fresh = is_fresh(created, sea_orm::prelude::ChronoUtc::now());
    if !crate::crypto::same(&stored_hash, &crate::crypto::sha256_hex(token)) {
        return Ok(None);
    }
    // Only this token's row: a newer link sent meanwhile stays.
    let delete = Query::delete()
        .from_table(Alias::new(TOKENS_TABLE))
        .and_where(Expr::col(Alias::new("user_id")).eq(id))
        .and_where(Expr::col(Alias::new("token")).eq(stored_hash))
        .to_owned();
    if !fresh {
        db.conn().execute_raw(backend.build(&delete)).await?;
        return Ok(None);
    }
    let hash = super::hash_password(new_password).await?;
    // Used up before the password changes: of two requests with one token, one wins.
    if db
        .conn()
        .execute_raw(backend.build(&delete))
        .await?
        .rows_affected()
        == 0
    {
        return Ok(None);
    }
    provider.set_column(&db, id, "password", Some(hash)).await?;
    // The password has changed: from here no step skips the next (as `end_credentials`). A failed write is kept,
    // the listeners still run and the event still goes out, then the first error is returned.
    let remember = provider.set_remember_token(&db, id, None).await;
    // The token proves control of the address it was mailed to: only that address is marked verified.
    let mailed_here = token.len() == 64
        && token
            .get(64 - ADDRESS_TAG_LEN..)
            .is_some_and(|tag| crate::crypto::same(tag, &address_tag(&current_address)));
    let mut was_unverified = false;
    let mut verified = Ok(());
    if mailed_here && let Some(verifier) = app.verifier() {
        match verifier.mark_verified(&db, id).await {
            Ok(was) => was_unverified = was,
            Err(e) => verified = Err(e),
        }
    }
    let listeners = super::credentials::changed(
        app,
        id,
        super::CredentialChange::Reset,
        None,
        was_unverified,
    )
    .await;
    for (step, result) in [
        ("the remember token", &remember),
        ("verification", &verified),
    ] {
        if let Err(e) = result {
            tracing::error!(user_id = id, error = %e, "a password reset could not update {step}");
        }
    }
    remember.and(verified).and(listeners).map(|()| Some(id))
}

/// How far in the future a stored `created_at` may lie and still count as "just created". MySQL's
/// `TIMESTAMP` keeps whole seconds and ROUNDS the fraction, so a token written at 12:00:00.6 reads
/// back as 12:00:01, after "now" (found by CI's MySQL generated-app job).
const CLOCK_SLACK: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether a token created at `created` is still valid at `now`.
fn is_fresh(created: sea_orm::prelude::DateTimeUtc, now: sea_orm::prelude::DateTimeUtc) -> bool {
    match now.signed_duration_since(created).to_std() {
        Ok(age) => age < TOKEN_LIFETIME,
        // A negative age: `created` lies after `now`.
        Err(_) => created
            .signed_duration_since(now)
            .to_std()
            .is_ok_and(|ahead| ahead <= CLOCK_SLACK),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_token_rounded_up_to_the_next_second_is_fresh() {
        let now = sea_orm::prelude::ChronoUtc::now();
        // MySQL rounds 12:00:00.6 up to 12:00:01: read back, the token was "created" after now.
        assert!(is_fresh(now + Duration::from_millis(400), now));
        assert!(is_fresh(now, now));
        assert!(is_fresh(now - Duration::from_secs(59 * 60), now));
    }

    #[test]
    fn old_or_far_future_tokens_are_not_fresh() {
        let now = sea_orm::prelude::ChronoUtc::now();
        assert!(!is_fresh(now - TOKEN_LIFETIME, now));
        assert!(!is_fresh(now + Duration::from_secs(5 * 60), now));
    }
}
