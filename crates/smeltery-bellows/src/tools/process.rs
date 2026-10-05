//! `run_generator` and `run_tests`: subprocesses in the app root, each with a time limit.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use super::Outcome;
use crate::Options;

/// The generators `run_generator` may run.
pub(crate) const GENERATORS: &[&str] = &[
    "make:model",
    "make:controller",
    "make:migration",
    "make:middleware",
    "make:seeder",
    "make:factory",
    "make:command",
    "make:spark",
    "make:page",
    "make:agent",
    "make:job",
    "make:mail",
];

/// The long options of the `make:*` commands (`--model` also as `--model=Name`).
const GENERATOR_FLAGS: &[&str] = &[
    "--migration",
    "--controller",
    "--resource",
    "--factory",
    "--seeder",
    "--all",
    "--model",
    "--no-color",
];

/// The short options of `make:model` (`-m -c -r -f -s`, also combined: `-mcr`).
const GENERATOR_SHORT_FLAGS: &str = "mcrfs";

/// The most arguments one generator call takes, and the longest argument.
const MAX_GENERATOR_ARGS: usize = 64;
const MAX_GENERATOR_ARG: usize = 100;

/// The longest `run_tests` filter.
const MAX_FILTER: usize = 200;

/// The most output returned to the agent.
const MAX_OUTPUT: usize = 12_000;

/// A name or field of a `make:*` command: an ASCII letter, then letters, digits and `_ : ? -`
/// (`Post`, `post_comment`, `title:string`, `body:text?`).
fn is_generator_word(arg: &str) -> bool {
    arg.len() <= MAX_GENERATOR_ARG
        && arg.starts_with(|c: char| c.is_ascii_alphabetic())
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '?' | '-'))
}

/// Whether `arg` is part of the `make:*` syntax: a name or field, one of the known options, or `--model=Name`.
/// Everything else is refused before the process starts, so no argument can reach the `smeltery` binary as an
/// option it was not meant to get (S5-01).
fn is_generator_arg(arg: &str) -> bool {
    if let Some(long) = arg.strip_prefix("--") {
        return match long.split_once('=') {
            Some((name, value)) => name == "model" && is_generator_word(value),
            None => GENERATOR_FLAGS.contains(&arg),
        };
    }
    if let Some(short) = arg.strip_prefix('-') {
        return !short.is_empty() && short.chars().all(|c| GENERATOR_SHORT_FLAGS.contains(c));
    }
    is_generator_word(arg)
}

/// The arguments of one generator call, or why they are refused.
fn check_generator_args(args: &[String]) -> Result<(), String> {
    if args.len() > MAX_GENERATOR_ARGS {
        return Err(format!(
            "invalid generator arguments: at most {MAX_GENERATOR_ARGS}"
        ));
    }
    match args.iter().find(|a| !is_generator_arg(a)) {
        None => Ok(()),
        Some(bad) => Err(format!(
            "invalid generator argument `{}`: names and fields are letters, digits and `_ : ? -` starting with a \
             letter (`Post`, `title:string`, `body:text?`); options are {}, `--model=Name` and `-{GENERATOR_SHORT_FLAGS}`",
            bad.escape_debug(),
            GENERATOR_FLAGS.join(", ")
        )),
    }
}

/// A `cargo test` filter is a test path: ASCII letters, digits, `_` and `:`, at most `MAX_FILTER` long. Anything
/// else could reach cargo or the test harness as an option (`--config=…` runs any program, `--logfile=…` writes a
/// file), so it is refused before cargo starts (S5-01).
fn check_filter(filter: &str) -> Result<(), String> {
    let ok = filter.len() <= MAX_FILTER
        && filter
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid test filter `{}`: a test path of letters, digits, `_` and `:` (e.g. `posts::creates_a_post`), \
             at most {MAX_FILTER} characters",
            filter.escape_debug()
        ))
    }
}

/// What a finished (or killed) process produced.
struct Run {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs `program args` in `dir`; `Err` when it cannot start or did not finish within `timeout` (it is killed then).
async fn run(
    program: &Path,
    args: &[String],
    dir: &Path,
    timeout: Duration,
) -> Result<Run, String> {
    let child = tokio::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("NO_COLOR", "1")
        .env("CARGO_TERM_COLOR", "never")
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => Ok(Run {
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
        Ok(Err(e)) => Err(format!("{} failed: {e}", program.display())),
        // Dropping the future dropped the child, which kill_on_drop kills.
        Err(_) => Err(format!(
            "{} did not finish within {} s and was stopped",
            program.display(),
            timeout.as_secs()
        )),
    }
}

/// The last `MAX_OUTPUT` characters of `text`.
pub(super) fn tail(text: &str) -> String {
    let count = text.chars().count();
    if count <= MAX_OUTPUT {
        return text.to_owned();
    }
    let skipped: String = text.chars().skip(count - MAX_OUTPUT).collect();
    format!("…{skipped}")
}

pub(super) async fn run_generator(options: &Options, command: &str, args: &[String]) -> Outcome {
    if !GENERATORS.contains(&command) {
        return Outcome::error(format!(
            "`{command}` is not an allowed generator; use one of: {}",
            GENERATORS.join(", ")
        ));
    }
    if let Err(e) = check_generator_args(args) {
        return Outcome::error(e);
    }
    let mut argv = vec![command.to_owned()];
    argv.extend(args.iter().cloned());
    match run(
        &options.smeltery_bin,
        &argv,
        &options.root,
        options.generator_timeout,
    )
    .await
    {
        Ok(r) => {
            let text = tail(format!("{}{}", r.stdout, r.stderr).trim());
            if r.status == Some(0) {
                Outcome::ok(text)
            } else {
                Outcome::error(text)
            }
        }
        Err(e) => Outcome::error(e),
    }
}

pub(super) async fn run_tests(options: &Options, filter: Option<&str>) -> Outcome {
    let mut args = vec!["test".to_owned()];
    if let Some(f) = filter.map(str::trim).filter(|f| !f.is_empty()) {
        if let Err(e) = check_filter(f) {
            return Outcome::error(e);
        }
        args.push(f.to_owned());
    }
    match run(
        &options.cargo_bin,
        &args,
        &options.root,
        options.test_timeout,
    )
    .await
    {
        Ok(r) => {
            let summary = summarize(&r.stdout, &r.stderr);
            if r.status == Some(0) {
                Outcome::ok(summary)
            } else {
                Outcome::error(summary)
            }
        }
        Err(e) => Outcome::error(e),
    }
}

/// The `test result:` lines and the failure reports of `cargo test`; compiler errors when it did not build.
fn summarize(stdout: &str, stderr: &str) -> String {
    let mut out = Vec::new();
    let mut in_failures = false;
    for line in stdout.lines() {
        if line.starts_with("failures:") {
            in_failures = true;
        }
        if line.starts_with("test result:") {
            in_failures = false;
            out.push(line.to_owned());
            continue;
        }
        if in_failures || line.ends_with("FAILED") {
            out.push(line.to_owned());
        }
    }
    let errors: Vec<&str> = stderr
        .lines()
        .filter(|l| {
            l.starts_with("error")
                || l.starts_with("warning: unused")
                || l.trim_start().starts_with("-->")
        })
        .collect();
    if !errors.is_empty() {
        out.push(String::new());
        out.extend(errors.iter().map(|l| (*l).to_owned()));
    }
    if out.is_empty() {
        return tail(format!("{stdout}{stderr}").trim());
    }
    tail(&out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_keep_results_and_failures() {
        let stdout = "running 2 tests\ntest a ... ok\ntest b ... FAILED\n\nfailures:\n\n---- b stdout ----\npanicked at x\n\nfailures:\n    b\n\ntest result: FAILED. 1 passed; 1 failed\n";
        let s = summarize(stdout, "");
        assert!(s.contains("test b ... FAILED"));
        assert!(s.contains("panicked at x"));
        assert!(s.contains("test result: FAILED. 1 passed; 1 failed"));
        assert!(!s.contains("test a ... ok"));
    }

    #[test]
    fn long_output_keeps_the_end() {
        let long = "x".repeat(MAX_OUTPUT + 10) + "END";
        let t = tail(&long);
        assert!(t.ends_with("END"));
        assert_eq!(t.chars().count(), MAX_OUTPUT + 1);
    }
}
