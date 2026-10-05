//! `smeltery serve` under a stop signal that is not Ctrl-C: SIGTERM to the wrapper (a supervisor,
//! `kill <pid>`) must take the app down with it instead of leaving it running on the port. Unix
//! only: Windows has no signal to send to another console process. The wrapper builds a stand-in
//! app (one dependency-free binary that writes its pid and sleeps) with the real `cargo build`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Whether a process with this pid exists (`kill -0`).
fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Writes the stand-in app: a Smeltery app's shape (`Cargo.toml`, `bootstrap/main.rs`, `routes/`)
/// around a binary that records its pid in `pid.txt` and sleeps.
fn write_stand_in(dir: &Path) {
    std::fs::create_dir_all(dir.join("bootstrap")).unwrap();
    std::fs::create_dir_all(dir.join("routes")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"standin\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[[bin]]\nname = \"standin\"\npath = \"bootstrap/main.rs\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("bootstrap/main.rs"),
        "fn main() {\n    std::fs::write(\"pid.txt\", std::process::id().to_string()).unwrap();\n    loop {\n        std::thread::sleep(std::time::Duration::from_secs(1));\n    }\n}\n",
    )
    .unwrap();
}

#[test]
fn sigterm_to_the_wrapper_stops_the_app() {
    let dir = tempfile::tempdir().unwrap();
    write_stand_in(dir.path());
    let pid_file = dir.path().join("pid.txt");
    let mut wrapper = Command::new(env!("CARGO_BIN_EXE_smeltery"))
        .args(["serve", "--no-color"])
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // The app is built (a few seconds, cold) and started: it writes its pid.
    let deadline = Instant::now() + Duration::from_secs(120);
    let app_pid = loop {
        if let Ok(pid) = std::fs::read_to_string(&pid_file)
            && !pid.is_empty()
        {
            break pid;
        }
        assert!(
            wrapper.try_wait().unwrap().is_none(),
            "smeltery serve exited before the app started"
        );
        assert!(Instant::now() < deadline, "the stand-in app never started");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(alive(&app_pid), "the app is not running");

    // A plain SIGTERM to the wrapper only (not to the process group, as a terminal's Ctrl-C would).
    let sent = Command::new("kill")
        .args(["-TERM", &wrapper.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    let deadline = Instant::now() + Duration::from_secs(20);
    while wrapper.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "smeltery serve did not exit after SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(&app_pid) {
        if Instant::now() >= deadline {
            let _ = Command::new("kill").args(["-KILL", &app_pid]).status();
            panic!("the app (pid {app_pid}) kept running after smeltery serve got SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
