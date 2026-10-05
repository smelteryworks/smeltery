//! The database layer against real databases: SQLite here (in memory and in a temp file),
//! PostgreSQL and MySQL when `DATABASE_URL_PG` / `DATABASE_URL_MYSQL` point at a server.
#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use smeltery_core::config::Settings;
use smeltery_core::console::{Args, Command, Commands, dispatch};
use smeltery_core::db::migration::{Migration, MigrationStatus, Migrator, Schema};
use smeltery_core::db::seed::{Seeder, Seeders};
use smeltery_core::db::{Backend, Db};
use smeltery_core::{App, AppBuilder, Result};

struct CreateUsers;

impl Migration for CreateUsers {
    fn name(&self) -> &'static str {
        "2026_10_03_000001_create_users_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("users", |t| {
                t.id();
                t.string("name");
                t.string("email").unique();
                t.timestamps();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("users").await
    }
}

struct CreatePosts;

impl Migration for CreatePosts {
    fn name(&self) -> &'static str {
        "2026_10_03_000002_create_posts_table"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("posts", |t| {
                t.id();
                t.foreign_id("user_id")
                    .constrained("users")
                    .cascade_on_delete();
                t.string("title").index();
                t.text("body").nullable();
                t.boolean("published").default(false);
                t.timestamps();
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop("posts").await
    }
}

struct AddViewsToPosts;

impl Migration for AddViewsToPosts {
    fn name(&self) -> &'static str {
        "2026_10_03_000003_add_views_to_posts"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .table("posts", |t| {
                t.integer("views").default(0);
            })
            .await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.raw("ALTER TABLE posts DROP COLUMN views").await
    }
}

/// Creates a table, then fails: with transactional DDL nothing of it remains.
struct Broken;

impl Migration for Broken {
    fn name(&self) -> &'static str {
        "2026_10_03_000004_broken"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("broken", |t| {
                t.id();
            })
            .await?;
        schema.raw("THIS IS NOT SQL").await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        schema.drop_if_exists("broken").await
    }
}

fn base() -> Migrator {
    let mut m = Migrator::new();
    m.add(CreateUsers).add(CreatePosts);
    m
}

fn with_views() -> Migrator {
    let mut m = base();
    m.add(AddViewsToPosts);
    m
}

fn status(list: &[MigrationStatus]) -> Vec<(String, Option<i32>)> {
    list.iter().map(|s| (s.name.clone(), s.batch)).collect()
}

async fn has_table(db: &Db, table: &str) -> bool {
    Schema::new(db).has_table(table).await.unwrap()
}

/// The whole migrator story on one database.
async fn migrator_suite(db: Db) {
    // A fresh start: drop whatever an earlier run left, run the base set.
    let ran = base().fresh(&db).await.unwrap();
    assert_eq!(
        ran,
        [
            "2026_10_03_000001_create_users_table",
            "2026_10_03_000002_create_posts_table"
        ]
    );
    assert!(base().migrate(&db).await.unwrap().is_empty(), "idempotent");

    // A second batch.
    let m = with_views();
    assert_eq!(
        status(&m.status(&db).await.unwrap()),
        [
            ("2026_10_03_000001_create_users_table".into(), Some(1)),
            ("2026_10_03_000002_create_posts_table".into(), Some(1)),
            ("2026_10_03_000003_add_views_to_posts".into(), None),
        ]
    );
    assert_eq!(
        m.migrate(&db).await.unwrap(),
        ["2026_10_03_000003_add_views_to_posts"]
    );
    db.execute("INSERT INTO users (name, email) VALUES ('Ada', 'ada@example.com')")
        .await
        .unwrap();
    db.execute("INSERT INTO posts (user_id, title, views) VALUES (1, 'Hi', 3)")
        .await
        .unwrap();

    // Roll back the last batch, then by steps.
    assert_eq!(
        m.rollback(&db, None).await.unwrap(),
        ["2026_10_03_000003_add_views_to_posts"]
    );
    assert_eq!(
        status(&m.status(&db).await.unwrap())[2],
        ("2026_10_03_000003_add_views_to_posts".into(), None)
    );
    assert_eq!(
        m.migrate(&db).await.unwrap(),
        ["2026_10_03_000003_add_views_to_posts"]
    );
    assert_eq!(
        m.rollback(&db, Some(2)).await.unwrap(),
        [
            "2026_10_03_000003_add_views_to_posts",
            "2026_10_03_000002_create_posts_table"
        ]
    );
    assert!(!has_table(&db, "posts").await);
    assert!(has_table(&db, "users").await);
    assert_eq!(
        status(&m.status(&db).await.unwrap()),
        [
            ("2026_10_03_000001_create_users_table".into(), Some(1)),
            ("2026_10_03_000002_create_posts_table".into(), None),
            ("2026_10_03_000003_add_views_to_posts".into(), None),
        ]
    );
    assert_eq!(m.migrate(&db).await.unwrap().len(), 2);
    assert_eq!(
        status(&m.status(&db).await.unwrap())[2],
        ("2026_10_03_000003_add_views_to_posts".into(), Some(2))
    );

    // A failing migration leaves no record (and, with transactional DDL, no table).
    let mut broken = with_views();
    broken.add(Broken);
    let err = broken.migrate(&db).await.unwrap_err().to_string();
    assert!(
        err.contains("migrating `2026_10_03_000004_broken` failed"),
        "{err}"
    );
    assert_eq!(
        status(&broken.status(&db).await.unwrap())[3],
        ("2026_10_03_000004_broken".into(), None)
    );
    if db.backend().transactional_ddl() {
        assert!(!has_table(&db, "broken").await);
    }

    // Fresh drops every table, including ones with rows pointing at each other and
    // tables no migration knows.
    db.execute("INSERT INTO posts (user_id, title) VALUES (1, 'Again')")
        .await
        .unwrap();
    db.execute("CREATE TABLE stray (id INTEGER)").await.unwrap();
    assert_eq!(m.fresh(&db).await.unwrap().len(), 3);
    assert!(!has_table(&db, "stray").await);
    assert!(!has_table(&db, "broken").await);
    let users = db.execute("DELETE FROM users").await.unwrap();
    assert_eq!(users, 0, "fresh tables are empty");

    // A recorded migration that is no longer registered cannot be rolled back.
    let err = base().rollback(&db, None).await.unwrap_err().to_string();
    assert!(err.contains("not registered"), "{err}");
    let listed = status(&base().status(&db).await.unwrap());
    assert_eq!(
        listed[2],
        ("2026_10_03_000003_add_views_to_posts".into(), Some(1))
    );

    // Clean up for the next run.
    m.rollback(&db, None).await.unwrap();
    assert_eq!(smeltery_core::db::migration::MIGRATIONS_TABLE, "migrations");
}

#[tokio::test]
async fn migrator_on_sqlite_in_memory() {
    let db = Db::connect("sqlite::memory:").await.unwrap();
    assert_eq!(db.backend(), Backend::Sqlite);
    migrator_suite(db).await;
}

#[tokio::test]
async fn migrator_on_sqlite_file() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("app.sqlite").display());
    migrator_suite(Db::connect(&url).await.unwrap()).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "needs DATABASE_URL_PG"]
async fn migrator_on_postgres() {
    let url = std::env::var("DATABASE_URL_PG").expect("DATABASE_URL_PG");
    let db = Db::connect(&url).await.unwrap();
    assert_eq!(db.backend(), Backend::Postgres);
    migrator_suite(db).await;
}

#[cfg(feature = "mysql")]
#[tokio::test]
#[ignore = "needs DATABASE_URL_MYSQL"]
async fn migrator_on_mysql() {
    let url = std::env::var("DATABASE_URL_MYSQL").expect("DATABASE_URL_MYSQL");
    let db = Db::connect(&url).await.unwrap();
    assert_eq!(db.backend(), Backend::MySql);
    migrator_suite(db).await;
}

/// A pooled SQLite connection whose copy of the schema is stale (another connection dropped a table since) still
/// creates the table again through `Schema`, outside any migration.
#[tokio::test]
async fn schema_changes_outside_migrations_see_other_connections_drops() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        dir.path()
            .join("app.sqlite")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    let a = Db::connect(&url).await.unwrap();
    let mut one = smeltery_core::db::DbOptions::default();
    one.pool_max = 1;
    let b = Db::connect_with(&url, one).await.unwrap();
    let define = |t: &mut smeltery_core::db::migration::Blueprint| {
        t.id();
        t.string("agent").index();
    };
    Schema::new(&a).create("runs", define).await.unwrap();
    // B's only connection reads the table: its copy of the schema now has `runs` and its index.
    b.execute("SELECT count(*) FROM runs").await.unwrap();
    Schema::new(&a).drop_if_exists("runs").await.unwrap();
    Schema::new(&b).create("runs", define).await.unwrap();
    assert!(Schema::new(&a).has_table("runs").await.unwrap());
    Schema::new(&b).drop("runs").await.unwrap();
    Schema::new(&a).create("runs", define).await.unwrap();
}

#[tokio::test]
async fn duplicate_migration_names_are_refused() {
    let db = Db::connect("sqlite::memory:").await.unwrap();
    let mut m = base();
    m.add(CreateUsers);
    let err = m.migrate(&db).await.unwrap_err().to_string();
    assert!(err.contains("two migrations are named"), "{err}");
}

// ---- seeders ---------------------------------------------------------------------------

struct UserSeeder;

impl Seeder for UserSeeder {
    async fn run(&self, db: &Db) -> Result<()> {
        let email = smeltery_core::db::factory::Fake::next().unique_email();
        db.execute(&format!(
            "INSERT INTO users (name, email) VALUES ('Ada', '{email}')"
        ))
        .await?;
        Ok(())
    }
}

struct PostSeeder;

impl Seeder for PostSeeder {
    async fn run(&self, db: &Db) -> Result<()> {
        db.execute("INSERT INTO posts (user_id, title) VALUES (1, 'Hello')")
            .await?;
        Ok(())
    }
}

fn seeders(s: &mut Seeders) {
    s.add(UserSeeder).add(PostSeeder);
}

async fn count(db: &Db, table: &str) -> i64 {
    use smeltery_core::db::prelude::*;
    let row = db
        .conn()
        .query_one_raw(sea_orm::Statement::from_string(
            db.conn().get_database_backend(),
            format!("SELECT COUNT(*) AS n FROM {table}"),
        ))
        .await
        .unwrap()
        .unwrap();
    row.try_get::<i64>("", "n").unwrap()
}

#[tokio::test]
async fn seeders_run_in_order_or_by_class() {
    let db = Db::connect("sqlite::memory:").await.unwrap();
    base().migrate(&db).await.unwrap();
    let mut s = Seeders::new();
    seeders(&mut s);
    assert_eq!(s.names(), ["UserSeeder", "PostSeeder"]);
    assert_eq!(
        s.run(&db, None).await.unwrap(),
        ["UserSeeder", "PostSeeder"]
    );
    assert_eq!(
        s.run(&db, Some("UserSeeder")).await.unwrap(),
        ["UserSeeder"]
    );
    assert_eq!(count(&db, "users").await, 2);
    assert_eq!(count(&db, "posts").await, 1);
    let err = s.run(&db, Some("Nope")).await.unwrap_err().to_string();
    assert!(err.contains("no seeder named `Nope`"), "{err}");
}

// ---- console ---------------------------------------------------------------------------

/// A user command that records its arguments.
struct Greet(Arc<Mutex<Vec<String>>>);

impl Command for Greet {
    fn name(&self) -> &'static str {
        "greet"
    }

    fn about(&self) -> &'static str {
        "Say hello"
    }

    async fn run(&self, app: &App, args: Args) -> Result<()> {
        let users = count(&app.db()?, "users").await;
        let mut seen = self.0.lock().unwrap();
        seen.push(format!(
            "{} loud={} users={users}",
            args.get(0).unwrap_or("world"),
            args.flag("loud")
        ));
        Ok(())
    }
}

struct Console {
    _dir: tempfile::TempDir,
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Console {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("app.sqlite").display());
        Self {
            _dir: dir,
            url,
            seen: Arc::default(),
        }
    }

    fn builder(&self, env: &str) -> AppBuilder {
        let mut builder = AppBuilder::new(Settings::from_env());
        builder.settings_mut().database_url = self.url.clone();
        builder.settings_mut().env = env.to_owned();
        let seen = Arc::clone(&self.seen);
        builder
            .migrations(|m| {
                m.add(CreateUsers).add(CreatePosts);
            })
            .seeders(seeders)
            .commands(move |c: &mut Commands| {
                c.add(Greet(seen));
            })
    }

    async fn run_in(&self, env: &str, args: &[&str]) -> (ExitCode, String) {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let mut out = Vec::new();
        let code = dispatch(self.builder(env), &args, &mut out).await.unwrap();
        (code, String::from_utf8(out).unwrap())
    }

    async fn run(&self, args: &[&str]) -> (ExitCode, String) {
        self.run_in("local", args).await
    }
}

#[tokio::test]
async fn console_runs_the_database_commands() {
    let c = Console::new();
    let (code, out) = c.run(&["migrate:status"]).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        "STATUS   BATCH  MIGRATION\n\
         Pending         2026_10_03_000001_create_users_table\n\
         Pending         2026_10_03_000002_create_posts_table\n"
    );
    let (_, out) = c.run(&["migrate"]).await;
    assert_eq!(
        out,
        "Migrated: 2026_10_03_000001_create_users_table\n\
         Migrated: 2026_10_03_000002_create_posts_table\n"
    );
    let (_, out) = c.run(&["migrate"]).await;
    assert_eq!(out, "Nothing to migrate.\n");
    let (_, out) = c.run(&["migrate:status"]).await;
    assert!(
        out.contains("Ran      1      2026_10_03_000002_create_posts_table"),
        "{out}"
    );

    let (_, out) = c.run(&["db:seed"]).await;
    assert_eq!(out, "Seeded: UserSeeder\nSeeded: PostSeeder\n");
    let (_, out) = c.run(&["db:seed", "--class", "UserSeeder"]).await;
    assert_eq!(out, "Seeded: UserSeeder\n");

    let (_, out) = c.run(&["migrate:rollback", "--step", "1"]).await;
    assert_eq!(out, "Rolled back: 2026_10_03_000002_create_posts_table\n");
    let (_, out) = c.run(&["migrate:rollback"]).await;
    assert_eq!(out, "Rolled back: 2026_10_03_000001_create_users_table\n");
    let (_, out) = c.run(&["migrate:rollback"]).await;
    assert_eq!(out, "Nothing to roll back.\n");

    let (_, out) = c.run(&["migrate:fresh", "--seed"]).await;
    assert_eq!(
        out,
        "Dropped all tables.\n\
         Migrated: 2026_10_03_000001_create_users_table\n\
         Migrated: 2026_10_03_000002_create_posts_table\n\
         Seeded: UserSeeder\n\
         Seeded: PostSeeder\n"
    );

    // A user command, with its arguments and the app's database.
    let (code, out) = c.run(&["greet", "Ada", "--loud"]).await;
    assert_eq!((code, out.as_str()), (ExitCode::SUCCESS, ""));
    assert_eq!(*c.seen.lock().unwrap(), ["Ada loud=true users=1"]);

    // A bad --step is an error, not a full rollback.
    let mut out = Vec::new();
    let err = dispatch(
        c.builder("local"),
        &["migrate:rollback".into(), "--step".into(), "x".into()],
        &mut out,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("--step must be a number"));
}

#[tokio::test]
async fn production_needs_force() {
    let c = Console::new();
    for command in ["migrate", "migrate:rollback", "migrate:fresh", "db:seed"] {
        let (code, out) = c.run_in("production", &[command]).await;
        assert_eq!(code, ExitCode::FAILURE, "{command}");
        assert!(out.contains("--force"), "{out}");
    }
    let (_, out) = c.run_in("production", &["migrate:status"]).await;
    assert!(out.contains("Pending"), "status is read-only: {out}");
    let (code, out) = c.run_in("production", &["migrate", "--force"]).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("Migrated: 2026_10_03_000002_create_posts_table"));
}

#[tokio::test]
async fn help_lists_built_ins_and_app_commands() {
    let c = Console::new();
    let (code, out) = c.run(&["help"]).await;
    assert_eq!(code, ExitCode::SUCCESS);
    for name in [
        "serve",
        "route:list",
        "migrate",
        "migrate:rollback",
        "migrate:fresh",
        "migrate:status",
        "db:seed",
        "help",
    ] {
        assert!(out.contains(&format!("  {name} ")), "{name} in {out}");
    }
    assert!(out.contains("App commands:\n  greet"), "{out}");
    assert!(out.contains("Say hello"));
    let (code, out) = c.run(&["nope"]).await;
    assert_eq!(code, ExitCode::from(2));
    assert!(out.contains("unknown command `nope`") && out.contains("greet"));
}

#[tokio::test]
async fn database_commands_need_a_database() {
    let mut out = Vec::new();
    let mut builder = AppBuilder::new(Settings::from_env());
    // Outside production: a missing APP_ENV means production, where `migrate` asks for --force first.
    builder.settings_mut().env = "local".into();
    builder.settings_mut().database_url = String::new();
    let err = dispatch(builder, &["migrate".into()], &mut out)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("set DATABASE_URL"), "{err}");
}

#[tokio::test]
async fn boot_fails_clearly_when_the_database_is_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    let mut builder = AppBuilder::new(Settings::from_env());
    // The directory does not exist, so SQLite cannot create the file.
    builder.settings_mut().database_url = format!(
        "sqlite://{}",
        dir.path().join("missing/dir/app.sqlite").display()
    );
    let err = builder.build().await.unwrap_err().to_string();
    assert!(err.contains("cannot connect to the database"), "{err}");
}

/// Every connection of a file pool loads the schema, then one connection adds a column and
/// another drops it. SQLite reports `DROP COLUMN` / `RENAME COLUMN` of a column missing from
/// a connection's cached schema without re-reading the schema (sqlite3.c
/// `sqlite3AlterDropColumn`), so each migration must start from a fresh schema.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_file_pool_migrations_see_the_current_schema() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("pool.sqlite").display());
    let db = Db::connect_with(&url, smeltery_core::db::DbOptions::default().pool_max(4))
        .await
        .unwrap();
    let m = with_views();
    base().migrate(&db).await.unwrap();
    for round in 0..20 {
        // Hold four connections at once so every pooled connection caches the schema.
        let mut readers = tokio::task::JoinSet::new();
        for _ in 0..4 {
            let db = db.clone();
            readers.spawn(async move {
                use smeltery_core::db::prelude::*;
                let txn = db.conn().begin().await.unwrap();
                txn.execute_unprepared("SELECT count(*) FROM posts")
                    .await
                    .unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                txn.commit().await.unwrap();
            });
        }
        while let Some(done) = readers.join_next().await {
            done.unwrap();
        }
        m.migrate(&db).await.unwrap();
        let rolled = m
            .rollback(&db, Some(1))
            .await
            .unwrap_or_else(|e| panic!("round {round}: {e}"));
        assert_eq!(rolled, ["2026_10_03_000003_add_views_to_posts"]);
    }
}

/// `Blueprint::drop_column` on a live database: the column and its framework index go, the other columns and the
/// rows stay. The 2FA columns' `down` is this shape.
async fn drop_column_suite(db: Db) {
    let schema = Schema::new(&db);
    schema.drop_if_exists("drop_col_people").await.unwrap();
    schema
        .create("drop_col_people", |t| {
            t.id();
            t.string("name");
        })
        .await
        .unwrap();
    schema
        .table("drop_col_people", |t| {
            t.text("secret").nullable();
            t.string("code").nullable().index();
        })
        .await
        .unwrap();
    db.execute("INSERT INTO drop_col_people (name, secret, code) VALUES ('Ada', 's', 'c')")
        .await
        .unwrap();
    schema
        .table("drop_col_people", |t| {
            t.drop_column("secret");
            t.drop_column("code");
        })
        .await
        .unwrap();
    // The columns are gone; the row and the other column stay.
    assert!(
        db.execute("UPDATE drop_col_people SET secret = 'x'")
            .await
            .is_err()
    );
    assert!(
        db.execute("UPDATE drop_col_people SET code = 'x'")
            .await
            .is_err()
    );
    assert_eq!(
        db.execute("UPDATE drop_col_people SET name = 'Bea'")
            .await
            .unwrap(),
        1
    );
    // Adding them back works (the index name is free again).
    schema
        .table("drop_col_people", |t| {
            t.string("code").nullable().index();
        })
        .await
        .unwrap();
    schema.drop("drop_col_people").await.unwrap();
}

#[tokio::test]
async fn drop_column_on_sqlite() {
    drop_column_suite(Db::connect("sqlite::memory:").await.unwrap()).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "needs DATABASE_URL_PG"]
async fn drop_column_on_postgres() {
    let url = std::env::var("DATABASE_URL_PG").expect("DATABASE_URL_PG");
    drop_column_suite(Db::connect(&url).await.unwrap()).await;
}

#[cfg(feature = "mysql")]
#[tokio::test]
#[ignore = "needs DATABASE_URL_MYSQL"]
async fn drop_column_on_mysql() {
    let url = std::env::var("DATABASE_URL_MYSQL").expect("DATABASE_URL_MYSQL");
    drop_column_suite(Db::connect(&url).await.unwrap()).await;
}
