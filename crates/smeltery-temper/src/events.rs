//! Temper's events and the listeners an app registers with [`Temper::listen`](crate::Temper::listen).

use std::sync::Arc;

use serde::Serialize;
use smeltery_core::{App, BoxFuture, Result};

/// Something that happened in a Temper flow. Events carry user ids (and for failed logins a keyed hash of the
/// normalized address), never passwords, tokens or addresses.
///
/// Signing out and password changes also reach every other credential through core's
/// [`AuthEvent`](smeltery_core::auth::AuthEvent)s; Temper's events are for the app (audit logs, welcome mails).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TemperEvent {
    /// A user registered (and is signed in).
    #[non_exhaustive]
    Registered {
        /// The new user.
        user_id: i64,
    },
    /// A user signed in.
    #[non_exhaustive]
    Login {
        /// The user.
        user_id: i64,
        /// Whether the sign-in passed a second factor.
        two_factor: bool,
        /// Whether a remember-me cookie was issued.
        remember: bool,
    },
    /// A login with a wrong address or password.
    #[non_exhaustive]
    Failed {
        /// HMAC-SHA256 of the normalized address that was typed, under a key derived from `APP_KEY` (purpose
        /// `temper.email-hash`), as 43 characters of unpadded base64url: the same address gives the same value
        /// within one app.
        email_hash: String,
    },
    /// A login refused because the login budgets are used up.
    #[non_exhaustive]
    Lockout {
        /// HMAC-SHA256 of the normalized address that was typed, under a key derived from `APP_KEY` (purpose
        /// `temper.email-hash`), as 43 characters of unpadded base64url: the same address gives the same value
        /// within one app.
        email_hash: String,
    },
    /// A user signed out.
    #[non_exhaustive]
    Logout {
        /// The user.
        user_id: i64,
    },
    /// A reset link was issued (the answer to the request is the same whether or not one was).
    #[non_exhaustive]
    PasswordResetLinkSent {
        /// The user.
        user_id: i64,
    },
    /// A password was reset through a reset link.
    #[non_exhaustive]
    PasswordReset {
        /// The user.
        user_id: i64,
    },
    /// A signed-in user changed their password.
    #[non_exhaustive]
    PasswordUpdated {
        /// The user.
        user_id: i64,
    },
    /// A signed-in user changed their profile.
    #[non_exhaustive]
    ProfileUpdated {
        /// The user.
        user_id: i64,
        /// Whether the e-mail address changed.
        email_changed: bool,
    },
    /// A user verified their e-mail address.
    #[non_exhaustive]
    Verified {
        /// The user.
        user_id: i64,
    },
    /// A login with the right password now waits for the second factor.
    #[non_exhaustive]
    TwoFactorChallenged {
        /// The user.
        user_id: i64,
    },
    /// A wrong two-factor or recovery code.
    #[non_exhaustive]
    TwoFactorFailed {
        /// The user.
        user_id: i64,
    },
    /// A code refused because the account's two-factor budget is used up.
    #[non_exhaustive]
    TwoFactorLockout {
        /// The user.
        user_id: i64,
    },
    /// Two-factor authentication was enabled (a new secret and recovery codes).
    #[non_exhaustive]
    TwoFactorEnabled {
        /// The user.
        user_id: i64,
    },
    /// The enrolment was confirmed with a first code.
    #[non_exhaustive]
    TwoFactorConfirmed {
        /// The user.
        user_id: i64,
    },
    /// Two-factor authentication was turned off.
    #[non_exhaustive]
    TwoFactorDisabled {
        /// The user.
        user_id: i64,
    },
    /// New recovery codes replaced the old ones.
    #[non_exhaustive]
    RecoveryCodesGenerated {
        /// The user.
        user_id: i64,
    },
    /// A recovery code was used (and is gone).
    #[non_exhaustive]
    RecoveryCodeUsed {
        /// The user.
        user_id: i64,
        /// Recovery codes left.
        left: usize,
    },
    /// A signed-in user confirmed their password.
    #[non_exhaustive]
    PasswordConfirmed {
        /// The user.
        user_id: i64,
    },
}

impl TemperEvent {
    /// The user the event is about (`None` for failed logins and lockouts).
    pub fn user_id(&self) -> Option<i64> {
        match self {
            Self::Registered { user_id }
            | Self::Login { user_id, .. }
            | Self::Logout { user_id }
            | Self::PasswordResetLinkSent { user_id }
            | Self::PasswordReset { user_id }
            | Self::PasswordUpdated { user_id }
            | Self::ProfileUpdated { user_id, .. }
            | Self::Verified { user_id }
            | Self::PasswordConfirmed { user_id }
            | Self::TwoFactorChallenged { user_id }
            | Self::TwoFactorFailed { user_id }
            | Self::TwoFactorLockout { user_id }
            | Self::TwoFactorEnabled { user_id }
            | Self::TwoFactorConfirmed { user_id }
            | Self::TwoFactorDisabled { user_id }
            | Self::RecoveryCodesGenerated { user_id }
            | Self::RecoveryCodeUsed { user_id, .. } => Some(*user_id),
            Self::Failed { .. } | Self::Lockout { .. } => None,
        }
    }

    /// The event's name, as in its JSON `type` (`login`, `password_reset`, …).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Registered { .. } => "registered",
            Self::Login { .. } => "login",
            Self::Failed { .. } => "failed",
            Self::Lockout { .. } => "lockout",
            Self::Logout { .. } => "logout",
            Self::PasswordResetLinkSent { .. } => "password_reset_link_sent",
            Self::PasswordReset { .. } => "password_reset",
            Self::PasswordUpdated { .. } => "password_updated",
            Self::ProfileUpdated { .. } => "profile_updated",
            Self::Verified { .. } => "verified",
            Self::PasswordConfirmed { .. } => "password_confirmed",
            Self::TwoFactorChallenged { .. } => "two_factor_challenged",
            Self::TwoFactorFailed { .. } => "two_factor_failed",
            Self::TwoFactorLockout { .. } => "two_factor_lockout",
            Self::TwoFactorEnabled { .. } => "two_factor_enabled",
            Self::TwoFactorConfirmed { .. } => "two_factor_confirmed",
            Self::TwoFactorDisabled { .. } => "two_factor_disabled",
            Self::RecoveryCodesGenerated { .. } => "recovery_codes_generated",
            Self::RecoveryCodeUsed { .. } => "recovery_code_used",
        }
    }
}

/// A registered listener.
pub(crate) type Listener =
    Arc<dyn Fn(App, TemperEvent) -> BoxFuture<'static, Result<()>> + Send + Sync>;

/// Hand `event` to the listeners, once each, after the answer is decided. They run on the app's owned tasks (the
/// answer never waits for them); under `APP_ENV=testing` they run before this returns, so tests see them. An error
/// or a panic in a listener is logged at `warn` and changes nothing else.
pub(crate) async fn fire(app: &App, listeners: &[Listener], event: TemperEvent) {
    if listeners.is_empty() {
        return;
    }
    let run = run_all(app.clone(), listeners.to_vec(), event);
    if app.settings().env == "testing" {
        run.await;
    } else {
        app.spawn_owned(run);
    }
}

async fn run_all(app: App, listeners: Vec<Listener>, event: TemperEvent) {
    let name = event.name();
    for listener in listeners {
        // Its own task, awaited here: a panic ends that task only and is reported as a `JoinError`.
        let (app, event) = (app.clone(), event.clone());
        // The listener is called inside the task: a panic before its future exists is caught too.
        match tokio::spawn(async move { listener(app, event).await }).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(event = name, error = %e, "a Temper listener failed"),
            Err(_) => tracing::warn!(event = name, "a Temper listener panicked"),
        }
    }
}
