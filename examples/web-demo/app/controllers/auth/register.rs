//! `GET /register` shows the form; `POST /register` creates the account and logs it in.

use smeltery::auth::{Auth, hash_password};
use smeltery::db::prelude::*;
use smeltery::http::Redirect;
use smeltery::validation::Valid;
use smeltery::{App, Result, Validate};

use crate::app::models::{User, user};

/// `resources/views/auth/register.mold.html`.
#[derive(smeltery::Mold)]
#[mold("auth/register")]
pub struct RegisterPage {}

/// The registration form.
#[derive(Debug, Deserialize, Validate)]
pub struct RegisterForm {
    #[validate(required, max = 255)]
    pub name: String,
    #[validate(required, email, max = 255, unique(table = "users", column = "email"))]
    pub email: String,
    #[validate(required, min = 8, confirmed)]
    pub password: String,
    pub password_confirmation: Option<String>,
}

/// Shows the registration form.
pub async fn create() -> RegisterPage {
    RegisterPage {}
}

/// Creates the user, logs them in and opens the dashboard.
pub async fn store(
    app: App,
    db: Db,
    auth: Auth,
    Valid(form): Valid<RegisterForm>,
) -> Result<Redirect> {
    let user = User::create(
        &db,
        user::ActiveModel {
            name: Set(form.name),
            email: Set(form.email),
            password: Set(hash_password(&form.password).await?),
            ..Default::default()
        },
    )
    .await?;
    auth.login(&user, false).await?;
    Ok(Redirect::to(&app.url("dashboard", &[])?))
}
