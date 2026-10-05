//! The scraper agent under the real Watchfire supervisor, against a fake HTTP transport (no network) and the
//! test database. These tests run on real time with short delays: SQLite works on its own thread, so paused
//! Tokio time could jump past the store's timeouts.

use std::time::{Duration, Instant};

use smeltery::db::prelude::*;
use smeltery::testing::TestApp;
use smeltery::watchfire::http::{FakeResponse, FakeTransport, Method};
use smeltery::watchfire::prelude::*;
use smeltery::watchfire::testing::Harness;

use web_demo::app::agents::scraper::{self, Crawl, Scraper};
use web_demo::app::models::{Page, page};
use web_demo::config::scraper::ScraperConfig;

const START: &str = "https://demo.test/";

fn config(delay_ms: u64) -> ScraperConfig {
    ScraperConfig {
        start_url: START.to_owned(),
        max_pages: 50,
        delay: Duration::from_millis(delay_ms),
        retries: 0,
    }
}

fn html(title: &str, links: &[&str]) -> FakeResponse {
    let links: String = links
        .iter()
        .map(|href| format!("<a href=\"{href}\">link</a>"))
        .collect();
    FakeResponse::text(&format!(
        "<html><head><title>{title}</title></head><body>{links}</body></html>"
    ))
}

/// A site of three pages: `/` links to `/a`, `b` (relative), another site, a mail address and a fragment.
fn site() -> FakeTransport {
    let fake = FakeTransport::new();
    fake.on(
        Method::GET,
        START,
        html(
            "Home",
            &[
                "/a",
                "b",
                "https://other.test/x",
                "mailto:hi@demo.test",
                "#top",
                "/a#again",
            ],
        ),
    );
    fake.on(
        Method::GET,
        "https://demo.test/a",
        html("A &amp; B", &["/"]),
    );
    fake.on(
        Method::GET,
        "https://demo.test/b",
        html("  Page\n  B ", &[]),
    );
    fake
}

/// Runs the registration of `app/agents/scraper.rs` with `config` until the scraper completes.
async fn crawl(app: &TestApp, fake: FakeTransport, config: ScraperConfig) -> Harness {
    let mut w = Watchfire::new();
    scraper::register(&mut w, config);
    let mut h = Harness::from_watchfire(w)
        .app(app.app().clone())
        .http(fake)
        .seed(7)
        .config(
            AgentConfig::default()
                .restart(Restart::OnFailure)
                .backoff(Duration::from_millis(10)..=Duration::from_millis(40)),
        );
    h.start().await.expect("the scraper starts");
    let started = Instant::now();
    while h.state() != AgentState::Completed {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the crawl did not complete: {:?}",
            h.transitions()
        );
        h.advance(Duration::from_millis(20)).await;
    }
    h
}

fn stored(app: &TestApp) -> Vec<(String, String)> {
    let pages = app
        .block_on(
            Page::query()
                .order_by_asc(page::Column::Id)
                .all(app.db().conn()),
        )
        .expect("pages");
    pages.into_iter().map(|p| (p.url, p.title)).collect()
}

fn urls(fake: &FakeTransport) -> Vec<String> {
    fake.requests().into_iter().map(|r| r.url).collect()
}

#[test]
fn it_crawls_the_start_host_and_stores_the_titles() {
    let app = TestApp::new(web_demo::build);
    let fake = site();
    let h = app.block_on(crawl(&app, fake.clone(), config(1)));
    assert_eq!(
        stored(&app),
        [
            ("https://demo.test/".to_owned(), "Home".to_owned()),
            ("https://demo.test/a".to_owned(), "A & B".to_owned()),
            ("https://demo.test/b".to_owned(), "Page B".to_owned()),
        ]
    );
    // Each page once, never another site.
    assert_eq!(
        urls(&fake),
        [
            "https://demo.test/",
            "https://demo.test/a",
            "https://demo.test/b"
        ]
    );
    let runs = app.block_on(h.runs());
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, RunOutcome::Completed);
    assert_eq!(runs[0].counters.get("pages"), Some(&3));
    app.block_on(h.shutdown());
}

#[test]
fn the_host_rate_limit_spaces_the_requests() {
    let app = TestApp::new(web_demo::build);
    let fake = site();
    let h = app.block_on(crawl(&app, fake.clone(), config(150)));
    let requests = fake.requests();
    assert_eq!(requests.len(), 3);
    for pair in requests.windows(2) {
        let gap = pair[1].at.duration_since(pair[0].at);
        assert!(
            gap >= Duration::from_millis(140),
            "only {gap:?} between requests"
        );
    }
    app.block_on(h.shutdown());
}

#[test]
fn a_failure_restarts_with_backoff_and_resumes_from_the_checkpoint() {
    let app = TestApp::new(web_demo::build);
    // `/a` fails once (no retries in this config), then answers.
    let fake = FakeTransport::new();
    fake.on(Method::GET, START, html("Home", &["/a", "b"]));
    fake.on(
        Method::GET,
        "https://demo.test/a",
        FakeResponse::connect_error(),
    )
    .on(Method::GET, "https://demo.test/a", html("A", &[]));
    fake.on(Method::GET, "https://demo.test/b", html("B", &[]));
    let h = app.block_on(crawl(&app, fake.clone(), config(1)));

    // The second run did not fetch `/` again: it resumed at `/a` from the checkpoint.
    assert_eq!(
        urls(&fake),
        [
            "https://demo.test/",
            "https://demo.test/a",
            "https://demo.test/a",
            "https://demo.test/b"
        ]
    );
    assert_eq!(h.restarts(), 1);
    assert!(h.transitions().contains(&AgentState::BackingOff));
    let runs = app.block_on(h.runs());
    let outcomes: Vec<RunOutcome> = runs.iter().map(|r| r.outcome).collect();
    assert_eq!(outcomes, [RunOutcome::Failed, RunOutcome::Completed]);
    assert_eq!(stored(&app).len(), 3);
    let logs = h.logs();
    assert!(
        logs.iter().any(|l| l
            .message
            .contains("resuming the crawl of https://demo.test/: 1 stored, 2 queued")),
        "{logs:?}"
    );
    app.block_on(h.shutdown());
}

#[test]
fn a_server_error_fails_the_run_and_a_client_error_skips_the_page() {
    let app = TestApp::new(web_demo::build);
    let fake = FakeTransport::new();
    fake.on(Method::GET, START, html("Home", &["/gone", "/busy"]));
    fake.on(
        Method::GET,
        "https://demo.test/gone",
        FakeResponse::status(404),
    );
    fake.on(
        Method::GET,
        "https://demo.test/busy",
        FakeResponse::status(503),
    )
    .on(Method::GET, "https://demo.test/busy", html("Busy", &[]));
    let h = app.block_on(crawl(&app, fake.clone(), config(1)));
    let outcomes: Vec<RunOutcome> = app.block_on(h.runs()).iter().map(|r| r.outcome).collect();
    assert_eq!(outcomes, [RunOutcome::Failed, RunOutcome::Completed]);
    let titles: Vec<String> = stored(&app).into_iter().map(|(_, t)| t).collect();
    assert_eq!(titles, ["Home", "Busy"]);
    app.block_on(h.shutdown());
}

#[test]
fn max_pages_ends_the_crawl() {
    let app = TestApp::new(web_demo::build);
    let mut config = config(1);
    config.max_pages = 2;
    let h = app.block_on(crawl(&app, site(), config));
    assert_eq!(stored(&app).len(), 2);
    app.block_on(h.shutdown());
}

#[test]
fn without_a_start_url_the_scraper_idles() {
    let app = TestApp::new(web_demo::build);
    let fake = FakeTransport::new();
    let mut config = config(1);
    config.start_url = String::new();
    app.block_on(async {
        let mut h = Harness::new(Scraper::new(config))
            .app(app.app().clone())
            .http(fake.clone());
        h.start().await.unwrap();
        h.advance(Duration::from_millis(100)).await;
        assert_eq!(h.state(), AgentState::Running);
        assert!(
            h.logs()
                .iter()
                .any(|l| l.message.contains("SCRAPER_START_URL is empty"))
        );
        h.shutdown().await;
    });
    assert!(fake.requests().is_empty());
}

#[test]
fn the_checkpoint_is_plain_json() {
    let crawl = Crawl {
        start: START.to_owned(),
        queue: ["https://demo.test/a".to_owned()].into(),
        seen: [START.to_owned(), "https://demo.test/a".to_owned()].into(),
        stored: 1,
    };
    let json = smeltery::json!(crawl);
    assert_eq!(json["queue"][0], "https://demo.test/a");
    assert_eq!(json["stored"], 1);
}

#[test]
fn links_titles_and_hosts() {
    let page = r#"<link rel="stylesheet" href="/app.css"><a class="nav" href="/x">1</a> <A HREF='y?q=1#f'>2</A> <a href="//demo.test/z">3</a>
        <a href="HTTPS://DEMO.TEST/w">4</a> <a href="javascript:void(0)">5</a> <a href="https://demo.test:8080/p">6</a>"#;
    assert_eq!(
        scraper::links_of(page, "https://demo.test/dir/page", "https://demo.test"),
        [
            "https://demo.test/x",
            "https://demo.test/dir/y?q=1",
            "https://demo.test/z",
            "https://demo.test/w"
        ]
    );
    assert_eq!(
        scraper::title_of("<TITLE>\n  Hello &amp;\n world </TITLE>").as_deref(),
        Some("Hello & world")
    );
    assert_eq!(scraper::title_of("<p>no title</p>"), None);
    assert_eq!(
        scraper::host_of("https://Demo.Test:8443/a").as_deref(),
        Some("demo.test")
    );
    assert_eq!(scraper::origin_of("ftp://demo.test/"), None);
    assert_eq!(scraper::origin_of("https://user@demo.test/"), None);
}
