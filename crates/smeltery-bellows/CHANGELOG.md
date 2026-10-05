# Changelog

All notable changes to `smeltery-bellows` are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-10-05

### Added

- `BellowsExt::bellows()`: the `bellows:mcp` app command, an MCP server over stdin / stdout (JSON-RPC 2.0, one
  message per line; `initialize` with version negotiation, `server/discover`, `ping`, `tools/list`, `tools/call`,
  batches; notifications and responses get no answer). The server instructions tell the agent that tool results
  are data, not instructions.
- Tools: `route_list`, `models`, `db_schema` (SQLite, PostgreSQL, MySQL catalogs; a SQLite virtual table such as
  an FTS5 search index is listed once, without its shadow tables), `config_keys` (names only, including the cache
  settings `CACHE_*`, `REDIS_URL`, `MEMCACHED_SERVERS`, `TEST_CACHE_STORE`, the queue and PubSub settings
  `QUEUE_PREFIX`, `PUBSUB_DRIVER`, `PUBSUB_POLL_MS`, `TEST_PUBSUB_DRIVER`, and `WATCHFIRE_IN_SERVE`,
  `WATCHFIRE_LOCK_STORE`, `WATCHFIRE_LEASE_TTL`, `WATCHFIRE_ALERT_MAIL`), `last_errors`, `docs_search` (the embedded
  Smeltery guide, `CLAUDE.md`, `.bellows/`), `run_generator` (allow-listed `make:*`, including `make:page` in React
  and Vue apps, 60 s), `run_tests` (`cargo test`, `BELLOWS_TEST_TIMEOUT`; its description says it compiles and runs
  the app's code), `agents_list` and `agent_control` (the running app's Watchfire API with the `APP_KEY` token).
- `last_errors` reads the app's `LOG_FILE` (and its `.1` backup) when it exists, else `storage/logs/*.log`; it
  matches the level word instead of any `ERROR` in the line, and `warnings: true` adds `WARN` lines.
  `Options::log_file`.
- `McpServer` and `Options` for running the server on any reader / writer. `McpServer::serve` keeps reading while
  a request runs: `ping` is answered at once and `notifications/cancelled` stops the running request (its
  `cargo test` or generator process is killed) or drops a waiting one. Requests run one at a time, in order.

### Security
- `run_tests` accepts only a test path as `filter` (letters, digits, `_`, `:`, at most 200 characters), so no
  option such as `--config=…` reaches `cargo test`.
- `run_generator` accepts only names, fields and the generators' options as arguments (at most 64, each at most 100
  characters, all strings).
- `last_errors` cuts lines at 2,000 characters, caps the answer at 12,000 characters and reads at most 20 files from
  `storage/logs/`.
- `McpServer::serve` refuses a line longer than 1 MiB without holding it in memory. A UTF-8 byte order mark before
  the first message and lines that are not UTF-8 neither end nor break the session.

[0.1.0]: https://github.com/smelteryworks/smeltery/releases/tag/v0.1.0
