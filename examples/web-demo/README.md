# Web Demo

A [Smeltery](https://github.com/smelteryworks/smeltery) web application with agents, built with the `smeltery`
generators. It shows:

- **Posts**: a CRUD resource behind `auth` (`app/controllers/posts.rs`, `resources/views/posts/`) with validated
  forms, flash messages and one image per post.
- **Image uploads**: the `post_image` Spark (`app/sparks/post_image.rs`) on each post's page takes a PNG, JPEG,
  GIF or WebP file of at most 2 MB, checks its first bytes, and stores it under `storage/app/public/posts/`, served
  at `/storage/posts/…`.
- **Authentication**: register, log in, log out, password reset (the generated scaffolding).
- **A live counter**: a scheduled Watchfire job (`app/jobs/bump_counter.rs`, every 5 seconds) adds one to a row of
  the `metrics` table and pushes a refresh to the `live_counter` Spark on the dashboard
  (`app/sparks/live_counter.rs`, `#[spark(stream)]`) over server-sent events.
- **A polite scraper**: the `scraper` agent (`app/agents/scraper.rs`) crawls one site from `SCRAPER_START_URL`,
  stores each page's title in the `pages` table (listed on the dashboard), waits `SCRAPER_DELAY_MS` between two
  requests to the host (`w.rate_limit`), saves its queue as a checkpoint after every page, and is restarted with
  backoff when a run fails, resuming from the checkpoint.
- **The live Watchfire dashboard** at `/_watchfire`: the scraper, the queue workers and the schedule.

## Run it

```sh
cp .env.example .env
smeltery key:generate
smeltery migrate
smeltery db:seed
smeltery storage:link
smeltery serve
```

Open <http://127.0.0.1:8000>, log in as `demo@example.com` with the password `password`, and open the dashboard.

The scraper idles until `SCRAPER_START_URL` names a site you may crawl. Pointing it at the app itself works
without the network: `SCRAPER_START_URL=http://127.0.0.1:8000/`.

| Key | Default | Meaning |
|---|---|---|
| `SCRAPER_START_URL` | empty | the first page; links on the same host are followed; empty: the scraper idles |
| `SCRAPER_MAX_PAGES` | `50` | the most pages one crawl stores |
| `SCRAPER_DELAY_MS` | `2000` | the pause between two requests to the host |
| `SCRAPER_RETRIES` | `3` | retries of a failed request before the run fails |

The crawl's progress is the agent's checkpoint: after a restart it continues; a finished crawl stays finished until
`SCRAPER_START_URL` changes.

## Test it

```sh
smeltery test
```

| File | Covers |
|---|---|
| `tests/http.rs` | home page, counter Spark, health check, registration, login, logout, password reset mail |
| `tests/posts.rs` | the posts CRUD behind `auth`, validation messages and old input, image upload, replacement and removal, size and type checks (on a temporary storage root) |
| `tests/live_counter.rs` | the job increments the counter and pushes a refresh, the dashboard's live counter re-renders, the schedule runs the job every 5 seconds |
| `tests/scraper.rs` | the crawl against a fake HTTP transport: titles stored, other sites skipped, the host's rate limit, restart with backoff and resume from the checkpoint, server and client errors, the page limit, idling without a start URL |

## How this app was built

The app was scaffolded by the `smeltery` command of this repository: every model, migration, controller, view,
Spark, job and agent file comes from a generator, and the hand-written parts are the business logic inside them, a
settings struct in `config/` and the test files. From the repository root:

```sh
cargo build -p smeltery --bin smeltery
target/debug/smeltery new web-demo --path examples --kind web --smelt watchfire,temper --db sqlite --bellows all --no-tailwind --no-git --smeltery-path .
cd examples/web-demo
smeltery make:model Post title:string body:text -mcrfs
smeltery make:migration add_image_to_posts_table
smeltery make:spark PostImage
smeltery make:model Metric name:string value:bigint -m
smeltery make:job BumpCounter
smeltery make:spark LiveCounter
smeltery make:model Page url:string title:string -m
smeltery make:agent Scraper
smeltery storage:link
cargo add tokio --no-default-features --features fs
cargo add tracing --no-default-features --features std
cargo add --dev tempfile
```

`--smeltery-path .` makes the app depend on this checkout's `crates/smeltery` by the relative path
`../../crates/smeltery`.

Written by hand after that:

| File | What was added |
|---|---|
| `app/models/post.rs`, `app/models/metric.rs` | the `image` column; `Metric::current` and `Metric::increment` |
| `database/migrations/*` | unique `metrics.name` and `pages.url`, the `down` of `add_image_to_posts_table` |
| `app/controllers/posts.rs`, `routes/web.rs` | newest posts first, the image file deleted with its post, `auth` on the resource |
| `app/controllers/dashboard.rs`, `resources/views/dashboard.mold.html` | the live counter, links, the scraped pages |
| `app/sparks/post_image.rs`, `app/sparks/live_counter.rs` and their views | the upload component and the streamed counter |
| `app/jobs/bump_counter.rs` | the increment and the push |
| `app/agents/scraper.rs`, `app/agents/mod.rs`, `.env.example` | the crawler, its rate limit, the schedule, the `SCRAPER_*` keys |
| `resources/views/posts/*.mold.html` | the image Spark on the post page, a marker on the list |
| `config/scraper.rs`, `config/mod.rs` | the scraper's settings (a new file) |
| `tests/posts.rs`, `tests/live_counter.rs`, `tests/scraper.rs` | the tests above (new files) |
