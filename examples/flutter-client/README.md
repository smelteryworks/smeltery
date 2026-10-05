# Flutter Client

A Flutter app that talks to a Smeltery app the way a mobile app does: it signs in for a Hallmark API token
(`POST /api/tokens`), opens the app's Anvil socket (Pusher protocol 7) with the
[`dart_pusher_channels`](https://pub.dev/packages/dart_pusher_channels) package, and authorizes private and presence
channels at `POST /api/broadcasting/auth` with `Authorization: Bearer <token>`. It shows:

- the public `announcements` channel and an **Announce** button that broadcasts on it;
- the user's private channel `private-users.<id>`;
- a presence room `presence-chat.<room>` with its members' names and a box that sends client events (whispers);
- **Sign out** (`DELETE /api/tokens/current`), and a return to the sign-in when the server ends the token (the socket
  is closed with 4200 and authorizing again answers 401).

The socket and API code is a small pure-Dart package, [`packages/anvil_link`](packages/anvil_link), which the app
and the tests use:

| File | Does |
|---|---|
| `lib/src/api.dart` | `SmelteryApi`: `issueToken`, `user`, `revoke`, `postJson` |
| `lib/src/session.dart` | `AnvilSession`: the socket, the bearer authorizer, resubscribing after a reconnect, `subscribe`, `whisper`, `unauthenticated` |
| `lib/src/roster.dart` | `PresenceRoster`: a presence channel's members with their `user_info` |
| `lib/src/wire.dart` | a connection that records each frame and the close code |

## The server app

A new app with Anvil and Hallmark:

```sh
smeltery new chat-server --kind web --frontend mold --db sqlite --no-tailwind --no-alpine \
  --smelt watchfire,temper,anvil,hallmark --bellows none
cd chat-server
```

The generated app has the public channel `announcements`, the private channel `users.{user}`, the events
`AnnouncementPosted` and `UserNotified`, `POST /api/tokens`, `DELETE /api/tokens/current`, `GET /api/user` and the
token endpoint `/api/broadcasting/auth`. The example adds four things.

**1. A fixed app key** in `.env` (the client is built with it):

```sh
ANVIL_APP_KEY=smeltery-example-key
```

**2. A presence room with client events**, in `routes/channels.rs`:

```rust
use smeltery::anvil::{ChannelCtx, Channels, Member};

use crate::app::models::User;

pub fn channels(c: &mut Channels) {
    // … the generated channels …
    // `presence-chat.{room}`: every signed-in user may join; members see each other's name.
    c.presence("chat.{room}", |ctx: ChannelCtx| async move {
        let _room: i64 = ctx.param("room")?;
        let Some(user) = ctx.user::<User>().await? else {
            return Ok(None);
        };
        Ok(Some(
            Member::new(user.id).info(smeltery::json!({ "name": user.name })),
        ))
    })
    .whispers();
    // smeltery:channels
}
```

**3. Two routes that broadcast**, for token holders. `app/controllers/api/broadcasts.rs`:

```rust
//! Broadcasts sent by API clients.

use serde::Deserialize;
use smeltery::anvil::{Anvil, SocketId};
use smeltery::auth::Authenticated;
use smeltery::http::Json;
use smeltery::prelude::*;

use crate::app::events::announcement_posted::AnnouncementPosted;
use crate::app::events::user_notified::UserNotified;

/// The body of both routes.
#[derive(Debug, Deserialize)]
pub struct MessageBody {
    pub message: String,
}

/// `POST /api/announce`: `AnnouncementPosted` on `announcements`, except to the sender's socket (`X-Socket-ID`).
pub async fn announce(
    anvil: Anvil,
    socket: Option<SocketId>,
    Json(body): Json<MessageBody>,
) -> Result<&'static str> {
    anvil
        .send(&AnnouncementPosted {
            message: body.message,
        })
        .except(socket)
        .await?;
    Ok("sent")
}

/// `POST /api/notify-me`: `UserNotified` on the token user's channel `private-users.<id>`.
pub async fn notify_me(
    anvil: Anvil,
    who: Authenticated,
    Json(body): Json<MessageBody>,
) -> Result<&'static str> {
    anvil
        .send(&UserNotified {
            user_id: who.user_id,
            message: body.message,
        })
        .await?;
    Ok("sent")
}
```

with `pub mod broadcasts;` in `app/controllers/api/mod.rs`, and in `routes/api.rs` above `// smeltery:routes`:

```rust
    r.post(
        "/announce",
        crate::app::controllers::api::broadcasts::announce,
    )
    .middleware("auth:hallmark")
    .middleware("throttle:30,1");
    r.post(
        "/notify-me",
        crate::app::controllers::api::broadcasts::notify_me,
    )
    .middleware("auth:hallmark")
    .middleware("throttle:30,1");
```

**4. A second user** for the presence room and whispers, in `database/seeders/database_seeder.rs`, inside the `if`
after the demo user's `User::create(…).await?;`:

```rust
            User::create(
                db,
                user::ActiveModel {
                    name: Set("Second User".to_owned()),
                    email: Set("second@example.com".to_owned()),
                    email_verified_at: Set(Some(ChronoUtc::now())),
                    password: Set(hash_password("password").await?),
                    ..Default::default()
                },
            )
            .await?;
```

Then:

```sh
smeltery migrate:fresh --seed
smeltery serve
```

`serve` listens on `127.0.0.1:8000` (`SERVER_HOST`, `SERVER_PORT`).

## Run the app

```sh
flutter pub get
flutter emulators --launch <emulator id>
flutter run
```

The Android emulator reaches the development machine's `127.0.0.1` at `10.0.2.2`, which is the app's default server
on Android (`http://10.0.2.2:8000`); other platforms default to `http://127.0.0.1:8000`. Other values:

```sh
flutter run --dart-define=SMELTERY_URL=https://chat.example.com --dart-define=ANVIL_APP_KEY=<key>
```

An `https://` URL makes the socket `wss://` on the same host and port. Debug builds may use plain `http` and `ws`
(`android/app/src/debug/AndroidManifest.xml` allows cleartext traffic); release builds keep Android's default, which
refuses it.

Sign in with a seeded user (the demo user, or `second@example.com`; password `password`), then sign in as the other
user on a second device or emulator to see the room's member list change and the whispers arrive.

## Tests

```sh
flutter pub get
(cd packages/anvil_link && dart pub get)
flutter analyze
flutter test                                # the widget test, no server
cd packages/anvil_link
dart test test/unit_test.dart               # no server
# against the server app above, at SMELTERY_URL (default http://127.0.0.1:8000):
dart test test/live_test.dart test/readme_test.dart
cd ../..
flutter test integration_test -d <emulator id> --dart-define=SMELTERY_URL=http://10.0.2.2:8000
```

`readme_test.dart` runs the code of the Flutter section of Smeltery's README with the plain `dart_pusher_channels`
calls. `live_test.dart` signs both seeded users in, subscribes the public, private and presence channels, receives
broadcasts, and checks that `X-Socket-ID` leaves the sender out, that another user's private channel and an
undeclared public channel are refused, the presence member list with `member_added` / `member_removed`, whispers
(the other member receives them with the sender's `user_id`; the sender does not), and that signing a token out
closes its socket with 4200 and authorizing again answers 401. `SMELTERY_EMAIL`, `SMELTERY_EMAIL2`,
`SMELTERY_PASSWORD` and `ANVIL_APP_KEY` set other values; `WIRE_DUMP=<file>` writes every frame to a file.
`POST /api/tokens` takes 10 requests a minute from one address, and each of the two live files makes two.
