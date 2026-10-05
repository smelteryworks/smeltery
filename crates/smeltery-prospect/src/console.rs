//! `prospect:status`, `prospect:import`, `prospect:flush`.

use std::sync::Arc;

use smeltery_core::console::{Args, Command, Output};
use smeltery_core::{App, Error, Result};

use crate::settings::Driver;
use crate::spec::Resolved;
use crate::{Prospect, driver};

/// The registered models named by `args` (table name or index name, any letter case; none: every model).
fn chosen(prospect: &Prospect, args: &Args) -> Result<Vec<Arc<Resolved>>> {
    let mut all: Vec<Arc<Resolved>> = prospect.inner.models.values().cloned().collect();
    all.sort_by(|a, b| a.table.cmp(b.table));
    let names = args.positional();
    if names.is_empty() {
        return Ok(all);
    }
    let mut out = Vec::new();
    for name in names {
        let found = all
            .iter()
            .find(|s| s.table.eq_ignore_ascii_case(name) || s.index.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                let known: Vec<&str> = all.iter().map(|s| s.table).collect();
                Error::internal(format!(
                    "no searchable model has the table `{}`; registered: {}",
                    printable(name),
                    if known.is_empty() {
                        "none".to_owned()
                    } else {
                        known.join(", ")
                    }
                ))
            })?;
        out.push(Arc::clone(found));
    }
    Ok(out)
}

/// Terminal-safe text: control and bidi characters become U+FFFD.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}') {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// `prospect:status`: the driver and each searchable model's index.
pub(crate) struct Status;

impl Command for Status {
    fn name(&self) -> &'static str {
        "prospect:status"
    }

    fn about(&self) -> &'static str {
        "Show the search driver and each searchable model's index"
    }

    async fn run(&self, app: &App, args: Args) -> Result<()> {
        self.run_with_output(app, args, Output::default()).await
    }

    async fn run_with_output(&self, app: &App, _args: Args, out: Output) -> Result<()> {
        let prospect = Prospect::of(app)?;
        out.line(format!("Driver: {}", prospect.driver().name()));
        let statuses = prospect.statuses().await;
        if statuses.is_empty() {
            out.line("No searchable models are registered.");
        }
        for (spec, state, rows) in statuses {
            let columns: Vec<&str> = spec.texts.iter().map(|(c, _)| c.as_str()).collect();
            let index = match (prospect.driver(), state) {
                (Driver::Memory, _) => format!(
                    "memory engine, {} documents",
                    prospect.memory().len(&spec.index)
                ),
                (_, Ok(None)) => "index present".to_owned(),
                (_, Ok(Some(reason))) => format!("index MISSING: {reason}"),
                (_, Err(e)) => format!("index not checked: {e}"),
            };
            out.line(format!(
                "{}: {} rows, text {}; {}",
                spec.table,
                rows.map_or_else(|| "?".to_owned(), |n| n.to_string()),
                columns.join(", "),
                index
            ));
        }
        Ok(())
    }
}

/// `prospect:import [table…]`: fill the indexes from their tables.
pub(crate) struct Import;

impl Command for Import {
    fn name(&self) -> &'static str {
        "prospect:import"
    }

    fn about(&self) -> &'static str {
        "Fill search indexes from their tables (SQLite: rebuild the FTS5 index)"
    }

    async fn run(&self, app: &App, args: Args) -> Result<()> {
        self.run_with_output(app, args, Output::default()).await
    }

    async fn run_with_output(&self, app: &App, args: Args, out: Output) -> Result<()> {
        let prospect = Prospect::of(app)?;
        let db = prospect.db()?;
        for spec in chosen(&prospect, &args)? {
            match prospect.driver() {
                Driver::Database => {
                    if db.backend() == smeltery_core::db::Backend::Sqlite {
                        let rows = driver::database::rebuild(&db, &spec).await?;
                        out.line(format!(
                            "{}: rebuilt the index over {rows} rows.",
                            spec.table
                        ));
                    } else {
                        out.line(format!(
                            "{}: the database keeps this index current; nothing to import.",
                            spec.table
                        ));
                    }
                }
                Driver::Memory => out.line(format!(
                    "{}: the memory engine is filled by model events and `Prospect::import`.",
                    spec.table
                )),
            }
        }
        Ok(())
    }
}

/// `prospect:flush table`: remove every document of a model from the engine.
pub(crate) struct Flush;

impl Command for Flush {
    fn name(&self) -> &'static str {
        "prospect:flush"
    }

    fn about(&self) -> &'static str {
        "Remove every document of a model from the search engine (not for the database driver)"
    }

    async fn run(&self, app: &App, args: Args) -> Result<()> {
        self.run_with_output(app, args, Output::default()).await
    }

    async fn run_with_output(&self, app: &App, args: Args, out: Output) -> Result<()> {
        let prospect = Prospect::of(app)?;
        if args.positional().is_empty() {
            return Err(Error::internal(
                "name the model's table: prospect:flush posts",
            ));
        }
        for spec in chosen(&prospect, &args)? {
            prospect.flush_spec(&spec)?;
            out.line(format!("{}: flushed.", spec.table));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn printable_neutralises_terminal_escapes() {
        assert_eq!(
            super::printable("a\u{1b}[31mb\u{202E}"),
            "a\u{FFFD}[31mb\u{FFFD}"
        );
    }
}
