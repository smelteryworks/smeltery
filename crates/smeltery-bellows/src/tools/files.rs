//! `config_keys` and `last_errors`: files of the app, read without exposing secrets.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::Outcome;
use crate::Options;

/// The settings the framework reads (core, sessions, auth, cache, Watchfire, Bellows), by name.
const FRAMEWORK_KEYS: &[&str] = &[
    "APP_NAME",
    "APP_ENV",
    "APP_DEBUG",
    "APP_URL",
    "APP_KEY",
    "SERVER_HOST",
    "SERVER_PORT",
    "LOG_LEVEL",
    "LOG_FILE",
    "LOG_MAX_BYTES",
    "REQUEST_TIMEOUT",
    "BODY_LIMIT",
    "SHUTDOWN_TIMEOUT",
    "SMELTERY_ROOT",
    "DATABASE_URL",
    "TEST_DATABASE_URL",
    "DB_POOL_MAX",
    "DB_CONNECT_TIMEOUT",
    "SESSION_DRIVER",
    "SESSION_LIFETIME",
    "SESSION_COOKIE",
    "AUTH_HOME",
    "CACHE_STORE",
    "CACHE_PREFIX",
    "CACHE_PATH",
    "CACHE_TABLE",
    "CACHE_MEMORY_CAPACITY",
    "CACHE_TIMEOUT",
    "TEST_CACHE_STORE",
    "REDIS_URL",
    "MEMCACHED_SERVERS",
    "PUBSUB_DRIVER",
    "PUBSUB_POLL_MS",
    "TEST_PUBSUB_DRIVER",
    "QUEUE_DRIVER",
    "QUEUE_PREFIX",
    "WATCHFIRE_MAX_CONCURRENT",
    "WATCHFIRE_WORKERS",
    "WATCHFIRE_JOB_TIMEOUT",
    "WATCHFIRE_HTTP_TIMEOUT",
    "WATCHFIRE_STORE_TIMEOUT",
    "WATCHFIRE_ALERT_WEBHOOK",
    "WATCHFIRE_ALERT_MAIL",
    "WATCHFIRE_DASHBOARD",
    "WATCHFIRE_API_ADDR",
    "WATCHFIRE_IN_SERVE",
    "WATCHFIRE_LOCK_STORE",
    "WATCHFIRE_LEASE_TTL",
    "BELLOWS_TEST_TIMEOUT",
];

/// The key names in a `.env`-style file. Values are never kept.
fn names(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line
                .strip_prefix("export ")
                .map(str::trim_start)
                .unwrap_or(line);
            let (key, _value) = line.split_once('=')?;
            let key = key.trim();
            let valid = !key.is_empty()
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !key.starts_with(|c: char| c.is_ascii_digit());
            valid.then(|| key.to_owned())
        })
        .collect()
}

pub(super) fn config_keys(root: &Path) -> Outcome {
    let mut keys: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for file in [".env", ".env.example"] {
        if let Ok(text) = std::fs::read_to_string(root.join(file)) {
            for key in names(&text) {
                let sources = keys.entry(key).or_default();
                if !sources.contains(&file) {
                    sources.push(file);
                }
            }
        }
    }
    for key in FRAMEWORK_KEYS {
        keys.entry((*key).to_owned()).or_default().push("framework");
    }
    let list: Vec<_> = keys
        .into_iter()
        .map(|(name, sources)| json!({ "name": name, "in": sources }))
        .collect();
    Outcome::json(&json!({
        "keys": list,
        "note": "names only; values are never shown. Settings are read with smeltery::config::env(\"KEY\", default); the real environment overrides .env.",
    }))
}

/// The log files to read, oldest first: the configured `LOG_FILE` (its `.1` backup, then the file) when it
/// exists, else `storage/logs/*.log` (each with its backup).
fn log_files(options: &Options) -> Vec<PathBuf> {
    if let Some(file) = &options.log_file {
        let files: Vec<PathBuf> = [backup(file), file.clone()]
            .into_iter()
            .filter(|p| p.is_file())
            .collect();
        if !files.is_empty() {
            return files;
        }
    }
    let dir = options.root.join("storage").join("logs");
    let mut files: Vec<(String, bool, PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_owned();
            let (base, current) = match name.strip_suffix(".1") {
                Some(base) => (base.to_owned(), false),
                None => (name, true),
            };
            base.ends_with(".log").then_some((base, current, p))
        })
        .collect();
    // By name, each backup before its file; only the last `MAX_LOG_FILES` (dated names sort oldest first).
    files.sort();
    let skip = files.len().saturating_sub(MAX_LOG_FILES);
    files.into_iter().skip(skip).map(|(_, _, p)| p).collect()
}

/// The most files `last_errors` reads from `storage/logs/`.
const MAX_LOG_FILES: usize = 20;

/// The longest log line `last_errors` returns; longer lines end in `…`.
const MAX_LOG_LINE: usize = 2_000;

/// `line` cut to `MAX_LOG_LINE` characters.
fn shorten(line: String) -> String {
    match line.char_indices().nth(MAX_LOG_LINE) {
        Some((end, _)) => format!("{}…", line.get(..end).unwrap_or_default()),
        None => line,
    }
}

fn backup(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// Whether `line` is logged at one of `levels`: the level is one of the first words (after the timestamp in
/// the framework's format).
fn has_level(line: &str, levels: &[&str]) -> bool {
    line.split_whitespace()
        .take(3)
        .any(|word| levels.contains(&word))
}

pub(super) fn last_errors(options: &Options, limit: usize, warnings: bool) -> Outcome {
    let files = log_files(options);
    if files.is_empty() {
        return Outcome::ok(
            "no log file: LOG_FILE is not set (or the file does not exist yet) and storage/logs/ has no *.log \
             files. Set LOG_FILE=storage/logs/smeltery.log in .env to keep the app's log in a file; otherwise \
             errors from `smeltery serve` show in its terminal.",
        );
    }
    let levels: &[&str] = if warnings {
        &["ERROR", "WARN"]
    } else {
        &["ERROR"]
    };
    let mut lines: Vec<String> = Vec::new();
    for file in &files {
        // Only the end of each file: logs grow without bound.
        let Some(text) = read_tail(file, 512 * 1024) else {
            continue;
        };
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        lines.extend(
            text.lines()
                .map(strip_ansi)
                .filter(|l| has_level(l, levels))
                .map(|l| shorten(format!("{name}: {l}"))),
        );
    }
    let start = lines.len().saturating_sub(limit);
    let tail = lines.get(start..).unwrap_or_default();
    if tail.is_empty() {
        let what = if warnings { "ERROR or WARN" } else { "ERROR" };
        let names: Vec<String> = files
            .iter()
            .filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        return Outcome::ok(format!("no {what} lines in {}", names.join(", ")));
    }
    // Log lines can carry anything a request put there: the whole answer is capped like process output.
    Outcome::ok(super::process::tail(&tail.join("\n")))
}

fn read_tail(path: &Path, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(max))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Drops terminal colour codes (`ESC [ … m`).
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_names_without_values() {
        let text = "# comment\nAPP_KEY=base64:secret\nexport DB_PASSWORD=\"hunter2\"\n\nBAD LINE\n1X=2\nEMPTY=\n";
        assert_eq!(names(text), ["APP_KEY", "DB_PASSWORD", "EMPTY"]);
    }

    #[test]
    fn levels_are_words_near_the_start() {
        let line = "2026-10-04T10:00:00.000000Z ERROR app: payment failed";
        assert!(has_level(line, &["ERROR"]));
        assert!(has_level(
            "  WARN smeltery::log: dropped",
            &["ERROR", "WARN"]
        ));
        assert!(!has_level(
            "2026-10-04T10:00:00Z  INFO app: no ERROR here",
            &["ERROR"]
        ));
    }

    #[test]
    fn ansi_codes_are_stripped() {
        assert_eq!(strip_ansi("\u{1b}[31mERROR\u{1b}[0m boom"), "ERROR boom");
    }
}
