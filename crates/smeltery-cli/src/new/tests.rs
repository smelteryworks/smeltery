use std::collections::BTreeSet;

use super::*;

const KEY: &str = "base64:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
/// 2026-10-03 12:00:00 UTC.
const NOW: u64 = 1_791_028_800;

fn opts(kind: Shape, db: Db) -> NewOptions {
    NewOptions {
        kind: kind.kind(),
        db,
        frontend: kind.has_web().then_some(Frontend::Mold),
        blocks: kind.blocks(kind.has_web()),
        tailwind: kind.has_web(),
        ..NewOptions::defaults("my-app")
    }
}

/// A web app (with or without Watchfire) with the given authentication and Alpine.js answers.
fn web_opts(kind: Shape, auth: bool, alpine: bool) -> NewOptions {
    NewOptions {
        blocks: kind.blocks(auth),
        alpine,
        ..opts(kind, Db::Sqlite)
    }
}

/// The summary's step results: Tailwind, npm (empty without a JavaScript kit), migrate, seed, git.
fn steps<'a>(
    tailwind: &'a str,
    npm: &'a str,
    migrated: &'a str,
    seeded: &'a str,
    git: &'a str,
) -> Steps<'a> {
    Steps {
        tailwind,
        npm,
        migrated,
        seeded,
        git,
    }
}

/// Generates into a fresh temp dir; returns the dir guard and the app root.
fn gen_app(o: &NewOptions, local: Option<&Path>) -> (tempfile::TempDir, PathBuf, Vec<String>) {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| unreachable!("tempdir: {e}"));
    let root = tmp.path().join(&o.name);
    let written =
        generate(o, &root, local, KEY, NOW).unwrap_or_else(|e| unreachable!("generate: {e:#}"));
    (tmp, root, written)
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| unreachable!("read {rel}: {e}"))
}

fn files_on_disk(root: &Path) -> BTreeSet<String> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else if let Ok(rel) = path.strip_prefix(base) {
                out.insert(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, root, &mut out);
    out
}

const COMMON: &[&str] = &[
    ".env",
    ".env.example",
    ".gitignore",
    "AGENTS.md",
    "CLAUDE.md",
    "Cargo.toml",
    "README.md",
    "app/commands/mod.rs",
    "app/helpers/mod.rs",
    "app/jobs/mod.rs",
    "app/mail/mod.rs",
    "app/middleware/mod.rs",
    "app/mod.rs",
    "app/models/mod.rs",
    "app/models/user.rs",
    "app/providers/mod.rs",
    "app/services/mod.rs",
    "bootstrap/app.rs",
    "bootstrap/main.rs",
    "config/app.rs",
    "config/database.rs",
    "config/mod.rs",
    "database/factories/mod.rs",
    "database/factories/user_factory.rs",
    "database/migrations/m2026_10_03_120000_create_users_table.rs",
    "database/migrations/m2026_10_03_120002_create_sessions_table.rs",
    "database/migrations/m2026_10_03_120004_create_cache_tables.rs",
    "database/migrations/mod.rs",
    "database/mod.rs",
    "database/seeders/database_seeder.rs",
    "database/seeders/mod.rs",
    "public/assets/css/.gitkeep",
    "public/assets/images/.gitkeep",
    "public/assets/js/.gitkeep",
    "public/robots.txt",
    "storage/app/private/.gitignore",
    "storage/app/public/.gitignore",
    "storage/framework/.gitignore",
    "storage/logs/.gitignore",
    "tests/http.rs",
];

const WEB: &[&str] = &[
    "app/sparks/counter.rs",
    "app/sparks/mod.rs",
    "app/controllers/home.rs",
    "app/controllers/mod.rs",
    "public/assets/css/app.css",
    "resources/css/app.css",
    "resources/views/components/card.mold.html",
    "resources/views/home.mold.html",
    "resources/views/layouts/app.mold.html",
    "resources/views/sparks/counter.mold.html",
    "routes/api.rs",
    "routes/mod.rs",
    "routes/web.rs",
];

/// Authentication (D-232), through Temper in the Mold kit (D-485): web apps with the `temper` building block only.
const AUTH: &[&str] = &[
    "app/actions/mod.rs",
    "app/actions/temper/create_new_user.rs",
    "app/actions/temper/mod.rs",
    "app/actions/temper/password_rules.rs",
    "app/actions/temper/reset_user_password.rs",
    "app/actions/temper/update_user_password.rs",
    "app/actions/temper/update_user_profile_information.rs",
    "app/controllers/dashboard.rs",
    "app/controllers/settings.rs",
    "app/providers/temper.rs",
    TWO_FACTOR,
    "resources/views/auth/confirm-password.mold.html",
    "resources/views/auth/forgot-password.mold.html",
    "resources/views/auth/login.mold.html",
    "resources/views/auth/register.mold.html",
    "resources/views/auth/reset-password.mold.html",
    "resources/views/auth/two-factor-challenge.mold.html",
    "resources/views/auth/verify-email.mold.html",
    "resources/views/dashboard.mold.html",
    "resources/views/settings/nav.mold.html",
    "resources/views/settings/password.mold.html",
    "resources/views/settings/profile.mold.html",
    "resources/views/settings/two-factor.mold.html",
];

/// The two-factor columns of Temper (D-485): apps with authentication.
const TWO_FACTOR: &str =
    "database/migrations/m2026_10_03_120006_add_two_factor_columns_to_users_table.rs";

/// The PubSub messages table: apps with Watchfire, which run as `serve --no-agents` + `work` (D-507).
const PUBSUB: &str = "database/migrations/m2026_10_03_120005_create_pubsub_messages_table.rs";

/// The password reset tokens table: apps with authentication, and headless apps (which always had it).
const RESETS: &str = "database/migrations/m2026_10_03_120001_create_password_reset_tokens_table.rs";

fn expected(kind: Shape) -> BTreeSet<String> {
    expected_for(kind, kind.has_web(), false)
}

fn expected_for(kind: Shape, auth: bool, alpine: bool) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = COMMON.iter().map(|s| (*s).to_owned()).collect();
    if kind.has_web() {
        set.extend(WEB.iter().map(|s| (*s).to_owned()));
    }
    if kind.has_web() && auth {
        set.extend(AUTH.iter().map(|s| (*s).to_owned()));
    }
    if !kind.has_web() || auth {
        set.insert(RESETS.to_owned());
    }
    if kind.has_web() && alpine {
        set.insert("public/assets/js/alpine.min.js".to_owned());
    }
    if kind.has_agents() {
        set.insert("app/agents/mod.rs".to_owned());
        set.insert("database/migrations/m2026_10_03_120003_create_watchfire_tables.rs".to_owned());
        set.insert(PUBSUB.to_owned());
    }
    set
}

#[test]
fn every_kind_and_db_generates_the_expected_files() {
    for kind in [Shape::WebWatchfire, Shape::Headless, Shape::Web] {
        for db in [Db::Sqlite, Db::Postgres, Db::Mysql] {
            let o = opts(kind, db);
            let (_tmp, root, written) = gen_app(&o, None);
            let on_disk = files_on_disk(&root);
            assert_eq!(on_disk, expected(kind), "{kind:?} {db:?}");
            assert_eq!(written.len(), on_disk.len());
            for file in &on_disk {
                let text = read(&root, file);
                // Mold views use `{{ }}`, and the agent guides show Mold syntax; no file keeps minijinja's `{% %}`.
                let mold = file.ends_with(".mold.html") || file.ends_with(".md");
                assert!(
                    (mold || !text.contains("{{")) && !text.contains("{%"),
                    "{file} has template syntax left"
                );
                if file.ends_with(".rs") || file.ends_with(".toml") || file.ends_with(".md") {
                    assert!(
                        text.ends_with('\n') && !text.ends_with("\n\n"),
                        "{file}: bad trailing newline"
                    );
                }
            }
            let feature = match db {
                Db::Sqlite => "sqlite",
                Db::Postgres => "postgres",
                Db::Mysql => "mysql",
            };
            assert!(read(&root, "Cargo.toml").contains(&format!("features = [\"{feature}\"]")));
            let url_prefix = format!("DATABASE_URL={feature}://");
            assert!(read(&root, ".env").contains(&url_prefix));
            assert!(read(&root, "config/database.rs").contains(&format!("\"{feature}://")));
            // SQLite lives in `database/` which every app has; git ignores it.
            let sqlite = db == Db::Sqlite;
            assert_eq!(
                read(&root, ".env")
                    .contains("DATABASE_URL=sqlite://database/database.sqlite?mode=rwc"),
                sqlite
            );
            assert!(root.join("database").is_dir());
            assert!(read(&root, ".gitignore").contains("/database/*.sqlite*\n"));
            assert_eq!(read(&root, "CLAUDE.md").contains("database.sqlite"), sqlite);
            let app_mod = read(&root, "app/mod.rs");
            assert_eq!(app_mod.contains("pub mod agents;"), kind.has_agents());
            assert_eq!(app_mod.contains("pub mod controllers;"), kind.has_web());
        }
    }
}

#[test]
fn auth_and_alpine_choose_the_files() {
    for kind in [Shape::WebWatchfire, Shape::Web] {
        for auth in [true, false] {
            for alpine in [true, false] {
                let (_tmp, root, written) = gen_app(&web_opts(kind, auth, alpine), None);
                let on_disk = files_on_disk(&root);
                assert_eq!(
                    on_disk,
                    expected_for(kind, auth, alpine),
                    "{kind:?} auth {auth} alpine {alpine}"
                );
                assert_eq!(written.len(), on_disk.len());
                for file in &on_disk {
                    let text = read(&root, file);
                    let mold = file.ends_with(".mold.html") || file.ends_with(".md");
                    assert!(
                        (mold || file.ends_with(".js") || !text.contains("{{"))
                            && !text.contains("{%"),
                        "{file} has template syntax left"
                    );
                }
                let layout = read(&root, "resources/views/layouts/app.mold.html");
                assert_eq!(layout.contains("@auth"), auth, "{layout}");
                assert_eq!(layout.contains("href=\"/login\""), auth, "{layout}");
                assert_eq!(layout.contains("alpine.min.js"), alpine, "{layout}");
                // Authentication is Temper (D-482): `.temper(…)` registers the user model.
                assert_eq!(
                    read(&root, "bootstrap/app.rs")
                        .contains(".temper(app::providers::temper::temper())"),
                    auth
                );
                assert!(!read(&root, "bootstrap/app.rs").contains(".auth::<"));
                assert_eq!(
                    read(&root, "app/models/user.rs")
                        .contains("impl smeltery::temper::TwoFactorAuthenticatable for Model"),
                    auth
                );
                assert_eq!(
                    read(&root, ".env.example").contains("\nAUTH_PASSWORD_TIMEOUT=10800\n"),
                    auth
                );
                assert!(!root.join("app/controllers/auth").exists());
                assert_eq!(read(&root, "routes/web.rs").contains("/dashboard"), auth);
                // Email verification is scaffolded with authentication, and off (D-236).
                assert_eq!(
                    read(&root, "bootstrap/app.rs").contains(
                        "        // .verify_email::<app::models::User>()
"
                    ),
                    auth
                );
                assert!(!read(&root, "bootstrap/app.rs").contains(
                    "
        .verify_email"
                ));
                assert_eq!(
                    read(&root, "routes/web.rs").contains(".middleware(\"verified\");"),
                    auth
                );
                assert_eq!(
                    read(&root, "app/models/user.rs")
                        .contains("impl smeltery::auth::MustVerifyEmail"),
                    auth
                );
                assert_eq!(
                    read(&root, ".env.example").contains(
                        "
AUTH_VERIFICATION_EXPIRE=60
"
                    ),
                    auth
                );
                assert_eq!(
                    read(&root, "database/seeders/database_seeder.rs").contains("demo@example.com"),
                    auth
                );
                assert_eq!(read(&root, "CLAUDE.md").contains("Alpine.js"), alpine);
                assert_eq!(read(&root, "CLAUDE.md"), read(&root, "AGENTS.md"));
            }
        }
    }
}

#[test]
fn the_layout_loads_alpine_after_the_sparks_runtime() {
    let (_tmp, root, _) = gen_app(&web_opts(Shape::WebWatchfire, true, true), None);
    let layout = read(&root, "resources/views/layouts/app.mold.html");
    let sparks = layout.find("@sparksScripts").unwrap_or(usize::MAX);
    let alpine = layout
        .find(&format!(
            "<script defer src=\"/assets/js/alpine.min.js?v={}\"></script>",
            templates::ALPINE_VERSION
        ))
        .unwrap_or(0);
    // Deferred scripts run in document order: sparks.js first, then Alpine.js (D-233).
    assert!(sparks < alpine, "{layout}");
    assert!(alpine < layout.find("</head>").unwrap_or(0), "{layout}");
}

#[test]
fn the_embedded_alpine_is_the_verified_release_with_its_licence() {
    use sha2::{Digest as _, Sha256};
    // `dist/cdn.min.js` of the npm package alpinejs@3.17.4, whose tarball matched the registry's sha512 integrity
    // (D-233). The file in the templates is this payload behind a licence header.
    const PAYLOAD_SHA256: &str = "232519394c6c8fdba6f362b1d9da16106db513cdbf899011f00daab4051df31c";
    let file = TEMPLATES
        .iter()
        .find(|t| t.out == "public/assets/js/alpine.min.js")
        .map(|t| t.source)
        .unwrap_or_default();
    let header_end = file.find(" */\n").map(|i| i + 4).unwrap_or(0);
    let header = file.get(..header_end).unwrap_or_default();
    assert!(
        header.starts_with(&format!("/*! Alpine.js v{} ", templates::ALPINE_VERSION)),
        "{header}"
    );
    for needle in [
        "MIT License",
        "Copyright © 2019-2025 Caleb Porzio and contributors",
        "The above copyright notice and this permission notice shall be included in all",
    ] {
        assert!(header.contains(needle), "{needle}");
    }
    let payload = file.get(header_end..).unwrap_or_default();
    let digest: String = Sha256::digest(payload.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(digest, PAYLOAD_SHA256);
    // The app gets the file byte for byte.
    let (_tmp, root, _) = gen_app(&web_opts(Shape::Web, true, true), None);
    assert_eq!(read(&root, "public/assets/js/alpine.min.js"), file);
}

#[test]
fn no_auth_and_alpine_files_match_golden() {
    let (_tmp, root, _) = gen_app(&web_opts(Shape::WebWatchfire, false, false), None);
    for rel in [
        "app/models/user.rs",
        "database/migrations/m2026_10_03_120000_create_users_table.rs",
        "database/factories/user_factory.rs",
        ".env.example",
        "app/controllers/mod.rs",
        "bootstrap/app.rs",
        "database/migrations/mod.rs",
        "database/seeders/database_seeder.rs",
        "resources/views/layouts/app.mold.html",
        "resources/views/home.mold.html",
        "routes/web.rs",
        "tests/http.rs",
        "README.md",
        "CLAUDE.md",
    ] {
        golden("web_no_auth", &root, rel);
    }
    let (_tmp, root, _) = gen_app(
        &NewOptions {
            bellows: Bellows {
                mcp: false,
                skills: true,
                guidelines: true,
            },
            ..web_opts(Shape::Web, false, false)
        },
        None,
    );
    for rel in [
        ".bellows/guidelines.md",
        ".bellows/skills/crud-resource.md",
        "CLAUDE.md",
    ] {
        golden("bellows_web_no_auth", &root, rel);
    }
    assert!(!root.join(".bellows/skills/auth-route.md").exists());
    let (_tmp, root, _) = gen_app(&web_opts(Shape::WebWatchfire, true, true), None);
    for rel in [
        "resources/views/layouts/app.mold.html",
        "resources/views/home.mold.html",
        "resources/views/sparks/counter.mold.html",
        "tests/http.rs",
        "README.md",
        "CLAUDE.md",
    ] {
        golden("web_alpine", &root, rel);
    }
    // Two or four cards sit in two columns; three get a third column on large screens.
    let (_tmp, root, _) = gen_app(&web_opts(Shape::Web, true, true), None);
    assert!(
        read(&root, "resources/views/home.mold.html").contains("md:grid-cols-2 lg:grid-cols-3\"")
    );
}

#[test]
fn mold_views_match_golden() {
    let (_tmp, root, _) = gen_app(&opts(Shape::Web, Db::Sqlite), None);
    // The welcome page has no Watchfire card without agents; the golden of the web app with Watchfire has it.
    let home = read(&root, "resources/views/home.mold.html");
    assert!(home.starts_with("@extends(\"layouts/app\")"), "{home}");
    assert!(!home.contains("/_watchfire"), "{home}");
    golden("web_only", &root, "resources/views/home.mold.html");
    golden(
        "web_only",
        &root,
        "resources/views/components/card.mold.html",
    );
    let home = read(&root, "app/controllers/home.rs");
    assert!(home.contains("#[derive(smeltery::Mold)]\n#[mold(\"home\")]\npub struct HomePage {"));
    assert!(home.contains("pub async fn index(app: smeltery::App) -> HomePage {"));
    assert!(home.contains("version: smeltery::VERSION,"));
}

#[test]
fn cargo_toml_matches_golden() {
    let (_tmp, root, _) = gen_app(&opts(Shape::WebWatchfire, Db::Sqlite), None);
    let want = format!(
        r#"[package]
name = "my-app"
version = "0.1.0"
edition = "2024"
publish = false

[lib]
name = "my_app"
path = "bootstrap/app.rs"

[[bin]]
name = "my-app"
path = "bootstrap/main.rs"

[dependencies]
smeltery = {{ version = "{}", default-features = false, features = ["sqlite"] }}
serde = {{ version = "1.0.229", features = ["derive"] }}

# An empty workspace table keeps this app its own workspace, even inside another one.
[workspace]
"#,
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(read(&root, "Cargo.toml"), want);
}

#[test]
fn local_smeltery_path_is_a_path_dependency() {
    let (_tmp, root, _) = gen_app(
        &opts(Shape::Web, Db::Postgres),
        Some(Path::new("/src/smeltery/crates/smeltery")),
    );
    assert!(read(&root, "Cargo.toml").contains(
        "smeltery = { path = \"/src/smeltery/crates/smeltery\", default-features = false, features = [\"postgres\"] }"
    ));
}

#[test]
fn bootstrap_app_matches_golden_for_every_kind() {
    for (kind, case) in [
        (Shape::WebWatchfire, "web"),
        (Shape::Web, "web_only"),
        (Shape::Headless, "headless"),
    ] {
        let (_tmp, root, _) = gen_app(&opts(kind, Db::Sqlite), None);
        golden(case, &root, "bootstrap/app.rs");
        golden(case, &root, "database/migrations/mod.rs");
        assert_eq!(
            read(&root, "bootstrap/app.rs").contains(".agents(app::agents::register)"),
            kind.has_agents()
        );
    }
    let (_tmp, root, _) = gen_app(&opts(Shape::Headless, Db::Sqlite), None);
    assert_eq!(
        read(&root, "bootstrap/main.rs"),
        "//! The my-app binary: `serve`, `route:list` and the other app commands.\n\nfn main() -> \
         std::process::ExitCode {\n    smeltery::run(my_app::build)\n}\n"
    );
}

#[test]
fn app_mod_lists_modules_sorted_above_the_marker() {
    let (_tmp, root, _) = gen_app(&opts(Shape::WebWatchfire, Db::Sqlite), None);
    assert_eq!(
        read(&root, "app/mod.rs"),
        "//! Application code, one module per folder.\n\npub mod actions;\npub mod agents;\npub mod commands;\n\
         pub mod controllers;\npub mod helpers;\npub mod jobs;\npub mod mail;\npub mod middleware;\npub mod models;\n\
         pub mod providers;\npub mod services;\npub mod sparks;\n// smeltery:mods\n"
    );
}

#[test]
fn env_has_the_key_and_example_has_none() {
    let (_tmp, root, _) = gen_app(&opts(Shape::WebWatchfire, Db::Sqlite), None);
    let env = read(&root, ".env");
    assert!(env.contains(&format!("\nAPP_KEY={KEY}\n")));
    assert!(env.starts_with("APP_NAME=\"My App\"\n"));
    let example = read(&root, ".env.example");
    assert!(example.contains("\nAPP_KEY=\n"));
    assert_eq!(env.replace(KEY, ""), example);
    for key in [
        "APP_ENV=local",
        "APP_DEBUG=true",
        "SERVER_HOST=127.0.0.1",
        "SERVER_PORT=8000",
        "LOG_LEVEL=debug",
        "LOG_FILE=storage/logs/smeltery.log",
        "SESSION_DRIVER=database",
    ] {
        assert!(example.contains(key), "{key}");
    }
    assert!(read(&root, "config/app.rs").contains("env(\"APP_NAME\", \"My App\")"));
    // S6-15: `.env` says local and debug; without those keys (a server's own environment) the app is production.
    let config = read(&root, "config/app.rs");
    assert!(
        config.contains("env: env(\"APP_ENV\", \"production\"),"),
        "{config}"
    );
    assert!(
        config.contains("debug: env(\"APP_DEBUG\", false),"),
        "{config}"
    );
    assert!(read(&root, "tests/http.rs").contains("contains(\"My App\")"));
    assert_eq!(read(&root, "CLAUDE.md"), read(&root, "AGENTS.md"));
}

/// Apps with Watchfire name their queue driver next to the cache store (D-512); apps without it have no queue line.
#[test]
fn env_names_the_queue_driver_only_with_watchfire() {
    for shape in [Shape::WebWatchfire, Shape::Headless] {
        let (_tmp, root, _) = gen_app(&opts(shape, Db::Sqlite), None);
        let env = read(&root, ".env");
        assert!(
            env.contains("CACHE_PREFIX=\n\n# Where queued jobs wait: database, redis"),
            "{env}"
        );
        assert!(env.contains("\nQUEUE_DRIVER=database\n"), "{env}");
        assert!(
            env.contains("\n# REDIS_URL=redis://127.0.0.1:6379\n"),
            "{env}"
        );
    }
    let (_tmp, root, _) = gen_app(&opts(Shape::Web, Db::Sqlite), None);
    let env = read(&root, ".env");
    assert!(!env.contains("QUEUE_DRIVER"), "{env}");
    assert!(!env.contains("REDIS_URL"), "{env}");
}

/// S6-11: `.env` (with APP_KEY) is owner-only, like `key:generate` writes it.
#[cfg(unix)]
#[test]
fn env_is_written_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_tmp, root, _) = gen_app(&opts(Shape::WebWatchfire, Db::Sqlite), None);
    let mode = |rel: &str| {
        std::fs::metadata(root.join(rel))
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or_default()
    };
    assert_eq!(mode(".env"), 0o600);
}

#[test]
fn bellows_parts_are_written_on_request() {
    let mut o = opts(Shape::WebWatchfire, Db::Sqlite);
    o.bellows = Bellows {
        mcp: true,
        skills: false,
        guidelines: true,
    };
    let (_tmp, root, _) = gen_app(&o, None);
    assert!(read(&root, ".mcp.json").contains("\"args\": [\"bellows:mcp\"]"));
    assert!(root.join(".bellows/guidelines.md").is_file());
    assert!(!root.join(".bellows/skills").exists());
    o.bellows = Bellows {
        mcp: false,
        skills: true,
        guidelines: false,
    };
    let (_tmp, root, _) = gen_app(&o, None);
    assert!(root.join(".bellows/skills/crud-resource.md").is_file());
    assert!(!root.join(".mcp.json").exists());
}

#[test]
fn refuses_a_non_empty_target() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| unreachable!("tempdir: {e}"));
    let root = tmp.path().join("my-app");
    assert!(std::fs::create_dir_all(&root).is_ok());
    // An empty directory is fine.
    assert!(generate(&opts(Shape::Web, Db::Sqlite), &root, None, KEY, NOW).is_ok());
    let err = generate(&opts(Shape::Web, Db::Sqlite), &root, None, KEY, NOW)
        .err()
        .map(|e| e.to_string());
    assert!(
        err.unwrap_or_default()
            .contains("already exists and is not empty")
    );
}

#[test]
fn names_are_validated() {
    for ok in ["my-app", "blog", "a1", "my_app", "shop-2"] {
        assert!(validate_name(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        "My-App",
        "1app",
        "-app",
        "my app",
        "app!",
        "smeltery",
        "test",
        "fn",
        "self",
        &"a".repeat(65),
    ] {
        assert!(validate_name(bad).is_err(), "{bad}");
    }
    assert_eq!(lib_name("my-cool-app"), "my_cool_app");
    assert_eq!(title("my-cool_app"), "My Cool App");
}

#[test]
fn one_line_command() {
    let o = NewOptions::defaults("my-app");
    assert_eq!(
        o.command_line(),
        "smeltery new my-app --kind web --db sqlite --frontend mold --tailwind --no-alpine --smelt watchfire,temper \
         --bellows none --migrate --seed --git"
    );
    // React and Vue: no Alpine.js, and the npm answer after Bellows (the order of the questions).
    let o = NewOptions {
        frontend: Some(Frontend::React),
        npm: true,
        ..NewOptions::defaults("my-app")
    };
    assert_eq!(
        o.command_line(),
        "smeltery new my-app --kind web --db sqlite --frontend react --tailwind --smelt watchfire,temper --bellows none \
         --npm --migrate --seed --git"
    );
    let o = NewOptions {
        kind: Shape::Web.kind(),
        blocks: Shape::Web.blocks(false),
        frontend: Some(Frontend::Vue),
        tailwind: false,
        npm: false,
        ..NewOptions::defaults("my-app")
    };
    assert_eq!(
        o.command_line(),
        "smeltery new my-app --kind web --db sqlite --frontend vue --no-tailwind --smelt none --bellows none --no-npm \
         --migrate --seed --git"
    );
    let o = NewOptions {
        tailwind: false,
        alpine: true,
        blocks: Blocks {
            watchfire: true,
            ..Blocks::NONE
        },
        ..NewOptions::defaults("my-app")
    };
    assert!(
        o.command_line().contains(
            " --db sqlite --frontend mold --no-tailwind --alpine --smelt watchfire --bellows none --migrate "
        ),
        "{}",
        o.command_line()
    );
    let o = NewOptions {
        kind: Shape::Headless.kind(),
        blocks: Shape::Headless.blocks(true),
        db: Db::Mysql,
        frontend: None,
        bellows: Bellows {
            mcp: true,
            skills: false,
            guidelines: true,
        },
        tailwind: false,
        migrate: false,
        seed: true,
        git: false,
        ..NewOptions::defaults("ops")
    };
    assert_eq!(
        o.command_line(),
        "smeltery new ops --kind headless --db mysql --bellows mcp,guidelines --no-migrate --seed --no-git"
    );
    let all = Bellows {
        mcp: true,
        skills: true,
        guidelines: true,
    };
    assert_eq!(all.to_string(), "all");
}

/// The one-line command creates the same app: parsing it gives the options back, for every kit and answer.
#[test]
fn the_one_line_command_round_trips() {
    use clap::Parser;
    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        args: NewArgs,
    }
    let mut cases = vec![NewOptions {
        kind: Shape::Headless.kind(),
        blocks: Shape::Headless.blocks(true),
        frontend: None,
        tailwind: false,
        ..NewOptions::defaults("ops")
    }];
    for kind in [Shape::WebWatchfire, Shape::Web] {
        for frontend in Frontend::ALL {
            for (tailwind, auth, extra) in [(true, true, true), (false, false, false)] {
                cases.push(NewOptions {
                    kind: kind.kind(),
                    blocks: kind.blocks(auth),
                    frontend: Some(frontend),
                    tailwind,
                    alpine: frontend == Frontend::Mold && extra,
                    npm: frontend.is_js() && extra,
                    migrate: extra,
                    ..NewOptions::defaults("shop")
                });
            }
        }
    }
    for frontend in Frontend::ALL {
        for blocks in [
            Blocks::default().with(Block::Hallmark).with(Block::Anvil),
            Blocks::NONE.with(Block::Anvil),
            Blocks::NONE.with(Block::Auth).with(Block::Hallmark),
        ] {
            cases.push(NewOptions {
                blocks,
                frontend: Some(frontend),
                ..NewOptions::defaults("shop")
            });
        }
    }
    for o in cases {
        let line = o.command_line();
        let argv: Vec<&str> = line.split(' ').skip(1).collect();
        let args = T::try_parse_from(argv)
            .unwrap_or_else(|e| unreachable!("{line}: {e}"))
            .args;
        assert_eq!(NewOptions::from_flags(&args), o, "{line}");
    }
}

/// The prompt's mapping (only reachable on a terminal): every subset of the entries maps to those blocks and back,
/// the answer line names them, and every entry starts selected.
#[test]
fn the_building_blocks_prompt_maps_every_subset() {
    let labels = Blocks::labels();
    assert_eq!(labels.len(), Block::ALL.len());
    // Watchfire and Temper start selected; Hallmark and Anvil do not (FEATURES.md).
    assert_eq!(Blocks::preselected(), [0, 1]);
    for mask in 0..(1u32 << Block::ALL.len()) {
        let picked: Vec<&str> = labels
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, l)| *l)
            .collect();
        let blocks = Blocks::from_labels(&picked);
        for (i, block) in Block::ALL.into_iter().enumerate() {
            assert_eq!(blocks.has(block), mask & (1 << i) != 0, "{picked:?}");
        }
        let back: Vec<&str> = blocks.iter().map(Block::label).collect();
        assert_eq!(back, picked);
        // The answer line names what the choice resolves to (Hallmark brings Temper).
        assert_eq!(
            Blocks::answer_for(&picked),
            blocks.with_required().0.answer()
        );
        let expected = if picked.is_empty() {
            "none".to_owned()
        } else {
            blocks
                .iter()
                .map(Block::name)
                .collect::<Vec<_>>()
                .join(", ")
        };
        assert_eq!(blocks.answer(), expected);
    }
    // A label that is not an entry selects nothing.
    assert_eq!(Blocks::from_labels(&["Watchfire"]), Blocks::NONE);
}

/// Every building block is one entry: a `--smelt` value of the same name, a prompt line that starts with its name,
/// on by default; the answer and the flag value list the chosen blocks in the prompt's order (D-502).
#[test]
fn building_blocks_are_one_entry_each() {
    let values: Vec<String> = SmeltArg::value_variants()
        .iter()
        .map(|v| value_name(*v))
        .collect();
    assert_eq!(values.first().map(String::as_str), Some("none"));
    let blocks: Vec<&str> = Block::ALL.iter().map(|b| b.value()).collect();
    assert_eq!(values.get(1..).unwrap_or_default(), blocks.as_slice());
    for (arg, block) in SmeltArg::value_variants().iter().skip(1).zip(Block::ALL) {
        assert_eq!(arg.block(), Some(block));
        // `Brand: description`, every block alike (D-503, D-593).
        let label = block.label();
        assert!(label.starts_with(&format!("{}: ", block.name())), "{label}");
        assert_eq!(block.value(), block.name().to_lowercase(), "{block:?}");
        assert_eq!(
            label.contains("two-factor"),
            block == Block::Auth,
            "{label}"
        );
        assert_eq!(
            Blocks::default().has(block),
            matches!(block, Block::Watchfire | Block::Auth),
            "{block:?}"
        );
        assert!(!Blocks::NONE.has(block));
    }
    assert_eq!(SmeltArg::None.block(), None);
    assert_eq!(Blocks::default().to_string(), "watchfire,temper");
    assert_eq!(Blocks::default().answer(), "Watchfire, Temper");
    assert_eq!(Blocks::NONE.to_string(), "none");
    assert_eq!(Blocks::NONE.answer(), "none");
    let auth_only = Blocks::NONE.with(Block::Auth);
    assert_eq!(auth_only.to_string(), "temper");
    assert_eq!(auth_only.answer(), "Temper");
    let search_only = Blocks::NONE.with(Block::Search);
    assert_eq!(search_only.to_string(), "prospect");
    assert_eq!(search_only.answer(), "Prospect");
    // The kinds: web first (the default), then headless.
    assert_eq!(Kind::ALL, [Kind::Web, Kind::Headless]);
    let kinds: Vec<String> = Kind::value_variants()
        .iter()
        .map(|k| value_name(*k))
        .collect();
    assert_eq!(kinds, ["web", "headless"]);
}

/// Review A M1: the Temper line names Temper and two-factor, and every kit's app gets them.
#[test]
fn the_authentication_line_says_what_each_kit_writes() {
    for kit in Frontend::ALL {
        let opts = NewOptions {
            frontend: Some(kit),
            ..NewOptions::defaults("my-app")
        };
        let (_tmp, root, _) = gen_app(&opts, None);
        let temper = root.join("app/providers/temper.rs").is_file();
        let two_factor = read(&root, "app/models/user.rs").contains("TwoFactorAuthenticatable");
        let label = Block::Auth.label();
        assert_eq!(label.starts_with("Temper: "), temper, "{kit:?}: {label}");
        assert_eq!(label.contains("two-factor"), two_factor, "{kit:?}: {label}");
    }
}

/// D-508: a block's requirements are chosen with it and said once each; a hint names a block a chosen one works
/// well with only while that block is not chosen. The rows here are a test table; the real table is checked for
/// self-references (and its rows in `the_real_block_table_requires_and_recommends`).
#[test]
fn the_block_table_adds_requirements_and_hints_at_recommendations() {
    let rows = |b: Block| match b {
        Block::Watchfire => BlockSpec {
            requires: &[Block::Auth],
            ..b.spec()
        },
        Block::Auth => BlockSpec {
            requires: &[],
            recommends: &[Recommendation {
                block: Block::Watchfire,
                gives: "jobs",
            }],
            ..b.spec()
        },
        Block::Hallmark | Block::Anvil | Block::Search => BlockSpec {
            requires: &[],
            recommends: &[],
            ..b.spec()
        },
    };
    let (blocks, notes) = Blocks::NONE.with(Block::Watchfire).with_required_by(rows);
    assert_eq!(blocks, Blocks::default());
    assert_eq!(notes, ["Watchfire needs Temper: added"]);
    let (same, notes) = Blocks::default().with_required_by(rows);
    assert_eq!(same, Blocks::default());
    assert!(notes.is_empty(), "nothing to add, nothing said");
    assert_eq!(
        Blocks::NONE.with(Block::Auth).hints_by(rows),
        ["Temper works with Watchfire: jobs"]
    );
    assert!(Blocks::default().hints_by(rows).is_empty());
    assert!(Blocks::NONE.hints_by(rows).is_empty());
    for block in Block::ALL {
        let row = block.spec();
        assert!(!row.requires.contains(&block), "{block:?} requires itself");
        assert!(row.recommends.iter().all(|r| r.block != block));
        assert!(
            row.requires
                .iter()
                .all(|r| row.recommends.iter().all(|c| c.block != *r))
        );
    }
    for blocks in [
        Blocks::NONE,
        Blocks::default(),
        Blocks::NONE.with(Block::Auth),
    ] {
        assert_eq!(blocks.with_required(), (blocks, Vec::new()));
        assert!(blocks.hints().is_empty());
    }
}

/// FEATURES.md "Installer building blocks" (D-467, D-512): Hallmark requires Temper (chosen with it and said),
/// Anvil recommends Temper, Hallmark and Watchfire (hints only, never chosen for the user).
#[test]
fn the_real_block_table_requires_and_recommends() {
    let (blocks, notes) = Blocks::NONE.with(Block::Hallmark).with_required();
    assert_eq!(blocks, Blocks::NONE.with(Block::Hallmark).with(Block::Auth));
    assert_eq!(notes, ["Hallmark needs Temper: added"]);
    let (blocks, notes) = Blocks::NONE.with(Block::Anvil).with_required();
    assert_eq!(
        blocks,
        Blocks::NONE.with(Block::Anvil),
        "recommendations are never chosen"
    );
    assert!(notes.is_empty());
    assert_eq!(
        blocks.hints(),
        [
            "Anvil works with Temper: private channels for signed-in users",
            "Anvil works with Hallmark: private channels for mobile apps and other clients",
            "Anvil works with Watchfire: broadcasting from agents and jobs",
        ]
    );
    // Only the missing partners are hinted at.
    let some = Blocks::default().with(Block::Anvil);
    assert_eq!(
        some.hints(),
        ["Anvil works with Hallmark: private channels for mobile apps and other clients"]
    );
    let all = some.with(Block::Hallmark);
    assert_eq!(all.with_required(), (all, Vec::new()));
    assert!(all.hints().is_empty());
    assert_eq!(all.to_string(), "watchfire,temper,hallmark,anvil");
    assert_eq!(all.answer(), "Watchfire, Temper, Hallmark, Anvil");
    let every = all.with(Block::Search);
    assert_eq!(
        every.to_string(),
        "watchfire,temper,hallmark,anvil,prospect"
    );
    assert_eq!(
        every.answer(),
        "Watchfire, Temper, Hallmark, Anvil, Prospect"
    );
}

/// `--smelt` resolves through the table: the notes say what was added (plain `  note: …`, styled `    › …`) and hint
/// at partners; the options, the summary and the one-line command hold the resolved set.
#[test]
fn smelt_flags_add_requirements_and_print_hints() {
    use clap::Parser;
    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        args: NewArgs,
    }
    let parse = |argv: &[&str]| {
        T::try_parse_from(argv)
            .map(|t| t.args)
            .unwrap_or_else(|e| unreachable!("{e}"))
    };
    let a = parse(&["new", "shop", "--smelt", "hallmark"]);
    let o = NewOptions::from_flags(&a);
    assert!(o.has_hallmark() && o.has_auth() && !o.has_agents() && !o.has_anvil());
    assert_eq!(o.blocks.to_string(), "temper,hallmark");
    assert!(o.command_line().contains(" --smelt temper,hallmark "));
    let (added, hints) = flag_notes(&a, &o);
    assert_eq!(added, ["Hallmark needs Temper: added"]);
    assert!(hints.is_empty());
    let plain = crate::ui::Ui::plain();
    assert_eq!(
        plain.note_line(&added[0]),
        "  note: Hallmark needs Temper: added"
    );

    let a = parse(&["new", "shop", "--smelt", "anvil"]);
    let o = NewOptions::from_flags(&a);
    assert!(o.has_anvil() && !o.has_auth() && !o.has_agents() && !o.has_hallmark());
    let (added, hints) = flag_notes(&a, &o);
    assert!(added.is_empty());
    assert_eq!(hints.len(), 3);

    let a = parse(&["new", "shop", "--smelt", "watchfire,temper,anvil,hallmark"]);
    let o = NewOptions::from_flags(&a);
    assert!(o.has_anvil() && o.has_hallmark() && o.has_auth() && o.has_agents());
    assert_eq!(flag_notes(&a, &o), (Vec::new(), Vec::new()));
    // Without `--smelt`: Watchfire and Temper only.
    let o = NewOptions::from_flags(&parse(&["new", "shop"]));
    assert!(!o.has_hallmark() && !o.has_anvil());
    // Headless apps take no block.
    let a = parse(&[
        "new",
        "ops",
        "--kind",
        "headless",
        "--smelt",
        "hallmark,anvil",
    ]);
    let o = NewOptions::from_flags(&a);
    assert!(!o.has_hallmark() && !o.has_anvil());
    assert_eq!(flag_notes(&a, &o), (Vec::new(), Vec::new()));
}

#[test]
fn flags_resolve_to_options() {
    use clap::Parser;
    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        args: NewArgs,
    }
    let parse = |argv: &[&str]| {
        T::try_parse_from(argv)
            .map(|t| t.args)
            .unwrap_or_else(|e| unreachable!("{e}"))
    };

    let a = parse(&["new", "shop"]);
    assert!(!a.any_question_flag());
    assert_eq!(NewOptions::from_flags(&a), NewOptions::defaults("shop"));

    let a = parse(&[
        "new",
        "ops",
        "--kind",
        "headless",
        "--db",
        "postgres",
        "--bellows",
        "skills,mcp",
        "--no-git",
        "--no-seed",
    ]);
    assert!(a.any_question_flag());
    let o = NewOptions::from_flags(&a);
    assert_eq!(o.kind, Kind::Headless);
    assert_eq!(o.frontend, None);
    assert_eq!(o.db, Db::Postgres);
    assert_eq!(
        o.bellows,
        Bellows {
            mcp: true,
            skills: true,
            guidelines: false
        }
    );
    assert!(o.migrate && !o.seed && !o.git);
    assert!(!o.tailwind, "headless apps never install Tailwind");

    let a = parse(&["new", "shop", "--no-tailwind"]);
    assert!(a.any_question_flag());
    let o = NewOptions::from_flags(&a);
    assert!(!o.tailwind && o.migrate);
    let a = parse(&["new", "shop", "--no-tailwind", "--tailwind"]);
    assert!(NewOptions::from_flags(&a).tailwind);

    let a = parse(&["new", "x", "--bellows", "all", "--no-migrate", "--migrate"]);
    let o = NewOptions::from_flags(&a);
    assert_eq!(o.bellows.to_string(), "all");
    assert!(o.migrate, "the last of --migrate/--no-migrate wins");

    // Alpine.js is off and every building block on unless asked otherwise.
    let o = NewOptions::from_flags(&parse(&["new", "shop", "--smelt", "watchfire"]));
    assert_eq!(
        o.blocks,
        Blocks {
            watchfire: true,
            ..Blocks::NONE
        }
    );
    assert!(o.has_agents() && !o.has_auth() && !o.alpine && o.tailwind);
    let a = parse(&["new", "shop", "--smelt", "none"]);
    assert!(a.any_question_flag());
    let o = NewOptions::from_flags(&a);
    assert_eq!(o.blocks, Blocks::NONE);
    assert!(!o.has_agents() && !o.has_auth());
    let o = NewOptions::from_flags(&parse(&["new", "shop", "--smelt", "temper,watchfire"]));
    assert_eq!(o.blocks, Blocks::default());
    assert_eq!(
        o.blocks.to_string(),
        "watchfire,temper",
        "the order of the prompt"
    );
    let o = NewOptions::from_flags(&parse(&["new", "shop", "--smelt", "none,temper"]));
    let p = NewOptions::from_flags(&parse(&["new", "shop", "--smelt", "prospect"]));
    assert!(p.has_search() && !p.has_auth() && !p.has_agents());
    assert!(o.has_auth() && !o.has_agents(), "`none` adds nothing");
    let a = parse(&["new", "shop", "--alpine"]);
    assert!(a.any_question_flag());
    let o = NewOptions::from_flags(&a);
    assert!(o.alpine && o.has_auth() && o.has_agents());
    assert!(!NewOptions::from_flags(&parse(&["new", "shop", "--alpine", "--no-alpine"])).alpine);
    // `web+agents` and `--auth` / `--no-auth` are gone (D-501).
    for old in [
        &["new", "shop", "--kind", "web+agents"][..],
        &["new", "shop", "--auth"],
        &["new", "shop", "--no-auth"],
        &["new", "shop", "--smelt", "sparks"],
        // The blocks are named after their crates, with no aliases (D-593).
        &["new", "shop", "--smelt", "auth"],
        &["new", "shop", "--smelt", "search"],
        &["new", "shop", "--smelt", "watchfire,auth"],
    ] {
        assert!(T::try_parse_from(old).is_err(), "{old:?}");
    }
    // Both only apply to web apps; a headless app always runs Watchfire and has no authentication pages.
    let o = NewOptions::from_flags(&parse(&[
        "new", "ops", "--kind", "headless", "--alpine", "--smelt", "temper",
    ]));
    assert!(!o.alpine && !o.has_auth() && o.has_agents());
    assert_eq!(o.blocks, Blocks::default());
    assert!(!o.command_line().contains("alpine") && !o.command_line().contains("--smelt"));
    // `smeltery new` itself refuses blocks for a headless app instead of dropping them silently (sweep W8-07);
    // `--smelt none` matches what a headless app is and passes.
    let err = check_flags(&parse(&[
        "new",
        "ops",
        "--kind",
        "headless",
        "--smelt",
        "temper,anvil",
    ]))
    .err()
    .map(|e| e.to_string())
    .unwrap_or_default();
    assert!(
        err.contains("headless apps take no building blocks"),
        "{err}"
    );
    for ok in [
        &["new", "ops", "--kind", "headless", "--smelt", "none"][..],
        &["new", "ops", "--kind", "headless"],
        &["new", "shop", "--smelt", "temper,anvil"],
    ] {
        assert!(check_flags(&parse(ok)).is_ok(), "{ok:?}");
    }

    // React / Vue: npm install by default, never Alpine.js; `--npm` means nothing for Mold.
    let o = NewOptions::from_flags(&parse(&["new", "shop", "--frontend", "react"]));
    assert_eq!(o.frontend, Some(Frontend::React));
    assert!(o.npm && o.tailwind && o.has_auth());
    let o = NewOptions::from_flags(&parse(&[
        "new",
        "shop",
        "--frontend",
        "vue",
        "--alpine",
        "--no-npm",
    ]));
    assert_eq!(o.frontend, Some(Frontend::Vue));
    assert!(!o.alpine && !o.npm);
    let a = parse(&["new", "shop", "--npm"]);
    assert!(a.any_question_flag());
    assert!(!NewOptions::from_flags(&a).npm, "Mold has no npm step");
    assert!(
        !NewOptions::from_flags(&parse(&[
            "new",
            "x",
            "--kind",
            "headless",
            "--frontend",
            "vue"
        ]))
        .npm
    );
}

/// D-507: apps with Watchfire ship the `pubsub_messages` migration, byte for byte what `pubsub:install` writes into
/// an app without it at the same second; `pubsub:install` then refuses in the app that has it.
#[test]
fn the_pubsub_migration_is_what_pubsub_install_writes() {
    let (_tmp, with, _) = gen_app(&opts(Shape::WebWatchfire, Db::Sqlite), None);
    let shipped = read(&with, PUBSUB);
    // A web app without Watchfire and without authentication: its newest migration is the cache tables' (`NOW + 4`;
    // Temper's two-factor columns are `NOW + 6`, after which `pubsub:install` would write).
    let (_tmp2, without, _) = gen_app(&web_opts(Shape::Web, false, false), None);
    assert!(!without.join(PUBSUB).exists());
    let ctx = crate::make::Ctx {
        root: &without,
        now: NOW + 5,
    };
    let plan = crate::make::pubsub::install(ctx).unwrap_or_else(|e| unreachable!("{e:#}"));
    let installed: Vec<(&str, &str)> = plan
        .files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    assert_eq!(installed, [(PUBSUB, shipped.as_str())]);
    let err = crate::make::pubsub::install(crate::make::Ctx {
        root: &with,
        now: NOW + 60,
    })
    .err()
    .map(|e| e.to_string())
    .unwrap_or_default();
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn database_scaffolding_matches_golden() {
    for kind in [Shape::WebWatchfire, Shape::Headless] {
        let (_tmp, root, _) = gen_app(&opts(kind, Db::Postgres), None);
        // Web apps with authentication add Temper's two-factor columns last (D-485).
        let (two_factor_mod, two_factor_add) = if kind.has_web() {
            (
                "pub mod m2026_10_03_120006_add_two_factor_columns_to_users_table;\n",
                "m.add(\n        \
                 m2026_10_03_120006_add_two_factor_columns_to_users_table::AddTwoFactorColumnsToUsersTable,\n    \
                 );\n    ",
            )
        } else {
            ("", "")
        };
        assert_eq!(
            read(&root, "database/migrations/mod.rs"),
            format!(
                "//! Database migrations, run in the order they are added below.\n\n\
                 pub mod m2026_10_03_120000_create_users_table;\n\
                 pub mod m2026_10_03_120001_create_password_reset_tokens_table;\n\
                 pub mod m2026_10_03_120002_create_sessions_table;\n\
                 pub mod m2026_10_03_120003_create_watchfire_tables;\n\
                 pub mod m2026_10_03_120004_create_cache_tables;\n\
                 pub mod m2026_10_03_120005_create_pubsub_messages_table;\n{two_factor_mod}// smeltery:mods\n\n\
                 use smeltery::db::migration::Migrator;\n\n/// Register every migration, oldest first.\n\
                 pub fn register(m: &mut Migrator) {{\n    \
                 m.add(m2026_10_03_120000_create_users_table::CreateUsersTable);\n    \
                 m.add(m2026_10_03_120001_create_password_reset_tokens_table::CreatePasswordResetTokensTable);\n    \
                 m.add(m2026_10_03_120002_create_sessions_table::CreateSessionsTable);\n    \
                 m.add(m2026_10_03_120003_create_watchfire_tables::CreateWatchfireTables);\n    \
                 m.add(m2026_10_03_120004_create_cache_tables::CreateCacheTables);\n    \
                 m.add(m2026_10_03_120005_create_pubsub_messages_table::CreatePubsubMessagesTable);\n    \
                 {two_factor_add}// smeltery:migrations\n}}\n"
            )
        );
        let migration = read(
            &root,
            "database/migrations/m2026_10_03_120000_create_users_table.rs",
        );
        assert!(migration.contains("\"2026_10_03_120000_create_users_table\""));
        assert!(migration.contains("t.string(\"email\").unique();"));
        assert_eq!(
            read(&root, "app/models/mod.rs"),
            "//! Database models, one module per table.\n\npub mod user;\n// smeltery:mods\n\n\
             pub use user::Model as User;\n// smeltery:models\n"
        );
        assert!(
            read(&root, "database/seeders/mod.rs")
                .contains("s.add(database_seeder::DatabaseSeeder);\n    // smeltery:seeders")
        );
        let seeder = read(&root, "database/seeders/database_seeder.rs");
        assert!(seeder.contains("Set(\"demo@example.com\".to_owned())"));
        // The demo user (a public password) is seeded only in local and testing, never with
        // `db:seed --force` on a production server (D-202).
        assert!(
            seeder.contains(
                "if matches!(env.as_str(), \"local\" | \"testing\") && User::count(db).await? == 0 {"
            ),
            "{seeder}"
        );
        assert!(
            read(&root, "database/factories/mod.rs")
                .contains("pub mod user_factory;\n// smeltery:mods")
        );
        assert!(read(&root, "app/commands/mod.rs").contains("pub fn register(c: &mut Commands) {"));
        assert!(read(&root, "app/commands/mod.rs").contains("// smeltery:commands"));
    }
}

/// Compares `rel` in the app with `tests/golden/new/<case>/<rel>`, or writes it with `SMELTERY_BLESS=1`.
fn golden(case: &str, root: &Path, rel: &str) {
    let actual = read(root, rel);
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/new")
        .join(case)
        .join(rel);
    if std::env::var_os("SMELTERY_BLESS").is_some() {
        if let Some(parent) = path.parent() {
            assert!(std::fs::create_dir_all(parent).is_ok());
        }
        assert!(std::fs::write(&path, &actual).is_ok());
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| unreachable!("golden {}: {e}", path.display()));
    assert_eq!(
        actual, expected,
        "{case}/{rel} differs from its golden file"
    );
}

#[test]
fn auth_scaffolding_matches_golden() {
    let (_tmp, root, _) = gen_app(&opts(Shape::WebWatchfire, Db::Sqlite), None);
    for rel in [
        "app/actions/mod.rs",
        "app/actions/temper/create_new_user.rs",
        "app/actions/temper/mod.rs",
        "app/actions/temper/password_rules.rs",
        "app/actions/temper/reset_user_password.rs",
        "app/actions/temper/update_user_password.rs",
        "app/actions/temper/update_user_profile_information.rs",
        "app/providers/mod.rs",
        "app/providers/temper.rs",
        "app/controllers/dashboard.rs",
        "app/controllers/settings.rs",
        "app/controllers/mod.rs",
        TWO_FACTOR,
        "app/middleware/mod.rs",
        "config/app.rs",
        "app/models/user.rs",
        "database/factories/user_factory.rs",
        "database/migrations/m2026_10_03_120000_create_users_table.rs",
        "database/migrations/m2026_10_03_120001_create_password_reset_tokens_table.rs",
        "database/migrations/m2026_10_03_120002_create_sessions_table.rs",
        "database/migrations/m2026_10_03_120004_create_cache_tables.rs",
        "database/migrations/m2026_10_03_120005_create_pubsub_messages_table.rs",
        "database/seeders/database_seeder.rs",
        "resources/views/auth/forgot-password.mold.html",
        "resources/views/auth/login.mold.html",
        "resources/views/auth/register.mold.html",
        "resources/views/auth/reset-password.mold.html",
        "resources/views/auth/verify-email.mold.html",
        "resources/views/auth/confirm-password.mold.html",
        "resources/views/auth/two-factor-challenge.mold.html",
        "resources/views/settings/nav.mold.html",
        "resources/views/settings/profile.mold.html",
        "resources/views/settings/password.mold.html",
        "resources/views/settings/two-factor.mold.html",
        "resources/views/dashboard.mold.html",
        "resources/views/layouts/app.mold.html",
        "routes/web.rs",
        "tests/http.rs",
        ".env.example",
        "app/agents/mod.rs",
        "app/sparks/mod.rs",
        "app/sparks/counter.rs",
        "resources/views/sparks/counter.mold.html",
        "resources/views/home.mold.html",
        "README.md",
        "CLAUDE.md",
    ] {
        golden("web", &root, rel);
    }
    let (_tmp, root, _) = gen_app(&opts(Shape::Headless, Db::Sqlite), None);
    for rel in [
        "tests/http.rs",
        "app/models/user.rs",
        "app/agents/mod.rs",
        "app/jobs/mod.rs",
        "database/migrations/m2026_10_03_120003_create_watchfire_tables.rs",
        "README.md",
        "CLAUDE.md",
        ".env.example",
    ] {
        golden("headless", &root, rel);
    }
    assert_eq!(
        read(&root, ".env").replace(KEY, ""),
        read(&root, ".env.example")
    );
    // Headless apps keep the users table and model but have no auth wiring.
    assert!(!read(&root, "bootstrap/app.rs").contains(".auth::"));
    assert!(!read(&root, "bootstrap/app.rs").contains(".temper("));
    assert!(!root.join(TWO_FACTOR).exists());
}

#[test]
fn bellows_files_match_golden() {
    let all = Bellows {
        mcp: true,
        skills: true,
        guidelines: true,
    };
    for (kind, case) in [
        (Shape::WebWatchfire, "bellows_web"),
        (Shape::Headless, "bellows_headless"),
    ] {
        let (_tmp, root, _) = gen_app(
            &NewOptions {
                bellows: all,
                ..opts(kind, Db::Sqlite)
            },
            None,
        );
        let mut files: Vec<String> = files_on_disk(&root)
            .into_iter()
            .filter(|f| f.starts_with(".bellows/") || f == ".mcp.json" || f == "CLAUDE.md")
            .collect();
        files.sort();
        for rel in &files {
            golden(case, &root, rel);
        }
        assert_eq!(read(&root, "CLAUDE.md"), read(&root, "AGENTS.md"));
        assert!(read(&root, "bootstrap/app.rs").contains(".bellows()"));
    }
}

/// React and Vue apps get guides for their kit: Alloy pages, `useForm`, partial reloads and deferred props, the
/// `alloy-page` skill instead of `spark` (Sparks appear only as the Watchfire dashboard's internals).
#[test]
fn kit_guides_match_golden() {
    let all = Bellows {
        mcp: true,
        skills: true,
        guidelines: true,
    };
    for (frontend, kind, auth, case) in [
        (Frontend::React, Shape::WebWatchfire, true, "bellows_react"),
        (Frontend::Vue, Shape::Web, false, "bellows_vue_no_auth"),
    ] {
        let (_tmp, root, _) = gen_app(
            &NewOptions {
                bellows: all,
                ..kit_opts(frontend, kind, auth, true)
            },
            None,
        );
        let mut files: Vec<String> = files_on_disk(&root)
            .into_iter()
            .filter(|f| f.starts_with(".bellows/") || f == "CLAUDE.md")
            .collect();
        files.sort();
        assert!(
            files.contains(&".bellows/skills/alloy-page.md".to_owned()),
            "{files:?}"
        );
        assert!(
            !files.contains(&".bellows/skills/spark.md".to_owned()),
            "{files:?}"
        );
        for rel in &files {
            golden(case, &root, rel);
            let text = read(&root, rel);
            for mold_only in [
                "make:spark",
                "@csrf",
                "resources/views/auth",
                "@spark(",
                "wire:",
                "smeltery:sparks",
                "tailwind:install",
            ] {
                assert!(!text.contains(mold_only), "{case} {rel}: {mold_only}");
            }
        }
        assert_eq!(read(&root, "CLAUDE.md"), read(&root, "AGENTS.md"));
        let claude = read(&root, "CLAUDE.md");
        for kit_fact in [
            "## Pages (",
            "useForm",
            "router.reload({ only: ['key'] })",
            "<Deferred data=\"key\">",
            "make:page Name",
            "Every prop is public",
        ] {
            assert!(claude.contains(kit_fact), "{case}: {kit_fact}");
        }
    }
}

#[test]
fn relative_path_walks_up_and_down() {
    let rel = |a: &str, b: &str| {
        relative_path(Path::new(a), Path::new(b)).map(|p| p.to_string_lossy().into_owned())
    };
    assert_eq!(
        rel("/repo/examples/web-demo", "/repo/crates/smeltery").as_deref(),
        Some("../../crates/smeltery")
    );
    assert_eq!(
        rel("/repo/examples/./demo/../web", "/repo/x/../crates/smeltery").as_deref(),
        Some("../../crates/smeltery")
    );
    assert_eq!(
        rel("/repo", "/repo/crates/smeltery").as_deref(),
        Some("crates/smeltery")
    );
    assert_eq!(rel("/repo/a", "/repo/a").as_deref(), Some("."));
}

#[test]
fn relative_smeltery_path_stays_relative_to_the_app() {
    use clap::Parser;
    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        args: NewArgs,
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("crates/smeltery")).unwrap();
    std::fs::write(repo.join("crates/smeltery/Cargo.toml"), "[package]\n").unwrap();
    let args = T::try_parse_from([
        "new",
        "web-demo",
        "--path",
        "examples",
        "--no-migrate",
        "--no-seed",
        "--no-git",
        // Tests never download Tailwind.
        "--no-tailwind",
        "--smeltery-path",
        ".",
    ])
    .unwrap()
    .args;
    run(args, &repo, crate::ui::Ui::plain()).unwrap();
    let cargo = std::fs::read_to_string(repo.join("examples/web-demo/Cargo.toml")).unwrap();
    assert!(
        cargo.contains("smeltery = { path = \"../../crates/smeltery\", default-features = false"),
        "{cargo}"
    );

    // An absolute --smeltery-path stays absolute.
    let args = T::try_parse_from([
        "new",
        "other",
        "--path",
        "examples",
        "--no-migrate",
        "--no-seed",
        "--no-git",
        // Tests never download Tailwind.
        "--no-tailwind",
        "--smeltery-path",
        repo.to_str().unwrap(),
    ])
    .unwrap()
    .args;
    run(args, &repo, crate::ui::Ui::plain()).unwrap();
    let cargo = std::fs::read_to_string(repo.join("examples/other/Cargo.toml")).unwrap();
    let abs = std::path::absolute(repo.join("crates/smeltery")).unwrap();
    assert!(
        cargo.contains(&format!(
            "path = \"{}\"",
            abs.display().to_string().replace('\\', "\\\\")
        )),
        "{cargo}"
    );
}

#[test]
fn plain_summary_matches_the_output_before_styling() {
    let mut o = opts(Shape::WebWatchfire, Db::Sqlite);
    o.name = "blog".to_owned();
    let text = plain_summary(
        &o,
        Path::new("/apps/blog"),
        "blog",
        &steps("installed", "", "done", "done", "initialized"),
    );
    let expected = format!(
        "Created blog in /apps/blog\n  kind      web\n  database  sqlite\n  frontend  Mold + Sparks\n  \
         tailwind  installed\n  alpine    no\n  smelt     watchfire,temper\n  bellows   {bellows}\n  migrate   done\n  seed      done\n  git       initialized\n\n\
         Same app without questions:\n  {command}\n\nNext steps:\n  cd blog && smeltery serve\n",
        bellows = o.bellows,
        command = o.command_line(),
    );
    assert_eq!(text, expected);
    assert!(!text.contains('\u{1b}'));
    // A failed download adds the retry command to the next steps.
    let failed = plain_summary(
        &o,
        Path::new("/apps/blog"),
        "blog",
        &steps("failed", "", "done", "done", "initialized"),
    );
    assert!(failed.contains("  tailwind  failed\n"), "{failed}");
    assert!(
        failed.ends_with("  cd blog && smeltery serve\n  smeltery tailwind:install   (retries the Tailwind CSS download)\n"),
        "{failed}"
    );
    // Headless apps have no Tailwind row.
    let headless = plain_summary(
        &opts(Shape::Headless, Db::Sqlite),
        Path::new("/x"),
        "x",
        &steps("no", "", "done", "done", "no"),
    );
    assert!(!headless.contains("tailwind"), "{headless}");
    assert!(
        !headless.contains("alpine")
            && !headless.contains("  smelt ")
            && !headless.contains("temper"),
        "{headless}"
    );
}

#[test]
fn created_lines_group_folders() {
    let written: Vec<String> = [
        "Cargo.toml",
        "app/mod.rs",
        "app/models/user.rs",
        "routes/web.rs",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(
        created_lines(&written),
        ["Cargo.toml", "app/ · 2 files", "routes/ · 1 file"]
    );
}

#[test]
fn styled_summary_shows_every_choice_and_the_command() {
    let o = opts(Shape::Headless, Db::Postgres);
    let text = crate::ui::strip_ansi(&styled_summary(
        &o,
        crate::ui::Ui::styled_for_test(),
        "my-app",
        &steps("no", "", "no", "no", "no"),
    ));
    for needle in [
        "headless",
        "postgres",
        "Next steps:",
        "cd my-app && smeltery serve",
    ] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
    assert!(!text.contains("frontend"));
    let web = crate::ui::strip_ansi(&styled_summary(
        &web_opts(Shape::Web, false, true),
        crate::ui::Ui::styled_for_test(),
        "my-app",
        &steps("no", "", "no", "no", "no"),
    ));
    // The rows follow the questions: tailwind, alpine, the building blocks, then bellows.
    let at = |needle: &str| web.find(needle).unwrap_or(usize::MAX);
    assert!(at("tailwind") < at("alpine    yes"), "{web}");
    assert!(at("alpine    yes") < at("smelt     none"), "{web}");
    assert!(at("smelt     none") < at("bellows"), "{web}");
}

// --- The React and Vue kits (D-283, D-286) ---

/// A web app with a JavaScript kit.
fn kit_opts(frontend: Frontend, kind: Shape, auth: bool, tailwind: bool) -> NewOptions {
    NewOptions {
        kind: kind.kind(),
        frontend: Some(frontend),
        blocks: kind.blocks(auth),
        tailwind,
        npm: true,
        ..NewOptions::defaults("my-app")
    }
}

/// Every `.rs` file under `root`.
fn rust_files(root: &Path) -> Vec<PathBuf> {
    files_on_disk(root)
        .into_iter()
        .filter(|f| f.ends_with(".rs"))
        .map(|f| root.join(f))
        .collect()
}

/// The kits' Rust code is already formatted (D-103): `cargo fmt` in a fresh app changes nothing. Needs `rustfmt`.
#[test]
fn kit_code_is_rustfmt_clean() {
    for frontend in [Frontend::React, Frontend::Vue] {
        for kind in [Shape::WebWatchfire, Shape::Web] {
            for auth in [true, false] {
                let (_tmp, root, _) = gen_app(&kit_opts(frontend, kind, auth, true), None);
                let out = std::process::Command::new("rustfmt")
                    .args(["--edition", "2024", "--check"])
                    .args(rust_files(&root))
                    .output()
                    .unwrap_or_else(|e| unreachable!("rustfmt must be installed: {e}"));
                assert!(
                    out.status.success(),
                    "{frontend:?} {kind:?} auth {auth}: not rustfmt-clean:\n{}",
                    String::from_utf8_lossy(&out.stdout)
                );
            }
        }
    }
}

/// The files a kit app has, beyond the ones every app has.
#[test]
fn kits_write_their_files_and_no_mold_pages() {
    for frontend in [Frontend::React, Frontend::Vue] {
        for (kind, auth, tailwind) in [
            (Shape::WebWatchfire, true, true),
            (Shape::Web, false, false),
            (Shape::Web, true, false),
            (Shape::WebWatchfire, false, true),
        ] {
            let o = kit_opts(frontend, kind, auth, tailwind);
            let (_tmp, root, written) = gen_app(&o, None);
            let on_disk = files_on_disk(&root);
            assert_eq!(written.len(), on_disk.len());
            let case = format!("{frontend:?} {kind:?} auth {auth} tailwind {tailwind}");
            // Mold pages, Sparks and the Mold kit's stylesheet are not written.
            for file in &on_disk {
                assert!(
                    !file.ends_with(".mold.html") || file == "resources/views/app.mold.html",
                    "{case}: {file}"
                );
                assert!(!file.starts_with("app/sparks/"), "{case}: {file}");
                assert!(
                    !file.contains("{{") && !file.contains("{%"),
                    "{case}: {file}"
                );
            }
            assert!(!on_disk.contains("public/assets/css/app.css"), "{case}");
            let (pages, ext, entry): (&[&str], &str, &str) = match frontend {
                Frontend::React => (
                    &[
                        "welcome",
                        "dashboard",
                        "auth/login",
                        "auth/register",
                        "auth/forgot-password",
                        "auth/reset-password",
                        "auth/verify-email",
                        "auth/confirm-password",
                        "auth/two-factor-challenge",
                        "settings/profile",
                        "settings/password",
                        "settings/two-factor",
                    ],
                    "tsx",
                    "resources/js/app.tsx",
                ),
                _ => (
                    &[
                        "Welcome",
                        "Dashboard",
                        "auth/Login",
                        "auth/Register",
                        "auth/ForgotPassword",
                        "auth/ResetPassword",
                        "auth/VerifyEmail",
                        "auth/ConfirmPassword",
                        "auth/TwoFactorChallenge",
                        "settings/Profile",
                        "settings/Password",
                        "settings/TwoFactor",
                    ],
                    "vue",
                    "resources/js/app.ts",
                ),
            };
            // With Temper (D-482) its provider renders the authentication pages and `settings.rs` the settings pages;
            // without it the app's own controllers render them.
            let temper = o.has_temper();
            let auth_page = |old: &'static str| {
                if temper {
                    "app/providers/temper.rs"
                } else {
                    old
                }
            };
            for (i, page) in pages.iter().enumerate() {
                let file = format!("resources/js/pages/{page}.{ext}");
                // The welcome page always, the others with authentication, the last five with Temper.
                let wanted = i == 0 || (auth && i < 7) || temper;
                assert_eq!(on_disk.contains(&file), wanted, "{case}: {file}");
                if on_disk.contains(&file) {
                    let controllers = [
                        "app/controllers/home.rs",
                        "app/controllers/dashboard.rs",
                        auth_page("app/controllers/auth/login.rs"),
                        auth_page("app/controllers/auth/register.rs"),
                        auth_page("app/controllers/auth/password.rs"),
                        auth_page("app/controllers/auth/password.rs"),
                        auth_page("app/controllers/auth/verify_email.rs"),
                        "app/providers/temper.rs",
                        "app/providers/temper.rs",
                        "app/controllers/settings.rs",
                        "app/controllers/settings.rs",
                        "app/controllers/settings.rs",
                    ];
                    let rust = read(&root, controllers.get(i).copied().unwrap_or_default());
                    assert!(
                        rust.contains(&format!("alloy::render(\"{page}\")")),
                        "{case}: {page}"
                    );
                }
            }
            for file in [
                "package.json",
                "tsconfig.json",
                "vite.config.ts",
                entry,
                "resources/css/app.css",
                "resources/views/app.mold.html",
                "app/providers/alloy.rs",
                "resources/js/types/global.d.ts",
            ] {
                assert!(on_disk.contains(file), "{case}: {file}");
            }
            assert_eq!(
                on_disk.contains("app/controllers/auth/mod.rs"),
                auth && !temper,
                "{case}"
            );
            assert_eq!(
                on_disk.contains("app/providers/temper.rs"),
                temper,
                "{case}"
            );
            assert_eq!(temper, auth, "{case}");
            let package = read(&root, "package.json");
            assert_eq!(package.contains("\"tailwindcss\""), tailwind, "{case}");
            assert_eq!(
                read(&root, "vite.config.ts").contains("tailwindcss()"),
                tailwind,
                "{case}"
            );
            let css = read(&root, "resources/css/app.css");
            if tailwind {
                assert!(
                    css.contains("@import \"tailwindcss\" source(none);\n@source \"../js\";"),
                    "{case}"
                );
            } else {
                // The prebuilt stylesheet (D-287), byte for byte.
                assert!(css.starts_with("/*! tailwindcss v4.3.3 "), "{case}");
            }
            let bootstrap = read(&root, "bootstrap/app.rs");
            assert!(
                bootstrap.contains(&format!(".entries([\"{entry}\"])")),
                "{case}"
            );
            assert_eq!(
                bootstrap.contains(".sparks(|_| {})"),
                kind.has_agents(),
                "{case}"
            );
            assert!(!bootstrap.contains("app::sparks"), "{case}");
            assert_eq!(bootstrap.contains(".encrypt_history()"), auth, "{case}");
            assert!(!read(&root, "app/mod.rs").contains("sparks"), "{case}");
            let gitignore = read(&root, ".gitignore");
            assert!(
                gitignore.ends_with("/public/storage\n/node_modules\n/public/build\n"),
                "{case}: {gitignore}"
            );
            let example = read(&root, ".env.example");
            assert!(example.contains("\nVITE_APP_NAME=\"My App\"\n"), "{case}");
            let cargo = read(&root, "Cargo.toml");
            let value = if frontend == Frontend::React {
                "react"
            } else {
                "vue"
            };
            assert!(
                cargo.contains(&format!(
                    "[package.metadata.smeltery]\nfrontend = \"{value}\"\n"
                )),
                "{case}"
            );
        }
    }
    // Mold apps have none of it.
    let (_tmp, root, _) = gen_app(&NewOptions::defaults("my-app"), None);
    assert!(!root.join("package.json").exists());
    assert!(!read(&root, ".gitignore").contains("node_modules"));
    assert!(!read(&root, ".env").contains("VITE_"));
    assert!(!read(&root, "Cargo.toml").contains("metadata"));
}

/// The kits' `package.json` pins exactly the versions of `frontend::NPM_VERSIONS`, without ranges, and every pinned
/// package is used by a kit.
#[test]
fn kit_package_versions_match_the_table() {
    let mut used = BTreeSet::new();
    for frontend in [Frontend::React, Frontend::Vue] {
        for tailwind in [true, false] {
            // With Anvil, so the Echo packages are checked too.
            let opts = NewOptions {
                blocks: Blocks::default().with(Block::Anvil),
                ..kit_opts(frontend, Shape::Web, true, tailwind)
            };
            let (_tmp, root, _) = gen_app(&opts, None);
            let package: serde_json::Value = serde_json::from_str(&read(&root, "package.json"))
                .unwrap_or_else(|e| unreachable!("package.json: {e}"));
            assert_eq!(package["name"], "my-app");
            for section in ["dependencies", "devDependencies"] {
                let deps = package[section].as_object().cloned().unwrap_or_default();
                for (name, version) in deps {
                    let version = version.as_str().unwrap_or_default().to_owned();
                    assert_eq!(
                        crate::frontend::pinned(&name),
                        Some(version.as_str()),
                        "{frontend:?}: {name} {version} is not the pinned version"
                    );
                    used.insert(name);
                }
            }
        }
    }
    let table: BTreeSet<String> = crate::frontend::NPM_VERSIONS
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    assert_eq!(used, table, "every pinned package is in a kit");
    assert_eq!(crate::frontend::pinned("typescript"), Some("6.0.3"));
}

#[test]
fn kit_files_match_golden() {
    let full = |root: &Path| -> Vec<String> {
        files_on_disk(root)
            .into_iter()
            .filter(|f| {
                (f.starts_with("resources/") && f != "resources/css/app.css")
                    || f.starts_with("app/controllers/")
                    || f.starts_with("app/providers/")
                    || [
                        "app/mod.rs",
                        "bootstrap/app.rs",
                        "tests/http.rs",
                        "package.json",
                        "tsconfig.json",
                        "vite.config.ts",
                        ".env.example",
                        ".gitignore",
                        "Cargo.toml",
                        "README.md",
                    ]
                    .contains(&f.as_str())
            })
            .collect()
    };
    let lean = [
        "package.json",
        "vite.config.ts",
        "bootstrap/app.rs",
        "app/providers/alloy.rs",
        "app/controllers/mod.rs",
        "tests/http.rs",
        "README.md",
    ];
    for (frontend, name, layout) in [
        (
            Frontend::React,
            "react",
            "resources/js/layouts/app-layout.tsx",
        ),
        (Frontend::Vue, "vue", "resources/js/layouts/AppLayout.vue"),
    ] {
        // Watchfire, Tailwind and authentication: every file.
        let (_tmp, root, _) = gen_app(&kit_opts(frontend, Shape::WebWatchfire, true, true), None);
        for rel in full(&root) {
            golden(name, &root, &rel);
        }
        // web without Tailwind or authentication.
        let (_tmp, root, _) = gen_app(&kit_opts(frontend, Shape::Web, false, false), None);
        for rel in lean
            .iter()
            .copied()
            .chain([layout, "resources/js/types/global.d.ts"])
        {
            golden(&format!("{name}_web_no_auth"), &root, rel);
        }
        let welcome = if frontend == Frontend::React {
            "resources/js/pages/welcome.tsx"
        } else {
            "resources/js/pages/Welcome.vue"
        };
        golden(&format!("{name}_web_no_auth"), &root, welcome);
    }
}

#[test]
fn kit_summaries_show_npm_and_no_alpine() {
    let o = kit_opts(Frontend::Vue, Shape::WebWatchfire, true, false);
    let text = plain_summary(
        &o,
        Path::new("/apps/my-app"),
        "my-app",
        &steps("no", "installed", "done", "done", "initialized"),
    );
    assert!(
        text.contains("  frontend  Vue (Inertia, TypeScript)\n  tailwind  no\n  smelt     watchfire,temper\n  bellows   none\n  npm       installed\n  migrate   done\n"),
        "{text}"
    );
    assert!(!text.contains("alpine"), "{text}");
    assert!(
        text.ends_with("Next steps:\n  cd my-app && smeltery serve\n"),
        "{text}"
    );
    // Without Node.js the step is skipped (never an error) and the next steps start with `npm install`.
    let dir = tempfile::tempdir().unwrap_or_else(|e| unreachable!("{e}"));
    let missing = crate::cmd::Tool::new(dir.path().join("no-such-npm"));
    let skipped = npm_step(
        dir.path(),
        &NodeCheck::Missing,
        &missing,
        crate::ui::Ui::plain(),
    );
    assert_eq!(skipped, "skipped (Node 20.19+ or 22.12+ not found)");
    let text = plain_summary(
        &o,
        Path::new("/apps/my-app"),
        "my-app",
        &steps("no", &skipped, "done", "done", "initialized"),
    );
    assert!(
        text.contains("  npm       skipped (Node 20.19+ or 22.12+ not found)\n"),
        "{text}"
    );
    assert!(
        text.ends_with("Next steps:\n  cd my-app && npm install && smeltery serve\n"),
        "{text}"
    );
    // A failed install too.
    let failed = npm_step(
        dir.path(),
        &NodeCheck::Ready {
            node: "v22.14.0".to_owned(),
        },
        &missing,
        crate::ui::Ui::plain(),
    );
    assert_eq!(failed, "failed");
}

/// The personal access tokens migration (Hallmark, `NOW + 7`).
const HALLMARK_MIGRATION: &str =
    "database/migrations/m2026_10_03_120007_create_personal_access_tokens_table.rs";

/// Hallmark's files (D-467).
const HALLMARK: &[&str] = &[
    HALLMARK_MIGRATION,
    "app/controllers/api/mod.rs",
    "app/controllers/api/tokens.rs",
    "app/controllers/api/user.rs",
    "tests/api_tokens.rs",
];

/// Anvil's files (D-512).
const ANVIL: &[&str] = &[
    "routes/channels.rs",
    "app/events/mod.rs",
    "app/events/announcement_posted.rs",
    "tests/broadcasting.rs",
];

fn block_opts(frontend: Frontend, blocks: Blocks) -> NewOptions {
    NewOptions {
        blocks,
        frontend: Some(frontend),
        tailwind: false,
        ..NewOptions::defaults("my-app")
    }
}

/// Each block writes its files and wiring in every kit, and nothing of it without the block.
#[test]
fn hallmark_and_anvil_choose_the_files() {
    let everything = Blocks::default().with(Block::Hallmark).with(Block::Anvil);
    for kit in Frontend::ALL {
        // Every block: both sets, the prune scheduled, the PubSub migration (Watchfire and Anvil both want it).
        let (_tmp, root, written) = gen_app(&block_opts(kit, everything), None);
        for rel in HALLMARK.iter().chain(ANVIL) {
            assert!(written.iter().any(|w| w == rel), "{kit:?}: {rel}");
        }
        let bootstrap = read(&root, "bootstrap/app.rs");
        assert!(bootstrap.contains("use smeltery::hallmark::{Hallmark, HallmarkExt as _};"));
        assert!(bootstrap.contains("        .hallmark(Hallmark::new())\n"));
        assert!(bootstrap.contains("use smeltery::anvil::AnvilExt as _;"));
        assert!(bootstrap.contains("        .anvil(routes::channels::channels)\n"));
        let api = read(&root, "routes/api.rs");
        assert!(api.contains(".middleware(\"throttle:10,1\")"));
        assert_eq!(api.matches(".middleware(\"auth:hallmark\")").count(), 2);
        assert!(read(&root, "app/agents/mod.rs").contains(".call(\"hallmark-prune\""));
        assert!(!read(&root, "app/agents/mod.rs").contains("let _ = &w;"));
        assert!(root.join(PUBSUB).is_file());
        let channels = read(&root, "routes/channels.rs");
        assert!(channels.contains("c.private(\"users.{user}\""));
        assert!(channels.contains("Ok(ctx.user_id() == Some(user))"));
        assert!(read(&root, "app/controllers/mod.rs").contains("pub mod api;\n"));
        assert!(read(&root, "app/mod.rs").contains("pub mod events;\n"));
        let migrations = read(&root, "database/migrations/mod.rs");
        assert!(migrations.contains(
            "m2026_10_03_120007_create_personal_access_tokens_table::CreatePersonalAccessTokensTable"
        ));
        for env in [".env", ".env.example"] {
            let text = read(&root, env);
            for line in [
                "# HALLMARK_TOKEN_EXPIRATION=365",
                "# HALLMARK_SPA=false",
                "# HALLMARK_STATEFUL=",
                "# ANVIL_APP_KEY=",
                "# ANVIL_ALLOWED_ORIGINS=",
                // The separate socket process (README "A separate socket process").
                "# ANVIL_IN_SERVE=true",
                "# ANVIL_SERVER_HOST=127.0.0.1",
                "# ANVIL_SERVER_PORT=8080",
                "the README's \"A separate socket process\"",
            ] {
                assert!(text.contains(line), "{kit:?} {env}: {line}");
            }
            // No secret and no active value: every ANVIL_ / HALLMARK_ line is a comment.
            assert!(
                text.lines()
                    .filter(|l| l.contains("ANVIL_") || l.contains("HALLMARK_"))
                    .all(|l| l.starts_with('#')),
                "{kit:?} {env}"
            );
            assert!(!text.contains("ANVIL_APP_SECRET="));
        }
        let guide = read(&root, "CLAUDE.md");
        assert!(
            guide.contains("## API tokens (Hallmark)") && guide.contains("## Broadcasting (Anvil)")
        );
        // Stage F (D-431, D-433): the Echo client in the kits, listening Sparks in Mold, the presence tables.
        assert_eq!(guide.contains("resources/js/echo.ts"), kit.is_js());
        assert_eq!(guide.contains("app/sparks/announcements.rs"), !kit.is_js());
        assert_eq!(root.join("resources/js/echo.ts").is_file(), kit.is_js());
        assert_eq!(
            root.join("app/sparks/notifications.rs").is_file(),
            !kit.is_js()
        );
        assert_eq!(
            root.join("database/migrations/m2026_10_03_120008_create_presence_tables.rs")
                .is_file(),
            kit.is_js()
        );
        if kit.is_js() {
            let package = read(&root, "package.json");
            for dep in ["\"laravel-echo\": \"2.5.0\"", "\"pusher-js\": \"8.6.0\""] {
                assert!(package.contains(dep), "{kit:?}: {package}");
            }
            assert!(read(&root, "app/providers/alloy.rs").contains("\"anvil_key\": anvil_key"));
            assert!(read(&root, "routes/web.rs").contains("r.post(\"/notify-me\""));
            let channels = read(&root, "routes/channels.rs");
            assert!(channels.contains("c.presence(\"dashboard\""));
            // D-434: the presence channel shares member ids only; the name line is a comment.
            assert!(channels.contains("let info = smeltery::json!({ \"id\": user.id });"));
            assert!(channels.contains(
                "// `let info = smeltery::json!({ \"id\": user.id, \"name\": user.name });`"
            ));
        }
        for env in [".env", ".env.example"] {
            let text = read(&root, env);
            assert!(
                text.contains(
                    "# CORS_ALLOWED_ORIGINS=
# CORS_PATHS=/api/
"
                ),
                "{env}"
            );
        }

        // Hallmark without Watchfire: no agents, so no prune.
        let (_tmp, root, _) = gen_app(
            &block_opts(kit, Blocks::NONE.with(Block::Auth).with(Block::Hallmark)),
            None,
        );
        for rel in HALLMARK {
            assert!(root.join(rel).is_file(), "{kit:?}: {rel}");
        }
        assert!(!root.join("app/agents/mod.rs").exists());
        assert!(!root.join(PUBSUB).exists());
        assert!(!root.join("routes/channels.rs").exists());

        // Anvil alone: public channels only, the PubSub migration without Watchfire, no auth.
        let (_tmp, root, _) = gen_app(&block_opts(kit, Blocks::NONE.with(Block::Anvil)), None);
        for rel in ANVIL {
            assert!(root.join(rel).is_file(), "{kit:?}: {rel}");
        }
        assert!(root.join(PUBSUB).is_file());
        assert!(!root.join("app/agents/mod.rs").exists());
        assert!(!read(&root, "routes/channels.rs").contains("c.private("));
        assert!(!read(&root, "tests/broadcasting.rs").contains("authorize"));
        assert!(!root.join(HALLMARK_MIGRATION).exists());
        assert!(!read(&root, "bootstrap/app.rs").contains(".temper("));
        assert!(read(&root, "CLAUDE.md").contains("it answers 404 until the"));

        // The default app: none of it.
        let (_tmp, root, written) = gen_app(&block_opts(kit, Blocks::default()), None);
        for rel in HALLMARK.iter().chain(ANVIL) {
            assert!(!written.iter().any(|w| w == rel), "{kit:?}: {rel}");
        }
        let bootstrap = read(&root, "bootstrap/app.rs");
        assert!(!bootstrap.contains("hallmark") && !bootstrap.contains("anvil"));
        let env = read(&root, ".env");
        assert!(!env.contains("HALLMARK_") && !env.contains("ANVIL_"));
    }
}

/// The Hallmark and Anvil files, byte for byte (Mold kit, every block; Anvil alone; the React kit's bootstrap).
#[test]
fn hallmark_and_anvil_match_golden() {
    let everything = Blocks::default().with(Block::Hallmark).with(Block::Anvil);
    let opts = NewOptions {
        bellows: Bellows {
            mcp: true,
            skills: true,
            guidelines: true,
        },
        ..block_opts(Frontend::Mold, everything)
    };
    let (_tmp, root, _) = gen_app(&opts, None);
    for rel in HALLMARK.iter().chain(ANVIL).copied().chain([
        "bootstrap/app.rs",
        "routes/api.rs",
        "routes/mod.rs",
        "app/agents/mod.rs",
        "app/controllers/mod.rs",
        "app/mod.rs",
        "database/migrations/mod.rs",
        ".env.example",
        "README.md",
        "CLAUDE.md",
        ".bellows/guidelines.md",
        ".bellows/skills/api-token.md",
        ".bellows/skills/broadcast.md",
        // The "Notify me" Spark: its ping is rate-limited (sweep W8-02).
        "app/sparks/notifications.rs",
    ]) {
        golden("web_blocks", &root, rel);
    }
    let opts = NewOptions {
        bellows: Bellows {
            mcp: false,
            skills: true,
            guidelines: true,
        },
        ..block_opts(Frontend::Mold, Blocks::NONE.with(Block::Anvil))
    };
    let (_tmp, root, _) = gen_app(&opts, None);
    for rel in ANVIL.iter().copied().chain([
        "bootstrap/app.rs",
        "database/migrations/mod.rs",
        ".env.example",
        "CLAUDE.md",
        ".bellows/skills/broadcast.md",
    ]) {
        golden("web_anvil", &root, rel);
    }
    let (_tmp, root, _) = gen_app(&block_opts(Frontend::React, everything), None);
    for rel in ["bootstrap/app.rs", "CLAUDE.md"] {
        golden("react_blocks", &root, rel);
    }
}

/// `smeltery hallmark:install` in an app made without Hallmark writes what `smeltery new --smelt hallmark` writes:
/// after it (and the printed `bootstrap/app.rs` lines), the two apps' code is the same file for file.
#[test]
fn hallmark_install_writes_what_new_writes() {
    for kit in Frontend::ALL {
        let with = block_opts(kit, Blocks::default().with(Block::Hallmark));
        let (_tmp, made, _) = gen_app(&with, None);
        let (_tmp2, installed, _) = gen_app(&block_opts(kit, Blocks::default()), None);
        let ctx = crate::make::Ctx {
            root: &installed,
            now: NOW + 7,
        };
        let plan = crate::make::hallmark::install(ctx).unwrap_or_else(|e| unreachable!("{e:#}"));
        assert!(
            plan.notes
                .iter()
                .any(|n| n.contains(".hallmark(Hallmark::new())"))
        );
        crate::make::apply(&installed, &plan).unwrap_or_else(|e| unreachable!("{e:#}"));
        for rel in HALLMARK.iter().copied().chain([
            "routes/api.rs",
            "app/agents/mod.rs",
            "app/controllers/mod.rs",
            "database/migrations/mod.rs",
        ]) {
            assert_eq!(read(&installed, rel), read(&made, rel), "{kit:?}: {rel}");
        }
        // Run again: refused, nothing changed.
        let err = crate::make::hallmark::install(ctx)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("already exists"), "{err}");
    }
    // Without authentication: refused.
    let (_tmp, bare, _) = gen_app(&block_opts(Frontend::Mold, Blocks::NONE), None);
    let err = crate::make::hallmark::install(crate::make::Ctx {
        root: &bare,
        now: NOW + 7,
    })
    .err()
    .map(|e| e.to_string())
    .unwrap_or_default();
    assert!(err.contains("needs user accounts"), "{err}");
    assert!(!bare.join("tests/api_tokens.rs").exists());
}

/// The Hallmark and Anvil code is already formatted (D-103) in every kit and block mix. Needs `rustfmt`.
#[test]
fn block_code_is_rustfmt_clean() {
    for kit in Frontend::ALL {
        for blocks in [
            Blocks::default().with(Block::Hallmark).with(Block::Anvil),
            Blocks::NONE.with(Block::Anvil),
            Blocks::NONE.with(Block::Auth).with(Block::Hallmark),
        ] {
            let (_tmp, root, _) = gen_app(&block_opts(kit, blocks), None);
            let out = std::process::Command::new("rustfmt")
                .args(["--edition", "2024", "--check"])
                .args(rust_files(&root))
                .output()
                .unwrap_or_else(|e| unreachable!("rustfmt must be installed: {e}"));
            assert!(
                out.status.success(),
                "{kit:?} {blocks}: not rustfmt-clean:\n{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

/// D-539: the Prospect block writes the provider, `.prospect(…)` and `PROSPECT_DRIVER`, in every kit; off by default
/// and with no hint (the database driver needs nothing else).
#[test]
fn the_search_block_writes_its_files() {
    assert!(!Blocks::default().has(Block::Search));
    assert!(Blocks::NONE.with(Block::Search).hints().is_empty());
    for kit in Frontend::ALL {
        let (_tmp, root, written) = gen_app(
            &block_opts(kit, Blocks::default().with(Block::Search)),
            None,
        );
        assert!(
            written.iter().any(|w| w == "app/providers/search.rs"),
            "{kit:?}"
        );
        let bootstrap = read(&root, "bootstrap/app.rs");
        assert!(bootstrap.contains("use smeltery::prospect::ProspectExt as _;"));
        assert!(bootstrap.contains("        .prospect(app::providers::search::register)\n"));
        assert!(read(&root, "app/providers/mod.rs").contains("pub mod search;\n"));
        for env in [".env", ".env.example"] {
            assert!(
                read(&root, env).contains("\nPROSPECT_DRIVER=database\n"),
                "{env}"
            );
        }
        assert!(read(&root, "CLAUDE.md").contains("## Search (Prospect)"));
        let (_tmp, root, _) = gen_app(&block_opts(kit, Blocks::default()), None);
        assert!(!root.join("app/providers/search.rs").exists());
        assert!(!read(&root, "bootstrap/app.rs").contains("prospect"));
        assert!(!read(&root, ".env").contains("PROSPECT_"));
    }
    let opts = NewOptions {
        bellows: Bellows {
            mcp: true,
            skills: true,
            guidelines: true,
        },
        ..block_opts(Frontend::Mold, Blocks::default().with(Block::Search))
    };
    let (_tmp, root, _) = gen_app(&opts, None);
    for rel in [
        "app/providers/search.rs",
        "app/providers/mod.rs",
        "bootstrap/app.rs",
        ".env.example",
        "README.md",
        "CLAUDE.md",
        ".bellows/guidelines.md",
        ".bellows/skills/search.md",
    ] {
        golden("web_search", &root, rel);
    }
    // Every kit with every block stays rustfmt-clean.
    for kit in Frontend::ALL {
        let all = Blocks::default()
            .with(Block::Hallmark)
            .with(Block::Anvil)
            .with(Block::Search);
        let (_tmp, root, _) = gen_app(&block_opts(kit, all), None);
        let out = std::process::Command::new("rustfmt")
            .args(["--edition", "2024", "--check"])
            .args(rust_files(&root))
            .output()
            .unwrap_or_else(|e| unreachable!("rustfmt must be installed: {e}"));
        assert!(
            out.status.success(),
            "{kit:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}
