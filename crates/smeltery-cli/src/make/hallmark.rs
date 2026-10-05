//! `hallmark:install`: Hallmark's API tokens for an app made without the Hallmark building block (D-467): the
//! `personal_access_tokens` migration, `app/controllers/api/{tokens,user}.rs`, the three routes of `routes/api.rs`,
//! `tests/api_tokens.rs` and, with Watchfire, the daily prune. The files are the ones `smeltery new --smelt hallmark`
//! writes; `bootstrap/app.rs` has no marker for the builder chain, so the two lines it needs are printed.

use anyhow::bail;
use serde::Serialize;

use super::{Ctx, Plan, render::unique_stamp};
use crate::templates::{TEMPLATES, render_source};

/// The migration file's name, after its stamp.
const SUFFIX: &str = "_create_personal_access_tokens_table.rs";

/// The token routes, as `smeltery new` writes them into `routes/api.rs` (inserted above `// smeltery:routes`).
pub(crate) const ROUTES: &str = "\
// API tokens (Hallmark): a mobile app, a desktop app or another client gets a token for an e-mail address and a
// password, sends it as `Authorization: Bearer …` on `auth:hallmark` routes, and signs it out.
r.post(\"/tokens\", crate::app::controllers::api::tokens::store)
    .name(\"api.tokens.store\")
    .middleware(\"throttle:10,1\");
r.delete(
    \"/tokens/current\",
    crate::app::controllers::api::tokens::destroy,
)
.name(\"api.tokens.destroy\")
.middleware(\"auth:hallmark\");
// `verified`: with `.verify_email` on, a token of an unverified address gets 403 here (it may still sign out).
r.get(\"/user\", crate::app::controllers::api::user::show)
    .name(\"api.user\")
    .middleware(\"auth:hallmark\")
    .middleware(\"verified\");";

/// The daily prune, as `smeltery new` writes it into `app/agents/mod.rs` (inserted above `// smeltery:agents`).
pub(crate) const PRUNE: &str = "\
// API tokens that expired a day ago or more leave the table (`smeltery hallmark:prune-expired` by hand).
w.schedule()
    .call(\"hallmark-prune\", |ctx| async move {
        let tokens = smeltery::hallmark::Tokens::of(ctx.app())?;
        tokens.prune_expired(24.hours()).await?;
        Ok(())
    })
    .daily();";

/// The `.env` lines (commented: the defaults apply).
const ENV: &str = "\
# API tokens (Hallmark). Days a new token lasts (0: never expires; lowering it also shortens existing tokens).
# HALLMARK_TOKEN_EXPIRATION=365
# Let a JavaScript frontend on this app's own origin call `auth:hallmark` API routes with its session cookie.
# HALLMARK_SPA=false
# More first-party origins for SPA mode, comma-separated (scheme://host[:port]); only origins you control.
# HALLMARK_STATEFUL=
# CORS for hybrid and other-origin apps that call /api/ with a bearer token (never cookies): their exact origins,
# comma-separated, such as capacitor://localhost or tauri://localhost; CORS_PATHS sets the paths (default /api/).
# CORS_ALLOWED_ORIGINS=
# CORS_PATHS=/api/";

#[derive(Serialize)]
struct Values {
    lib_name: String,
    hallmark_migration_name: String,
}

/// The source of the `smeltery new` template that writes `out`.
fn source(out: &str) -> anyhow::Result<&'static str> {
    TEMPLATES
        .iter()
        .find(|t| t.out == out)
        .map(|t| t.source)
        .ok_or_else(|| anyhow::anyhow!("no template for {out}"))
}

/// The plan. Refuses an app without authentication (tokens belong to user accounts), and an app that has the
/// migration already; [`super::apply`] refuses when any file to create exists.
pub(crate) fn install(ctx: Ctx<'_>) -> anyhow::Result<Plan> {
    let bootstrap =
        std::fs::read_to_string(ctx.root.join("bootstrap").join("app.rs")).unwrap_or_default();
    let has = |prefix: &str| {
        bootstrap
            .lines()
            .any(|l| l.trim_start().starts_with(prefix))
    };
    if !has(".temper(") && !has(".auth::<") {
        bail!(
            "Hallmark needs user accounts: this app has no authentication (`.temper(…)` or `.auth::<User>()` in \
             bootstrap/app.rs); nothing was changed"
        );
    }
    if let Ok(entries) = std::fs::read_dir(ctx.root.join("database").join("migrations")) {
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if file.starts_with('m') && file.ends_with(SUFFIX) {
                bail!("database/migrations/{file} already exists; nothing was changed");
            }
        }
    }
    let name = crate::cmd::package_name(ctx.root)?;
    let stamp = unique_stamp(ctx);
    let module = format!("m{stamp}_create_personal_access_tokens_table");
    let values = Values {
        lib_name: crate::new::lib_name(&name),
        hallmark_migration_name: format!("{stamp}_create_personal_access_tokens_table"),
    };
    let mut plan = Plan::default();
    plan.file(
        format!("database/migrations/{module}.rs"),
        render_source(
            "hallmark migration",
            source("database/migrations/{{ hallmark_migration }}.rs")?,
            &values,
        )?,
    );
    plan.insert(
        "database/migrations/mod.rs",
        "// smeltery:mods",
        format!("pub mod {module};"),
    );
    plan.insert(
        "database/migrations/mod.rs",
        "// smeltery:migrations",
        format!("m.add({module}::CreatePersonalAccessTokensTable);"),
    );
    let api = ctx.root.join("app").join("controllers").join("api");
    if api.join("mod.rs").is_file() {
        // The app has API controllers of its own: add the two modules to its list.
        plan.insert(
            "app/controllers/api/mod.rs",
            "// smeltery:mods",
            "pub mod tokens;",
        );
        plan.insert(
            "app/controllers/api/mod.rs",
            "// smeltery:mods",
            "pub mod user;",
        );
    } else {
        plan.file(
            "app/controllers/api/mod.rs",
            source("app/controllers/api/mod.rs")?.to_owned(),
        );
        plan.insert("app/controllers/mod.rs", "// smeltery:mods", "pub mod api;");
    }
    for rel in [
        "app/controllers/api/tokens.rs",
        "app/controllers/api/user.rs",
    ] {
        plan.file(rel, source(rel)?.to_owned());
    }
    plan.file(
        "tests/api_tokens.rs",
        render_source(
            "tests/api_tokens.rs",
            source("tests/api_tokens.rs")?,
            &values,
        )?,
    );
    plan.insert("routes/api.rs", "// smeltery:routes", ROUTES);
    if ctx.root.join("app").join("agents").join("mod.rs").is_file() {
        plan.insert("app/agents/mod.rs", "// smeltery:agents", PRUNE);
    }
    if !has(".hallmark(") {
        plan.notes.push(
            "Add Hallmark to `build` in bootstrap/app.rs (it has no marker for this):\n    \
             use smeltery::hallmark::{Hallmark, HallmarkExt as _};\n    \
             .hallmark(Hallmark::new())   // in the builder chain, after `.temper(…)` / `.auth::<User>()`"
                .to_owned(),
        );
    }
    plan.notes.push(format!(
        "Optional settings for .env and .env.example (the defaults apply without them):\n{}",
        ENV.lines()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    ));
    plan.notes.push(
        "Run `smeltery migrate` to create the personal_access_tokens table, then `smeltery test`."
            .to_owned(),
    );
    Ok(plan)
}
