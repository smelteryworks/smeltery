# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- `Hallmark` settings (`HALLMARK_TOKEN_EXPIRATION`, `HALLMARK_MAX_TOKENS_PER_USER`, `HALLMARK_GUESS_LIMIT`) and
  `HallmarkExt::hallmark`, which registers the guard `hallmark`, the middleware families `abilities:` and `ability:`,
  the `Tokens` service, the credential listener and the console command `hallmark:prune-expired`.
- Personal access tokens: `smt_` + 64 lowercase hex characters, stored as SHA-256, bound to the user's password hash
  and credentials epoch; `Tokens::{create, list, find, revoke, revoke_all, revoke_all_except, prune_expired}`,
  `AccessToken`, `NewToken`, `PlainToken` (no `Display` / `Serialize`, redacted `Debug`) and the `HasApiTokens` methods
  on every user model. The creation transaction takes the write lock at its start, so concurrent creations on SQLite
  queue instead of failing with "database is locked".
- The bearer guard: one `Authorization: Bearer` header only, shape check before the lookup, constant-time
  re-compare, expiry and maximum age, the password binding, one 401 answer with `WWW-Authenticate: Bearer` and
  `no-store`, a per-client guess budget in the cache (429 with `Retry-After`), `Vary: Authorization, Cookie`.
- Abilities (exact names or `*`, at most 64 of 100 bytes; stored lists that fail the rules grant nothing).
- A per-user token cap with least-recently-used eviction (`COALESCE(last_used_at, created_at)`; on a tie, as with
  MySQL's whole-second timestamps, the never-used token goes first), `last_used_at` written at most once a minute.
- Revocation events (`AuthEvent::Revoked` / `RevokedAll { Tokens }` on the PubSub topic `auth`).
- `CurrentToken`, `migrations::{up, down}` and `testing::{acting_as, token_for}`.
- SPA mode: `Hallmark::spa` / `HALLMARK_SPA`, `Hallmark::stateful` / `HALLMARK_STATEFUL`, the first-party rule
  (`Guard::first_party`), `GET /hallmark/csrf-cookie`.
- `issue_for_credentials` (core's login budgets and password gate, a registered second factor), `NewToken::created`
  (201, `no-store`) and the `revoke_current` handler.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
