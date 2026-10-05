//! `smeltery serve`: build and run the app, rebuild and restart it when Rust code changes.
//!
//! The CLI builds with `cargo build` and runs the binary itself (not `cargo run`), so a restart kills the app
//! process and never leaves it orphaned behind a killed `cargo`. Mold templates under `resources/` are not
//! watched: the app reloads them at runtime.

use std::path::Path;
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use anyhow::Context;
use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::cmd::{binary_path, require_app};

/// Quiet period after the last change before a restart.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the loop checks for Ctrl-C and a crashed app.
const TICK: Duration = Duration::from_millis(250);
/// How long the app may take to shut down after SIGTERM before it is killed (Unix).
const STOP_GRACE: Duration = Duration::from_secs(10);
/// Folders whose `.rs` files trigger a restart.
const WATCHED_DIRS: &[&str] = &["app", "bootstrap", "config", "routes", "database"];

/// Runs `smeltery serve` in `dir` until Ctrl-C (on Unix also SIGTERM and SIGHUP, so a supervisor or a closing
/// terminal stops the app with the wrapper instead of orphaning it). `no_agents`: the app serves HTTP only (`serve --no-agents`).
pub(crate) fn run(dir: &Path, no_agents: bool) -> anyhow::Result<()> {
    require_app(dir)?;
    let bin = binary_path(dir, "debug")?;
    let args = app_args(dir, no_agents)?;

    // Bounded: one pending wake-up is enough, extra ones are dropped.
    let (tx, rx) = sync_channel::<()>(16);
    let stop = Arc::new(AtomicBool::new(false));
    {
        let (stop, tx) = (Arc::clone(&stop), tx.clone());
        ctrlc::set_handler(move || {
            stop.store(true, Ordering::SeqCst);
            wake(&tx);
        })
        .context("cannot install the stop-signal handler")?;
    }

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if res.is_ok_and(|event| is_relevant(&event)) {
            wake(&tx);
        }
    })
    .context("cannot start the file watcher")?;
    for name in WATCHED_DIRS {
        let path = dir.join(name);
        if path.is_dir() {
            watcher
                .watch(&path, RecursiveMode::Recursive)
                .with_context(|| format!("cannot watch {name}/"))?;
        }
    }
    // The root non-recursively, for Cargo.toml (watching the file itself breaks when editors replace it).
    watcher
        .watch(dir, RecursiveMode::NonRecursive)
        .context("cannot watch Cargo.toml")?;

    // A React / Vue app gets the Vite dev server (which builds its CSS too); other web apps the Tailwind watcher,
    // which in a Vite app would write a stray public/assets/css/app.css.
    let vite_app = crate::frontend::is_vite_app(dir);
    let mut assets = if vite_app {
        crate::frontend::start_vite(dir, &crate::frontend::node_from_env())
    } else {
        start_tailwind(dir)
    };
    let mut app = start_app(dir, &bin, &args, &stop);
    let result = watch_loop(dir, &bin, &args, &rx, &stop, &mut app);
    stop_app(app.as_mut(), STOP_GRACE);
    if vite_app {
        crate::frontend::stop_vite(dir, assets.as_mut());
    } else {
        kill(assets.as_mut());
    }
    result
}

fn wake(tx: &SyncSender<()>) {
    match tx.try_send(()) {
        Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
    }
}

fn watch_loop(
    dir: &Path,
    bin: &Path,
    args: &[&str],
    rx: &Receiver<()>,
    stop: &AtomicBool,
    app: &mut Option<Child>,
) -> anyhow::Result<()> {
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match rx.recv_timeout(TICK) {
            Ok(()) => {
                if stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                // Debounce: wait until nothing changed for DEBOUNCE.
                let mut deadline = Instant::now() + DEBOUNCE;
                loop {
                    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                        Ok(()) if !stop.load(Ordering::SeqCst) => {
                            deadline = Instant::now() + DEBOUNCE
                        }
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => break,
                    }
                }
                if stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                println!("smeltery: change detected, rebuilding");
                stop_app(app.as_mut(), STOP_GRACE);
                *app = start_app(dir, bin, args, stop);
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(child) = app.as_mut()
                    && let Ok(Some(status)) = child.try_wait()
                {
                    println!("smeltery: the app exited ({status}); waiting for changes");
                    *app = None;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

/// Builds the app and starts `<bin> <args>`. A failed build is reported and leaves no app running.
fn start_app(dir: &Path, bin: &Path, args: &[&str], stop: &AtomicBool) -> Option<Child> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    match Command::new(cargo).arg("build").current_dir(dir).status() {
        Ok(status) if status.success() => {}
        Ok(_) => {
            if !stop.load(Ordering::SeqCst) {
                eprintln!("smeltery: build failed; waiting for changes");
            }
            return None;
        }
        Err(e) => {
            eprintln!("smeltery: cannot run cargo: {e}");
            return None;
        }
    }
    if stop.load(Ordering::SeqCst) {
        return None;
    }
    match Command::new(bin).args(args).current_dir(dir).spawn() {
        Ok(child) => Some(child),
        Err(e) => {
            eprintln!("smeltery: cannot start {}: {e}", bin.display());
            None
        }
    }
}

/// The app command `smeltery serve` runs: `serve` (web + agents), or `work` (agents only) in a headless app, which
/// has no `routes/` folder.
fn app_command(dir: &Path) -> &'static str {
    if dir.join("routes").is_dir() {
        "serve"
    } else {
        "work"
    }
}

/// The app's arguments: [`app_command`], with `--no-agents` for `serve`. A headless app has nothing but agents.
fn app_args(dir: &Path, no_agents: bool) -> anyhow::Result<Vec<&'static str>> {
    let command = app_command(dir);
    if !no_agents {
        return Ok(vec![command]);
    }
    if command != "serve" {
        anyhow::bail!(
            "--no-agents needs an app with web routes; this app runs only its agents (`work`)"
        );
    }
    Ok(vec![command, "--no-agents"])
}

fn kill(child: Option<&mut Child>) {
    if let Some(child) = child {
        // Already exited is fine; either way reap it.
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Stops the app: on Unix SIGTERM (the app drains its requests and jobs within its shutdown budget), then up to
/// `grace` for it to exit, then `kill`; on Windows `kill` (there is no signal to deliver to another console
/// process). Either way the child is reaped.
fn stop_app(child: Option<&mut Child>, grace: Duration) {
    let Some(child) = child else {
        return;
    };
    if terminate(child) {
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    kill(Some(child));
}

/// Sends SIGTERM to the child. False when it was not delivered (already exited, or Windows), so the caller kills.
#[cfg(unix)]
fn terminate(child: &Child) -> bool {
    let Ok(pid) = i32::try_from(child.id()) else {
        return false;
    };
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    )
    .is_ok()
}

#[cfg(not(unix))]
fn terminate(_child: &Child) -> bool {
    false
}

/// True for create/modify/remove events on `.rs` files or `Cargo.toml`.
fn is_relevant(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    ) && event.paths.iter().any(|p| {
        p.extension().is_some_and(|e| e == "rs") || p.file_name().is_some_and(|n| n == "Cargo.toml")
    })
}

/// The Tailwind input, relative to the app root (`smeltery serve` and `smeltery build`).
pub(crate) const CSS_INPUT: &str = "resources/css/app.css";
/// The Tailwind output the layout links as `/assets/css/app.css`, relative to the app root.
pub(crate) const CSS_OUTPUT: &str = "public/assets/css/app.css";

/// The arguments of the Tailwind watcher. `--watch=always`: a plain `--watch` exits as soon as stdin is closed (D-210).
const WATCH_ARGS: [&str; 5] = ["-i", CSS_INPUT, "-o", CSS_OUTPUT, "--watch=always"];

/// Starts `tailwindcss --watch` when the binary (see [`crate::tailwind::from_env`]) and `resources/css/app.css` exist;
/// prints a hint otherwise.
fn start_tailwind(dir: &Path) -> Option<Child> {
    if !dir.join(CSS_INPUT).is_file() {
        return None;
    }
    let Some(bin) = crate::tailwind::from_env() else {
        println!(
            "smeltery: hint: CSS is not rebuilt; {}",
            crate::tailwind::HOW_TO_INSTALL
        );
        return None;
    };
    let spawned = Command::new(&bin)
        .args(WATCH_ARGS)
        .stdin(std::process::Stdio::null())
        .current_dir(dir)
        .spawn();
    match spawned {
        Ok(child) => Some(child),
        Err(e) => {
            eprintln!("smeltery: cannot start {}: {e}", bin.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use notify::event::{AccessKind, CreateKind, ModifyKind};

    fn event(kind: EventKind, path: &str) -> Event {
        Event::new(kind).add_path(PathBuf::from(path))
    }

    #[test]
    fn rust_files_and_cargo_toml_trigger_restarts() {
        assert!(is_relevant(&event(
            EventKind::Modify(ModifyKind::Any),
            "/a/app/models/post.rs"
        )));
        assert!(is_relevant(&event(
            EventKind::Create(CreateKind::File),
            "/a/routes/web.rs"
        )));
        assert!(is_relevant(&event(
            EventKind::Modify(ModifyKind::Any),
            "/a/Cargo.toml"
        )));
        assert!(!is_relevant(&event(
            EventKind::Modify(ModifyKind::Any),
            "/a/resources/views/home.mold.html"
        )));
        assert!(!is_relevant(&event(
            EventKind::Modify(ModifyKind::Any),
            "/a/Cargo.lock"
        )));
        assert!(!is_relevant(&event(
            EventKind::Access(AccessKind::Any),
            "/a/app/mod.rs"
        )));
    }

    #[test]
    fn headless_apps_run_work() {
        let Some(dir) = tempfile::tempdir().ok() else {
            return;
        };
        assert_eq!(app_command(dir.path()), "work");
        assert!(app_args(dir.path(), true).is_err());
        assert!(std::fs::create_dir(dir.path().join("routes")).is_ok());
        assert_eq!(app_command(dir.path()), "serve");
        assert_eq!(app_args(dir.path(), false).ok(), Some(vec!["serve"]));
        assert_eq!(
            app_args(dir.path(), true).ok(),
            Some(vec!["serve", "--no-agents"])
        );
    }

    /// The app gets SIGTERM first (it drains within its own budget), not SIGKILL.
    #[cfg(unix)]
    #[test]
    fn the_app_is_stopped_with_sigterm() {
        use std::os::unix::process::ExitStatusExt;
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let started = Instant::now();
        stop_app(Some(&mut child), Duration::from_secs(5));
        let status = child.try_wait().unwrap().expect("reaped");
        assert_eq!(status.signal(), Some(15), "{status}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waited the whole grace"
        );
    }

    /// An app that ignores SIGTERM is killed once the grace is over, so a stop never hangs.
    #[cfg(unix)]
    #[test]
    fn an_app_that_ignores_sigterm_is_killed_after_the_grace() {
        use std::os::unix::process::ExitStatusExt;
        let mut child = Command::new("sh")
            .args(["-c", "trap '' TERM; while :; do sleep 1; done"])
            .spawn()
            .unwrap();
        // Let the shell install its trap before the signal arrives.
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        stop_app(Some(&mut child), Duration::from_millis(500));
        let status = child.try_wait().unwrap().expect("reaped");
        assert_eq!(status.signal(), Some(9), "{status}");
        assert!(started.elapsed() >= Duration::from_millis(500));
    }

    /// Tailwind's `--watch` stops at once when stdin is closed (an IDE task, a service, `smeltery serve < /dev/null`):
    /// the watcher must keep running whatever stdin is.
    #[test]
    fn tailwind_keeps_watching_without_a_stdin() {
        assert_eq!(
            WATCH_ARGS,
            [
                "-i",
                "resources/css/app.css",
                "-o",
                "public/assets/css/app.css",
                "--watch=always"
            ]
        );
    }
}
