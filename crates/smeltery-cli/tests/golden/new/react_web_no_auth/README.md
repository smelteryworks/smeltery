# My App

A [Smeltery](https://github.com/smelteryworks/smeltery) application.

## Run it

```sh
smeltery serve
```

The app listens on <http://127.0.0.1:8000> (`SERVER_HOST` and `SERVER_PORT` in `.env`). `smeltery serve` rebuilds
and restarts it when Rust code changes.

The pages are React components in `resources/js/pages/`, rendered in the browser by Inertia; the
controllers in `app/controllers/` return them with `alloy::render("…")` and their props. Node.js and npm build
them (Node.js 20.19+ or 22.12+): run `npm install` once, then `smeltery serve` also starts the Vite dev server
(`node node_modules/vite/bin/vite.js`, on `127.0.0.1:5173`), which updates the page as files change. The
stylesheet is `resources/css/app.css`, imported by `resources/js/app.tsx`: the forge theme, prebuilt for the kit's pages.

Every `VITE_` value in `.env` is compiled into the JavaScript and readable by anyone; the app writes
`VITE_APP_NAME` only.

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

The binary lands in `target/release/my-app`. Before it, `smeltery build` runs `npm run build`
(after `npm ci`, or `npm install` without a `package-lock.json`, when `node_modules/` is missing), which writes the
JavaScript, the CSS and `manifest.json` into `public/build/`. Ship the `public/` folder next to the binary; the
server needs no Node.js.

On a server the binary runs the setup commands itself, without the `smeltery` CLI:
`./my-app key:generate`, `./my-app storage:link` and `./my-app migrate --force` (`--force` is required with
`APP_ENV=production`).
