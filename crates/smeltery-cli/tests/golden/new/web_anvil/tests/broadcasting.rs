//! Broadcasting tests (Anvil): sockets connect to the app in memory (no network), and events reach the sockets on
//! their channels.

use smeltery::anvil::Anvil;
use smeltery::anvil::testing::{AnvilSpy, TestSocket};
use smeltery::testing::TestApp;

use my_app::app::events::announcement_posted::AnnouncementPosted;

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
