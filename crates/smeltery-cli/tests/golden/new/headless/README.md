# My App

A [Smeltery](https://github.com/smelteryworks/smeltery) application.

## Run it

```sh
smeltery work
```

`smeltery work` runs the agents, the job queue workers and the schedule registered in `app/agents/mod.rs`, until
Ctrl-C. `smeltery serve` does the same and also rebuilds and restarts the app when Rust code changes.

While it runs, these commands talk to it through `WATCHFIRE_API_ADDR` (`127.0.0.1:8001` in `.env`):

| Command | What it does |
|---|---|
| `smeltery agents:list` | Lists the agents with their state. |
| `smeltery agents:start\|stop\|pause\|resume\|restart <name>` | Controls an agent. |
| `smeltery agents:logs <name>` | Shows an agent's recent log lines. |

`smeltery agents:runs <name>` (recent runs), `smeltery schedule:list` and `smeltery schedule:run` (run what is due
now, once) work without a running worker.

## Test it

```sh
smeltery test
```

## Build for production

```sh
smeltery build
```

The binary lands in `target/release/my-app`.

On a server the binary runs the setup commands itself, without the `smeltery` CLI:
`./my-app key:generate` and `./my-app migrate --force` (`--force` is required with
`APP_ENV=production`).
