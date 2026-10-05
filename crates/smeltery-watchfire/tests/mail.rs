//! The `mail` feature: queued mail through the `SendMail` job, and alerts mailed to `WATCHFIRE_ALERT_MAIL`.
#![cfg(feature = "mail")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use smeltery_core::AppBuilder;
use smeltery_core::testing::TestApp;
use smeltery_mail::{MailExt as _, Mailer, ResetPassword};
use smeltery_watchfire::mail::{QueueMail as _, SendMail};
use smeltery_watchfire::prelude::*;

fn wait_for(t: &TestApp, mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        t.block_on(async { tokio::time::sleep(Duration::from_millis(20)).await });
    }
    panic!("timed out");
}

fn reset_mail() -> ResetPassword {
    ResetPassword {
        app_name: "Demo".into(),
        email: "ada@example.com".into(),
        url: "https://app.test/reset-password/x".into(),
        minutes: 60,
    }
}

#[test]
fn queued_mail_is_sent_by_a_worker() {
    // The memory queue: these tests have no Watchfire tables.
    smeltery_core::config::set_env_value("QUEUE_DRIVER", "memory");
    let t = TestApp::new(|b: AppBuilder| b.mail().agents(|_w| {})).with_agents();
    let agents = t.app().service::<Agents>().unwrap();
    assert_eq!(
        agents.names(),
        ["queue#0", "queue#1"],
        "SendMail was registered"
    );
    let mailer = Mailer::of(t.app()).unwrap();
    let mailbox = mailer.mailbox().unwrap();
    t.block_on(mailer.queue(reset_mail())).unwrap();
    assert!(mailbox.is_empty(), "not sent inline");
    wait_for(&t, || mailbox.len() == 1);
    let email = &mailbox.emails()[0];
    assert_eq!(email.subject(), "Reset your Demo password");
    assert!(email.has_recipient("ada@example.com"));
    // The job round-trips through JSON.
    let json = serde_json::to_string(&SendMail {
        email: email.clone(),
    })
    .unwrap();
    let back: SendMail = serde_json::from_str(&json).unwrap();
    assert_eq!(&back.email, email);
}

#[test]
fn queueing_needs_watchfire() {
    smeltery_core::config::set_env_value("QUEUE_DRIVER", "memory");
    let t = TestApp::new(|b: AppBuilder| b.mail());
    let err = t
        .block_on(Mailer::of(t.app()).unwrap().queue(reset_mail()))
        .unwrap_err();
    assert!(err.to_string().contains("Watchfire is not set up"), "{err}");
}

#[test]
fn alerts_are_mailed() {
    smeltery_core::config::set_env_value("QUEUE_DRIVER", "memory");
    smeltery_core::config::set_env_value(
        "WATCHFIRE_ALERT_MAIL",
        "ops@example.com, oncall@example.com",
    );
    let t = TestApp::new(|mut b: AppBuilder| {
        b.settings_mut().name = "Shop".into();
        b.mail().agents(|w| {
            w.run("doomed", |_ctx| async move { Err(AgentError::msg("boom")) })
                .restart(Restart::Never);
        })
    })
    .with_agents();
    let mailbox = Mailer::of(t.app()).unwrap().mailbox().unwrap();
    wait_for(&t, || !mailbox.is_empty());
    let email = &mailbox.emails()[0];
    assert_eq!(email.subject(), "[Shop] Watchfire alert: failed doomed");
    assert!(email.has_recipient("ops@example.com") && email.has_recipient("oncall@example.com"));
    let text = email.text_body().unwrap();
    assert!(
        text.contains("Agent: doomed") && text.contains("Kind: failed"),
        "{text}"
    );
    assert!(email.html_body().unwrap().starts_with("<p>"));
}

#[test]
fn an_app_with_agents_and_alert_mail_is_freed() {
    smeltery_core::config::set_env_value("QUEUE_DRIVER", "memory");
    // The same value as `alerts_are_mailed` (the environment is shared by the tests).
    smeltery_core::config::set_env_value(
        "WATCHFIRE_ALERT_MAIL",
        "ops@example.com, oncall@example.com",
    );
    let t = TestApp::new(|b: AppBuilder| {
        b.mail().agents(|w| {
            w.every(
                Duration::from_millis(10),
                "tick",
                |_ctx| async move { Ok(()) },
            );
        })
    })
    .with_agents();
    t.block_on(async { tokio::time::sleep(Duration::from_millis(30)).await });
    let weak = t.app().downgrade();
    assert!(t.app().service::<Agents>().is_some());
    drop(t);
    // Before the fix the `Agents` service (and the alert hook) held the app: it was never freed.
    assert!(weak.upgrade().is_none(), "the app leaked");
}
