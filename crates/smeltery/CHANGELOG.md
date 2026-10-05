# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- The facade: re-exports `smeltery-core` (`App`, `AppBuilder`, routing, config, middleware, errors, testing) and
  `prelude`; `smeltery::VERSION` (the Smeltery version; new apps' welcome page shows it).
- The `smeltery` binary (feature `cli`, on by default).
- `smeltery::mold` (the Mold crate), `#[derive(Mold)]` (`smeltery::Mold`), `smeltery::view`; `Mold`, `Template`
  and `view` in the prelude.
- `smeltery::db` (models, migrations, seeders, factories, `Found`); the `sqlite`, `postgres` and `mysql` features
  enable the database drivers in `smeltery-core`; `Db`, `Found` and `Record` in the prelude.
- `smeltery::{session, validation, auth}`, `#[derive(Validate)]` (`smeltery::Validate`); `Session`, `Auth`, `Back`,
  `Valid` and `Validate` in the prelude. `auth.intended(default)` / `auth.set_intended(path)` (the page a guest asked
  for, after the login). Email verification: `smeltery::auth::{MustVerifyEmail, EmailVerificationRequest,
  verification}`, `AppBuilder::verify_email`, the `verified` middleware and `smeltery::mail::VerifyEmail`.
- `smeltery::http`: `UploadedFile` (file fields in `Valid<T>` forms posted as `multipart/form-data`),
  `{ClientInfo, TrustedProxies}` (`TRUSTED_PROXIES`), the `GET /up` health route, response compression,
  `STATIC_CACHE_CONTROL`, default security headers on every response (`SECURITY_HEADERS`, `FRAME_OPTIONS`,
  `HSTS_MAX_AGE`), and SQLite in WAL mode with paths relative to the app root (see `smeltery-core`'s changelog).
- `smeltery::cache` (`Cache` in the prelude) and the `redis` and `memcached` features (the cache stores); the
  `redis` feature also turns on Watchfire's `redis` queue driver (`QUEUE_DRIVER=redis`).
- `smeltery::pubsub`: messages between the app's processes (`PubSub`, drivers `local`, `database`, `redis`);
  Sparks pushes reach the pages of every process. `smeltery::channels` (core's channel seam).
- `smeltery::sparks` (the `smeltery-sparks` crate: live components), `#[derive(Spark)]` and `#[actions]`
  (`smeltery::{Spark, actions}`), Sparks listeners (`#[on("anvil:…", "…")]`); `Spark`, `actions`, `SparkCtx`,
  `Sparks`, `Broadcast`, `TemporaryUpload` and `SparksExt` in the prelude.
- `smeltery::watchfire` (the `smeltery-watchfire` crate: agents, jobs, the scheduler); `AgentsExt` in the prelude;
  the `llm` feature forwards to `smeltery-watchfire/llm`.
- `smeltery::mail` (the `smeltery-mail` crate); Watchfire's `mail` feature is on; `Mailable`, `Mailer`,
  `Envelope`, `Attachment`, `MailExt` and `QueueMail` in the prelude.
- `smeltery::bellows` (the `smeltery-bellows` crate: the `bellows:mcp` MCP server for coding agents);
  `BellowsExt` is in the prelude.
- `smeltery::alloy` (the `smeltery-alloy` crate: Alloy, the React and Vue bridge through the Inertia protocol),
  `#[derive(smeltery::Alloy)]`, and `AlloyExt` in the prelude.
- `smeltery::anvil` (the `smeltery-anvil` crate: Anvil, WebSockets over the Pusher Channels protocol 7,
  broadcasting and channel authorization), and `AnvilExt` in the prelude.
- `smeltery::temper` (the `smeltery-temper` crate: the authentication routes), `smeltery::hallmark` (the
  `smeltery-hallmark` crate: API tokens) and `smeltery::prospect` (the `smeltery-prospect` crate: full-text search),
  with `TemperExt`, `HallmarkExt`, `ProspectExt` and `Searchable` in the prelude.

### Security
- Security defaults of the data layer (see `smeltery-core`'s and `smeltery-mail`'s changelogs): a missing `APP_ENV`
  means `production`; secrets reach the log only in local development; private file modes on Unix; root-run
  commands never follow folders another user controls; line breaks in logged values are escaped; connection errors
  hide the database password; `cache:clear` keeps Watchfire's leases (`--all` removes them); plain SMTP with a
  login only to a relay on this machine.
- The web stack (see `smeltery-core`'s changelog): password resets mail and change the stored account only, an
  atomic login throttle with per-client, per-address and per-network budgets that reveal no accounts and lock
  nobody out, reset tokens keyed by account id, uploads stored with allow-listed extensions only and served
  sandboxed by their real path, bounded multipart bodies, header-read and idle timeouts (HTTP/2 included), a
  connection cap and a per-client connection limit, bounded argon2 work, sessions bound to the password hash with
  an absolute lifetime and `logout_other_devices`, quiet reset requests, a new CSRF token at sign-in, no session
  for visitors that store nothing, no server under `APP_ENV=testing`, secret-looking fields never flashed, CSRF for
  every non-reading method, `__Host-` cookies under https, and the `throttle:<max>,<minutes>` route middleware.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
