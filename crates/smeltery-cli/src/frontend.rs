//! The React and Vue starter kits' JavaScript side: the pinned npm versions, Node / npm detection, `npm install`
//! for `smeltery new`, the Vite dev server for `smeltery serve` and `npm run build` for `smeltery build` (D-281,
//! D-283, D-286).
//!
//! Node is a build tool only: the app binary never runs it. Everything here runs a program found through
//! `SMELTERY_NODE` / `SMELTERY_NPM` (tests point them at fake executables), else `node` and `npm` (`npm.cmd` on
//! Windows) on `PATH`.

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};

use crate::cmd::Tool;
use crate::ui::{Badge, Ui};

/// The exact npm versions the kits' `package.json` files pin (no `^`). Bumped deliberately, like Tailwind (D-207):
/// change a line here and the template test points at every `package.json` that must follow.
///
/// TypeScript stays on 6.0.3: `vue-tsc` 3.3.12 fails to start on TypeScript 7 (ALLOY.md §0.4). `@types/node` is the
/// newest 22.x on 2026-10-05, the lowest Node major the kits support.
// The templates hold the versions; this table is what the template test checks them against.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const NPM_VERSIONS: &[(&str, &str)] = &[
    ("@inertiajs/react", "3.8.0"),
    ("@inertiajs/vue3", "3.8.0"),
    ("@inertiajs/vite", "3.8.0"),
    ("react", "19.3.0"),
    ("react-dom", "19.3.0"),
    ("@types/react", "19.3.0"),
    ("@types/react-dom", "19.3.0"),
    ("vue", "3.5.43"),
    ("vite", "8.3.2"),
    ("@vitejs/plugin-react", "6.1.1"),
    ("@vitejs/plugin-vue", "6.0.9"),
    ("tailwindcss", "4.3.3"),
    ("@tailwindcss/vite", "4.3.3"),
    ("typescript", "6.0.3"),
    ("vue-tsc", "3.3.12"),
    ("@types/node", "22.20.5"),
    // The Anvil block (D-431): the Echo client, its React / Vue helpers and the Pusher protocol client.
    ("laravel-echo", "2.5.0"),
    ("@laravel/echo-react", "2.5.0"),
    ("@laravel/echo-vue", "2.5.0"),
    ("pusher-js", "8.6.0"),
];

/// The oldest Node versions the pinned Vite and its plugins accept: `^20.19.0 || >=22.12.0`.
pub(crate) const NODE_REQUIREMENT: &str = "Node 20.19+ or 22.12+";

/// How long `node --version` / `npm --version` may take.
const DETECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The dev-server marker the kits' Vite plugin writes (`storage/framework/vite.hot`, D-281).
pub(crate) const HOT_FILE: &str = "storage/framework/vite.hot";

/// Vite's command-line entry, started with `node` (never through `npm`, whose `npm.cmd` would orphan the node child
/// holding port 5173 on Windows).
pub(crate) const VITE_BIN: &str = "node_modules/vite/bin/vite.js";

/// The starter kit of the app at `dir`: `frontend` in its `Cargo.toml`'s `[package.metadata.smeltery]` table, which
/// `smeltery new` writes for the React and Vue kits. Absent (Mold apps, apps made before the kits, headless apps) is
/// Mold.
///
/// # Errors
/// `Cargo.toml` cannot be read, or names a starter kit other than `mold`, `react` or `vue`.
pub(crate) fn app_frontend(dir: &Path) -> anyhow::Result<crate::new::Frontend> {
    use crate::new::Frontend;
    let path = dir.join("Cargo.toml");
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line.replace(' ', "") == "[package.metadata.smeltery]";
            continue;
        }
        if !inside {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "frontend" {
            continue;
        }
        let value = value.split('#').next().unwrap_or_default().trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'');
        return match value {
            "mold" => Ok(Frontend::Mold),
            "react" => Ok(Frontend::React),
            "vue" => Ok(Frontend::Vue),
            other => bail!(
                "Cargo.toml: unknown starter kit `{other}` in [package.metadata.smeltery] frontend; use \"mold\", \
                 \"react\" or \"vue\""
            ),
        };
    }
    Ok(Frontend::Mold)
}

/// True when `dir` is an app built with Vite (a React or Vue kit): it has `vite.config.ts` or `vite.config.js`.
pub(crate) fn is_vite_app(dir: &Path) -> bool {
    dir.join("vite.config.ts").is_file() || dir.join("vite.config.js").is_file()
}

/// `node`, or `SMELTERY_NODE`.
pub(crate) fn node_from_env() -> Tool {
    Tool::new(tool_from_env("SMELTERY_NODE", "node"))
}

/// `npm` (`npm.cmd` on Windows), or `SMELTERY_NPM`.
pub(crate) fn npm_from_env() -> Tool {
    let default = if cfg!(windows) { "npm.cmd" } else { "npm" };
    Tool::new(tool_from_env("SMELTERY_NPM", default))
}

fn tool_from_env(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

/// What `smeltery new` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NodeCheck {
    /// A Node the kits support, and npm.
    Ready {
        /// `node --version`, e.g. `v22.14.0`.
        node: String,
    },
    /// Node is there but older than [`NODE_REQUIREMENT`].
    TooOld {
        /// `node --version`.
        node: String,
    },
    /// Node or npm is missing (or did not answer within 5 seconds).
    Missing,
}

impl NodeCheck {
    /// The summary value when the npm step is skipped.
    pub(crate) fn skipped(&self) -> String {
        match self {
            NodeCheck::Ready { .. } => "skipped".to_owned(),
            NodeCheck::TooOld { node } => {
                format!("skipped ({NODE_REQUIREMENT} needed, found {node})")
            }
            NodeCheck::Missing => format!("skipped ({NODE_REQUIREMENT} not found)"),
        }
    }
}

/// Runs `node --version` and `npm --version` (each with a 5 second timeout).
pub(crate) fn detect(node: &Tool, npm: &Tool) -> NodeCheck {
    let Some(version) = version_of(node) else {
        return NodeCheck::Missing;
    };
    if !node_supported(&version) {
        return NodeCheck::TooOld { node: version };
    }
    if version_of(npm).is_none() {
        return NodeCheck::Missing;
    }
    NodeCheck::Ready { node: version }
}

/// The first line of `<tool> --version`, `None` when it cannot start, fails or takes longer than the timeout.
fn version_of(tool: &Tool) -> Option<String> {
    version_within(tool, DETECT_TIMEOUT)
}

fn version_within(tool: &Tool, timeout: Duration) -> Option<String> {
    let mut child = tool
        .command()
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    // The child has exited; its few bytes of output are in the pipe.
    let output = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

/// `^20.19.0 || >=22.12.0` for a `node --version` string (`v22.14.0`).
pub(crate) fn node_supported(version: &str) -> bool {
    let mut parts = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|p| p.parse::<u64>().ok());
    let (Some(Some(major)), Some(Some(minor))) = (parts.next(), parts.next()) else {
        return false;
    };
    (major == 20 && minor >= 19) || (major == 22 && minor >= 12) || major > 22
}

/// The last non-empty lines of a failed command's output, for a short reason.
fn tail(stdout: &[u8], stderr: &[u8], lines: usize) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let kept: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    kept.get(kept.len().saturating_sub(lines)..)
        .unwrap_or_default()
        .join("\n")
}

/// How long `npm install` in `smeltery new` may take: `SMELTERY_NPM_TIMEOUT` seconds, else 5 minutes. Offline or
/// behind a broken proxy npm retries for a long time; after this the step is reported as failed (review R3).
pub(crate) fn install_timeout() -> Duration {
    std::env::var("SMELTERY_NPM_TIMEOUT")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .map_or(Duration::from_secs(5 * 60), Duration::from_secs)
}

/// `npm install --no-audit --no-fund` in a new app, behind the spinner, for at most [`install_timeout`]. Never an
/// error: a failure is a warning and `failed` in the summary (the app is complete without `node_modules`; `npm
/// install` is in the next steps).
pub(crate) fn npm_install(dir: &Path, npm: &Tool, ui: Ui) -> &'static str {
    handle_ctrl_c();
    npm_install_within(dir, npm, ui, install_timeout())
}

/// The outcome of a command run with a time limit.
enum Timed {
    Finished(std::process::Output),
    TimedOut,
}

/// Runs `command` (stdin null) and waits at most `timeout`; on timeout the process (with its children on Windows,
/// where `npm.cmd` is a `cmd.exe` running node) is killed. Output goes through temporary files, not pipes: a child
/// that outlives a killed parent would keep a pipe open and block the reader.
fn output_within(mut command: std::process::Command, timeout: Duration) -> std::io::Result<Timed> {
    let (out_file, out_path) = new_temp_file("out")?;
    let (err_file, err_path) = match new_temp_file("err") {
        Ok(created) => created,
        Err(e) => {
            let _ = std::fs::remove_file(&out_path);
            return Err(e);
        }
    };
    // On Unix the child leads its own process group, so a timeout stops npm's children too (D-292).
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let result = (|| {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(out_file)
            .stderr(err_file)
            .spawn()?;
        *RUNNING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((child.id(), out_path.clone(), err_path.clone()));
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() >= timeout {
                kill_tree(&mut child);
                return Ok(Timed::TimedOut);
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Ok(Timed::Finished(std::process::Output {
            status,
            stdout: std::fs::read(&out_path).unwrap_or_default(),
            stderr: std::fs::read(&err_path).unwrap_or_default(),
        }))
    })();
    RUNNING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&err_path);
    result
}

/// A new file in the temp dir for a child's output, opened with `create_new` under an unguessable-enough fresh name
/// (never an existing file or a link someone planted), retried on a name clash.
fn new_temp_file(ext: &str) -> std::io::Result<(std::fs::File, PathBuf)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut last = None;
    for _ in 0..16 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "smeltery-{}-{nanos}-{}.{ext}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free temp file name")))
}

/// Kills `child` and the processes it started: on Windows with `taskkill /T` (`npm.cmd` is a `cmd.exe` running
/// node), on Unix by killing its process group (`output_within` made it a group leader).
fn kill_tree(child: &mut Child) {
    kill_tree_of(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// The npm run in progress (its pid, which leads its process group on Unix, and its output files), for the Ctrl-C
/// handler: on Unix npm is outside the terminal's foreground group, so Ctrl-C reaches only the CLI (review Q-a).
static RUNNING: std::sync::Mutex<Option<(u32, PathBuf, PathBuf)>> = std::sync::Mutex::new(None);

/// Installs (once per process) the Ctrl-C handler of the npm step: stop the running npm with its children, remove
/// the output files and exit with 130, as the shell does for an interrupted command.
fn handle_ctrl_c() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let _ = ctrlc::set_handler(|| {
            stop_running();
            eprintln!();
            std::process::exit(130);
        });
    });
}

/// Kills the npm run registered in [`RUNNING`] (its whole process tree) and removes its output files.
fn stop_running() {
    let running = RUNNING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(entry) = running {
        stop_entry(&entry);
    }
}

/// Kills one registered npm run and removes its output files.
fn stop_entry((pid, out, err): &(u32, PathBuf, PathBuf)) {
    kill_tree_of(*pid);
    let _ = std::fs::remove_file(out);
    let _ = std::fs::remove_file(err);
}

/// Kills process `pid` and the processes it started (Windows: `taskkill /T`; Unix: its process group).
fn kill_tree_of(pid: u32) {
    let pid = pid.to_string();
    let tree = if cfg!(windows) {
        std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        std::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    };
    let _ = tree;
}

fn npm_install_within(dir: &Path, npm: &Tool, ui: Ui, timeout: Duration) -> &'static str {
    const STEP: &str = "npm install";
    let spinner = if ui.styled() {
        Some(ui.spinner("Installing the npm packages (npm install)"))
    } else {
        println!("Installing the npm packages (npm install)...");
        None
    };
    let mut command = npm.command();
    command
        .args(["install", "--no-audit", "--no-fund"])
        .current_dir(dir);
    let output = output_within(command, timeout);
    if let Some(spinner) = spinner {
        spinner.finish();
    }
    let (summary, reason) = match output {
        Ok(Timed::Finished(out)) if out.status.success() => {
            if ui.styled() {
                println!("{}", ui.done_line(STEP));
            } else {
                println!("npm packages installed");
            }
            return "installed";
        }
        Ok(Timed::Finished(out)) => {
            let reason = tail(&out.stdout, &out.stderr, 3);
            let reason = if reason.is_empty() {
                format!("it exited with {}", out.status)
            } else {
                reason
            };
            ("failed", reason)
        }
        Ok(Timed::TimedOut) => (
            "failed (timed out)",
            format!(
                "it did not finish within {} seconds (SMELTERY_NPM_TIMEOUT sets the limit)",
                timeout.as_secs()
            ),
        ),
        Err(err) => ("failed", err.to_string()),
    };
    let message = format!(
        "the npm packages were not installed: {reason}. The app is complete otherwise; run `npm install` in it later"
    );
    if ui.styled() {
        println!("{}", ui.fail_line(STEP, summary));
        eprintln!("{}", ui.badged(Badge::Warn, &message));
    } else {
        eprintln!("warning: {message}");
    }
    summary
}

/// Deletes a leftover dev-server marker: a forced kill skips Vite's exit handler (ALLOY.md §0.5), and a stale file
/// would point debug builds at a dead dev server.
pub(crate) fn remove_hot_file(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(HOT_FILE));
}

/// `smeltery serve` in a Vite app: starts `node node_modules/vite/bin/vite.js` in `dir` (stdin null, output passed
/// through). Without `node_modules` it prints one warning and starts nothing.
pub(crate) fn start_vite(dir: &Path, node: &Tool) -> Option<Child> {
    remove_hot_file(dir);
    if !dir.join(VITE_BIN).is_file() {
        eprintln!(
            "smeltery: warning: the Vite dev server is not running: node_modules/ is missing; run `npm install` \
             (pages show a \"No Vite assets\" error until then)"
        );
        return None;
    }
    let spawned = node
        .command()
        .arg(VITE_BIN)
        .stdin(Stdio::null())
        .current_dir(dir)
        .spawn();
    match spawned {
        Ok(child) => Some(child),
        Err(e) => {
            eprintln!(
                "smeltery: cannot start the Vite dev server ({} {VITE_BIN}): {e}; install {NODE_REQUIREMENT} or set \
                 SMELTERY_NODE",
                node.program().display()
            );
            None
        }
    }
}

/// Stops the Vite dev server started by [`start_vite`] and removes the marker file it leaves behind.
pub(crate) fn stop_vite(dir: &Path, vite: Option<&mut Child>) {
    if let Some(child) = vite {
        let _ = child.kill();
        let _ = child.wait();
    }
    remove_hot_file(dir);
}

/// `smeltery build` in a Vite app, before cargo: removes a stale marker, installs the packages when `node_modules/`
/// is missing (`npm ci` with a `package-lock.json`, else `npm install`), then `npm run build`. Output is captured and
/// shown on failure.
///
/// # Errors
/// npm cannot be started (Node is required to build a React or Vue app), or one of the steps fails.
pub(crate) fn build_assets(dir: &Path, npm: &Tool) -> anyhow::Result<()> {
    remove_hot_file(dir);
    if !dir.join("node_modules").is_dir() {
        let install: &[&str] = if dir.join("package-lock.json").is_file() {
            &["ci", "--no-audit", "--no-fund"]
        } else {
            &["install", "--no-audit", "--no-fund"]
        };
        run_npm(dir, npm, install)?;
    }
    run_npm(dir, npm, &["run", "build"])?;
    println!("Built public/build/");
    Ok(())
}

fn run_npm(dir: &Path, npm: &Tool, args: &[&str]) -> anyhow::Result<()> {
    let shown = format!("npm {}", args.join(" "));
    println!("Running `{shown}`...");
    let output = npm
        .command()
        .args(args)
        .stdin(Stdio::null())
        .current_dir(dir)
        .output()
        .with_context(|| {
            format!(
                "cannot run npm ({}): building a React or Vue app needs {NODE_REQUIREMENT} and npm (or set \
                 SMELTERY_NPM)",
                npm.program().display()
            )
        })?;
    if !output.status.success() {
        let text = tail(&output.stdout, &output.stderr, 40);
        bail!("`{shown}` failed ({}):\n{text}", output.status);
    }
    Ok(())
}

/// The npm version pinned for `package` in [`NPM_VERSIONS`].
#[cfg(test)]
pub(crate) fn pinned(package: &str) -> Option<&'static str> {
    NPM_VERSIONS
        .iter()
        .find(|(name, _)| *name == package)
        .map(|(_, v)| *v)
}

#[cfg(test)]
mod tests;
