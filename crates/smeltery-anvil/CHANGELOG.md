# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added
- The `anvil` command (`smeltery anvil`): a process that serves only the socket endpoint and `/up` on
  `ANVIL_SERVER_HOST:ANVIL_SERVER_PORT` (`127.0.0.1:8080`, `--host` / `--port`), through core's server and its
  limits; it runs no background work, uses the shared PubSub driver under `PUBSUB_DRIVER=auto` and stops at start
  without one. `ANVIL_IN_SERVE=false` makes `serve` answer the socket path with 404 and use the shared driver.
  `Anvil::serves_sockets`.
- Anvil registers core's `ChannelAuthorizer` (the channel rules of `POST /broadcasting/auth` for the request's
  session) and hands every event it delivers in a process to it, so Sparks listeners receive broadcasts.
- The socket `Origin` policy also allows the origins of `CORS_ALLOWED_ORIGINS`; `null` is accepted when either list
  names it.
- Presence channels: `Channels::presence(pattern, callback)` returning a `Member` (`Member::new(id).info(json)`), the
  member signed into the subscription (`channel_data`), `subscription_succeeded` with the member list,
  `pusher_internal:member_added` / `member_removed` once per user across tabs and processes, `Anvil::members`,
  `Channel::presence`, `ANVIL_MAX_PRESENCE_MEMBERS` (100) and `ANVIL_MAX_MEMBER_BYTES` (1024). The member store
  follows the PubSub driver: memory, the `presence_sockets` / `presence_users` tables (`presence_migrations`) or Redis
  (feature `redis`); members of a crashed process are swept after 90 s.
- Client events: `.whispers()` on a private or presence registration; delivered to the other subscribers in every
  process, never to the sender, with the sender's `user_id` on presence channels; 10 a second per socket, 100 a
  second per channel in each process.
- Presence limits per socket: `ANVIL_MAX_PRESENCE_CHANNELS` (10) and a join budget (5 at once, then 1 a second); the
  presence heartbeat finishes joins and leaves cut off halfway and restores rows swept while the process was alive,
  deciding each row against the process's memberships at that moment (a join or leave during a heartbeat is kept).
- `TestSocket::subscribe_presence`, `TestSocket::whisper`, `testing::presence_of`; a dropped `TestSocket` leaves its
  presence channels.
- The socket endpoint `GET /app/<ANVIL_APP_KEY>`: the Pusher Channels protocol 7 (`pusher:connection_established`,
  `pusher:ping` / `pusher:pong`, `pusher:subscribe` / `pusher:unsubscribe`, `pusher:subscription_error`,
  `pusher:error`), the WebSocket upgrade answered by the crate (tungstenite's frame codec with 8 KiB read buffers),
  the connection's `UpgradeHold` kept for the socket's life, one socket task owned by the app that closes with 1001
  at shutdown.
- `Channels` (`public`, `private` with `{param}` patterns and `.guests()`), `ChannelCtx`, and the web route
  `POST /broadcasting/auth` (session, CSRF, `throttle:600,1`; one 403 answer for unknown, guest and denied).
- `BroadcastEvent` (with `#[derive(BroadcastEvent)]` from `smeltery-macros`), `Channel`, `Anvil::send`,
  `Anvil::to(...).event(...).with(...)`, `PendingEvent::except` and the `SocketId` extractor (`X-Socket-ID`);
  events reach the app's other processes through its PubSub (topic `anvil`).
- Signatures: HMAC-SHA256 bound to the socket id, the channel and a grant (user, session binding, five-minute
  expiry); `ANVIL_APP_SECRET` defaults to a secret derived from `APP_KEY` (purpose `anvil.secret`).
- The `Origin` policy (`APP_URL`'s origin and `ANVIL_ALLOWED_ORIGINS`; no `Origin` accepted; loopback aliases in
  local development), limits (`ANVIL_MAX_CONNECTIONS`, `ANVIL_MAX_CONNECTIONS_PER_IP`,
  `ANVIL_HANDSHAKES_PER_MINUTE`, `ANVIL_MAX_SUBSCRIPTIONS`, `ANVIL_MAX_MESSAGE_SIZE`, `ANVIL_MAX_EVENT_SIZE`, 20
  frames a second with a burst of 40, an outbox of 256, a 10 s write timeout, `ANVIL_PING_INTERVAL`,
  `ANVIL_PONG_TIMEOUT`, `ANVIL_MAX_CONNECTION_AGE`) and close codes clients reconnect after.
- `testing::AnvilSpy`, `testing::TestSocket`, `testing::authorize`, `testing::auth_of`.
- Settings checks at boot: timer values bounded (clamped from `.env` with a warning), an explicit
  `ANVIL_APP_SECRET` of at least 32 bytes outside `local` / `testing`, `ANVIL_MAX_CONNECTIONS_PER_IP` below
  `SERVER_MAX_CONNECTIONS_PER_IP`; `APP_URL`'s origin taken from its scheme and host whatever its path.
- Event names starting with `pusher:` / `pusher_internal:` are refused; a channel named twice in one event is sent
  once. The handshake budget counts clients per network once its client table is full.
- `POST /api/broadcasting/auth`: private-channel signatures for the bearer credentials of the app's stateless guards
  (401 without one, 403 without the `broadcasting` ability), `ChannelCtx::principal`.
- Revocation: core's auth events close (4200) the sockets whose private subscriptions an ended session or token
  authorized, in every serving process; a signature made before the event (or up to 30 seconds after it) cannot
  subscribe. A process refuses the signatures it cannot check: made before it started, before an event it forgot
  early, before it started listening, or before it fell behind the auth events (then it also closes the sockets
  they authorized with 4200, spread over 30 s, at most once a minute). A
  signed-in user's credential without a key a revocation can name gets no signature (500).
- Client events have a budget per client address (`ANVIL_CLIENT_EVENTS_PER_CLIENT`, 50 a second, an IPv6 client by
  its /64) and per process (`ANVIL_CLIENT_EVENTS_PER_SECOND`, 500 a second); more are dropped with `pusher:error`
  4301.
- Subscribing again to a joined presence channel costs a join from the presence join budget.
- The socket write buffer fits the largest presence member list the settings allow; settings whose member list could
  exceed 16 MiB stop the app at boot. `ANVIL_MAX_MESSAGE_SIZE` is 256 to 1048576 bytes.
- A socket closed by a revocation leaves its channels at once and joins none. An auth event this version cannot read
  ends every credential of the user it names, and one that names no user is handled like falling behind the auth
  events. Revocations are dated by when they were sent.
- The derived `ANVIL_APP_KEY` is the first 10 bytes of `App::derive_key("anvil.key")`, in hex.
- `POST /api/broadcasting/auth` answers a guard's client error (a 429 guess budget) with its status and logs it at
  debug level. The socket endpoint answers 503 to an upgrade the server offers no connection hold for.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
