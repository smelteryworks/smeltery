//! `route_list`, `models` and `db_schema`: what the app and its database contain.

use std::path::Path;

use serde_json::{Value, json};
use smeltery_core::App;
use smeltery_core::db::prelude::sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement};

use super::Outcome;

/// Tables the framework owns; `models` leaves them out.
const FRAMEWORK_TABLES: &[&str] = &["migrations", "sessions", "password_reset_tokens"];

pub(super) fn route_list(app: &App) -> Outcome {
    let routes: Vec<Value> = app
        .routes()
        .iter()
        .map(|r| {
            json!({
                "methods": r.methods,
                "path": r.path,
                "name": r.name,
                "middleware": r.middleware,
            })
        })
        .collect();
    Outcome::json(&json!({ "routes": routes }))
}

/// One table's description.
#[derive(Debug, Default)]
struct Table {
    name: String,
    columns: Vec<Value>,
    indexes: Vec<Value>,
}

impl Table {
    fn to_json(&self) -> Value {
        json!({ "name": self.name, "columns": self.columns, "indexes": self.indexes })
    }
}

pub(super) async fn db_schema(app: &App, only: Option<&str>) -> Outcome {
    match read_schema(app, only).await {
        Ok(tables) => {
            if let Some(name) = only
                && tables.is_empty()
            {
                return Outcome::error(format!("no table named `{name}`"));
            }
            Outcome::json(
                &json!({ "tables": tables.iter().map(Table::to_json).collect::<Vec<_>>() }),
            )
        }
        Err(e) => Outcome::error(e),
    }
}

pub(super) async fn models(app: &App, root: &Path) -> Outcome {
    let dir = root.join("app").join("models");
    let mut files: Vec<(String, Option<String>)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
            if path.extension().is_none_or(|e| e != "rs") || stem == "mod" {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap_or_default();
            files.push((stem, table_name(&source)));
        }
    }
    files.sort();
    let tables = read_schema(app, None).await;
    let mut out = Vec::new();
    for (stem, table) in &files {
        let columns = match (&tables, table) {
            (Ok(tables), Some(t)) => tables
                .iter()
                .find(|x| &x.name == t)
                .map(|x| x.columns.clone()),
            _ => None,
        };
        out.push(json!({
            "model": stem,
            "file": format!("app/models/{stem}.rs"),
            "table": table,
            "columns": columns,
        }));
    }
    let mut answer = serde_json::Map::new();
    answer.insert("models".to_owned(), json!(out));
    match &tables {
        Ok(tables) => {
            let modelled: Vec<&str> = files.iter().filter_map(|(_, t)| t.as_deref()).collect();
            let other: Vec<&str> = tables
                .iter()
                .map(|t| t.name.as_str())
                .filter(|n| !modelled.contains(n))
                .filter(|n| !FRAMEWORK_TABLES.contains(n) && !n.starts_with("watchfire_"))
                .collect();
            answer.insert("tables_without_model".to_owned(), json!(other));
        }
        Err(e) => {
            answer.insert("database".to_owned(), json!(e));
        }
    }
    Outcome::json(&Value::Object(answer))
}

/// `#[sea_orm(table_name = "posts")]` in a model file.
fn table_name(source: &str) -> Option<String> {
    let at = source.find("table_name")?;
    let rest = source.get(at..)?;
    let start = rest.find('"')? + 1;
    let len = rest.get(start..)?.find('"')?;
    rest.get(start..start + len).map(str::to_owned)
}

async fn read_schema(app: &App, only: Option<&str>) -> Result<Vec<Table>, String> {
    let db = app.db().map_err(|e| format!("no database: {e}"))?;
    let conn = db.conn();
    let backend = conn.get_database_backend();
    let names_sql = match backend {
        // Ordinary and virtual tables (an FTS5 search index is one table), not the shadow tables a virtual table
        // stores its data in: `PRAGMA table_list` types those `shadow` (SQLite 3.37+).
        DbBackend::Sqlite => {
            "SELECT name AS name FROM pragma_table_list WHERE schema = 'main' \
             AND type IN ('table', 'virtual') AND name NOT LIKE 'sqlite_%' ORDER BY name"
        }
        DbBackend::Postgres => {
            "SELECT table_name::text AS name FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_type = 'BASE TABLE' ORDER BY table_name"
        }
        _ => {
            "SELECT CAST(table_name AS CHAR) AS name FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE' ORDER BY table_name"
        }
    };
    let rows = query(conn, backend, names_sql, vec![]).await?;
    let mut tables = Vec::new();
    for row in rows {
        let name: String = get(&row, "name").unwrap_or_default();
        if only.is_some_and(|o| o != name) {
            continue;
        }
        let table = match backend {
            DbBackend::Sqlite => sqlite_table(conn, &name).await?,
            DbBackend::Postgres => postgres_table(conn, &name).await?,
            _ => mysql_table(conn, &name).await?,
        };
        tables.push(table);
    }
    Ok(tables)
}

async fn query(
    conn: &impl ConnectionTrait,
    backend: DbBackend,
    sql: &str,
    values: Vec<smeltery_core::db::prelude::sea_orm::Value>,
) -> Result<Vec<QueryResult>, String> {
    conn.query_all_raw(Statement::from_sql_and_values(backend, sql, values))
        .await
        .map_err(|e| format!("schema query failed: {e}"))
}

fn get<T: smeltery_core::db::prelude::sea_orm::TryGetable>(
    row: &QueryResult,
    col: &str,
) -> Option<T> {
    row.try_get::<T>("", col).ok()
}

/// SQLite quotes identifiers with `"`; a table name read from `sqlite_master` is escaped.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

async fn sqlite_table(conn: &impl ConnectionTrait, name: &str) -> Result<Table, String> {
    let cols = query(
        conn,
        DbBackend::Sqlite,
        &format!("PRAGMA table_info({})", quote(name)),
        vec![],
    )
    .await?;
    let columns = cols
        .iter()
        .map(|r| {
            json!({
                "name": get::<String>(r, "name"),
                "type": get::<String>(r, "type"),
                "nullable": get::<i64>(r, "notnull") == Some(0),
                "default": get::<String>(r, "dflt_value"),
                "primary_key": get::<i64>(r, "pk").unwrap_or(0) > 0,
            })
        })
        .collect();
    let list = query(
        conn,
        DbBackend::Sqlite,
        &format!("PRAGMA index_list({})", quote(name)),
        vec![],
    )
    .await?;
    let mut indexes = Vec::new();
    for r in &list {
        let index: String = get(r, "name").unwrap_or_default();
        let info = query(
            conn,
            DbBackend::Sqlite,
            &format!("PRAGMA index_info({})", quote(&index)),
            vec![],
        )
        .await?;
        let columns: Vec<String> = info
            .iter()
            .filter_map(|c| get::<String>(c, "name"))
            .collect();
        indexes.push(json!({
            "name": index,
            "columns": columns,
            "unique": get::<i64>(r, "unique") == Some(1),
        }));
    }
    Ok(Table {
        name: name.to_owned(),
        columns,
        indexes,
    })
}

async fn postgres_table(conn: &impl ConnectionTrait, name: &str) -> Result<Table, String> {
    let cols = query(
        conn,
        DbBackend::Postgres,
        "SELECT c.column_name::text AS name, c.data_type::text AS type, c.is_nullable::text AS nullable, \
         c.column_default::text AS dflt, \
         EXISTS (SELECT 1 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage k ON tc.constraint_name = k.constraint_name \
                  AND tc.table_schema = k.table_schema \
                 WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = c.table_schema \
                   AND tc.table_name = c.table_name AND k.column_name = c.column_name) AS pk \
         FROM information_schema.columns c \
         WHERE c.table_schema = current_schema() AND c.table_name = $1 ORDER BY c.ordinal_position",
        vec![name.into()],
    )
    .await?;
    let columns = cols
        .iter()
        .map(|r| {
            json!({
                "name": get::<String>(r, "name"),
                "type": get::<String>(r, "type"),
                "nullable": get::<String>(r, "nullable").as_deref() == Some("YES"),
                "default": get::<String>(r, "dflt"),
                "primary_key": get::<bool>(r, "pk").unwrap_or(false),
            })
        })
        .collect();
    let idx = query(
        conn,
        DbBackend::Postgres,
        "SELECT indexname::text AS name, indexdef::text AS def FROM pg_indexes \
         WHERE schemaname = current_schema() AND tablename = $1 ORDER BY indexname",
        vec![name.into()],
    )
    .await?;
    let indexes = idx
        .iter()
        .map(|r| {
            let def = get::<String>(r, "def").unwrap_or_default();
            json!({
                "name": get::<String>(r, "name"),
                "definition": def,
                "unique": def.contains("UNIQUE"),
            })
        })
        .collect();
    Ok(Table {
        name: name.to_owned(),
        columns,
        indexes,
    })
}

async fn mysql_table(conn: &impl ConnectionTrait, name: &str) -> Result<Table, String> {
    let cols = query(
        conn,
        DbBackend::MySql,
        "SELECT CAST(column_name AS CHAR) AS name, CAST(column_type AS CHAR) AS type, \
         CAST(is_nullable AS CHAR) AS nullable, CAST(column_default AS CHAR) AS dflt, \
         CAST(column_key AS CHAR) AS colkey FROM information_schema.columns \
         WHERE table_schema = DATABASE() AND table_name = ? ORDER BY ordinal_position",
        vec![name.into()],
    )
    .await?;
    let columns = cols
        .iter()
        .map(|r| {
            json!({
                "name": get::<String>(r, "name"),
                "type": get::<String>(r, "type"),
                "nullable": get::<String>(r, "nullable").as_deref() == Some("YES"),
                "default": get::<String>(r, "dflt"),
                "primary_key": get::<String>(r, "colkey").as_deref() == Some("PRI"),
            })
        })
        .collect();
    let idx = query(
        conn,
        DbBackend::MySql,
        "SELECT CAST(index_name AS CHAR) AS name, CAST(column_name AS CHAR) AS col, \
         CAST(non_unique AS SIGNED) AS non_unique FROM information_schema.statistics \
         WHERE table_schema = DATABASE() AND table_name = ? ORDER BY index_name, seq_in_index",
        vec![name.into()],
    )
    .await?;
    let mut indexes: Vec<Value> = Vec::new();
    for r in &idx {
        let index = get::<String>(r, "name").unwrap_or_default();
        let col = get::<String>(r, "col").unwrap_or_default();
        if let Some(existing) = indexes.iter_mut().find(|i| i["name"] == index.as_str()) {
            if let Some(cols) = existing["columns"].as_array_mut() {
                cols.push(json!(col));
            }
        } else {
            indexes.push(json!({
                "name": index,
                "columns": [col],
                "unique": get::<i64>(r, "non_unique") == Some(0),
            }));
        }
    }
    Ok(Table {
        name: name.to_owned(),
        columns,
        indexes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_names_come_from_the_model_attribute() {
        assert_eq!(
            table_name("#[sea_orm(table_name = \"posts\")]\npub struct Model {}").as_deref(),
            Some("posts")
        );
        assert_eq!(table_name("pub struct Model {}"), None);
    }
}
