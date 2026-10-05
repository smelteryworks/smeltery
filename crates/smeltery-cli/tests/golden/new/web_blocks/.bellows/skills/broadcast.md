# Skill: broadcast an event and authorize a channel

The app has WebSockets and broadcasting (Anvil, `.anvil(routes::channels::channels)` in `bootstrap/app.rs`).
Clients speak the Pusher protocol; channels are declared in `routes/channels.rs`, events live in `app/events/`.

1. Declare the channel in `routes/channels.rs` above `// smeltery:channels`. A public channel
   (`c.public("scores.{game}")`) is readable by anyone with the app key: only public data goes there. A private
   channel decides who may join (the client subscribes to `private-orders.7`):

   ```rust
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
   ```
2. Add the event to `app/events/` (and `pub mod` it in `app/events/mod.rs` above `// smeltery:mods`):
   `#[derive(Serialize, BroadcastEvent)]` with `#[broadcast(private = "orders.{order_id}")]`; its JSON is what
   clients receive (`#[serde(skip)]` keeps a field out), under the name `App\Events\<TypeName>` unless
   `#[broadcast(as = "…")]` names it.
3. Send it: a handler takes `anvil: smeltery::anvil::Anvil` and calls `anvil.send(&event).await?`
   (`.except(socket)` with `socket: Option<smeltery::anvil::SocketId>` leaves out the client that made the request);
   jobs and agents use `smeltery::anvil::Anvil::of(&app)`. Send after a database transaction commits, not inside it.
4. Receive it in the pages:
   a listening Spark, without JavaScript: `smeltery make:spark OrderStatus --listen "private-orders.{order_id}"
   --event OrderShipped` writes a `#[spark(stream)]` component with an `#[on(…)]` method; fill its event struct with
   the fields it reads and show it with `@spark("order_status", { order_id: … })`.
5. Test it with `smeltery::anvil::testing`: `AnvilSpy::of(app.app())` records what was sent, `TestSocket::connect`
   subscribes without a network, `authorize(&app, socket_id, channel, None)` asks the auth endpoint as the test's
   signed-in user. See `tests/broadcasting.rs`.
6. Run `smeltery test`.
