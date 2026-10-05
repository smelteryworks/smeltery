//! Test helpers: real tokens for a [`TestApp`], checked by the real guard.
//!
//! ```
//! use smeltery::hallmark::testing::{acting_as, token_for};
//! # fn demo<U: smeltery::auth::Authenticatable>(app: &smeltery::testing::TestApp, user: &U) {
//! acting_as(app, user, &["orders:read"]); // every following request sends this token
//! assert_eq!(app.get_json("/api/orders").status(), 200);
//! let other = token_for(app, user, &["*"]); // a token string, the app's headers unchanged
//! # let _ = other;
//! # }
//! ```

use smeltery_core::auth::Authenticatable;
use smeltery_core::testing::TestApp;

use crate::tokens::Tokens;

/// Create a real token for `user` with `abilities` and send it as `Authorization: Bearer …` with every following
/// request of `app` ([`TestApp::with_bearer`]). Returns the plain token.
///
/// # Panics
/// When Hallmark is not installed or the token cannot be created (a test cannot go on then).
pub fn acting_as<U: Authenticatable>(app: &TestApp, user: &U, abilities: &[&str]) -> String {
    let token = token_for(app, user, abilities);
    app.with_bearer(&token);
    token
}

/// Create a real token for `user` with `abilities` and return it, without changing `app`'s headers.
///
/// # Panics
/// When Hallmark is not installed or the token cannot be created.
#[allow(clippy::panic)]
pub fn token_for<U: Authenticatable>(app: &TestApp, user: &U, abilities: &[&str]) -> String {
    let created = app.block_on(async {
        Tokens::of(app.app())?
            .create(user.auth_id(), "test", abilities, None)
            .await
    });
    match created {
        Ok(new) => new.plain_text().to_owned(),
        Err(e) => panic!("the test token could not be created: {e}"),
    }
}
