//! What a deployed app relies on: the app root for SQLite paths, secrets kept out of the
//! request log, trusted proxies, static file caching, compression and the `/up` health route.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::cell::RefCell;
use std::sync::Once;

use smeltery_core::AppBuilder;
use smeltery_core::config::Settings;
use smeltery_core::http::{ClientInfo, HeaderMap, HeaderValue, Method, Path};
use smeltery_core::testing::{TestApp, TestResponse};

thread_local! {
    /// The log lines of this thread while [`debug_log`] runs.
    static CAPTURE: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// Writes into this thread's [`CAPTURE`], or nowhere.
struct ThreadSink;

impl std::io::Write for ThreadSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        CAPTURE.with(|c| {
            if let Some(out) = c.borrow_mut().as_mut() {
                out.extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `f` with every `debug` (and higher) event of this thread written to the returned
/// text. One global subscriber (a scoped one races with the other tests' threads over
/// `tracing`'s cached callsite interest); `TestApp` runs requests on the calling thread.
fn debug_log(f: impl FnOnce()) -> String {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(|| ThreadSink)
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
    });
    CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
    f();
    let bytes = CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn reset_form(Path(token): Path<String>) -> String {
    format!("form for {}", token.len())
}

async fn plain() -> &'static str {
    "ok"
}

#[test]
fn the_request_log_never_shows_secret_path_segments_or_query_values() {
    const TOKEN: &str = "a3f9c2d1e8b74f6a9c0d1e2f3a4b5c6d";
    let app = TestApp::new(|app| {
        app.api_routes(|r| {
            r.get("/reset-password/{token}", reset_form);
            r.get("/posts", plain);
        })
    });
    let log = debug_log(|| {
        assert_eq!(
            app.get(&format!("/api/reset-password/{TOKEN}")).status(),
            200
        );
        assert_eq!(
            app.get(&format!("/api/posts?signature={TOKEN}&page=2"))
                .status(),
            200
        );
    });
    assert!(log.contains("/api/reset-password/"), "{log}");
    assert!(log.contains("/api/posts"), "{log}");
    assert!(!log.contains(TOKEN), "a secret reached the log:\n{log}");
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_relative_sqlite_path_resolves_against_the_app_root() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("db-root-check")).unwrap();
    let mut builder = AppBuilder::new(Settings::from_env());
    builder.settings_mut().root = root.path().to_path_buf();
    // The process cwd (the crate directory) has no `db-root-check/`, so this only connects
    // when the path is taken relative to the root.
    builder.settings_mut().database_url = "sqlite://db-root-check/app.sqlite?mode=rwc".to_owned();
    let built = builder.build().await.unwrap_or_else(|e| panic!("{e}"));
    built
        .app
        .db()
        .unwrap()
        .execute("CREATE TABLE t (x INTEGER)")
        .await
        .unwrap();
    assert!(root.path().join("db-root-check/app.sqlite").is_file());
    assert!(!std::path::Path::new("db-root-check").exists());
}

const KEY: &str = "0123456789abcdef0123456789abcdef";

fn local(port: u16) -> std::net::SocketAddr {
    std::net::SocketAddr::from(([127, 0, 0, 1], port))
}

fn get_with(app: &TestApp, path: &str, headers: &[(&'static str, &str)]) -> TestResponse {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        map.append(*name, value.parse().unwrap());
    }
    app.request(Method::GET, path, map, axum::body::Body::empty())
}

async fn client_ip(client: ClientInfo) -> String {
    format!(
        "{} {} {}",
        client
            .ip()
            .map_or_else(|| "none".to_owned(), |ip| ip.to_string()),
        client.scheme(),
        client.host().unwrap_or("-")
    )
}

async fn auth_ip(auth: smeltery_core::auth::Auth) -> String {
    auth.ip().to_owned()
}

fn proxied_app(trusted: &str) -> TestApp {
    let trusted = trusted.to_owned();
    TestApp::new(move |mut app| {
        app.settings_mut().trusted_proxies = trusted;
        app.settings_mut().key = KEY.to_owned();
        app.routes(|r| {
            r.get("/auth-ip", auth_ip);
        })
        .api_routes(|r| {
            r.get("/client", client_ip);
        })
    })
}

const FORWARDED: &[(&str, &str)] = &[
    ("x-forwarded-for", "198.51.100.23"),
    ("x-forwarded-proto", "https"),
    ("x-forwarded-host", "shop.example"),
    ("host", "127.0.0.1:8000"),
];

#[test]
fn forwarded_headers_from_an_untrusted_peer_are_ignored() {
    // The default: trust nobody, not even a loopback peer.
    let app = proxied_app("");
    app.from_addr(local(40000));
    assert_eq!(
        get_with(&app, "/api/client", FORWARDED).text(),
        "127.0.0.1 http 127.0.0.1:8000"
    );
    assert_eq!(get_with(&app, "/auth-ip", FORWARDED).text(), "127.0.0.1");

    // A trusted list that does not hold this peer.
    let app = proxied_app("10.0.0.0/8");
    app.from_addr("203.0.113.66:5000".parse().unwrap());
    assert_eq!(
        get_with(&app, "/api/client", FORWARDED).text(),
        "203.0.113.66 http 127.0.0.1:8000"
    );
    assert_eq!(get_with(&app, "/auth-ip", FORWARDED).text(), "203.0.113.66");
}

#[test]
fn a_trusted_proxy_gives_the_real_client_to_handlers_auth_and_the_log() {
    let app = proxied_app("127.0.0.1");
    app.from_addr(local(40000));
    assert_eq!(
        get_with(&app, "/api/client", FORWARDED).text(),
        "198.51.100.23 https shop.example"
    );
    // `Auth::ip`, which keys the login throttle, is the forwarded client too.
    assert_eq!(
        get_with(&app, "/auth-ip", FORWARDED).text(),
        "198.51.100.23"
    );
    let log = debug_log(|| {
        get_with(&app, "/api/client", FORWARDED);
    });
    assert!(log.contains("client=198.51.100.23"), "{log}");
}

#[test]
fn an_invalid_trusted_proxies_entry_stops_the_build() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut builder = AppBuilder::new(Settings::from_env());
    builder.settings_mut().trusted_proxies = "127.0.0.1, proxy.internal".to_owned();
    builder.settings_mut().database_url = String::new();
    let err = rt.block_on(builder.build()).unwrap_err().to_string();
    assert!(
        err.contains("TRUSTED_PROXIES") && err.contains("proxy.internal"),
        "{err}"
    );
}

fn static_app(cache_control: Option<&str>) -> (tempfile::TempDir, TestApp) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("public/assets")).unwrap();
    std::fs::write(
        root.path().join("public/assets/app.css"),
        "body { color: black; margin: 0; padding: 0; }\n".repeat(20),
    )
    .unwrap();
    let path = root.path().to_path_buf();
    let cache_control = cache_control.map(str::to_owned);
    let app = TestApp::new(move |mut app| {
        app.settings_mut().root = path;
        if let Some(value) = cache_control {
            app.settings_mut().static_cache_control = value;
        }
        app
    });
    (root, app)
}

#[test]
fn static_files_revalidate_by_default_and_take_the_configured_cache_control() {
    let (_root, app) = static_app(None);
    let res = app.get("/assets/app.css");
    assert_eq!(res.status(), 200);
    assert_eq!(res.header("cache-control"), Some("no-cache"));
    let etag = res
        .header("etag")
        .expect("ServeDir sends an ETag")
        .to_owned();
    assert!(res.header("last-modified").is_some());
    let again = get_with(&app, "/assets/app.css", &[("if-none-match", &etag)]);
    assert_eq!(again.status(), 304);
    assert_eq!(again.header("cache-control"), Some("no-cache"));
    let missing = app.get("/assets/missing.css");
    assert_eq!(missing.status(), 404);
    assert_ne!(missing.header("cache-control"), Some("no-cache"));

    let (_root, app) = static_app(Some("public, max-age=31536000, immutable"));
    assert_eq!(
        app.get("/assets/app.css").header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
}

async fn big_text() -> String {
    "compress me ".repeat(200)
}

async fn events() -> axum::response::Response {
    use futures_util::StreamExt as _;
    let first = futures_util::stream::once(async {
        Ok::<_, std::convert::Infallible>("event: hello\ndata: first\n\n".to_owned())
    });
    // The stream then stays open, like a live SSE connection.
    let body = axum::body::Body::from_stream(first.chain(futures_util::stream::pending()));
    axum::response::Response::builder()
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
}

#[test]
fn responses_are_compressed_when_the_client_accepts_it() {
    let (_root, app) = static_app(None);
    let gzip = get_with(&app, "/assets/app.css", &[("accept-encoding", "gzip")]);
    assert_eq!(gzip.status(), 200);
    assert_eq!(gzip.header("content-encoding"), Some("gzip"));
    assert!(gzip.bytes().len() < 300, "{} bytes", gzip.bytes().len());
    assert!(
        gzip.header("vary")
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains("accept-encoding")
    );

    let app = TestApp::new(|app| {
        app.api_routes(|r| {
            r.get("/big", big_text);
        })
    });
    let br = get_with(&app, "/api/big", &[("accept-encoding", "br")]);
    assert_eq!(br.header("content-encoding"), Some("br"));
    assert!(br.bytes().len() < 200, "{} bytes", br.bytes().len());
    let plain = app.get("/api/big");
    assert_eq!(plain.header("content-encoding"), None);
    assert_eq!(plain.text(), "compress me ".repeat(200));
}

/// Through a real server on 127.0.0.1: an SSE response asked for with `Accept-Encoding` is
/// neither compressed nor buffered, so its first event arrives while the stream stays open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_sent_events_are_not_compressed_or_buffered() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut builder = AppBuilder::new(Settings::from_env());
    builder.settings_mut().database_url = String::new();
    let built = builder
        .api_routes(|r| {
            r.get("/events", events);
        })
        .build()
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = built.app.clone();
    let server = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            b"GET /api/events HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept-Encoding: gzip, br\r\n\r\n",
        )
        .await
        .unwrap();
    let mut seen = Vec::new();
    let read = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut buf = [0u8; 1024];
        while !String::from_utf8_lossy(&seen).contains("data: first") {
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "the server closed the stream");
            seen.extend_from_slice(&buf[..n]);
        }
    })
    .await;
    let text = String::from_utf8_lossy(&seen).to_ascii_lowercase();
    assert!(
        read.is_ok(),
        "the first event never arrived (buffered?): {text}"
    );
    assert!(text.contains("content-type: text/event-stream"), "{text}");
    assert!(!text.contains("content-encoding"), "{text}");
    app.shutdown();
    drop(stream);
    server.await.unwrap().unwrap();
}

async fn own_up() -> &'static str {
    "mine"
}

#[test]
fn the_health_route_answers_without_a_session() {
    let app = TestApp::new(|mut app| {
        app.settings_mut().key = KEY.to_owned();
        app.routes(|r| {
            r.get("/", plain);
        })
    })
    .with_csrf();
    let res = app.get("/up");
    assert_eq!(res.status(), 200);
    assert_eq!(res.text(), "OK");
    assert_eq!(res.header("cache-control"), Some("no-store"));
    assert!(res.header("set-cookie").is_none(), "no session for /up");
    let up = app
        .app()
        .routes()
        .iter()
        .find(|r| r.path == "/up")
        .expect("listed by route:list");
    assert_eq!(up.methods, ["GET", "HEAD"]);
    assert!(up.middleware.is_empty());
    let head = app.request(
        Method::HEAD,
        "/up",
        HeaderMap::new(),
        axum::body::Body::empty(),
    );
    assert_eq!(head.status(), 200);
}

#[test]
fn an_app_route_at_up_wins_and_the_health_route_can_be_left_out() {
    let app = TestApp::new(|app| {
        app.routes(|r| {
            r.get("/up", own_up);
        })
    });
    assert_eq!(app.get("/up").text(), "mine");
    assert_eq!(
        app.app()
            .routes()
            .iter()
            .filter(|r| r.path == "/up")
            .count(),
        1
    );

    let app = TestApp::new(AppBuilder::without_health_route);
    assert_eq!(app.get("/up").status(), 404);
    assert!(app.app().routes().iter().all(|r| r.path != "/up"));
}

#[cfg(feature = "sqlite")]
async fn pragma(db: &smeltery_core::db::Db, name: &str) -> String {
    use smeltery_core::db::prelude::*;
    let row = db
        .conn()
        .query_one_raw(sea_orm::Statement::from_string(
            db.conn().get_database_backend(),
            format!("PRAGMA {name}"),
        ))
        .await
        .unwrap()
        .unwrap();
    row.try_get_by_index::<String>(0)
        .or_else(|_| row.try_get_by_index::<i64>(0).map(|n| n.to_string()))
        .unwrap()
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_files_use_wal_with_a_busy_timeout_and_memory_databases_still_work() {
    use smeltery_core::db::{Db, DbOptions};
    let dir = tempfile::tempdir().unwrap();
    let db = Db::connect_with("sqlite://wal.sqlite", DbOptions::default().root(dir.path()))
        .await
        .unwrap();
    db.execute("CREATE TABLE t (x INTEGER)").await.unwrap();
    assert_eq!(pragma(&db, "journal_mode").await, "wal");
    assert_eq!(pragma(&db, "synchronous").await, "1", "NORMAL");
    assert_eq!(pragma(&db, "busy_timeout").await, "5000");
    assert!(dir.path().join("wal.sqlite").is_file());
    db.close().await.unwrap();

    let memory = Db::connect_with("sqlite::memory:", DbOptions::default().root(dir.path()))
        .await
        .unwrap();
    memory.execute("CREATE TABLE t (x INTEGER)").await.unwrap();
    memory.execute("INSERT INTO t VALUES (1)").await.unwrap();
    assert_eq!(pragma(&memory, "journal_mode").await, "memory");
}

// ---- Review fixes -----------------------------------------------------------------------

fn get_raw(app: &TestApp, path: &str, headers: Vec<(&'static str, HeaderValue)>) -> TestResponse {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        map.append(name, value);
    }
    app.request(Method::GET, path, map, axum::body::Body::empty())
}

/// A byte the client wrote that is not UTF-8 must not hide the hop the proxy appended (nginx's
/// `$proxy_add_x_forwarded_for` puts both in one header line).
#[test]
fn a_non_ascii_forwarded_for_byte_never_makes_the_proxy_the_client() {
    let app = proxied_app("127.0.0.1");
    app.from_addr(local(40000));
    let one_line = || {
        vec![(
            "x-forwarded-for",
            HeaderValue::from_bytes(b"\xff, 203.0.113.9").unwrap(),
        )]
    };
    let two_lines = || {
        vec![
            ("x-forwarded-for", HeaderValue::from_bytes(b"\xfe").unwrap()),
            ("x-forwarded-for", HeaderValue::from_static("203.0.113.9")),
        ]
    };
    for headers in [one_line(), two_lines()] {
        assert_eq!(
            get_raw(&app, "/auth-ip", headers.clone()).text(),
            "203.0.113.9"
        );
        assert!(
            get_raw(&app, "/api/client", headers)
                .text()
                .starts_with("203.0.113.9 ")
        );
    }
    // Garbage as the only hop: unknown, never the proxy's own address.
    let garbage = vec![("x-forwarded-for", HeaderValue::from_bytes(b"\xff").unwrap())];
    assert_eq!(get_raw(&app, "/auth-ip", garbage).text(), "unknown");
}

async fn back(back: smeltery_core::http::Back) -> String {
    back.url().to_owned()
}

#[test]
fn back_accepts_referers_for_the_forwarded_host_or_app_url() {
    let app = TestApp::new(|mut app| {
        app.settings_mut().trusted_proxies = "127.0.0.1".to_owned();
        app.settings_mut().key = KEY.to_owned();
        app.settings_mut().url = "https://app.example".to_owned();
        app.routes(|r| {
            r.get("/back", back);
        })
    });
    app.from_addr(local(40000));
    // nginx without `proxy_set_header Host`: Host is the upstream address.
    let via_proxy = [
        ("host", "127.0.0.1:8000"),
        ("x-forwarded-host", "shop.example"),
        ("referer", "https://shop.example/posts/1/edit"),
    ];
    assert_eq!(get_with(&app, "/back", &via_proxy).text(), "/posts/1/edit");
    // APP_URL's host is always this site.
    let app_url = [
        ("host", "127.0.0.1:8000"),
        ("referer", "https://app.example/settings"),
    ];
    assert_eq!(get_with(&app, "/back", &app_url).text(), "/settings");
    // Another site stays out.
    let evil = [
        ("host", "127.0.0.1:8000"),
        ("x-forwarded-host", "shop.example"),
        ("referer", "https://evil.example/x"),
    ];
    assert_eq!(get_with(&app, "/back", &evil).text(), "/");

    // From an untrusted peer a forged X-Forwarded-Host does not count.
    app.from_addr("203.0.113.5:1".parse().unwrap());
    assert_eq!(get_with(&app, "/back", &via_proxy).text(), "/");
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn absolute_sqlite_urls_ignore_the_root() {
    use smeltery_core::db::{Db, DbOptions};
    let data = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let abs = data.path().join("abs.sqlite");
    let forward = abs.display().to_string().replace('\\', "/");
    for (url, file) in [
        (format!("sqlite:///{forward}"), "abs.sqlite"),
        (
            format!(
                "sqlite://{}",
                data.path()
                    .join("abs2.sqlite")
                    .display()
                    .to_string()
                    .replace('\\', "/")
            ),
            "abs2.sqlite",
        ),
    ] {
        let db = Db::connect_with(&url, DbOptions::default().root(elsewhere.path()))
            .await
            .unwrap_or_else(|e| panic!("{url}: {e}"));
        db.execute("CREATE TABLE t (x INTEGER)").await.unwrap();
        db.close().await.unwrap();
        assert!(data.path().join(file).is_file(), "{url}");
    }
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_read_only_sqlite_url_keeps_the_journal_mode() {
    use smeltery_core::db::{Db, DbOptions};
    let dir = tempfile::tempdir().unwrap();
    let url = |q: &str| format!("sqlite://ro.sqlite{q}");
    // Create the file with the rollback journal, outside Smeltery's options.
    let plain = Db::connect_with(
        &url("?mode=rwc"),
        DbOptions::default().pool_max(1).root(dir.path()),
    )
    .await
    .unwrap();
    plain.execute("PRAGMA journal_mode=DELETE").await.unwrap();
    plain.execute("CREATE TABLE t (x INTEGER)").await.unwrap();
    plain.close().await.unwrap();
    for query in ["?mode=ro", "?immutable=1", "?mode=ro&cache=private"] {
        let ro = Db::connect_with(&url(query), DbOptions::default().root(dir.path()))
            .await
            .unwrap();
        assert_eq!(pragma(&ro, "journal_mode").await, "delete", "{query}");
        ro.close().await.unwrap();
    }
}

#[derive(smeltery_mold_macros::Mold)]
#[mold("csrf", crate = "smeltery_mold", dir = "tests/app/resources/views")]
struct CsrfPage {}

async fn csrf_page() -> smeltery_core::Response {
    smeltery_core::view::view(CsrfPage {})
}

async fn raw_token(session: smeltery_core::session::Session) -> String {
    session.token()
}

async fn accepted() -> &'static str {
    "accepted"
}

fn between<'a>(text: &'a str, start: &str) -> &'a str {
    let rest = &text[text
        .find(start)
        .unwrap_or_else(|| panic!("{start} in {text}"))
        + start.len()..];
    &rest[..rest.find('"').unwrap()]
}

fn post_token(app: &TestApp, field: Option<&str>, header: Option<&str>) -> u16 {
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    if let Some(token) = header {
        headers.insert("x-csrf-token", HeaderValue::from_str(token).unwrap());
    }
    let body = field.map_or_else(String::new, |t| {
        serde_urlencoded::to_string([("_token", t)]).unwrap()
    });
    app.request(
        Method::POST,
        "/submit",
        headers,
        axum::body::Body::from(body),
    )
    .status()
}

/// BREACH: the CSRF token in a compressed page is masked with a fresh pad per response, so its
/// bytes differ every time while every rendered form still verifies.
#[test]
fn the_csrf_token_is_masked_per_response_and_still_verifies() {
    let app = TestApp::new(|mut app| {
        app.settings_mut().key = KEY.to_owned();
        app.settings_mut().root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/app");
        app.routes(|r| {
            r.get("/csrf", csrf_page);
            r.get("/raw-token", raw_token);
            r.post("/submit", accepted);
        })
    })
    .with_csrf();
    let first = app.get("/csrf").text();
    let second = app.get("/csrf").text();
    let field_1 = between(&first, "name=\"_token\" value=\"").to_owned();
    let meta_1 = between(&first, "name=\"csrf-token\" content=\"").to_owned();
    let field_2 = between(&second, "name=\"_token\" value=\"").to_owned();
    let raw = app.get("/raw-token").text();
    assert_ne!(field_1, field_2, "a fresh mask per response");
    for shown in [&field_1, &meta_1, &field_2] {
        assert!(!shown.contains(&raw), "the raw token is never rendered");
    }

    // `@csrf`, `csrf_token()` (the meta tag sparks.js sends as X-CSRF-TOKEN), a multipart field.
    assert_eq!(post_token(&app, Some(&field_1), None), 200);
    assert_eq!(post_token(&app, Some(&field_2), None), 200);
    assert_eq!(post_token(&app, None, Some(&meta_1)), 200);
    let body = format!(
        "--b0\r\nContent-Disposition: form-data; name=\"_token\"\r\n\r\n{field_2}\r\n--b0--\r\n"
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        HeaderValue::from_static("multipart/form-data; boundary=b0"),
    );
    let res = app.request(
        Method::POST,
        "/submit",
        headers,
        axum::body::Body::from(body),
    );
    assert_eq!(res.status(), 200);
    // The unmasked session token still verifies (API clients that read it from the session).
    assert_eq!(post_token(&app, Some(&raw), None), 200);

    // A tampered token fails, in the pad half and in the masked half.
    let flip = |token: &str, at: usize| {
        let mut bytes = token.as_bytes().to_vec();
        bytes[at] = if bytes[at] == b'A' { b'B' } else { b'A' };
        String::from_utf8(bytes).unwrap()
    };
    assert_eq!(post_token(&app, Some(&flip(&field_1, 3)), None), 419);
    assert_eq!(
        post_token(&app, Some(&flip(&field_1, field_1.len() - 3)), None),
        419
    );
    assert_eq!(
        post_token(&app, Some(&field_1[..field_1.len() - 1]), None),
        419
    );
    assert_eq!(post_token(&app, Some(""), None), 419);
}

#[test]
fn compressed_static_files_get_a_weak_etag_and_compressed_formats_are_left_alone() {
    let (root, app) = static_app(None);
    let gzip = get_with(&app, "/assets/app.css", &[("accept-encoding", "gzip")]);
    assert_eq!(gzip.header("content-encoding"), Some("gzip"));
    let etag = gzip.header("etag").unwrap().to_owned();
    assert!(etag.starts_with("W/\""), "{etag}");
    let again = get_with(
        &app,
        "/assets/app.css",
        &[("accept-encoding", "gzip"), ("if-none-match", &etag)],
    );
    assert_eq!(again.status(), 304);
    // Identity keeps the strong validator.
    assert!(
        !app.get("/assets/app.css")
            .header("etag")
            .unwrap()
            .starts_with("W/")
    );

    for (file, kind) in [
        ("font.woff2", "font/woff2"),
        ("archive.zip", "application/zip"),
        ("doc.pdf", "application/pdf"),
        ("clip.mp4", "video/mp4"),
    ] {
        std::fs::write(root.path().join("public").join(file), vec![b'a'; 4096]).unwrap();
        let res = get_with(
            &app,
            &format!("/{file}"),
            &[("accept-encoding", "gzip, br")],
        );
        assert_eq!(res.status(), 200, "{file}");
        assert_eq!(res.header("content-type"), Some(kind), "{file}");
        assert_eq!(res.header("content-encoding"), None, "{file}");
    }
    // SVG is text: compressed.
    std::fs::write(
        root.path().join("public/logo.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>".repeat(40),
    )
    .unwrap();
    let svg = get_with(&app, "/logo.svg", &[("accept-encoding", "gzip")]);
    assert_eq!(svg.header("content-encoding"), Some("gzip"));
}

#[cfg(feature = "sqlite")]
mod reset_log {
    use super::*;
    use smeltery_core::db::migration::{Migration, Schema};

    mod user {
        use smeltery_core::db::prelude::*;

        #[sea_orm::model]
        #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
        #[sea_orm(table_name = "users")]
        pub struct Model {
            #[sea_orm(primary_key)]
            pub id: i64,
            pub email: String,
            pub password: String,
            pub remember_token: Option<String>,
        }

        impl ActiveModelBehavior for ActiveModel {}

        impl smeltery_core::auth::Authenticatable for Model {
            fn auth_id(&self) -> i64 {
                self.id
            }
            fn password_hash(&self) -> &str {
                &self.password
            }
            fn remember_token(&self) -> Option<&str> {
                self.remember_token.as_deref()
            }
        }
    }

    struct Tables;

    impl Migration for Tables {
        fn name(&self) -> &'static str {
            "2026_10_04_000001_reset_tables"
        }

        async fn up(&self, schema: &Schema) -> smeltery_core::Result<()> {
            schema
                .create("users", |t| {
                    t.id();
                    t.string("email").unique();
                    t.string("password");
                    t.string_len("remember_token", 100).nullable();
                })
                .await?;
            schema
                .create("password_reset_tokens", |t| {
                    t.foreign_id("user_id")
                        .unique()
                        .constrained("users")
                        .cascade_on_delete();
                    t.string("token");
                    t.datetime("created_at").nullable();
                })
                .await
        }

        async fn down(&self, schema: &Schema) -> smeltery_core::Result<()> {
            schema.drop_if_exists("password_reset_tokens").await?;
            schema.drop_if_exists("users").await
        }
    }

    /// Ask for a reset link without mail installed, under `APP_ENV=env` and `APP_URL=url`; the log.
    fn reset_log(env: &str, url: &str) -> String {
        let env = env.to_owned();
        let url = url.to_owned();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        debug_log(|| {
            rt.block_on(async move {
                let mut builder = AppBuilder::new(Settings::from_env())
                    .migrations(|m| {
                        m.add(Tables);
                    })
                    .auth::<user::Model>();
                builder.settings_mut().env = env;
                builder.settings_mut().key = KEY.to_owned();
                builder.settings_mut().database_url = "sqlite::memory:".to_owned();
                builder.settings_mut().url = url;
                let app = builder.build().await.unwrap().app;
                let db = app.db().unwrap();
                app.migrator().fresh(&db).await.unwrap();
                db.execute("INSERT INTO users (email, password) VALUES ('ada@example.com', 'x')")
                    .await
                    .unwrap();
                smeltery_core::auth::passwords::send_reset_link(&app, "ada@example.com")
                    .await
                    .unwrap();
            });
        })
    }

    #[test]
    fn reset_links_reach_the_log_only_in_local_development() {
        for env in ["local", "testing"] {
            let log = reset_log(env, "http://127.0.0.1:8000");
            assert!(
                log.contains("http://127.0.0.1:8000/reset-password/"),
                "{env}: {log}"
            );
            // A public APP_URL means a server, whatever APP_ENV says.
            let log = reset_log(env, "https://app.example");
            assert!(!log.contains("/reset-password/"), "{env}: {log}");
            assert!(log.contains("no mail is set up"), "{env}: {log}");
        }
        for env in ["production", "staging"] {
            let log = reset_log(env, "http://127.0.0.1:8000");
            assert!(!log.contains("/reset-password/"), "{env}: {log}");
            assert!(log.contains("no mail is set up"), "{env}: {log}");
        }
    }
}

#[cfg(feature = "sqlite")]
mod verification_log {
    use super::*;
    use smeltery_core::auth::{Auth, EmailVerificationRequest};
    use smeltery_core::db::migration::{Migration, Schema};

    mod user {
        use smeltery_core::db::prelude::*;

        #[sea_orm::model]
        #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
        #[sea_orm(table_name = "users")]
        pub struct Model {
            #[sea_orm(primary_key)]
            pub id: i64,
            pub email: String,
            pub email_verified_at: Option<DateTimeUtc>,
            pub password: String,
            pub remember_token: Option<String>,
        }

        impl ActiveModelBehavior for ActiveModel {}

        impl smeltery_core::auth::Authenticatable for Model {
            fn auth_id(&self) -> i64 {
                self.id
            }
            fn password_hash(&self) -> &str {
                &self.password
            }
            fn remember_token(&self) -> Option<&str> {
                self.remember_token.as_deref()
            }
        }

        impl smeltery_core::auth::MustVerifyEmail for Model {
            fn email(&self) -> &str {
                &self.email
            }
            fn email_verified_at(&self) -> Option<DateTimeUtc> {
                self.email_verified_at
            }
        }
    }

    struct Users;

    impl Migration for Users {
        fn name(&self) -> &'static str {
            "2026_10_04_000002_verification_users"
        }

        async fn up(&self, schema: &Schema) -> smeltery_core::Result<()> {
            schema
                .create("users", |t| {
                    t.id();
                    t.string("email").unique();
                    t.datetime("email_verified_at").nullable();
                    t.string("password");
                    t.string_len("remember_token", 100).nullable();
                })
                .await
        }

        async fn down(&self, schema: &Schema) -> smeltery_core::Result<()> {
            schema.drop_if_exists("users").await
        }
    }

    async fn verify(request: EmailVerificationRequest) -> smeltery_core::Result<&'static str> {
        request.fulfill().await?;
        Ok("verified")
    }

    async fn send(auth: Auth) -> smeltery_core::Result<&'static str> {
        auth.send_verification_email().await?;
        Ok("sent")
    }

    fn build(app: AppBuilder, env: &str) -> AppBuilder {
        build_at(app, env, "https://app.example")
    }

    fn build_at(app: AppBuilder, env: &str, url: &str) -> AppBuilder {
        let mut app = app
            .migrations(|m| {
                m.add(Users);
            })
            .auth::<user::Model>()
            .verify_email::<user::Model>()
            .routes(|r| {
                r.get("/send", send).middleware("auth");
                r.get("/email/verify/{id}/{hash}", verify)
                    .name("verification.verify")
                    .middleware("auth");
            });
        app.settings_mut().env = env.to_owned();
        app.settings_mut().key = KEY.to_owned();
        app.settings_mut().url = url.to_owned();
        app
    }

    /// Under `APP_ENV=env`, without mail: the log of sending a verification link, and the
    /// link (built in a `testing` app with the same key).
    fn send_log(env: &str, url: &str) -> String {
        let env = env.to_owned();
        let app = TestApp::new(|b| build_at(b, &env, url));
        app.block_on(
            app.db()
                .execute("INSERT INTO users (email, password) VALUES ('ada@example.com', 'x')"),
        )
        .unwrap();
        app.acting_as(1);
        debug_log(|| {
            assert_eq!(app.get("/send").text(), "sent");
        })
    }

    #[test]
    fn verification_links_reach_the_log_only_in_local_development() {
        for env in ["local", "testing"] {
            let log = send_log(env, "http://localhost:8000");
            assert!(
                log.contains("http://localhost:8000/email/verify/1/"),
                "{env}: {log}"
            );
            // A public APP_URL means a server, whatever APP_ENV says.
            let log = send_log(env, "https://app.example");
            assert!(!log.contains("/email/verify/"), "{env}: {log}");
            assert!(log.contains("no mail is set up"), "{env}: {log}");
        }
        for env in ["production", "staging"] {
            let log = send_log(env, "http://localhost:8000");
            assert!(!log.contains("/email/verify/"), "{env}: {log}");
            assert!(!log.contains("signature="), "{env}: {log}");
            assert!(log.contains("no mail is set up"), "{env}: {log}");
        }
    }

    #[test]
    fn the_request_log_redacts_verification_links() {
        let app = TestApp::new(|b| build(b, "production"));
        app.block_on(
            app.db()
                .execute("INSERT INTO users (email, password) VALUES ('ada@example.com', 'x')"),
        )
        .unwrap();
        app.acting_as(1);
        let user = app
            .block_on(<user::Model as smeltery_core::db::Record>::find(
                &app.db(),
                1,
            ))
            .unwrap()
            .unwrap();
        let url = smeltery_core::auth::verification::verification_url(app.app(), &user).unwrap();
        let path = url.strip_prefix("https://app.example").unwrap().to_owned();
        let (base, query) = path.split_once('?').unwrap();
        let hash = base.rsplit('/').next().unwrap().to_owned();
        let signature = query.split("signature=").nth(1).unwrap().to_owned();
        let log = debug_log(|| {
            assert_eq!(app.get(&path).text(), "verified");
        });
        assert!(log.contains("/email/verify/1/[redacted]"), "{log}");
        assert!(!log.contains(&hash), "the hash reached the log:\n{log}");
        assert!(
            !log.contains(&signature),
            "the signature reached the log:\n{log}"
        );
    }
}
