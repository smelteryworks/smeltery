# smeltery-anvil

Anvil, the real-time layer of the [Smeltery](https://github.com/smelteryworks/smeltery) framework: a WebSocket server
inside the app that speaks the Pusher Channels protocol 7, events broadcast from handlers, jobs and Watchfire
agents, and channel authorization. `pusher-js`, `laravel-echo` and the Pusher client libraries for other platforms
connect to it with a custom host.

Apps use it through the facade, as `smeltery::anvil`:

```rust
use serde::Serialize;
use smeltery::anvil::prelude::*;

/// An event: its JSON is the data clients receive.
#[derive(Serialize, BroadcastEvent)]
#[broadcast(private = "orders.{order_id}")]
pub struct OrderShipped {
    pub order_id: i64,
    pub tracking: String,
}

/// `routes/channels.rs`: the public channels and who may join the private ones.
pub fn channels(c: &mut Channels) {
    c.public("news");
    c.private("orders.{order}", |ctx: ChannelCtx| async move {
        let order: i64 = ctx.param("order")?;
        let Some(user_id) = ctx.user_id() else { return Ok(false) };
        // The order's owner, from the database.
        let rows = ctx
            .db()?
            .query_with("SELECT user_id FROM orders WHERE id = ?", [order.into()])
            .await?;
        let owner: Option<i64> = rows.first().and_then(|row| row.try_get("", "user_id").ok());
        Ok(owner == Some(user_id))
    });
}

/// A handler: send the event to every socket on its channel, except the one that made this request.
async fn ship(anvil: Anvil, socket: Option<SocketId>) -> smeltery::Result<&'static str> {
    anvil
        .send(&OrderShipped { order_id: 7, tracking: "1Z999".into() })
        .except(socket)
        .await?;
    Ok("shipped")
}

fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    app.anvil(channels).routes(|r| {
        r.post("/orders/ship", ship);
    })
}
# let _ = build;
```

## What it does

- **The socket endpoint** `GET /app/<ANVIL_APP_KEY>` answers the WebSocket upgrade itself, on the app's own port
  (no extra process). A socket keeps its connection's place in the server's limits (`SERVER_MAX_CONNECTIONS`,
  `SERVER_MAX_CONNECTIONS_PER_IP`) and is closed with code 1001 when the app shuts down.
- **A separate socket process:** `smeltery anvil` serves only the socket endpoint (and `/up`) on
  `ANVIL_SERVER_HOST:ANVIL_SERVER_PORT` (`127.0.0.1:8080`), with the server's limits and shutdown; the web processes
  run with `ANVIL_IN_SERVE=false` and keep the auth endpoints. Events, revocations and presence reach it through the
  app's shared PubSub driver.
- **Channels:** public channels are declared with `c.public(...)`; a public name that is not declared is refused.
  Private channels (`private-<name>`) need a signature from `POST /broadcasting/auth`, a web route with the
  session and the CSRF check, which runs the pattern's callback for the signed-in user; clients with a bearer token
  (mobile apps, other backends) use `POST /api/broadcasting/auth`, checked by the app's stateless guards. When the
  session or token that authorized a subscription ends, its socket is closed with 4200. Channel names starting
  with `private-encrypted-` are refused.
- **Presence channels** (`presence-<name>`, `c.presence(...)`): the callback names the `Member` a user joins as;
  subscribers get the member list and `member_added` / `member_removed` once per user, across tabs and processes
  (the members live in memory, in the database or in Redis, following the app's PubSub driver).
- **Client events:** `.whispers()` on a private or presence channel lets its subscribers send `client-…` events to
  each other (never back to the sender, 10 a second per socket); elsewhere they are refused.
- **Events:** `#[derive(BroadcastEvent)]` names the channels (`{field}` fills in a field's value) and the event
  name (`App\Events\<TypeName>` by default, `#[broadcast(as = "order.shipped")]` for another);
  `anvil.send(&event).await` delivers it to this process's sockets and, through the app's PubSub, to its other
  processes. `anvil.to(Channel::public("news")).event("posted").with(&data).await` sends without an event type.
- **Signatures:** HMAC-SHA256 under `ANVIL_APP_SECRET`, by default derived from `APP_KEY`; each signature is bound
  to one socket id, one channel and the user it was made for, and is usable for five minutes.
- **Limits:** message size, frames per second, subscriptions per socket, sockets per process and per client
  address, handshakes per client address and minute, a bounded outbox per socket with a write timeout, a ping
  after silence and a maximum connection age.
- **Sparks listeners:** every event delivered in a process also reaches the app's Sparks components that listen to
  its channel (`#[on("anvil:<channel>", "<event>")]`), without JavaScript; see "Listening to broadcasts" in the
  [Sparks section](https://github.com/smelteryworks/smeltery#sparks-live-components) of the Smeltery README.
- **Testing:** `testing::AnvilSpy` records what an app sends, `testing::TestSocket` connects to the hub without a
  network, `testing::authorize` calls the auth endpoint.

The full guide (settings, clients, deployment) is the "Broadcasting" section of the
[Smeltery README](https://github.com/smelteryworks/smeltery#broadcasting).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
