//! [`SearchIndex`]: the migration helper that creates and drops a table's full-text index for the database driver.

use smeltery_core::Result;
use smeltery_core::db::Backend;
use smeltery_core::db::migration::Schema;

use crate::driver::database::{PG_COLUMN, index_name, pg_index_name, quote};
use crate::error::ProspectError;
use crate::spec::valid_name;
pub use crate::spec::{Language, Weight};

/// The full-text index of one table, for a migration. It is standalone (a migration is history and does not read
/// today's model code): name the table, its text columns in the model's `text` order, their weights and the language.
///
/// ```
/// use smeltery::Result;
/// use smeltery::db::migration::Schema;
/// use smeltery::prospect::migration::{Language, SearchIndex, Weight};
///
/// async fn up(schema: &Schema) -> Result<()> {
///     schema.create("posts", |t| { t.id(); t.string("title"); t.text("body"); t.timestamps(); }).await?;
///     SearchIndex::on("posts")
///         .text("title", Weight::A)
///         .text("body", Weight::B)
///         .language(Language::Simple)
///         .create(schema)
///         .await
/// }
///
/// async fn down(schema: &Schema) -> Result<()> {
///     SearchIndex::on("posts").drop(schema).await?;
///     schema.drop_if_exists("posts").await
/// }
/// ```
///
/// | Backend | `create` | `drop` |
/// |---|---|---|
/// | SQLite | the FTS5 table `<table>_search` (external content, `content_rowid` = the key), the triggers `<table>_search_ai` / `_ad` / `_au` that keep it current, and a `rebuild` that indexes the rows already there | the triggers and the table |
/// | PostgreSQL | the stored generated column `search_vector` (`tsvector`, weighted) and the GIN index `<table>_search_index` | the index and the column |
/// | MySQL / MariaDB | the `FULLTEXT` index `<table>_search` over the text columns | the index |
///
/// Names are `[A-Za-z_][A-Za-z0-9_]*`, at most 48 characters; anything else is an error before any statement.
#[derive(Clone, Debug)]
pub struct SearchIndex {
    table: String,
    key: String,
    texts: Vec<(String, Weight)>,
    language: Language,
}

impl SearchIndex {
    /// The index of `table` (its integer key column is `id`).
    pub fn on(table: &str) -> Self {
        Self {
            table: table.to_owned(),
            key: "id".to_owned(),
            texts: Vec::new(),
            language: Language::Simple,
        }
    }

    /// The integer key column, when it is not `id`.
    #[must_use]
    pub fn key(mut self, column: &str) -> Self {
        self.key = column.to_owned();
        self
    }

    /// A searched column and its weight, in the model's `text` order.
    #[must_use]
    pub fn text(mut self, column: &str, weight: Weight) -> Self {
        self.texts.push((column.to_owned(), weight));
        self
    }

    /// How words are compared (default [`Language::Simple`]).
    #[must_use]
    pub fn language(mut self, language: Language) -> Self {
        self.language = language;
        self
    }

    fn check(&self, need_texts: bool) -> std::result::Result<(), ProspectError> {
        let names = std::iter::once(&self.table)
            .chain(std::iter::once(&self.key))
            .chain(self.texts.iter().map(|(c, _)| c));
        for name in names {
            if !valid_name(name) {
                return Err(ProspectError::Migration(format!(
                    "`{name}` is not a valid name for a search index: use ASCII letters, digits and `_` (at most 48)"
                )));
            }
        }
        if need_texts && self.texts.is_empty() {
            return Err(ProspectError::Migration(format!(
                "the search index of `{}` needs at least one `.text(…)` column",
                self.table
            )));
        }
        Ok(())
    }

    /// The statements `create` runs on `backend`.
    pub fn create_sql(&self, backend: Backend) -> Result<Vec<String>> {
        self.check(true)?;
        let q = |name: &str| quote(backend, name);
        let table = q(&self.table);
        let texts: Vec<String> = self.texts.iter().map(|(c, _)| q(c)).collect();
        Ok(match backend {
            Backend::Sqlite => {
                let index = q(&index_name(&self.table));
                let raw_index = index_name(&self.table);
                let cols = texts.join(", ");
                let new: Vec<String> = texts.iter().map(|c| format!("new.{c}")).collect();
                let old: Vec<String> = texts.iter().map(|c| format!("old.{c}")).collect();
                let key = q(&self.key);
                vec![
                    format!(
                        "CREATE VIRTUAL TABLE {index} USING fts5({cols}, content='{table_raw}', content_rowid='{key_raw}', \
                         tokenize='{tokenize}')",
                        table_raw = self.table,
                        key_raw = self.key,
                        tokenize = self.language.fts5_tokenizer()
                    ),
                    format!(
                        "CREATE TRIGGER {ai} AFTER INSERT ON {table} BEGIN \
                         INSERT INTO {index}(rowid, {cols}) VALUES (new.{key}, {new}); END",
                        ai = q(&format!("{raw_index}_ai")),
                        new = new.join(", ")
                    ),
                    format!(
                        "CREATE TRIGGER {ad} AFTER DELETE ON {table} BEGIN \
                         INSERT INTO {index}({index}, rowid, {cols}) VALUES ('delete', old.{key}, {old}); END",
                        ad = q(&format!("{raw_index}_ad")),
                        old = old.join(", ")
                    ),
                    // The key too: a row whose key changes leaves its old rowid and is indexed under the new
                    // one, so a later row given the old key never matches the old words.
                    format!(
                        "CREATE TRIGGER {au} AFTER UPDATE OF {key}, {cols} ON {table} BEGIN \
                         INSERT INTO {index}({index}, rowid, {cols}) VALUES ('delete', old.{key}, {old}); \
                         INSERT INTO {index}(rowid, {cols}) VALUES (new.{key}, {new}); END",
                        au = q(&format!("{raw_index}_au")),
                        old = old.join(", "),
                        new = new.join(", ")
                    ),
                    format!("INSERT INTO {index}({index}) VALUES('rebuild')"),
                ]
            }
            Backend::Postgres => {
                let config = self.language.pg_config();
                let parts: Vec<String> = self
                    .texts
                    .iter()
                    .map(|(c, w)| {
                        format!(
                            "setweight(to_tsvector('{config}', coalesce({}, '')), '{}')",
                            q(c),
                            w.letter()
                        )
                    })
                    .collect();
                vec![
                    format!(
                        "ALTER TABLE {table} ADD COLUMN {col} tsvector GENERATED ALWAYS AS ({}) STORED",
                        parts.join(" || "),
                        col = q(PG_COLUMN)
                    ),
                    format!(
                        "CREATE INDEX {} ON {table} USING GIN ({})",
                        q(&pg_index_name(&self.table)),
                        q(PG_COLUMN)
                    ),
                ]
            }
            _ => vec![format!(
                "CREATE FULLTEXT INDEX {} ON {table} ({})",
                q(&index_name(&self.table)),
                texts.join(", ")
            )],
        })
    }

    /// The statements `drop` runs on `backend`.
    pub fn drop_sql(&self, backend: Backend) -> Result<Vec<String>> {
        self.check(false)?;
        let q = |name: &str| quote(backend, name);
        let raw_index = index_name(&self.table);
        Ok(match backend {
            Backend::Sqlite => vec![
                format!("DROP TRIGGER IF EXISTS {}", q(&format!("{raw_index}_ai"))),
                format!("DROP TRIGGER IF EXISTS {}", q(&format!("{raw_index}_ad"))),
                format!("DROP TRIGGER IF EXISTS {}", q(&format!("{raw_index}_au"))),
                format!("DROP TABLE IF EXISTS {}", q(&raw_index)),
            ],
            Backend::Postgres => vec![
                format!("DROP INDEX IF EXISTS {}", q(&pg_index_name(&self.table))),
                format!(
                    "ALTER TABLE {} DROP COLUMN IF EXISTS {}",
                    q(&self.table),
                    q(PG_COLUMN)
                ),
            ],
            _ => vec![format!(
                "DROP INDEX {} ON {}",
                q(&raw_index),
                q(&self.table)
            )],
        })
    }

    /// Create the index (see the table above). Inside a migration on SQLite and PostgreSQL the statements share the
    /// migration's transaction; MySQL commits each on its own, as for every schema change there. It fails when an
    /// index (or a table or column) of that name already exists.
    ///
    /// # Errors
    /// An invalid name, no text column, or a statement fails.
    pub async fn create(&self, schema: &Schema) -> Result<()> {
        for sql in self.create_sql(schema.backend())? {
            schema.raw(&sql).await?;
        }
        Ok(())
    }

    /// Drop the index when it exists.
    ///
    /// # Errors
    /// An invalid name, or a statement fails.
    pub async fn drop(&self, schema: &Schema) -> Result<()> {
        let mysql = schema.backend() == Backend::MySql;
        for sql in self.drop_sql(schema.backend())? {
            match schema.raw(&sql).await {
                // MySQL has no `DROP INDEX IF EXISTS`: error 1091 says the index is not there.
                Err(e) if mysql && e.to_string().contains("1091") => {}
                other => other?,
            }
        }
        Ok(())
    }

    /// Drop and create the index again (a migration that changes its columns, weights or language). On SQLite the
    /// new index is filled from the table.
    ///
    /// # Errors
    /// See [`create`](Self::create).
    pub async fn rebuild(&self, schema: &Schema) -> Result<()> {
        self.drop(schema).await?;
        self.create(schema).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posts() -> SearchIndex {
        SearchIndex::on("posts")
            .text("title", Weight::A)
            .text("body", Weight::B)
    }

    #[test]
    fn sqlite_ddl_is_an_external_content_table_with_triggers() {
        let sql = posts().create_sql(Backend::Sqlite).unwrap();
        assert_eq!(
            sql[0],
            "CREATE VIRTUAL TABLE \"posts_search\" USING fts5(\"title\", \"body\", content='posts', content_rowid='id', \
             tokenize='unicode61 remove_diacritics 2')"
        );
        assert!(sql[1].contains("AFTER INSERT ON \"posts\""));
        assert!(sql[2].contains("VALUES ('delete', old.\"id\", old.\"title\", old.\"body\")"));
        assert!(sql[3].starts_with(
            "CREATE TRIGGER \"posts_search_au\" AFTER UPDATE OF \"id\", \"title\", \"body\" ON \"posts\""
        ));
        assert_eq!(
            sql[4],
            "INSERT INTO \"posts_search\"(\"posts_search\") VALUES('rebuild')"
        );
        let english = posts()
            .language(Language::English)
            .create_sql(Backend::Sqlite)
            .unwrap();
        assert!(english[0].contains("tokenize='porter unicode61 remove_diacritics 2'"));
    }

    #[test]
    fn postgres_and_mysql_ddl() {
        let pg = posts().create_sql(Backend::Postgres).unwrap();
        assert_eq!(
            pg[0],
            "ALTER TABLE \"posts\" ADD COLUMN \"search_vector\" tsvector GENERATED ALWAYS AS \
             (setweight(to_tsvector('simple', coalesce(\"title\", '')), 'A') || \
             setweight(to_tsvector('simple', coalesce(\"body\", '')), 'B')) STORED"
        );
        assert_eq!(
            pg[1],
            "CREATE INDEX \"posts_search_index\" ON \"posts\" USING GIN (\"search_vector\")"
        );
        assert_eq!(
            posts().create_sql(Backend::MySql).unwrap(),
            ["CREATE FULLTEXT INDEX `posts_search` ON `posts` (`title`, `body`)"]
        );
        assert_eq!(
            posts().drop_sql(Backend::Postgres).unwrap()[1],
            "ALTER TABLE \"posts\" DROP COLUMN IF EXISTS \"search_vector\""
        );
    }

    #[test]
    fn names_are_checked_before_any_statement() {
        for bad in ["posts\"; DROP TABLE x; --", "a b", "", "1x"] {
            assert!(
                SearchIndex::on(bad)
                    .text("t", Weight::A)
                    .create_sql(Backend::Sqlite)
                    .is_err()
            );
            assert!(
                SearchIndex::on("posts")
                    .text(bad, Weight::A)
                    .create_sql(Backend::Sqlite)
                    .is_err()
            );
        }
        assert!(
            SearchIndex::on("posts")
                .create_sql(Backend::Sqlite)
                .is_err()
        );
        assert!(SearchIndex::on("posts").drop_sql(Backend::Sqlite).is_ok());
    }
}
