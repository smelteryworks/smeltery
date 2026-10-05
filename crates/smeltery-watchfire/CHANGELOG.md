# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- The Watchfire runtime: `Watchfire` registration (`run`, `every`, `on_event`, `agent`, `pool`, `group(..).limit`,
  `rate_limit`, `job`, `schedule`) with `AgentBuilder` settings; the `Agent` trait and `agent_fn`.
- Supervision: states `starting`, `running`, `paused`, `stopping`, `stopped`, `backing_off`, `completed`, `failed`,
  `standby`; restart policies; exponential backoff with full jitter; `max_restarts(n, per)`; heartbeat stall restart;
  panic capture; outcomes `completed`, `failed`, `panicked`, `stopped`, `killed`, `stalled`, `interrupted`; the
  `Agents` handle (`start`, `stop`, `pause`, `resume`, `restart`, `add`, `remove`, `logs`, `runs`, `emit`,
  `subscribe`, `schedule`, `shutdown_token`); pools, group and global (`WATCHFIRE_MAX_CONCURRENT`) limits;
  supervision trees with `spawn_child`. An agent's run keeps running while its supervisor writes the agent's status
  to the database.
- `AgentCtx`: cancellation, heartbeat, `sleep`, `interval`, `acquire`, `http`, `rate_limited`, `app`, `db`,
  `service`, `cache` (the app's cache, e.g. for locks shared with other processes; also on `JobCtx`),
  `checkpoint` / `checkpoint_get`, `emit`, `counter`, `log`, `span`, `spawn_child`, `dispatch`.
- `http`: the `Http` client (timeouts, retries with jitter and `Retry-After`, per-host token buckets), the
  `Transport` trait, `ReqwestTransport` (reqwest 0.13, rustls with ring, the platform verifier) and
  `FakeTransport`; `RequestBuilder::repeatable`, `retries`, `max_body` and `redirects`. A `Retry-After` date with
  out-of-range fields is ignored; a cron step near `u32::MAX` parses without overflow.
- Jobs: the `Job` trait with `dispatch` / `dispatch_later`, `JobCtx`, the `Queue` (`database`, `memory` and `redis`
  drivers; atomic reservation, stale reservation release, retries, dead letters; `Queue::retry_dead`,
  `Queue::delete_dead`; `Queue::driver` names the driver), queue workers. `QUEUE_DRIVER` is trimmed. Every driver
  stores a worker's outcome (delete, retry, release, dead letter) only while its reservation (`reserved_at` +
  `attempts`) is still the one held; a worker whose reservation was released as stale and taken by another changes
  nothing and logs a warning. Dead-lettering a job (insert + delete) and retrying a dead letter (delete + insert) are
  one transaction each; a job reserved for an attempt beyond its `max_attempts` goes to the dead letters without
  running again. The `database` driver's transactions take the write lock at their start, so concurrent requeues of
  dead letters on SQLite succeed. The `memory` driver keeps the latest 1000 dead letters.
- The `redis` queue driver (feature `redis`; `QUEUE_DRIVER=redis`): jobs and dead letters on the Redis server of
  `REDIS_URL`, under `<QUEUE_PREFIX>{default}:` (`QUEUE_PREFIX` defaults to `<app name in snake case>_queue_`; the
  `{default}` hash tag keeps the queue in one hash slot). Each operation is one Lua script (`EVALSHA`), so a job is
  reserved by one worker across every process; attempts, backoff, stale reservation release, dead letters and the
  dashboard's retry and delete work as with the `database` driver. New ids never reuse the id of a job or dead letter
  that still exists. `WatchfireSettings::queue_prefix`. The app refuses to boot when `CACHE_PREFIX` and the start of
  the queue's keys, `<QUEUE_PREFIX>{default}:`, overlap (either starts with the other), or when `QUEUE_PREFIX`
  contains `{` / `}`.
- The scheduler: `Cron` (own 5-field parser), `every`, `every_minute`, `hourly`, `daily`, `daily_at`, `weekly`,
  `cron`, `Overlap`; job, call and agent targets.
- Persistence: memory and database stores; `migrations::up` / `down` for the `watchfire_*` tables (including
  `watchfire_commands` and `watchfire_runs.process`; `migrations::up_multi_process` / `down_multi_process` add and
  remove those two on their own).
- Several processes on one shared cache store (`WATCHFIRE_LOCK_STORE`, `WatchfireSettings::lock_store`, default
  `CACHE_STORE`; `database`, `redis`, `memcached`, and `file` on one machine): each agent runs in one process at a
  time, holding a lease (`WatchfireSettings::lease_ttl`, default 30 s TTL) with a hard cut-off of
  `TTL - shutdown timeout - TTL / 6` after its last renewal was sent; a singleton whose shutdown timeout the TTL
  cannot cover is refused at launch. The other processes show it as `AgentState::Standby` (with
  `AgentStatus::held_by`) and take over when the holder stops (it releases) or dies (the lease expires). Each due
  tick of a scheduled job or call runs in one process (a claim with `Cache::add`, also by `schedule:run`), and calls
  with `Overlap::Skip` / `Queue` hold a lease while they run (cancelled when it is lost). `AgentConfig::per_process`,
  `AgentBuilder::per_process` and `ScheduledTask::per_process` opt out. With a shared store `every(d)` schedules run
  on multiples of `d` since the Unix epoch. A store that does not answer at launch is coordinated through anyway
  outside `APP_ENV=local` / `testing`.
- `RunRecord::process` (`watchfire_runs.process`): the process that ran each run; runs of ended processes are marked
  `interrupted` by the others. A process back from a lock-store outage puts its runs that were marked so back to
  `running` (a run that ended meanwhile keeps its outcome), and final run records that failed to write are written
  again. An agent's run count (`AgentStatus::runs`, the dashboard's "Runs") includes the runs recorded inside its
  run: a queue worker's jobs and the scheduler's calls, also in the `watchfire_agents` row that dashboards of other
  processes read (written when such a run starts).
- `AgentsExt::agents` for `AppBuilder`: queue at boot, agents on `serve` / `work`, commands `agents:runs`,
  `schedule:list`, `schedule:run`; `WatchfireSettings`. `WATCHFIRE_IN_SERVE=false` (`WatchfireSettings::in_serve`):
  `serve` runs no agents, queue workers or scheduler and marks itself web-only (`App::set_web_only`), so
  `PUBSUB_DRIVER=auto` gives it the shared PubSub driver, like `serve --no-agents`.
- `testing::Harness` and `testing::JobHarness`; `testing::JobHarness::for_app(app)` runs jobs inside a given app (its
  database and services), e.g. a `TestApp`'s.
- The JSON API under `/_watchfire/api` (agents, the five commands, logs, runs, jobs with dead letter retry and
  delete, schedule) and the SSE stream `/_watchfire/api/events` (`snapshot`, `status`, `event`); access by
  `WATCHFIRE_DASHBOARD` (`local`, `auth`, `off`) and `Authorization: Bearer <web::api_token(APP_KEY)>`; headless
  `work` serves the API on `WATCHFIRE_API_ADDR`.
- The dashboard `GET /_watchfire`: a Mold page compiled into the crate (`views/`), command forms with CSRF and a
  flash, 5-second refresh. The forge colours of new apps (light and dark following the system setting): a header
  with the app name and `APP_ENV`, summary tiles (agents running, paused, failed or backing off, in standby; queue
  depth, dead letters, the next scheduled run), state pills, a process column, a "Heartbeat" column (`on time` /
  `stalled`), empty states, and agent cards on narrow screens. Its stylesheet is embedded in the crate and served at
  `GET /_watchfire/assets/watchfire.css?v=<version>` (cached for a year; 404 under `WATCHFIRE_DASHBOARD=off`), so
  the page needs nothing from the app. Each agent shows the buttons its state allows; stop and restart ask for a
  confirmation first: a link to `/_watchfire?agent=<name>&confirm=<action>` (the page with the confirmation open,
  without the live panels and the 5 s reload), or, in the live agents panel, its actions `ask(name, action)` and
  `cancel`. Recent runs of an earlier process are marked (run ids count per process). A query that does not decode
  is ignored.
- The live dashboard: with Sparks installed, the panels are the Spark components `watchfire.agents` (with the
  `act(name, action)` action under the dashboard's access rule) and `watchfire.queue`, refreshed through `Broadcast`
  (agents at most every 500 ms after status changes, queue every 5 s while a page is connected); `<noscript>` meta
  refresh and the POST forms as the no-JS fallback.
- One dashboard for every process: a process without Watchfire (`serve --no-agents`) or with an agent in standby
  shows the agents of the others from the shared tables, and sends their commands through the `watchfire_commands`
  table, which the holding process polls every second (the outcome, or the holder's refusal with its own status;
  202 when the holder is still carrying it out after 5 s, 504 when nobody took it, 429 when 20 wait; untaken
  commands lapse after 60 s, by the database's clock; the commands of different agents are carried out side by side,
  each agent's in order; a process that shuts down lets the commands it is carrying out finish for up to 1 s, so
  their true outcome is recorded, and closes the rest at once with 503). The API's `GET /agents`, `/agents/{name}`,
  `/runs`, `/schedule` and the commands work the same way. `GET /_watchfire/api/agents/{name}/logs` and
  `agents:logs` also work from a process that does not run the agent: the lines are read from the process that
  does, through `watchfire_commands` (the newest that fit in 60 KB; the row is deleted once read, or after a
  minute).
- `Watchfire::dashboard_gate(|auth, app| async move { … })`: decides which signed-in users may use the dashboard.
- Console commands `agents:list`, `agents:start|stop|pause|resume|restart`, `agents:logs` (through the API) and
  `agents:token` (prints the Watchfire API token, derived from `APP_KEY`, to standard output);
  `web::api_base_url(app)` is the API address the console commands call.
- Alerts: `Watchfire::on_alert`, `WATCHFIRE_ALERT_WEBHOOK`, kinds `failed`, `stalled`, `dead_letter`.
- The `mail` feature: `mail::QueueMail` (`mailer.queue(mail)`, `queue_later`) and the `mail::SendMail` job
  (registered when the app has `.mail()`), and alert mail to `WATCHFIRE_ALERT_MAIL`
  (`WatchfireSettings::alert_mail`).
- Feature `llm`: `llm::{Provider, FakeProvider, Reply, Anthropic, AnthropicConfig, Agent, Budget, Price, Outcome}`.

### Security
- The dashboard (page, forms, live panels) needs a signed-in user that the dashboard gate admits; without a gate
  nobody gets in. `WATCHFIRE_DASHBOARD=auth` asks for the gate also under `APP_ENV=local`. Guests get a redirect to
  `login` (401 for JSON clients), refused users 403. The live panels hand out Sparks stream tokens only to viewers
  the gate admits (`stream_hook`), so a guest cannot subscribe to their refreshes.
- Without signing in, the dashboard and the token-less API are open only under `APP_ENV=local` with
  `WATCHFIRE_DASHBOARD=local` and an `APP_URL` on this machine, to requests from a loopback address that carry no
  reverse-proxy or CDN header (`Forwarded`, `X-Forwarded-*`, `X-Real-IP`, `Via`, `CF-Connecting-IP` and others), with
  a `Host` on this machine (`localhost`, `*.localhost`, a loopback address or the `APP_URL` host). Requests another
  site sent are refused: a foreign `Origin` (host and port compared with the request's own or `APP_URL`'s), more than
  one `Origin`, or `Sec-Fetch-Site` other than `same-origin` / `none` outside a top-level `GET` navigation; a URI
  authority that disagrees with `Host` is refused too.
- The dashboard is mounted on the public test key only under `APP_ENV=testing` with an empty `APP_KEY` (core's
  rule); a short or malformed key in `testing` does not count as one.
- The dashboard page is sent with `X-Frame-Options: DENY` and `Content-Security-Policy: frame-ancestors 'none'`.
- The headless API server (`WATCHFIRE_API_ADDR`) has a 10 s header-read timeout (also between requests), serves at
  most 64 connections at once and sends `X-Content-Type-Options: nosniff`.
- The console commands refuse to send the API token over plain `http://` to an address that is not this machine
  (`web::api_base_url` refuses such an address), follow no redirect, and print control characters from the app
  (terminal escape sequences in log lines) as `U+FFFD`.
- `WATCHFIRE_MAX_PAYLOAD` (`WatchfireSettings::max_payload`, default 1 MiB): `dispatch` / `Queue::push_raw` refuse a
  larger payload; a stored job with a larger payload goes to the dead letters without being read or run (the dead
  letter keeps the stored payload). A payload that does not decode is dead-lettered with an error naming the problem
  and its position, never a value of the payload. Stored error texts (runs, agents, dead letters, alert messages)
  are cut to 8 KiB.
- A `redis://` `REDIS_URL` with a password to another machine logs a warning when the Redis queue starts.
- A response size limit for `ctx.http()`: `HttpOptions::max_body` (`WATCHFIRE_HTTP_MAX_BODY`,
  `WatchfireSettings::http_max_body`; default 10 MiB) and `RequestBuilder::max_body`; a larger body fails with
  `HttpError::TooLarge` (`TransportError::TooLarge`, `Request::max_body`), and `ReqwestTransport` stops reading at
  the limit.
- `ctx.http()` follows redirects itself, by the policy `Redirects` (`Follow`, the default; `SameOrigin`; `None`),
  `HttpOptions::redirects` / `max_redirects` (5) and `RequestBuilder::redirects`: every hop waits for its own host's
  rate limit, a hop to another origin carries no `Authorization`, cookie or key header and no request body, `https`
  never redirects to `http`, and targets other than `http` / `https` are refused; a refused redirect is
  `HttpError::Redirect`. `Anthropic` follows no redirect, and its `Debug` output hides the API key.
- Errors and logs of `ctx.http()` carry no query strings: reqwest's errors are stripped of their URL and name it as
  `scheme://host:port/path`, `HttpError::InvalidUrl` does not repeat the URL, the retry log names the host, and a
  failed alert webhook delivery logs the webhook's host only. `WatchfireSettings`' `Debug` shows only the webhook's
  host.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
