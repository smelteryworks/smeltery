# Headless Demo

A [Smeltery](https://github.com/smelteryworks/smeltery) application with agents only (no web routes), built with the
`smeltery` generators. It runs two supervised Watchfire agents:

- **`poller`** (`app/agents/poller.rs`): every `POLLER_EVERY_SECS` seconds it reads a number from `POLLER_URL`
  through `ctx.http()` (plain text such as `21.5`, or JSON `{"value": 21.5}`). Without a URL it simulates a value.
  The poll count and the latest value are its checkpoint, so a restart continues the count. A failed poll fails the
  run, and the supervisor restarts it after a backoff.
- **`flaky`** (`app/agents/flaky.rs`): a worker that fails on purpose. A run processes `FLAKY_BATCHES_PER_RUN`
  batches of `FLAKY_BATCH_SECS` seconds; about `FLAKY_FAILURE_PERCENT` of the runs fail at a random batch. Its
  random numbers come from `Fake::seeded(FLAKY_SEED)`, so a seed gives the same failures every time. Every run is
  restarted (`Restart::Always`) after an exponential backoff with jitter (1 to 10 seconds); more than 5 restarts
  within a minute mark it `failed` until it is started again.

## Run it

```sh
cp .env.example .env
smeltery key:generate
smeltery migrate
smeltery work
```

`smeltery work` runs the agents, the job queue workers and the schedule registered in `app/agents/mod.rs`, until
Ctrl-C. `smeltery serve` does the same and also rebuilds and restarts the app when Rust code changes.

While it runs, these commands talk to it through `WATCHFIRE_API_ADDR` (`127.0.0.1:8001` in `.env`):

| Command | What it does |
|---|---|
| `smeltery agents:list` | Lists the agents with their state, restarts and next restart. |
| `smeltery agents:start\|stop\|pause\|resume\|restart <name>` | Controls an agent. |
| `smeltery agents:logs <name>` | Shows an agent's recent log lines. |

`smeltery agents:runs flaky` lists the flaky worker's runs from the database, with their outcome (`completed`,
`failed`, `stopped`), duration, error and batch count, with or without a running worker.

| Key | Default | Meaning |
|---|---|---|
| `POLLER_URL` | empty | the URL polled for a number; empty: a simulated value |
| `POLLER_EVERY_SECS` | `30` | seconds between two polls |
| `FLAKY_FAILURE_PERCENT` | `30` | the share of runs that fail |
| `FLAKY_SEED` | `42` | the seed of the failures |
| `FLAKY_BATCH_SECS` | `5` | seconds per batch |
| `FLAKY_BATCHES_PER_RUN` | `6` | batches in one run |

## Test it

```sh
smeltery test
```

`tests/agents.rs` runs both agents under the real supervisor on paused Tokio time with the `Harness` and a fake HTTP
transport: the poller's ticks, its checkpoint across a restart, readings over HTTP and a failing poll restarted with
backoff; the flaky worker's share of failed runs, its restarts and backoffs, the same failures for the same seed, and
the restart limit marking it `failed`.

## How this app was built

The app was scaffolded by the `smeltery` command of this repository: both agent files come from `make:agent`, and
the hand-written parts are the business logic inside them, a settings struct in `config/` and the test file. From
the repository root:

```sh
cargo build -p smeltery --bin smeltery
target/debug/smeltery new headless-demo --path examples --kind headless --db sqlite --no-git --smeltery-path .
cd examples/headless-demo
smeltery make:agent Poller
smeltery make:agent Flaky
cargo add --dev tokio --features macros,rt,test-util
```

`--smeltery-path .` makes the app depend on this checkout's `crates/smeltery` by the relative path
`../../crates/smeltery`.

Written by hand after that: the bodies of `app/agents/poller.rs` and `app/agents/flaky.rs`, their registration
in `app/agents/mod.rs`, their settings in `config/agents.rs` (a new file, listed in `config/mod.rs`, with the keys in
`.env.example`), and `tests/agents.rs` (a new file).

## Build for production

```sh
smeltery build
```

The binary lands in `target/release/headless-demo`.
