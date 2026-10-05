# smeltery-temper

Temper is the authentication layer of [Smeltery](https://github.com/smelteryworks/smeltery): the routes and handlers of
login, logout, registration, password reset, e-mail verification, password confirmation and profile and password
updates, each feature switched on in one builder. The app keeps what is its own: the pages (Mold views, or React /
Vue pages through Alloy), small **actions** (create a user, the password rules, the profile fields) and, where it
wants, its own answers. Every security mechanic (the login budgets, password hashing, sessions and their binding to
the password, reset tokens, verification links, password confirmation) is core's `smeltery::auth`; Temper calls it.

Apps use it through the `smeltery` crate as `smeltery::temper`.

## Setup

```rust
# mod user {
#     use smeltery::db::prelude::*;
#     #[sea_orm::model]
#     #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#     #[sea_orm(table_name = "users")]
#     pub struct Model {
#         #[sea_orm(primary_key)]
#         pub id: i64,
#         pub name: String,
#         pub email: String,
#         pub email_verified_at: Option<DateTimeUtc>,
#         pub password: String,
#         pub remember_token: Option<String>,
#     }
#     impl ActiveModelBehavior for ActiveModel {}
#     impl smeltery::auth::Authenticatable for Model {
#         fn auth_id(&self) -> i64 { self.id }
#         fn password_hash(&self) -> &str { &self.password }
#         fn remember_token(&self) -> Option<&str> { self.remember_token.as_deref() }
#     }
#     impl smeltery::auth::MustVerifyEmail for Model {
#         fn email(&self) -> &str { &self.email }
#         fn email_verified_at(&self) -> Option<DateTimeUtc> { self.email_verified_at }
#     }
# }
use smeltery::auth::hash_password;
use smeltery::db::prelude::*;
use smeltery::http::Html;
use smeltery::temper::{
    CreatesNewUsers, PasswordInput, ResetsUserPasswords, Temper, TemperCtx, TemperExt as _, TemperViews,
};
use smeltery::{Result, Validate};

use user::Model as User;

#[derive(Deserialize, Validate)]
pub struct RegisterForm {
    #[validate(required, max = 255)]
    pub name: String,
    #[validate(required, email, max = 255, unique(table = "users", column = "email"))]
    #[serde(deserialize_with = "smeltery::auth::deserialize_email")]
    pub email: String,
    #[validate(required, min = 8, confirmed)]
    pub password: String,
    pub password_confirmation: Option<String>,
}

/// Creates the user; Temper then signs them in and sends the verification link.
pub struct CreateNewUser;

impl CreatesNewUsers<User> for CreateNewUser {
    type Input = RegisterForm;

    async fn create(&self, ctx: &TemperCtx, input: RegisterForm) -> Result<User> {
        User::create(
            &ctx.db()?,
            user::ActiveModel {
                name: Set(input.name),
                email: Set(input.email),
                password: Set(hash_password(&input.password).await?),
                ..Default::default()
            },
        )
        .await
    }
}

#[derive(Deserialize, Validate)]
pub struct ResetForm {
    #[validate(required, email)]
    #[serde(deserialize_with = "smeltery::auth::deserialize_email")]
    pub email: String,
    #[validate(required, min = 8, confirmed)]
    pub password: String,
    pub password_confirmation: Option<String>,
}

impl PasswordInput for ResetForm {
    fn email(&self) -> &str { &self.email }
    fn password(&self) -> &str { &self.password }
}

/// The reset itself is core's; the action holds the password rules (its form).
pub struct ResetUserPassword;

impl ResetsUserPasswords<User> for ResetUserPassword {
    type Input = ResetForm;
}

fn views() -> TemperViews {
    TemperViews::new()
        .login(|_| Html("<h1>Log in</h1>"))
        .register(|_| Html("<h1>Register</h1>"))
        .forgot_password(|_| Html("<h1>Forgot your password?</h1>"))
        // `ctx.token()` and `ctx.email()` come from the link as sent: untrusted. Hand them to a template that
        // escapes them (a Mold view's `{{ token }}`, an Alloy page prop), never into HTML text built by hand.
        .reset_password(|_| Html("<h1>Choose a new password</h1>"))
        .verify_email(|_| Html("<h1>Check your inbox</h1>"))
        .confirm_password(|_| Html("<h1>Confirm your password</h1>"))
}

fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    app.temper(
        Temper::<User>::new()
            .registration(CreateNewUser)
            .reset_passwords(ResetUserPassword)
            .email_verification()
            .views(views()),
    )
}
# let _ = build;
```

`.temper(…)` registers `User` as the user model (`.auth::<User>()`; no separate call is needed), adds Temper's web
routes (sessions and CSRF, like every web route) and never flashes a field named `code` back as old input. The app
stops at boot when a page route has no view (the error names the `TemperViews` method), when `without_route` names
an unknown route or when the prefix is invalid; core stops it when `.auth::<…>()` is called with another model,
before or after `.temper(…)`.

## Routes

| Method | Path | Name | Middleware | Feature |
|---|---|---|---|---|
| GET | `/login` | `login` | `guest` | always |
| POST | `/login` | `login.store` | `guest`, `throttle:30,1` | always |
| POST | `/logout` | `logout` | `auth` | always |
| GET | `/register` | `register` | `guest` | `registration` |
| POST | `/register` | `register.store` | `guest`, `throttle:6,1` | `registration` |
| GET | `/forgot-password` | `password.request` | `guest` | `reset_passwords` |
| POST | `/forgot-password` | `password.email` | `guest`, `throttle:6,1` | `reset_passwords` |
| GET | `/reset-password/{token}` | `password.reset` | `guest` | `reset_passwords` |
| POST | `/reset-password/{token}` | `password.update` | `guest`, `throttle:6,1` | `reset_passwords` |
| GET | `/email/verify` | `verification.notice` | `auth` | `email_verification` |
| GET | `/email/verify/{id}/{hash}` | `verification.verify` | `auth`, `throttle:6,1` | `email_verification` |
| POST | `/email/verification-notification` | `verification.send` | `auth` (six a minute per user) | `email_verification` |
| PUT | `/user/profile-information` | `user-profile-information.update` | `auth`, `password.confirm`, `throttle:6,1` | `update_profile_information` |
| PUT | `/user/password` | `user-password.update` | `auth`, `throttle:6,1` | `update_passwords` |
| GET | `/user/confirm-password` | `password.confirm` | `auth` | always |
| POST | `/user/confirm-password` | `password.confirm.store` | `auth`, `throttle:6,1` | always |
| GET | `/user/confirmed-password-status` | `password.confirmation` | `auth` | always |
| GET | `/two-factor-challenge` | `two-factor.login` | `guest` | `two_factor` |
| POST | `/two-factor-challenge` | `two-factor.login.store` | `guest`, `throttle:30,1` | `two_factor` |
| POST | `/user/two-factor-authentication` | `two-factor.enable` | `auth`, `password.confirm`, `throttle:6,1` | `two_factor` |
| POST | `/user/confirmed-two-factor-authentication` | `two-factor.confirm` | `auth`, `password.confirm`, `throttle:6,1` | `two_factor` |
| DELETE | `/user/two-factor-authentication` | `two-factor.disable` | `auth`, `password.confirm`, `throttle:6,1` | `two_factor` |
| GET | `/user/two-factor-qr-code` | `two-factor.qr-code` | `auth`, `password.confirm` | `two_factor` |
| GET | `/user/two-factor-secret-key` | `two-factor.secret-key` | `auth`, `password.confirm` | `two_factor` |
| GET | `/user/two-factor-recovery-codes` | `two-factor.recovery-codes` | `auth`, `password.confirm` | `two_factor` |
| POST | `/user/two-factor-recovery-codes` | `two-factor.regenerate-recovery-codes` | `auth`, `password.confirm`, `throttle:6,1` | `two_factor` |

`password.confirm` on the two-factor management routes goes away with `TwoFactor::new().confirm_password(false)`;
everything that touches a confirmed enrolment (its QR code and key, new recovery codes, turning it off, enabling
over it) still needs a password confirmation within `AUTH_PASSWORD_TIMEOUT` (423 for JSON clients, else the
confirmation page).
HTML forms send `PUT` / `DELETE` with a `_method` field (CSRF still applies). `smeltery route:list` lists the routes.

| Builder method | Does |
|---|---|
| `.registration(action)` | registration; `action: CreatesNewUsers<User>` creates the user |
| `.reset_passwords(action)` | the forgot / reset forms; `action: ResetsUserPasswords<User>` (its `Input` holds the password rules) |
| `.email_verification()` | the verification routes and the link after registration and after an address change (`User: MustVerifyEmail`); requiring verified addresses stays core's `.verify_email::<User>()` |
| `.update_profile_information(action)` | `PUT /user/profile-information`; `action: UpdatesUserProfileInformation<User>` |
| `.update_passwords(action)` | `PUT /user/password`; `action: UpdatesUserPasswords<User>` |
| `.views(TemperViews)` / `.views(false)` | the pages; `false` registers no `GET` page routes (a single-page or mobile client sending `Accept: application/json`; a browser request gets redirects to pages that do not exist) |
| `.responses(r)` | the app's own answers (`TemperResponses`, one method per outcome with a default) |
| `.listen(f)` | an event listener: `f(app, TemperEvent)` |
| `.login_pipeline(step)` | a step after the password check and before the sign-in (`PipelineStep::Continue` or `Respond(response)`, or an error); steps gate new web sign-ins only, not open sessions, remember-me restores or API tokens |
| `.login_policy(check)` | core's `LoginPolicy` typed on the user model: `check(app, user)` answers `LoginDecision::Allow` or `LoginDecision::refuse(status, message)`; asked before the steps and the second factor, again before a two-factor code is checked, after registration creates the account, and by every other way in (`Auth::attempt`, the login completion, remember-me cookies, a token endpoint calling `app.check_login`). A rule that must also hold for API tokens (a suspended account) belongs here. A refusal answers its status with the message on `email` (browsers are sent back; from the challenge, JSON clients get it on `code` and browsers go to the login page); nobody is signed in. It gates new sign-ins only: core's `auth::end_credentials` ends the open sessions (through the user's `credentials_epoch` column), remember token, API tokens and sockets a refused user already holds |
| `.home(path)` | where signed-in users go (default `AUTH_HOME`) |
| `.prefix("/auth")` | every route under a prefix; names unchanged |
| `.routes(false)` | no routes at all |
| `.without_route(name)` | leave one route out (the app declares its own there) |
| `.limits(Limits::new().login("30,1").forms("6,1"))` | the throttles of the form routes (`login` also on `POST /two-factor-challenge`) |
| `.two_factor(TwoFactor::new())` | two-factor authentication (`User: TwoFactorAuthenticatable`); see below |

## Flows

- **Login** (`POST /login`, fields `email`, `password`, `remember`): core's `Auth::validate` counts the login budgets
  before the lookup and the password check, then the login pipeline runs: the login policies (`app.check_login`),
  the app's steps, the sign-in (`Auth::login_user`: a new
  session id and CSRF token), the `Login` event and the answer: the page the visitor first asked for (local paths
  only), else home; JSON clients get 200 `{"two_factor": false}`. A wrong address or password answers with "These
  credentials do not match our records." on `email`: the login page for browsers and Inertia, 422 for JSON clients.
- **Registration**: the action's `Input` is validated, `create` returns the user, Temper signs them in, sends the
  verification link (with `.email_verification()` and core's `.verify_email::<User>()`) and answers home (201 to JSON
  clients). The login policies are asked after `create`: a refusal (an account awaiting approval) keeps the account,
  fires `Registered`, signs nobody in and answers the refusal. Such a registration sends no verification mail; once
  approved, the user signs in and asks for the link (`POST /email/verification-notification`). The sign-in asks no password and no second factor,
  which is right only for a new account: when `create`
  returns a user that has two-factor authentication (an existing account, such as an "upsert by email"), the request
  fails with 500 and an `error` log line, and nobody is signed in.
- **Password reset**: `POST /forgot-password` answers the same for every address ("If that e-mail address has an
  account, a password reset link has been sent to it."); the link (`APP_URL` + the route `password.reset`) goes to
  the address stored on the account. `POST /reset-password/{token}` resets through core (the account by its stored
  address and changed by its id, the token used up once, every session and remember-me cookie of the user ended),
  then runs the action and answers with the login page; a wrong, used or expired link flashes "This password reset
  link is invalid or has expired." and opens the forgot-password page. `GET /reset-password/{token}` answers the same
  for a token that is not 64 ASCII letters and digits (the page never puts anything else into its form's `action`).
- **E-mail verification**: the notice page sends users who need no verification home; the link marks the address
  verified ("Your e-mail address is verified." the first time); "send it again" allows six a minute per user.
- **Password confirmation**: `POST /user/confirm-password` checks the password through core (five tries a minute per
  user) and opens the remembered page; core's `password.confirm` middleware then lets the session through for
  `AUTH_PASSWORD_TIMEOUT` seconds. `GET /user/confirmed-password-status` answers `{"confirmed": true|false}`.
- **Profile**: the address decides where reset links go, so `PUT /user/profile-information` needs a password
  confirmation within `AUTH_PASSWORD_TIMEOUT` (core's `password.confirm`: 423 for JSON clients, else the confirmation
  page). The action writes the profile and returns `EmailChanged::Yes` when the address changed. Temper then deletes
  the user's pending reset link (it went to the old address). When the app requires verified addresses (core's
  `.verify_email::<User>()`), Temper empties `email_verified_at` through core (`auth::mark_unverified`) and sends a
  link to the new address (through Temper's verification routes, or the app's own route named
  `verification.verify`), at most three such links an hour per user (beyond that the change is saved and the user
  asks for the link with "send it again").
- **Password update**: `current_password` is checked through core's password confirmation (the same five tries a
  minute), the new password is stored with `Auth::set_password`: this device stays signed in, every other session
  and remember-me cookie of the user ends.

The reset page's token and address (`ViewCtx::token`, `ViewCtx::email`) come from the link as sent: they are
untrusted, so a view renders them escaped (Mold's `{{ }}`, React / Vue props), never into HTML built by hand.

Answers (`TemperResponses`, with `DefaultResponses` as the defaults) and the sentences they use
(`smeltery::temper::messages`):

| Outcome | Browser / Inertia | JSON client (`Accept: application/json`) |
|---|---|---|
| login | the remembered page, else home | 200 `{"two_factor": false}` |
| failed login | the login page, message on `email` | 422 on `email` |
| register | home | 201 |
| logout | the route `home`, else `/` | 204 |
| reset link requested | `status` + the forgot-password page | 200 `{"message": …}` |
| password reset | `status` + the login page | 200 `{"message": …}` |
| reset failed | `error` + the forgot-password page | 422 on `email` |
| profile / password updated | `status` + back | 200 |
| password confirmed | the remembered page, else home | 201 |
| e-mail verified | `status` (first time) + home | 204 |
| verification link sent | `status` + back | 202 |

## Two-factor authentication

`.two_factor(TwoFactor::new())` adds codes from an authenticator app (RFC 6238 TOTP: HMAC-SHA1, six digits, 30-second
steps, as Google Authenticator, Authy, 1Password, Microsoft Authenticator and Aegis read them) and single-use recovery
codes. The user model implements `TwoFactorAuthenticatable` over four columns, which the app's migration adds in
`up` with `schema.table("users", |t| { … })` (and `down` removes them with `t.drop_column("…")`):

```text
schema.table("users", |t| {
    t.text("two_factor_secret").nullable();
    t.text("two_factor_recovery_codes").nullable();
    t.datetime("two_factor_confirmed_at").nullable();
    t.big_integer("two_factor_last_step").nullable();
}).await
```

| `TwoFactor` option | Default | Does |
|---|---|---|
| `confirm(bool)` | `true` | two-factor counts as on only after a first code confirms the enrolment |
| `confirm_password(bool)` | `true` | `password.confirm` on the management routes (with `false` a first enrolment needs no password; a confirmed one always does) |
| `window(n)` | `1` | steps of tolerance either side of now (at most 2) |
| `recovery_codes(n)` | `8` | codes per enrolment (4 to 16) |
| `challenge_ttl(d)` | 5 minutes | how long a pending login waits for its code (1 to 15 minutes) |

- **Enrolment:** `POST /user/two-factor-authentication` stores a new 160-bit secret, encrypted with
  `App::encrypt("temper.two-factor", <user id>, …)` (AES-256-GCM, a key derived from `APP_KEY`; the user id as
  associated data, so a copy in another row does not open), and new recovery codes, stored as SHA-256 hashes. A
  confirmed enrolment is never replaced: turn it off first (409 / an `error` flash). The plaintext codes are flashed
  once under `two_factor::RECOVERY_CODES_KEY` for browsers (JSON clients get them only in the answer, with
  `Cache-Control: no-store`). `GET /user/two-factor-qr-code`
  answers `{"svg": …, "url": "data:image/svg+xml;base64,…"}` (`Cache-Control: no-store`): an SVG made only of
  numbers for an `<img src>`, never raw HTML; the `otpauth://` URI names `APP_NAME` and
  `TwoFactorAuthenticatable::two_factor_account` (default: the user id; return the e-mail address). `GET
  /user/two-factor-secret-key` answers `{"secretKey": …}` for manual entry; for a confirmed enrolment both answer only after a password
  confirmation within `AUTH_PASSWORD_TIMEOUT` (423 otherwise), also with `confirm_password(false)`. `POST
  /user/confirmed-two-factor-authentication` (field `code`) confirms with a first code. Confirming and turning off
  replace the remember token, so remember-me cookies from before stop signing in; this session stays. Other open
  sessions of the user stay signed in: a settings page offers `auth.logout_other_devices(password)` for them.
- **The challenge:** a correct password of a user with two-factor on gives no sign-in: the session gets a new id and
  a pending login (the user, `remember`, a binding to the password hash, the time), and the answer is the challenge
  (`two-factor.login`; JSON clients 200 `{"two_factor": true}`). `POST /two-factor-challenge` takes `code` or
  `recovery_code`. A pending login ends after `challenge_ttl`, when the password changes, and after five wrong codes
  (the password is needed again). Every code counts first against the account's budget (app codes: five per five
  minutes and one hundred a day; recovery codes: ten an hour on a budget of their own, so used-up app-code budgets
  never block the owner's recovery codes; in the cache store, shared by every process; a cache failure refuses). Past
  a budget the challenge answers 429 with `Retry-After` (browsers: the message on the challenge page). Someone who
  holds the password can start pending logins and spend both the app-code and the recovery-code budgets, again each
  time they expire, and so keep the owner from finishing a sign-in. The owner's remedy is a password reset (it ends
  that person's password; the recovery-code budget then expires within an hour, the daily app-code budget within a
  day) or the operator command `temper:two-factor-disable`. The login policies are asked again before a code is checked. Then a code is accepted
  only for a time step after the last accepted one (one conditional `UPDATE` of `two_factor_last_step`), and a
  recovery code is compared against every stored hash in constant time and removed by compare-and-set: of two
  requests with one code, one wins. Success signs in through core (a new session id and CSRF secret). Whether a user needs
  the second factor is read from their row at that moment, also for `login_pipeline` and the login completion. The
  wrong-code count of one pending login lives in the session, so parallel requests of one session can pass five;
  the account budget still bounds them.
- **Status for settings pages:** `two_factor::status::<User>(&auth, &session)` gives `TwoFactorStatus { enabled,
  confirmed, recovery_codes_left, new_recovery_codes }`, and `two_factor::setup::<User>(&app, &auth)` gives
  `TwoFactorSetup { qr_code_url, secret_key }` for a page rendered on the server (the same values as the two JSON
  routes, under the same rule: a confirmed enrolment's only after a recent password confirmation). `GET
  /user/two-factor-recovery-codes` answers `{"remaining": n}`; `POST` there makes new codes.
- **Other endpoints:** `.two_factor(…)` registers core's `SecondFactor` (`app.second_factor()`: `required`, `verify`
  with a code or a recovery code, the same budgets and rules; a spent budget answers
  `SecondFactorVerdict::TooManyAttempts { retry_after }`, which `into_result` turns into 429 with `Retry-After`), a credential listener that removes an enrolment when a
  password reset verifies a previously unverified address, and the console command `temper:two-factor-disable
  <email> --force` (without `--force` it changes nothing).
- **`APP_KEY`:** secrets are encrypted under a key derived from `APP_KEY`. After a new `APP_KEY` they do not decrypt:
  codes from the app are refused (an `error` log line, at most once per user per hour; two-factor stays on), recovery
  codes still work, and the user enrols again.

## Events

`TemperEvent::{Registered, Login, Failed, Lockout, Logout, PasswordResetLinkSent, PasswordReset, PasswordUpdated,
ProfileUpdated, Verified, PasswordConfirmed, TwoFactorChallenged, TwoFactorFailed, TwoFactorLockout, TwoFactorEnabled,
TwoFactorConfirmed, TwoFactorDisabled, RecoveryCodesGenerated, RecoveryCodeUsed}` carry user ids (a failed login or lockout an HMAC-SHA256 of the
normalized address under an `APP_KEY`-derived key: the same address gives the same value within one app), never
passwords, tokens or addresses. `PasswordResetLinkSent` fires only when a link was issued; the answer to the request
is the same either way. Listeners (`.listen(…)`) run once per event on the app's owned tasks after the
answer is decided, so they never change or delay it; under `APP_ENV=testing` they run before the answer returns. An
error or a panic in a listener is logged at `warn`. Sign-outs and password changes also publish core's `AuthEvent`s.

## The login pipeline for other sign-in paths

`smeltery::temper::login_pipeline(&ctx, &user, remember)` runs the pipeline for a user whose identity another path
proved (`ctx: TemperCtx` is also a handler argument). `.temper(…)` also registers it as core's
`smeltery::auth::LoginCompletion`, so a crate without a dependency on Temper signs users in through
`app.login_completion()`. Pipeline steps gate new sign-ins only: an open session and a sign-in restored from a
remember-me cookie never meet them, so locking an account out also ends its credentials (`auth::password_changed`
ends every session and remember-me cookie) or checks the user in a middleware.
`CreatesNewUsers::create_social(ctx, SocialUser)` creates a user without a password for such a path (the default
refuses).

## Testing

`smeltery::temper::testing` has `log_in(&app, email, password)`, `confirm_password(&app, password)`,
`EventRecorder` (`.listen(recorder.listener())`, then `recorder.events()`), and for two-factor
`fake_clock(app.app(), unix_seconds)` (Temper's two-factor time), `two_factor_code::<User>(&app, user_id)`,
`two_factor_code_at::<User>(&app, user_id, steps)`, `enable_two_factor::<User>(&app, user_id)` (enabled and
confirmed; the recovery codes in clear), `has_pending_two_factor(&app)` and `code_for_secret(secret, unix_seconds)`.

## Licence

MIT OR Apache-2.0.
