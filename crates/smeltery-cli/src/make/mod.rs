//! The `make:*` generators: new files from embedded templates, registration lines at markers.
//!
//! A generator builds a [`Plan`] (files to create, lines to insert, notes) without touching the disk, then
//! [`apply`] writes it: it refuses when any target file exists, creates the files, and inserts lines only above
//! their marker comments. A missing marker prints the exact line to add by hand; nothing else in the file changes.

mod args;
mod fields;
pub(crate) mod hallmark;
mod names;
pub(crate) mod prospect;
pub(crate) mod pubsub;
mod render;

use std::path::Path;

use anyhow::{Context as _, bail};

pub(crate) use args::{
    AgentArgs, CommandArgs, ControllerArgs, FactoryArgs, JobArgs, MailArgs, MiddlewareArgs,
    MigrationArgs, ModelArgs, PageArgs, SeederArgs, SparkArgs,
};

use crate::generator::{Inserted, insert_before_marker};

/// What a generator does to an app.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// New files: path relative to the app root, contents.
    pub(crate) files: Vec<(String, String)>,
    /// Lines to insert above markers.
    pub(crate) inserts: Vec<Insert>,
    /// Hints printed at the end.
    pub(crate) notes: Vec<String>,
}

/// One line (or block) to insert above a marker.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Insert {
    pub(crate) file: String,
    pub(crate) marker: &'static str,
    pub(crate) line: String,
}

impl Plan {
    fn file(&mut self, path: impl Into<String>, contents: String) {
        self.files.push((path.into(), contents));
    }

    fn insert(&mut self, file: &str, marker: &'static str, line: impl Into<String>) {
        self.inserts.push(Insert {
            file: file.to_owned(),
            marker,
            line: line.into(),
        });
    }

    fn extend(&mut self, other: Plan) {
        self.files.extend(other.files);
        self.inserts.extend(other.inserts);
        self.notes.extend(other.notes);
    }
}

/// Inputs every generator shares: the app root and the clock (seconds since the Unix epoch).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ctx<'a> {
    pub(crate) root: &'a Path,
    pub(crate) now: u64,
}

impl Ctx<'_> {
    /// The current clock, for migration file names.
    pub(crate) fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    }
}

/// `YYYY_MM_DD_HHMMSS` (UTC) for `secs` since the Unix epoch.
pub(crate) fn stamp(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or_default();
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}_{month:02}_{day:02}_{:02}{:02}{:02}",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// Writes `plan` into the app at `root`. Refuses (changing nothing) when a file to create exists, a dangling symlink
/// included; new files are created with `create_new`, so nothing is ever written through a link.
pub(crate) fn apply(root: &Path, plan: &Plan) -> anyhow::Result<()> {
    for (rel, _) in &plan.files {
        // `symlink_metadata`, not `exists()`: a dangling symlink "does not exist" but would be written through.
        if std::fs::symlink_metadata(root.join(rel)).is_ok() {
            bail!("{rel} already exists; nothing was changed");
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for (rel, _) in &plan.files {
        if !seen.insert(rel) {
            bail!("{rel} would be written twice; nothing was changed");
        }
    }
    for (rel, contents) in &plan.files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        // `create_new`: an entry that appeared since the check above is never written through or replaced.
        crate::files::create_new(&path, contents.as_bytes(), crate::files::Mode::Default)
            .with_context(|| format!("cannot write {rel}"))?;
        println!("created {rel}");
    }
    format_created(root, plan);
    let mut updated = std::collections::BTreeSet::new();
    for ins in &plan.inserts {
        let path = root.join(&ins.file);
        match insert_before_marker(&path, ins.marker, &ins.line) {
            Ok(Inserted::Added) => {
                if updated.insert(ins.file.as_str()) {
                    println!("updated {}", ins.file);
                }
            }
            Ok(Inserted::AlreadyThere) => {}
            Err(_) => {
                eprintln!(
                    "warning: {}: marker `{}` not found; add this by hand:\n{}",
                    ins.file,
                    ins.marker,
                    indent(&ins.line, "    ")
                );
            }
        }
    }
    for note in &plan.notes {
        println!("{note}");
    }
    Ok(())
}

/// Runs `rustfmt --edition 2024` (or `RUSTFMT`) on the Rust files the plan created, from the app root so the app's
/// `rustfmt.toml` applies: templates cannot lay out every width (a long model name changes where rustfmt breaks a
/// line). Files the plan only added lines to are left alone (the user's code). Without rustfmt, or when it fails, the
/// files stay as written and a note says to run `cargo fmt`; the generator never fails because of it.
// The created files declare no modules (`mod x;`), which rustfmt would follow into the user's files; a test checks it.
fn format_created(root: &Path, plan: &Plan) {
    let files: Vec<&str> = plan
        .files
        .iter()
        .map(|(rel, _)| rel.as_str())
        .filter(|rel| rel.ends_with(".rs"))
        .collect();
    if files.is_empty() {
        return;
    }
    let rustfmt = std::env::var_os("RUSTFMT")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "rustfmt".into());
    if let Err(failed) = rustfmt_files(root, &files, &rustfmt) {
        println!("note: the new files were not formatted (rustfmt: {failed}); run `cargo fmt`");
    }
}

/// `rustfmt --edition 2024 <files>` in `root`; the reason when it cannot run or fails.
fn rustfmt_files(root: &Path, files: &[&str], rustfmt: &std::ffi::OsStr) -> Result<(), String> {
    let output = std::process::Command::new(rustfmt)
        .args(["--edition", "2024"])
        .args(files)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output();
    match output {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or("it failed")
            .to_owned()),
        Err(e) => Err(e.to_string()),
    }
}

fn indent(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|l| format!("{prefix}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs a generator in the app at `cwd` with the real clock.
pub(crate) fn run(
    cwd: &Path,
    plan: impl FnOnce(Ctx<'_>) -> anyhow::Result<Plan>,
) -> anyhow::Result<()> {
    crate::cmd::require_app(cwd)?;
    let plan = plan(Ctx {
        root: cwd,
        now: Ctx::now_secs(),
    })?;
    apply(cwd, &plan)
}

pub(crate) use hallmark::install as hallmark_install;
pub(crate) use prospect::install as prospect_install;
pub(crate) use pubsub::install as pubsub_install;
pub(crate) use render::{
    agent, command, controller, factory, job, mail, middleware, migration, model, page, seeder,
    spark,
};

#[cfg(test)]
mod tests;
