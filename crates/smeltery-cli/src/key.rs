//! `smeltery key:generate`: the application key in `.env`.
//!
//! The work is done by `smeltery_core::console::setup`, which the app binary's own `key:generate` uses too, so both
//! behave the same.

use std::path::Path;
use std::process::ExitCode;

use smeltery_core::console::setup::{self, KeyWrite};

use crate::cmd::require_app;

/// Generates a key in the form `base64:<32 random bytes, base64>`.
pub(crate) fn generate() -> anyhow::Result<String> {
    Ok(setup::generate_key()?)
}

/// Runs `key:generate` in the app `dir`: prints the key with `show`, otherwise writes it into `dir/.env`. In
/// production (`APP_ENV=production` in the environment or `.env`, or no `APP_ENV` at all) an existing key is kept
/// unless `force` is given.
pub(crate) fn run(dir: &Path, show: bool, force: bool) -> anyhow::Result<ExitCode> {
    let key = generate()?;
    if show {
        println!("{key}");
        return Ok(ExitCode::SUCCESS);
    }
    require_app(dir)?;
    match setup::write_app_key(dir, &key, setup::is_production_at(dir), force)? {
        KeyWrite::Written(_) => {
            println!("{}", setup::key_written_message());
            Ok(ExitCode::SUCCESS)
        }
        KeyWrite::WrittenInPlace(_) => {
            println!("{}", setup::key_written_message());
            println!("{}", setup::KEY_WRITTEN_IN_PLACE);
            Ok(ExitCode::SUCCESS)
        }
        KeyWrite::KeptInProduction => {
            println!("{}", setup::KEY_KEPT_IN_PRODUCTION);
            Ok(ExitCode::FAILURE)
        }
        _ => {
            eprintln!("error: key:generate did not write the key");
            Ok(ExitCode::FAILURE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::tests::fake_app;

    #[test]
    fn generated_keys_are_32_bytes_and_differ() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert!(a.starts_with("base64:"));
        assert_eq!(a.len(), "base64:".len() + 44);
        assert_ne!(a, b);
    }

    #[test]
    fn run_writes_into_env_file() {
        let app = fake_app();
        let dir = app.path();
        std::fs::write(dir.join(".env"), "APP_NAME=x\nAPP_KEY=\n").unwrap();
        assert_eq!(run(dir, false, false).unwrap(), ExitCode::SUCCESS);
        let text = std::fs::read_to_string(dir.join(".env")).unwrap();
        assert!(text.starts_with("APP_NAME=x\nAPP_KEY=base64:"), "{text}");
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn outside_an_app_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run(dir.path(), false, false).is_err());
        assert!(!dir.path().join(".env").exists());
        // --show only prints, anywhere.
        assert_eq!(run(dir.path(), true, false).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn run_keeps_a_production_key_without_force() {
        // The process environment wins over `.env`; skip when this test process sets APP_ENV itself.
        if std::env::var_os("APP_ENV").is_some() {
            return;
        }
        let app = fake_app();
        let env = app.path().join(".env");
        let before = "APP_ENV=production\nAPP_KEY=base64:old\n";
        std::fs::write(&env, before).unwrap();
        assert_eq!(run(app.path(), false, false).unwrap(), ExitCode::FAILURE);
        assert_eq!(std::fs::read_to_string(&env).unwrap(), before);
        assert_eq!(run(app.path(), false, true).unwrap(), ExitCode::SUCCESS);
        let after = std::fs::read_to_string(&env).unwrap();
        assert!(
            after.starts_with("APP_ENV=production\nAPP_KEY=base64:"),
            "{after}"
        );
        assert!(!after.contains("base64:old"), "{after}");
    }
}
