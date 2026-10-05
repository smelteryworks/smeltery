# smeltery-bellows

Bellows is the AI-agent support of the [Smeltery](https://github.com/smelteryworks/smeltery) framework: an
[MCP](https://modelcontextprotocol.io) server that runs inside a Smeltery app as the app command `bellows:mcp`
(JSON-RPC 2.0 over stdin / stdout), so a coding agent can ask the app about itself and act on it. Apps use it
through the facade as `smeltery::bellows`:

```rust
# // The facade's paths, rebuilt from this crate's dependencies (D-140).
# mod smeltery {
#     pub use smeltery_bellows as bellows;
#     pub use smeltery_core::AppBuilder;
# }
use smeltery::bellows::BellowsExt as _;

pub fn build(app: smeltery::AppBuilder) -> smeltery::AppBuilder {
    app.bellows() // adds the `bellows:mcp` command
}
# fn main() {}
```

Tools: `route_list`, `models`, `db_schema`, `config_keys` (names only, never values), `last_errors`, `docs_search`,
`run_generator` (`smeltery make:*` only, with names, fields and the generators' options as arguments), `run_tests`
(`cargo test` with a time limit and a test-path filter; it compiles and runs the app's code), `agents_list` and
`agent_control` (the running app's Watchfire agents). Protocol: `initialize` (versions `2025-11-25`, `2025-06-18`,
`2025-03-26`, `2024-11-05`), `server/discover` (`2026-07-28`), `ping`, `tools/list`, `tools/call`. Requests run one
at a time; while one runs, `ping` is answered and `notifications/cancelled` stops it. Tool results hold the app's
data, log lines and test output; the server's instructions tell the agent to treat them as data, not as
instructions.

The full guide is the "Bellows: AI-agent support" section of the Smeltery README.

Licensed under either of Apache License 2.0 or MIT license at your option.
