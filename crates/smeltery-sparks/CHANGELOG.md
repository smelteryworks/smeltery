# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- Sparks, wire protocol 3: the `Spark` and `Actions` traits (implemented by `#[derive(Spark)]` and `#[actions]` from
  `smeltery-macros`), `Guard`, `ActionInfo`, `UploadRule`, the `Sparks` registry and `SparksExt::sparks`.
  `smeltery_sparks::extend(app, register)`: framework crates add components at boot.
- `@spark` / `@sparksScripts` rendering through core's `SparkRenderer` seam; `mount` and `updated` hooks; the
  `rendering` hook (`Actions::rendering_hook`; `async fn rendering` in `#[actions]`) runs before every render,
  `$refresh` included.
- `POST /_sparks/update`: signed snapshots (HMAC-SHA256, key derived from `APP_KEY`), 419 on a bad checksum or
  protocol version, model-field and action allow-lists, guards, input coercion, validation errors rendered in the
  component, effects (redirect, browser events), flash, nested components keyed by `key`. `wire:click` arguments
  decode `\uXXXX`, `\n`, `\t` and the other JavaScript escapes, so values written with Mold's `json` filter arrive
  unchanged. `wire:model="form.title"` shows key `title` of field `form`.
- `SparkCtx` (`app`, `db`, `auth`, `user_id`, `session`, `id`, `name`, `prop`, `redirect`, `redirect_away`,
  `dispatch`, `flash`, `validate`, `error`).
- Uploads: `POST /_sparks/upload` with signed, session-bound, expiring tokens; size and extension rules;
  `TemporaryUpload` (`store`, `store_as`, `bytes`, `temp_path`); temp files wait in `storage/framework/sparks/`
  (framework scratch, so `storage/app/` holds only user files) and are cleaned up after 24 hours.
  `#[validate(...)]` on upload fields: `TemporaryUpload` implements `AsSubject` as a `Subject::Upload` (name and type
  included), so `#[validate(mimes = "…")]` works on upload fields.
- `Broadcast` and `GET /_sparks/stream` (server-sent events, bounded channel, keep-alive, ends on shutdown).
  `Broadcast` reaches the pages of the app's other processes: `refresh()` and `emit()` also forward the message
  through the app's PubSub (never blocking; the return value counts the pages of the sending process), and each
  `serve` process hands what the others send to its own streams. An agent in `work` refreshes pages held by
  `serve --no-agents`.
- Listeners: `#[on("anvil:private-orders.{order_id}", "OrderShipped")]` on a method of a `#[spark(stream)]`
  component runs it when that event is broadcast on that channel (with Anvil installed), in any process of the app.
  The channel is authorized for the viewer at render, when the stream opens and before the method runs; the event
  reaches the page as a `listen` message signed for the instance, sent back with `$listen`, run once (remembered in
  the app's cache for 60 s), at most 60 seconds after it was sent, and only when the state names the channel when
  the listener runs. The window event `anvil:<event>` carries the data for Alpine code. `ListenerInfo`,
  `Actions::LISTENERS` and `Actions::listen`. Boot fails for listeners without `stream`, without Anvil, or naming a
  missing field.
- The client runtime `js/sparks.js` (`SPARKS_JS`), served at `GET /_sparks/sparks.js` with long caching. It reopens
  a stream after an error with the tokens of the latest renders, after 5 s doubling to 5 minutes, and not after the
  server ended it for a sign-out (`{"kind":"end"}`).
- The morph: a re-render that adds an element with `autofocus` focuses the first such new element once, after its
  `wire:model` value is filled (browsers ignore `autofocus` on inserted nodes), unless focus is on an element outside
  that Spark; attributes whose names `setAttribute` refuses (Alpine's `@click`) are copied.
- The Alpine.js bridge in `sparks.js`: with Alpine 3.13 or newer on the page, the magic `$spark` reads and sets the
  fields of the Spark around an element (`$spark.step = 2` deferred, `$spark.$set` sent at once), calls its actions
  (`$spark.save()`, `$spark.$call`), and binds Alpine properties to fields (`$spark.$entangle('open')`, `.live`).
  Requests go through the same queue and allow-lists as `wire:*`. `wire:model` input shows in `$spark` at once. An
  action called while the morph renders bindings is not sent (a console warning says so); with an Alpine older than
  3.13, `$spark` is not registered and a console warning says so. The morph keeps Alpine's state: it renders Alpine's
  bindings into the new HTML first (`Alpine.cloneNode`), keeps `x-data` elements, `x-model` values and the elements
  of `x-for` / `x-if`.
- `testing::TestSpark`, `testing::post_update`, `testing::stream_token` and `testing::BroadcastSpy` (with `Pushed`:
  records what a `Broadcast` pushes, for tests of jobs and agents).

### Security
- Wire protocol 3: snapshots carry the session they were rendered for (a hash of its CSRF secret), the signed-in
  user and the issue time; the memo's `l` holds the listener grants. A snapshot from another session or user, or
  older than the session lifetime (`Sparks::snapshot_ttl`), answers 419. Snapshots and stream tokens of other protocol
  versions are refused (419; the page reloads).
- Spark update responses render the CSRF token masked per response (`Session::csrf_token`), like pages.
- `POST /_sparks/update` takes one component per request (`Sparks::max_components`) and 50 calls per component
  (`Sparks::max_calls`); a larger request answers 413 before any snapshot is opened. With several components
  allowed, all are checked before any runs, and a component that fails after others ran gets an `error` entry next
  to their results.
- A `#[spark(model)]` value never holds an object and never sets a struct (a JSON array serde would read as one is
  refused too; 403, checked before anything of the component runs); a struct field takes only the keys listed in
  `#[spark(model(fields = "…"))]` (`form.title`, or `form` with an object of listed keys).
- `SparkCtx::redirect` accepts paths on this site and `APP_URL` addresses; `SparkCtx::redirect_away` accepts
  `http`/`https` URLs; any other target fails the update (500). `sparks.js` follows only `http`/`https` redirects.
- `GET /_sparks/stream` subscribes only through signed stream tokens (`?t=`; `wire:stream="<token>"` on each render
  of a `#[spark(stream)]` component, valid for the snapshot time to live) and answers 403 without one.
  `Actions::stream_hook` (`async fn can_stream(&mut self, ctx)` in `#[actions]`) decides per render whether the
  visitor gets a token. `stream_token(app, name, id, session, auth)` issues one, for the given viewer, for wrappers
  rendered outside the mount path; `testing::stream_token` mints one for tests.
- Stream tokens are bound to the session and signed-in user of the render and carry the render's listener grants:
  `GET /_sparks/stream` accepts a token only with that session's cookie (read without storing anything), and the
  stream ends when that session signs out (logout, `logout_other_devices`, password change or reset,
  `end_credentials`, in any process), when it misses auth events, and when its tokens expire. A token copied out of
  a page stops working at that logout.
- `GET /_sparks/stream` keeps at most `Sparks::max_streams` connections open (default 1000); past that it answers
  503. It allows 60 opens a minute per client address, an IPv6 client by its /64 (`Sparks::stream_opens_per_minute`,
  429), takes the connection slot before any session read or channel check, and checks at most 64 listener channels
  per request.
- `sparks.js` sends `$listen` calls in requests of their own, so a refused listen message (expired, out of order,
  access lost) never takes the visitor's queued input and actions with it.
- `Broadcast` forwards its messages through the framework's own PubSub path; the topic `sparks` is refused to app
  code (`PubSub::publish` / `forward`), so only Sparks pushes refresh components in other processes.
  `Broadcast::emit` refuses event names starting with `anvil:`.
- Uploads have a per-session quota (`Sparks::upload_quota`, default 30 files and 100 MiB per 10 minutes) and a
  per-client-address quota (`Sparks::upload_address_quota`, default 120 files and 400 MiB in the same window); past
  either `POST /_sparks/upload` answers 429. The bytes count as they arrive, and an upload that does not finish (an
  error, too large, or the request cut off by `REQUEST_TIMEOUT` or a closed connection) leaves no temp file. Temp
  files are created new (never an existing file); on Unix upload files are `0640` and the folders Sparks creates
  `0750`.
- `TemporaryUpload::store` keeps a file's extension only when it is on core's allow-list
  (`smeltery_core::http::is_safe_extension`: images, audio, video, PDF, text and tables, office documents,
  archives); any other one (`html`, `svg`, `xml`, `xsd`, `js`, unknown ones …) is stored as `.bin`.
- `$spark` never copies `__proto__`, `constructor` or `prototype` from the state into its reactive copy.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
