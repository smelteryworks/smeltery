# My App

A [Smeltery](https://github.com/smelteryworks/smeltery) application.

## Run it

```sh
smeltery serve
```

The app listens on <http://127.0.0.1:8000> (`SERVER_HOST` and `SERVER_PORT` in `.env`). `smeltery serve` rebuilds
and restarts it when Rust code changes. It also runs the agents, jobs and schedule in `app/agents/`; the
Watchfire dashboard is at <http://127.0.0.1:8000/_watchfire>.

The pages are styled by `public/assets/css/app.css`, which Tailwind builds from `resources/css/app.css` and the
classes in `resources/views/`. With Tailwind installed (`smeltery tailwind:install`), `smeltery serve` rebuilds it
on every change.

## Authentication

This app was created without login pages (without the `temper` building block).
`.temper(app::providers::temper::temper())` in `bootstrap/app.rs`, with the files `CLAUDE.md` lists, turns
authentication on.

## Test it

```sh
smeltery test
```

## Build for production

```sh
smeltery build
```

The binary lands in `target/release/my-app`. `smeltery build` also builds a minified
`public/assets/css/app.css` with Tailwind when it is installed. Ship the `public/` folder next to the binary.

On a server the binary runs the setup commands itself, without the `smeltery` CLI:
`./my-app key:generate`, `./my-app storage:link` and `./my-app migrate --force` (`--force` is required with
`APP_ENV=production`).
