//! The asset check runs when the server starts, not in console commands (review R2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use smeltery::alloy::{Alloy, AlloyExt as _};
use smeltery::prelude::*;

#[derive(smeltery::Mold, Default)]
#[mold("app", crate = "smeltery::mold", dir = "tests/app/resources/views")]
struct Root {}

#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Logs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn builder(root: &std::path::Path) -> AppBuilder {
    let mut settings = smeltery::config::Settings::from_env();
    settings.root = root.to_owned();
    settings.env = "local".into();
    settings.database_url = String::new();
    settings.key = "0123456789abcdef0123456789abcdef".into();
    AppBuilder::new(settings)
        .alloy(Alloy::new().root::<Root>())
        .routes(|r| {
            r.get("/", || async { "ok" });
        })
}

#[test]
fn the_asset_check_logs_when_serving_only() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = Logs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        // A console command (`migrate`, `route:list`, `bellows:mcp`) builds the app and never serves.
        builder(tmp.path()).build().await.unwrap();
        assert!(
            !logs.text().contains("Alloy assets"),
            "a build without serving logs no asset line: {}",
            logs.text()
        );

        // `serve`: the check runs before the first request.
        let built = builder(tmp.path()).build().await.unwrap();
        let app = built.app.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));
        for _ in 0..200 {
            if logs.text().contains("Alloy assets") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        app.shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("server stops")
            .unwrap()
            .unwrap();
    });
    let text = logs.text();
    let line = if cfg!(debug_assertions) {
        "Alloy assets: none yet"
    } else {
        "Alloy assets: the Vite manifest is missing"
    };
    assert_eq!(text.matches("Alloy assets").count(), 1, "{text}");
    assert!(text.contains(line), "{text}");
}
