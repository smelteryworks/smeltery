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

Login, registration, password reset, e-mail verification, two-factor authentication and the settings pages
(`/settings/profile`, `/settings/password`, `/settings/two-factor`) come from Temper: `app/providers/temper.rs`
turns the features on and names their pages, `app/actions/temper/` holds the forms and what they do.
`smeltery route:list` lists the routes.

## API tokens

Mobile apps, desktop apps and other clients get an API token from `POST /api/tokens` (`email`, `password`,
`device_name`, and `code` for an account with two-factor authentication on), send it as
`Authorization: Bearer smt_…` to the `auth:hallmark` routes of `routes/api.rs` (`GET /api/user`), and sign it out
with `DELETE /api/tokens/current`. Hallmark stores only each token's hash; `HALLMARK_*` in `.env` change its
settings.

## Broadcasting

`smeltery serve` also serves WebSockets at `ws://127.0.0.1:8000/app/<key>` (the key is derived from `APP_KEY`;
`ANVIL_APP_KEY` in `.env` sets it). Clients that speak the Pusher protocol (`pusher-js`, `laravel-echo`, the Pusher
libraries for mobile platforms) connect with this host and key, subscribe to the channels of
`routes/channels.rs` (private ones through `POST /broadcasting/auth`), and receive the events
in `app/events/`.

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
