//! Two-factor authentication: TOTP codes from an authenticator app (RFC 6238) and single-use recovery codes, the
//! login challenge, enrolment, and core's [`SecondFactor`] for other sign-in
//! endpoints.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use smeltery_core::auth::{
    Auth, AuthUser, Authenticatable, CredentialChange, CredentialListener, CredentialsChanged,
    SecondFactor, SecondFactorVerdict,
};
use smeltery_core::cache::{RateLimit, RateLimiter};
use smeltery_core::console::{Args, Command, Output};
use smeltery_core::crypto::{constant_time_eq, random_bytes, random_token, sha256_hex};
use smeltery_core::db::Record;
use smeltery_core::session::Session;
use smeltery_core::{App, BoxFuture, Error, Result};

pub(crate) mod qr;
pub(crate) mod routes;
pub(crate) mod store;
pub(crate) mod totp;

/// The encryption purpose of two-factor secrets (`App::encrypt`, the user id as associated data).
pub(crate) const PURPOSE: &str = "temper.two-factor";

/// The session key of a pending two-factor login.
pub(crate) const PENDING_KEY: &str = "_temper.login";

/// The session key the plaintext recovery codes are flashed under, for the one page that shows them.
pub const RECOVERY_CODES_KEY: &str = "_temper.recovery_codes";

/// Wrong codes one pending login may take before the password is needed again.
pub(crate) const MAX_PENDING_FAILURES: u32 = 5;

/// A user model with the two-factor columns: `two_factor_secret` and `two_factor_recovery_codes` (nullable text),
/// `two_factor_confirmed_at` (`Option<DateTimeUtc>`) and `two_factor_last_step` (nullable big integer).
///
/// ```
/// # mod user {
/// #     use smeltery::db::prelude::*;
/// #     #[sea_orm::model]
/// #     #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
/// #     #[sea_orm(table_name = "users")]
/// #     pub struct Model {
/// #         #[sea_orm(primary_key)]
/// #         pub id: i64,
/// #         pub password: String,
/// #         pub remember_token: Option<String>,
/// #         #[serde(skip_serializing)]
/// #         pub two_factor_secret: Option<String>,
/// #         #[serde(skip_serializing)]
/// #         pub two_factor_recovery_codes: Option<String>,
/// #         pub two_factor_confirmed_at: Option<DateTimeUtc>,
/// #         pub two_factor_last_step: Option<i64>,
/// #     }
/// #     impl ActiveModelBehavior for ActiveModel {}
/// #     impl smeltery::auth::Authenticatable for Model {
/// #         fn auth_id(&self) -> i64 { self.id }
/// #         fn password_hash(&self) -> &str { &self.password }
/// #         fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
/// #     }
/// use smeltery::db::prelude::DateTimeUtc;
///
/// impl smeltery::temper::TwoFactorAuthenticatable for Model {
///     fn two_factor_secret(&self) -> Option<&str> { self.two_factor_secret.as_deref() }
///     fn two_factor_recovery_codes(&self) -> Option<&str> { self.two_factor_recovery_codes.as_deref() }
///     fn two_factor_confirmed_at(&self) -> Option<DateTimeUtc> { self.two_factor_confirmed_at }
///     fn two_factor_last_step(&self) -> Option<i64> { self.two_factor_last_step }
/// }
/// # }
/// ```
pub trait TwoFactorAuthenticatable: Authenticatable + Record {
    /// The encrypted secret (`two_factor_secret`).
    fn two_factor_secret(&self) -> Option<&str>;
    /// The SHA-256 hashes of the unused recovery codes, as a JSON array (`two_factor_recovery_codes`).
    fn two_factor_recovery_codes(&self) -> Option<&str>;
    /// When the enrolment was confirmed with a first code (`two_factor_confirmed_at`).
    fn two_factor_confirmed_at(&self) -> Option<sea_orm::prelude::DateTimeUtc>;
    /// The last accepted time step (`two_factor_last_step`).
    fn two_factor_last_step(&self) -> Option<i64>;

    /// The account name the authenticator app shows next to the app's name (`APP_NAME`); default: the user id.
    /// Return the e-mail address here.
    fn two_factor_account(&self) -> String {
        self.auth_id().to_string()
    }
}

/// The two-factor options: `Temper::two_factor(TwoFactor::new())`.
///
/// ```
/// use std::time::Duration;
/// use smeltery::temper::TwoFactor;
///
/// let options = TwoFactor::new()
///     .confirm(true)
///     .confirm_password(true)
///     .window(1)
///     .recovery_codes(8)
///     .challenge_ttl(Duration::from_secs(300));
/// # let _ = options;
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TwoFactor {
    pub(crate) confirm: bool,
    pub(crate) confirm_password: bool,
    pub(crate) window: u8,
    pub(crate) recovery_codes: usize,
    pub(crate) challenge_ttl: Duration,
}

impl Default for TwoFactor {
    fn default() -> Self {
        Self::new()
    }
}

impl TwoFactor {
    /// The defaults: confirmation with a first code, password confirmation on the management routes, a window of
    /// one step, eight recovery codes, a five-minute challenge.
    pub fn new() -> Self {
        Self {
            confirm: true,
            confirm_password: true,
            window: 1,
            recovery_codes: 8,
            challenge_ttl: Duration::from_secs(300),
        }
    }

    /// Whether two-factor authentication counts as on only after a first code confirms the enrolment (default
    /// `true`); with `false` it is on as soon as it is enabled.
    #[must_use]
    pub fn confirm(mut self, confirm: bool) -> Self {
        self.confirm = confirm;
        self
    }

    /// Whether the management routes carry the `password.confirm` middleware (default `true`). With `false`, a first
    /// enrolment (enabling, reading its QR code and key, confirming it) needs no password. Everything that touches a
    /// confirmed enrolment needs a password confirmation within `AUTH_PASSWORD_TIMEOUT` either way: its QR code and
    /// key, new recovery codes, turning it off and enabling over it (423 for JSON clients, else the confirmation
    /// page).
    #[must_use]
    pub fn confirm_password(mut self, confirm_password: bool) -> Self {
        self.confirm_password = confirm_password;
        self
    }

    /// Steps of tolerance either side of now (default 1 = ±30 seconds; at most 2).
    #[must_use]
    pub fn window(mut self, window: u8) -> Self {
        self.window = window;
        self
    }

    /// How many recovery codes an enrolment gets (default 8; 4 to 16).
    #[must_use]
    pub fn recovery_codes(mut self, count: usize) -> Self {
        self.recovery_codes = count;
        self
    }

    /// How long a pending login waits for its code (default 5 minutes; 1 to 15 minutes).
    #[must_use]
    pub fn challenge_ttl(mut self, ttl: Duration) -> Self {
        self.challenge_ttl = ttl;
        self
    }

    /// Why these options are invalid, if they are.
    pub(crate) fn problem(&self) -> Option<String> {
        if self.window > 2 {
            return Some(format!("`window({})`: at most 2", self.window));
        }
        if !(4..=16).contains(&self.recovery_codes) {
            return Some(format!(
                "`recovery_codes({})`: 4 to 16",
                self.recovery_codes
            ));
        }
        if !(60..=900).contains(&self.challenge_ttl.as_secs()) {
            return Some(format!(
                "`challenge_ttl({}s)`: 1 to 15 minutes",
                self.challenge_ttl.as_secs()
            ));
        }
        None
    }

    /// Whether `user` must give a code at sign-in.
    pub(crate) fn required<U: TwoFactorAuthenticatable>(&self, user: &U) -> bool {
        user.two_factor_secret().is_some()
            && (!self.confirm || user.two_factor_confirmed_at().is_some())
    }
}

// ---- time ---------------------------------------------------------------------------------------------------------

/// A fixed time for tests ([`testing::fake_clock`](crate::testing::fake_clock)).
pub(crate) struct FakeClock(pub(crate) AtomicI64);

/// Unix seconds now: the app's fake clock when a test set one, else the system clock.
pub(crate) fn now(app: &App) -> i64 {
    if let Some(fake) = app.service::<FakeClock>() {
        return fake.0.load(Ordering::SeqCst);
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

// ---- secrets and codes --------------------------------------------------------------------------------------------

/// A new secret: 20 random bytes (160 bits), as base32.
pub(crate) fn new_secret() -> Result<String> {
    Ok(totp::base32(&random_bytes(20)?))
}

/// The secret encrypted for user `id` (the id is the associated data: a copy in another row does not open).
pub(crate) fn seal(app: &App, id: i64, secret: &str) -> Result<String> {
    app.encrypt(PURPOSE, id.to_string().as_bytes(), secret.as_bytes())
}

/// The secret bytes of `user`, or `None` when there is none or it does not decrypt (another `APP_KEY`, another
/// row's value): fail closed, logged once per user per hour.
pub(crate) async fn secret_of<U: TwoFactorAuthenticatable>(
    app: &App,
    user: &U,
) -> Result<Option<Vec<u8>>> {
    let Some(sealed) = user.two_factor_secret() else {
        return Ok(None);
    };
    let opened = app
        .decrypt(PURPOSE, user.auth_id().to_string().as_bytes(), sealed)?
        .and_then(|plain| String::from_utf8(plain).ok())
        .and_then(|text| totp::base32_decode(&text));
    if opened.is_none() {
        static LOG: LazyLock<RateLimiter> = LazyLock::new(|| {
            RateLimiter::new("temper.two-factor.unreadable", 1, Duration::from_secs(3600))
        });
        if LOG
            .hit(app, &format!("user:{}", user.auth_id()))
            .await
            .is_ok_and(RateLimit::allowed)
        {
            tracing::error!(
                user_id = user.auth_id(),
                "the two-factor secret cannot be decrypted; APP_KEY changed?"
            );
        }
    }
    Ok(opened)
}

/// The base32 secret of `user` (for the manual-entry key and the QR code).
pub(crate) async fn secret_text<U: TwoFactorAuthenticatable>(
    app: &App,
    user: &U,
) -> Result<Option<String>> {
    Ok(secret_of(app, user)
        .await?
        .map(|bytes| totp::base32(&bytes)))
}

/// `count` new recovery codes (`xxxxxxxxxx-xxxxxxxxxx`) and their stored form (a JSON array of SHA-256 hex).
pub(crate) fn new_codes(count: usize) -> Result<(Vec<String>, String)> {
    let codes = (0..count)
        .map(|_| Ok(format!("{}-{}", random_token(10)?, random_token(10)?)))
        .collect::<Result<Vec<_>>>()?;
    let hashes: Vec<String> = codes.iter().map(|c| sha256_hex(c)).collect();
    Ok((codes, serde_json::to_string(&hashes)?))
}

fn stored_hashes(stored: Option<&str>) -> Vec<String> {
    stored
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

/// How many unused recovery codes `user` has.
pub(crate) fn codes_left<U: TwoFactorAuthenticatable>(user: &U) -> usize {
    stored_hashes(user.two_factor_recovery_codes()).len()
}

// ---- checking a code ----------------------------------------------------------------------------------------------

/// Which kind of code was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodeKind {
    /// A code from the authenticator app (field `code`).
    Totp,
    /// A recovery code (field `recovery_code`).
    Recovery,
}

impl CodeKind {
    pub(crate) fn field(self) -> &'static str {
        match self {
            Self::Totp => "code",
            Self::Recovery => "recovery_code",
        }
    }
}

/// The outcome of [`check`].
pub(crate) enum Checked {
    /// The code was right (and is now used up); for a recovery code, how many are left.
    Accepted { recovery_left: Option<usize> },
    /// Wrong, used, or the secret cannot be read.
    Wrong,
    /// Over the account's budget.
    Limited { retry_after: u64 },
}

/// Codes per account: five per five minutes.
static SHORT: LazyLock<RateLimiter> =
    LazyLock::new(|| RateLimiter::new("temper.two-factor.short", 5, Duration::from_secs(300)));
/// Recovery codes per account: ten an hour. Apart from the app-code budgets, so a password holder who burns those
/// never blocks the owner's recovery codes (~119 bits each: unguessable at this rate).
static RECOVERY: LazyLock<RateLimiter> =
    LazyLock::new(|| RateLimiter::new("temper.two-factor.recovery", 10, Duration::from_secs(3600)));
/// Codes per account: one hundred a day.
static DAILY: LazyLock<RateLimiter> =
    LazyLock::new(|| RateLimiter::new("temper.two-factor.daily", 100, Duration::from_secs(86_400)));

/// Count a code against `user`'s budgets (before any check; a cache failure fails closed), then check it. A
/// TOTP code is accepted only for a step later than the last accepted one (one conditional update), a recovery code
/// is compared against every stored hash in constant time and removed by compare-and-set. A success clears the
/// account's budgets.
pub(crate) async fn check<U: TwoFactorAuthenticatable>(
    app: &App,
    options: &TwoFactor,
    user: &U,
    code: &str,
    kind: CodeKind,
) -> Result<Checked> {
    let key = format!("user:{}", user.auth_id());
    let budgets: &[&RateLimiter] = match kind {
        CodeKind::Totp => &[&SHORT, &DAILY],
        CodeKind::Recovery => &[&RECOVERY],
    };
    for limiter in budgets {
        if let RateLimit::Limited { retry_after, .. } = limiter.hit(app, &key).await? {
            return Ok(Checked::Limited { retry_after });
        }
    }
    let accepted = match kind {
        CodeKind::Totp => {
            let Some(secret) = secret_of(app, user).await? else {
                return Ok(Checked::Wrong);
            };
            let current = totp::step(now(app));
            match totp::matching_step(&secret, code, current, options.window) {
                Some(step) => store::accept_step::<U>(app, user.auth_id(), step)
                    .await?
                    .then_some(None),
                None => None,
            }
        }
        CodeKind::Recovery => {
            let stored = user.two_factor_recovery_codes().unwrap_or("");
            let hashes = stored_hashes(Some(stored));
            let given = sha256_hex(code.trim());
            let mut found = None;
            // Every stored hash is compared (no early exit).
            for (index, hash) in hashes.iter().enumerate() {
                if constant_time_eq(hash, &given) {
                    found = Some(index);
                }
            }
            match found {
                Some(index) if !code.trim().is_empty() => {
                    let left: Vec<&String> = hashes
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != index)
                        .map(|(_, h)| h)
                        .collect();
                    let count = left.len();
                    let swapped = store::swap_codes::<U>(
                        app,
                        user.auth_id(),
                        stored,
                        serde_json::to_string(&left)?,
                    )
                    .await?;
                    swapped.then_some(Some(count))
                }
                _ => None,
            }
        }
    };
    match accepted {
        Some(recovery_left) => {
            for limiter in [&*SHORT, &*DAILY, &*RECOVERY] {
                limiter.clear(app, &key).await?;
            }
            Ok(Checked::Accepted { recovery_left })
        }
        None => Ok(Checked::Wrong),
    }
}

/// The user with id `id`, read again (two-factor columns change during a request).
pub(crate) async fn fresh<U: TwoFactorAuthenticatable>(app: &App, id: i64) -> Result<Option<U>> {
    Ok(app.find_user(id).await?.and_then(|u| u.downcast::<U>()))
}

// ---- the pending login --------------------------------------------------------------------------------------------

/// A login waiting for its second factor, kept in the session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Pending {
    pub(crate) id: i64,
    pub(crate) remember: bool,
    /// The user's binding for this purpose: a password change in between ends the pending login.
    pub(crate) binding: String,
    pub(crate) issued: i64,
    pub(crate) failures: u32,
}

/// The binding purpose of a pending login.
pub(crate) const PENDING_PURPOSE: &str = "temper.two-factor-login";

/// Start a pending login for `user` in this session (a new session id first).
pub(crate) async fn start_pending(
    app: &App,
    session: &Session,
    user_id: i64,
    remember: bool,
) -> Result<()> {
    let user = app
        .find_user(user_id)
        .await?
        .ok_or_else(|| Error::internal("the user signing in is gone"))?;
    session.regenerate();
    session.insert(
        PENDING_KEY,
        Pending {
            id: user_id,
            remember,
            binding: user.binding(PENDING_PURPOSE)?,
            issued: now(app),
            failures: 0,
        },
    );
    Ok(())
}

/// The pending login of this session and its user when it is still valid: younger than the challenge time, the
/// user still there and the password unchanged since. An invalid one is removed.
pub(crate) async fn pending<U: TwoFactorAuthenticatable>(
    app: &App,
    session: &Session,
    options: &TwoFactor,
) -> Result<Option<(Pending, U)>> {
    let Some(pending) = session.get::<Pending>(PENDING_KEY) else {
        return Ok(None);
    };
    let ttl = i64::try_from(options.challenge_ttl.as_secs()).unwrap_or(300);
    let age = now(app).saturating_sub(pending.issued);
    let user = app.find_user(pending.id).await?;
    let valid = match &user {
        Some(user) if (0..ttl).contains(&age) => {
            constant_time_eq(&user.binding(PENDING_PURPOSE)?, &pending.binding)
        }
        _ => false,
    };
    match user.and_then(|u| u.downcast::<U>()) {
        Some(user) if valid => Ok(Some((pending, user))),
        _ => {
            session.remove(PENDING_KEY);
            Ok(None)
        }
    }
}

// ---- status for settings pages -------------------------------------------------------------------------------------

/// The signed-in user's two-factor state, for a settings page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TwoFactorStatus {
    /// A secret is stored (enabled, confirmed or not).
    pub enabled: bool,
    /// The enrolment was confirmed with a first code.
    pub confirmed: bool,
    /// Unused recovery codes.
    pub recovery_codes_left: usize,
    /// The recovery codes just generated (flashed once, after enabling or regenerating; `None` otherwise).
    pub new_recovery_codes: Option<Vec<String>>,
}

/// The two-factor state of the signed-in user (`None` for a guest).
///
/// # Errors
/// Loading the user fails, or `U` is not the registered model.
pub async fn status<U: TwoFactorAuthenticatable>(
    auth: &Auth,
    session: &Session,
) -> Result<Option<TwoFactorStatus>> {
    let Some(user) = auth.user::<U>().await? else {
        return Ok(None);
    };
    Ok(Some(TwoFactorStatus {
        enabled: user.two_factor_secret().is_some(),
        confirmed: user.two_factor_confirmed_at().is_some(),
        recovery_codes_left: codes_left(&user),
        new_recovery_codes: session.get::<Vec<String>>(RECOVERY_CODES_KEY),
    }))
}

/// What an authenticator app needs to add the signed-in user's enrolment: the QR code and the key for manual entry,
/// for a settings page rendered on the server (the routes `two-factor.qr-code` and `two-factor.secret-key` answer
/// the same as JSON).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TwoFactorSetup {
    /// The QR code as `data:image/svg+xml;base64,…`, for an `<img src>` (an SVG made only of numbers).
    pub qr_code_url: String,
    /// The secret in base32, for typing into the app.
    pub secret_key: String,
}

/// The QR code and key of the signed-in user's enrolment (`None`: a guest, no enrolment, or a secret that does not
/// decrypt). As on the JSON routes, a confirmed enrolment's secret needs a password confirmation within
/// `AUTH_PASSWORD_TIMEOUT`; put the page that shows it behind the `password.confirm` middleware.
///
/// # Errors
/// Loading the user fails, `U` is not the registered model, or the QR code cannot be drawn.
pub async fn setup<U: TwoFactorAuthenticatable>(
    app: &App,
    auth: &Auth,
) -> Result<Option<TwoFactorSetup>> {
    let Some(user) = auth.user::<U>().await? else {
        return Ok(None);
    };
    if user.two_factor_confirmed_at().is_some()
        && !auth.password_confirmed_within(app.settings().password_timeout)
    {
        return Ok(None);
    }
    let Some(secret) = secret_text(app, &user).await? else {
        return Ok(None);
    };
    let uri = qr::otpauth_uri(&app.settings().name, &user.two_factor_account(), &secret);
    Ok(Some(TwoFactorSetup {
        qr_code_url: qr::data_uri(&qr::svg(&uri)?),
        secret_key: secret,
    }))
}

// ---- core seams ---------------------------------------------------------------------------------------------------

/// Core's [`SecondFactor`]: other sign-in endpoints (token issuing) ask it after the password.
pub(crate) struct Factor<U> {
    pub(crate) options: TwoFactor,
    pub(crate) _user: std::marker::PhantomData<fn() -> U>,
}

impl<U: TwoFactorAuthenticatable> SecondFactor for Factor<U> {
    fn required<'a>(&'a self, _app: &'a App, user: &'a AuthUser) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            Ok(user
                .downcast::<U>()
                .is_some_and(|u| self.options.required(&u)))
        })
    }

    fn verify<'a>(
        &'a self,
        app: &'a App,
        user: &'a AuthUser,
        code: &'a str,
    ) -> BoxFuture<'a, Result<SecondFactorVerdict>> {
        Box::pin(async move {
            let Some(user) = fresh::<U>(app, user.id()).await? else {
                return Ok(SecondFactorVerdict::Invalid);
            };
            if !self.options.required(&user) {
                return Ok(SecondFactorVerdict::Invalid);
            }
            let trimmed = code.trim();
            let kind = if trimmed.len() == 21 && trimmed.contains('-') {
                CodeKind::Recovery
            } else {
                CodeKind::Totp
            };
            Ok(
                match check(app, &self.options, &user, trimmed, kind).await? {
                    Checked::Accepted { .. } => SecondFactorVerdict::Valid,
                    Checked::Wrong => SecondFactorVerdict::Invalid,
                    // The budget is shared with the web challenge: a spent one answers 429 everywhere.
                    Checked::Limited { retry_after } => {
                        SecondFactorVerdict::TooManyAttempts { retry_after }
                    }
                },
            )
        })
    }
}

/// Removes a two-factor enrolment added while the address was unverified, when a reset proves control of it
/// (someone who registered the address first may have enrolled their own device).
pub(crate) struct ResetListener<U>(pub(crate) std::marker::PhantomData<fn() -> U>);

impl<U: TwoFactorAuthenticatable> CredentialListener for ResetListener<U> {
    fn credentials_changed<'a>(
        &'a self,
        app: &'a App,
        change: &'a CredentialsChanged,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if change.why == CredentialChange::Reset && change.was_unverified {
                store::disable::<U>(app, change.user_id).await?;
                tracing::info!(
                    user_id = change.user_id,
                    "two-factor authentication removed: a reset verified the address"
                );
            }
            Ok(())
        })
    }
}

/// `temper:two-factor-disable <email> --force`: an operator turns off two-factor authentication for a user who lost
/// the device and the recovery codes (the four columns cleared, the remember token replaced, `TwoFactorDisabled`
/// fired). Without `--force` it changes nothing and says so. `Temper::two_factor` registers it.
pub struct DisableCommand<U>(std::marker::PhantomData<fn() -> U>);

impl<U> DisableCommand<U> {
    /// The command for the user model `U`.
    pub fn new() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<U> Default for DisableCommand<U> {
    fn default() -> Self {
        Self::new()
    }
}

impl<U> std::fmt::Debug for DisableCommand<U> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DisableCommand")
    }
}

impl<U: TwoFactorAuthenticatable> Command for DisableCommand<U> {
    fn name(&self) -> &'static str {
        "temper:two-factor-disable"
    }

    fn about(&self) -> &'static str {
        "Turn off two-factor authentication for a user (<email> --force)"
    }

    async fn run(&self, app: &App, args: Args) -> Result<()> {
        self.run_with_output(app, args, Output::default()).await
    }

    async fn run_with_output(&self, app: &App, args: Args, out: Output) -> Result<()> {
        let Some(email) = args.get(0) else {
            return Err(Error::bad_request(
                "usage: temper:two-factor-disable <email> --force",
            ));
        };
        let Some(user) = smeltery_core::auth::find_by_email::<U>(app, email).await? else {
            return Err(Error::not_found());
        };
        if user.two_factor_secret().is_none() {
            out.line("Two-factor authentication is not enabled for this user.");
            return Ok(());
        }
        if !args.flag("force") {
            out.line(
                "Nothing changed: add --force to turn off two-factor authentication for this user.",
            );
            return Ok(());
        }
        store::disable::<U>(app, user.auth_id()).await?;
        smeltery_core::auth::cycle_remember_token(app, user.auth_id()).await?;
        tracing::info!(
            user_id = user.auth_id(),
            "two-factor authentication turned off by an operator"
        );
        if let Some(shared) = app.service::<std::sync::Arc<crate::Shared<U>>>() {
            let user_id = user.auth_id();
            crate::events::fire(
                app,
                &shared.listeners,
                crate::TemperEvent::TwoFactorDisabled { user_id },
            )
            .await;
        }
        out.line("Two-factor authentication is turned off for this user.");
        Ok(())
    }
}
