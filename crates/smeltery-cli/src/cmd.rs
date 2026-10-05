//! Commands that run `cargo` in the app or touch its files: `build` (with the CSS), `test`, `storage:link` and
//! delegation.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

use anyhow::{Context, bail};

use crate::serve::{CSS_INPUT, CSS_OUTPUT};
use crate::ui::{Badge, Ui};

/// Fails unless `dir` looks like a Smeltery app (a `Cargo.toml` and `bootstrap/main.rs`).
pub(crate) fn require_app(dir: &Path) -> anyhow::Result<()> {
    if !dir.join("Cargo.toml").is_file() || !dir.join("bootstrap").join("main.rs").is_file() {
        bail!(
            "not in a Smeltery app: {} has no Cargo.toml and bootstrap/main.rs",
            dir.display()
        );
    }
    Ok(())
}

/// The package name from the app's `Cargo.toml` (the first `name = "…"` line, which `smeltery new` writes under
/// `[package]`).
pub(crate) fn package_name(dir: &Path) -> anyhow::Result<String> {
    let path = dir.join("Cargo.toml");
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("name"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .map(|v| v.trim().trim_matches('"').to_owned())
        .next()
        .with_context(|| format!("no package name in {}", path.display()))
}

/// The cargo target directory of the app (`CARGO_TARGET_DIR` or `target/`).
pub(crate) fn target_dir(dir: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR").filter(|t| !t.is_empty()) {
        Some(t) => dir.join(t),
        None => dir.join("target"),
    }
}

/// The path of the app binary for `profile` (`debug` or `release`).
pub(crate) fn binary_path(dir: &Path, profile: &str) -> anyhow::Result<PathBuf> {
    let name = package_name(dir)?;
    Ok(target_dir(dir)
        .join(profile)
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
}

fn cargo(dir: &Path, args: &[&str], extra: &[String]) -> anyhow::Result<ExitStatus> {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(args)
        .args(extra)
        .current_dir(dir)
        .status()
        .context("cannot run cargo; is it installed and on PATH?")
}

/// Maps a child's exit status to ours (1 when it was killed by a signal).
pub(crate) fn exit_code(status: ExitStatus) -> ExitCode {
    match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        None => ExitCode::FAILURE,
    }
}

/// `smeltery build`: the assets, then `cargo build --release`. A React or Vue app (one with a Vite config) builds its
/// JavaScript and CSS with `npm run build` (D-281); other web apps build their CSS with Tailwind when the app has
/// `resources/css/app.css`.
pub(crate) fn build(dir: &Path, ui: Ui) -> anyhow::Result<ExitCode> {
    require_app(dir)?;
    build_assets_with(dir, &crate::frontend::npm_from_env(), ui)?;
    let status = cargo(dir, &["build", "--release"], &[])?;
    if !status.success() {
        return Ok(exit_code(status));
    }
    let bin = binary_path(dir, "release")?;
    println!("Built {}", bin.display());
    println!("Ship the public/ folder next to it; the binary serves files from there.");
    Ok(ExitCode::SUCCESS)
}

/// The asset step of `smeltery build`: npm for Vite apps, else the standalone Tailwind (skipped for Vite apps, whose
/// CSS `@tailwindcss/vite` or the kit's own stylesheet provides).
pub(crate) fn build_assets_with(dir: &Path, npm: &Tool, ui: Ui) -> anyhow::Result<()> {
    if crate::frontend::is_vite_app(dir) {
        return crate::frontend::build_assets(dir, npm);
    }
    build_css(dir, crate::tailwind::from_env().map(Tool::new), ui)
}

/// A program to run, with arguments that come before the command's own (tests run `sh <script>`).
#[derive(Debug, Clone)]
pub(crate) struct Tool {
    program: PathBuf,
    prefix: Vec<OsString>,
}

impl Tool {
    pub(crate) fn new(program: PathBuf) -> Self {
        Self {
            program,
            prefix: Vec::new(),
        }
    }

    /// A tool run as `sh <script>`: tests use it on Unix so no freshly written file is executed (ETXTBSY).
    #[cfg(all(test, not(windows)))]
    pub(crate) fn script(program: PathBuf, prefix: Vec<OsString>) -> Self {
        Self { program, prefix }
    }

    /// The program's path or name, for messages.
    pub(crate) fn program(&self) -> &Path {
        &self.program
    }

    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix);
        command
    }
}

/// Runs `tailwind -i resources/css/app.css -o public/assets/css/app.css --minify` once in `dir`. Without a Tailwind
/// binary it prints a warning and builds on; an app without `resources/css/app.css` (headless) has no CSS to build.
///
/// # Errors
/// Tailwind cannot be started or exits with a failure; the error carries its output.
fn build_css(dir: &Path, tailwind: Option<Tool>, ui: Ui) -> anyhow::Result<()> {
    if !dir.join(CSS_INPUT).is_file() {
        return Ok(());
    }
    let Some(tool) = tailwind else {
        let message = css_skipped(dir);
        if ui.styled() {
            eprintln!("{}", ui.badged(Badge::Warn, &message));
        } else {
            eprintln!("warning: {message}");
        }
        return Ok(());
    };
    let output = tool
        .command()
        .args(["-i", CSS_INPUT, "-o", CSS_OUTPUT, "--minify"])
        .current_dir(dir)
        .output()
        .with_context(|| format!("cannot run Tailwind ({})", tool.program.display()))?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let text = [stdout.trim(), stderr.trim()]
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "Tailwind failed ({}) building {CSS_OUTPUT}:\n{text}",
            output.status
        );
    }
    println!("Built {CSS_OUTPUT}");
    Ok(())
}

/// The warning `smeltery build` prints when no Tailwind binary is found.
fn css_skipped(dir: &Path) -> String {
    let state = if dir.join(CSS_OUTPUT).is_file() {
        "is left unchanged"
    } else {
        "is missing, so pages have no stylesheet"
    };
    format!(
        "CSS not built: no Tailwind binary found; {}. {CSS_OUTPUT} {state}.",
        crate::tailwind::HOW_TO_INSTALL
    )
}

/// `smeltery test`: `cargo test` with the given arguments.
pub(crate) fn test(dir: &Path, args: &[String]) -> anyhow::Result<ExitCode> {
    require_app(dir)?;
    Ok(exit_code(cargo(dir, &["test"], args)?))
}

/// Any other command: `cargo run --quiet -- <args>` in the app.
pub(crate) fn delegate(dir: &Path, args: &[String]) -> anyhow::Result<ExitCode> {
    if require_app(dir).is_err() {
        let cmd = args.first().map(String::as_str).unwrap_or_default();
        bail!("unknown command `{cmd}`; app commands run inside a Smeltery app directory");
    }
    Ok(exit_code(cargo(dir, &["run", "--quiet", "--"], args)?))
}

/// `smeltery storage:link`: links `public/storage` to `../storage/app/public` (the code the app binary's own
/// `storage:link` runs).
pub(crate) fn storage_link(dir: &Path) -> anyhow::Result<()> {
    require_app(dir)?;
    let linked = smeltery_core::console::setup::link_storage(dir)?;
    println!("{}", linked.message());
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// A folder `require_app` accepts.
    pub(crate) fn fake_app() -> tempfile::TempDir {
        let dir = tmp();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"my-app\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("bootstrap")).unwrap();
        std::fs::write(dir.path().join("bootstrap/main.rs"), "fn main() {}\n").unwrap();
        dir
    }

    #[test]
    fn reads_the_package_name() {
        let dir = tmp();
        let toml =
            "[package]\nname = \"my-app\"\nversion = \"0.1.0\"\n\n[lib]\nname = \"my_app\"\n";
        std::fs::write(dir.path().join("Cargo.toml"), toml).unwrap();
        assert_eq!(package_name(dir.path()).ok().as_deref(), Some("my-app"));
    }

    #[test]
    fn outside_an_app_commands_refuse() {
        let dir = tmp();
        assert!(require_app(dir.path()).is_err());
        let err = delegate(dir.path(), &["route:list".to_owned()])
            .err()
            .map(|e| e.to_string());
        assert!(
            err.unwrap_or_default()
                .contains("unknown command `route:list`")
        );
        assert!(build(dir.path(), Ui::plain()).is_err());
        assert!(test(dir.path(), &[]).is_err());
        // storage:link creates nothing outside an app.
        assert!(storage_link(dir.path()).is_err());
        assert!(!dir.path().join("public").exists());
        assert!(!dir.path().join("storage").exists());
    }

    /// An app folder with the Tailwind input, as `smeltery new` writes it for apps with web pages.
    fn css_app() -> tempfile::TempDir {
        let dir = tmp();
        std::fs::create_dir_all(dir.path().join("resources/css")).unwrap();
        std::fs::write(dir.path().join(CSS_INPUT), "@import \"tailwindcss\";\n").unwrap();
        dir
    }

    /// A fake Tailwind in `dir`: it appends its arguments as one line to `public/assets/css/app.css`, or with `fail`
    /// prints an error and exits with 3. No real Tailwind is downloaded for tests. On Unix the script runs as
    /// `sh <script>`, so no freshly written file is ever executed (which can fail with ETXTBSY while another test
    /// thread forks).
    fn fake_tailwind(dir: &Path, fail: bool) -> Tool {
        #[cfg(windows)]
        let (name, script) = if fail {
            (
                "tw-fail.cmd",
                "@echo off\r\necho boom: bad input 1>&2\r\nexit /b 3\r\n",
            )
        } else {
            (
                "tw.cmd",
                "@echo off\r\nif not exist public\\assets\\css mkdir public\\assets\\css\r\n\
                 echo %*>> public\\assets\\css\\app.css\r\n",
            )
        };
        #[cfg(not(windows))]
        let (name, script) = if fail {
            ("tw-fail.sh", "echo 'boom: bad input' >&2\nexit 3\n")
        } else {
            (
                "tw.sh",
                "mkdir -p public/assets/css\necho \"$@\" >> public/assets/css/app.css\n",
            )
        };
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        if cfg!(windows) {
            Tool::new(path)
        } else {
            Tool {
                program: PathBuf::from("sh"),
                prefix: vec![path.into_os_string()],
            }
        }
    }

    #[test]
    fn build_css_runs_tailwind_once_with_minify() {
        let (app, bin_dir) = (css_app(), tmp());
        let tool = fake_tailwind(bin_dir.path(), false);
        let result = build_css(app.path(), Some(tool), Ui::plain());
        assert!(result.is_ok(), "{result:?}");
        let css = std::fs::read_to_string(app.path().join(CSS_OUTPUT)).unwrap();
        assert_eq!(css.lines().count(), 1, "Tailwind ran once: {css}");
        assert!(
            css.contains("-i resources/css/app.css -o public/assets/css/app.css --minify"),
            "{css}"
        );
        assert!(!css.contains("--watch"), "{css}");
    }

    #[test]
    fn a_failing_tailwind_fails_the_build_with_its_output() {
        let (app, bin_dir) = (css_app(), tmp());
        let tool = fake_tailwind(bin_dir.path(), true);
        let err = build_css(app.path(), Some(tool), Ui::plain())
            .err()
            .map(|e| format!("{e:#}"))
            .unwrap_or_default();
        assert!(err.contains("Tailwind failed"), "{err}");
        assert!(err.contains("boom: bad input"), "{err}");
        // A Tailwind binary that cannot be started fails too.
        let missing = Tool::new(bin_dir.path().join("no-such-tailwind"));
        assert!(build_css(app.path(), Some(missing), Ui::plain()).is_err());
    }

    #[test]
    fn without_tailwind_the_css_is_skipped_with_a_warning() {
        let app = css_app();
        assert!(build_css(app.path(), None, Ui::plain()).is_ok());
        assert!(!app.path().join(CSS_OUTPUT).exists());
        let warning = css_skipped(app.path());
        assert!(warning.contains("smeltery tailwind:install"), "{warning}");
        assert!(warning.contains("TAILWIND_BIN"), "{warning}");
        assert!(
            warning.contains("on PATH it must be named `tailwindcss`"),
            "{warning}"
        );
        assert!(
            warning.contains("public/assets/css/app.css is missing"),
            "{warning}"
        );
        std::fs::create_dir_all(app.path().join("public/assets/css")).unwrap();
        std::fs::write(app.path().join(CSS_OUTPUT), "old").unwrap();
        assert!(css_skipped(app.path()).contains("app.css is left unchanged"));
    }

    #[test]
    fn headless_apps_have_no_css_to_build() {
        let dir = tmp();
        // No resources/css/app.css: nothing runs, not even a missing binary.
        let missing = Tool::new(dir.path().join("no-such-tailwind"));
        assert!(build_css(dir.path(), Some(missing), Ui::plain()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn storage_link_creates_a_relative_link_and_accepts_a_second_run() {
        let dir = fake_app();
        storage_link(dir.path()).unwrap();
        let link = dir.path().join("public/storage");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("../storage/app/public")
        );
        std::fs::write(dir.path().join("storage/app/public/a.txt"), "hi").unwrap();
        assert_eq!(std::fs::read_to_string(link.join("a.txt")).unwrap(), "hi");
        // A second run is idempotent (D-212): it succeeds and leaves the link as it was.
        storage_link(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("../storage/app/public")
        );
        assert_eq!(std::fs::read_to_string(link.join("a.txt")).unwrap(), "hi");
    }
}
