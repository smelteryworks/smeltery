# My App: guidelines for coding agents

- Create files with the generators (`smeltery make:*`, or the `run_generator` MCP tool) instead of by hand: they write
  the file and its registration line together and never overwrite an existing file.
- One module per folder. A new module is listed in its folder's `mod.rs` above the `// smeltery:mods` marker;
  registrations go above `// smeltery:models`, `// smeltery:migrations`, `// smeltery:seeders`,
  `// smeltery:commands`, `// smeltery:routes`, `// smeltery:agents`. Keep the markers.
- Read settings through the typed structs in `config/` (`smeltery::config::env("KEY", default)`), never with
  `std::env::var`. Secrets live only in `.env`; never log them or put them in code, tests or fixtures.
- Change the database only through a new migration (`smeltery make:migration name`); never edit one that has run.
- Pages are React components in `resources/js/pages/`; a controller returns `alloy::render("name")` with its
  props (`smeltery make:page Name`, `smeltery make:model Name … --all`). Every prop and every flashed value is
  readable in the browser: never pass models with secrets, session values or tokens; send a struct with the fields
  the page needs.
- Forms use `useForm` and need no CSRF field (Inertia's client sends the `XSRF-TOKEN` cookie back as a header);
  validate input with a `#[derive(Validate)]` form struct and `Valid<Form>`; protect pages with
  `.middleware("auth")`. Run `npm run types` after changing pages.
- Authentication is Temper: change its features and pages in `app/providers/temper.rs` and its forms in
  `app/actions/temper/` instead of writing login or password routes; pages that change security settings take
  `.middleware("password.confirm")`.
- Send mail through `Mailer` with a mail class (`smeltery make:mail Name`); never call SMTP directly. Mail settings
  come from the `MAIL_*` keys in `.env`.
- Async code never blocks: use `tokio::task::spawn_blocking` for CPU-heavy or blocking work.
- Every change comes with a test in `tests/` (`smeltery::testing::TestApp`); run `smeltery test` before finishing.
