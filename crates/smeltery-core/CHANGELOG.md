# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- `App` / `AppBuilder`: services and config by type, route and global middleware, async boot hooks.
  `AppBuilder::on_serve`: async hooks that run once when the server starts (`serve`, `serve --no-agents`), after the
  build and before the first request; `work`, console commands and `TestApp` never run them, and a failing hook
  stops the server from starting.
- `Router` with `get`/`post`/`put`/`patch`/`delete`/`any`, route names, groups with prefixes, name prefixes and
  middleware, resource routes, URL generation and the route table. `AppBuilder::api_routes_at(prefix, routes)` (API
  routes under any prefix), `App::serves_http`.
- `AppBuilder::middleware_family(prefix, make)` and `middleware::BoxedMiddleware`: aliases with arguments, built per
  route at boot (`throttle:` and `auth:` are families). `AppBuilder::web_middleware`: middleware on every web route,
  inside the session stack (after the CSRF check, before the session is saved), in registration order; API routes
  skip it.
- `config::env` typed reader, Smeltery's own `.env` parser (a `.env` that starts with a UTF-8 byte order mark keeps
  its first key: `config::parse_env` strips the mark), framework `Settings`. A missing `APP_ENV` means `production`.
  `config::loopback_url`, `config::loopback_host` and `Settings::is_local_development` (`APP_ENV` `local` or `testing`
  with an `APP_URL` on this machine: `localhost`, `*.localhost`, a loopback address). `config::app_key_bytes`,
  `Settings::uses_test_key` and `Settings::has_signing_key` (the `APP_KEY` / test-key rules Watchfire shares): the
  fixed test key signs only under `APP_ENV=testing` with an empty `APP_KEY`, and a malformed or short `APP_KEY` is an
  error there too; in production a plain-text (not `base64:`) `APP_KEY` logs a warning at boot.
- `Error` with HTML / JSON error pages (details only in debug), panic catching; `Error::Validation` and
  `Error::validation(field, message)`; `From<DbErr> for Error`.
- The server: static files from `public/` (their `Cache-Control` is `STATIC_CACHE_CONTROL`,
  `Settings::static_cache_control`, default `no-cache`), method spoofing (`_method`, in URL-encoded and multipart
  forms), request ids, tracing (the request log span is `request{method, uri, version, client}`), request timeout,
  body limit; `serve` with graceful shutdown. The CSRF check and method spoofing read a multipart body only up to
  the field they need (at most `BODY_LIMIT`) and pass the rest on as a stream, so a multipart body larger than
  `BODY_LIMIT` reaches the handler.
- The health route `GET /up` (`200 OK`, `Cache-Control: no-store`, no session or CSRF), added by `AppBuilder::build`
  unless the app declares `GET /up`; `AppBuilder::without_health_route`.
- Responses are compressed with brotli or gzip (tower-http `CompressionLayer`, quality 4) when the client accepts it;
  server-sent event streams, images other than SVG and formats that are compressed already (fonts, archives, PDF,
  video, audio) are not. A compressed response carries a weak `ETag`.
- `UpgradeHold`: a route that serves upgraded connections (WebSockets, any HTTP/1.1 `Upgrade`) takes it from the
  upgrade request (`UpgradeHold::take(req.extensions_mut())`) and keeps it in the task that owns the socket; while
  it is held, the socket counts against `SERVER_MAX_CONNECTIONS` and `SERVER_MAX_CONNECTIONS_PER_IP`, the idle rule
  (`SERVER_HEADER_TIMEOUT`) never closes it, and the shutdown drain waits for it within `SHUTDOWN_TIMEOUT`. HTTP/2
  extended `CONNECT` (RFC 8441, WebSockets over HTTP/2) is answered `501 Not Implemented` (plain text) by the server
  before routing; WebSocket clients connect over HTTP/1.1.
- Security headers on every response unless the response sets them itself: `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: strict-origin-when-cross-origin`, `X-Frame-Options` and `Content-Security-Policy:
  frame-ancestors` (`SECURITY_HEADERS`, default `true`; `FRAME_OPTIONS` = `SAMEORIGIN` (default), `DENY` or `off`,
  any other value fails `build`), and `Strict-Transport-Security` with `HSTS_MAX_AGE` seconds (default 0, off), sent
  only when `APP_URL` is https. `Settings` fields `security_headers`, `frame_options`, `hsts_max_age`.
- CORS for listed origins (`smeltery_core::cors`): `CORS_ALLOWED_ORIGINS` (exact origins, `null` only when listed,
  `*` refused) and `CORS_PATHS` (default `/api/`, a prefix without a trailing `/` is warned at boot); preflights
  answered `204` before routing, `Access-Control-Allow-Origin` on answers to listed origins (the request timeout's
  408, a panic's 500 and error pages included), `Vary: Origin` on every answer of a covered path, never credentials.
  Off without `CORS_ALLOWED_ORIGINS`. `cors::normalize_origin` (the framework's origin parser), `cors::origin_of_url`,
  `cors::origin_parts`.
- `http::Back` and `http::Back::for_client`: `Back` accepts a `Referer` on the client's host (`X-Forwarded-Host` from
  a trusted proxy) or on `APP_URL`'s host, and never follows a `Referer` path that starts with `//` or `/\` or holds
  a backslash, whitespace, a control character or non-ASCII text (browsers drop tabs and line breaks from a
  `Location`). `http::is_local_path` (the redirect-target rule shared by `Back`, the intended URL, Sparks and Alloy),
  `http::is_safe_extension`, `http::stored_extension`, `http::wants_json` and `http::is_inertia` are public;
  `http::FromRequestParts` (for extractors of framework crates and apps) and `http::request` (the `http` crate's
  request module, for `Parts`) are re-exported.
- A request with `X-Inertia: true` is never treated as a JSON client by the web stack: a failed validation redirects
  back with the errors flashed, and a CSRF failure redirects back with the flash message `error` = "The page
  expired. Please try again."
- `TRUSTED_PROXIES` (`Settings::trusted_proxies`): IPs, CIDR ranges or `*`; only from a trusted TCP peer are
  `X-Forwarded-For` (rightmost untrusted hop), `X-Forwarded-Proto` and `X-Forwarded-Host` used. An invalid entry fails
  the build. Each `X-Forwarded-For` hop is decoded on its own, and a hop that is not an address (a non-UTF-8 byte
  included) leaves the client unknown. `http::ClientInfo` (handler argument and request extension: `ip`, `peer`,
  `scheme`, `is_secure`, `host`, `is_proxied`, `resolve`, `from_parts`), `http::TrustedProxies` and
  `App::trusted_proxies`. The login throttle, `Auth::ip` and the request log use the resolved client IP.
- Features `sqlite`, `postgres`, `mysql` (SeaORM 2.0.4 over sqlx, rustls with ring).
- `db::Db` (pool handle, handler extractor, `App::db`), connected at boot from `DATABASE_URL`, `DB_POOL_MAX`,
  `DB_CONNECT_TIMEOUT`; `db::Backend`, `db::DbOptions` (`DbOptions::root`), `db::SQLITE_BUSY_TIMEOUT`.
  `Db::execute_with(sql, values)` and `Db::query_with(sql, values)`: raw SQL with bound parameters (`?` on SQLite and
  MySQL, `$1` … on PostgreSQL). `Db::begin_write()`: a transaction that starts with the write lock on SQLite (`BEGIN
  IMMEDIATE`), so a transaction that reads before it writes waits for other writers instead of failing with
  "database is locked". Database connection errors show the URL's credentials as `***` (with a hint to
  percent-encode a password holding `/`, `?`, `#` or `@`); `mariadb://` URLs connect (as `mysql://`).
- SQLite database files run in WAL journal mode with `synchronous=NORMAL` and a 5-second busy timeout; in-memory and
  read-only (`mode=ro`, `immutable=1`) databases keep their journal mode. A relative SQLite path in `DATABASE_URL`
  resolves against the app root (`SMELTERY_ROOT`), not the process's working directory. Connections run with
  `PRAGMA recursive_triggers = ON`: `INSERT OR REPLACE` fires the delete triggers of the row it replaces, and an app
  trigger that writes to its own table fires itself again (guard it with `AFTER UPDATE OF <column>` or `WHEN
  new.<column> IS NOT old.<column>`). An in-memory SQLite database (`sqlite::memory:`) keeps its tables when a query
  is cancelled (for example by a timeout) while the pool hands it the connection.
- `db::Record` for every SeaORM entity model (`all`, `find`, `find_or_404`, `query`, `count`, `create`, `update`,
  `delete`, with `created_at` / `updated_at` handling) and `db::Found<T>` route model binding; `db::prelude`.
- Model listeners (`smeltery_core::db`): `ModelListener`, `ModelEvent` (`change`, `table`, `model_type`, `row`;
  `model::<M>()`, `is::<M>()`, `ModelEvent::new`), `ModelChange::{Created, Updated, Deleted}`,
  `AppBuilder::model_listener`. They are told about every successful `Record::create`, `update` (one that wrote) and
  `delete` (one that deleted a row), after the write, in registration order, on a task the app owns (a cancelled
  caller does not cut them short); a panicking listener is logged and never fails the write.
  `db::without_listeners(fut)` and `db::listeners_paused()` switch them off for one task. An app without listeners
  does no listener work.
  `db::MAX_LISTENER_DEPTH` (8): writes made by model listeners are reported to the listeners up to this depth; a
  deeper write is not reported and logs an ERROR, so a listener that writes the table it listens to cannot loop.
- Pagination: `db::Page<T>` (`items`, `page`, `per_page`, `total`, `last_page`; `new`, `map`, `has_next`,
  `has_previous`, serializes with those names), the `db::PageQuery` extractor (`?page=` 1..=10,000, `?per_page=`
  1..=100 by default, never rejects; `max`, `default_per_page`, `offset`, `from_query`, `new`), `Record::paginate`
  and `db::paginate(&db, select, page)`. `Page` and `PageQuery` are in `db::prelude`.
- `db::migration`: `Migration`, `Migrator` (the `migrations` table, batches, `migrate`, `rollback`, `fresh`,
  `status`), `Schema` and the `Blueprint` table builder generating SQL for SQLite, PostgreSQL and MySQL;
  `Blueprint::drop_column(name)` in `Schema::table` (SQLite, PostgreSQL, MySQL; the column's framework indexes go
  first on SQLite). On SQLite every migration, and `Schema::new(&db).create(…)` (and `table`, `drop`,
  `drop_if_exists`, `rename`) outside a migration, runs on one connection that re-reads the schema first, so
  `DROP COLUMN` / `RENAME COLUMN` and `create` work when another pooled connection changed the schema. On SQLite the
  table list (`migrate:fresh`) holds ordinary and virtual tables, not the shadow tables a virtual table keeps its data
  in (FTS5's `_data`, `_idx`, `_content`, `_docsize`, `_config`), and `migrate:fresh` drops the virtual tables first.
  `migrate:fresh` / `db:wipe` close the connection they switched foreign-key checks off on when they stop before
  switching them on again (it is not returned to the pool).
- `db::seed` (`Seeder`, `Seeders`) and `db::factory` (`Factory`, `Fake`).
- `console::{Command, Commands, Args}`; `AppBuilder::{migrations, seeders, commands}`; console commands `migrate`,
  `migrate:rollback [--step N]`, `migrate:fresh [--seed]`, `migrate:status`, `db:seed [--class X]`, with a
  production guard (`--force`), and app commands listed by `help`; the console kernel (`serve`, `route:list`,
  `help`). `console::Output` and `Command::run_with_output` (a command's output written by the console). An app's own
  console commands start its PubSub, and after the command the app's background tasks (queued PubSub messages, a
  reset mail) finish within `SHUTDOWN_TIMEOUT` before the process exits.
- `AppBuilder::on_start` and `Background`: background work started by `serve` and the `work` command and stopped
  within the shutdown budget; `App::start_background`, `App::has_background`; `serve_on` starts it. The `work`
  console command (background work without the HTTP server; exits 0 after a graceful stop; says "press Ctrl-C to
  stop" only when it runs at a terminal, not under systemd or Docker). `serve --no-agents` and `App::skip_background`:
  serve HTTP without the background work (Watchfire's agents); `App::set_web_only` / `App::is_web_only`: a `serve`
  process whose background work runs in `work` (`skip_background` sets it). `App::spawn_owned(task)`: work the app
  owns; `serve`, `work` and app console commands wait for it within the shutdown budget after the shutdown token is
  cancelled. `AppBuilder::serve_command(name, about, run)`: a console command that runs a server of its own from the
  built app (Anvil's `anvil` process); listed in `help`.
- The app binary's built-in `key:generate` (`--show` prints the key, reads no `.env` and sets up no logging, so a
  user who cannot read `.env` gets the key without a warning; with `APP_ENV=production`, or a missing `APP_ENV`, an
  existing key is kept unless `--force` is given) and `storage:link` commands, so a server needs no `smeltery` CLI;
  both log to stderr only. Their code is public in `console::setup` (`generate_key`, `set_app_key`, `write_app_key`,
  `KeyWrite`, `is_production_at`, `link_storage`, `key_written_message`), which the CLI uses too:
  `is_production_at` treats a missing `APP_ENV` as production, like `Settings`; `set_app_key` replaces an `APP_KEY`
  line that follows a UTF-8 byte order mark at the start of `.env` (and keeps the mark), keeps `\r\n` line endings
  and replaces `APP_KEY = x` lines; `write_app_key` replaces `.env` atomically (temp file, sync, rename; a symlinked
  `.env` stays a link, a `.env` symlink to a missing file creates that file; a new `.env` is `0600` on Unix) and keeps
  the owner and group of `.env` on Unix, and when it cannot, rewrites the file in place and returns
  `KeyWrite::WrittenInPlace` (message `KEY_WRITTEN_IN_PLACE`); `key_written_message` says when `APP_KEY` in the
  process environment takes precedence over `.env`; `link_storage` returns `StorageLink::{Linked, AlreadyLinked}`
  (an existing link to `storage/app/public` is not an error, anything else at `public/storage` is) and the app
  binary's `storage:link` prints which; a Windows `storage:link` refused for lack of privilege names Developer Mode
  or an elevated shell.
- Sessions (`session::Session`): cookie driver (AES-256-GCM through the `cookie` crate's private jar, key from
  `APP_KEY` via HKDF), database driver (`sessions` table) and `SESSION_DRIVER=file` (the encrypted cookie holds the
  session id and the data lives in `storage/framework/sessions/<id>`, written through a temp file and a rename;
  expired files swept on about one request in a hundred); flash values, `regenerate`, `invalidate`, CSRF token;
  settings `SESSION_DRIVER`, `SESSION_LIFETIME`, `SESSION_COOKIE`, `AUTH_HOME`. The server refuses to start without a
  32-byte `APP_KEY` when the app has web routes. `Session::binding()`, `Session::flashed` (the values the previous
  request flashed, without the `_`-prefixed internal keys), `Session::flash_errors(&app, &errors, &input)` (flash
  validation errors and filtered old input before any redirect), `session::peek(&app, &headers)` (the session a
  request's cookie names, read without saving anything or setting a cookie, for API routes such as the Sparks
  stream), `session::run_web_stack(app, req, next)` (the web routes' session stack, CSRF included, as middleware),
  `AppBuilder::vary_web_responses` (every web response names a request header in `Vary`, the session stack's own
  answers included: CSRF failure, the redirect after a failed validation). Every sign-in removes the session keys
  starting with `_auth.` or `_temper.`.
- CSRF check on web routes (`_token` in URL-encoded or multipart forms, the `X-CSRF-TOKEN` header, then
  `X-XSRF-TOKEN`, then the `_token` field), 419 "Page Expired". `Session::csrf_token`: the CSRF token masked with a
  fresh random pad; `@csrf`, `csrf_token()` and the Sparks meta tag render it, so a compressed page never repeats
  the secret (BREACH); the check accepts every masked form and the unmasked `Session::token`.
  `AppBuilder::xsrf_cookie`: every web response sets the `XSRF-TOKEN` cookie (the masked CSRF token, a fresh mask
  per response, readable by JavaScript, `SameSite=Lax`, `Secure` under an https `APP_URL`).
- `validation`: `Validate`, `Valid<T>` (empty strings dropped, 422 JSON or redirect back with errors and old input),
  `ValidationErrors`, `ValidationContext`, the rules and their messages. `validation::Invalid::retry_after` (sent
  as `Retry-After`, at least 1) and `Invalid::too_many(field, message, retry_after)`; the login,
  password-confirmation and verification-mail 429 answers carry `Retry-After`.
- `Valid<T>` reads `multipart/form-data` forms: file fields are `http::UploadedFile` (`name`, `size`, `mime`,
  `extension`, `temp_path`, `bytes`, `store`, `store_as`), streamed to `storage/framework/uploads/` and deleted on drop
  unless stored; content of PNG, JPEG, GIF, WebP and PDF files is checked against the declared type and extension;
  `UPLOAD_MAX_BYTES` (`Settings::upload_max_bytes`, 10 MB) caps a multipart request (a validation error past it).
  `validation::rules::Subject::File(bytes)` and `Subject::Upload`: an uploaded file as validation rules see it;
  `required` passes, and `min`, `max` and `between` count kilobytes ("The photo field must not be greater than 2
  kilobytes."); the `mimes` rule (`rules::mimes`).
- `auth`: `hash_password` / `verify_password` (argon2id, blocking thread), `Authenticatable`, `Auth` (`check`, `id`,
  `user`, `attempt`, `login`, `logout`, `ip()`: the request's client address), remember-me cookies, login throttling
  (`TooManyAttempts`), `auth` / `guest` middleware, `AppBuilder::auth::<User>()` (`U: sea_orm::ModelTrait`, which
  every model is, so the stored address can be read from the found row; two different models fail `build`, the same
  model twice is fine; a guard named `web` other than core's fails `build`). `auth::verify_credentials(&app,
  client, email, password)` (the login budgets, the dummy hash and the hash gate, without a session),
  `Auth::validate`; `Auth::attempt` is `validate` + `login`. `auth::normalize_email` (trimmed, ASCII lowercase),
  `auth::find_by_email::<U>`, `auth::deserialize_email`, `auth::model_column::<U>`, `App::auth_model`,
  `App::find_user(id)`, `Auth::auth_user()`, `AuthUser` (`id`, `downcast::<U>`, `binding(purpose)` (returns
  `Result`, refuses purposes outside `[a-z0-9._-]`), `credential_binding(purpose)`: the binding with the
  credentials epoch), `AuthUser::of(&user)`, `auth::cycle_remember_token(&app, user_id)`.
- `Auth::intended(default)`: a 303 to the page the `auth` middleware turned the visitor away from, else to
  `default`, and `Auth::set_intended(path)`. The `auth` middleware remembers a guest's `GET` page visit (not JSON
  clients, background `fetch` / XHR, subresources by `Sec-Fetch-Mode` / `Sec-Fetch-Dest`, prefetches or event
  streams; Inertia visits count): its path and query, never its host, at most 2048 bytes. Only paths on this site
  (visible ASCII, one leading `/`, no `\`) are remembered or followed.
- Guards and the principal (`smeltery_core::auth`): `Principal` (`user_id`, `guard`, `credential`, `expires_at`;
  `can(ability)`, `key()`, `user::<U>(&app)`, `auth_user(&app)`, `with_user(user)`), `Credential` (`Session {
  binding }`, `Token { id, abilities }`), `CredentialKind`, the `Guard` trait (`Guard::first_party(app, parts)`,
  default `false`: the `auth:` family runs the session stack around first-party API requests and lets their session
  through), `AppBuilder::guard`, core's `web` guard (registered by `.auth::<U>()`; the session stack stores the
  principal of every signed-in web request), the `auth:<g1>,<g2>` middleware family (`auth:web` is `auth`; a list
  naming a stateless guard answers like `auth` on web routes (login redirect) and 401 with `WWW-Authenticate:
  Bearer` on API routes; `auth::unauthenticated_bearer()` is that 401, for guard crates), the `Authenticated`
  extractor (and `Option<Authenticated>`), `auth::authenticate(&app, &mut parts, GuardSet::Stateless)` (`None` on
  web requests) and `App::has_stateless_guard`. Stateless guards never run on web routes. `Principal::key()` names
  the guard: `<guard>:session:<binding>` / `<guard>:token:<id>`; auth events use the same keys
  (`auth::credential_key(guard, kind, id)`). `throttle:` counts per principal user (a guard's principal when an
  `auth:` alias runs before it).
- Login policies: `auth::LoginPolicy` (`check(app, user)`), `auth::LoginDecision::{Allow, Refuse}` (+ `refuse`),
  `AppBuilder::login_policy` (several, asked in order) and `App::check_login(&user)` (a refusal is a validation error
  with the policy's status and its message on `email`). `Auth::attempt` asks them before signing in, and remember-me
  restores ask them too (a refusal stays a guest and removes the cookie).
- `LoginCompletion` trait (`complete(app, auth, session, headers, user, remember) -> Result<Response>`),
  `AppBuilder::login_completion`, `App::login_completion`, `Auth::login_user(&AuthUser, remember)`. While a
  `LoginCompletion` is registered, `Auth::attempt` and `Auth::login` return an error (the completion signs in with
  `Auth::login_user`).
- `SecondFactor` trait, `AppBuilder::second_factor`, `App::second_factor`;
  `auth::SecondFactorVerdict::{Valid, Invalid, TooManyAttempts { retry_after }}` (`is_valid`, `into_result`), which
  `SecondFactor::verify` returns, so a spent code budget answers 429 with `Retry-After`.
- Password confirmation: `Auth::confirm_password` (five tries a minute per user), `Auth::password_confirmed_within`,
  the `password.confirm` middleware (423 JSON, else a redirect to `password.confirm` / `/user/confirm-password`),
  `AUTH_PASSWORD_TIMEOUT` (`Settings::password_timeout`, default 10800 s). `Auth::logout_other_devices` counts
  against the same budget.
- `Auth::set_password(new)` (keeps this session, ends the others, replaces the remember token),
  `auth::password_changed(&app, user_id, except)` for passwords written by app code (an `except` principal of
  another user is refused), `CredentialListener` / `CredentialsChanged` (`was_unverified`: a reset verified a
  previously unverified address) / `CredentialChange` and `AppBuilder::credential_listener`: run after a reset,
  `logout_other_devices`, `set_password` and `password_changed`; a failing listener skips neither the other
  listeners nor the `RevokedAll` event. `auth::end_credentials(&app, user_id)` and `CredentialChange::Ended`: end
  the user's open sessions (through `Authenticatable::credentials_epoch`, a nullable `credentials_epoch` column
  mixed into the session binding; `None` adds nothing to the binding), replace the remember token, run the
  credential listeners and publish `RevokedAll`, without a password change.
- `AuthEvent::{Revoked, RevokedAll}` published on the PubSub topic `auth` (`auth::EVENTS_TOPIC`,
  `auth::publish_event`) by logout, `logout_other_devices`, resets, `set_password` and `password_changed`;
  `AuthEvent::ends(user_id, key)`: whether an auth event ends that credential.
- `auth::passwords`: reset tokens (`password_reset_tokens` has `user_id`, `token`, `created_at`; `create_token(db,
  user_id)`), `auth::passwords::RESEND_INTERVAL`, the `ResetNotifier` service trait (`Arc<dyn ResetNotifier>`):
  `send_reset_link` hands the link to it when installed (mail does) and returns `Result<Option<i64>>` (the user's id
  when a link was issued); without a notifier the link reaches the log only in local development
  (`Settings::is_local_development`), elsewhere a warning without the link. `auth::passwords::reset(&App, …)` goes
  through the registered user model (its table and columns), returns `Option<i64>` (the user's id), uses the token up
  in one conditional delete before the password changes (two resets with one token cannot both change the
  password), marks the address verified with `.verify_email`, runs the credential listeners and publishes
  `RevokedAll`. Reset links follow the route named `password.reset` (else `/reset-password/{token}`). A token
  created up to 60 seconds "in the future" counts as fresh, so a link works on MySQL right after it is sent (MySQL
  rounds the stored `created_at` to whole seconds, up to half a second after the current time).
- Email verification (`auth::verification`): the `auth::MustVerifyEmail` trait (`email`, `email_verified_at`) and
  `AppBuilder::verify_email::<User>()`; signed, expiring links (`verification_url`, `send_verification_link`) bound
  to the user id and the current email, valid for `AUTH_VERIFICATION_EXPIRE` minutes (`Settings::verification_expire`,
  default 60, at least 1); the `EmailVerificationRequest` handler argument (`fulfill` sets `email_verified_at`, and
  `updated_at` when present, once); `Auth::has_verified_email`, `Auth::send_verification_email` and
  `Auth::resend_verification_email` (six a minute per user); the `verified` middleware alias (registered by
  `.auth::<User>()`; guests never pass; it checks the principal's user, answers 403 JSON on API routes and 401 for a
  guard's principal whose user row is gone); the `VerificationNotifier` service trait. Without a notifier the link is
  logged only in local development. `build` fails when a `verification.verify` route lacks `{id}` or `{hash}`.
  `auth::mark_verified(&app, id)` / `auth::mark_unverified(&app, id)` through the user model; `App::verifies_email()`.
- `cache`: the app's cache. `Cache` (handler argument, `App::cache`, `Cache::open`) with `get`, `put`, `forever`,
  `add`, `remember`, `remember_forever`, `has`, `forget`, `pull`, `increment`, `decrement`, `flush`, `flush_all`,
  `store`, `with_prefix`; atomic locks (`Cache::lock`, `Cache::restore_lock`, `Lock::{get, block, release,
  force_release, refresh, owner, current_owner, is_owned}`; `Lock::refresh`: the owner of a live lock holds it for
  its full time to live again, atomically on every store). Stores: `database` (tables from
  `cache::migrations::{up, down}`; `cache::migrations::mysql_statements(table)` holds the MySQL column changes for
  apps whose cache tables already exist; on MySQL the migration makes `key` and `owner` compare byte for byte,
  `utf8mb4_bin`; `CACHE_TABLE` is quoted in its statements), `redis` (feature `redis`), `memcached` (feature
  `memcached`), `file` (shared by several processes on one `CACHE_PATH`, on Windows too), `memory` (moka,
  process-wide), `array`, `null`. Settings `CACHE_STORE`, `CACHE_PREFIX`, `CACHE_PATH`, `CACHE_TABLE`,
  `CACHE_MEMORY_CAPACITY`, `CACHE_TIMEOUT`, `CACHE_MAX_VALUE_BYTES` (`Settings::cache_max_value_bytes`, default
  `cache::DEFAULT_MAX_VALUE_BYTES` = 16 MiB), `REDIS_URL`, `MEMCACHED_SERVERS`; `TestApp` uses the `array` store
  (`TEST_CACHE_STORE`). The `cache:clear [store] [--all]` console command.
- Cache store behaviour: `Cache::flush` and `cache:clear` keep Watchfire's leases and schedule claims (names starting
  with `cache::RESERVED_PREFIX` = `watchfire:`), so clearing the cache never lets a singleton agent run in two
  processes or a tick run twice; `Cache::flush_all` and `cache:clear --all` remove every entry and lock under the
  prefix, and `cache:clear` on memcached (which cannot keep some keys) asks for `--all`. A cached value over
  `CACHE_MAX_VALUE_BYTES` is an error for `get` / `pull` and a miss for `remember` (the `file` store checks the file
  size, the `database` store the size in the database and the `redis` store `STRLEN` before the value is read); a
  value of another shape is reported without serde's text, which could quote the cached value. The `database` store
  stores keys longer than 191 characters as their first 120 characters plus their SHA-256, and removes expired
  entries on `put` and `add` (about one write in a hundred); the `file` store removes the expired entries and locks
  of one shard on about one `put` / `add` in a hundred. The clean-up runs before the write (on the `database` store
  for at most 500 ms, in statements of at most 500 rows, and its errors are only logged), so a caller's timeout
  never reports an `add` that stored its key as failed. The `file` store's `flush` removes only files shaped like
  its entries (`<2 hex>/<64 hex>`) that parse; other files under `CACHE_PATH` stay. Its stale-lock takeover runs
  under its own `.takeover` file, and a holder removes its lock file only while it still holds its own token, so one
  per-key lock never goes to two waiters at once.
- `cache::RateLimiter` (`new(name, max, window)`, `hit(&app, key)` → `RateLimit::{Allowed, Limited}`, `peek(&app,
  key)`: a key's state without counting a hit, `clear`): fixed-window counts in the app's cache store, atomic,
  failing closed; the engine of `throttle:`. `RateLimiter`s with one name, `max` and window share their in-memory
  counts.
- The `throttle:<max>,<minutes>[,<field>]` route middleware (`throttle:5,1`; `throttle:<max>` per minute), in
  fixed windows that follow the clock (up to twice the limit can pass across a window's end),
  counted in the app's cache store (shared by the app's processes; memory with `CACHE_STORE=null`; a cache error
  answers 500) per route pattern and per signed-in user or client address (IPv6 by /64); past the limit web forms go
  back with the message on `email` (or `<field>`), JSON clients and API routes get 429 with `Retry-After`;
  `X-RateLimit-Limit` / `X-RateLimit-Remaining` on every answer. `AppBuilder::dont_flash` names fields old input
  leaves out.
- PubSub (`smeltery_core::pubsub`): messages between the processes of one app. `PubSub` (a service of every app,
  `PubSub::of`) with `publish` (this process at once, the other processes through the driver, awaited), `forward`
  (other processes only, never blocks: a queue of 1024, drops counted in `dropped()` and logged at most once a
  minute), `subscribe(topic)` (`Subscription::recv`, `RecvError::Lagged`; a buffer of 1024 per topic, so a burst
  on one topic never lags another; past `pubsub::MAX_TOPICS` = 256 topics they share one, except the
  `pubsub::RESERVED_TOPICS` `auth`, `anvil`, `sparks`); messages of at most 64 KiB, at most once,
  in order per publisher. Drivers `local`, `database` (table `pubsub_messages`, `pubsub::migrations::{up, down}`,
  polled every `PUBSUB_POLL_MS`, rows deleted after a minute, late commits caught by a 2 s overlap) and `redis`
  (feature `redis`, `PUBLISH` / `SUBSCRIBE` on `<CACHE_PREFIX>pubsub`). `PUBSUB_DRIVER=auto` (default) chooses by
  process: `serve` with its background work, console commands and `TestApp` stay local; `serve --no-agents` and
  `work` use Redis (with `CACHE_STORE=redis`) or the database. Messages between processes are sealed with
  AES-256-GCM under an `APP_KEY`-derived key. `Settings` fields `pubsub_driver`, `pubsub_poll_interval`;
  `TestApp` uses `TEST_PUBSUB_DRIVER` or `local`. Messages older than `pubsub::MAX_MESSAGE_AGE` (5 min) are refused;
  every `database`-driver process deletes old rows every 10 s; the Redis subscriber is checked with `PING` every 30 s.
  `pubsub::start_as_part(&app)`: start the PubSub of a process that serves one part of the app beside the others;
  under `PUBSUB_DRIVER=auto` it uses the shared driver.
- `smeltery_core::channels`: `ChannelAuthorizer` (who may receive a broadcast channel, and the events delivered in
  this process), `ChannelEvent`, `ChannelEventSender`, `ChannelEvents`, `AppBuilder::channel_authorizer`,
  `App::channel_authorizer`; `ChannelEvent::data_sha256` (the data's hash, computed once for all receivers).
- `smeltery_core::crypto` is public: `random_token`, `random_bytes`, `sha256_hex`, `constant_time_eq`. `App::sign` /
  `App::verify_signature`: HMAC-SHA256 under a key derived from `APP_KEY` per purpose. `App::derive_key(purpose)`: a
  32-byte key under the label `smeltery-app-key:`. `App::encrypt(purpose, aad, plaintext)` / `App::decrypt(purpose,
  aad, sealed)`: AES-256-GCM under the purpose's key derived from `APP_KEY`, with associated data; its keys are
  distinct from `App::derive_key` keys of the same purpose and from PubSub's own sealing keys (PubSub seals its
  messages through the same code).
- `view` module: `view(t)` / `view_with_status(status, t)` return a template as a deferred view; the view
  middleware renders it with a `RequestHost` (`ViewData` from request or response extensions, `route()` through the
  app's named routes). Render errors answer 500 with the Mold error page under `APP_DEBUG=true`. `App::views` (the
  app's Mold engine for `<root>/resources/views`) and `Settings::views_dir`. `ViewData::session` and
  `session("key")` in views; `ViewData::session_handle` and `ViewData::auth` on web routes; `RequestHost::app` /
  `RequestHost::data`.
- `view::SparkRenderer` (installed as the service `Arc<dyn SparkRenderer>`): `RequestHost` renders `@spark` and
  `@sparksScripts` through it. `view::AlloyRenderer` (service `Arc<dyn AlloyRenderer>`, behind Mold's `@alloy`,
  `@alloyHead` and `@vite`), `view::PagePayload` (built from the page object, which it serializes and escapes for a
  `<script>` element), `view::is_view`, `RequestHost::with_page_payload` / `page_payload`.
- `LOG_FILE` / `LOG_MAX_BYTES` (`Settings::log_file`, `Settings::log_max_bytes`): `run` also writes the log to a
  file (plain text, appended, parent directory created) from a dedicated thread behind a bounded queue (a full
  queue drops and counts lines instead of blocking, and the file notes how many were dropped), flushed on exit,
  rotated to one `<file>.1` backup past `LOG_MAX_BYTES` (10 MB). `logging::init`, `logging::FileLog`,
  `logging::LogGuard`, `logging::file_subscriber`. The console log on stderr is coloured only when stderr is a
  terminal and `NO_COLOR` is unset or empty, so redirected logs (a file, `docker logs`, journald) carry no escape
  codes.
- `testing::TestApp`: a fresh migrated database per test app (`TEST_DATABASE_URL`, in-memory SQLite, or
  `DATABASE_URL`), `TestApp::db`; cookie jar across requests, `acting_as`, `with_csrf`, `cookie`, `set_cookie`,
  `clear_cookies`; `with_header`, `without_header`, `with_bearer` (headers sent with every following request);
  `from_addr` (a simulated client address); `with_agents` (start the background work in a test); `post_multipart`,
  `testing::TestFile` and `testing::multipart_body`.

### Security
- The login throttle counts every attempt before the account lookup and the password check, under one lock, so 40
  parallel guesses get exactly five checks. Budgets: thirty a minute per client whatever the address, five a minute
  per address and client, twenty per five minutes per address and client network (IPv4 /24, IPv6 /48). They count
  addresses with and without an account alike, so a 429 tells nothing about which addresses are registered, and
  guesses from other networks never lock the owner out. An IPv6 client counts by its /64; a flood of new keys never
  unblocks a blocked one.
- A signed-in session is bound to the user's password hash: a password change or reset, or
  `Auth::logout_other_devices(password)`, signs out every older session, copied cookies included, and replaces the
  remember token. `logout` ends this session only and replaces the remember token with a new random one, so no
  remember-me cookie of the user signs in any more. Sessions also end `SESSION_ABSOLUTE_LIFETIME` minutes (default
  7 days, `0` = off) after they started or their user signed in (`Settings::session_absolute_lifetime`).
- Password resets mail the address stored on the account, never the typed one: `send_reset_link` stores the token
  for the account by its id, so no column collation lets two accounts share or delete each other's token, and
  `reset` changes the password of the account's row by its `id`. Users are found by the address after
  `auth::normalize_email`, and a row counts only when its stored address equals the typed one apart from ASCII case.
  A reset marks only the address its link was mailed to as verified (mailed tokens end with a tag of the address).
- `send_reset_link` sends one link a minute per address (further requests do nothing), sends the mail in the
  background outside `APP_ENV=testing` (owned by the app; `serve` and `work` wait for it on shutdown) and logs a
  failed delivery instead of returning it, so neither timing nor errors tell whether an account exists.
- Signing in (also from a remember-me cookie) gives the session a new CSRF token.
- A visitor without a session whose request stores nothing gets no session (no cookie, row or file); only responses
  that show a page create the CSRF token. Old input is at most 100 fields, 16 KB per value and 64 KB in all.
- Old input leaves out every field that looks like a secret (names holding `password`, `passcode`, `secret`,
  `token`, `apikey`, `privatekey`, `cardnumber`, … with separators left out, or a word such as `key`, `pin`, `otp`,
  `mfa`, `2fa`, `auth`, `card`, `cvv`, `answer`, `recovery`, camelCase split), and `AppBuilder::dont_flash` names
  more.
- The CSRF check covers every method except `GET`, `HEAD`, `OPTIONS` and `TRACE`, extension methods on `any` routes
  included.
- Under an https `APP_URL` the session and remember-me cookies are named with the `__Host-` prefix, and
  `Settings::secure_cookies` compares `https://` ignoring case.
- `serve` and `work` refuse to start under `APP_ENV=testing` (one refusal, given before the app is built).
- `UploadedFile::store` takes the extension from the content for PNG, JPEG, GIF, WebP and PDF and keeps a name's
  extension only when it is on an allow-list (images, audio, video, PDF, text and tables, office documents,
  archives); every other one (`html`, `svg`, `xml`, `xsd`, `mathml`, `js`, unknown ones …) is stored as `.bin`. Files
  from `public/` whose real path lies in `storage/app/public` or `public/storage` are served with
  `Content-Security-Policy: sandbox` (not PDFs), `X-Content-Type-Options: nosniff` and, except images, PDF, text,
  video and audio, `Content-Disposition: attachment`, whatever URL reached them; static-file paths with a segment
  ending in `.` or a space or holding `:` or `\` answer 404.
- A multipart body is bounded as a whole (`UPLOAD_MAX_BYTES` plus 1 MB for boundaries and part headers, 413 past
  it), to 1000 parts and in the length of a part header.
- The server runs its own accept loop: `SERVER_HEADER_TIMEOUT` (30 s) for a request's headers and for a connection
  with no request running, HTTP/2 included (an idle HTTP/2 connection is closed even while it answers pings),
  `SERVER_MAX_CONNECTIONS` (4096) open connections, `SERVER_MAX_CONNECTIONS_PER_IP` (128) per client address (IPv6 by
  /64; `TRUSTED_PROXIES` not counted; `0` = off), and `REQUEST_TIMEOUT` counted from when the request arrives, the
  `_method` body read included. `Settings` fields `server_header_timeout`, `server_max_connections`,
  `server_max_connections_per_ip`.
- Argon2 hashing is bounded process-wide: `HASH_CONCURRENCY` (default: the number of CPUs) at once, `HASH_QUEUE`
  (64) waiting, 503 beyond. `Settings` fields `hash_concurrency`, `hash_queue`.
- PubSub messages carry a random id, and each process delivers an id once: a message written into the table or
  Redis again within `MAX_MESSAGE_AGE` is skipped, counted and logged. `Message::sent_at` holds the send time. A
  message longer than a sealed envelope of `MAX_MESSAGE_BYTES` is skipped before it is decoded, and the `database`
  poll does not read such a payload. The `database` driver's read position never runs ahead of the database's clock
  (a clock step back is logged), and its prune also deletes rows dated more than 5 minutes ahead.
- `PubSub::publish` and `PubSub::forward` refuse the framework's topics `auth`, `anvil` and `sparks` (an error,
  `Forward::Dropped`); auth events go through `auth::publish_event`.
- The request log (at `debug`) shows no secrets: path segments of route parameters named like `token`, `secret`,
  `signature`, `password`, `hash` or `key` (e.g. `/reset-password/{token}`), every query value and every query item
  without a value are `[redacted]`, in mangled links too (extra slashes, other letter case, percent-encoding).
  `Settings`' `Debug` hides credentials in `MEMCACHED_SERVERS` and `APP_URL` (`user:password@` becomes `***@`), and
  database connection errors show the URL's credentials as `***`.
- Line breaks inside a logged event are written as `\n` / `\r`, on the console and in `LOG_FILE`, so a logged value
  (a database error quoting user input, a cache key) cannot start a forged line.
- On Unix the framework creates its files private: the log file `0640` (folders `0750`), `file` cache entries and
  lock files `0600` (folders `0700`), a new SQLite database file `0600` (its `-wal` / `-shm` files follow). A process
  running as root opens `LOG_FILE` only when no other user controls a folder on its path (and never through a
  symlink at the file; otherwise it logs to stderr and says why), and `storage:link` as root only adds the link to
  such a `public/` and leaves `storage/` alone.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
