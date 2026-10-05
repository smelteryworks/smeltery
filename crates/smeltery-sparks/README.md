# smeltery-sparks

Sparks are the live components of the [Smeltery](https://github.com/smelteryworks/smeltery) framework: a Rust struct (the
state) with a Mold view and async actions, rendered on the server and kept in sync with the browser by a small
client runtime (`js/sparks.js`, embedded in this crate and served at `/_sparks/sparks.js`). Apps use it through the
facade as `smeltery::sparks`, with `#[derive(smeltery::Spark)]` and `#[smeltery::actions]`; the full guide is the
"Sparks: live components" section of the Smeltery README.

```rust
use smeltery::prelude::*;
use serde::{Deserialize, Serialize};
# use smeltery::json;

#[derive(Serialize, Deserialize, Default, Spark)]
#[spark(name = "counter")]          // resources/views/sparks/counter.mold.html
pub struct Counter {
    pub count: i64,
    #[spark(model)]
    pub step: i64,
}

#[actions]
impl Counter {
    pub async fn increment(&mut self, ctx: &mut SparkCtx) -> Result<()> {
        self.count += self.step;
        ctx.dispatch("counted", json!({ "count": self.count }));
        Ok(())
    }
}

// app/sparks/mod.rs: `pub fn register(s: &mut Sparks) { s.add::<counter::Counter>(); }`
// bootstrap/app.rs:  `app.sparks(app::sparks::register)` (`smeltery::sparks::SparksExt`)
# fn main() {}
```

What it has:

- `SparksExt::sparks(register)`: the registry, `@spark("name", { props })` and `@sparksScripts` in Mold views, and
  the routes `POST /_sparks/update`, `POST /_sparks/upload` (session + CSRF), `GET /_sparks/sparks.js`,
  `GET /_sparks/stream`.
- Signed snapshots (HMAC-SHA256 under a key derived from `APP_KEY`), bound to the session and the signed-in user they
  were rendered for and accepted for the session lifetime (`Sparks::snapshot_ttl`); a tampered, foreign or expired
  snapshot, a CSRF mismatch or another protocol version answers 419 and the page reloads.
- Only `#[spark(model)]` fields accept updates (never an object; a struct field takes the keys listed in
  `#[spark(model(fields = "…"))]`) and only the `pub async fn` methods of the `#[actions]` block are callable;
  `#[guard(auth)]` / `#[guard(guest)]` on actions; `mount`, `updated` and `rendering` hooks. The state's keys are the
  field names: `#[serde(rename …)]`, `alias` and `flatten` on a Spark are compile errors.
- Request limits on the registry: `max_components` (default 1), `max_calls` (default 50), `upload_quota` (default 30
  files and 100 MiB per session per 10 minutes), `upload_address_quota` (default 120 files and 400 MiB per client
  address in that window), `max_streams` (default 1000).
- `SparkCtx`: `app`, `db`, `auth`, `user_id`, `session`, `prop`, `redirect` (paths on this site and `APP_URL`
  addresses), `redirect_away` (`http`/`https` URLs), `dispatch`, `flash`, `validate`.
- Nested components keyed by `key` (or position), each with its own state.
- `TemporaryUpload` fields with size and extension rules, `store` / `store_as` into `storage/app/{public,private}`.
- An Alpine.js bridge in `sparks.js` (with Alpine 3.13 or newer on the page): `$spark` reads and sets fields, calls
  actions and binds Alpine properties to fields (`$spark.$entangle('open')`, `.live`); the morph keeps Alpine's state.
- `Broadcast`: server push over server-sent events (`to(target).refresh()`, `.emit(event, payload)`), to the pages of
  every process of the app through its PubSub. Stream tokens are bound to the session and user of the render; a
  stream ends when that session signs out and when its tokens expire.
- Listeners: `#[on("anvil:private-orders.{order_id}", "OrderShipped")]` on a method of a `#[spark(stream)]`
  component runs it when that event is broadcast on that channel (with Anvil installed). Channels are authorized for
  the viewer at render, when the stream opens and before the method runs; the event reaches the page as a message
  signed for the instance (`sparks.listen`), which runs once (a seen-set in the app's cache) and at most 60 seconds
  after it was sent, only when the state names the channel at the moment the listener runs.
- `testing::TestSpark`: drives a component on a `TestApp` page; `testing::BroadcastSpy` records what a `Broadcast` pushes.

Licensed under either of Apache License 2.0 or MIT license at your option.
