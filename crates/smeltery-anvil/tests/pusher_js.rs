//! A real pusher-js client (Node) against an app on 127.0.0.1: connect, a public channel, a private channel
//! authorized through the token endpoint, a refused private channel, a presence channel with its member list and
//! `member_added` / `member_removed`, a whisper between two clients (not echoed), and a revocation closing with 4200.
//!
//! Ignored by default: it needs Node and the npm package `pusher-js`. Run it with
//!
//! ```text
//! npm install --prefix <dir> pusher-js@8.6.0
//! ANVIL_NODE_DIR=<dir> cargo test -p smeltery-anvil --test pusher_js -- --ignored --nocapture
//! ```
//!
//! `ANVIL_NODE_DIR` is the folder holding `node_modules/pusher-js`; `node` must be on `PATH` (or set `NODE`). The
//! client script is written to a temporary folder and run with `NODE_PATH=<dir>/node_modules`.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use serde_json::json;
use smeltery_anvil::{Anvil, AnvilExt as _, Channel, ChannelCtx, Channels, Member};
use smeltery_core::auth::{AuthEvent, Credential, CredentialKind, Guard, Principal, publish_event};
use smeltery_core::config::Settings;
use smeltery_core::http::request::Parts;
use smeltery_core::{App, AppBuilder, BoxFuture, Result};

/// `Authorization: Bearer <user>:<token id>` with the `broadcasting` ability (a test double for a token guard).
struct FakeTokens;

impl Guard for FakeTokens {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn stateless(&self) -> bool {
        true
    }

    fn authenticate<'a>(
        &'a self,
        _app: &'a App,
        parts: &'a mut Parts,
    ) -> BoxFuture<'a, Result<Option<Principal>>> {
        let token = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_owned);
        Box::pin(async move {
            Ok(token.and_then(|t| {
                let (user, id) = t.split_once(':')?;
                Some(Principal::new(
                    user.parse().ok()?,
                    "fake",
                    Credential::token(id.parse::<i64>().ok()?, ["broadcasting"]),
                ))
            }))
        })
    }
}

fn channels(c: &mut Channels) {
    c.public("news");
    // User n owns order n.
    c.private("orders.{order}", |ctx: ChannelCtx| async move {
        Ok(ctx.user_id() == Some(ctx.param::<i64>("order")?))
    });
    c.presence("room.{room}", |ctx: ChannelCtx| async move {
        Ok(ctx
            .user_id()
            .map(|id| Member::new(id).info(json!({ "name": format!("user {id}") }))))
    })
    .whispers();
}

fn build(b: AppBuilder) -> AppBuilder {
    b.guard(FakeTokens).anvil(channels).api_routes(|r| {
        // Test-only triggers the client script calls.
        r.post("/fire/news", |anvil: Anvil| async move {
            anvil
                .to(Channel::public("news"))
                .event("posted")
                .with(&json!({ "title": "hello" }))
                .await
                .unwrap();
            "ok"
        });
        r.post("/fire/order", |anvil: Anvil| async move {
            anvil
                .to(Channel::private("orders.7"))
                .event("shipped")
                .with(&json!({ "id": 7 }))
                .await
                .unwrap();
            "ok"
        });
        r.post("/revoke/7", |app: App| async move {
            publish_event(
                &app,
                &AuthEvent::RevokedAll {
                    user_id: 7,
                    kind: CredentialKind::Tokens,
                    except: None,
                },
            )
            .await
            .unwrap();
            "ok"
        });
    })
}

fn settings() -> Settings {
    let mut s = Settings::from_env();
    s.env = "production".into();
    s.key = "anvil-pusher-js-key-0123456789abcdef0123".into();
    s.url = "http://127.0.0.1".into();
    s.cache_store = "array".into();
    s.session_driver = "cookie".into();
    s.pubsub_driver = "local".into();
    s.shutdown_timeout = Duration::from_secs(3);
    s.log_level = "warn".into();
    s
}

/// The client script (CommonJS, so `NODE_PATH` resolves `pusher-js`). Prints `PASS <step>` per check and exits 0 when
/// every check passed.
const SCRIPT: &str = r#"
const Pusher = require('pusher-js');
const [host, port, key] = [process.env.HOST, Number(process.env.PORT), process.env.KEY];
const base = `http://${host}:${port}`;
const fail = (m) => { console.log(`FAIL ${m}`); process.exit(1); };
setTimeout(() => fail('timeout (30 s)'), 30000).unref();
const pass = (m) => console.log(`PASS ${m}`);
const wait = (emitter, event) => new Promise((resolve) => emitter.bind(event, resolve));
const post = (path) => fetch(`${base}${path}`, { method: 'POST' });

function client(token) {
  return new Pusher(key, {
    cluster: 'local', wsHost: host, wsPort: port, forceTLS: false, enabledTransports: ['ws'],
    channelAuthorization: {
      customHandler: async ({ socketId, channelName }, callback) => {
        const res = await fetch(`${base}/api/broadcasting/auth`, {
          method: 'POST',
          headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/x-www-form-urlencoded' },
          body: new URLSearchParams({ socket_id: socketId, channel_name: channelName }).toString(),
        });
        if (res.status !== 200) return callback(new Error(`auth ${res.status}`), null);
        callback(null, await res.json());
      },
    },
  });
}

(async () => {
  const a = client('7:5');
  const b = client('8:6');
  await Promise.all([wait(a.connection, 'connected'), wait(b.connection, 'connected')]);
  if (!/^\d+\.\d+$/.test(a.connection.socket_id)) fail(`socket id ${a.connection.socket_id}`);
  pass('connect');

  const news = a.subscribe('news');
  await wait(news, 'pusher:subscription_succeeded');
  const posted = wait(news, 'posted');
  await post('/api/fire/news');
  const n = await posted;
  if (n.title !== 'hello') fail(`public data ${JSON.stringify(n)}`);
  pass('public channel');

  const order = a.subscribe('private-orders.7');
  await wait(order, 'pusher:subscription_succeeded');
  const shipped = wait(order, 'shipped');
  await post('/api/fire/order');
  if ((await shipped).id !== 7) fail('private data');
  pass('private channel (token auth)');

  const denied = b.subscribe('private-orders.7');
  const err = await wait(denied, 'pusher:subscription_error');
  if (!String(err.error || err.status || JSON.stringify(err)).includes('403')) fail(`refusal ${JSON.stringify(err)}`);
  pass('private channel refused for another user');

  const roomA = a.subscribe('presence-room.1');
  await wait(roomA, 'pusher:subscription_succeeded');
  if (roomA.members.count !== 1 || roomA.members.me.id !== '7') fail(`members ${roomA.members.count}`);
  const added = wait(roomA, 'pusher:member_added');
  const roomB = b.subscribe('presence-room.1');
  await wait(roomB, 'pusher:subscription_succeeded');
  const member = await added;
  if (member.id !== '8' || member.info.name !== 'user 8') fail(`member_added ${JSON.stringify(member)}`);
  if (roomB.members.count !== 2 || roomB.members.get('7').info.name !== 'user 7') fail('member list');
  pass('presence join with member list');

  let echoed = false;
  roomA.bind('client-typing', () => { echoed = true; });
  const typed = wait(roomB, 'client-typing');
  if (!roomA.trigger('client-typing', { typing: true })) fail('trigger refused');
  const [data, metadata] = await new Promise((resolve) => roomB.bind('client-typing', (d, m) => resolve([d, m])));
  await typed;
  if (data.typing !== true || metadata.user_id !== '7') fail(`whisper ${JSON.stringify([data, metadata])}`);
  await new Promise((r) => setTimeout(r, 300));
  if (echoed) fail('whisper echoed to its sender');
  pass('whisper between two clients (not echoed)');

  const removed = wait(roomA, 'pusher:member_removed');
  b.unsubscribe('presence-room.1');
  if ((await removed).id !== '8') fail('member_removed');
  pass('presence leave');

  const closed = new Promise((resolve) => a.connection.bind('error', (e) => {
    const code = (e && e.data && e.data.code) || (e && e.error && e.error.data && e.error.data.code);
    if (code) resolve(code);
  }));
  await post('/api/revoke/7');
  const code = await closed;
  if (code !== 4200) fail(`close code ${code}`);
  pass('revocation closes with 4200');

  a.disconnect();
  b.disconnect();
  console.log('ALL PASS');
  process.exit(0);
})().catch((e) => fail(e && e.stack || String(e)));
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs Node and pusher-js: ANVIL_NODE_DIR=<dir with node_modules/pusher-js>"]
async fn pusher_js_connects_subscribes_whispers_and_is_revoked() {
    let dir = std::env::var("ANVIL_NODE_DIR")
        .expect("set ANVIL_NODE_DIR to the folder holding node_modules/pusher-js");
    let modules = std::path::Path::new(&dir).join("node_modules");
    assert!(
        modules.join("pusher-js").is_dir(),
        "no pusher-js in {}",
        modules.display()
    );
    let node = std::env::var("NODE").unwrap_or_else(|_| "node".into());

    let built = build(AppBuilder::new(settings())).build().await.unwrap();
    let app = built.app.clone();
    let key = Anvil::of(&app).unwrap().app_key().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));

    let scratch = tempfile::tempdir().unwrap();
    let script = scratch.path().join("smoke.cjs");
    std::fs::write(&script, SCRIPT).unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::process::Command::new(&node)
            .arg(&script)
            .env("NODE_PATH", &modules)
            .env("HOST", "127.0.0.1")
            .env("PORT", addr.port().to_string())
            .env("KEY", &key)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("the client script ends within 60 s")
    .expect("node runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}");
    if !stderr.trim().is_empty() {
        println!("node stderr:\n{stderr}");
    }
    assert!(
        output.status.success() && stdout.contains("ALL PASS"),
        "the pusher-js client failed:\n{stdout}\n{stderr}"
    );

    app.shutdown();
    tokio::time::timeout(Duration::from_secs(15), server)
        .await
        .expect("serve stops")
        .unwrap()
        .unwrap();
}
