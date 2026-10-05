//! The `redis` driver (feature `redis`): `PUBLISH` on a multiplexed connection (redis's `ConnectionManager`, which
//! reconnects by itself), and one subscriber connection per process (`SUBSCRIBE`), checked with `PING` every 30 s and
//! reconnected with backoff when it ends or a `PING` goes unanswered.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{Stream, StreamExt as _};
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use super::{DRIVER_TIMEOUT, Outage, Shared, Transport, backoff};
use crate::app::BoxFuture;
use crate::error::{Error, Result};

fn client(url: &str) -> Result<redis::Client> {
    if url.starts_with("rediss:") && rustls::crypto::CryptoProvider::get_default().is_none() {
        // redis builds its TLS config with the process-wide provider; Smeltery uses ring.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    redis::Client::open(url)
        .map_err(|e| Error::internal(format!("REDIS_URL is not a valid Redis URL: {e}")))
}

/// Sends with `PUBLISH <channel> <sealed message>`.
pub(super) struct RedisTransport {
    client: redis::Client,
    conn: OnceCell<ConnectionManager>,
    channel: String,
}

impl RedisTransport {
    pub(super) fn new(url: &str, channel: &str) -> Result<Self> {
        Ok(Self {
            client: client(url)?,
            conn: OnceCell::new(),
            channel: channel.to_owned(),
        })
    }

    async fn conn(&self) -> Result<ConnectionManager> {
        let conn = self
            .conn
            .get_or_try_init(|| async {
                let config = ConnectionManagerConfig::new()
                    .set_connection_timeout(Some(DRIVER_TIMEOUT))
                    .set_response_timeout(Some(DRIVER_TIMEOUT))
                    .set_number_of_retries(1);
                ConnectionManager::new_with_config(self.client.clone(), config)
                    .await
                    .map_err(Error::other)
            })
            .await?;
        Ok(conn.clone())
    }
}

impl Transport for RedisTransport {
    fn send<'a>(&'a self, sealed: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut conn = self.conn().await?;
            let _: i64 = redis::cmd("PUBLISH")
                .arg(&self.channel)
                .arg(sealed)
                .query_async(&mut conn)
                .await
                .map_err(Error::other)?;
            Ok(())
        })
    }
}

/// Subscribe to `channel` and deliver its messages until shutdown, reconnecting with backoff (1 s to 30 s) and
/// logging once per outage.
pub(super) async fn subscribe_loop(
    shared: Arc<Shared>,
    url: String,
    channel: String,
    token: CancellationToken,
) {
    let mut outage = Outage::new("pubsub: the Redis subscription");
    let client = match client(&url) {
        Ok(client) => client,
        Err(e) => {
            outage.fail(&e);
            return;
        }
    };
    let mut failures = 0_u32;
    loop {
        if failures > 0 {
            tokio::select! {
                biased;
                () = token.cancelled() => return,
                () = tokio::time::sleep(backoff(failures - 1)) => {}
            }
        }
        let connect = async {
            let mut pubsub = client.get_async_pubsub().await.map_err(Error::other)?;
            pubsub.subscribe(&channel).await.map_err(Error::other)?;
            Ok::<_, Error>(pubsub)
        };
        let connected = tokio::select! {
            biased;
            () = token.cancelled() => return,
            connected = tokio::time::timeout(DRIVER_TIMEOUT, connect) => connected,
        };
        let pubsub = match connected {
            Ok(Ok(pubsub)) => pubsub,
            Ok(Err(e)) => {
                outage.fail(&e);
                failures = failures.saturating_add(1);
                continue;
            }
            Err(_) => {
                outage.fail(&format!("no answer within {DRIVER_TIMEOUT:?}"));
                failures = failures.saturating_add(1);
                continue;
            }
        };
        outage.ok();
        failures = 0;
        let (sink, stream) = pubsub.split();
        let messages =
            stream.filter_map(|message| async move { message.get_payload::<String>().ok() });
        let ping = move || {
            let mut sink = sink.clone();
            Box::pin(async move {
                sink.ping::<redis::Value>()
                    .await
                    .map(|_| ())
                    .map_err(Error::other)
            }) as BoxFuture<'static, Result<()>>
        };
        let ended = pump(
            Box::pin(messages),
            ping,
            |text| shared.receive(text),
            &token,
        )
        .await;
        match ended {
            Ended::Shutdown => return,
            Ended::Closed => outage.fail(&"the subscriber connection closed"),
            Ended::PingFailed => outage.fail(&format!(
                "the subscriber connection did not answer a PING within {DRIVER_TIMEOUT:?}"
            )),
        }
        failures = failures.saturating_add(1);
    }
}

/// How often the subscriber connection is checked with `PING`: a connection a NAT or load balancer dropped without
/// a FIN or RST never ends the message stream, so only an unanswered `PING` reveals it.
pub(super) const PING_EVERY: Duration = Duration::from_secs(30);

/// Why [`pump`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ended {
    Shutdown,
    Closed,
    PingFailed,
}

/// Deliver `messages` until shutdown, the end of the stream, or a `ping` that fails or takes longer than
/// [`DRIVER_TIMEOUT`] (sent every [`PING_EVERY`]).
pub(super) async fn pump<S, P>(
    mut messages: S,
    mut ping: P,
    deliver: impl Fn(&str),
    token: &CancellationToken,
) -> Ended
where
    S: Stream<Item = String> + Unpin,
    P: FnMut() -> BoxFuture<'static, Result<()>>,
{
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = token.cancelled() => return Ended::Shutdown,
            next = messages.next() => match next {
                Some(text) => deliver(&text),
                None => return Ended::Closed,
            },
            _ = tick.tick() => {
                let answered = tokio::select! {
                    biased;
                    () = token.cancelled() => return Ended::Shutdown,
                    answered = tokio::time::timeout(DRIVER_TIMEOUT, ping()) => answered,
                };
                if !matches!(answered, Ok(Ok(()))) {
                    return Ended::PingFailed;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Review M2: a silent connection (no message, no end) is noticed by the unanswered PING.
    #[tokio::test(start_paused = true)]
    async fn a_silent_connection_is_found_by_ping() {
        let token = CancellationToken::new();
        let pings = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&pings);
        // The server never answers: the PING hangs.
        let ping = move || {
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending::<Result<()>>()) as BoxFuture<'static, Result<()>>
        };
        let started = tokio::time::Instant::now();
        let ended = pump(
            futures_util::stream::pending::<String>(),
            ping,
            |_| {},
            &token,
        )
        .await;
        assert_eq!(ended, Ended::PingFailed);
        assert_eq!(pings.load(Ordering::SeqCst), 1);
        assert_eq!(started.elapsed(), PING_EVERY + DRIVER_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn answered_pings_keep_the_connection_and_messages_flow() {
        let token = CancellationToken::new();
        let ping = || Box::pin(async { Ok(()) }) as BoxFuture<'static, Result<()>>;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let stream = Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|m| (m, rx))
        }));
        let seen = std::sync::Mutex::new(Vec::new());
        let stopper = token.clone();
        tokio::spawn(async move {
            tx.send("a".to_owned()).unwrap();
            tokio::time::sleep(PING_EVERY * 5).await;
            tx.send("b".to_owned()).unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
            stopper.cancel();
            drop(tx);
        });
        let ended = pump(
            stream,
            ping,
            |m| seen.lock().unwrap().push(m.to_owned()),
            &token,
        )
        .await;
        assert_eq!(ended, Ended::Shutdown);
        assert_eq!(*seen.lock().unwrap(), ["a", "b"]);
        // A failing PING (an error answer) ends it as well; the end of the stream too.
        let failing =
            || Box::pin(async { Err(Error::internal("gone")) }) as BoxFuture<'static, Result<()>>;
        let ended = pump(
            futures_util::stream::pending::<String>(),
            failing,
            |_| {},
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(ended, Ended::PingFailed);
        let ended = pump(
            futures_util::stream::empty::<String>(),
            ping,
            |_| {},
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(ended, Ended::Closed);
    }
}
