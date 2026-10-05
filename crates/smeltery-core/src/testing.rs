//! Test helpers: drive the whole app with HTTP requests, no server or network.
//!
//! ```
//! use smeltery_core::testing::TestApp;
//!
//! async fn home() -> &'static str {
//!     "Hello"
//! }
//!
//! let app = TestApp::new(|app| app.routes(|r| { r.get("/", home); }));
//! let res = app.get("/");
//! assert_eq!(res.status(), 200);
//! assert_eq!(res.text(), "Hello");
//! assert_eq!(app.get("/missing").status(), 404);
//! ```

use axum::body::Body;
use http::{HeaderMap, Method, Request, header};

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use crate::app::{App, AppBuilder, Built};
use crate::config::{Settings, env_value, load_env_file, root_dir};
use crate::db::Db;

/// The app under test, with its own async runtime. Requests are synchronous calls.
pub struct TestApp {
    runtime: tokio::runtime::Runtime,
    app: App,
    router: axum::Router,
    /// Cookies set by responses, sent with every later request (like a browser).
    cookies: Mutex<BTreeMap<String, String>>,
    acting_as: Mutex<Option<i64>>,
    background: Mutex<Option<crate::app::Background>>,
    client: Mutex<Option<std::net::SocketAddr>>,
    /// Headers sent with every request ([`TestApp::with_header`]).
    headers: Mutex<HeaderMap>,
}

impl std::fmt::Debug for TestApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestApp")
            .field("app", &self.app)
            .finish_non_exhaustive()
    }
}

impl TestApp {
    /// Build the app with `build` (like `bootstrap/app.rs`'s `build`), reading `.env` from
    /// the app root and forcing `APP_ENV=testing`.
    ///
    /// The database is `TEST_DATABASE_URL` when it is set; otherwise, with the `sqlite`
    /// feature, a fresh in-memory SQLite database for this `TestApp` alone; otherwise
    /// `DATABASE_URL`. When there is a database, every registered migration runs on it
    /// after dropping every table (`migrate:fresh`), so each `TestApp` starts empty and
    /// migrated.
    ///
    /// The cache store is `TEST_CACHE_STORE` when it is set, otherwise `array` (a cache kept in
    /// this `TestApp`'s memory). The [PubSub](crate::pubsub) driver is `TEST_PUBSUB_DRIVER` when
    /// it is set, otherwise `local`.
    ///
    /// # Panics
    /// When the runtime cannot start, the app fails to build or the migrations fail: a
    /// test cannot go on then.
    #[allow(clippy::panic, clippy::expect_used)]
    pub fn new(build: impl FnOnce(AppBuilder) -> AppBuilder) -> Self {
        let _ = load_env_file(root_dir().join(".env"));
        let mut settings = Settings::from_env();
        settings.env = "testing".to_owned();
        settings.database_url = test_database_url(settings.database_url);
        // A cache of this app's own (`array`), unless TEST_CACHE_STORE
        // names another store.
        settings.cache_store = env_value("TEST_CACHE_STORE")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "array".to_owned());
        // Messages stay in this `TestApp` (`local`), unless TEST_PUBSUB_DRIVER names another driver.
        settings.pubsub_driver = env_value("TEST_PUBSUB_DRIVER")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "local".to_owned());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("start the test runtime");
        let built = runtime.block_on(build(AppBuilder::new(settings)).build());
        let Built { app, router } = match built {
            Ok(built) => built,
            Err(e) => panic!("the app failed to build: {e}"),
        };
        if let Ok(db) = app.db()
            && let Err(e) = runtime.block_on(app.migrator().fresh(&db))
        {
            panic!("the test migrations failed: {e}");
        }
        if let Err(e) = runtime.block_on(crate::pubsub::start(&app, crate::pubsub::Role::Other)) {
            panic!("the PubSub failed to start: {e}");
        }
        Self {
            runtime,
            app,
            router,
            cookies: Mutex::new(BTreeMap::new()),
            acting_as: Mutex::new(None),
            background: Mutex::new(None),
            client: Mutex::new(None),
            headers: Mutex::new(HeaderMap::new()),
        }
    }

    /// Start the app's background work (Watchfire's agents, workers and scheduler), which
    /// `TestApp` otherwise leaves off. It is stopped when the `TestApp` is dropped.
    ///
    /// The test runtime runs on the test's thread, so the background work makes progress
    /// while a request or [`TestApp::block_on`] runs, e.g.
    /// `app.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await })`.
    ///
    /// # Panics
    /// When a start hook fails.
    #[allow(clippy::panic)]
    pub fn with_agents(self) -> Self {
        match self.runtime.block_on(self.app.start_background()) {
            Ok(background) => {
                *self
                    .background
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = background;
            }
            Err(e) => panic!("the background work failed to start: {e}"),
        }
        self
    }

    /// Check CSRF tokens like in production (`APP_ENV=testing` skips the check otherwise).
    pub fn with_csrf(self) -> Self {
        self.app.force_csrf();
        self
    }

    /// Sign in the user with this id for every following request.
    pub fn acting_as(&self, user_id: i64) -> &Self {
        *self
            .acting_as
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(user_id);
        self
    }

    /// Send the following requests as if from `addr` (what the server's connection info gives
    /// handlers; without it requests carry no client address).
    pub fn from_addr(&self, addr: std::net::SocketAddr) -> &Self {
        *self.client.lock().unwrap_or_else(PoisonError::into_inner) = Some(addr);
        self
    }

    /// Send header `name: value` with every following request (replacing an earlier value of `name`). A header
    /// passed to [`TestApp::request`] itself wins over it.
    ///
    /// ```
    /// use smeltery_core::testing::TestApp;
    ///
    /// let app = TestApp::new(|app| app);
    /// app.with_header("accept", "application/json").with_bearer("smt_example");
    /// assert_eq!(app.get("/missing").status(), 404);
    /// app.without_header("authorization");
    /// ```
    ///
    /// # Panics
    /// When `name` or `value` is not a valid header name or value.
    #[allow(clippy::panic)]
    pub fn with_header(&self, name: &str, value: &str) -> &Self {
        let Ok(name) = header::HeaderName::from_bytes(name.as_bytes()) else {
            panic!("invalid test header name {name:?}");
        };
        let Ok(value) = http::HeaderValue::from_str(value) else {
            panic!("invalid value for the test header {name}");
        };
        self.headers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name, value);
        self
    }

    /// Stop sending header `name` set with [`TestApp::with_header`].
    pub fn without_header(&self, name: &str) -> &Self {
        self.headers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        self
    }

    /// Send `Authorization: Bearer <token>` with every following request
    /// (`with_header("authorization", "Bearer …")`).
    ///
    /// # Panics
    /// When `token` holds characters a header value cannot.
    pub fn with_bearer(&self, token: &str) -> &Self {
        self.with_header("authorization", &format!("Bearer {token}"))
    }

    /// Forget every cookie (a new browser) and stop acting as a user.
    pub fn clear_cookies(&self) {
        self.cookies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        *self
            .acting_as
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The cookie `name` the test browser holds (encrypted values stay encrypted).
    pub fn cookie(&self, name: &str) -> Option<String> {
        self.cookies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    /// Set a cookie in the test browser (e.g. to send a tampered one).
    pub fn set_cookie(&self, name: &str, value: &str) {
        self.cookies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned(), value.to_owned());
    }

    /// The app, for reaching services directly.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The database handle.
    ///
    /// # Panics
    /// When the app has no database (no `sqlite` feature and no `TEST_DATABASE_URL` /
    /// `DATABASE_URL`).
    #[allow(clippy::panic)]
    pub fn db(&self) -> Db {
        match self.app.db() {
            Ok(db) => db,
            Err(e) => panic!("{e}"),
        }
    }

    /// Run a future on the test runtime (e.g. to seed data through a service).
    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    /// `GET path`.
    pub fn get(&self, path: &str) -> TestResponse {
        self.request(Method::GET, path, HeaderMap::new(), Body::empty())
    }

    /// `GET path` asking for JSON.
    pub fn get_json(&self, path: &str) -> TestResponse {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            http::HeaderValue::from_static("application/json"),
        );
        self.request(Method::GET, path, headers, Body::empty())
    }

    /// `POST path` with a URL-encoded form body.
    pub fn post_form(&self, path: &str, fields: &[(&str, &str)]) -> TestResponse {
        let body = serde_urlencoded::to_string(fields).unwrap_or_default();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        self.request(Method::POST, path, headers, Body::from(body))
    }

    /// `POST path` with a `multipart/form-data` body: text `fields` and `files`.
    ///
    /// ```
    /// use smeltery_core::testing::{TestApp, TestFile};
    ///
    /// let app = TestApp::new(|app| app);
    /// let png = b"\x89PNG\r\n\x1a\n".to_vec();
    /// let res = app.post_multipart("/photos", &[("title", "Sea")], &[TestFile::new("image", "sea.png", png)]);
    /// assert_eq!(res.status(), 404);
    /// ```
    pub fn post_multipart(
        &self,
        path: &str,
        fields: &[(&str, &str)],
        files: &[TestFile],
    ) -> TestResponse {
        let (content_type, body) = multipart_body(fields, files);
        let mut headers = HeaderMap::new();
        if let Ok(value) = http::HeaderValue::from_str(&content_type) {
            headers.insert(header::CONTENT_TYPE, value);
        }
        self.request(Method::POST, path, headers, Body::from(body))
    }

    /// `POST path` with a JSON body.
    pub fn post_json(&self, path: &str, json: &serde_json::Value) -> TestResponse {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        self.request(Method::POST, path, headers, Body::from(json.to_string()))
    }

    /// Any request.
    #[allow(clippy::panic)]
    pub fn request(
        &self,
        method: Method,
        path: &str,
        headers: HeaderMap,
        body: Body,
    ) -> TestResponse {
        let mut req = match Request::builder().method(method).uri(path).body(body) {
            Ok(req) => req,
            Err(e) => panic!("invalid test request {path}: {e}"),
        };
        req.headers_mut().extend(headers);
        if !req.headers().contains_key(header::COOKIE) {
            let jar = self.cookies.lock().unwrap_or_else(PoisonError::into_inner);
            let line = jar
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            drop(jar);
            if let Ok(value) = http::HeaderValue::from_str(&line)
                && !line.is_empty()
            {
                req.headers_mut().insert(header::COOKIE, value);
            }
        }
        for (name, value) in self
            .headers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            if !req.headers().contains_key(name) {
                req.headers_mut().insert(name.clone(), value.clone());
            }
        }
        if let Some(id) = *self
            .acting_as
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            req.extensions_mut()
                .insert(crate::session::web::ActingAs(id));
        }
        if let Some(addr) = *self.client.lock().unwrap_or_else(PoisonError::into_inner) {
            req.extensions_mut()
                .insert(axum::extract::ConnectInfo(addr));
        }
        let settings = self.app.settings();
        let (limit, timeout) = (settings.body_limit, settings.request_timeout);
        let (parts, body) =
            self.runtime
                .block_on(crate::server::call(&self.router, limit, timeout, req));
        self.remember_cookies(&parts.headers);
        TestResponse {
            status: parts.status.as_u16(),
            headers: parts.headers,
            body: body.to_vec(),
        }
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let background = self
            .background
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(background) = background {
            self.app.shutdown();
            let budget = self.app.settings().shutdown_timeout;
            // The timer must be created inside the runtime.
            let _ = self
                .runtime
                .block_on(async move { tokio::time::timeout(budget, background.wait()).await });
        }
    }
}

impl TestApp {
    fn remember_cookies(&self, headers: &HeaderMap) {
        let mut jar = self.cookies.lock().unwrap_or_else(PoisonError::into_inner);
        for value in headers.get_all(header::SET_COOKIE) {
            let Ok(text) = value.to_str() else { continue };
            let Ok(cookie) = cookie::Cookie::parse(text.to_owned()) else {
                continue;
            };
            let removed = cookie
                .max_age()
                .is_some_and(|age| age <= cookie::time::Duration::ZERO);
            if removed {
                jar.remove(cookie.name());
            } else {
                jar.insert(cookie.name().to_owned(), cookie.value().to_owned());
            }
        }
    }
}

/// `TEST_DATABASE_URL`, else in-memory SQLite (with the `sqlite` feature), else the
/// configured URL.
fn test_database_url(configured: String) -> String {
    match env_value("TEST_DATABASE_URL") {
        Some(url) if !url.is_empty() => url,
        _ if cfg!(feature = "sqlite") => "sqlite::memory:".to_owned(),
        _ => configured,
    }
}

/// A file for [`TestApp::post_multipart`] and [`multipart_body`].
#[derive(Clone, Debug)]
pub struct TestFile {
    field: String,
    name: String,
    mime: String,
    bytes: Vec<u8>,
}

impl TestFile {
    /// The file `name` with `bytes` in form field `field`; the content type follows the
    /// extension (`png`, `jpg`/`jpeg`, `gif`, `webp`, `pdf`, `txt`), else
    /// `application/octet-stream`.
    pub fn new(field: &str, name: &str, bytes: impl Into<Vec<u8>>) -> Self {
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        let mime = match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "pdf" => "application/pdf",
            "txt" => "text/plain",
            _ => "application/octet-stream",
        };
        Self {
            field: field.to_owned(),
            name: name.to_owned(),
            mime: mime.to_owned(),
            bytes: bytes.into(),
        }
    }

    /// Send this content type instead.
    pub fn with_mime(mut self, mime: &str) -> Self {
        mime.clone_into(&mut self.mime);
        self
    }
}

/// A `multipart/form-data` body: its content type (with the boundary) and its bytes.
pub fn multipart_body(fields: &[(&str, &str)], files: &[TestFile]) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "smeltery-test-boundary-7MA4YWxkTrZu0gW";
    let quote = |s: &str| s.replace('"', "%22").replace(['\r', '\n'], " ");
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n",
                quote(name)
            )
            .as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    for file in files {
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n\
                 Content-Type: {}\r\n\r\n",
                quote(&file.field),
                quote(&file.name),
                file.mime
            )
            .as_bytes(),
        );
        body.extend_from_slice(&file.bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

/// A response from [`TestApp`].
#[derive(Clone, Debug)]
pub struct TestResponse {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl TestResponse {
    /// The status code.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// A header value as text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// All headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The body as text (lossy UTF-8).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The raw body.
    pub fn bytes(&self) -> &[u8] {
        &self.body
    }

    /// The body parsed as JSON (`Null` when it is not JSON).
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}
