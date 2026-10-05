//! `GET /login` shows the form, `POST /login` logs in, `POST /logout` logs out.

use smeltery::auth::Auth;
use smeltery::http::Redirect;
use smeltery::session::Session;
use smeltery::validation::Valid;
use smeltery::{App, Result, Validate};

/// `resources/views/auth/login.mold.html`.
#[derive(smeltery::Mold)]
#[mold("auth/login")]
pub struct LoginPage {}

/// The login form.
#[derive(Debug, serde::Deserialize, Validate)]
pub struct LoginForm {
    #[validate(required, email)]
    pub email: String,
    #[validate(required)]
    pub password: String,
    /// The "remember me" checkbox.
    #[serde(default)]
    pub remember: bool,
}

/// Shows the login form.
pub async fn create() -> LoginPage {
    LoginPage {}
}

/// Checks the credentials: the dashboard on success, back to the form with an error otherwise.
pub async fn store(
    app: App,
    auth: Auth,
    session: Session,
    Valid(form): Valid<LoginForm>,
) -> Result<Redirect> {
    if auth
        .attempt(&form.email, &form.password, form.remember)
        .await?
    {
        return Ok(Redirect::to(&app.url("dashboard", &[])?));
    }
    session.flash("error", "These credentials do not match our records.");
    Ok(Redirect::to(&app.url("login", &[])?))
}

/// Logs out and returns to the home page.
pub async fn destroy(app: App, auth: Auth) -> Result<Redirect> {
    auth.logout().await?;
    Ok(Redirect::to(&app.url("home", &[])?))
}
