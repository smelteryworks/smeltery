use std::path::PathBuf;

use super::*;

/// A fake executable in `dir`: a `.cmd` on Windows, an `sh` script elsewhere (run as `sh <script>`, so no freshly
/// written file is executed). Tests never run the real Node.js or npm.
fn fake(dir: &Path, name: &str, windows: &str, unix: &str) -> Tool {
    #[cfg(windows)]
    {
        let _ = unix;
        let path = dir.join(format!("{name}.cmd"));
        std::fs::write(&path, format!("@echo off\r\n{windows}")).unwrap();
        Tool::new(path)
    }
    #[cfg(not(windows))]
    {
        let _ = windows;
        let path = dir.join(format!("{name}.sh"));
        std::fs::write(&path, unix).unwrap();
        Tool::script(PathBuf::from("sh"), vec![path.into_os_string()])
    }
}

/// A fake that prints `version` for `--version`.
fn fake_version(dir: &Path, name: &str, version: &str) -> Tool {
    fake(
        dir,
        name,
        &format!("echo {version}\r\n"),
        &format!("echo {version}\n"),
    )
}

/// A fake npm that appends its arguments to `calls.txt` next to it and succeeds; `run build` also writes
/// `public/build/manifest.json` in the working directory.
fn fake_npm(dir: &Path) -> (Tool, PathBuf) {
    let calls = dir.join("calls.txt");
    let tool = fake(
        dir,
        "npm",
        &format!(
            "echo %*>> \"{}\"\r\nif \"%1\"==\"run\" (if not exist public\\build mkdir public\\build\r\necho {{}}> public\\build\\manifest.json)\r\n",
            calls.display()
        ),
        &format!(
            "echo \"$@\" >> '{}'\nif [ \"$1\" = run ]; then mkdir -p public/build && echo '{{}}' > public/build/manifest.json; fi\n",
            calls.display()
        ),
    );
    (tool, calls)
}

/// A fake npm that prints an error and exits with 1.
fn failing_npm(dir: &Path) -> Tool {
    fake(
        dir,
        "npm-fail",
        "echo npm error code ENOTFOUND 1>&2\r\necho npm error network registry.npmjs.org 1>&2\r\nexit /b 1\r\n",
        "echo 'npm error code ENOTFOUND' >&2\necho 'npm error network registry.npmjs.org' >&2\nexit 1\n",
    )
}

fn calls(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_owned())
        .collect()
}

#[test]
fn node_versions_follow_the_engines_of_vite() {
    for ok in [
        "v20.19.0", "v20.20.1", "v22.12.0", "v22.14.0", "v23.0.0", "v24.4.1", "22.12.0",
    ] {
        assert!(node_supported(ok), "{ok}");
    }
    for old in [
        "v18.19.1", "v20.18.3", "v21.7.3", "v22.11.0", "", "nope", "v22",
    ] {
        assert!(!node_supported(old), "{old}");
    }
}

#[test]
fn detection_reads_node_and_npm() {
    let dir = tempfile::tempdir().unwrap();
    let npm = fake_version(dir.path(), "npm", "11.2.0");
    let node = fake_version(dir.path(), "node", "v22.14.0");
    assert_eq!(
        detect(&node, &npm),
        NodeCheck::Ready {
            node: "v22.14.0".to_owned()
        }
    );
    let old = fake_version(dir.path(), "old-node", "v18.19.1");
    let check = detect(&old, &npm);
    assert_eq!(
        check,
        NodeCheck::TooOld {
            node: "v18.19.1".to_owned()
        }
    );
    assert!(
        check.skipped().contains("found v18.19.1"),
        "{}",
        check.skipped()
    );
    let missing = Tool::new(dir.path().join("no-such-node"));
    assert_eq!(detect(&missing, &npm), NodeCheck::Missing);
    assert!(NodeCheck::Missing.skipped().contains("not found"));
    // Node without npm is not enough.
    let no_npm = Tool::new(dir.path().join("no-such-npm"));
    assert_eq!(detect(&node, &no_npm), NodeCheck::Missing);
    // A failing `--version` counts as missing.
    let broken = fake(dir.path(), "broken", "exit /b 2\r\n", "exit 2\n");
    assert_eq!(detect(&broken, &npm), NodeCheck::Missing);
}

#[test]
fn a_hanging_version_check_times_out() {
    let dir = tempfile::tempdir().unwrap();
    // Waits about 3 seconds (ping on the loopback address on Windows, which has no portable sleep for scripts).
    let slow = fake(
        dir.path(),
        "slow",
        "ping -n 4 127.0.0.1 > nul\r\necho v22.14.0\r\n",
        "sleep 3\necho v22.14.0\n",
    );
    let started = Instant::now();
    assert_eq!(version_within(&slow, Duration::from_millis(300)), None);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn npm_install_runs_quietly_and_never_fails_the_app() {
    let bin = tempfile::tempdir().unwrap();
    let app = tempfile::tempdir().unwrap();
    let (npm, log) = fake_npm(bin.path());
    assert_eq!(npm_install(app.path(), &npm, Ui::plain()), "installed");
    assert_eq!(calls(&log), ["install --no-audit --no-fund"]);
    // Offline: a warning and `failed`, not an error.
    assert_eq!(
        npm_install(app.path(), &failing_npm(bin.path()), Ui::plain()),
        "failed"
    );
    // No npm at all.
    let missing = Tool::new(bin.path().join("no-such-npm"));
    assert_eq!(npm_install(app.path(), &missing, Ui::plain()), "failed");
}

/// An npm that hangs (offline, a broken proxy) is stopped after the limit and reported as failed; the app goes on
/// (review R3).
#[test]
fn a_hanging_npm_install_times_out_and_reports_failed() {
    let bin = tempfile::tempdir().unwrap();
    let app = tempfile::tempdir().unwrap();
    let hanging = fake(
        bin.path(),
        "npm-hang",
        "ping -n 6 127.0.0.1 > nul\r\necho done\r\n",
        "sleep 5\necho done\n",
    );
    let started = Instant::now();
    assert_eq!(
        npm_install_within(
            app.path(),
            &hanging,
            Ui::plain(),
            Duration::from_millis(500)
        ),
        "failed (timed out)"
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    // Within the limit the same npm succeeds.
    let (npm, log) = fake_npm(bin.path());
    assert_eq!(
        npm_install_within(app.path(), &npm, Ui::plain(), Duration::from_secs(30)),
        "installed"
    );
    assert_eq!(calls(&log), ["install --no-audit --no-fund"]);
}

/// What the Ctrl-C handler of the npm step does (review Q-a): it kills the running npm with the processes it started
/// (its process group on Unix, its tree on Windows) and removes the output files.
#[test]
fn ctrl_c_cleanup_stops_npm_with_its_children_and_removes_the_files() {
    let bin = tempfile::tempdir().unwrap();
    // A fake npm with a child of its own (a postinstall script, say) that would outlive it.
    let npm = fake(
        bin.path(),
        "npm-busy",
        "start /b ping -n 30 127.0.0.1 > nul\r\nping -n 30 127.0.0.1 > nul\r\n",
        "sleep 30 &\necho $! > child.pid\nsleep 30\n",
    );
    let mut command = npm.command();
    command.current_dir(bin.path()).stdin(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().unwrap();
    let (_o, out) = new_temp_file("out").unwrap();
    let (_e, err) = new_temp_file("err").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let started = Instant::now();
    stop_entry(&(child.id(), out.clone(), err.clone()));
    child.wait().unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!out.exists() && !err.exists());
    #[cfg(unix)]
    {
        // The background child was in the group and is gone too.
        let pid = std::fs::read_to_string(bin.path().join("child.pid")).unwrap();
        let mut gone = false;
        for _ in 0..50 {
            let alive = std::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(gone, "the npm child {pid} survived");
    }
}

/// The output files are new files with fresh names, never an existing file or link (review P3-4).
#[test]
fn output_files_are_created_new() {
    let (_a, a) = new_temp_file("out").unwrap();
    let (_b, b) = new_temp_file("out").unwrap();
    assert_ne!(a, b);
    let existing = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&a);
    assert!(existing.is_err(), "create_new refuses an existing path");
    std::fs::remove_file(a).unwrap();
    std::fs::remove_file(b).unwrap();
}

#[test]
fn the_install_limit_is_five_minutes_unless_set() {
    // Reads the variable as the user sets it; the test only checks the default when it is unset.
    if std::env::var_os("SMELTERY_NPM_TIMEOUT").is_none() {
        assert_eq!(install_timeout(), Duration::from_secs(300));
    }
}

#[test]
fn failure_output_keeps_the_last_lines() {
    assert_eq!(tail(b"out\n", b"one\n\ntwo\nthree\n", 2), "two\nthree");
    assert_eq!(tail(b"only stdout\n", b"  \n", 3), "only stdout");
}

#[test]
fn the_app_frontend_comes_from_cargo_metadata() {
    use crate::new::Frontend;
    let dir = tempfile::tempdir().unwrap();
    let toml = |text: &str| std::fs::write(dir.path().join("Cargo.toml"), text).unwrap();
    toml("[package]\nname = \"a\"\n\n[dependencies]\nserde = \"1\"\n");
    assert_eq!(app_frontend(dir.path()).unwrap(), Frontend::Mold);
    toml(
        "[package]\nname = \"a\"\n\n# The starter kit\n[package.metadata.smeltery]\nfrontend = \"react\"\n\n[dependencies]\n",
    );
    assert_eq!(app_frontend(dir.path()).unwrap(), Frontend::React);
    toml("[package.metadata.smeltery]\nfrontend = 'vue' # comment\n");
    assert_eq!(app_frontend(dir.path()).unwrap(), Frontend::Vue);
    // The key of another table does not count.
    toml("[package.metadata.other]\nfrontend = \"react\"\n");
    assert_eq!(app_frontend(dir.path()).unwrap(), Frontend::Mold);
    toml("[package.metadata.smeltery]\nfrontend = \"svelte\"\n");
    let err = app_frontend(dir.path()).unwrap_err().to_string();
    assert!(err.contains("unknown starter kit `svelte`"), "{err}");
}

#[test]
fn vite_apps_are_the_ones_with_a_vite_config() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!is_vite_app(dir.path()));
    std::fs::write(dir.path().join("vite.config.ts"), "").unwrap();
    assert!(is_vite_app(dir.path()));
    std::fs::remove_file(dir.path().join("vite.config.ts")).unwrap();
    std::fs::write(dir.path().join("vite.config.js"), "").unwrap();
    assert!(is_vite_app(dir.path()));
}

/// An app folder with a stale dev-server marker.
fn app_with_hot_file() -> tempfile::TempDir {
    let app = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(app.path().join("storage/framework")).unwrap();
    std::fs::write(app.path().join(HOT_FILE), "http://127.0.0.1:5173").unwrap();
    std::fs::write(app.path().join("vite.config.ts"), "").unwrap();
    app
}

#[test]
fn serve_starts_vite_with_node_and_removes_the_hot_file_after_it() {
    let bin = tempfile::tempdir().unwrap();
    let app = app_with_hot_file();
    let log = bin.path().join("node-calls.txt");
    // The fake Vite records how it was started and writes the marker, as the kits' plugin does when listening.
    let node = fake(
        bin.path(),
        "node",
        &format!(
            "echo %*>> \"{}\"\r\necho http://127.0.0.1:5173> storage\\framework\\vite.hot\r\n",
            log.display()
        ),
        &format!(
            "echo \"$@\" >> '{}'\necho http://127.0.0.1:5173 > storage/framework/vite.hot\n",
            log.display()
        ),
    );
    std::fs::create_dir_all(app.path().join("node_modules/vite/bin")).unwrap();
    std::fs::write(app.path().join(VITE_BIN), "").unwrap();
    let mut child = start_vite(app.path(), &node).expect("Vite starts");
    let status = child.wait().unwrap();
    assert!(status.success());
    assert_eq!(calls(&log), ["node_modules/vite/bin/vite.js"]);
    assert!(
        app.path().join(HOT_FILE).is_file(),
        "the fake wrote the marker"
    );
    // A killed Vite cannot remove its marker; `serve` does.
    stop_vite(app.path(), Some(&mut child));
    assert!(!app.path().join(HOT_FILE).exists());
}

#[test]
fn serve_without_node_modules_starts_nothing_and_clears_a_stale_hot_file() {
    let bin = tempfile::tempdir().unwrap();
    let app = app_with_hot_file();
    let log = bin.path().join("node-calls.txt");
    let node = fake(
        bin.path(),
        "node",
        &format!("echo %*>> \"{}\"\r\n", log.display()),
        &format!("echo \"$@\" >> '{}'\n", log.display()),
    );
    assert!(start_vite(app.path(), &node).is_none());
    assert!(calls(&log).is_empty(), "node never ran");
    assert!(!app.path().join(HOT_FILE).exists());
    // Without Node.js: no dev server, and `serve` goes on.
    std::fs::create_dir_all(app.path().join("node_modules/vite/bin")).unwrap();
    std::fs::write(app.path().join(VITE_BIN), "").unwrap();
    assert!(start_vite(app.path(), &Tool::new(bin.path().join("no-such-node"))).is_none());
}

#[test]
fn build_installs_when_needed_then_runs_the_vite_build() {
    let bin = tempfile::tempdir().unwrap();
    let (npm, log) = fake_npm(bin.path());
    // No node_modules and no lock file: `npm install`, then the build; the stale marker goes.
    let app = app_with_hot_file();
    build_assets(app.path(), &npm).unwrap();
    assert_eq!(calls(&log), ["install --no-audit --no-fund", "run build"]);
    assert!(!app.path().join(HOT_FILE).exists());
    assert!(app.path().join("public/build/manifest.json").is_file());
    // With a lock file: `npm ci`.
    std::fs::remove_file(&log).unwrap();
    let app = app_with_hot_file();
    std::fs::write(app.path().join("package-lock.json"), "{}").unwrap();
    build_assets(app.path(), &npm).unwrap();
    assert_eq!(calls(&log), ["ci --no-audit --no-fund", "run build"]);
    // With node_modules: only the build.
    std::fs::remove_file(&log).unwrap();
    std::fs::create_dir_all(app.path().join("node_modules")).unwrap();
    build_assets(app.path(), &npm).unwrap();
    assert_eq!(calls(&log), ["run build"]);
}

#[test]
fn a_failing_or_missing_npm_fails_the_build() {
    let bin = tempfile::tempdir().unwrap();
    let app = app_with_hot_file();
    std::fs::create_dir_all(app.path().join("node_modules")).unwrap();
    let err = format!(
        "{:#}",
        build_assets(app.path(), &failing_npm(bin.path())).unwrap_err()
    );
    assert!(err.contains("`npm run build` failed"), "{err}");
    assert!(err.contains("ENOTFOUND"), "{err}");
    let err = format!(
        "{:#}",
        build_assets(app.path(), &Tool::new(bin.path().join("no-such-npm"))).unwrap_err()
    );
    assert!(err.contains("needs Node 20.19+ or 22.12+ and npm"), "{err}");
}

#[test]
fn smeltery_build_uses_npm_and_not_tailwind_in_a_vite_app() {
    let bin = tempfile::tempdir().unwrap();
    let (npm, log) = fake_npm(bin.path());
    let app = app_with_hot_file();
    std::fs::create_dir_all(app.path().join("resources/css")).unwrap();
    std::fs::write(
        app.path().join("resources/css/app.css"),
        "@import \"tailwindcss\";",
    )
    .unwrap();
    std::fs::create_dir_all(app.path().join("node_modules")).unwrap();
    crate::cmd::build_assets_with(app.path(), &npm, Ui::plain()).unwrap();
    assert_eq!(calls(&log), ["run build"]);
    // The standalone Tailwind step would have written public/assets/css/app.css (or warned); it did not run.
    assert!(!app.path().join("public/assets/css/app.css").exists());
}
