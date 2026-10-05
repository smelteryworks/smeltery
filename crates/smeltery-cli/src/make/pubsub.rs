//! `pubsub:install`: the migration of the `pubsub_messages` table (PubSub's `database` driver) for an app.

use anyhow::bail;

use super::{Ctx, Plan, render::unique_stamp};

/// The migration file's name, after its stamp.
const SUFFIX: &str = "_create_pubsub_messages_table.rs";

/// The plan: `database/migrations/m<stamp>_create_pubsub_messages_table.rs` and its registration. Refuses when the
/// app has that migration already.
pub(crate) fn install(ctx: Ctx<'_>) -> anyhow::Result<Plan> {
    if let Ok(entries) = std::fs::read_dir(ctx.root.join("database").join("migrations")) {
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if file.starts_with('m') && file.ends_with(SUFFIX) {
                bail!("database/migrations/{file} already exists; nothing was changed");
            }
        }
    }
    let stamp = unique_stamp(ctx);
    let module = format!("m{stamp}_create_pubsub_messages_table");
    let contents = format!(
        "//! Create the `pubsub_messages` table, used when `PUBSUB_DRIVER` is `database` (or `auto` chooses it).\n\
         \n\
         use smeltery::Result;\n\
         use smeltery::db::migration::{{Migration, Schema}};\n\
         \n\
         /// Creates `pubsub_messages`.\n\
         pub struct CreatePubsubMessagesTable;\n\
         \n\
         impl Migration for CreatePubsubMessagesTable {{\n\
         \x20   fn name(&self) -> &'static str {{\n\
         \x20       \"{stamp}_create_pubsub_messages_table\"\n\
         \x20   }}\n\
         \n\
         \x20   async fn up(&self, schema: &Schema) -> Result<()> {{\n\
         \x20       smeltery::pubsub::migrations::up(schema).await\n\
         \x20   }}\n\
         \n\
         \x20   async fn down(&self, schema: &Schema) -> Result<()> {{\n\
         \x20       smeltery::pubsub::migrations::down(schema).await\n\
         \x20   }}\n\
         }}\n"
    );
    let mut plan = Plan::default();
    plan.file(format!("database/migrations/{module}.rs"), contents);
    plan.insert(
        "database/migrations/mod.rs",
        "// smeltery:mods",
        format!("pub mod {module};"),
    );
    plan.insert(
        "database/migrations/mod.rs",
        "// smeltery:migrations",
        format!("m.add({module}::CreatePubsubMessagesTable);"),
    );
    plan.notes.push(
        "Run `smeltery migrate` to create the table. Processes use it under PUBSUB_DRIVER=database, and under \
         `auto` in `serve --no-agents` and `work` when CACHE_STORE is not redis."
            .to_owned(),
    );
    Ok(plan)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn writes_the_migration_and_registers_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let migrations = dir.path().join("database/migrations");
        std::fs::create_dir_all(&migrations).unwrap();
        std::fs::write(
            migrations.join("mod.rs"),
            "// smeltery:mods\n\npub fn register(m: &mut Migrator) {\n    // smeltery:migrations\n}\n",
        )
        .unwrap();
        // 2026-10-05 12:00:00 UTC.
        let ctx = Ctx {
            root: dir.path(),
            now: 1_791_201_600,
        };
        let plan = install(ctx).unwrap();
        assert_eq!(plan.files.len(), 1);
        let (path, contents) = &plan.files[0];
        assert_eq!(
            path,
            "database/migrations/m2026_10_05_120000_create_pubsub_messages_table.rs"
        );
        assert!(contents.contains("\"2026_10_05_120000_create_pubsub_messages_table\""));
        assert!(contents.contains("smeltery::pubsub::migrations::up(schema).await"));
        assert!(contents.contains("    async fn down(&self, schema: &Schema) -> Result<()> {\n"));
        super::super::apply(dir.path(), &plan).unwrap();
        let registry = std::fs::read_to_string(migrations.join("mod.rs")).unwrap();
        assert!(
            registry.contains(
                "pub mod m2026_10_05_120000_create_pubsub_messages_table;\n// smeltery:mods"
            ),
            "{registry}"
        );
        assert!(
            registry.contains(
                "m.add(m2026_10_05_120000_create_pubsub_messages_table::CreatePubsubMessagesTable);"
            ),
            "{registry}"
        );
        // A second run changes nothing.
        let err = install(ctx).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
    }
}
