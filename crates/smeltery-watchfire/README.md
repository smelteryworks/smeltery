# smeltery-watchfire

Watchfire is the agent runtime of the [Smeltery](https://github.com/smelteryworks/smeltery) framework: supervised
long-running **agents**, background **jobs** on a queue, and a **scheduler**, on one Tokio runtime. Apps use it
through the facade as `smeltery::watchfire`; the full guide is the "Watchfire" section of the Smeltery README.

```rust,no_run
use smeltery_watchfire::prelude::*;

pub fn register(w: &mut Watchfire) {
    w.every(30.secs(), "heartbeat", |ctx| async move {
        ctx.log().info("tick");
        Ok(())
    });
    w.run("scraper", |ctx| async move {
        while ctx.sleep(5.secs()).await {
            ctx.http().get("https://example.com/").await?;
        }
        Ok(())
    })
    .restart(Restart::OnFailure)
    .backoff(1.secs()..=30.secs());
    w.rate_limit("example.com", 2.per_second());
    w.schedule()
        .call("cleanup", |_ctx| async move { Ok(()) })
        .every(5.mins());
}

// bootstrap/app.rs: `app.agents(register)` with `use smeltery_watchfire::AgentsExt as _;`
```

What it has:

- **Supervision:** restart policies (`Never`, `OnFailure`, `Always`), exponential backoff with full jitter and a
  cap, a restart limit per time window, heartbeat stall detection with restart, panic capture, pause / resume,
  start / stop / restart, agents added and removed at runtime, pools (`name#0..n`), group and global concurrency
  limits, supervision trees (`ctx.spawn_child`), and exactly one recorded outcome per run, including on shutdown.
- **`AgentCtx`:** cancellation, heartbeat, `sleep` / `interval`, a polite HTTP client (timeouts, retries honouring
  `Retry-After`, per-host token buckets, a response size limit, a redirect policy that keeps credentials on their
  origin; reqwest with rustls and ring behind a `Transport` trait), checkpoints,
  events (`emit` / `on_event`), counters saved in run records, logs kept in a ring buffer, the app and its database.
- **Jobs:** `Job` structs dispatched with `job.dispatch(&app)`, a database queue with atomic reservation, a Redis
  queue (feature `redis`: Lua scripts, atomic across processes) or an in-memory one, retries with backoff, dead
  letters (moved in and out in one step), stale reservation release.
- **Scheduler:** `every`, `hourly`, `daily`, `daily_at`, `weekly`, 5-field `cron` (UTC), overlap `Skip` / `Queue` /
  `Allow`; targets are jobs, closures and agents.
- **Persistence:** the `watchfire_*` tables over the app's database (`migrations::up` / `down`), or memory.
- **App integration:** `serve` runs agents next to the web server (`serve --no-agents` or `WATCHFIRE_IN_SERVE=false`:
  the web only), `work` runs them alone, both within the app's shutdown budget; the `agents:runs`, `schedule:list`
  and `schedule:run` commands.
- **Several processes:** over a shared cache store's atomic locks, each agent runs in one process at a time (a
  renewed lease with a hard cut-off, `standby` elsewhere, takeover when the holder stops or dies) and each scheduled
  tick runs once; run history records each run's process. The dashboard of any process shows the agents of the
  others from the shared tables and sends them commands through `watchfire_commands`.
- **Web:** a JSON API and an SSE stream under `/_watchfire/api` (bearer token; local development without it), a
  dashboard at `/_watchfire` for signed-in users the app's `dashboard_gate` admits (a Mold page compiled into the
  crate with its own embedded stylesheet, live through Sparks, CSRF-protected command forms that ask before a stop
  or restart), and console commands
  (`agents:list`, `agents:start|stop|pause|resume|restart`, `agents:logs`) calling the running app, and
  `agents:token`.
- **Alerts:** `on_alert` hooks and a JSON webhook for failed agents, stalled runs and dead-lettered jobs.
- **LLM helpers** (feature `llm`): a `Provider` trait, `FakeProvider`, the `Anthropic` Messages API adapter and a
  tool-calling `Agent` loop with typed tools, a turn limit, token / cost budgets and retries.
- **Testing:** `testing::Harness` on paused time, `testing::JobHarness` (alone or inside a given app), `http::FakeTransport`.

## Licence

Dual-licensed under MIT or Apache-2.0, at your option.
