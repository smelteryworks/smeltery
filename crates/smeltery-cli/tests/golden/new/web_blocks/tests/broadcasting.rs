//! Broadcasting tests (Anvil): sockets connect to the app in memory (no network), private channels admit only
//! the users `routes/channels.rs` allows, and events reach the sockets on their channels.

use smeltery::anvil::Anvil;
use smeltery::anvil::testing::{AnvilSpy, TestSocket, auth_of, authorize};
use smeltery::db::factory::Factory as _;
use smeltery::sparks::testing::TestSpark;
use smeltery::testing::TestApp;

use my_app::app::events::announcement_posted::AnnouncementPosted;
use my_app::database::factories::user_factory::UserFactory;

#[test]
fn an_announcement_reaches_the_public_channel() {
    let app = TestApp::new(my_app::build);
    let spy = AnvilSpy::of(app.app());
    let mut socket = TestSocket::connect(app.app());
    assert_eq!(
        socket.subscribe("announcements", None)["event"],
        "pusher_internal:subscription_succeeded"
    );
    let anvil = Anvil::of(app.app()).expect("Anvil is installed");
    let event = AnnouncementPosted {
        message: "Hello".into(),
    };
    app.block_on(async { anvil.send(&event).await })
        .expect("sending the event");
    assert!(spy.sent_on("announcements", r"App\Events\AnnouncementPosted"));
    let events = socket.events();
    let received = events.last().expect("the event");
    assert_eq!(received["channel"], "announcements");
    assert_eq!(received["data"], r#"{"message":"Hello"}"#);
}

#[test]
fn an_undeclared_public_channel_is_refused() {
    let app = TestApp::new(my_app::build);
    let mut socket = TestSocket::connect(app.app());
    assert_eq!(
        socket.subscribe("secrets", None)["event"],
        "pusher:subscription_error"
    );
}

#[test]
fn a_users_private_channel_admits_only_that_user() {
    let app = TestApp::new(my_app::build);
    let db = app.db();
    let owner = app
        .block_on(UserFactory.create(&db))
        .expect("creating a user");
    let other = app
        .block_on(UserFactory.create(&db))
        .expect("creating a user");
    let channel = format!("private-users.{}", owner.id);
    let mut socket = TestSocket::connect(app.app());

    // A guest is refused.
    assert_eq!(
        authorize(&app, socket.socket_id(), &channel, None).status(),
        403
    );
    // Another user is refused.
    app.acting_as(other.id);
    assert_eq!(
        authorize(&app, socket.socket_id(), &channel, None).status(),
        403
    );
    // The owner gets a signature, and the socket subscribes with it.
    app.acting_as(owner.id);
    let auth =
        auth_of(&authorize(&app, socket.socket_id(), &channel, None)).expect("the owner may join");
    assert_eq!(
        socket.subscribe(&channel, Some(&auth))["event"],
        "pusher_internal:subscription_succeeded"
    );
    // Without a signature, nobody subscribes.
    let mut stranger = TestSocket::connect(app.app());
    assert_eq!(
        stranger.subscribe(&channel, None)["event"],
        "pusher:subscription_error"
    );
}

#[test]
fn the_notifications_spark_pings_the_signed_in_user() {
    let app = TestApp::new(my_app::build);
    let user = app
        .block_on(UserFactory.create(&app.db()))
        .expect("creating a user");
    let spy = AnvilSpy::of(app.app());
    app.acting_as(user.id);
    let html = app.get("/dashboard").text();
    let mut spark =
        TestSpark::from_html(&html, "notifications").expect("the dashboard shows the Spark");
    assert_eq!(spark.data()["user_id"], user.id);
    assert_eq!(
        spark.call("ping", smeltery::json!([])).send(&app).status(),
        200
    );
    assert!(spy.sent_on(
        &format!("private-users.{}", user.id),
        r"App\Events\UserNotified"
    ));
}

#[test]
fn the_notifications_spark_sends_at_most_ten_pings_a_minute() {
    let app = TestApp::new(my_app::build);
    let user = app
        .block_on(UserFactory.create(&app.db()))
        .expect("creating a user");
    let spy = AnvilSpy::of(app.app());
    app.acting_as(user.id);
    let html = app.get("/dashboard").text();
    let mut spark =
        TestSpark::from_html(&html, "notifications").expect("the dashboard shows the Spark");
    for _ in 0..11 {
        assert_eq!(
            spark.call("ping", smeltery::json!([])).send(&app).status(),
            200
        );
    }
    let pings = spy
        .sent()
        .iter()
        .filter(|s| s.name == r"App\Events\UserNotified")
        .count();
    assert_eq!(pings, 10, "the eleventh ping in a minute sends nothing");
}
