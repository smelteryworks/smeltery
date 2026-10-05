//! Golden tests for the generators: exact output for sample invocations.
//!
//! Golden files live in `tests/golden/make/`. After an intended template change, regenerate them with
//! `SMELTERY_BLESS=1 cargo test -p smeltery-cli make::` and review the diff.

use std::path::{Path, PathBuf};

use clap::Parser;

use super::*;
use crate::new::{Db, NewOptions, Shape, generate};

/// 2026-10-03 12:00:00 UTC.
const NOW: u64 = 1_791_028_800;

fn new_app(kind: Shape) -> (tempfile::TempDir, PathBuf) {
    new_app_with(kind, true, false)
}

/// [`new_app`] with the authentication and Alpine.js answers (web apps).
fn new_app_with(kind: Shape, auth: bool, alpine: bool) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| unreachable!("tempdir: {e}"));
    let root = tmp.path().join("blog");
    let opts = NewOptions {
        kind: kind.kind(),
        db: Db::Sqlite,
        blocks: kind.blocks(auth),
        alpine,
        ..NewOptions::defaults("blog")
    };
    // 2026-10-01 09:00:00 UTC.
    generate(&opts, &root, None, "base64:k", 1_790_845_200)
        .unwrap_or_else(|e| unreachable!("generate: {e:#}"));
    (tmp, root)
}

/// Parses `argv` as the `smeltery` command line and runs the generator at `root` with the fixed clock.
fn make(root: &Path, argv: &[&str]) -> anyhow::Result<Plan> {
    let mut full = vec!["smeltery"];
    full.extend_from_slice(argv);
    let cli = crate::Cli::try_parse_from(full)?;
    let ctx = Ctx { root, now: NOW };
    let plan = match cli.command {
        crate::Command::MakeModel(a) => model(ctx, &a),
        crate::Command::MakeController(a) => controller(ctx, &a),
        crate::Command::MakeMigration(a) => migration(ctx, &a),
        crate::Command::MakeMiddleware(a) => middleware(ctx, &a),
        crate::Command::MakeSeeder(a) => seeder(ctx, &a),
        crate::Command::MakeFactory(a) => factory(ctx, &a),
        crate::Command::MakeCmd(a) => command(ctx, &a),
        crate::Command::MakeAgent(a) => agent(ctx, &a),
        crate::Command::MakeSpark(a) => spark(ctx, &a),
        crate::Command::MakePage(a) => page(ctx, &a),
        crate::Command::MakeMail(a) => mail(ctx, &a),
        crate::Command::MakeJob(a) => job(ctx, &a),
        other => unreachable!("not a generator: {other:?}"),
    }?;
    apply(root, &plan)?;
    Ok(plan)
}

fn ok(root: &Path, argv: &[&str]) -> Plan {
    make(root, argv).unwrap_or_else(|e| unreachable!("{argv:?}: {e:#}"))
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| unreachable!("read {rel}: {e}"))
}

/// Compares `rel` in the app with `tests/golden/make/<case>/<rel>`, or writes it with `SMELTERY_BLESS=1`.
fn golden(case: &str, root: &Path, rel: &str) {
    let actual = read(root, rel);
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/make")
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

/// Every file the plan created or changed matches its golden file, and no template syntax is left.
fn golden_plan(case: &str, root: &Path, plan: &Plan) {
    let mut paths: Vec<&str> = plan.files.iter().map(|(p, _)| p.as_str()).collect();
    paths.extend(plan.inserts.iter().map(|i| i.file.as_str()));
    paths.sort_unstable();
    paths.dedup();
    for rel in paths {
        let text = read(root, rel);
        assert!(
            !text.contains("{%") && !text.contains("{#"),
            "{rel}: template syntax left"
        );
        golden(case, root, rel);
    }
}

#[test]
fn stamps_are_utc_calendar_times() {
    assert_eq!(stamp(0), "1970_01_01_000000");
    assert_eq!(stamp(NOW), "2026_10_03_120000");
    assert_eq!(stamp(951_868_799), "2000_02_29_235959");
    assert_eq!(stamp(NOW + 3_661), "2026_10_03_130101");
}

#[test]
fn model_with_everything_matches_golden() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let plan = ok(
        &root,
        &[
            "make:model",
            "Post",
            "title:string",
            "body:text?",
            "published:bool",
            "-mcrfs",
        ],
    );
    let created: Vec<&str> = plan.files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        created,
        [
            "app/models/post.rs",
            "database/migrations/m2026_10_03_120000_create_posts_table.rs",
            "app/controllers/posts.rs",
            "resources/views/posts/index.mold.html",
            "resources/views/posts/create.mold.html",
            "resources/views/posts/show.mold.html",
            "resources/views/posts/edit.mold.html",
            "database/factories/post_factory.rs",
            "database/seeders/post_seeder.rs",
        ]
    );
    golden_plan("model_all", &root, &plan);
    // --all is the same as -mrfs.
    let (_tmp2, root2) = new_app(Shape::WebWatchfire);
    let all = ok(
        &root2,
        &[
            "make:model",
            "Post",
            "title:string",
            "body:text?",
            "published:bool",
            "--all",
        ],
    );
    assert_eq!(all, plan);
}

#[test]
fn model_with_every_field_type_matches_golden() {
    let (_tmp, root) = new_app(Shape::Web);
    let plan = ok(
        &root,
        &[
            "make:model",
            "Event",
            "name:string",
            "notes:text?",
            "seats:integer",
            "views:bigint?",
            "open:bool",
            "price:float?",
            "day:date",
            "starts_at:datetime?",
            "meta:json",
            "code:uuid",
            "user_id:foreign",
            "-mrf",
        ],
    );
    golden_plan("model_types", &root, &plan);
    assert_eq!(
        plan.notes,
        [
            AUTH_NOTE.replace("{url}", "/events").as_str(),
            "note: day, starts_at, meta, code not in the forms and views; set them in app/controllers/events.rs"
        ]
    );
}

/// The note a resource gets in an app with authentication (`{url}` is its URL).
const AUTH_NOTE: &str = "note: in routes/web.rs, the {url} routes that create, edit and delete records need a \
                         signed-in user (`auth`); the list and the record pages are public";

/// S6-02: with authentication, `index` and `show` are public and the five actions that change records sit behind
/// `auth`; without it the seven routes are public and every place says so. Two resources in one app both register
/// (the comment line naming the URL keeps the inserts apart).
#[test]
fn resource_routes_need_a_signed_in_user_to_change_records() {
    let (_tmp, root) = new_app(Shape::Web);
    let plan = ok(&root, &["make:model", "Post", "title:string", "-mr"]);
    assert_eq!(plan.notes, [AUTH_NOTE.replace("{url}", "/posts")]);
    ok(&root, &["make:model", "Photo", "title:string", "-mr"]);
    let web = read(&root, "routes/web.rs");
    let posts = "    // /posts: viewing is public; creating, editing and deleting need a signed-in user.\n    \
                 r.resource(\"/posts\")\n        .index(crate::app::controllers::posts::index)\n        \
                 .show(crate::app::controllers::posts::show);\n    r.resource(\"/posts\")\n        \
                 .create(crate::app::controllers::posts::create)\n        \
                 .store(crate::app::controllers::posts::store)\n        \
                 .edit(crate::app::controllers::posts::edit)\n        \
                 .update(crate::app::controllers::posts::update)\n        \
                 .destroy(crate::app::controllers::posts::destroy)\n        .middleware(\"auth\");\n";
    assert!(web.contains(posts), "{web}");
    assert!(web.contains("    // /photos: viewing is public;"), "{web}");
    let controller = read(&root, "app/controllers/posts.rs");
    assert!(controller.contains("the other actions sit behind the `auth` middleware"));

    let (_tmp, open) = new_app_with(Shape::Web, false, false);
    let plan = ok(&open, &["make:model", "Post", "title:string", "-mr"]);
    assert_eq!(
        plan.notes,
        [
            "note: the /posts routes are public: anyone can create, edit and delete records; protect them in \
             routes/web.rs before deploying"
        ]
    );
    let web = read(&open, "routes/web.rs");
    assert!(
        web.contains(
            "    // /posts: public, anyone can create, edit and delete records.\n    r.resource(\"/posts\")\n"
        ),
        "{web}"
    );
    assert!(
        !web.contains(".middleware(\"auth\")"),
        "no `auth` alias without authentication"
    );
    assert!(
        read(&open, "app/controllers/posts.rs")
            .contains("//! These routes are public: anyone can create, edit and delete records.")
    );
}

/// S6-01: a `file` field accepts a list of types, never `html` / `svg` / `xml` / `js`: images for a name that says
/// image, images and documents otherwise; the edit form keeps the list.
#[test]
fn file_fields_accept_only_listed_types() {
    let (_tmp, root) = new_app(Shape::Web);
    ok(
        &root,
        &[
            "make:model",
            "Photo",
            "image:file",
            "cover_image:file?",
            "scan:file?",
            "-r",
        ],
    );
    let controller = read(&root, "app/controllers/photos.rs");
    let images = "mimes = \"jpg,jpeg,png,gif,webp\"";
    let documents = "mimes = \"jpg,jpeg,png,gif,webp,pdf,txt,csv,docx,xlsx\"";
    assert!(controller.contains(&format!(
        "#[validate(required, max = 2048, {images})]\n    pub image:"
    )));
    assert!(
        controller.contains(&format!(
            "#[validate(max = 2048, {images})]\n    pub image:"
        )),
        "edit form"
    );
    assert!(controller.contains(&format!(
        "#[validate(max = 2048, {images})]\n    pub cover_image:"
    )));
    assert!(controller.contains(&format!(
        "#[validate(max = 2048, {documents})]\n    pub scan:"
    )));
    for bad in ["html", "svg", "xml", "js"] {
        assert!(!documents.contains(bad) && !images.contains(bad), "{bad}");
    }
}

/// `make:model Post --all` without fields: every file compiles without unused imports (an app builds with
/// `-D warnings`); the goldens pin the whole output.
#[test]
fn a_model_without_fields_makes_warning_free_files() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let plan = ok(&root, &["make:model", "Post", "--all"]);
    golden_plan("model_all_bare", &root, &plan);
    let factory = read(&root, "database/factories/post_factory.rs");
    assert!(
        !factory.contains("use smeltery::db::prelude::*;"),
        "the prelude is unused without fields:\n{factory}"
    );
}

#[test]
fn plain_model_and_names_normalise() {
    let (_tmp, root) = new_app(Shape::Headless);
    let plan = ok(&root, &["make:model", "post-comment", "body:text"]);
    assert_eq!(plan.files.len(), 1);
    golden_plan("model_plain", &root, &plan);
    for spelling in ["PostComment", "post_comment", "postComment"] {
        let (_tmp, other) = new_app(Shape::Headless);
        assert_eq!(
            ok(&other, &["make:model", spelling, "body:text"]),
            plan,
            "{spelling}"
        );
    }
}

#[test]
fn controllers_match_golden() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let plan = ok(&root, &["make:controller", "ReportsController"]);
    golden_plan("controller_plain", &root, &plan);
    // `-c` on a model: a plain controller named after the table.
    let plan = ok(&root, &["make:model", "Tag", "-c"]);
    assert!(
        plan.files
            .iter()
            .any(|(p, _)| p == "app/controllers/tags.rs")
    );
    assert!(read(&root, "routes/web.rs").contains(
        "    r.get(\"/tags\", crate::app::controllers::tags::index)\n        .name(\"tags.index\");"
    ));
}

#[test]
fn resource_controller_from_an_existing_model_matches_make_model_r() {
    let args = ["Article", "title:string", "words:integer", "draft:bool?"];
    let (_tmp, a) = new_app(Shape::WebWatchfire);
    let mut argv = vec!["make:model"];
    argv.extend(args);
    argv.push("-r");
    let with_model = ok(&a, &argv);

    let (_tmp, b) = new_app(Shape::WebWatchfire);
    let mut argv = vec!["make:model"];
    argv.extend(args);
    ok(&b, &argv);
    let separate = ok(
        &b,
        &[
            "make:controller",
            "ArticleController",
            "--resource",
            "--model",
            "Article",
        ],
    );
    assert_eq!(
        separate.files,
        with_model.files.get(1..).unwrap_or_default()
    );
    assert_eq!(read(&a, "routes/web.rs"), read(&b, "routes/web.rs"));
}

#[test]
fn file_fields_upload_through_the_resource_matches_golden() {
    let args = ["Photo", "title:string", "image:file", "scan:file?"];
    let (_tmp, root) = new_app(Shape::Web);
    let mut argv = vec!["make:model"];
    argv.extend(args);
    argv.push("-mrf");
    let plan = ok(&root, &argv);
    golden_plan("model_file", &root, &plan);
    assert_eq!(plan.notes, [AUTH_NOTE.replace("{url}", "/photos")]);
    let form = read(&root, "resources/views/photos/create.mold.html");
    assert!(form.contains(
        "<form method=\"POST\" action=\"/photos\" enctype=\"multipart/form-data\" class="
    ));
    assert!(form.contains(
        "<input id=\"image\" name=\"image\" type=\"file\" required class=\"form-input\" @error("
    ));

    // `make:controller --resource` reads the file fields back from the model.
    let (_tmp, other) = new_app(Shape::Web);
    let mut argv = vec!["make:model"];
    argv.extend(args);
    ok(&other, &argv);
    let separate = ok(&other, &["make:controller", "Photo", "--resource"]);
    let resource: Vec<_> = plan
        .files
        .iter()
        .filter(|(p, _)| p.starts_with("app/controllers/") || p.starts_with("resources/"))
        .cloned()
        .collect();
    assert_eq!(separate.files, resource);
}

#[test]
fn resource_needs_an_existing_model() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let msg = make(&root, &["make:controller", "Post", "--resource"])
        .err()
        .map(|e| e.to_string());
    assert_eq!(
        msg.as_deref(),
        Some("app/models/post.rs not found; create the model first: smeltery make:model Post")
    );
    assert!(make(&root, &["make:controller", "Post", "--model", "Post"]).is_err());
}

#[test]
fn migrations_match_golden() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let a = ok(&root, &["make:migration", "create_comments_table"]);
    golden_plan("migration_create", &root, &a);
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let b = ok(&root, &["make:migration", "add_slug_to_posts_table"]);
    golden_plan("migration_add", &root, &b);
    // `down` undoes `up`: a rollback drops the added column, and `create_*` drops the table.
    let add = b
        .files
        .first()
        .map(|(_, text)| text.as_str())
        .unwrap_or_default();
    assert!(
        add.contains(
            "let sql = \"ALTER TABLE posts DROP COLUMN slug\";
        schema.raw(sql).await"
        ),
        "{add}"
    );
    let create = a
        .files
        .first()
        .map(|(_, text)| text.as_str())
        .unwrap_or_default();
    assert!(
        create.contains("schema.drop_if_exists(\"comments\").await"),
        "{create}"
    );
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let c = ok(&root, &["make:migration", "BackfillSlugs"]);
    golden_plan("migration_blank", &root, &c);
    // The same migration name twice is refused, even a second later.
    let msg = make(&root, &["make:migration", "backfill_slugs"])
        .err()
        .map(|e| e.to_string());
    assert_eq!(
        msg.as_deref(),
        Some(
            "database/migrations/m2026_10_03_120000_backfill_slugs.rs already exists; pick another migration name"
        )
    );
}

#[test]
fn seeder_factory_command_middleware_match_golden() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    ok(&root, &["make:model", "Post", "title:string", "body:text"]);
    let factory = ok(&root, &["make:factory", "PostFactory"]);
    golden_plan("factory", &root, &factory);
    let seeder = ok(&root, &["make:seeder", "PostSeeder"]);
    golden_plan("seeder_with_factory", &root, &seeder);
    let plain = ok(&root, &["make:seeder", "Settings"]);
    assert_eq!(
        plain.files.first().map(|(p, _)| p.as_str()),
        Some("database/seeders/settings_seeder.rs")
    );
    golden("seeder_plain", &root, "database/seeders/settings_seeder.rs");
    let command = ok(&root, &["make:command", "SendReport"]);
    golden_plan("command", &root, &command);
    assert_eq!(command.notes, ["run it with: smeltery send-report"]);
    let middleware = ok(&root, &["make:middleware", "EnsureAdmin"]);
    golden_plan("middleware", &root, &middleware);
    assert!(
        read(&root, "bootstrap/app.rs")
            .find("ensure_admin")
            .is_none(),
        "bootstrap is never edited"
    );
}

#[test]
fn existing_files_are_never_overwritten() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    ok(&root, &["make:model", "Post", "title:string"]);
    let models_mod = read(&root, "app/models/mod.rs");
    let model = read(&root, "app/models/post.rs");
    let msg = make(&root, &["make:model", "Post", "body:text", "-m"])
        .err()
        .map(|e| e.to_string());
    assert_eq!(
        msg.as_deref(),
        Some("app/models/post.rs already exists; nothing was changed")
    );
    assert_eq!(read(&root, "app/models/mod.rs"), models_mod);
    assert_eq!(read(&root, "app/models/post.rs"), model);
    assert!(
        !root
            .join("database/migrations/m2026_10_03_120000_create_posts_table.rs")
            .exists()
    );
    // The user's own file is safe too.
    assert!(std::fs::write(root.join("app/commands/greet.rs"), "// mine\n").is_ok());
    assert!(make(&root, &["make:command", "Greet"]).is_err());
    assert_eq!(read(&root, "app/commands/greet.rs"), "// mine\n");
}

/// S6-12: a dangling symlink where a generator writes (e.g. planted in a cloned repository) is an existing entry:
/// the generator refuses, and the file the link names is never created or truncated.
#[test]
fn a_dangling_symlink_is_never_written_through() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let victim = root.join("victim.txt");
    if !crate::files::tests::symlink_file(&victim, &root.join("app/models/post.rs")) {
        return;
    }
    let models_mod = read(&root, "app/models/mod.rs");
    let msg = make(&root, &["make:model", "Post", "title:string"])
        .err()
        .map(|e| e.to_string());
    assert_eq!(
        msg.as_deref(),
        Some("app/models/post.rs already exists; nothing was changed")
    );
    assert!(!victim.exists(), "nothing was written through the link");
    assert_eq!(read(&root, "app/models/mod.rs"), models_mod);
}

/// S6-12: lines are added through a temporary file and a rename (a new inode), never by truncating the user's file
/// in place, so a crash or a full disk cannot leave it empty.
#[cfg(unix)]
#[test]
fn marker_inserts_replace_the_file_atomically() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let mods = root.join("app/models/mod.rs");
    assert!(std::fs::set_permissions(&mods, std::fs::Permissions::from_mode(0o640)).is_ok());
    let original = read(&root, "app/models/mod.rs");
    // A second name keeps the original inode alive: the command replaces mod.rs more than once, and a file system
    // (ext4) may hand a freed inode to the next temp file, which would make the final inode equal the first one.
    let kept = root.join("mod.rs.orig");
    assert!(std::fs::hard_link(&mods, &kept).is_ok());
    let before = std::fs::metadata(&mods)
        .map(|m| m.ino())
        .unwrap_or_default();
    ok(&root, &["make:model", "Post", "title:string"]);
    let after = std::fs::metadata(&mods).ok();
    assert_ne!(after.as_ref().map(|m| m.ino()), Some(before));
    assert_eq!(after.map(|m| m.permissions().mode() & 0o777), Some(0o640));
    assert!(read(&root, "app/models/mod.rs").contains("pub mod post;"));
    // The old file was never written in place: its other name still holds the old text.
    assert_eq!(std::fs::read_to_string(&kept).ok(), Some(original));
}

#[test]
fn a_missing_marker_leaves_the_file_and_still_creates_the_rest() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let web = read(&root, "routes/web.rs").replace("    // smeltery:routes\n", "");
    assert!(std::fs::write(root.join("routes/web.rs"), &web).is_ok());
    ok(&root, &["make:controller", "Stats"]);
    assert!(root.join("app/controllers/stats.rs").is_file());
    assert!(read(&root, "app/controllers/mod.rs").contains("pub mod stats;"));
    assert_eq!(read(&root, "routes/web.rs"), web);
    // A headless app has no routes file: same behaviour.
    let (_tmp, headless) = new_app(Shape::Headless);
    assert!(make(&headless, &["make:command", "Sync"]).is_ok());
}

/// D-506: a web app made without the Watchfire building block has no `app/agents/mod.rs`; `make:agent` and
/// `make:job` refuse before writing anything (they wrote the file and then warned about missing markers).
#[test]
fn apps_without_watchfire_get_no_agents_or_jobs() {
    let (_tmp, root) = new_app(Shape::Web);
    let jobs_mod = std::fs::read_to_string(root.join("app/jobs/mod.rs")).unwrap();
    for argv in [&["make:agent", "Pinger"][..], &["make:job", "SendDigest"]] {
        let msg = make(&root, argv).err().map(|e| e.to_string());
        assert_eq!(
            msg.as_deref(),
            Some(
                "this app has no Watchfire (no app/agents/mod.rs: it was created with `--smelt` without \
                 `watchfire`), so it takes no agents or jobs; nothing was changed"
            ),
            "{argv:?}"
        );
    }
    assert!(!root.join("app/agents").exists());
    assert!(!root.join("app/jobs/send_digest.rs").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("app/jobs/mod.rs")).unwrap(),
        jobs_mod
    );
    // With Watchfire (and in headless apps) both work.
    for shape in [Shape::WebWatchfire, Shape::Headless] {
        let (_tmp, root) = new_app(shape);
        assert!(make(&root, &["make:agent", "Pinger"]).is_ok(), "{shape:?}");
        assert!(
            make(&root, &["make:job", "SendDigest"]).is_ok(),
            "{shape:?}"
        );
    }
}

#[test]
fn headless_apps_get_no_controllers() {
    let (_tmp, root) = new_app(Shape::Headless);
    let msg = make(&root, &["make:controller", "Reports"])
        .err()
        .map(|e| e.to_string());
    assert_eq!(
        msg.as_deref(),
        Some(
            "this app has no web routes (no routes/web.rs, as in a headless app), so it takes no \
             controllers; nothing was changed"
        )
    );
    assert!(make(&root, &["make:controller", "Post", "--resource"]).is_err());
    assert!(!root.join("app/controllers").exists());
    assert!(!root.join("resources").exists());

    // `make:model -c` / `-r`: the model and its other parts, no controller.
    for flags in ["-mc", "-mrfs"] {
        let (_tmp, root) = new_app(Shape::Headless);
        let plan = ok(&root, &["make:model", "Post", "title:string", flags]);
        assert!(
            plan.files
                .iter()
                .all(|(p, _)| p.starts_with("app/models/") || p.starts_with("database/"))
        );
        assert!(
            plan.files
                .iter()
                .any(|(p, _)| p.starts_with("database/migrations/"))
        );
        assert_eq!(
            plan.notes,
            [
                "note: this app has no web routes (no routes/web.rs, as in a headless app), so it takes \
              no controllers; the controller part was skipped"
            ]
        );
        assert!(!root.join("app/controllers").exists());
    }
}

#[test]
fn bad_fields_and_names_are_rejected() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let msg = make(&root, &["make:model", "Post", "title:strng"])
        .err()
        .map(|e| e.to_string());
    assert_eq!(
        msg.as_deref(),
        Some(
            "unknown field type `strng` in `title:strng`; valid types: string, text, integer, bigint, bool, \
             float, date, datetime, json, uuid, foreign, file"
        )
    );
    assert!(make(&root, &["make:model", "1Post"]).is_err());
    assert!(make(&root, &["make:model", "Type"]).is_err());
    assert!(!root.join("app/models/post.rs").exists());
}

#[test]
fn generators_refuse_outside_an_app() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| unreachable!("tempdir: {e}"));
    let err = run(tmp.path(), |c| {
        model(
            c,
            &ModelArgs {
                name: "Post".into(),
                fields: vec![],
                migration: false,
                controller: false,
                resource: false,
                factory: false,
                seeder: false,
                all: false,
                searchable: false,
            },
        )
    });
    assert!(
        err.err()
            .map(|e| e.to_string())
            .unwrap_or_default()
            .contains("not in a Smeltery app")
    );
}

#[test]
fn agents_and_jobs_match_golden() {
    let (_tmp, root) = new_app(Shape::Headless);
    let agent_plan = ok(&root, &["make:agent", "PricePollerAgent"]);
    golden_plan("agent", &root, &agent_plan);
    let job_plan = ok(&root, &["make:job", "send-welcome"]);
    golden_plan("job", &root, &job_plan);
    assert_eq!(
        job_plan.notes,
        ["dispatch it with: crate::app::jobs::send_welcome::SendWelcome {}.dispatch(&app).await?"]
    );
    let agents = read(&root, "app/agents/mod.rs");
    // The first registration removes the placeholder that kept `w` used.
    assert!(!agents.contains("let _ = &w;"), "{agents}");
    assert!(agents.contains(
        "    w.agent(price_poller::PricePoller::default());\n    \
         w.job::<crate::app::jobs::send_welcome::SendWelcome>();\n    // smeltery:agents"
    ));
    assert!(
        make(&root, &["make:agent", "PricePoller"]).is_err(),
        "an existing agent is never overwritten"
    );
    // An app without Watchfire refuses both (D-506; `apps_without_watchfire_get_no_agents_or_jobs`).
}

#[test]
fn sparks_match_golden() {
    let (_tmp, root) = new_app(Shape::Web);
    let plan = ok(&root, &["make:spark", "TodoListSpark"]);
    golden_plan("spark", &root, &plan);
    assert_eq!(
        plan.notes,
        ["show it on a page with: @spark(\"todo_list\")"]
    );
    assert!(read(&root, "app/sparks/mod.rs").contains(
        "    s.add::<counter::Counter>();\n    s.add::<todo_list::TodoList>();\n    // smeltery:sparks"
    ));
    assert!(
        make(&root, &["make:spark", "Counter"]).is_err(),
        "the generated counter is never overwritten"
    );
}

#[test]
fn mails_match_golden() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let plan = ok(&root, &["make:mail", "InvoicePaidMail"]);
    golden_plan("mail", &root, &plan);
    assert!(
        plan.notes
            .first()
            .is_some_and(|n| n.contains("mailer.send(crate::app::mail::invoice_paid::InvoicePaid"))
    );
    // A headless app has no `home` route: the template has no link to it.
    let (_tmp, headless) = new_app(Shape::Headless);
    ok(&headless, &["make:mail", "Report"]);
    let view = read(&headless, "resources/views/mail/report.mold.html");
    assert!(!view.contains("route("), "{view}");
    assert!(
        make(&headless, &["make:mail", "Report"]).is_err(),
        "never overwritten"
    );
}

#[test]
fn migrations_made_in_the_same_second_get_increasing_stamps() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    ok(&root, &["make:model", "Post", "title:string", "-m"]);
    ok(&root, &["make:migration", "add_image_to_posts_table"]);
    ok(&root, &["make:migration", "create_tags_table"]);
    let dir = root.join("database/migrations");
    for f in [
        "m2026_10_03_120000_create_posts_table.rs",
        "m2026_10_03_120001_add_image_to_posts_table.rs",
        "m2026_10_03_120002_create_tags_table.rs",
    ] {
        assert!(dir.join(f).is_file(), "{f} is missing");
    }
    assert!(
        read(
            &root,
            "database/migrations/m2026_10_03_120001_add_image_to_posts_table.rs"
        )
        .contains("\"2026_10_03_120001_add_image_to_posts_table\"")
    );
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A new app plus one of every generator is already formatted: `cargo fmt` in a fresh app changes nothing.
/// Needs `rustfmt` (a component of the pinned toolchain).
#[test]
fn generated_code_is_rustfmt_clean() {
    // Every web kind with every authentication / Alpine.js answer (D-232, D-233), and headless.
    let mut variants = vec![(Shape::Headless, false, false)];
    for kind in [Shape::WebWatchfire, Shape::Web] {
        for auth in [true, false] {
            for alpine in [true, false] {
                variants.push((kind, auth, alpine));
            }
        }
    }
    for (kind, auth, alpine) in variants {
        let (_tmp, root) = new_app_with(kind, auth, alpine);
        ok(
            &root,
            &[
                "make:model",
                "Post",
                "title:string",
                "body:text?",
                "views:integer",
                "published:bool",
            ],
        );
        ok(
            &root,
            &[
                "make:model",
                "Tag",
                "name:string",
                "post_id:foreign",
                "-mfs",
            ],
        );
        // Long names (review P3-1): the created files go through rustfmt, the added registration lines (the
        // migration's `m.add(...)` passes 100 columns) take rustfmt's layout.
        ok(
            &root,
            &[
                "make:model",
                "WarehouseStockMovement",
                "quantity:integer",
                "-mfs",
            ],
        );
        if kind.has_web() {
            ok(
                &root,
                &[
                    "make:model",
                    "CustomerSupportTicket",
                    "subject:string",
                    "attachment:file?",
                    "urgent:bool",
                    "-mrfs",
                ],
            );
            ok(&root, &["make:controller", "CustomerSupportTicketReports"]);
        }
        ok(
            &root,
            &[
                "make:model",
                "Photo",
                "title:string",
                "image:file",
                "scan:file?",
                "-mf",
            ],
        );
        // A model without fields, with every part (D-208).
        ok(&root, &["make:model", "Note", "--all"]);
        ok(&root, &["make:migration", "add_slug_to_posts_table"]);
        ok(&root, &["make:migration", "backfill_slugs"]);
        ok(&root, &["make:command", "SendReport"]);
        ok(&root, &["make:mail", "Welcome"]);
        ok(&root, &["make:seeder", "ExtraSeeder"]);
        ok(&root, &["make:factory", "PostFactory", "--model", "Post"]);
        if kind.has_web() {
            ok(
                &root,
                &[
                    "make:controller",
                    "PostController",
                    "--resource",
                    "--model",
                    "Post",
                ],
            );
            ok(&root, &["make:controller", "Photo", "--resource"]);
            ok(&root, &["make:controller", "About"]);
            // A long name: its route entry no longer fits rustfmt's call width.
            ok(&root, &["make:controller", "SupplierInvoiceAttachments"]);
            ok(&root, &["make:middleware", "EnsureAdmin"]);
            ok(&root, &["make:spark", "Todo"]);
        }
        if kind.has_agents() {
            ok(&root, &["make:agent", "PricePoller"]);
            ok(&root, &["make:job", "SendWelcome"]);
        }
        let mut files = Vec::new();
        rust_files(&root, &mut files);
        let out = std::process::Command::new("rustfmt")
            .args(["--edition", "2024", "--check"])
            .args(&files)
            .output()
            .unwrap_or_else(|e| {
                unreachable!("rustfmt must be installed (rust-toolchain.toml): {e}")
            });
        assert!(
            out.status.success(),
            "{kind:?} auth {auth} alpine {alpine}: generated code is not rustfmt-clean:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// Every form a resource view posts carries the CSRF field (a delete form without it answers 419).
#[test]
fn every_resource_form_has_csrf() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    ok(&root, &["make:model", "Post", "title:string", "-r"]);
    for view in ["index", "create", "show", "edit"] {
        let text = read(&root, &format!("resources/views/posts/{view}.mold.html"));
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.starts_with("<form method=\"POST\"") {
                assert_eq!(lines.get(i + 1).copied(), Some("@csrf"), "{view}: {line}");
            }
        }
    }
}

/// Every field of a resource form points screen readers at its error message (D-209).
#[test]
fn every_resource_field_describes_its_error() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    ok(
        &root,
        &[
            "make:model",
            "Post",
            "title:string",
            "body:text?",
            "published:bool",
            "image:file?",
            "-r",
        ],
    );
    for view in ["create", "edit"] {
        let text = read(&root, &format!("resources/views/posts/{view}.mold.html"));
        for field in ["title", "body", "published", "image"] {
            let opening = format!("id=\"{field}\" name=\"{field}\"");
            let fields: Vec<&str> = text.lines().filter(|l| l.contains(&opening)).collect();
            assert!(!fields.is_empty(), "{view}: no {field} field");
            for line in fields {
                assert!(
                    line.contains(&format!(
                        "@error(\"{field}\") aria-invalid=\"true\" aria-describedby=\"{field}-error\" @enderror"
                    )),
                    "{view}: {line}"
                );
            }
            assert!(
                text.contains(&format!("<p id=\"{field}-error\" class=\"form-error\">")),
                "{view}: {field}"
            );
        }
    }
}

// --- React and Vue apps (Alloy kits, ALLOY.md §4.7) ---

/// A new app with the React or Vue starter kit.
fn kit_app(
    frontend: crate::new::Frontend,
    kind: Shape,
    auth: bool,
) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| unreachable!("tempdir: {e}"));
    let root = tmp.path().join("blog");
    let opts = NewOptions {
        kind: kind.kind(),
        db: Db::Sqlite,
        frontend: Some(frontend),
        blocks: kind.blocks(auth),
        ..NewOptions::defaults("blog")
    };
    generate(&opts, &root, None, "base64:k", 1_790_845_200)
        .unwrap_or_else(|e| unreachable!("generate: {e:#}"));
    (tmp, root)
}

const KITS: [(crate::new::Frontend, &str); 2] = [
    (crate::new::Frontend::React, "react"),
    (crate::new::Frontend::Vue, "vue"),
];

#[test]
fn kit_models_with_everything_match_golden() {
    for (frontend, kit) in KITS {
        let (_tmp, root) = kit_app(frontend, Shape::WebWatchfire, true);
        let plan = ok(
            &root,
            &[
                "make:model",
                "Post",
                "title:string",
                "body:text?",
                "views:integer",
                "rating:float?",
                "published:bool",
                "--all",
            ],
        );
        let created: Vec<&str> = plan.files.iter().map(|(p, _)| p.as_str()).collect();
        let pages: [&str; 4] = if kit == "react" {
            [
                "resources/js/pages/posts/index.tsx",
                "resources/js/pages/posts/create.tsx",
                "resources/js/pages/posts/show.tsx",
                "resources/js/pages/posts/edit.tsx",
            ]
        } else {
            [
                "resources/js/pages/posts/Index.vue",
                "resources/js/pages/posts/Create.vue",
                "resources/js/pages/posts/Show.vue",
                "resources/js/pages/posts/Edit.vue",
            ]
        };
        let mut expected = vec![
            "app/models/post.rs",
            "database/migrations/m2026_10_03_120000_create_posts_table.rs",
            "app/controllers/posts.rs",
        ];
        expected.extend(pages);
        expected.extend([
            "resources/js/types/post.ts",
            "database/factories/post_factory.rs",
            "database/seeders/post_seeder.rs",
        ]);
        assert_eq!(created, expected, "{kit}");
        assert!(
            !root.join("resources/views/posts").exists(),
            "{kit}: no Mold views"
        );
        golden_plan(&format!("{kit}_model_all"), &root, &plan);
        let controller = read(&root, "app/controllers/posts.rs");
        let component = if kit == "react" {
            "posts/index"
        } else {
            "posts/Index"
        };
        assert!(
            controller.contains(&format!("alloy::render(\"{component}\")")),
            "{kit}: {controller}"
        );
        assert!(!controller.contains("smeltery::Mold"), "{kit}");
    }
}

#[test]
fn kit_file_fields_post_multipart_and_match_golden() {
    for (frontend, kit) in KITS {
        let (_tmp, root) = kit_app(frontend, Shape::Web, true);
        let plan = ok(
            &root,
            &[
                "make:model",
                "Photo",
                "title:string",
                "image:file",
                "scan:file?",
                "public:bool",
                "-r",
            ],
        );
        golden_plan(&format!("{kit}_model_file"), &root, &plan);
        let create = if kit == "react" {
            read(&root, "resources/js/pages/photos/create.tsx")
        } else {
            read(&root, "resources/js/pages/photos/Create.vue")
        };
        assert!(
            create.contains("{ forceFormData: true }"),
            "{kit}: {create}"
        );
        assert!(
            create.contains("public: String(data.public)"),
            "{kit}: {create}"
        );
    }
}

#[test]
fn kit_resource_controller_matches_make_model_r() {
    for (frontend, kit) in KITS {
        let args = ["Article", "title:string", "words:integer", "draft:bool?"];
        let (_tmp, a) = kit_app(frontend, Shape::WebWatchfire, true);
        let mut argv = vec!["make:model"];
        argv.extend(args);
        argv.push("-r");
        let with_model = ok(&a, &argv);
        let (_tmp, b) = kit_app(frontend, Shape::WebWatchfire, true);
        let mut argv = vec!["make:model"];
        argv.extend(args);
        ok(&b, &argv);
        let separate = ok(&b, &["make:controller", "Article", "--resource"]);
        assert_eq!(
            separate.files,
            with_model.files.get(1..).unwrap_or_default(),
            "{kit}"
        );
        assert_eq!(
            read(&a, "routes/web.rs"),
            read(&b, "routes/web.rs"),
            "{kit}"
        );
    }
}

#[test]
fn kit_controllers_and_pages_match_golden() {
    for (frontend, kit) in KITS {
        let (_tmp, root) = kit_app(frontend, Shape::WebWatchfire, false);
        let plan = ok(&root, &["make:controller", "ReportsController"]);
        golden_plan(&format!("{kit}_controller_plain"), &root, &plan);
        let plan = ok(&root, &["make:page", "AboutUs"]);
        golden_plan(&format!("{kit}_page"), &root, &plan);
        let page = if kit == "react" {
            "resources/js/pages/about-us.tsx"
        } else {
            "resources/js/pages/AboutUs.vue"
        };
        assert!(root.join(page).is_file(), "{kit}: {page}");
        assert!(read(&root, "routes/web.rs").contains(
            "    r.get(\"/about-us\", crate::app::controllers::about_us::show)\n        .name(\"about-us\");"
        ));
        // `make:page AboutUsPage` names the same page: refused, it exists.
        let err = make(&root, &["make:page", "AboutUsPage"])
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("already exists"), "{kit}: {err}");
    }
}

#[test]
fn sparks_are_refused_in_kit_apps_and_pages_in_mold_apps() {
    for (frontend, kit) in KITS {
        let (_tmp, root) = kit_app(frontend, Shape::WebWatchfire, true);
        let err = make(&root, &["make:spark", "Todo"])
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("make:page Name"), "{kit}: {err}");
        assert!(err.contains("Nothing was changed"), "{kit}: {err}");
        assert!(!root.join("app/sparks").exists(), "{kit}");
    }
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let err = make(&root, &["make:page", "About"])
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(err.contains("make:controller Name"), "{err}");
    assert!(!root.join("app/controllers/about.rs").exists());
    let (_tmp, root) = new_app(Shape::Headless);
    let err = make(&root, &["make:page", "About"])
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(err.contains("no web routes"), "{err}");
}

/// The Rust files the generators write into kit apps are rustfmt-clean, also with long names.
#[test]
fn kit_generated_code_is_rustfmt_clean() {
    for (frontend, kit) in KITS {
        for auth in [true, false] {
            let (_tmp, root) = kit_app(frontend, Shape::WebWatchfire, auth);
            ok(
                &root,
                &[
                    "make:model",
                    "WarehouseStockMovement",
                    "title:string",
                    "body:text?",
                    "views:integer",
                    "published:bool",
                    "--all",
                ],
            );
            ok(
                &root,
                &[
                    "make:model",
                    "CustomerSupportTicket",
                    "subject:string",
                    "attachment:file",
                    "urgent:bool",
                    "-mrfs",
                ],
            );
            ok(&root, &["make:model", "Note", "--all"]);
            ok(&root, &["make:controller", "About"]);
            ok(&root, &["make:page", "PrivacyPolicy"]);
            ok(&root, &["make:page", "CustomerSupportOverviewPage"]);
            let mut files = Vec::new();
            rust_files(&root, &mut files);
            let out = std::process::Command::new("rustfmt")
                .args(["--edition", "2024", "--check"])
                .args(&files)
                .output()
                .unwrap_or_else(|e| {
                    unreachable!("rustfmt must be installed (rust-toolchain.toml): {e}")
                });
            assert!(
                out.status.success(),
                "{kit} auth {auth}: generated code is not rustfmt-clean:\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    }
}

/// Every field of a kit form points screen readers at its error message (D-209), like the Mold forms.
#[test]
fn every_kit_field_describes_its_error() {
    for (frontend, kit) in KITS {
        let (_tmp, root) = kit_app(frontend, Shape::Web, true);
        ok(
            &root,
            &[
                "make:model",
                "Post",
                "title:string",
                "body:text?",
                "views:integer",
                "published:bool",
                "image:file?",
                "-r",
            ],
        );
        let pages = if kit == "react" {
            ["create.tsx", "edit.tsx"]
        } else {
            ["Create.vue", "Edit.vue"]
        };
        for page in pages {
            let text = read(&root, &format!("resources/js/pages/posts/{page}"));
            for field in ["title", "body", "views", "published", "image"] {
                assert!(
                    text.contains(&format!("id=\"{field}\"")),
                    "{kit} {page}: {field}"
                );
                assert!(
                    text.contains(&format!("? '{field}-error' : undefined")),
                    "{kit} {page}: {field}"
                );
                assert!(
                    text.contains(&format!("id=\"{field}-error\" ")),
                    "{kit} {page}: {field}"
                );
            }
        }
    }
}

/// Without rustfmt the generator still writes its files (formatting is a convenience, never a failure).
#[test]
fn a_missing_rustfmt_is_only_a_note() {
    let (_tmp, root) = new_app(Shape::Headless);
    std::fs::write(
        root.join("loose.rs"),
        "fn  main( ) { }
",
    )
    .unwrap_or_default();
    let err = rustfmt_files(
        &root,
        &["loose.rs"],
        std::ffi::OsStr::new("no-such-rustfmt-binary"),
    );
    assert!(err.is_err());
    assert_eq!(
        read(&root, "loose.rs"),
        "fn  main( ) { }
"
    );
    // With rustfmt the same file is formatted, with the app's settings.
    assert_eq!(
        rustfmt_files(&root, &["loose.rs"], std::ffi::OsStr::new("rustfmt")),
        Ok(())
    );
    assert_eq!(
        read(&root, "loose.rs"),
        "fn main() {}
"
    );
}

/// rustfmt follows `mod x;` declarations, so a created file that declared modules would reformat the user's files
/// too: the files the generators create declare none (their registrations go into existing `mod.rs` files).
#[test]
fn created_rust_files_declare_no_modules() {
    for (frontend, _) in KITS {
        let (_tmp, root) = kit_app(frontend, Shape::WebWatchfire, true);
        let mut plans = vec![ok(
            &root,
            &["make:model", "Post", "title:string", "image:file?", "--all"],
        )];
        for argv in [
            &["make:page", "About"][..],
            &["make:controller", "Reports"],
            &["make:agent", "Poller"],
            &["make:job", "Ping"],
            &["make:mail", "Welcome"],
            &["make:command", "SendReport"],
            &["make:middleware", "EnsureAdmin"],
            &["make:migration", "backfill_posts"],
        ] {
            plans.push(ok(&root, argv));
        }
        for (rel, contents) in plans.iter().flat_map(|p| p.files.iter()) {
            for line in contents.lines() {
                let line = line.trim_start();
                let declares = (line.starts_with("mod ") || line.starts_with("pub mod "))
                    && line.ends_with(';');
                assert!(!declares, "{rel}: {line}");
            }
        }
    }
}

/// A multi-word model's pages read the props the controller sends: `alloy::render(…).with("<name>", …)` uses the
/// names the page destructures (`postComments`, `postComment`), not the Rust names (`post_comments`).
#[test]
fn kit_pages_read_the_props_their_controller_sends() {
    for (frontend, kit) in KITS {
        let (_tmp, root) = kit_app(frontend, Shape::WebWatchfire, true);
        ok(
            &root,
            &["make:model", "PostComment", "body:string", "--all"],
        );
        let controller = read(&root, "app/controllers/post_comments.rs");
        for prop in ["postComments", "postComment"] {
            assert!(
                controller.contains(&format!(".with(\"{prop}\", ")),
                "{kit}: {controller}"
            );
        }
        assert!(!controller.contains(".with(\"post_comment"), "{kit}");
        let (index, show) = if kit == "react" {
            ("index.tsx", "show.tsx")
        } else {
            ("Index.vue", "Show.vue")
        };
        let index = read(&root, &format!("resources/js/pages/post-comments/{index}"));
        assert!(index.contains("postComments"), "{kit}: {index}");
        let show = read(&root, &format!("resources/js/pages/post-comments/{show}"));
        assert!(show.contains("postComment"), "{kit}: {show}");
    }
}

// --- Search (Prospect, D-539) ---

/// A web app with the Prospect building block (and the default blocks) in the given kit.
fn search_app(frontend: crate::new::Frontend) -> (tempfile::TempDir, PathBuf) {
    use crate::new::{Block, Blocks};
    let tmp = tempfile::tempdir().unwrap_or_else(|e| unreachable!("tempdir: {e}"));
    let root = tmp.path().join("blog");
    let opts = NewOptions {
        db: Db::Sqlite,
        frontend: Some(frontend),
        blocks: Blocks::default().with(Block::Search),
        ..NewOptions::defaults("blog")
    };
    generate(&opts, &root, None, "base64:k", 1_790_845_200)
        .unwrap_or_else(|e| unreachable!("generate: {e:#}"));
    (tmp, root)
}

#[test]
fn searchable_models_match_golden() {
    for (frontend, case) in [
        (crate::new::Frontend::Mold, "model_searchable"),
        (crate::new::Frontend::React, "react_model_searchable"),
        (crate::new::Frontend::Vue, "vue_model_searchable"),
    ] {
        let (_tmp, root) = search_app(frontend);
        let plan = ok(
            &root,
            &[
                "make:model",
                "Post",
                "title:string",
                "body:text?",
                "user_id:foreign?",
                "--searchable",
                "--all",
            ],
        );
        golden_plan(case, &root, &plan);
        let model = read(&root, "app/models/post.rs");
        assert!(
            model.contains("i.text(\"title\").weight(Weight::A);"),
            "{model}"
        );
        assert!(model.contains("i.text(\"body\");") && model.contains("i.filter(\"user_id\");"));
        let migration = read(
            &root,
            "database/migrations/m2026_10_03_120000_create_posts_table.rs",
        );
        assert!(
            migration.contains(".text(\"title\", Weight::A)"),
            "{migration}"
        );
        assert!(migration.contains(".text(\"body\", Weight::B)"));
        assert!(migration.contains("SearchIndex::on(\"posts\").drop(schema).await?;"));
        assert!(
            read(&root, "app/providers/search.rs").contains(
                "    p.model::<crate::app::models::Post>();\n    // smeltery:searchables"
            )
        );
        assert!(!read(&root, "app/providers/search.rs").contains("let _ = &p;"));
        let routes = read(&root, "routes/web.rs");
        assert!(routes.contains(
            ".index(crate::app::controllers::posts::index)\n        .middleware(\"throttle:60,1\");"
        ));
        let controller = read(&root, "app/controllers/posts.rs");
        assert!(
            controller.contains(".highlight([\"title\"])"),
            "{controller}"
        );
        assert!(controller.contains("Post::search(&prospect, &q)"));
    }
}

#[test]
fn searchable_without_a_migration_writes_the_index_migration() {
    let (_tmp, root) = search_app(crate::new::Frontend::Mold);
    ok(&root, &["make:model", "Note", "body:text", "-m"]);
    // The table exists already: the index comes in a migration of its own.
    let plan = make(
        &root,
        &["make:model", "Article", "title:string", "--searchable"],
    );
    let plan = plan.unwrap_or_else(|e| unreachable!("{e:#}"));
    let (path, contents) = plan
        .files
        .iter()
        .find(|(p, _)| p.contains("_add_search_index_to_articles_table.rs"))
        .unwrap_or_else(|| unreachable!("{:?}", plan.files));
    assert!(path.starts_with("database/migrations/m"));
    assert!(
        contents.contains("SearchIndex::on(\"articles\")\n            .text(\"title\", Weight::A)")
    );
    assert!(contents.contains("SearchIndex::on(\"articles\").drop(schema).await"));
    assert!(!contents.contains("DROP COLUMN"), "{contents}");
    golden("searchable_index_migration", &root, path);
}

#[test]
fn searchable_is_refused_without_the_block_or_a_text_field() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let err = make(
        &root,
        &["make:model", "Post", "title:string", "--searchable"],
    )
    .err()
    .map(|e| e.to_string())
    .unwrap_or_default();
    assert!(err.contains("no Prospect building block"), "{err}");
    assert!(!root.join("app/models/post.rs").exists());
    let (_tmp, root) = search_app(crate::new::Frontend::Mold);
    let err = make(
        &root,
        &["make:model", "Score", "points:integer", "--searchable"],
    )
    .err()
    .map(|e| e.to_string())
    .unwrap_or_default();
    assert!(err.contains("needs a `string` or `text` field"), "{err}");
    assert!(!root.join("app/models/score.rs").exists());
}

/// `make:controller --resource` for a searchable model writes the searching list, as `make:model -r` does.
#[test]
fn a_resource_for_a_searchable_model_searches() {
    for frontend in crate::new::Frontend::ALL {
        let (_tmp, root) = search_app(frontend);
        ok(
            &root,
            &["make:model", "Post", "title:string", "--searchable", "-m"],
        );
        let plan = ok(&root, &["make:controller", "Post", "--resource"]);
        let controller = read(&root, "app/controllers/posts.rs");
        assert!(
            controller.contains("Post::search(&prospect, &q)"),
            "{frontend:?}"
        );
        assert!(read(&root, "routes/web.rs").contains("throttle:60,1"));
        let (_tmp2, other) = search_app(frontend);
        ok(
            &other,
            &[
                "make:model",
                "Post",
                "title:string",
                "--searchable",
                "-m",
                "-r",
            ],
        );
        for (rel, contents) in &plan.files {
            assert_eq!(&read(&other, rel), contents, "{frontend:?}: {rel}");
        }
    }
}

#[test]
fn prospect_install_adds_the_provider() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let ctx = Ctx {
        root: &root,
        now: NOW,
    };
    let plan = prospect::install(ctx).unwrap_or_else(|e| unreachable!("{e:#}"));
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains(".prospect(app::providers::search::register)"))
    );
    apply(&root, &plan).unwrap_or_else(|e| unreachable!("{e:#}"));
    let (_tmp2, made) = search_app(crate::new::Frontend::Mold);
    assert_eq!(
        read(&root, "app/providers/search.rs"),
        read(&made, "app/providers/search.rs")
    );
    assert_eq!(
        read(&root, "app/providers/mod.rs"),
        read(&made, "app/providers/mod.rs")
    );
    // Refused in an app that has it, and in a headless app.
    let err = prospect::install(Ctx {
        root: &made,
        now: NOW,
    })
    .err()
    .map(|e| e.to_string())
    .unwrap_or_default();
    assert!(err.contains("already calls"), "{err}");
    let (_tmp3, headless) = new_app(Shape::Headless);
    assert!(
        prospect::install(Ctx {
            root: &headless,
            now: NOW
        })
        .is_err()
    );
}

/// Mold reads `@word` right after a letter, digit or `_` as text (so `me@else.com` stays text): a directive glued to
/// a word (`@endfor@else`) would be printed instead of run. No template writes one (Stage E review H1).
#[test]
fn no_template_glues_a_mold_directive_to_a_word() {
    // Mold's own list, so the test follows the language.
    const DIRECTIVES: &[&str] = smeltery_mold::DIRECTIVES;
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.to_string_lossy().contains(".mold.html") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("templates"),
        &mut files,
    );
    assert!(files.len() > 10);
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        for (at, _) in text.match_indices('@') {
            let before = text[..at].chars().next_back();
            let word: String = text[at + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            let glued = before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            assert!(
                !(glued && DIRECTIVES.contains(&word.as_str())),
                "{}: `@{word}` right after `{}`",
                file.display(),
                before.unwrap_or(' ')
            );
        }
    }
}

/// Stage E review M1: a model whose search is scoped gets no generated list (it would need the scope value).
#[test]
fn a_resource_for_a_scoped_searchable_model_is_refused() {
    let (_tmp, root) = search_app(crate::new::Frontend::Mold);
    ok(
        &root,
        &[
            "make:model",
            "Post",
            "title:string",
            "team_id:bigint",
            "--searchable",
            "-m",
        ],
    );
    let path = root.join("app/models/post.rs");
    let source = read(&root, "app/models/post.rs").replace(
        "        i.text(\"title\").weight(Weight::A);\n",
        "        i.text(\"title\").weight(Weight::A);\n        i.scoped_by(\"team_id\");\n",
    );
    std::fs::write(&path, source).unwrap_or_else(|e| unreachable!("{e}"));
    let err = make(&root, &["make:controller", "Post", "--resource"])
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert_eq!(
        err,
        "app/models/post.rs scopes its search (`i.scoped_by(…)`): the list needs the scope value of the signed-in \
         user, so write its `index` by hand with `.within(value)` (see the Search section of CLAUDE.md); nothing \
         was changed"
    );
    assert!(!root.join("app/controllers/posts.rs").exists());
}

/// Stage E review L1: the search box shows the text the search used, cut at `PROSPECT_MAX_QUERY_LENGTH`.
#[test]
fn the_search_text_is_cut_before_it_is_shown() {
    for frontend in crate::new::Frontend::ALL {
        let (_tmp, root) = search_app(frontend);
        ok(
            &root,
            &[
                "make:model",
                "Post",
                "title:string",
                "--searchable",
                "-m",
                "-r",
            ],
        );
        let controller = read(&root, "app/controllers/posts.rs");
        assert!(
            controller.contains(".take(prospect.settings().query_length())"),
            "{frontend:?}"
        );
        assert!(
            !controller.contains("search.q)"),
            "{frontend:?}: {controller}"
        );
    }
}

/// Stage E review H1: `make:model … --searchable --all` writes a test of the list, without and with a search.
#[test]
fn a_searchable_resource_with_a_factory_gets_a_search_test() {
    for (frontend, case) in [
        (crate::new::Frontend::Mold, "search_test"),
        (crate::new::Frontend::React, "react_search_test"),
    ] {
        let (_tmp, root) = search_app(frontend);
        ok(
            &root,
            &[
                "make:model",
                "Post",
                "title:string",
                "--searchable",
                "--all",
            ],
        );
        golden(case, &root, "tests/posts_search.rs");
        // Without a factory, no test (it would have no records).
        let (_tmp2, other) = search_app(frontend);
        ok(
            &other,
            &[
                "make:model",
                "Post",
                "title:string",
                "--searchable",
                "-m",
                "-r",
            ],
        );
        assert!(!other.join("tests/posts_search.rs").exists());
    }
}

// --- Listening Sparks (Anvil, D-432) ---

/// A web app with the Anvil building block (and the default blocks) in the given kit.
fn anvil_app(frontend: crate::new::Frontend) -> (tempfile::TempDir, PathBuf) {
    use crate::new::{Block, Blocks};
    let tmp = tempfile::tempdir().unwrap_or_else(|e| unreachable!("tempdir: {e}"));
    let root = tmp.path().join("blog");
    let opts = NewOptions {
        db: Db::Sqlite,
        frontend: Some(frontend),
        blocks: Blocks::default().with(Block::Anvil),
        ..NewOptions::defaults("blog")
    };
    generate(&opts, &root, None, "base64:k", 1_790_845_200)
        .unwrap_or_else(|e| unreachable!("generate: {e:#}"));
    (tmp, root)
}

#[test]
fn a_listening_spark_matches_golden() {
    let (_tmp, root) = anvil_app(crate::new::Frontend::Mold);
    let plan = ok(
        &root,
        &[
            "make:spark",
            "OrderStatus",
            "--listen",
            "private-orders.{order_id}",
            "--event",
            "OrderShipped",
        ],
    );
    golden_plan("spark_listener", &root, &plan);
    let source = read(&root, "app/sparks/order_status.rs");
    assert!(
        source.contains("#[spark(name = \"order_status\", stream)]"),
        "{source}"
    );
    assert!(
        source.contains(r#"#[on("anvil:private-orders.{order_id}", "App\\Events\\OrderShipped")]"#)
    );
    assert!(source.contains("pub order_id: i64,"));
    assert!(source.contains("pub struct OrderShipped {}"));
    // An event sent under its own name, on a public channel.
    let plan = ok(
        &root,
        &[
            "make:spark",
            "Scores",
            "--listen",
            "scores.{game}",
            "--event",
            "score.updated",
        ],
    );
    let source = &plan.files[0].1;
    assert!(
        source.contains(r#"#[on("anvil:scores.{game}", "score.updated")]"#),
        "{source}"
    );
    assert!(source.contains("pub game: String,") && source.contains("pub struct ScoresEvent {}"));
}

#[test]
fn a_listening_spark_is_refused_without_anvil_or_with_a_bad_channel() {
    let (_tmp, root) = new_app(Shape::WebWatchfire);
    let err = make(
        &root,
        &[
            "make:spark",
            "Feed",
            "--listen",
            "news",
            "--event",
            "Posted",
        ],
    )
    .err()
    .map(|e| e.to_string())
    .unwrap_or_default();
    assert!(err.contains("no Anvil building block"), "{err}");
    assert!(!root.join("app/sparks/feed.rs").exists());
    let (_tmp, root) = anvil_app(crate::new::Frontend::Mold);
    for (channel, event) in [
        ("news room", "Posted"),
        ("orders.{Order}", "Posted"),
        ("orders.{order", "Posted"),
        ("news", "bad\"name"),
    ] {
        assert!(
            make(
                &root,
                &["make:spark", "Feed", "--listen", channel, "--event", event]
            )
            .is_err(),
            "{channel} {event}"
        );
    }
    // `--listen` and `--event` go together.
    assert!(make(&root, &["make:spark", "Feed", "--listen", "news"]).is_err());
    assert!(!root.join("app/sparks/feed.rs").exists());
}
