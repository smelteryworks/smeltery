//! The `scraper` agent: a polite crawler that stores the title of every page it visits in the `pages` table.
//!
//! It stays on the start URL's host, waits for the host's rate limit before every request (`w.rate_limit`, set
//! up by [`register`]), and saves its queue as a checkpoint after every page, so a restart (a failure, a stop, a
//! new deploy) resumes where it stopped instead of crawling again from the start.

use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use smeltery::db::prelude::*;
use smeltery::watchfire::Rate;
use smeltery::watchfire::http::Method;
use smeltery::watchfire::prelude::*;

use crate::app::models::{Page, page};
use crate::config::scraper::ScraperConfig;

/// The agent's name.
pub const NAME: &str = "scraper";

/// The crawl's progress, saved as the agent's checkpoint after every page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Crawl {
    /// The start URL the crawl belongs to; another `SCRAPER_START_URL` starts a new crawl.
    pub start: String,
    /// URLs still to visit, in order.
    pub queue: VecDeque<String>,
    /// Every URL queued so far, so a page is visited once.
    pub seen: BTreeSet<String>,
    /// Pages stored so far.
    pub stored: usize,
}

impl Crawl {
    fn starting_at(start: &str) -> Self {
        Self {
            start: start.to_owned(),
            queue: VecDeque::from([start.to_owned()]),
            seen: BTreeSet::from([start.to_owned()]),
            stored: 0,
        }
    }
}

/// Registers the scraper and the rate limit of its host.
pub fn register(w: &mut Watchfire, config: ScraperConfig) {
    if let Some(host) = host_of(&config.start_url) {
        // One request per `delay`, never a burst.
        w.rate_limit(&host, Rate::new(1, config.delay));
    }
    w.agent(Scraper::new(config));
}

/// A supervised agent: restarted with backoff when a run fails; its progress survives in the checkpoint.
pub struct Scraper {
    config: ScraperConfig,
}

impl Scraper {
    /// A scraper with these settings.
    pub fn new(config: ScraperConfig) -> Self {
        Self { config }
    }
}

impl Agent for Scraper {
    fn name(&self) -> String {
        NAME.into()
    }

    fn config(&self) -> AgentConfig {
        AgentConfig::default()
            .restart(Restart::OnFailure)
            .backoff(1.secs()..=60.secs())
            .heartbeat_timeout(2.mins())
    }

    async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
        let start = self.config.start_url.trim().to_owned();
        let Some(origin) = origin_of(&start) else {
            if start.is_empty() {
                ctx.log()
                    .info("SCRAPER_START_URL is empty: nothing to crawl, the scraper idles");
            } else {
                ctx.log().warn(format!(
                    "SCRAPER_START_URL `{start}` is not an http(s) URL: the scraper idles"
                ));
            }
            // Idle until stopped; every tick is a heartbeat.
            let mut ticker = ctx.interval(60.secs());
            while ticker.tick().await {}
            return Ok(());
        };

        let mut crawl = match ctx.checkpoint_get::<Crawl>().await? {
            Some(saved) if saved.start == start => {
                ctx.log().info(format!(
                    "resuming the crawl of {start}: {} stored, {} queued",
                    saved.stored,
                    saved.queue.len()
                ));
                saved
            }
            _ => Crawl::starting_at(&start),
        };
        let db = ctx.db()?;

        while let Some(url) = crawl.queue.front().cloned() {
            if crawl.stored >= self.config.max_pages || ctx.is_cancelled() {
                break;
            }
            ctx.heartbeat();
            // `ctx.http()` waits for the host's rate limit, times out, and retries; an error that is left
            // fails the run, and the supervisor restarts it after a backoff.
            let res = ctx
                .http()
                .request(Method::GET, &url)
                .retries(self.config.retries)
                .send()
                .await?;
            let status = res.status();
            if status.is_server_error() {
                return Err(AgentError::msg(format!("{url} answered {status}")));
            }
            if status.is_success() {
                let html = res.text().await?;
                let title = title_of(&html).unwrap_or_else(|| url.clone());
                store_page(&db, &url, &title).await?;
                crawl.stored += 1;
                ctx.counter("pages").inc();
                for link in links_of(&html, &url, &origin) {
                    if crawl.seen.insert(link.clone()) {
                        crawl.queue.push_back(link);
                    }
                }
            } else {
                ctx.log().info(format!("skipped {url}: {status}"));
            }
            crawl.queue.pop_front();
            ctx.checkpoint(&crawl).await?;
        }
        if !ctx.is_cancelled() {
            ctx.log()
                .info(format!("crawl of {start} done: {} pages", crawl.stored));
        }
        Ok(())
    }
}

/// Inserts the page, or updates its title when the URL is stored already.
async fn store_page(db: &Db, url: &str, title: &str) -> smeltery::Result<()> {
    let existing = Page::query()
        .filter(page::Column::Url.eq(url))
        .one(db.conn())
        .await?;
    match existing {
        Some(found) => {
            found
                .update(db, |m| m.title = Set(title.to_owned()))
                .await?;
        }
        None => {
            Page::create(
                db,
                page::ActiveModel {
                    url: Set(url.to_owned()),
                    title: Set(title.to_owned()),
                    ..Default::default()
                },
            )
            .await?;
        }
    }
    Ok(())
}

/// `https://example.com:8080/a?b` → `https://example.com:8080` (lowercase); `None` for other schemes.
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    Some(format!("{scheme}://{}", authority.to_ascii_lowercase()))
}

/// The host of an http(s) URL, without the port: the key of its rate limit.
pub fn host_of(url: &str) -> Option<String> {
    let origin = origin_of(url)?;
    let authority = origin.split_once("://")?.1;
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => authority,
    };
    Some(host.to_owned())
}

/// The text of the first `<title>`, entities decoded and whitespace collapsed, at most 255 characters.
pub fn title_of(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let start = open + lower.get(open..)?.find('>')? + 1;
    let end = start + lower.get(start..)?.find("</title")?;
    let raw = html.get(start..end)?;
    let text = decode_entities(&raw.split_whitespace().collect::<Vec<_>>().join(" "));
    let text: String = text.chars().take(255).collect();
    (!text.is_empty()).then_some(text)
}

fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// The `<a href>` links of `html` (read from `base`) that stay on `origin`, without fragments, in page order.
pub fn links_of(html: &str, base: &str, origin: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower.get(from..).and_then(|rest| rest.find("href=")) {
        let attr = from + found;
        let at = attr + "href=".len();
        from = at;
        // Only links (`<a href>`), not stylesheets or icons (`<link href>`).
        let tag = lower
            .get(..attr)
            .and_then(|before| before.rfind('<'))
            .and_then(|open| lower.get(open + 1..attr))
            .and_then(|inside| inside.split_whitespace().next());
        if tag != Some("a") {
            continue;
        }
        let Some(quote) = html.get(at..at + 1) else {
            break;
        };
        if quote != "\"" && quote != "'" {
            continue;
        }
        let Some(len) = html.get(at + 1..).and_then(|rest| rest.find(quote)) else {
            break;
        };
        let href = html.get(at + 1..at + 1 + len).unwrap_or_default();
        if let Some(link) = resolve(base, origin, &decode_entities(href.trim()))
            && link.len() <= 255
            && !out.contains(&link)
        {
            out.push(link);
        }
    }
    out
}

/// `href` as an absolute URL on `origin`, or `None` (another site, `mailto:`, a fragment only, …).
fn resolve(base: &str, origin: &str, href: &str) -> Option<String> {
    let href = href.split('#').next().unwrap_or_default();
    if href.is_empty() {
        return None;
    }
    let absolute = if href.contains("://") {
        href.to_owned()
    } else if let Some(rest) = href.strip_prefix("//") {
        format!("{}://{rest}", origin.split_once("://")?.0)
    } else if href.starts_with('/') {
        format!("{origin}{href}")
    } else if href
        .split(['/', '?'])
        .next()
        .is_some_and(|first| first.contains(':'))
    {
        return None; // mailto:, javascript:, tel:, …
    } else {
        // Relative to the base page's directory.
        let path_start = base.find("://").map_or(0, |i| i + 3);
        let without_query = base.split(['?', '#']).next().unwrap_or(base);
        let dir_end = without_query
            .rfind('/')
            .filter(|&i| i >= path_start)
            .map_or(without_query.len(), |i| i + 1);
        let dir = without_query.get(..dir_end).unwrap_or(without_query);
        if dir.ends_with('/') {
            format!("{dir}{href}")
        } else {
            format!("{dir}/{href}")
        }
    };
    let link_origin = origin_of(&absolute)?;
    (link_origin == origin).then(|| {
        // The origin in its normalized (lowercase) form, the rest as written.
        let rest = absolute.split_once("://").map_or("", |(_, r)| r);
        let path = rest
            .find(['/', '?'])
            .map_or("/", |i| rest.get(i..).unwrap_or("/"));
        format!("{origin}{path}")
    })
}
