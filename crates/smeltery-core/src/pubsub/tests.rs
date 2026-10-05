#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use serde_json::json;

use super::*;
use crate::app::AppBuilder;

const KEY: &str = "pubsub-test-key-0123456789abcdef!";

fn facts(role: Role, has_db: bool, has_key: bool, redis_cache: bool) -> Facts {
    Facts {
        role,
        has_db,
        has_key,
        redis_cache,
    }
}

#[test]
fn the_setting_names_a_driver_or_fails() {
    assert_eq!(parse_setting("auto").unwrap(), Setting::Auto);
    assert_eq!(parse_setting("").unwrap(), Setting::Auto);
    assert_eq!(
        parse_setting(" Database ").unwrap(),
        Setting::Fixed(Driver::Database)
    );
    assert_eq!(
        parse_setting("local").unwrap(),
        Setting::Fixed(Driver::Local)
    );
    let err = parse_setting("kafka").unwrap_err().to_string();
    assert!(
        err.contains("PUBSUB_DRIVER") && err.contains("kafka"),
        "{err}"
    );
    if cfg!(feature = "redis") {
        assert_eq!(
            parse_setting("redis").unwrap(),
            Setting::Fixed(Driver::Redis)
        );
    } else {
        let err = parse_setting("redis").unwrap_err().to_string();
        assert!(err.contains("`redis` feature"), "{err}");
    }
}

#[tokio::test]
async fn an_unknown_driver_fails_the_build() {
    let mut settings = Settings::from_env();
    settings.pubsub_driver = "kafka".into();
    let err = AppBuilder::new(settings).build().await.unwrap_err();
    assert!(err.to_string().contains("PUBSUB_DRIVER"), "{err}");
}

/// A3: under `auto` the driver follows the process role, never the cache store alone.
#[test]
fn auto_chooses_by_process_role() {
    // One `serve` with its background work, console commands, tests: in-process, even with a database and Redis.
    for role in [Role::Serve, Role::Other] {
        let c = choose(Setting::Auto, facts(role, true, true, true)).unwrap();
        assert_eq!(c.driver, Driver::Local, "{role:?}");
        assert!(!c.warn);
    }
    // Web-only `serve`, `work` and a process serving one part (`anvil`): the shared driver, Redis first when the
    // cache is Redis.
    for role in [Role::WebOnly, Role::Work, Role::Part] {
        let c = choose(Setting::Auto, facts(role, true, true, false)).unwrap();
        assert_eq!(c.driver, Driver::Database, "{role:?}");
        assert!(c.reason.starts_with("auto:"), "{}", c.reason);
        let c = choose(Setting::Auto, facts(role, true, true, true)).unwrap();
        assert_eq!(c.driver, Driver::Redis, "{role:?}");
        // Nothing shared is usable: in-process, with a warning.
        let c = choose(Setting::Auto, facts(role, false, true, false)).unwrap();
        assert_eq!((c.driver, c.warn), (Driver::Local, true), "{role:?}");
        let c = choose(Setting::Auto, facts(role, true, false, true)).unwrap();
        assert_eq!((c.driver, c.warn), (Driver::Local, true), "{role:?}");
    }
}

#[test]
fn an_explicit_driver_is_used_in_every_role_or_fails_loudly() {
    for role in [
        Role::Serve,
        Role::WebOnly,
        Role::Work,
        Role::Part,
        Role::Other,
    ] {
        let c = choose(
            Setting::Fixed(Driver::Database),
            facts(role, true, true, false),
        )
        .unwrap();
        assert_eq!(c.driver, Driver::Database);
        let c = choose(Setting::Fixed(Driver::Local), facts(role, true, true, true)).unwrap();
        assert_eq!(c.driver, Driver::Local);
    }
    let err = choose(
        Setting::Fixed(Driver::Database),
        facts(Role::Work, false, true, false),
    )
    .unwrap_err();
    assert!(err.to_string().contains("DATABASE_URL"), "{err}");
    let err = choose(
        Setting::Fixed(Driver::Redis),
        facts(Role::Work, true, false, true),
    )
    .unwrap_err();
    assert!(err.to_string().contains("APP_KEY"), "{err}");
}

#[test]
fn sealed_messages_are_secret_and_tamper_proof() {
    let sealer = Sealer::new(&[7u8; 32]).unwrap();
    let sealed = sealer.seal(br#"{"secret":"order 7"}"#).unwrap();
    assert!(!sealed.contains("order"), "{sealed}");
    assert_ne!(
        sealed,
        sealer.seal(br#"{"secret":"order 7"}"#).unwrap(),
        "fresh nonce"
    );
    assert_eq!(sealer.open(&sealed).unwrap(), br#"{"secret":"order 7"}"#);
    // One flipped character, another key, garbage: nothing.
    let mut chars: Vec<char> = sealed.chars().collect();
    let i = chars.len() / 2;
    chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
    assert!(
        sealer
            .open(&chars.into_iter().collect::<String>())
            .is_none()
    );
    assert!(Sealer::new(&[8u8; 32]).unwrap().open(&sealed).is_none());
    assert!(sealer.open("not base64!").is_none());
    assert!(sealer.open("").is_none());
}

async fn app_with(settings: Settings) -> App {
    AppBuilder::new(settings).build().await.unwrap().app
}

fn settings() -> Settings {
    let mut s = Settings::from_env();
    s.key = KEY.into();
    s.pubsub_driver = "auto".into();
    s.database_url = String::new();
    s.cache_store = "array".into();
    s.pubsub_poll_interval = Duration::from_millis(20);
    s
}

async fn recv(sub: &mut Subscription) -> Arc<Message> {
    tokio::time::timeout(Duration::from_secs(5), sub.recv())
        .await
        .expect("a message within 5 s")
        .expect("open")
}

#[tokio::test]
async fn publish_reaches_this_processs_subscribers_of_the_topic() {
    let app = app_with(settings()).await;
    let pubsub = PubSub::of(&app).unwrap();
    assert_eq!(pubsub.driver(), None, "not started yet");
    let mut prices = pubsub.subscribe("prices");
    let mut other = pubsub.subscribe("other");
    pubsub.publish("other", &json!(1)).await.unwrap();
    pubsub
        .publish("prices", &json!({ "price": 7 }))
        .await
        .unwrap();
    let message = recv(&mut prices).await;
    assert_eq!(
        (
            message.topic.as_str(),
            message.payload["price"].as_i64(),
            message.remote
        ),
        ("prices", Some(7), false)
    );
    assert_eq!(recv(&mut other).await.payload, json!(1));
    assert_eq!(prices.topic(), "prices");
}

/// A burst on one topic never makes another topic's subscribers fall behind (each topic has its own buffer).
#[tokio::test]
async fn a_burst_on_one_topic_does_not_lag_another() {
    let app = app_with(settings()).await;
    let pubsub = PubSub::of(&app).unwrap();
    let mut auth = pubsub.subscribe("auth");
    let mut busy = pubsub.subscribe("busy");
    pubsub
        .publish_reserved("auth", &json!("before"))
        .await
        .unwrap();
    for n in 0..5_000 {
        pubsub.publish("busy", &json!(n)).await.unwrap();
    }
    pubsub
        .publish_reserved("auth", &json!("after"))
        .await
        .unwrap();
    assert_eq!(recv(&mut auth).await.payload, json!("before"));
    assert_eq!(recv(&mut auth).await.payload, json!("after"));
    // The busy topic's own subscriber did fall behind its topic, and receives again afterwards.
    assert!(matches!(busy.recv().await, Err(RecvError::Lagged(n)) if n > 0));
    assert!(busy.recv().await.is_ok());
}

/// The framework's topics keep a buffer of their own even after an app subscribed `MAX_TOPICS` topics of its own.
#[tokio::test]
async fn reserved_topics_always_get_their_own_buffer() {
    let app = app_with(settings()).await;
    let pubsub = PubSub::of(&app).unwrap();
    let _held: Vec<Subscription> = (0..MAX_TOPICS)
        .map(|n| pubsub.subscribe(format!("app{n}")))
        .collect();
    let mut busy = pubsub.subscribe("busy");
    let mut auth = pubsub.subscribe(crate::auth::EVENTS_TOPIC);
    assert_eq!(crate::auth::EVENTS_TOPIC, "auth");
    for topic in RESERVED_TOPICS {
        let _sub = pubsub.subscribe(topic);
        assert!(pubsub.shared.topics().contains_key(topic), "{topic}");
    }
    assert!(
        !pubsub.shared.topics().contains_key("busy"),
        "past the limit"
    );
    for n in 0..5_000 {
        pubsub.publish("busy", &json!(n)).await.unwrap();
    }
    pubsub
        .publish_reserved("auth", &json!("after"))
        .await
        .unwrap();
    assert_eq!(recv(&mut auth).await.payload, json!("after"));
    assert!(matches!(busy.recv().await, Err(RecvError::Lagged(_))));
}

/// Past `MAX_TOPICS` subscribed topics, further topics share one buffer and still get their own messages only;
/// topics nobody subscribes to any more give their place back.
#[tokio::test]
async fn topics_past_the_limit_share_a_buffer() {
    let app = app_with(settings()).await;
    let pubsub = PubSub::of(&app).unwrap();
    let held: Vec<Subscription> = (0..MAX_TOPICS)
        .map(|n| pubsub.subscribe(format!("t{n}")))
        .collect();
    let mut extra = pubsub.subscribe("extra");
    let mut extra2 = pubsub.subscribe("extra2");
    pubsub.publish("extra2", &json!(2)).await.unwrap();
    pubsub.publish("extra", &json!(1)).await.unwrap();
    assert_eq!(recv(&mut extra).await.payload, json!(1));
    assert_eq!(recv(&mut extra2).await.payload, json!(2));
    assert_eq!(pubsub.shared.subscribers(), MAX_TOPICS + 2);
    drop(held);
    let mut own = pubsub.subscribe("own");
    pubsub.publish("own", &json!(3)).await.unwrap();
    assert_eq!(recv(&mut own).await.payload, json!(3));
    assert!(
        pubsub.shared.topics().contains_key("own"),
        "a buffer of its own again"
    );
}

#[tokio::test]
async fn a_message_over_the_limit_is_refused_whole() {
    let app = app_with(settings()).await;
    let pubsub = PubSub::of(&app).unwrap();
    let mut sub = pubsub.subscribe("big");
    let err = pubsub
        .publish("big", &"x".repeat(MAX_MESSAGE_BYTES))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("more than"), "{err}");
    assert!(matches!(
        sub.rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(
        pubsub.forward("big", &"x".repeat(MAX_MESSAGE_BYTES)),
        Forward::Dropped
    );
    assert_eq!(pubsub.dropped(), 1);
}

/// A5: forwarding never blocks; a full queue drops and counts.
#[tokio::test]
async fn forwarding_is_bounded_and_counts_what_it_drops() {
    let app = app_with(settings()).await;
    let pubsub = PubSub::of(&app).unwrap();
    // Before the start the queue holds what is forwarded, up to its size.
    for _ in 0..FORWARD_QUEUE {
        assert_eq!(pubsub.forward("t", &json!(1)), Forward::Queued);
    }
    assert_eq!(pubsub.forward("t", &json!(1)), Forward::Dropped);
    assert_eq!(pubsub.dropped(), 1);
    // Once the driver is `local`, there is nobody to forward to.
    start(&app, Role::Serve).await.unwrap();
    assert_eq!(pubsub.driver(), Some(Driver::Local));
    assert_eq!(pubsub.forward("t", &json!(1)), Forward::NotShared);
    assert_eq!(pubsub.dropped(), 1);
}

#[test]
fn the_driver_is_chosen_once_and_logged() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let app = runtime.block_on(app_with(settings()));
    // The capture is per thread: the current-thread runtime runs the start here.
    let logged = crate::logging::capture(|| {
        runtime.block_on(async {
            start(&app, Role::Serve).await.unwrap();
            start(&app, Role::Work).await.unwrap();
        });
    });
    assert_eq!(PubSub::of(&app).unwrap().driver(), Some(Driver::Local));
    assert_eq!(logged.matches("pubsub driver").count(), 1, "{logged}");
    assert!(logged.contains("local"), "{logged}");
}

/// Sweep W5-09: the framework's topics `auth`, `anvil` and `sparks` are refused through the public `publish` /
/// `forward` (a payload there would close sockets, reach private channels or refresh components in every process);
/// the framework's own path works.
#[tokio::test]
async fn the_frameworks_topics_are_refused_to_app_code() {
    let app = app_with(settings()).await;
    let pubsub = PubSub::of(&app).unwrap();
    let mut auth = pubsub.subscribe("auth");
    let mut anvil = pubsub.subscribe("anvil");
    let mut sparks = pubsub.subscribe("sparks");
    for topic in ["auth", "anvil", "sparks"] {
        let err = pubsub
            .publish(
                topic,
                &json!({ "type": "revoked_all", "user_id": 1, "kind": "every" }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("framework"), "{err}");
        assert_eq!(pubsub.forward(topic, &json!(1)), Forward::Dropped);
    }
    assert!(matches!(
        auth.rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        anvil.rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        sparks.rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    pubsub
        .publish_reserved("auth", &json!("framework"))
        .await
        .unwrap();
    assert_eq!(recv(&mut auth).await.payload, json!("framework"));
    // App topics (and Sparks' public pushes) are unchanged.
    pubsub.publish("orders", &json!(1)).await.unwrap();
}

/// Sweep W5-06: the replay memory is bounded; what it forgets, it refuses (every message sent at or before the
/// forgotten one), never accepts twice.
#[test]
fn the_replay_memory_is_bounded_and_fails_closed() {
    let mut replays = Replays::default();
    let now = 10_000_000;
    assert!(replays.first_time("a", now - 10, now));
    assert!(!replays.first_time("a", now - 10, now), "the same id again");
    for n in 1..MAX_REMEMBERED_IDS {
        assert!(replays.first_time(&format!("m{n}"), now - 5, now));
    }
    assert_eq!(replays.order.len(), MAX_REMEMBERED_IDS);
    // One more: "a" (sent at now - 10) is forgotten, so everything sent at or before then is refused.
    assert!(replays.first_time("new", now, now));
    assert_eq!(replays.order.len(), MAX_REMEMBERED_IDS);
    assert!(
        !replays.first_time("a", now - 10, now),
        "forgotten, still refused"
    );
    assert!(!replays.first_time("older", now - 20, now));
    assert!(replays.first_time("newer", now - 1, now));
    // Ids older than the age limit are forgotten as time passes.
    let later = now + 3 * u64::try_from(MAX_MESSAGE_AGE.as_millis()).unwrap();
    assert!(replays.first_time("fresh", later, later));
    assert_eq!(replays.order.len(), 1);
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    pub(super) struct Shared2 {
        pub(super) a: App,
        pub(super) b: App,
        _dir: tempfile::TempDir,
    }

    /// Two apps (two "processes") on one SQLite file, both started as `role`.
    pub(super) async fn two(role: Role, driver: &str) -> Shared2 {
        two_with(role, driver, Duration::from_millis(20)).await
    }

    /// [`two`] with this poll interval (an hour for tests that poll by hand: the background poller then never runs).
    pub(super) async fn two_with(role: Role, driver: &str, poll: Duration) -> Shared2 {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path()
                .join("db.sqlite")
                .display()
                .to_string()
                .replace('\\', "/")
        );
        let mut s = settings();
        s.database_url = url;
        s.pubsub_driver = driver.into();
        s.pubsub_poll_interval = poll;
        let a = app_with(s.clone()).await;
        migrations::up(&crate::db::migration::Schema::new(&a.db().unwrap()))
            .await
            .unwrap();
        let b = app_with(s).await;
        start(&a, role).await.unwrap();
        start(&b, role).await.unwrap();
        Shared2 { a, b, _dir: dir }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_database_driver_carries_messages_between_processes() {
        let t = two(Role::Work, "auto").await;
        let (a, b) = (PubSub::of(&t.a).unwrap(), PubSub::of(&t.b).unwrap());
        assert_eq!(a.driver(), Some(Driver::Database));
        let mut on_a = a.subscribe("t");
        let mut on_b = b.subscribe("t");
        // Let B's poller take its starting point.
        tokio::time::sleep(Duration::from_millis(100)).await;
        for n in 1..=10 {
            a.publish("t", &json!({ "n": n })).await.unwrap();
        }
        for n in 1..=10 {
            let message = recv(&mut on_b).await;
            assert!(message.remote);
            assert_eq!(message.payload["n"], n, "in order");
        }
        // Forwarded messages go through the queue: they reach B, and never A's own subscribers.
        assert_eq!(a.forward("t", &json!({ "n": 11 })), Forward::Queued);
        assert_eq!(a.forward("t", &json!({ "n": 12 })), Forward::Queued);
        assert_eq!(recv(&mut on_b).await.payload["n"], 11);
        assert_eq!(recv(&mut on_b).await.payload["n"], 12);
        // A delivered its published messages itself, once: its own rows are skipped.
        let mut local = Vec::new();
        while let Ok(Ok(m)) = tokio::time::timeout(Duration::from_millis(300), on_a.recv()).await {
            local.push(m.payload["n"].as_i64().unwrap());
        }
        assert_eq!(local, (1..=10).collect::<Vec<i64>>());
        // The rows are not readable without APP_KEY.
        let rows =
            t.a.db()
                .unwrap()
                .query_with("SELECT payload FROM pubsub_messages", [])
                .await
                .unwrap();
        assert!(!rows.is_empty());
        for row in rows {
            let payload: String = row.try_get("", "payload").unwrap();
            assert!(!payload.contains("\"n\""), "{payload}");
        }
        t.a.shutdown();
        t.b.shutdown();
    }

    /// PostgreSQL and MySQL can commit a lower id after a higher one; the poll still delivers it, once.
    #[tokio::test]
    async fn a_row_committed_late_with_a_lower_id_is_delivered_once() {
        let t = two_with(Role::Other, "database", HOUR).await;
        let b = PubSub::of(&t.b).unwrap();
        let shared = &b.shared;
        let _sub = b.subscribe("t");
        let mut on_b = b.subscribe("t");
        let db = t.a.db().unwrap();
        let mut cursor = database::Cursor::default();
        assert_eq!(
            database::poll_once(&db, shared, &mut cursor).await.unwrap(),
            0
        );
        let now = cursor.since.unwrap();
        let sealer = PubSub::of(&t.a).unwrap();
        let sealed = |n: i64| {
            let text = sealer.envelope("t", json!({ "n": n })).unwrap();
            sealer
                .shared
                .sealer
                .get()
                .unwrap()
                .seal(text.as_bytes())
                .unwrap()
        };
        let insert = |id: i64, created_at: i64, payload: String| {
            let db = db.clone();
            async move {
                db.execute_with(
                    "INSERT INTO pubsub_messages (id, payload, created_at) VALUES (?, ?, ?)",
                    [id.into(), payload.into(), created_at.into()],
                )
                .await
                .unwrap();
            }
        };
        insert(10, now + 500, sealed(10)).await;
        assert_eq!(
            database::poll_once(&db, shared, &mut cursor).await.unwrap(),
            1
        );
        // Id 5, created before id 10, committed after the poll that saw id 10.
        insert(5, now + 400, sealed(5)).await;
        assert_eq!(
            database::poll_once(&db, shared, &mut cursor).await.unwrap(),
            1
        );
        // Nothing twice.
        assert_eq!(
            database::poll_once(&db, shared, &mut cursor).await.unwrap(),
            0
        );
        assert_eq!(recv(&mut on_b).await.payload["n"], 10);
        assert_eq!(recv(&mut on_b).await.payload["n"], 5);
        assert_eq!(cursor.seen.len(), 2);
    }

    #[tokio::test]
    async fn old_rows_are_deleted_in_bounded_batches() {
        let t = two_with(Role::Other, "database", HOUR).await;
        let db = t.a.db().unwrap();
        let transport = database::DatabaseTransport::new(db.clone());
        transport.send("fresh").await.unwrap();
        for i in 0..3 {
            db.execute_with(
                "INSERT INTO pubsub_messages (payload, created_at) VALUES (?, ?)",
                [format!("old {i}").into(), 1_000_i64.into()],
            )
            .await
            .unwrap();
        }
        assert_eq!(database::prune(&db).await.unwrap(), 3);
        let rows = db
            .query_with("SELECT payload FROM pubsub_messages", [])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].try_get::<String>("", "payload").unwrap(), "fresh");
    }

    #[tokio::test]
    async fn messages_sealed_with_another_key_are_skipped_and_counted() {
        let t = two(Role::Other, "database").await;
        let b = PubSub::of(&t.b).unwrap();
        let mut on_b = b.subscribe("t");
        let foreign = Sealer::new(&[9u8; 32]).unwrap();
        let text = b.envelope("t", json!(1)).unwrap();
        let logged = crate::logging::capture(|| {
            b.shared.receive(&foreign.seal(text.as_bytes()).unwrap());
            b.shared.receive("garbage");
        });
        assert!(logged.contains("could not be read"), "{logged}");
        assert_eq!(b.shared.undecodable.load(Ordering::Relaxed), 2);
        assert!(matches!(
            on_b.rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    /// What was forwarded right before shutdown is still sent (within the drain budget).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_messages_are_sent_at_shutdown() {
        let t = two(Role::Work, "database").await;
        let a = PubSub::of(&t.a).unwrap();
        for n in 0..5 {
            assert_eq!(a.forward("t", &json!(n)), Forward::Queued);
        }
        t.a.shutdown();
        t.a.tasks().close();
        tokio::time::timeout(Duration::from_secs(5), t.a.tasks().wait())
            .await
            .expect("the PubSub tasks end at shutdown");
        let rows =
            t.a.db()
                .unwrap()
                .query_with("SELECT id FROM pubsub_messages", [])
                .await
                .unwrap();
        assert_eq!(rows.len(), 5);
        t.b.shutdown();
    }

    /// A3 through the real start path: `serve` (with its work) stays in-process; web-only `serve` and `work` share.
    #[tokio::test]
    async fn start_background_picks_the_driver_of_the_process() {
        async fn driver_of(prepare: impl FnOnce(&App)) -> Driver {
            let dir = tempfile::tempdir().unwrap();
            let mut s = settings();
            s.database_url = format!(
                "sqlite://{}?mode=rwc",
                dir.path()
                    .join("db.sqlite")
                    .display()
                    .to_string()
                    .replace('\\', "/")
            );
            let app = app_with(s).await;
            prepare(&app);
            assert!(app.start_background().await.unwrap().is_none());
            let driver = PubSub::of(&app).unwrap().driver().unwrap();
            app.shutdown();
            driver
        }
        assert_eq!(driver_of(|app| app.mark_serving()).await, Driver::Local);
        assert_eq!(
            driver_of(|app| {
                app.mark_serving();
                app.skip_background();
            })
            .await,
            Driver::Database
        );
        assert_eq!(
            driver_of(|app| {
                app.mark_serving();
                app.set_web_only();
            })
            .await,
            Driver::Database
        );
        assert_eq!(driver_of(|_| {}).await, Driver::Database, "work");
    }

    /// A process serving one part of the app (`smeltery anvil`) shares, and the later start of `serve_on` keeps it.
    #[tokio::test]
    async fn a_process_serving_one_part_shares() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = settings();
        s.database_url = format!(
            "sqlite://{}?mode=rwc",
            dir.path()
                .join("db.sqlite")
                .display()
                .to_string()
                .replace('\\', "/")
        );
        let app = app_with(s).await;
        assert_eq!(start_as_part(&app).await.unwrap(), Driver::Database);
        app.mark_serving();
        assert!(app.start_background().await.unwrap().is_none());
        assert_eq!(PubSub::of(&app).unwrap().driver(), Some(Driver::Database));
        app.shutdown();
        // Without a database or Redis nothing is shared: the caller sees `Local` and decides.
        let app = app_with(settings()).await;
        assert_eq!(start_as_part(&app).await.unwrap(), Driver::Local);
        app.shutdown();
    }

    #[tokio::test]
    async fn an_explicit_driver_that_cannot_work_stops_the_start() {
        let mut s = settings();
        s.pubsub_driver = "database".into();
        let app = app_with(s).await;
        let err = app.start_background().await.unwrap_err();
        assert!(err.to_string().contains("DATABASE_URL"), "{err}");
    }

    /// Insert `count` rows created at `created_at` (the database clock) holding `payload(i)`, in multi-row statements.
    async fn insert_rows(
        db: &crate::db::Db,
        count: usize,
        created_at: i64,
        payload: impl Fn(usize) -> String,
    ) {
        let mut i = 0;
        while i < count {
            let n = (count - i).min(250);
            let marks = vec!["(?, ?)"; n].join(", ");
            let mut values: Vec<sea_orm::Value> = Vec::with_capacity(n * 2);
            for j in i..i + n {
                values.push(payload(j).into());
                values.push(created_at.into());
            }
            db.execute_with(
                &format!("INSERT INTO pubsub_messages (payload, created_at) VALUES {marks}"),
                values,
            )
            .await
            .unwrap();
            i += n;
        }
    }

    /// Review H1: more rows in one overlap window than a poll reads (10 pages of 200) stalled the poller on the same
    /// 2,000 rows; it now goes on from where it stopped, delivers each row once and then the later ones too.
    #[tokio::test]
    async fn a_burst_larger_than_a_poll_does_not_stall_the_poller() {
        let t = two_with(Role::Other, "database", HOUR).await;
        let (a, b) = (PubSub::of(&t.a).unwrap(), PubSub::of(&t.b).unwrap());
        let _sub = b.subscribe("t");
        let db = t.a.db().unwrap();
        let mut cursor = database::Cursor::default();
        assert_eq!(
            database::poll_once(&db, &b.shared, &mut cursor)
                .await
                .unwrap(),
            0
        );
        let now = cursor.since.unwrap();
        let sealer = a.shared.sealer.get().unwrap();
        // Distinct messages (each has its own id): a process delivers one id once.
        insert_rows(&db, 2_500, now + 100, |_| {
            let text = a.envelope("t", json!("burst")).unwrap();
            sealer.seal(text.as_bytes()).unwrap()
        })
        .await;
        let mut delivered = 0;
        for _ in 0..5 {
            delivered += database::poll_once(&db, &b.shared, &mut cursor)
                .await
                .unwrap();
        }
        assert_eq!(delivered, 2_500, "every row once");
        a.publish("t", &json!("after")).await.unwrap();
        let mut later = 0;
        for _ in 0..3 {
            later += database::poll_once(&db, &b.shared, &mut cursor)
                .await
                .unwrap();
        }
        assert_eq!(later, 1, "the row after the burst arrives");
    }

    /// Review L7: one prune deletes more than a fixed number of batches when the rows are there.
    #[tokio::test]
    async fn one_prune_deletes_a_large_backlog() {
        let t = two_with(Role::Other, "database", HOUR).await;
        let db = t.a.db().unwrap();
        insert_rows(&db, 12_000, 1_000, |i| format!("old {i}")).await;
        assert_eq!(database::prune(&db).await.unwrap(), 12_000);
    }

    /// Review L5: a captured message written again later (a replay) is refused once it is older than
    /// `MAX_MESSAGE_AGE`; so is one dated far in the future.
    #[tokio::test]
    async fn old_or_future_messages_are_refused_and_counted() {
        let t = two_with(Role::Other, "database", HOUR).await;
        let b = PubSub::of(&t.b).unwrap();
        let mut on_b = b.subscribe("t");
        let sealer = b.shared.sealer.get().unwrap();
        let max = u64::try_from(MAX_MESSAGE_AGE.as_millis()).unwrap();
        let at = |sent: u64| {
            let text = serde_json::to_string(&Envelope {
                v: ENVELOPE_VERSION,
                t: "t".into(),
                o: "another-process".into(),
                p: json!(sent),
                s: sent,
                i: format!("id-{sent}"),
            })
            .unwrap();
            sealer.seal(text.as_bytes()).unwrap()
        };
        let now = now_ms();
        b.shared.receive(&at(now - max - 60_000));
        b.shared.receive(&at(now + max + 60_000));
        assert_eq!(b.shared.undecodable.load(Ordering::Relaxed), 2);
        b.shared.receive(&at(now - 1_000));
        assert_eq!(recv(&mut on_b).await.payload, json!(now - 1_000));
        assert!(fresh(now, now) && !fresh(0, now));
    }

    /// Review L3: a push made after the forwarder stopped (an agent stopping at shutdown) is counted and logged as
    /// dropped, not reported as "not shared".
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_push_after_shutdown_is_counted_as_dropped() {
        let t = two(Role::Work, "database").await;
        let a = PubSub::of(&t.a).unwrap();
        t.a.shutdown();
        t.a.tasks().close();
        tokio::time::timeout(Duration::from_secs(5), t.a.tasks().wait())
            .await
            .unwrap();
        assert_eq!(a.forward("t", &json!(1)), Forward::Dropped);
        assert_eq!(a.dropped(), 1);
        t.b.shutdown();
    }

    /// Review L3: what stopping work pushes right after the token is cancelled still goes out.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pushes_right_after_the_shutdown_signal_are_still_sent() {
        let t = two(Role::Work, "database").await;
        let a = PubSub::of(&t.a).unwrap();
        t.a.shutdown();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(a.forward("t", &json!("stopped")), Forward::Queued);
        t.a.tasks().close();
        tokio::time::timeout(Duration::from_secs(5), t.a.tasks().wait())
            .await
            .unwrap();
        let rows =
            t.a.db()
                .unwrap()
                .query_with("SELECT id FROM pubsub_messages", [])
                .await
                .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(a.dropped(), 0);
        t.b.shutdown();
    }

    /// Sweep W5-06: a captured message written again inside `MAX_MESSAGE_AGE` (into Redis or the table) is
    /// delivered once; the subscriber learns when it was sent.
    #[tokio::test]
    async fn a_message_received_twice_is_delivered_once() {
        let t = two_with(Role::Other, "database", HOUR).await;
        let (a, b) = (PubSub::of(&t.a).unwrap(), PubSub::of(&t.b).unwrap());
        let mut on_b = b.subscribe("orders");
        let sealer = a.shared.sealer.get().unwrap();
        let before = now_ms();
        let sealed = sealer
            .seal(a.envelope("orders", json!("paid")).unwrap().as_bytes())
            .unwrap();
        b.shared.receive(&sealed);
        let first = recv(&mut on_b).await;
        assert_eq!(first.payload, json!("paid"));
        assert!(first.remote && first.sent_at >= before && first.sent_at <= now_ms());
        let logged = crate::logging::capture(|| b.shared.receive(&sealed));
        assert!(logged.contains("arrived again"), "{logged}");
        assert_eq!(b.shared.replayed.load(Ordering::Relaxed), 1);
        assert!(matches!(
            on_b.rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        // Another message (another id) goes through.
        let other = sealer
            .seal(a.envelope("orders", json!("shipped")).unwrap().as_bytes())
            .unwrap();
        b.shared.receive(&other);
        assert_eq!(recv(&mut on_b).await.payload, json!("shipped"));
    }

    /// Sweep W5-07: a sealed message longer than any envelope (written by someone without the size rule) is skipped
    /// and counted, whether it arrives through Redis (`receive`) or a row (the poll does not read its payload).
    #[tokio::test]
    async fn a_message_longer_than_any_envelope_is_skipped_unread() {
        let t = two_with(Role::Other, "database", HOUR).await;
        let (a, b) = (PubSub::of(&t.a).unwrap(), PubSub::of(&t.b).unwrap());
        let mut on_b = b.subscribe("t");
        let sealer = a.shared.sealer.get().unwrap();
        let big = serde_json::to_string(&Envelope {
            v: ENVELOPE_VERSION,
            t: "t".into(),
            o: "another-process".into(),
            p: json!("x".repeat(MAX_MESSAGE_BYTES)),
            s: now_ms(),
            i: "big".into(),
        })
        .unwrap();
        let sealed = sealer.seal(big.as_bytes()).unwrap();
        assert!(sealed.len() > MAX_SEALED_BYTES);
        b.shared.receive(&sealed);
        assert_eq!(b.shared.undecodable.load(Ordering::Relaxed), 1);
        assert!(matches!(
            on_b.rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        // A row: read back empty, counted, never delivered.
        let db = t.a.db().unwrap();
        let mut cursor = database::Cursor::default();
        database::poll_once(&db, &b.shared, &mut cursor)
            .await
            .unwrap();
        let now = cursor.since.unwrap();
        insert_rows(&db, 1, now, |_| sealed.clone()).await;
        database::poll_once(&db, &b.shared, &mut cursor)
            .await
            .unwrap();
        assert_eq!(b.shared.undecodable.load(Ordering::Relaxed), 2);
        assert!(matches!(
            on_b.rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        // The largest message the sender's rule allows still fits.
        let largest = MAX_MESSAGE_BYTES;
        assert!((12 + largest + 16).div_ceil(3) * 4 <= MAX_SEALED_BYTES);
    }

    /// Sweep W5-08: a row dated in the future (or a database clock that stepped back) never moves the poll position
    /// past the rows inserted afterwards; such rows are pruned.
    #[tokio::test]
    async fn a_row_dated_in_the_future_does_not_stop_delivery() {
        let t = two_with(Role::Other, "database", HOUR).await;
        let (a, b) = (PubSub::of(&t.a).unwrap(), PubSub::of(&t.b).unwrap());
        let mut on_b = b.subscribe("t");
        let db = t.a.db().unwrap();
        let mut cursor = database::Cursor::default();
        database::poll_once(&db, &b.shared, &mut cursor)
            .await
            .unwrap();
        let now = cursor.since.unwrap();
        let sealer = a.shared.sealer.get().unwrap();
        insert_rows(&db, 1, now + 3_600_000, |_| {
            sealer
                .seal(a.envelope("t", json!("future")).unwrap().as_bytes())
                .unwrap()
        })
        .await;
        database::poll_once(&db, &b.shared, &mut cursor)
            .await
            .unwrap();
        assert_eq!(recv(&mut on_b).await.payload, json!("future"));
        a.publish("t", &json!("later")).await.unwrap();
        database::poll_once(&db, &b.shared, &mut cursor)
            .await
            .unwrap();
        assert_eq!(recv(&mut on_b).await.payload, json!("later"));
        // A position ahead of the clock (the database's clock stepped back) is put back to the clock.
        cursor.since = Some(now + 3_600_000);
        a.publish("t", &json!("after the step")).await.unwrap();
        database::poll_once(&db, &b.shared, &mut cursor)
            .await
            .unwrap();
        assert_eq!(recv(&mut on_b).await.payload, json!("after the step"));
        // The future row is pruned.
        assert_eq!(database::prune(&db).await.unwrap(), 1);
    }
}
