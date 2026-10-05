use std::collections::BTreeSet;
use std::io::{BufRead as _, BufReader};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// What the fake release server answers.
#[derive(Clone)]
enum Reply {
    Body(Vec<u8>),
    Status(u16),
    /// Headers announcing 1000 bytes, 10 bytes, then silence.
    Stall,
    /// A 302 to this URL.
    Redirect(String),
    /// A body of this many bytes without a `Content-Length` (the connection closes at the end).
    Unsized(usize),
}

/// A release server on 127.0.0.1 answering every request with `reply`; returns its base URL and a hit counter.
fn serve(reply: Reply) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                if line == "\r\n" {
                    break;
                }
                line.clear();
            }
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = match &reply {
                Reply::Body(body) => stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .and_then(|()| stream.write_all(body)),
                Reply::Status(code) => stream.write_all(
                    format!(
                        "HTTP/1.1 {code} Nope\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                ),
                Reply::Stall => stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789")
                    .map(|()| std::thread::sleep(Duration::from_secs(3))),
                Reply::Redirect(location) => stream.write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                ),
                Reply::Unsized(len) => stream
                    .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                    .and_then(|()| stream.write_all(&vec![b'x'; *len])),
            };
        }
    });
    (format!("http://{addr}/v{VERSION}"), hits)
}

const BINARY: &[u8] = b"#!fake tailwind binary\n";

fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn installer(base_url: String, dir: &Path) -> Installer {
    Installer {
        base_url,
        dir: dir.to_path_buf(),
        asset_name: "tailwindcss-test-x64".to_owned(),
        sha256: sha(BINARY),
        file_name: file_name(),
        connect_timeout: Duration::from_secs(5),
        read_timeout: Duration::from_millis(300),
        max_bytes: 4096,
    }
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn downloads_verifies_and_installs() {
    let (url, hits) = serve(Reply::Body(BINARY.to_vec()));
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let installed = installer(url, &bin).install().unwrap();
    assert_eq!(installed, Installed::Downloaded(bin.join(file_name())));
    assert_eq!(std::fs::read(installed.path()).unwrap(), BINARY);
    assert_eq!(
        files(&bin),
        [file_name(), format!("{}.verified", file_name())],
        "no temporary file is left"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(installed.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}

#[test]
fn a_verified_binary_is_reused_without_a_download() {
    let (url, hits) = serve(Reply::Body(BINARY.to_vec()));
    let dir = tempfile::tempdir().unwrap();
    let inst = installer(url, dir.path());
    assert!(matches!(inst.install().unwrap(), Installed::Downloaded(_)));
    assert_eq!(
        inst.install().unwrap(),
        Installed::Reused(dir.path().join(file_name()))
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // A damaged file is downloaded again and replaced.
    std::fs::write(inst.target(), b"damaged").unwrap();
    assert!(matches!(inst.install().unwrap(), Installed::Downloaded(_)));
    assert_eq!(std::fs::read(inst.target()).unwrap(), BINARY);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn a_checksum_mismatch_keeps_no_file() {
    let (url, _) = serve(Reply::Body(b"something else".to_vec()));
    let dir = tempfile::tempdir().unwrap();
    let err = format!("{:#}", installer(url, dir.path()).install().unwrap_err());
    assert!(err.contains("SHA-256"), "{err}");
    assert!(err.contains("not kept"), "{err}");
    assert!(files(dir.path()).is_empty(), "{:?}", files(dir.path()));
}

#[test]
fn an_http_error_fails_with_the_status() {
    let (url, _) = serve(Reply::Status(404));
    let dir = tempfile::tempdir().unwrap();
    let err = format!("{:#}", installer(url, dir.path()).install().unwrap_err());
    assert!(err.contains("404"), "{err}");
    assert!(files(dir.path()).is_empty());
}

#[test]
fn a_stalled_download_times_out_and_keeps_no_file() {
    let (url, _) = serve(Reply::Stall);
    let dir = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let err = format!("{:#}", installer(url, dir.path()).install().unwrap_err());
    assert!(started.elapsed() < Duration::from_secs(3), "{err}");
    assert!(err.contains("cannot download"), "{err}");
    assert!(files(dir.path()).is_empty(), "{:?}", files(dir.path()));
}

#[test]
fn a_refused_connection_is_an_error() {
    // Bind and drop: nothing listens on the port.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let dir = tempfile::tempdir().unwrap();
    let inst = installer(format!("http://127.0.0.1:{port}"), dir.path());
    assert!(inst.install().is_err());
}

#[test]
fn lookup_order_is_env_then_user_install_then_path() {
    let dir = tempfile::tempdir().unwrap();
    let on_path = dir.path().join("path");
    std::fs::create_dir(&on_path).unwrap();
    let exe = on_path.join(format!("tailwindcss{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&exe, "").unwrap();
    let user = dir.path().join("user-tw");
    let path = Some(on_path.as_os_str().to_owned());

    assert_eq!(
        find(Some("/opt/tw".into()), Some(user.clone()), path.clone()),
        Some(PathBuf::from("/opt/tw"))
    );
    assert_eq!(
        find(Some("".into()), Some(user.clone()), path.clone()),
        Some(user)
    );
    assert_eq!(find(None, None, path), Some(exe));
    assert_eq!(find(None, None, None), None);
}

/// S6-13: relative `PATH` entries (empty, `.`, `bin`) name the current folder, which is the app; they are skipped.
#[test]
fn path_lookup_skips_relative_entries() {
    let dir = tempfile::tempdir().unwrap();
    let exe = format!("tailwindcss{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::join_paths([
        PathBuf::from("."),
        PathBuf::from("node_modules/.bin"),
        dir.path().to_path_buf(),
    ])
    .unwrap();
    assert_eq!(path_candidates(&path), [dir.path().join(&exe)]);
    let empty_entry = format!(
        "{}{}",
        if cfg!(windows) { ";" } else { ":" },
        dir.path().display()
    );
    assert_eq!(
        path_candidates(std::ffi::OsStr::new(&empty_entry)),
        [dir.path().join(&exe)]
    );
}

/// S6-13: the per-user binary runs only while it is the pinned release; a replaced one is refused.
#[test]
fn a_replaced_user_binary_is_not_run() {
    let (url, _) = serve(Reply::Body(BINARY.to_vec()));
    let dir = tempfile::tempdir().unwrap();
    let inst = installer(url, dir.path());
    inst.install().unwrap();
    let pinned = sha(BINARY);
    assert_eq!(
        verify_installed(dir.path(), &file_name(), &pinned, None),
        Ok(Some(inst.target()))
    );
    std::fs::write(inst.target(), b"#!evil tailwind\n").unwrap();
    let err = verify_installed(dir.path(), &file_name(), &pinned, None).unwrap_err();
    assert!(err.contains("SHA-256"), "{err}");
    // Nothing installed: nothing to check.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        verify_installed(empty.path(), &file_name(), &pinned, None),
        Ok(None)
    );
}

/// The stamp saves the 110 MB hash on later runs; a stamp that does not match the file is not trusted.
#[test]
fn the_verified_stamp_is_used_only_when_it_matches() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join(file_name());
    std::fs::write(&bin, BINARY).unwrap();
    // As `tailwind:install` leaves them, whatever the umask (a group-writable binary or folder is refused on Unix;
    // `tempdir()` follows the umask, 775 under 002).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let pinned = sha(BINARY);
    let stamp = dir.path().join(format!("{}.verified", file_name()));
    assert!(!stamp.exists());
    assert_eq!(
        verify_installed(dir.path(), &file_name(), &pinned, None),
        Ok(Some(bin.clone()))
    );
    let written = std::fs::read_to_string(&stamp).unwrap();
    assert!(written.ends_with(&format!(" {pinned}\n")), "{written}");
    // A stamp for another hash is not the pinned one's: the file is hashed again (and passes).
    std::fs::write(
        &stamp,
        format!("{} {}\n", fingerprint(&bin).unwrap(), "0".repeat(64)),
    )
    .unwrap();
    assert_eq!(
        verify_installed(dir.path(), &file_name(), &pinned, None),
        Ok(Some(bin.clone()))
    );
    assert_eq!(std::fs::read_to_string(&stamp).unwrap(), written);
    // A binary changed after its stamp was written no longer matches the stamp's size and time: hashed, refused.
    std::fs::write(&bin, b"#!another binary, longer than the first\n").unwrap();
    assert!(verify_installed(dir.path(), &file_name(), &pinned, None).is_err());
}

/// S6-13: on Unix a binary in a folder other users can write to is refused, and `tailwind:install` tightens it.
#[cfg(unix)]
#[test]
fn a_user_binary_others_can_replace_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let (url, _) = serve(Reply::Body(BINARY.to_vec()));
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let inst = installer(url, &bin);
    inst.install().unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o777)).unwrap();
    let err = verify_installed(&bin, &file_name(), &sha(BINARY), None).unwrap_err();
    assert!(err.contains("writable by other users"), "{err}");
    assert!(matches!(inst.install().unwrap(), Installed::Reused(_)));
    assert_eq!(
        verify_installed(&bin, &file_name(), &sha(BINARY), None),
        Ok(Some(inst.target()))
    );
}

/// R-3: on Unix every folder between the binary and the user's data folder is checked, not only `bin`.
#[cfg(unix)]
#[test]
fn a_shared_folder_above_bin_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let mode =
        |p: &Path, m: u32| std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
    let (url, _) = serve(Reply::Body(BINARY.to_vec()));
    let base = tempfile::tempdir().unwrap();
    let ours = base.path().join("smeltery");
    let bin = ours.join("bin");
    installer(url, &bin).install().unwrap();
    let check = || verify_installed(&bin, &file_name(), &sha(BINARY), Some(base.path()));
    assert_eq!(check(), Ok(Some(bin.join(file_name()))));
    // `smeltery/` writable by others: someone could rename `bin` away and put their own in its place.
    mode(&ours, 0o777);
    let err = check().unwrap_err();
    assert!(
        err.contains("writable by other users") && err.contains("smeltery"),
        "{err}"
    );
    mode(&ours, 0o755);
    // The data folder itself may be group-writable (per-user groups), never writable by others.
    mode(base.path(), 0o775);
    assert!(check().is_ok());
    mode(base.path(), 0o777);
    assert!(check().unwrap_err().contains("writable by other users"));
    mode(base.path(), 0o700);
    // Without the data folder only `bin` is checked (the tests' own temp folders).
    mode(&ours, 0o777);
    assert!(verify_installed(&bin, &file_name(), &sha(BINARY), None).is_ok());
}

#[test]
fn every_supported_platform_has_a_pinned_checksum() {
    for (os, arch, musl) in [
        ("linux", "x86_64", false),
        ("linux", "x86_64", true),
        ("linux", "aarch64", false),
        ("linux", "aarch64", true),
        ("macos", "x86_64", false),
        ("macos", "aarch64", false),
        ("windows", "x86_64", false),
    ] {
        let asset = asset_for(os, arch, musl).unwrap();
        assert!(asset.name.starts_with("tailwindcss-"));
        assert_eq!(asset.sha256.len(), 64);
        assert!(asset.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
    }
    assert_eq!(
        asset_for("macos", "aarch64", true).map(|a| a.name),
        Some("tailwindcss-macos-arm64"),
        "musl only matters on Linux"
    );
    assert_eq!(asset_for("windows", "aarch64", false), None);
    assert_eq!(asset_for("freebsd", "x86_64", false), None);
    assert_eq!(
        file_name(),
        format!("tailwindcss-v{VERSION}{}", std::env::consts::EXE_SUFFIX)
    );
}

// --- The prebuilt CSS of new web apps (D-206) ---

fn templates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("templates")
}

const PREBUILT: &str = "new/public/assets/css/app.css";
const MANIFEST: &str = "css/classes.txt";

/// `text` with every `{% … %}`, `{# … #}` and `{{ … }}` span (Jinja and Mold syntax) replaced by a space.
fn strip_template_syntax(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(['{']) {
        let close = match rest.get(start..start + 2) {
            Some("{%") => "%}",
            Some("{#") => "#}",
            Some("{{") => "}}",
            _ => {
                out.push_str(rest.get(..=start).unwrap_or_default());
                rest = rest.get(start + 1..).unwrap_or_default();
                continue;
            }
        };
        out.push_str(rest.get(..start).unwrap_or_default());
        out.push(' ');
        rest = match rest
            .get(start + 2..)
            .and_then(|r| r.find(close).map(|end| (r, end)))
        {
            Some((r, end)) => r.get(end + 2..).unwrap_or_default(),
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// The class names of every `class="…"` attribute in a template's text, template syntax removed first.
fn class_tokens(text: &str) -> Vec<String> {
    attribute_tokens(text, "class=\"")
}

/// The class names of every `<attr>"…"` attribute (`class="`, React's `className="`), template syntax removed first.
fn attribute_tokens(text: &str, attr: &str) -> Vec<String> {
    let text = strip_template_syntax(text);
    let mut out = Vec::new();
    for chunk in text.split(attr).skip(1) {
        let value = chunk.split('"').next().unwrap_or_default();
        out.extend(
            value
                .split_whitespace()
                .filter(|t| !t.contains('@'))
                .map(str::to_owned),
        );
    }
    out
}

#[test]
fn class_tokens_skip_template_syntax() {
    let line = r#"<div class="mt-4 grid gap-4 md:grid-cols-2{% endraw %}{% if agents %} lg:grid-cols-3{% endif %}{% raw %}">"#;
    assert_eq!(
        class_tokens(line),
        ["mt-4", "grid", "gap-4", "md:grid-cols-2", "lg:grid-cols-3"]
    );
    let mold = r#"<p class="{{ kind }} text-sm">{{ old("x") }}</p><a class="link">"#;
    assert_eq!(class_tokens(mold), ["text-sm", "link"]);
    assert_eq!(strip_template_syntax("a{# note #}b{x}"), "a b{x}");
}

/// Every class named in a `class="…"` attribute of a view template (`*.mold.html`, `*.mold.html.jinja`) under
/// `templates/new` and `templates/make`; tokens with template syntax are skipped.
fn template_classes() -> BTreeSet<String> {
    fn walk(dir: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !(name.ends_with(".mold.html") || name.ends_with(".mold.html.jinja")) {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            out.extend(class_tokens(&text));
        }
    }
    let mut out = BTreeSet::new();
    walk(&templates_dir().join("new"), &mut out);
    walk(&templates_dir().join("make"), &mut out);
    out
}

/// The manifest's first line: the Tailwind version and the hash of the Tailwind input, so a change to either makes
/// the prebuilt CSS stale.
fn manifest_header() -> String {
    let input = std::fs::read_to_string(templates_dir().join("new/resources/css/app.css"))
        .unwrap()
        .replace("\r\n", "\n");
    format!(
        "# Tailwind CSS v{VERSION}, resources/css/app.css sha256 {}. The classes in the view templates when \
         templates/{PREBUILT} was built; regenerate with TAILWIND_BIN=<tailwindcss v{VERSION}> SMELTERY_BLESS=1 \
         cargo test -p smeltery-cli prebuilt_css -- --ignored",
        sha(input.as_bytes())
    )
}

fn manifest_text(classes: &BTreeSet<String>) -> String {
    let mut text = manifest_header();
    text.push('\n');
    for class in classes {
        text.push_str(class);
        text.push('\n');
    }
    text
}

#[test]
fn the_prebuilt_css_covers_every_template_class() {
    let manifest = std::fs::read_to_string(templates_dir().join(MANIFEST)).unwrap();
    let mut lines = manifest.lines();
    assert_eq!(
        lines.next(),
        Some(manifest_header().as_str()),
        "the prebuilt CSS was built with another Tailwind version or resources/css/app.css: regenerate it (see templates/{MANIFEST})"
    );
    let built: BTreeSet<&str> = lines.collect();
    let missing: Vec<String> = template_classes()
        .into_iter()
        .filter(|c| !built.contains(c.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "classes used in the templates but not in the prebuilt CSS: {missing:?}; regenerate it (see templates/{MANIFEST})"
    );
    let css = std::fs::read_to_string(templates_dir().join(PREBUILT)).unwrap();
    assert!(
        css.starts_with(&format!("/*! tailwindcss v{VERSION} ")),
        "{}",
        css.get(..80).unwrap_or_default()
    );
}

/// Builds the CSS of a fresh web app with Watchfire (with authentication and Alpine.js) with the real Tailwind
/// (`TAILWIND_BIN`) and compares it with the prebuilt file and the manifest; `SMELTERY_BLESS=1` writes both. CI runs it with the pinned, checksum-verified
/// release.
#[test]
#[ignore = "needs TAILWIND_BIN pointing at the pinned tailwindcss release"]
fn prebuilt_css_matches_a_fresh_tailwind_build() {
    use crate::new::{Db, Frontend, NewOptions, Shape};
    let bin = std::env::var_os("TAILWIND_BIN").expect("TAILWIND_BIN");
    let version = std::process::Command::new(&bin)
        .arg("--help")
        .output()
        .unwrap();
    let banner = String::from_utf8_lossy(&version.stdout).to_string()
        + &String::from_utf8_lossy(&version.stderr);
    assert!(banner.contains(&format!("v{VERSION}")), "{banner}");

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("css-app");
    let opts = NewOptions {
        kind: Shape::WebWatchfire.kind(),
        blocks: Shape::WebWatchfire.blocks(true),
        db: Db::Sqlite,
        frontend: Some(Frontend::Mold),
        // Authentication and Alpine.js on: the app with every view (D-232, D-233).
        alpine: true,
        ..NewOptions::defaults("css-app")
    };
    // Fixed key and time: the app's text, which Tailwind scans, is the same on every run.
    let key = "base64:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    crate::new::generate(&opts, &root, None, key, 1_791_028_800).unwrap();
    // The welcome page without Alpine.js has three cards (`lg:grid-cols-3`); Tailwind scans it as an extra view.
    let plain = tmp.path().join("plain-app");
    let plain_opts = NewOptions {
        alpine: false,
        ..opts.clone()
    };
    crate::new::generate(&plain_opts, &plain, None, key, 1_791_028_800).unwrap();
    std::fs::copy(
        plain.join("resources/views/home.mold.html"),
        root.join("resources/views/home-without-alpine.mold.html"),
    )
    .unwrap();
    // The views the generators write use the same classes, so the prebuilt CSS covers them too.
    for argv in [
        &[
            "make:model",
            "Post",
            "title:string",
            "body:text?",
            "published:bool",
            "image:file?",
            "--all",
        ][..],
        &["make:controller", "About"],
        &["make:spark", "Todo"],
    ] {
        use clap::Parser as _;
        let cli =
            crate::Cli::try_parse_from(std::iter::once("smeltery").chain(argv.iter().copied()))
                .unwrap();
        let ctx = crate::make::Ctx {
            root: &root,
            now: 1_791_028_900,
        };
        let plan = match cli.command {
            crate::Command::MakeModel(a) => crate::make::model(ctx, &a),
            crate::Command::MakeController(a) => crate::make::controller(ctx, &a),
            crate::Command::MakeSpark(a) => crate::make::spark(ctx, &a),
            _ => unreachable!(),
        }
        .unwrap();
        crate::make::apply(&root, &plan).unwrap();
    }
    let out = root.join("public/assets/css/app.css");
    std::fs::remove_file(&out).unwrap();
    let status = std::process::Command::new(&bin)
        .args([
            "-i",
            "resources/css/app.css",
            "-o",
            "public/assets/css/app.css",
            "--minify",
        ])
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success());
    let fresh = std::fs::read_to_string(&out).unwrap();
    let manifest = manifest_text(&template_classes());
    if std::env::var_os("SMELTERY_BLESS").is_some() {
        std::fs::write(templates_dir().join(PREBUILT), &fresh).unwrap();
        std::fs::write(templates_dir().join(MANIFEST), &manifest).unwrap();
        return;
    }
    let prebuilt = std::fs::read_to_string(templates_dir().join(PREBUILT)).unwrap();
    assert!(
        fresh == prebuilt,
        "templates/{PREBUILT} differs from a fresh Tailwind build; regenerate it (see templates/{MANIFEST})"
    );
    assert_eq!(
        std::fs::read_to_string(templates_dir().join(MANIFEST)).unwrap(),
        manifest
    );
}

/// CI regenerates the prebuilt CSS with the same pinned release (skipped outside the repository checkout).
#[test]
fn ci_pins_the_same_tailwind_release() {
    let ci = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml");
    let Ok(ci) = std::fs::read_to_string(ci) else {
        return;
    };
    let linux = asset_for("linux", "x86_64", false).unwrap();
    assert!(
        ci.contains(&format!("/v{VERSION}/{}", linux.name)),
        "ci.yml downloads another Tailwind version"
    );
    assert!(ci.contains(linux.sha256), "ci.yml checks another checksum");
}

/// S6-10: CI runs with a read-only token, every action pinned to a full commit SHA (the tag in a comment), checkouts
/// that do not keep the token in `.git/config`, and no event data interpolated into a shell script.
#[test]
fn ci_actions_are_pinned_and_the_token_is_read_only() {
    let ci = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml");
    let Ok(ci) = std::fs::read_to_string(ci) else {
        return;
    };
    assert!(
        ci.contains("\npermissions:\n  contents: read\n"),
        "ci.yml sets no read-only top-level permissions"
    );
    let lines: Vec<&str> = ci.lines().collect();
    let mut checkouts = 0;
    for (i, line) in lines.iter().enumerate() {
        let Some(action) = line
            .trim_start()
            .trim_start_matches("- ")
            .strip_prefix("uses: ")
        else {
            continue;
        };
        let (name, rest) = action.split_once('@').unwrap_or((action, ""));
        let (sha, comment) = rest.split_once(" # ").unwrap_or((rest, ""));
        assert!(
            sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "ci.yml line {}: {name} is not pinned to a commit SHA",
            i + 1
        );
        assert!(!comment.is_empty(), "ci.yml line {}: no tag comment", i + 1);
        if name == "actions/checkout" {
            checkouts += 1;
            let next: Vec<&str> = lines.iter().skip(i + 1).take(2).map(|l| l.trim()).collect();
            assert_eq!(
                next,
                ["with:", "persist-credentials: false"],
                "ci.yml line {}: checkout keeps the token",
                i + 1
            );
        }
    }
    assert!(checkouts > 0);
    assert!(!ci.contains("${{ github.event"), "event data in ci.yml");
}

/// Every building block, as the CI rows name it.
const EVERY_BLOCK: &str = "watchfire,temper,hallmark,anvil,prospect";

/// What `ci.yml` misses of the sweep's rules (W8-03, W8-05): every service image pinned by a digest with its tag in
/// a comment; an every-block generated app on each database; the starter-kit job generating every block.
fn ci_gaps(ci: &str) -> Vec<String> {
    let mut gaps = Vec::new();
    let lines: Vec<&str> = ci.lines().map(str::trim).collect();
    for (i, line) in lines.iter().enumerate() {
        let Some(image) = line.strip_prefix("image: ") else {
            continue;
        };
        let (reference, comment) = image.split_once(" # ").unwrap_or((image, ""));
        let digest = reference.split_once("@sha256:").map(|(_, d)| d);
        let pinned =
            digest.is_some_and(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()));
        if !pinned || comment.is_empty() {
            gaps.push(format!("line {}: {image} is not pinned by digest", i + 1));
        }
    }
    for db in ["sqlite", "postgres", "mysql"] {
        let row = lines.iter().enumerate().any(|(i, l)| {
            *l == format!("- db: {db}")
                && lines
                    .iter()
                    .skip(i + 1)
                    .take(3)
                    .any(|n| *n == format!("smelt: {EVERY_BLOCK}"))
        });
        if !row {
            gaps.push(format!(
                "no generated app with every building block on {db}"
            ));
        }
    }
    let kits = lines.iter().any(|l| {
        l.contains("new demo")
            && l.contains("--frontend ${{ matrix.kit }}")
            && l.contains(&format!("--smelt {EVERY_BLOCK}"))
    });
    if !kits {
        gaps.push("the starter-kit job does not generate every building block".to_owned());
    }
    gaps
}

/// W8-03 / W8-05: the real `ci.yml` has no gap (skipped outside the repository checkout).
#[test]
fn ci_pins_service_images_and_builds_every_block() {
    let ci = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml");
    let Ok(ci) = std::fs::read_to_string(ci) else {
        return;
    };
    assert_eq!(ci_gaps(&ci), Vec::<String>::new());
}

/// The check finds the shapes `ci.yml` had before (tags without digests, default blocks only).
#[test]
fn ci_gaps_finds_floating_images_and_missing_block_rows() {
    let before = "\
      redis:
        image: redis:7
          - db: sqlite
            url: sqlite://database/database.sqlite?mode=rwc
            smelt: watchfire,temper
          - db: postgres
            smelt: watchfire,temper
        image: postgres@sha256:abc # 16
          \"$CARGO_TARGET_DIR/debug/smeltery\" new demo --path x --frontend ${{ matrix.kit }} --npm
";
    let gaps = ci_gaps(before);
    assert_eq!(gaps.len(), 6, "{gaps:#?}");
    assert!(gaps.iter().any(|g| g.contains("redis:7")), "{gaps:#?}");
    assert!(
        gaps.iter().any(|g| g.contains("postgres@sha256:abc")),
        "{gaps:#?}"
    );
    for db in ["sqlite", "postgres", "mysql"] {
        assert!(gaps.iter().any(|g| g.ends_with(db)), "{gaps:#?}");
    }
    assert!(gaps.iter().any(|g| g.contains("starter-kit")), "{gaps:#?}");
}

/// W8-06: the Flutter example's Gradle wrapper checks the distribution it downloads (skipped outside the checkout).
#[test]
fn the_flutter_example_pins_its_gradle_distribution() {
    let props = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/flutter-client/android/gradle/wrapper/gradle-wrapper.properties");
    let Ok(props) = std::fs::read_to_string(props) else {
        return;
    };
    assert!(
        props
            .lines()
            .any(|l| l.starts_with("distributionUrl=https\\://services.gradle.org/")),
        "{props}"
    );
    let sum = props
        .lines()
        .find_map(|l| l.strip_prefix("distributionSha256Sum="))
        .unwrap_or_default();
    assert!(
        sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit()),
        "no distributionSha256Sum: {props}"
    );
}

#[test]
fn a_download_past_the_size_cap_keeps_no_file() {
    let dir = tempfile::tempdir().unwrap();
    // Announced too large: refused before reading.
    let (url, _) = serve(Reply::Body(vec![b'x'; 5000]));
    let err = format!("{:#}", installer(url, dir.path()).install().unwrap_err());
    assert!(err.contains("larger than"), "{err}");
    // No length announced: stopped while streaming.
    let (url, _) = serve(Reply::Unsized(10_000));
    let err = format!("{:#}", installer(url, dir.path()).install().unwrap_err());
    assert!(err.contains("larger than"), "{err}");
    assert!(files(dir.path()).is_empty(), "{:?}", files(dir.path()));
}

#[test]
fn a_redirect_to_another_host_is_refused() {
    let (target, target_hits) = serve(Reply::Body(BINARY.to_vec()));
    // The same server under another name: a different host for the policy.
    let elsewhere = target.replace("127.0.0.1", "localhost");
    let (url, _) = serve(Reply::Redirect(format!("{elsewhere}/tailwindcss-test-x64")));
    let dir = tempfile::tempdir().unwrap();
    let err = format!("{:#}", installer(url, dir.path()).install().unwrap_err());
    assert!(err.contains("cannot download"), "{err}");
    assert_eq!(
        target_hits.load(Ordering::SeqCst),
        0,
        "the redirect was not followed"
    );
    assert!(files(dir.path()).is_empty());
    // A redirect on the same host is followed.
    let (url, _) = serve(Reply::Redirect(format!("{target}/tailwindcss-test-x64")));
    assert!(matches!(
        installer(url, dir.path()).install().unwrap(),
        Installed::Downloaded(_)
    ));
}

#[test]
fn github_downloads_follow_only_https_github_redirects() {
    let base = format!("{RELEASES}/v{VERSION}");
    assert!(base.starts_with("https://github.com/"));
    for (scheme, host, ok) in [
        ("https", "github.com", true),
        ("https", "release-assets.githubusercontent.com", true),
        ("https", "objects.githubusercontent.com", true),
        ("http", "release-assets.githubusercontent.com", false),
        ("https", "githubusercontent.com.evil.example", false),
        ("https", "example.com", false),
    ] {
        assert_eq!(
            redirect_allowed(&base, scheme, host),
            ok,
            "{scheme}://{host}"
        );
    }
    assert!(redirect_allowed(
        "http://127.0.0.1:9/v1",
        "http",
        "127.0.0.1"
    ));
    assert!(!redirect_allowed(
        "http://127.0.0.1:9/v1",
        "http",
        "localhost"
    ));
}

#[test]
fn stale_part_files_are_removed() {
    let dir = tempfile::tempdir().unwrap();
    let inst = installer("http://127.0.0.1:9".to_owned(), dir.path());
    let stale = dir.path().join(format!("{}.111.part", file_name()));
    let fresh = dir.path().join(format!("{}.222.part", file_name()));
    let other = dir.path().join("notes.part");
    for f in [&stale, &fresh, &other] {
        std::fs::write(f, "x").unwrap();
    }
    let two_hours_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(two_hours_ago)
        .unwrap();
    inst.remove_stale_parts(Duration::from_secs(60 * 60));
    assert!(!stale.exists());
    assert!(fresh.exists(), "a running download's part file stays");
    assert!(other.exists(), "other files stay");
}

// --- The prebuilt CSS of the React and Vue kits without Tailwind (D-287) ---

const KIT_PREBUILT: &str = "new-alloy/resources/css/app.prebuilt.css";
const KIT_MANIFEST: &str = "css/classes-alloy.txt";

/// Every class of the kits' templates: `className="…"` in `templates/new-react/**/*.tsx[.jinja]`, `class="…"` in
/// `templates/new-vue/**/*.vue[.jinja]` and in the root template (`templates/new-alloy/**/*.mold.html[.jinja]`).
fn kit_classes() -> BTreeSet<String> {
    fn walk(dir: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let name = name.strip_suffix(".jinja").unwrap_or(&name);
            let attr = if name.ends_with(".tsx") {
                "className=\""
            } else if name.ends_with(".vue") || name.ends_with(".mold.html") {
                "class=\""
            } else {
                continue;
            };
            out.extend(attribute_tokens(
                &std::fs::read_to_string(&path).unwrap(),
                attr,
            ));
        }
    }
    let mut out = BTreeSet::new();
    // The kits, and the pages the generators write into kit apps (`make:model --all`, `make:page`).
    for kit in [
        "new-alloy",
        "new-react",
        "new-vue",
        "make/react",
        "make/vue",
    ] {
        walk(&templates_dir().join(kit), &mut out);
    }
    out
}

/// The kit manifest's first line: the Tailwind version and the hash of the kits' Tailwind input.
fn kit_manifest_header() -> String {
    let input = std::fs::read_to_string(templates_dir().join("new-alloy/resources/css/app.css"))
        .unwrap()
        .replace("\r\n", "\n");
    format!(
        "# Tailwind CSS v{VERSION}, new-alloy/resources/css/app.css sha256 {}. The classes in the React and Vue kits \
         when templates/{KIT_PREBUILT} was built; regenerate with TAILWIND_BIN=<tailwindcss v{VERSION}> \
         SMELTERY_BLESS=1 cargo test -p smeltery-cli prebuilt_css -- --ignored",
        sha(input.as_bytes())
    )
}

#[test]
fn kit_class_tokens_read_class_name_and_class() {
    let tsx =
        r#"<p className="mt-2 text-sm">{{ x }}<span className={x}></span><b class="no"></b></p>"#;
    assert_eq!(attribute_tokens(tsx, "className=\""), ["mt-2", "text-sm"]);
    let vue = r#"<p class="mt-2{% if agents %} lg:grid-cols-3{% endif %}">{{ title }}</p>"#;
    assert_eq!(
        attribute_tokens(vue, "class=\""),
        ["mt-2", "lg:grid-cols-3"]
    );
}

#[test]
fn the_kit_prebuilt_css_covers_every_kit_class() {
    let manifest = std::fs::read_to_string(templates_dir().join(KIT_MANIFEST)).unwrap();
    let mut lines = manifest.lines();
    assert_eq!(
        lines.next(),
        Some(kit_manifest_header().as_str()),
        "the kits' prebuilt CSS was built with another Tailwind version or input: regenerate it (see templates/{KIT_MANIFEST})"
    );
    let built: BTreeSet<&str> = lines.collect();
    let missing: Vec<String> = kit_classes()
        .into_iter()
        .filter(|c| !built.contains(c.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "classes used in the kits but not in their prebuilt CSS: {missing:?}; regenerate it (see templates/{KIT_MANIFEST})"
    );
    let css = std::fs::read_to_string(templates_dir().join(KIT_PREBUILT)).unwrap();
    assert!(
        css.starts_with(&format!("/*! tailwindcss v{VERSION} ")),
        "{}",
        css.get(..80).unwrap_or_default()
    );
}

/// Builds the kits' CSS with the real Tailwind (`TAILWIND_BIN`) from a React app with every page plus the Vue
/// kit's pages, and compares it with the prebuilt file and the manifest; `SMELTERY_BLESS=1` writes both.
#[test]
#[ignore = "needs TAILWIND_BIN pointing at the pinned tailwindcss release"]
fn kit_prebuilt_css_matches_a_fresh_tailwind_build() {
    use crate::new::{Frontend, NewOptions, Shape};
    let bin = std::env::var_os("TAILWIND_BIN").expect("TAILWIND_BIN");
    let tmp = tempfile::tempdir().unwrap();
    let key = "base64:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    let kit = |frontend: Frontend, name: &str| {
        let root = tmp.path().join(name);
        let opts = NewOptions {
            kind: Shape::WebWatchfire.kind(),
            blocks: Shape::WebWatchfire.blocks(true),
            frontend: Some(frontend),
            // With Tailwind: the app gets the Tailwind input (`resources/css/app.css`). Authentication on: every page.
            tailwind: true,
            ..NewOptions::defaults(name)
        };
        crate::new::generate(&opts, &root, None, key, 1_791_028_800).unwrap();
        root
    };
    let root = kit(Frontend::React, "react-app");
    let vue = kit(Frontend::Vue, "vue-app");
    // The pages the generators write use the kits' classes, so the prebuilt CSS covers them too.
    for app in [&root, &vue] {
        for argv in [
            &[
                "make:model",
                "Post",
                "title:string",
                "body:text?",
                "views:integer",
                "published:bool",
                "image:file?",
                "--all",
            ][..],
            &["make:controller", "About"],
            &["make:page", "Privacy"],
        ] {
            use clap::Parser as _;
            let cli =
                crate::Cli::try_parse_from(std::iter::once("smeltery").chain(argv.iter().copied()))
                    .unwrap();
            let ctx = crate::make::Ctx {
                root: app,
                now: 1_791_028_900,
            };
            let plan = match cli.command {
                crate::Command::MakeModel(a) => crate::make::model(ctx, &a),
                crate::Command::MakeController(a) => crate::make::controller(ctx, &a),
                crate::Command::MakePage(a) => crate::make::page(ctx, &a),
                _ => unreachable!(),
            }
            .unwrap();
            crate::make::apply(app, &plan).unwrap();
        }
    }
    // `@source "../js"` scans resources/js/ recursively: the Vue pages go in next to the React ones.
    copy_dir(&vue.join("resources/js"), &root.join("resources/js/vue"));
    let out = root.join("public/kit.css");
    let status = std::process::Command::new(&bin)
        .args([
            "-i",
            "resources/css/app.css",
            "-o",
            "public/kit.css",
            "--minify",
        ])
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success());
    let fresh = std::fs::read_to_string(&out).unwrap();
    let mut manifest = kit_manifest_header();
    manifest.push('\n');
    for class in kit_classes() {
        manifest.push_str(&class);
        manifest.push('\n');
    }
    if std::env::var_os("SMELTERY_BLESS").is_some() {
        std::fs::write(templates_dir().join(KIT_PREBUILT), &fresh).unwrap();
        std::fs::write(templates_dir().join(KIT_MANIFEST), &manifest).unwrap();
        return;
    }
    let prebuilt = std::fs::read_to_string(templates_dir().join(KIT_PREBUILT)).unwrap();
    assert!(
        fresh == prebuilt,
        "templates/{KIT_PREBUILT} differs from a fresh Tailwind build; regenerate it (see templates/{KIT_MANIFEST})"
    );
    assert_eq!(
        std::fs::read_to_string(templates_dir().join(KIT_MANIFEST)).unwrap(),
        manifest
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}
