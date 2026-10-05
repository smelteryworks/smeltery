//! `prospect:install`: Prospect's search for an app made without the Prospect building block (D-539): the provider
//! `app/providers/search.rs` (where `make:model --searchable` registers models) and its `mod` line.
//! `bootstrap/app.rs` has no marker for the builder chain and `.env` is the user's file, so their lines are printed.

use anyhow::bail;

use super::{Ctx, Plan};
use crate::templates::TEMPLATES;

/// The provider, as `smeltery new --smelt prospect` writes it.
fn provider() -> anyhow::Result<&'static str> {
    TEMPLATES
        .iter()
        .find(|t| t.out == "app/providers/search.rs")
        .map(|t| t.source)
        .ok_or_else(|| anyhow::anyhow!("no template for app/providers/search.rs"))
}

/// The plan. Refuses in an app without web routes and in an app that has search already (`.prospect(` in
/// `bootstrap/app.rs`); [`super::apply`] refuses when the provider file exists.
pub(crate) fn install(ctx: Ctx<'_>) -> anyhow::Result<Plan> {
    if !ctx.root.join("routes").join("web.rs").is_file() {
        bail!(
            "this app has no web routes (a headless app); the Prospect building block is for web apps; nothing was changed"
        );
    }
    let bootstrap =
        std::fs::read_to_string(ctx.root.join("bootstrap").join("app.rs")).unwrap_or_default();
    if bootstrap
        .lines()
        .any(|l| l.trim_start().starts_with(".prospect("))
    {
        bail!("bootstrap/app.rs already calls `.prospect(…)`; nothing was changed");
    }
    let mut plan = Plan::default();
    plan.file("app/providers/search.rs", provider()?.to_owned());
    plan.insert(
        "app/providers/mod.rs",
        "// smeltery:mods",
        "pub mod search;",
    );
    plan.notes.push(
        "Add Prospect to `build` in bootstrap/app.rs (it has no marker for this):\n    \
         use smeltery::prospect::ProspectExt as _;\n    \
         .prospect(app::providers::search::register)   // in the builder chain"
            .to_owned(),
    );
    plan.notes.push(
        "Add to .env and .env.example (the default without it is the same):\n    PROSPECT_DRIVER=database".to_owned(),
    );
    plan.notes.push(
        "Then make models searchable: `smeltery make:model Post title:string --searchable --all`."
            .to_owned(),
    );
    Ok(plan)
}
