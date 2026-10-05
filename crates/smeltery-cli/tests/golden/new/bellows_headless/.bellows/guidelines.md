# My App: guidelines for coding agents

- Create files with the generators (`smeltery make:*`, or the `run_generator` MCP tool) instead of by hand: they write
  the file and its registration line together and never overwrite an existing file.
- One module per folder. A new module is listed in its folder's `mod.rs` above the `// smeltery:mods` marker;
  registrations go above `// smeltery:models`, `// smeltery:migrations`, `// smeltery:seeders`,
  `// smeltery:commands`, `// smeltery:agents`. Keep the markers.
- Read settings through the typed structs in `config/` (`smeltery::config::env("KEY", default)`), never with
  `std::env::var`. Secrets live only in `.env`; never log them or put them in code, tests or fixtures.
- Change the database only through a new migration (`smeltery make:migration name`); never edit one that has run.
- Send mail through `Mailer` with a mail class (`smeltery make:mail Name`); never call SMTP directly. Mail settings
  come from the `MAIL_*` keys in `.env`.
- Async code never blocks: use `tokio::task::spawn_blocking` for CPU-heavy or blocking work.
- Every change comes with a test in `tests/` (`smeltery::testing::TestApp`); run `smeltery test` before finishing.
