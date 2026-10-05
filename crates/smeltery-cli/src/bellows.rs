//! `smeltery bellows:install`: add the Bellows files (MCP registration, skills, guidelines) to an existing app.

use std::path::Path;

use anyhow::{Context as _, bail};
use clap::Args;

use crate::templates::{self, TEMPLATES, When};

/// Arguments of `smeltery bellows:install`.
#[derive(Debug, Args)]
pub(crate) struct InstallArgs {
    /// `.mcp.json`: register the app's MCP server (`smeltery bellows:mcp`).
    #[arg(long)]
    mcp: bool,
    /// `.bellows/skills/`: step-by-step guides for common tasks.
    #[arg(long)]
    skills: bool,
    /// `.bellows/guidelines.md`: the app's conventions.
    #[arg(long)]
    guidelines: bool,
    /// All of the above.
    #[arg(long)]
    all: bool,
}

/// The `.mcp.json` entry for the app's MCP server.
const SERVER_ENTRY: &str = r#""smeltery": {
      "command": "smeltery",
      "args": ["bellows:mcp"]
    }"#;

/// What `install` did to one file.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Created(String),
    Kept(String),
    Updated(String),
    /// The file could not be changed safely; the message says what to add by hand.
    ByHand(String),
}

/// Runs `bellows:install` in the app at `dir`.
pub(crate) fn run(dir: &Path, args: &InstallArgs) -> anyhow::Result<()> {
    crate::cmd::require_app(dir)?;
    let (mcp, skills, guidelines) = (
        args.mcp || args.all,
        args.skills || args.all,
        args.guidelines || args.all,
    );
    if !(mcp || skills || guidelines) {
        bail!("choose what to install: --mcp, --skills, --guidelines or --all");
    }
    for change in install(dir, mcp, skills, guidelines)? {
        match change {
            Change::Created(p) => println!("created {p}"),
            Change::Updated(p) => println!("updated {p}"),
            Change::Kept(p) => println!("kept {p} (it exists)"),
            Change::ByHand(msg) => eprintln!("warning: {msg}"),
        }
    }
    if mcp {
        println!("The MCP server runs as `smeltery bellows:mcp` from the app directory.");
    }
    Ok(())
}

/// Writes the chosen parts into the app at `dir`; existing files are kept.
pub(crate) fn install(
    dir: &Path,
    mcp: bool,
    skills: bool,
    guidelines: bool,
) -> anyhow::Result<Vec<Change>> {
    let name = crate::cmd::package_name(dir)?;
    let web = dir.join("routes").is_dir();
    // The starter kit (`[package.metadata.smeltery] frontend`): React and Vue apps get their own guides.
    let frontend = if web {
        Some(crate::frontend::app_frontend(dir)?)
    } else {
        None
    };
    let temper = dir
        .join("app")
        .join("providers")
        .join("temper.rs")
        .is_file();
    // Hallmark: `.hallmark(` in `bootstrap/app.rs` (`smeltery new --smelt hallmark`, or added by hand after
    // `hallmark:install`). Anvil: the channels file `smeltery new --smelt anvil` writes.
    let hallmark =
        std::fs::read_to_string(dir.join("bootstrap").join("app.rs")).is_ok_and(|text| {
            text.lines()
                .any(|l| l.trim_start().starts_with(".hallmark("))
        });
    let anvil = dir.join("routes").join("channels.rs").is_file();
    // Search: the provider `smeltery new --smelt prospect` and `prospect:install` write.
    let search = dir
        .join("app")
        .join("providers")
        .join("search.rs")
        .is_file();
    let ctx = templates::Context {
        lib_name: crate::new::lib_name(&name),
        title: crate::new::title(&name),
        name,
        web,
        agents: dir.join("app").join("agents").is_dir(),
        // Authentication: Temper's provider (`smeltery new` with the `temper` block, D-486), or the controllers that
        // apps made before Temper have in `app/controllers/auth/` (D-232).
        auth: temper || dir.join("app").join("controllers").join("auth").is_dir(),
        temper,
        hallmark: web && hallmark,
        anvil: web && anvil,
        search: web && search,
        bellows_mcp: mcp,
        bellows_skills: skills,
        bellows_guidelines: guidelines,
        ..templates::Context::default()
    }
    .with_frontend(frontend);
    let mut changes = Vec::new();
    for template in TEMPLATES {
        let wanted = match template.when {
            When::BellowsSkills => skills,
            When::BellowsSkillsWeb => skills && ctx.web,
            When::BellowsSkillsAuth => skills && ctx.web && ctx.auth,
            When::BellowsSkillsAgents => skills && ctx.agents,
            When::BellowsSkillsHallmark => skills && ctx.hallmark,
            When::BellowsSkillsAnvil => skills && ctx.anvil,
            When::BellowsSkillsSearch => skills && ctx.search,
            When::BellowsGuidelines => guidelines,
            When::BellowsMcp => mcp,
            _ => false,
        };
        if !wanted || !template.fits_kit(&ctx) {
            continue;
        }
        let rel = templates::out_path(template, &ctx)?;
        let path = dir.join(&rel);
        if template.when == When::BellowsMcp && path.exists() {
            changes.push(add_server(&path, &rel)?);
            continue;
        }
        if path.exists() {
            changes.push(Change::Kept(rel));
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        crate::files::create_new(
            &path,
            templates::render(template, &ctx)?.as_bytes(),
            crate::files::Mode::Default,
        )
        .with_context(|| format!("cannot write {rel}"))?;
        changes.push(Change::Created(rel));
    }
    Ok(changes)
}

/// Adds the `smeltery` server to an existing `.mcp.json` when it has an `mcpServers` object without it. The text
/// is changed only by inserting the entry after `"mcpServers": {`, so the rest of the file stays as it was.
fn add_server(path: &Path, rel: &str) -> anyhow::Result<Change> {
    let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {rel}"))?;
    let by_hand = |why: &str| {
        Change::ByHand(format!(
            "{rel}: {why}; add this entry to its \"mcpServers\" object by hand:\n    {}",
            SERVER_ENTRY.replace("\n  ", "\n")
        ))
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(by_hand("it is not valid JSON"));
    };
    let Some(servers) = json
        .get("mcpServers")
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(by_hand("it has no \"mcpServers\" object"));
    };
    if servers.contains_key("smeltery") {
        return Ok(Change::Kept(rel.to_owned()));
    }
    let Some(key) = text.find("\"mcpServers\"") else {
        return Ok(by_hand(
            "its \"mcpServers\" key is written in an unusual way",
        ));
    };
    let Some(brace) = text
        .get(key..)
        .and_then(|t| t.find('{'))
        .map(|i| key + i + 1)
    else {
        return Ok(by_hand("its \"mcpServers\" object was not found"));
    };
    let separator = if servers.is_empty() { "" } else { "," };
    let mut updated = text.clone();
    updated.insert_str(brace, &format!("\n    {SERVER_ENTRY}{separator}"));
    let valid = serde_json::from_str::<serde_json::Value>(&updated)
        .ok()
        .is_some_and(|v| v.pointer("/mcpServers/smeltery/command").is_some());
    if !valid {
        return Ok(by_hand("the entry could not be inserted safely"));
    }
    crate::files::replace(path, updated.as_bytes())
        .with_context(|| format!("cannot write {rel}"))?;
    Ok(Change::Updated(rel.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::new::{Bellows, Db, NewOptions, Shape, generate};

    fn app(kind: Shape) -> (tempfile::TempDir, std::path::PathBuf) {
        app_with(kind, kind.has_web())
    }

    fn app_with(kind: Shape, auth: bool) -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| unreachable!("{e}"));
        let root = tmp.path().join("shop");
        let opts = NewOptions {
            kind: kind.kind(),
            db: Db::Sqlite,
            blocks: kind.blocks(auth),
            ..NewOptions::defaults("shop")
        };
        generate(&opts, &root, None, "base64:k", 1_791_028_800)
            .unwrap_or_else(|e| unreachable!("{e:#}"));
        (tmp, root)
    }

    fn read(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| unreachable!("{rel}: {e}"))
    }

    /// The Hallmark and Anvil skills and guideline lines (D-467, D-430): `bellows:install` finds the blocks in the
    /// app and writes what `smeltery new` writes with them.
    #[test]
    fn installs_the_hallmark_and_anvil_parts_of_new() {
        use crate::new::{Block, Blocks};
        for blocks in [
            Blocks::default().with(Block::Hallmark).with(Block::Anvil),
            Blocks::NONE.with(Block::Anvil),
            Blocks::default(),
        ] {
            let opts = |bellows| NewOptions {
                blocks,
                bellows,
                ..NewOptions::defaults("shop")
            };
            let tmp = tempfile::tempdir().unwrap_or_else(|e| unreachable!("{e}"));
            let later = tmp.path().join("later");
            let fresh = tmp.path().join("fresh");
            generate(
                &opts(Bellows::default()),
                &later,
                None,
                "base64:k",
                1_791_028_800,
            )
            .unwrap_or_else(|e| unreachable!("{e:#}"));
            let all = Bellows {
                mcp: true,
                skills: true,
                guidelines: true,
            };
            generate(&opts(all), &fresh, None, "base64:k", 1_791_028_800)
                .unwrap_or_else(|e| unreachable!("{e:#}"));
            install(&later, true, true, true).unwrap_or_else(|e| unreachable!("{e:#}"));
            for rel in [
                ".bellows/guidelines.md",
                ".bellows/skills/api-token.md",
                ".bellows/skills/broadcast.md",
            ] {
                assert_eq!(
                    later.join(rel).is_file(),
                    fresh.join(rel).is_file(),
                    "{blocks} {rel}"
                );
                if fresh.join(rel).is_file() {
                    assert_eq!(read(&later, rel), read(&fresh, rel), "{blocks} {rel}");
                }
            }
            assert_eq!(
                fresh.join(".bellows/skills/api-token.md").is_file(),
                blocks.hallmark
            );
            assert_eq!(
                fresh.join(".bellows/skills/broadcast.md").is_file(),
                blocks.anvil
            );
        }
    }

    #[test]
    fn installs_the_same_files_as_new_and_is_idempotent() {
        for (kind, auth) in [
            (Shape::WebWatchfire, true),
            (Shape::Headless, false),
            (Shape::Web, true),
            (Shape::Web, false),
        ] {
            let (_a, later) = app_with(kind, auth);
            let first = install(&later, true, true, true).unwrap_or_else(|e| unreachable!("{e:#}"));
            assert!(
                first.iter().all(|c| matches!(c, Change::Created(_))),
                "{first:?}"
            );
            // The same bytes as choosing everything at `smeltery new`.
            let tmp = tempfile::tempdir().unwrap_or_else(|e| unreachable!("{e}"));
            let fresh = tmp.path().join("shop");
            let opts = NewOptions {
                kind: kind.kind(),
                db: Db::Sqlite,
                blocks: kind.blocks(auth),
                bellows: Bellows {
                    mcp: true,
                    skills: true,
                    guidelines: true,
                },
                ..NewOptions::defaults("shop")
            };
            generate(&opts, &fresh, None, "base64:k", 1_791_028_800)
                .unwrap_or_else(|e| unreachable!("{e:#}"));
            for change in &first {
                let Change::Created(rel) = change else {
                    unreachable!()
                };
                assert_eq!(read(&later, rel), read(&fresh, rel), "{kind:?} {rel}");
            }
            // The auth skill only where the authentication scaffolding is (D-232).
            assert_eq!(
                later.join(".bellows/skills/auth-route.md").is_file(),
                kind.has_web() && auth,
                "{kind:?} auth {auth}"
            );
            let again = install(&later, true, true, true).unwrap_or_else(|e| unreachable!("{e:#}"));
            assert!(
                again.iter().all(|c| matches!(c, Change::Kept(_))),
                "{again:?}"
            );
        }
    }

    #[test]
    fn never_overwrites_and_adds_the_server_to_an_existing_mcp_json() {
        let (_a, root) = app(Shape::Web);
        std::fs::create_dir_all(root.join(".bellows")).unwrap_or_else(|e| unreachable!("{e}"));
        std::fs::write(root.join(".bellows/guidelines.md"), "mine\n")
            .unwrap_or_else(|e| unreachable!("{e}"));
        let other = "{\n  \"mcpServers\": {\n    \"other\": { \"command\": \"x\" }\n  }\n}\n";
        std::fs::write(root.join(".mcp.json"), other).unwrap_or_else(|e| unreachable!("{e}"));
        let changes = install(&root, true, false, true).unwrap_or_else(|e| unreachable!("{e:#}"));
        assert_eq!(
            changes,
            [
                Change::Kept(".bellows/guidelines.md".into()),
                Change::Updated(".mcp.json".into())
            ]
        );
        assert_eq!(read(&root, ".bellows/guidelines.md"), "mine\n");
        let mcp: serde_json::Value =
            serde_json::from_str(&read(&root, ".mcp.json")).unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(mcp["mcpServers"]["other"]["command"], "x");
        assert_eq!(mcp["mcpServers"]["smeltery"]["args"][0], "bellows:mcp");
        assert!(
            read(&root, ".mcp.json").contains("\"other\": { \"command\": \"x\" }"),
            "the rest is untouched"
        );
        // Second run: nothing changes.
        assert_eq!(
            install(&root, true, false, false).ok(),
            Some(vec![Change::Kept(".mcp.json".into())])
        );
        // An empty servers object and a broken file.
        std::fs::write(root.join(".mcp.json"), "{\"mcpServers\": {}}")
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert_eq!(
            install(&root, true, false, false).ok(),
            Some(vec![Change::Updated(".mcp.json".into())])
        );
        assert!(read(&root, ".mcp.json").contains("bellows:mcp"));
        std::fs::write(root.join(".mcp.json"), "{ broken").unwrap_or_else(|e| unreachable!("{e}"));
        let changes = install(&root, true, false, false).unwrap_or_else(|e| unreachable!("{e:#}"));
        assert!(matches!(changes.as_slice(), [Change::ByHand(m)] if m.contains("not valid JSON")));
        assert_eq!(read(&root, ".mcp.json"), "{ broken");
    }

    /// A React or Vue app (read from its `Cargo.toml`) gets the kit's guides, as at `smeltery new`.
    #[test]
    fn kit_apps_get_the_kit_guides() {
        use crate::new::Frontend;
        for frontend in [Frontend::React, Frontend::Vue] {
            let opts = |bellows| NewOptions {
                kind: Shape::WebWatchfire.kind(),
                blocks: Shape::WebWatchfire.blocks(true),
                db: Db::Sqlite,
                frontend: Some(frontend),
                bellows,
                ..NewOptions::defaults("shop")
            };
            let tmp = tempfile::tempdir().unwrap_or_else(|e| unreachable!("{e}"));
            let later = tmp.path().join("shop");
            generate(
                &opts(Bellows::default()),
                &later,
                None,
                "base64:k",
                1_791_028_800,
            )
            .unwrap_or_else(|e| unreachable!("{e:#}"));
            let changes =
                install(&later, true, true, true).unwrap_or_else(|e| unreachable!("{e:#}"));
            let fresh = tmp.path().join("fresh");
            let all = Bellows {
                mcp: true,
                skills: true,
                guidelines: true,
            };
            generate(&opts(all), &fresh, None, "base64:k", 1_791_028_800)
                .unwrap_or_else(|e| unreachable!("{e:#}"));
            for change in &changes {
                let Change::Created(rel) = change else {
                    unreachable!("{change:?}")
                };
                assert_eq!(read(&later, rel), read(&fresh, rel), "{frontend:?} {rel}");
            }
            assert!(later.join(".bellows/skills/alloy-page.md").is_file());
            assert!(!later.join(".bellows/skills/spark.md").exists());
        }
    }

    #[test]
    fn headless_apps_get_only_the_skills_that_apply() {
        let (_a, root) = app(Shape::Headless);
        install(&root, false, true, false).unwrap_or_else(|e| unreachable!("{e:#}"));
        assert!(root.join(".bellows/skills/migration.md").is_file());
        assert!(root.join(".bellows/skills/agent.md").is_file());
        assert!(!root.join(".bellows/skills/spark.md").exists());
        assert!(!root.join(".bellows/skills/crud-resource.md").exists());
    }
}
