#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod abilities;
mod console;
mod extract;
mod guard;
mod issue;
mod settings;
mod stateful;
pub mod testing;
mod tokens;

use std::sync::atomic::AtomicU64;

use smeltery_core::auth::{CredentialListener, CredentialsChanged};
use smeltery_core::cache::RateLimiter;
use smeltery_core::db::migration::Schema;
use smeltery_core::{App, AppBuilder, BoxFuture, Error, Result};

pub use abilities::{MAX_ABILITIES, MAX_ABILITY_LEN, valid_ability};
pub use extract::CurrentToken;
pub use guard::GUARD;
pub use issue::{CODE_INVALID, CODE_REQUIRED, FAILED, issue_for_credentials, revoke_current};
pub use settings::Hallmark;
pub use tokens::{
    AccessToken, HasApiTokens, MAX_NAME_LEN, NewToken, PlainToken, TABLE, TOKEN_PREFIX, Tokens,
};

/// The memory of recently used tokens and of blocked clients: at most this many entries each.
const MEMORY_ENTRIES: u64 = 10_000;

/// Hallmark's state in an app (a service).
pub(crate) struct State {
    pub(crate) settings: Hallmark,
    /// Invalid bearer tokens per client a minute (the app's cache store, shared by its processes).
    pub(crate) guesses: RateLimiter,
    /// Tokens whose `last_used_at` this process wrote in the last minute.
    pub(crate) recently_used: moka::future::Cache<i64, ()>,
    /// SHA-256 hashes of tokens this process accepted in the last five minutes: while their client is blocked they
    /// still get the full check.
    pub(crate) accepted: moka::future::Cache<String, ()>,
    /// The first-party rule when SPA mode is on (made at boot, from the final `APP_URL`).
    pub(crate) first_party: std::sync::OnceLock<stateful::FirstParty>,
    /// When a `last_used_at` failure was last logged (Unix seconds).
    pub(crate) last_warned: AtomicU64,
}

impl State {
    fn new(settings: Hallmark) -> Self {
        Self {
            first_party: std::sync::OnceLock::new(),
            guesses: RateLimiter::new(
                "hallmark.guesses",
                settings.guesses_per_minute(),
                guard::GUESS_WINDOW,
            ),
            recently_used: moka::future::Cache::builder()
                .max_capacity(MEMORY_ENTRIES)
                .time_to_live(std::time::Duration::from_secs(60))
                .build(),
            accepted: moka::future::Cache::builder()
                .max_capacity(MEMORY_ENTRIES)
                .time_to_live(std::time::Duration::from_secs(300))
                .build(),
            last_warned: AtomicU64::new(0),
            settings,
        }
    }
}

/// Installs Hallmark on an [`AppBuilder`]: `.hallmark(Hallmark::new())` in `bootstrap/app.rs`, after
/// `.auth::<User>()`.
pub trait HallmarkExt: Sized {
    /// Install Hallmark: the guard `hallmark` (routes use `auth:hallmark`), the middleware families `abilities:`
    /// (every listed ability) and `ability:` (at least one), the [`Tokens`] service, the credential listener that
    /// deletes a user's tokens when the password changes, and the console command `hallmark:prune-expired`.
    /// The app stops at boot when no user model is registered (`.auth::<User>()`); `serve` logs an error when the
    /// `personal_access_tokens` table is missing.
    fn hallmark(self, hallmark: Hallmark) -> Self;
}

impl HallmarkExt for AppBuilder {
    fn hallmark(self, hallmark: Hallmark) -> Self {
        let spa = hallmark.is_spa();
        let stateful = hallmark.stateful_origins().join(",");
        let builder = if spa {
            self.xsrf_cookie().routes(|r| {
                r.get("/hallmark/csrf-cookie", csrf_cookie)
                    .name("hallmark.csrf-cookie");
            })
        } else {
            self
        };
        builder
            .service(State::new(hallmark))
            .guard(guard::HallmarkGuard)
            .global_middleware(guard::response_headers)
            .middleware_family("abilities", |args, _route| {
                abilities::family(abilities::Need::All, "abilities", args)
            })
            .middleware_family("ability", |args, _route| {
                abilities::family(abilities::Need::Any, "ability", args)
            })
            .credential_listener(PasswordListener)
            .commands(|c| {
                c.add(console::PruneExpired);
            })
            .on_boot(move |app| async move {
                if spa {
                    // An invalid `HALLMARK_STATEFUL` entry stops the app here.
                    let rule = stateful::FirstParty::new(&app.settings().url, &stateful)?;
                    if let Some(state) = app.service::<State>() {
                        let _ = state.first_party.set(rule);
                    }
                }
                if app.auth_model().is_none() {
                    return Err(Error::internal(
                        "Hallmark needs a user model: call `.auth::<User>()` in bootstrap/app.rs",
                    ));
                }
                Ok(())
            })
            .on_serve(|app| async move {
                check_table(&app).await;
                Ok(())
            })
    }
}

/// `GET /hallmark/csrf-cookie` (SPA mode): 204; the web stack sets the `XSRF-TOKEN` cookie on it.
async fn csrf_cookie() -> http::StatusCode {
    http::StatusCode::NO_CONTENT
}

/// Log one error when the tokens table is missing (`serve` still starts: every token is refused).
async fn check_table(app: &App) {
    let Ok(db) = app.db() else {
        tracing::error!("hallmark: API tokens need a database (DATABASE_URL)");
        return;
    };
    match Schema::new(&db).has_table(TABLE).await {
        Ok(true) => {}
        Ok(false) => tracing::error!(
            table = TABLE,
            "hallmark: the API tokens table is missing; add its migration and run `migrate`"
        ),
        Err(e) => {
            tracing::error!(error = %e, "hallmark: the API tokens table could not be checked")
        }
    }
}

/// Deletes a user's tokens when the password changes (a reset, another device signed out, a new password),
/// except the Hallmark token of the request that made the change, which is bound to the new password so it keeps
/// working. Core publishes the revocation of every credential, so nothing is published here.
struct PasswordListener;

/// The id of the Hallmark token that a [`CredentialsChanged::except`] names: the key of this
/// guard's token (core's [`credential_key`](smeltery_core::auth::credential_key)), rebuilt and compared
/// whole, so another guard's key never matches.
fn kept_token(except: Option<&str>) -> Option<i64> {
    let key = except?;
    let (_, id) = key.rsplit_once(':')?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id = id.parse::<i64>().ok()?;
    (tokens::token_key(id) == key).then_some(id)
}

impl CredentialListener for PasswordListener {
    fn credentials_changed<'a>(
        &'a self,
        app: &'a App,
        change: &'a CredentialsChanged,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let tokens = Tokens::of(app)?;
            let keep = kept_token(change.except.as_deref());
            let extra = keep.map(|id| {
                use sea_orm::sea_query::{Alias, Expr, ExprTrait};
                Expr::col(Alias::new("id")).ne(id)
            });
            tokens.delete_where(change.user_id, extra).await?;
            if let Some(id) = keep
                // The new password hash is stored before listeners run: bind the kept token to it, or its next
                // request would find the old binding and end it.
                && tokens.rebind(change.user_id, id).await? == 0
            {
                // A request of the same token that ran between the new hash and this rebind found the old binding
                // and ended the token: the caller must know it was not kept.
                return Err(Error::http(
                    http::StatusCode::UNAUTHORIZED,
                    "The token that changed the password was signed out meanwhile; sign in again.",
                ));
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::kept_token;

    #[test]
    fn the_kept_token_is_read_from_the_key() {
        let key = crate::tokens::token_key(7);
        assert_eq!(kept_token(Some(&key)), Some(7));
        let session = smeltery_core::auth::Principal::new(
            3,
            smeltery_core::auth::WEB_GUARD,
            smeltery_core::auth::Credential::session("ab"),
        )
        .key();
        let other_guard = smeltery_core::auth::Principal::new(
            3,
            "demo",
            smeltery_core::auth::Credential::token::<String>(7, []),
        )
        .key();
        for other in [
            None,
            Some(session.as_str()),
            Some(other_guard.as_str()),
            Some("7"),
            Some("x:+7"),
        ] {
            assert_eq!(kept_token(other), None, "{other:?}");
        }
    }
}

/// The `personal_access_tokens` table, for the app's migration.
///
/// ```
/// use smeltery::Result;
/// use smeltery::db::migration::{Migration, Schema};
///
/// pub struct CreatePersonalAccessTokensTable;
///
/// impl Migration for CreatePersonalAccessTokensTable {
///     fn name(&self) -> &'static str {
///         "2026_10_05_000000_create_personal_access_tokens_table"
///     }
///
///     async fn up(&self, schema: &Schema) -> Result<()> {
///         smeltery::hallmark::migrations::up(schema).await
///     }
///
///     async fn down(&self, schema: &Schema) -> Result<()> {
///         smeltery::hallmark::migrations::down(schema).await
///     }
/// }
/// ```
///
/// | Column | Type |
/// |---|---|
/// | `id` | 64-bit auto-increment primary key |
/// | `user_id` | the user (`users.id`, deleted with the user), indexed |
/// | `name` | `varchar(255)` |
/// | `token_hash` | `varchar(64)`, unique: the SHA-256 (hex) of the token |
/// | `binding` | `varchar(64)`: a hash of the user's password hash when the token was created |
/// | `abilities` | text: a JSON array |
/// | `last_used_at` | timestamp, nullable |
/// | `expires_at` | timestamp, nullable, indexed |
/// | `created_at` | timestamp |
/// | `updated_at` | timestamp, nullable |
pub mod migrations {
    use smeltery_core::Result;
    use smeltery_core::db::migration::Schema;

    use crate::TABLE;

    /// Create the `personal_access_tokens` table.
    ///
    /// # Errors
    /// The table exists already, `users` does not, or a statement fails.
    pub async fn up(schema: &Schema) -> Result<()> {
        schema
            .create(TABLE, |t| {
                t.id();
                t.foreign_id("user_id")
                    .constrained("users")
                    .cascade_on_delete()
                    .index();
                t.string("name");
                t.string_len("token_hash", 64).unique();
                t.string_len("binding", 64);
                t.text("abilities");
                t.datetime("last_used_at").nullable();
                t.datetime("expires_at").nullable().index();
                t.datetime("created_at");
                t.datetime("updated_at").nullable();
            })
            .await
    }

    /// Drop the table.
    ///
    /// # Errors
    /// A statement fails.
    pub async fn down(schema: &Schema) -> Result<()> {
        schema.drop_if_exists(TABLE).await
    }
}
