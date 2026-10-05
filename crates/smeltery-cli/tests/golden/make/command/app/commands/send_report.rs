//! The `send-report` command.

use smeltery::console::{Args, Command};
use smeltery::{App, Result};

/// `smeltery send-report`.
pub struct SendReport;

impl Command for SendReport {
    fn name(&self) -> &'static str {
        "send-report"
    }

    fn about(&self) -> &'static str {
        "Send report"
    }

    async fn run(&self, _app: &App, _args: Args) -> Result<()> {
        println!("send-report: done");
        Ok(())
    }
}
