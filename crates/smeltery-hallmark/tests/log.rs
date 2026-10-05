//! Plain tokens never reach the log, at TRACE. Its own test binary: a `tracing` subscriber set for one thread misses
//! events whose call sites other test threads already registered without one.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use smeltery_core::crypto::sha256_hex;
use support::*;

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// Captures every log line, TRACE included.
#[derive(Clone, Default)]
struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn plain_tokens_never_reach_the_log() {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(captured.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let new = create(&app, &ada, &["*"]);
    let unknown = format!("smt_{}", "d".repeat(64));
    assert_eq!(
        get_with(&app, "/api/me", &bearer(new.plain_text())).status(),
        200
    );
    assert_eq!(get_with(&app, "/api/me", &bearer(&unknown)).status(), 401);
    sql(
        &app,
        "UPDATE personal_access_tokens SET abilities = 'x' WHERE id = ?",
        vec![new.token().id.into()],
    );
    assert_eq!(
        get_with(&app, "/api/orders", &bearer(new.plain_text())).status(),
        403
    );
    app.block_on(tokens(&app).revoke(ada.id, new.token().id))
        .unwrap();
    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("abilities are invalid"), "the capture works");
    for secret in [
        &new.plain_text()[4..],
        &unknown[4..],
        &sha256_hex(new.plain_text()),
    ] {
        assert!(!log.contains(secret), "a token reached the log");
    }
}
