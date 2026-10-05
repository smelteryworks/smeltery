//! Test helpers for apps with Temper: sign in and confirm the password through Temper's routes, and record the
//! events a test caused.
//!
//! ```
//! use smeltery::temper::testing::EventRecorder;
//! use smeltery::temper::TemperEvent;
//!
//! let recorder = EventRecorder::new();
//! // `Temper::new()….listen(recorder.listener())` in the app under test, then after some requests:
//! assert!(recorder.events().is_empty());
//! assert_eq!(recorder.count(|e| matches!(e, TemperEvent::Login { .. })), 0);
//! ```

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use smeltery_core::testing::{TestApp, TestResponse};
use smeltery_core::{App, Result};

use crate::{TemperEvent, TwoFactorAuthenticatable};

/// `POST /login` with this address and password (the route named `login.store`, so a prefix is followed).
pub fn log_in(app: &TestApp, email: &str, password: &str) -> TestResponse {
    let path = route(app, "login.store", "/login");
    app.post_form(&path, &[("email", email), ("password", password)])
}

/// `POST /user/confirm-password` with this password (the route named `password.confirm.store`).
pub fn confirm_password(app: &TestApp, password: &str) -> TestResponse {
    let path = route(app, "password.confirm.store", "/user/confirm-password");
    app.post_form(&path, &[("password", password)])
}

fn route(app: &TestApp, name: &str, fallback: &str) -> String {
    app.app()
        .url(name, &[])
        .unwrap_or_else(|_| fallback.to_owned())
}

/// Keeps every [`TemperEvent`] the app fires, for assertions: `Temper::new()….listen(recorder.listener())`.
/// Under `APP_ENV=testing` (every `TestApp`) listeners run before the answer returns, so the events of a request
/// are there when the request call returns.
#[derive(Clone, Debug, Default)]
pub struct EventRecorder {
    events: Arc<Mutex<Vec<TemperEvent>>>,
}

impl EventRecorder {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// A listener that records into this recorder.
    pub fn listener(
        &self,
    ) -> impl Fn(App, TemperEvent) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send>>
    + Send
    + Sync
    + 'static {
        let events = Arc::clone(&self.events);
        move |_app, event| {
            events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event);
            Box::pin(async { Ok(()) })
        }
    }

    /// Every event so far, in order.
    pub fn events(&self) -> Vec<TemperEvent> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// How many events so far match `pick`.
    pub fn count(&self, pick: impl Fn(&TemperEvent) -> bool) -> usize {
        self.events().iter().filter(|e| pick(e)).count()
    }

    /// Forget the events so far.
    pub fn clear(&self) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

/// Fix the time Temper's two-factor code reads (TOTP steps, the challenge's expiry) at `unix_seconds`; call again to
/// move it. Without it the system clock is used.
///
/// # Panics
/// The app does not run under `APP_ENV=testing` (a frozen clock would let codes and pending logins outlive their
/// time).
#[allow(clippy::panic)]
pub fn fake_clock(app: &App, unix_seconds: i64) {
    if app.settings().env != "testing" {
        panic!("`fake_clock` works only under APP_ENV=testing");
    }
    match app.service::<crate::two_factor::FakeClock>() {
        Some(clock) => clock
            .0
            .store(unix_seconds, std::sync::atomic::Ordering::SeqCst),
        None => app.insert_service(crate::two_factor::FakeClock(
            std::sync::atomic::AtomicI64::new(unix_seconds),
        )),
    }
}

/// The current TOTP code of user `user_id` (from the stored, decrypted secret), as an authenticator app shows it.
///
/// # Panics
/// The user or their secret is missing or unreadable.
pub fn two_factor_code<U: TwoFactorAuthenticatable>(app: &TestApp, user_id: i64) -> String {
    two_factor_code_at::<U>(app, user_id, 0)
}

/// The TOTP code of user `user_id` `steps` steps of 30 seconds from now (`1`: the next code).
///
/// # Panics
/// The user or their secret is missing or unreadable.
#[allow(clippy::panic)]
pub fn two_factor_code_at<U: TwoFactorAuthenticatable>(
    app: &TestApp,
    user_id: i64,
    steps: i64,
) -> String {
    let test = app;
    let app = test.app();
    let secret = block(test, async {
        let user = crate::two_factor::fresh::<U>(app, user_id).await?;
        match user {
            Some(user) => crate::two_factor::secret_of(app, &user).await,
            None => Ok(None),
        }
    })
    .unwrap_or_else(|| panic!("user {user_id} has no readable two-factor secret"));
    let now = crate::two_factor::totp::step(crate::two_factor::now(app));
    crate::two_factor::totp::code_at(&secret, now + steps)
}

/// Enable and confirm two-factor authentication for user `user_id` (eight recovery codes); the recovery codes in
/// clear.
///
/// # Panics
/// Storing fails.
#[allow(clippy::panic)]
pub fn enable_two_factor<U: TwoFactorAuthenticatable>(app: &TestApp, user_id: i64) -> Vec<String> {
    let test = app;
    let app = test.app();
    block(test, async {
        let secret = crate::two_factor::new_secret()?;
        let (codes, stored) = crate::two_factor::new_codes(8)?;
        let sealed = crate::two_factor::seal(app, user_id, &secret)?;
        let now = crate::two_factor::now(app);
        crate::two_factor::store::enable::<U>(app, user_id, sealed, stored, Some(now)).await?;
        Ok(Some(codes))
    })
    .unwrap_or_else(|| panic!("two-factor authentication could not be enabled for user {user_id}"))
}

/// Whether the test client's session holds a pending two-factor login (the challenge page answers 200).
pub fn has_pending_two_factor(app: &TestApp) -> bool {
    let path = route(app, "two-factor.login", "/two-factor-challenge");
    app.get(&path).status() == 200
}

/// Run `future` on the test app's runtime.
fn block<T>(app: &TestApp, future: impl Future<Output = Result<Option<T>>>) -> Option<T> {
    app.block_on(future).ok().flatten()
}

/// The TOTP code of a base32 `secret` at Unix time `unix_seconds` (RFC 6238: HMAC-SHA1, six digits, 30 seconds), as
/// an authenticator app computes it.
pub fn code_for_secret(secret: &str, unix_seconds: i64) -> Option<String> {
    let bytes = crate::two_factor::totp::base32_decode(secret)?;
    Some(crate::two_factor::totp::code_at(
        &bytes,
        crate::two_factor::totp::step(unix_seconds),
    ))
}
