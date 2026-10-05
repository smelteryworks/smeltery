//! The auth endpoint for presence channels (the member, its bounds, the signature over it) and presence through
//! `TestSocket` (the in-memory store of a `TestApp`).
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde_json::{Value, json};
use smeltery_anvil::testing::{TestSocket, authorize, presence_of};
use smeltery_anvil::{AnvilExt as _, ChannelCtx, Channels, Member};
use smeltery_core::testing::TestApp;

fn channels(c: &mut Channels) {
    c.presence("room.{room}", |ctx: ChannelCtx| async move {
        let room: i64 = ctx.param("room")?;
        Ok(ctx
            .user_id()
            .filter(|_| room < 100)
            .map(|id| Member::new(id).info(json!({ "name": format!("user {id}") }))))
    })
    .whispers();
    // A callback that returns members no client could join as.
    c.presence("broken.{kind}", |ctx: ChannelCtx| async move {
        let kind: String = ctx.param("kind")?;
        Ok(Some(match kind.as_str() {
            "big" => Member::new(1).info(json!({ "bio": "x".repeat(2_000) })),
            _ => Member::new(""),
        }))
    });
    c.presence("lobby", |_| async { Ok(Some(Member::new("visitor"))) })
        .guests();
}

fn app() -> TestApp {
    TestApp::new(|b| b.anvil(channels))
}

fn data(frame: &Value) -> Value {
    serde_json::from_str(frame["data"].as_str().unwrap()).unwrap()
}

#[test]
fn the_endpoint_signs_the_member_and_the_socket_joins_with_it() {
    let app = app();
    app.acting_as(7);
    let mut socket = TestSocket::connect(app.app());
    let res = authorize(&app, socket.socket_id(), "presence-room.1", None);
    assert_eq!(res.status(), 200, "{}", res.text());
    let (auth, channel_data) = presence_of(&res).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&channel_data).unwrap(),
        json!({ "user_id": "7", "user_info": { "name": "user 7" } })
    );
    let answer = socket.subscribe_presence("presence-room.1", &auth, &channel_data);
    assert_eq!(answer["event"], "pusher_internal:subscription_succeeded");
    assert_eq!(data(&answer)["presence"]["ids"], json!(["7"]));
    // The member cannot be changed by the client.
    let mut other = TestSocket::connect(app.app());
    let res = authorize(&app, other.socket_id(), "presence-room.1", None);
    let (auth, _) = presence_of(&res).unwrap();
    let forged = other.subscribe_presence("presence-room.1", &auth, r#"{"user_id":"1"}"#);
    assert_eq!(forged["event"], "pusher:subscription_error");
}

#[test]
fn refusals_and_bad_members() {
    let app = app();
    let socket = TestSocket::connect(app.app());
    // A guest on a pattern without `.guests()`.
    assert_eq!(
        authorize(&app, socket.socket_id(), "presence-room.1", None).status(),
        403
    );
    // `.guests()` lets them in as whom the callback says.
    let res = authorize(&app, socket.socket_id(), "presence-lobby", None);
    assert_eq!(res.status(), 200);
    assert_eq!(presence_of(&res).unwrap().1, r#"{"user_id":"visitor"}"#);
    app.acting_as(7);
    // The callback says no.
    assert_eq!(
        authorize(&app, socket.socket_id(), "presence-room.100", None).status(),
        403
    );
    // A member larger than ANVIL_MAX_MEMBER_BYTES, or without a user id: the app's mistake, never a grant.
    for channel in ["presence-broken.big", "presence-broken.empty"] {
        let res = authorize(&app, socket.socket_id(), channel, None);
        assert_eq!(res.status(), 500, "{channel}");
        assert!(presence_of(&res).is_none());
    }
    // A private answer carries no channel_data.
    assert_eq!(
        authorize(&app, socket.socket_id(), "private-room.1", None).status(),
        403,
        "no private pattern"
    );
}

#[test]
fn test_sockets_see_members_come_and_go() {
    let app = app();
    let join = |user: i64| {
        app.acting_as(user);
        let mut socket = TestSocket::connect(app.app());
        let res = authorize(&app, socket.socket_id(), "presence-room.1", None);
        let (auth, channel_data) = presence_of(&res).unwrap();
        let answer = socket.subscribe_presence("presence-room.1", &auth, &channel_data);
        (socket, answer)
    };
    let (mut ada, _) = join(7);
    let (mut bob, answer) = join(8);
    assert_eq!(data(&answer)["presence"]["count"], 2);
    let events = ada.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "pusher_internal:member_added");
    assert!(bob.events().is_empty(), "no member_added for oneself");
    // Whispers to the others only.
    assert!(
        bob.whisper("presence-room.1", "client-wave", &json!(1))
            .is_empty()
    );
    let wave = ada.events();
    assert_eq!(wave[0]["user_id"], "8");
    assert!(bob.events().is_empty());
    drop(bob);
    let events = ada.events();
    assert_eq!(events[0]["event"], "pusher_internal:member_removed");
    assert_eq!(data(&events[0]), json!({ "user_id": "8" }));
}
