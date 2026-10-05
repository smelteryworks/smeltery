//! The settings pages: profile, password and two-factor authentication. Their forms post to Temper's routes
//! (`PUT /user/profile-information`, `PUT /user/password`, `/user/two-factor-…`), which answer back here.

use smeltery::auth::Auth;
use smeltery::http::{HeaderValue, IntoResponse, header};
use smeltery::session::Session;
use smeltery::temper::two_factor;
use smeltery::{App, Error, Response, Result};

use crate::app::models::User;

/// `resources/views/settings/profile.mold.html`.
#[derive(smeltery::Mold)]
#[mold("settings/profile")]
pub struct ProfilePage {
    pub name: String,
    pub email: String,
}

/// `resources/views/settings/password.mold.html`.
#[derive(smeltery::Mold)]
#[mold("settings/password")]
pub struct PasswordPage {}

/// `resources/views/settings/two-factor.mold.html`.
#[derive(smeltery::Mold)]
#[mold("settings/two-factor")]
pub struct TwoFactorPage {
    /// A secret is stored (confirmed or not).
    pub enabled: bool,
    /// A first code confirmed the enrolment: logins ask for a code.
    pub confirmed: bool,
    /// Unused recovery codes.
    pub recovery_codes_left: usize,
    /// The recovery codes just made, shown once (empty otherwise).
    pub recovery_codes: Vec<String>,
    /// While enrolling: the QR code (`data:image/svg+xml;base64,…`) and the key for typing in.
    pub qr_code_url: String,
    pub secret_key: String,
}

/// A page that nobody caches: it shows account data, and the two-factor page secrets.
fn private(page: impl IntoResponse) -> Response {
    let mut response = page.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /settings/profile`.
pub async fn profile(auth: Auth) -> Result<Response> {
    let user = auth.user::<User>().await?.ok_or_else(Error::unauthorized)?;
    Ok(private(ProfilePage {
        name: user.name,
        email: user.email,
    }))
}

/// `GET /settings/password`.
pub async fn password() -> Response {
    private(PasswordPage {})
}

/// `GET /settings/two-factor` (behind `password.confirm`: it shows the key while enrolling).
pub async fn two_factor(app: App, auth: Auth, session: Session) -> Result<Response> {
    let status = two_factor::status::<User>(&auth, &session)
        .await?
        .ok_or_else(Error::unauthorized)?;
    let setup = if status.enabled && !status.confirmed {
        two_factor::setup::<User>(&app, &auth).await?
    } else {
        None
    };
    let (qr_code_url, secret_key) = setup
        .map(|s| (s.qr_code_url, s.secret_key))
        .unwrap_or_default();
    Ok(private(TwoFactorPage {
        enabled: status.enabled,
        confirmed: status.confirmed,
        recovery_codes_left: status.recovery_codes_left,
        recovery_codes: status.new_recovery_codes.unwrap_or_default(),
        qr_code_url,
        secret_key,
    }))
}
