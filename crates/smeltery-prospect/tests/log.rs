//! Search text never reaches the log, at TRACE (it may be personal data). Its own test binary: a `tracing`
//! subscriber set for one thread misses events whose call sites other test threads already registered without one.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use smeltery_core::db::PageQuery;
use smeltery_prospect::Searchable as _;
use support::*;

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
fn search_text_never_reaches_the_log() {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(captured.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let app = app();
    post(&app, "zanzibarite", None, 1);
    let prospect = prospect(&app);
    let page = app
        .block_on(
            Post::search(&prospect, "Zanzibarite qwertyuiop")
                .highlight(["title"])
                .paginate(PageQuery::default()),
        )
        .unwrap();
    assert!(page.is_empty());
    app.block_on(Post::search(&prospect, "zanzibar").count())
        .unwrap();
    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(
        log.contains("prospect: search"),
        "the search was logged at all: {log}"
    );
    for secret in ["qwertyuiop", "zanzibar", "Zanzibar"] {
        assert!(!log.contains(secret), "`{secret}` reached the log:\n{log}");
    }
}
