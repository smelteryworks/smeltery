//! The code a generated app writes for Watchfire (the M5 contract), through `smeltery::` paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod app {
    pub mod agents {
        use smeltery::watchfire::prelude::*;

        pub mod price_poller {
            use smeltery::watchfire::prelude::*;

            /// Polls prices.
            #[derive(Default)]
            pub struct PricePoller {
                last: Option<String>,
            }

            impl Agent for PricePoller {
                fn name(&self) -> String {
                    "price_poller".into()
                }

                fn config(&self) -> AgentConfig {
                    AgentConfig::default()
                        .restart(Restart::OnFailure)
                        .backoff(1.secs()..=60.secs())
                        .heartbeat_timeout(2.mins())
                }

                async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
                    let mut ticker = ctx.interval(30.secs());
                    while ticker.tick().await {
                        self.last = Some("42".into());
                        ctx.log().info("polled");
                    }
                    Ok(())
                }
            }
        }

        pub mod fetcher {
            use smeltery::watchfire::prelude::*;

            /// One fetcher of a pool.
            pub struct Fetcher {
                index: usize,
            }

            impl Fetcher {
                pub fn new(index: usize) -> Self {
                    Self { index }
                }
            }

            impl Agent for Fetcher {
                fn name(&self) -> String {
                    format!("fetcher{}", self.index)
                }

                async fn run(&mut self, ctx: AgentCtx) -> Result<(), AgentError> {
                    ctx.cancelled().await;
                    Ok(())
                }
            }
        }
        // smeltery:mods

        pub fn register(w: &mut Watchfire) {
            w.every(30.secs(), "heartbeat", |ctx| async move {
                ctx.log().info("tick");
                Ok(())
            });
            w.run("scraper", |ctx| async move {
                ctx.cancelled().await;
                Ok(())
            })
            .restart(Restart::OnFailure)
            .backoff(1.secs()..=30.secs());
            w.on_event(
                "post.created",
                "notify",
                |_ctx, _event| async move { Ok(()) },
            );
            w.agent(price_poller::PricePoller::default());
            w.pool(4, "fetcher", fetcher::Fetcher::new)
                .group("scrapers");
            w.group("scrapers").limit(2);
            w.rate_limit("example.com", 2.per_second());
            w.job::<crate::app::jobs::send_welcome::SendWelcome>();
            w.job::<crate::app::jobs::Report>();
            w.schedule()
                .job(crate::app::jobs::Report::default())
                .daily_at("03:00");
            w.schedule()
                .call("cleanup", |_ctx| async move { Ok(()) })
                .every(5.mins())
                .overlap(Overlap::Skip);
            w.schedule().agent("scraper").cron("0 3 * * *");
            // smeltery:agents
        }
    }

    pub mod jobs {
        use serde::{Deserialize, Serialize};
        use smeltery::watchfire::prelude::*;

        pub mod send_welcome {
            use serde::{Deserialize, Serialize};
            use smeltery::watchfire::prelude::*;

            #[derive(Serialize, Deserialize)]
            pub struct SendWelcome {
                pub user_id: i64,
            }

            impl Job for SendWelcome {
                const NAME: &'static str = "send_welcome";

                async fn handle(&self, ctx: JobCtx) -> Result<(), AgentError> {
                    ctx.log().info(format!("welcome {}", self.user_id));
                    Ok(())
                }
            }
        }

        #[derive(Serialize, Deserialize, Default)]
        pub struct Report {
            pub day: Option<String>,
        }

        impl Job for Report {
            const NAME: &'static str = "report";

            fn max_attempts(&self) -> u32 {
                5
            }

            async fn handle(&self, _ctx: JobCtx) -> Result<(), AgentError> {
                Ok(())
            }
        }
    }
}

mod database {
    use smeltery::Result;
    use smeltery::db::migration::{Migration, Schema};

    pub struct CreateWatchfireTables;

    impl Migration for CreateWatchfireTables {
        fn name(&self) -> &'static str {
            "2026_10_03_000000_create_watchfire_tables"
        }

        async fn up(&self, schema: &Schema) -> Result<()> {
            smeltery::watchfire::migrations::up(schema).await
        }

        async fn down(&self, schema: &Schema) -> Result<()> {
            smeltery::watchfire::migrations::down(schema).await
        }
    }
}

mod bootstrap {
    use smeltery::AppBuilder;
    use smeltery::watchfire::AgentsExt as _;

    pub fn build(app: AppBuilder) -> AppBuilder {
        app.migrations(|m| {
            m.add(crate::database::CreateWatchfireTables);
        })
        .agents(crate::app::agents::register)
    }
}

use app::jobs::send_welcome::SendWelcome;
use smeltery::prelude::*;
use smeltery::testing::TestApp;
use smeltery::watchfire::Job as _;

async fn welcome(app: App) -> Result<&'static str> {
    SendWelcome { user_id: 1 }.dispatch(&app).await?;
    SendWelcome { user_id: 2 }
        .dispatch_later(&app, smeltery::watchfire::DurationExt::mins(10))
        .await?;
    Ok("queued")
}

#[test]
fn generated_app_code_builds_dispatches_and_lists_its_schedule() {
    let t = TestApp::new(|b| {
        bootstrap::build(b).routes(|r| {
            r.get("/welcome", welcome);
        })
    });
    assert_eq!(t.get("/welcome").text(), "queued");
    let queue = t.app().service::<smeltery::watchfire::Queue>().unwrap();
    assert_eq!(t.block_on(queue.stats()).unwrap().pending, 2);

    let mut out = Vec::new();
    let code = t
        .block_on(smeltery::console::dispatch(
            bootstrap::build(AppBuilder::new(smeltery::config::Settings::from_env())),
            &["schedule:list".to_owned()],
            &mut out,
        ))
        .unwrap();
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("report") && text.contains("0 3 * * *"),
        "{text}"
    );
    assert!(
        text.contains("cleanup") && text.contains("every 5m"),
        "{text}"
    );
    assert!(text.contains("agent:scraper"), "{text}");
}

#[test]
fn the_registration_is_valid() {
    let mut w = smeltery::watchfire::Watchfire::new();
    app::agents::register(&mut w);
    w.validate().unwrap();
    assert_eq!(
        w.agent_names(),
        [
            "heartbeat",
            "scraper",
            "notify",
            "price_poller",
            "fetcher#0",
            "fetcher#1",
            "fetcher#2",
            "fetcher#3"
        ]
    );
}
