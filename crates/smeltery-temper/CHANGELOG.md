# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- `PUT /user/profile-information` carries `password.confirm`. An address change deletes the user's pending reset
  link and mails at most three verification links an hour per user.
- Every write on a confirmed two-factor enrolment (new recovery codes, turning it off, enabling over it) needs a
  password confirmation within `AUTH_PASSWORD_TIMEOUT`, also with `TwoFactor::confirm_password(false)`;
  `two-factor.disable` carries `throttle:6,1`.
- `GET /reset-password/{token}` answers like an invalid link for a token that is not 64 ASCII letters and digits.
- Registration asks the login policies after creating the account (a refusal keeps the account, fires
  `Registered` and signs nobody in). A refusal at the two-factor challenge reaches JSON clients on `code` and
  browsers on the login page.
- `Temper::login_policy(check)`: a login rule typed on the user model, registered as core's `LoginPolicy`. The login
  pipeline asks core's login policies before its steps and the second factor, and the two-factor challenge asks them
  again before a code is checked.
- Registration answers 500 and signs nobody in when the `create` action returns a user that has two-factor
  authentication.
- The two-factor challenge's 429 carries `Retry-After`; core's `SecondFactor::verify` answers
  `SecondFactorVerdict::TooManyAttempts` past the account's code budget.
- `Temper` and `TemperExt::temper`: login, logout and password confirmation routes, with `registration`,
  `reset_passwords`, `email_verification`, `update_profile_information` and `update_passwords` as features; `home`,
  `prefix`, `routes(false)`, `without_route`, `views`, `responses`, `listen`, `limits`, `login_pipeline`. Route names,
  middleware and throttles (`throttle:30,1` on `POST /login`, `throttle:6,1` on the other form routes) as in the
  README. `.temper(…)` registers the user model, never flashes a `code` field back as old input, and stops the app at
  boot for a missing view, an unknown `without_route` name, an invalid prefix or another user model (`TemperError`).
- Actions with typed, validated inputs: `CreatesNewUsers` (with `create_social` and `SocialUser`),
  `ResetsUserPasswords` (`PasswordInput`), `UpdatesUserPasswords` (`UpdatePasswordInput`),
  `UpdatesUserProfileInformation` (`EmailChanged`).
- `TemperViews` / `ViewSetting` / `ViewCtx`; `TemperResponses` with `DefaultResponses` (browser, Inertia and JSON
  answers); `messages`; `TemperCtx`.
- `TemperEvent` (with `PasswordResetLinkSent`; failed logins carry an `APP_KEY`-keyed hash of the address) and
  listeners on the app's owned tasks (before the answer under `APP_ENV=testing`).
- `login_pipeline`, `PipelineStep`; the pipeline is registered as core's `auth::LoginCompletion`.
- Two-factor authentication (`Temper::two_factor`, `TwoFactor`, `TwoFactorAuthenticatable`): RFC 6238 TOTP and
  recovery codes, the challenge after the password, enable / confirm / disable, the QR code (SVG data URI), the
  secret key, recovery codes; secrets encrypted with `App::encrypt`, codes single use, per-account budgets; core's
  `SecondFactor`; `two_factor::status`; `two_factor::setup` (`TwoFactorSetup`: the QR code and key for a page rendered
  on the server); `two_factor::DisableCommand` (`temper:two-factor-disable`); the answers
  `two_factor_*` and `recovery_codes_generated`; the view `two_factor_challenge`; the events of the feature.
- `testing`: `log_in`, `confirm_password`, `EventRecorder`, `fake_clock`, `two_factor_code`, `two_factor_code_at`,
  `enable_two_factor`, `has_pending_two_factor`, `code_for_secret`.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
