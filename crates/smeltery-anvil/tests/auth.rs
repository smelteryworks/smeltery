//! `POST /broadcasting/auth` (session and CSRF), private subscriptions with its answer, sends and the test helpers.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde::Serialize;
use serde_json::json;
use smeltery_anvil::testing::{AnvilSpy, TestSocket, auth_of, authorize};
use smeltery_anvil::{
    Anvil, AnvilExt as _, BroadcastEvent, Channel, ChannelCtx, Channels, SocketId,
};
use smeltery_core::session::Session;
use smeltery_core::testing::TestApp;
use smeltery_core::{AppBuilder, Error};

#[derive(Serialize, BroadcastEvent)]
#[broadcast(crate = "smeltery_anvil", private = "orders.{order_id}")]
struct OrderShipped {
    order_id: i64,
    tracking: String,
}

fn channels(c: &mut Channels) {
    c.public("news");
    // User 7 owns order 7 (and only that one).
    c.private("orders.{order}", |ctx: ChannelCtx| async move {
        let order: i64 = ctx.param("order")?;
        Ok(ctx.user_id() == Some(order))
    });
    c.private("lobby", |ctx: ChannelCtx| async move {
        Ok(ctx.user_id().is_none())
    })
    .guests();
    c.private("broken", |_| async {
        Err(Error::internal("the database is down"))
    });
    c.private("missing.{id}", |_| async { Err(Error::not_found()) });
}

fn build(b: AppBuilder) -> AppBuilder {
    b.anvil(channels).routes(|r| {
        r.get("/token", |session: Session| async move {
            session.csrf_token().unwrap_or_default()
        });
    })
}

#[test]
fn the_owner_gets_a_signature_that_subscribes_her_socket() {
    let app = TestApp::new(build);
    let mut socket = TestSocket::connect(app.app());
    app.acting_as(7);
    let res = authorize(&app, socket.socket_id(), "private-orders.7", None);
    assert_eq!(res.status(), 200, "{}", res.text());
    assert_eq!(res.header("cache-control"), Some("no-store"));
    let auth = auth_of(&res).unwrap();
    assert!(auth.starts_with(&format!("{}:7.", Anvil::of(app.app()).unwrap().app_key())));
    let answer = socket.subscribe("private-orders.7", Some(&auth));
    assert_eq!(answer["event"], "pusher_internal:subscription_succeeded");

    // The event reaches the subscribed socket, with its data as a JSON string.
    let anvil = Anvil::of(app.app()).unwrap();
    let delivered = app
        .block_on(async {
            anvil
                .send(&OrderShipped {
                    order_id: 7,
                    tracking: "1Z".into(),
                })
                .await
        })
        .unwrap();
    assert_eq!(delivered.local, 1);
    let events = socket.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "App\\Events\\OrderShipped");
    assert_eq!(events[0]["channel"], "private-orders.7");
    let data: serde_json::Value =
        serde_json::from_str(events[0]["data"].as_str().unwrap()).unwrap();
    assert_eq!(data, json!({ "order_id": 7, "tracking": "1Z" }));

    // `except` leaves the requesting socket out.
    let me = SocketId::parse(socket.socket_id()).unwrap();
    let delivered = app
        .block_on(async {
            anvil
                .send(&OrderShipped {
                    order_id: 7,
                    tracking: "2Z".into(),
                })
                .except(me)
                .await
        })
        .unwrap();
    assert_eq!(delivered.local, 0);
    assert!(socket.events().is_empty());
}

#[test]
fn a_signature_for_one_socket_fails_on_another() {
    let app = TestApp::new(build);
    let socket = TestSocket::connect(app.app());
    let mut other = TestSocket::connect(app.app());
    app.acting_as(7);
    let auth = auth_of(&authorize(
        &app,
        socket.socket_id(),
        "private-orders.7",
        None,
    ))
    .unwrap();
    let answer = other.subscribe("private-orders.7", Some(&auth));
    assert_eq!(answer["event"], "pusher:subscription_error");
    let data: serde_json::Value = serde_json::from_str(answer["data"].as_str().unwrap()).unwrap();
    assert_eq!(data["status"], 401);
}

#[test]
fn guests_others_and_unknown_channels_get_the_same_403() {
    let app = TestApp::new(build);
    let socket = TestSocket::connect(app.app());
    // Not signed in.
    let guest = authorize(&app, socket.socket_id(), "private-orders.7", None);
    assert_eq!(guest.status(), 403);
    // Signed in as another user: the callback says no.
    app.acting_as(8);
    let other = authorize(&app, socket.socket_id(), "private-orders.7", None);
    // No such pattern.
    let unknown = authorize(&app, socket.socket_id(), "private-nothing.here", None);
    // A parameter the callback cannot parse, a callback answering 404: denials too.
    let unparsable = authorize(&app, socket.socket_id(), "private-orders.abc", None);
    let missing = authorize(&app, socket.socket_id(), "private-missing.1", None);
    // A presence channel without a presence pattern.
    let presence = authorize(&app, socket.socket_id(), "presence-orders.8", None);
    for res in [&guest, &other, &unknown, &unparsable, &missing, &presence] {
        assert_eq!(res.status(), 403);
        assert_eq!(res.json(), json!({ "error": "Forbidden" }));
        assert_eq!(res.header("cache-control"), Some("no-store"));
    }
}

#[test]
fn guest_patterns_let_guests_reach_the_callback() {
    let app = TestApp::new(build);
    let socket = TestSocket::connect(app.app());
    let res = authorize(&app, socket.socket_id(), "private-lobby", None);
    assert_eq!(res.status(), 200);
    assert!(auth_of(&res).unwrap().contains(":g."), "a guest grant");
    app.acting_as(7);
    assert_eq!(
        authorize(&app, socket.socket_id(), "private-lobby", None).status(),
        403
    );
}

#[test]
fn a_failing_callback_is_a_server_error_not_a_grant() {
    let app = TestApp::new(build);
    let socket = TestSocket::connect(app.app());
    app.acting_as(7);
    let res = authorize(&app, socket.socket_id(), "private-broken", None);
    assert_eq!(res.status(), 500);
    assert!(auth_of(&res).is_none());
    assert!(
        !res.text().contains("database is down"),
        "details stay in the log"
    );
}

#[test]
fn bad_requests_are_400() {
    let app = TestApp::new(build);
    app.acting_as(7);
    let res = authorize(&app, "1.2.3", "private-orders.7", None);
    assert_eq!(res.status(), 400);
    let res = authorize(&app, "1.2", "private-orders#7", None);
    assert_eq!(res.status(), 400);
    let res = authorize(&app, "1.2", "news", None);
    assert_eq!(res.status(), 400, "public channels need no signature");
    let res = app.post_form("/broadcasting/auth", &[("socket_id", "1.2")]);
    assert_eq!(res.status(), 400);
}

#[test]
fn json_requests_work_too() {
    let app = TestApp::new(build);
    app.acting_as(7);
    let res = app.post_json(
        "/broadcasting/auth",
        &json!({ "socket_id": "1.2", "channel_name": "private-orders.7" }),
    );
    assert_eq!(res.status(), 200, "{}", res.text());
    assert!(auth_of(&res).is_some());
}

#[test]
fn the_cookie_endpoint_needs_the_csrf_token() {
    let app = TestApp::new(build).with_csrf();
    app.acting_as(7);
    let token = app.get("/token").text();
    assert_eq!(
        authorize(&app, "1.2", "private-orders.7", None).status(),
        419,
        "no token"
    );
    assert_eq!(
        authorize(&app, "1.2", "private-orders.7", Some("not-the-token")).status(),
        419,
        "a wrong token"
    );
    let res = authorize(&app, "1.2", "private-orders.7", Some(&token));
    assert_eq!(res.status(), 200, "{}", res.text());
}

#[test]
fn public_channels_must_be_declared() {
    let app = TestApp::new(build);
    let mut socket = TestSocket::connect(app.app());
    assert_eq!(
        socket.subscribe("news", None)["event"],
        "pusher_internal:subscription_succeeded"
    );
    assert_eq!(
        socket.subscribe("secret-news", None)["event"],
        "pusher:subscription_error"
    );
    // Without a signature a private channel is refused.
    assert_eq!(
        socket.subscribe("private-orders.7", None)["event"],
        "pusher:subscription_error"
    );
}

#[test]
fn the_spy_records_sends_and_events_reach_public_subscribers() {
    let app = TestApp::new(build);
    let spy = AnvilSpy::of(app.app());
    assert!(spy.nothing_sent());
    let mut socket = TestSocket::connect(app.app());
    socket.subscribe("news", None);
    let anvil = Anvil::of(app.app()).unwrap();
    app.block_on(async {
        anvil
            .to(Channel::public("news"))
            .event("posted")
            .with(&json!({ "id": 3 }))
            .await
    })
    .unwrap();
    assert!(spy.sent_on("news", "posted"));
    assert_eq!(spy.sent()[0].json(), json!({ "id": 3 }));
    assert_eq!(socket.events()[0]["event"], "posted");
    socket.unsubscribe("news");
    app.block_on(async {
        anvil
            .to(Channel::public("news"))
            .event("posted")
            .with(&json!({ "id": 4 }))
            .await
    })
    .unwrap();
    assert!(socket.events().is_empty(), "unsubscribed");
}

#[test]
fn oversized_and_invalid_events_are_errors_and_reach_nobody() {
    let app = TestApp::new(build);
    let mut socket = TestSocket::connect(app.app());
    socket.subscribe("news", None);
    let anvil = Anvil::of(app.app()).unwrap();
    let big = "x".repeat(anvil.settings().max_event_size + 1);
    let err = app
        .block_on(async {
            anvil
                .to(Channel::public("news"))
                .event("big")
                .with(&json!({ "text": big }))
                .await
        })
        .unwrap_err();
    assert!(err.to_string().contains("ANVIL_MAX_EVENT_SIZE"), "{err}");
    let err = app
        .block_on(async {
            anvil
                .to(Channel::public("bad channel"))
                .event("x")
                .with(&json!({}))
                .await
        })
        .unwrap_err();
    assert!(err.to_string().contains("invalid channel"), "{err}");
    let err = app
        .block_on(async {
            anvil
                .to(Channel::public("news"))
                .event("")
                .with(&json!({}))
                .await
        })
        .unwrap_err();
    assert!(err.to_string().contains("event name"), "{err}");
    assert!(socket.events().is_empty());
}

fn build_fails(build: impl FnOnce(AppBuilder) -> AppBuilder + std::panic::UnwindSafe) -> String {
    let result = std::panic::catch_unwind(|| TestApp::new(build));
    match result {
        Ok(_) => panic!("the build should fail"),
        Err(payload) => payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default(),
    }
}

#[test]
fn mistakes_fail_the_build() {
    let message = build_fails(|b| {
        b.anvil(|c| {
            c.public("news");
            c.public("news");
        })
    });
    assert!(message.contains("registered twice"), "{message}");
    let message = build_fails(|b| {
        b.anvil(|c| {
            c.public("private-x");
        })
    });
    assert!(message.contains("prefix"), "{message}");
    let message = build_fails(|b| {
        let mut settings = smeltery_anvil::Settings::from_env(b.settings());
        settings.max_connections = b.settings().server_max_connections;
        b.anvil_with(settings, |_| {})
    });
    assert!(message.contains("SERVER_MAX_CONNECTIONS"), "{message}");
    let message = build_fails(|b| {
        let mut settings = smeltery_anvil::Settings::from_env(b.settings());
        settings.allowed_origins = vec!["not an origin".into()];
        b.anvil_with(settings, |_| {})
    });
    assert!(message.contains("ANVIL_ALLOWED_ORIGINS"), "{message}");
    let message = build_fails(|b| {
        let mut settings = smeltery_anvil::Settings::from_env(b.settings());
        settings.app_key = "a/b".into();
        b.anvil_with(settings, |_| {})
    });
    assert!(message.contains("ANVIL_APP_KEY"), "{message}");
}

#[test]
fn the_socket_endpoint_answers_503_while_stopping() {
    let app = TestApp::new(build);
    let key = Anvil::of(app.app()).unwrap().app_key().to_owned();
    let path = format!("/app/{key}");
    assert_eq!(app.get(&path).status(), 426, "a plain GET while running");
    app.app().shutdown();
    let res = app.get(&path);
    assert_eq!(res.status(), 503);
    assert_eq!(res.header("retry-after"), Some("5"));
}
