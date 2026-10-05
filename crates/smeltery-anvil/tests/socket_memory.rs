//! Memory per idle socket: the server process's memory grows by this much for each open socket subscribed to one
//! public channel. Ignored by default; a measurement, not a check (it prints the figure). Run it in release:
//!
//! ```text
//! cargo test --release -p smeltery-anvil --test socket_memory -- --ignored --nocapture
//! ```
//!
//! `ANVIL_MEMORY_SOCKETS` sets the number of sockets (default 2000). The clients run in a child process (this test
//! binary again, test `client_process`), so the figure is the server's alone: resident memory (Linux `VmRSS`,
//! Windows working set and private bytes, otherwise `ps` RSS) before and after the sockets connect.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{BufRead as _, Write as _};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use smeltery_anvil::{Anvil, AnvilExt as _};
use smeltery_core::AppBuilder;
use smeltery_core::config::Settings;
use tokio_tungstenite::tungstenite::Message;

/// The child's line once every socket is subscribed.
const READY: &str = "ANVIL-MEMORY-READY";

/// The resident memory of this process in bytes, and its private / anonymous bytes; `None` when the platform's
/// source cannot be read (no `/proc`, no `powershell`, no `ps`).
fn memory() -> Option<(u64, Option<u64>)> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let kb = |key: &str| {
            status
                .lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|n| n.parse::<u64>().ok())
                .map(|n| n * 1024)
        };
        Some((kb("VmRSS:")?, kb("RssAnon:")))
    }
    #[cfg(windows)]
    {
        let script = format!(
            "$p = Get-Process -Id {}; \"$($p.WorkingSet64) $($p.PrivateMemorySize64)\"",
            std::process::id()
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut numbers = text.split_whitespace().map(|n| n.parse::<u64>().ok());
        Some((numbers.next()??, numbers.next().flatten()))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        let kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        Some((kb * 1024, None))
    }
}

fn settings(sockets: usize) -> Settings {
    let mut s = Settings::from_env();
    s.env = "production".into();
    s.key = "anvil-memory-key-0123456789abcdef0123456".into();
    s.url = "http://127.0.0.1".into();
    s.cache_store = "array".into();
    s.pubsub_driver = "local".into();
    s.server_max_connections = sockets * 2 + 100;
    s.server_max_connections_per_ip = 0;
    s.shutdown_timeout = Duration::from_secs(5);
    s.log_level = "warn".into();
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a measurement: run in release with --ignored --nocapture"]
async fn memory_per_idle_socket() {
    let sockets: usize = std::env::var("ANVIL_MEMORY_SOCKETS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(2000);
    let core = settings(sockets);
    let mut anvil_settings = smeltery_anvil::Settings::from_env(&core);
    anvil_settings.max_connections = sockets + 50;
    anvil_settings.max_connections_per_ip = 0;
    anvil_settings.handshakes_per_minute = u32::MAX;
    let built = AppBuilder::new(core)
        .anvil_with(anvil_settings, |c| {
            c.public("news");
        })
        .build()
        .await
        .unwrap();
    let app = built.app.clone();
    let anvil = Anvil::of(&app).unwrap();
    let key = anvil.app_key().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(smeltery_core::serve_on(built.app, built.router, listener));

    // Warm up (the first socket allocates the hub's tables, the runtime's buffers), then the baseline.
    let mut child = spawn_clients(addr, &key, 20);
    wait_ready(&mut child);
    drop(child.stdin.take());
    let _ = child.wait();
    for _ in 0..100 {
        if anvil.connections() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let before = memory();

    let mut child = spawn_clients(addr, &key, sockets);
    wait_ready(&mut child);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(anvil.connections(), sockets, "every socket is open");
    assert_eq!(anvil.subscribers("news"), sockets);
    let after = memory();

    let per = |b: u64, a: u64| (a.saturating_sub(b)) as f64 / sockets as f64 / 1024.0;
    match (before, after) {
        (Some(before), Some(after)) => {
            println!(
                "anvil memory: {sockets} idle sockets (subscribed to one public channel): resident {:.1} KiB per \
                 socket ({} -> {} bytes)",
                per(before.0, after.0),
                before.0,
                after.0
            );
            if let (Some(b), Some(a)) = (before.1, after.1) {
                println!(
                    "anvil memory: private / anonymous {:.1} KiB per socket ({b} -> {a} bytes)",
                    per(b, a)
                );
            }
        }
        _ => println!(
            "anvil memory: not measured (this platform's memory source could not be read); {sockets} sockets were \
             open"
        ),
    }

    drop(child.stdin.take());
    let _ = child.wait();
    app.shutdown();
    tokio::time::timeout(Duration::from_secs(15), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// This test binary again, as the client process.
fn spawn_clients(addr: std::net::SocketAddr, key: &str, sockets: usize) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["client_process", "--exact", "--ignored", "--nocapture"])
        .env("ANVIL_MEMORY_CLIENT", format!("{addr} {key} {sockets}"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait_ready(child: &mut std::process::Child) {
    let stdout = child.stdout.take().unwrap();
    let mut lines = std::io::BufReader::new(stdout).lines();
    loop {
        let line = lines
            .next()
            .expect("the client process ended before it was ready")
            .unwrap();
        if line.trim() == READY {
            break;
        }
    }
    // Keep draining in the background so the child never blocks on a full pipe.
    std::thread::spawn(move || for _ in lines {});
}

/// The client process: opens the sockets, subscribes each to `news`, prints [`READY`], and holds them until its
/// stdin closes. Does nothing unless started by [`memory_per_idle_socket`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "the client half of memory_per_idle_socket"]
async fn client_process() {
    let Ok(spec) = std::env::var("ANVIL_MEMORY_CLIENT") else {
        return;
    };
    let mut parts = spec.split(' ');
    let addr = parts.next().unwrap().to_owned();
    let key = parts.next().unwrap().to_owned();
    let sockets: usize = parts.next().unwrap().parse().unwrap();
    let mut open = Vec::with_capacity(sockets);
    for _ in 0..sockets {
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://{addr}/app/{key}?protocol=7"))
                .await
                .unwrap();
        // connection_established
        ws.next().await.unwrap().unwrap();
        ws.send(Message::text(
            r#"{"event":"pusher:subscribe","data":{"channel":"news"}}"#,
        ))
        .await
        .unwrap();
        ws.next().await.unwrap().unwrap();
        open.push(ws);
    }
    println!("{READY}");
    std::io::stdout().flush().unwrap();
    // Hold the sockets until the parent closes stdin.
    tokio::task::spawn_blocking(|| {
        let mut sink = String::new();
        while std::io::stdin().read_line(&mut sink).unwrap_or(0) > 0 {}
    })
    .await
    .unwrap();
    drop(open);
}
