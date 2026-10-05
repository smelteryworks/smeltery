//! `hallmark:prune-expired`.

use std::time::Duration;

use smeltery_core::console::{Args, Command, Output};
use smeltery_core::{App, Error, Result};

use crate::tokens::Tokens;

/// `hallmark:prune-expired [--hours=24]`: delete tokens that expired at least that many hours ago.
pub(crate) struct PruneExpired;

impl Command for PruneExpired {
    fn name(&self) -> &'static str {
        "hallmark:prune-expired"
    }

    fn about(&self) -> &'static str {
        "Delete API tokens that expired at least --hours ago (default 24)"
    }

    async fn run(&self, app: &App, args: Args) -> Result<()> {
        self.run_with_output(app, args, Output::default()).await
    }

    async fn run_with_output(&self, app: &App, args: Args, out: Output) -> Result<()> {
        let hours: u64 = match args.value("hours") {
            None => 24,
            Some(text) => text
                .trim()
                .parse()
                .ok()
                .filter(|h| *h <= 24 * 365 * 100)
                .ok_or_else(|| {
                    Error::internal("--hours takes a whole number of hours, e.g. --hours=24")
                })?,
        };
        let deleted = Tokens::of(app)?
            .prune_expired(Duration::from_secs(hours * 3600))
            .await?;
        out.line(format!(
            "Deleted {deleted} expired API token{}.",
            if deleted == 1 { "" } else { "s" }
        ));
        Ok(())
    }
}
