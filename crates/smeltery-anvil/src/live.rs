//! What sockets do besides receiving events: join and leave presence channels (with `member_added` /
//! `member_removed` for every member, in every process) and send client events (D-408, D-414).

use std::sync::{Arc, PoisonError};
use std::time::Duration;

use serde_json::Value;
use smeltery_core::pubsub::{Driver, PubSub};
use smeltery_core::{App, Error, Result};
use tungstenite::Utf8Bytes;

use crate::presence::{self, Joined, Member, Store, timed};
use crate::{Anvil, TOPIC, Wire, protocol};

/// `Wire::k` of a member event.
pub(crate) const MEMBER: &str = "m";
/// `Wire::k` of a client event.
pub(crate) const WHISPER: &str = "w";

/// Client events one channel may carry a second in this process (whatever its subscriber count: the fan-out grows
/// with the square of the subscribers otherwise).
pub(crate) const CHANNEL_CLIENT_EVENTS_PER_SECOND: u32 = 100;

/// Channels whose client-event budget is tracked at once (more are refused until a second passes).
const MAX_BUDGETED_CHANNELS: usize = 10_000;

/// Client addresses whose client-event budget is tracked at once; past it, further addresses count against the
/// process's budget only (never a lockout of new clients).
const MAX_BUDGETED_CLIENTS: usize = 10_000;

/// The client-event budgets of one second: per channel, per client address and for the whole process. Every client
/// event reaches the other processes too (a row in the app's database with the `database` driver), so neither one
/// address nor all of them together may send more than their share.
#[derive(Debug, Default)]
pub(crate) struct WhisperBudgets {
    channels: std::collections::HashMap<String, (std::time::Instant, u32)>,
    clients: std::collections::HashMap<Option<std::net::IpAddr>, (std::time::Instant, u32)>,
    process: Option<(std::time::Instant, u32)>,
}

/// The channel spent its budget (the `pusher:error` 4301 message).
pub(crate) const CHANNEL_BUDGET_SPENT: &str = "client event rate limit reached on this channel";
/// The client address spent its budget.
pub(crate) const CLIENT_BUDGET_SPENT: &str = "client event rate limit reached for this address";
/// The process spent its budget.
pub(crate) const PROCESS_BUDGET_SPENT: &str = "client event rate limit reached on this server";

/// The count of a one-second window at `now` (a new window once a second has passed).
fn window(entry: &mut (std::time::Instant, u32), now: std::time::Instant) -> &mut u32 {
    if now.duration_since(entry.0) >= Duration::from_secs(1) {
        *entry = (now, 0);
    }
    &mut entry.1
}

/// What a presence join answered.
#[derive(Debug)]
pub(crate) enum JoinAnswer {
    /// `subscription_succeeded` with the members (the frame).
    In(String),
    /// The channel is full: `subscription_error` 403.
    Full,
}

impl Anvil {
    /// The presence store: the one of the process's PubSub driver (in memory until a driver is chosen).
    pub(crate) fn store(&self) -> Arc<dyn Store> {
        if let Some(store) = self.inner.store.get() {
            return Arc::clone(store);
        }
        let Some(driver) = self.inner.pubsub.get().and_then(PubSub::driver) else {
            return Arc::clone(&self.inner.memory) as Arc<dyn Store>;
        };
        let store: Arc<dyn Store> = match self.shared_store(driver) {
            Ok(Some(store)) => store,
            Ok(None) => Arc::clone(&self.inner.memory) as Arc<dyn Store>,
            Err(error) => {
                tracing::error!(%error, "anvil: the presence store could not start; presence stays in this process");
                Arc::clone(&self.inner.memory) as Arc<dyn Store>
            }
        };
        Arc::clone(self.inner.store.get_or_init(|| store))
    }

    fn shared_store(&self, driver: Driver) -> Result<Option<Arc<dyn Store>>> {
        let Some(env) = self.inner.env.get() else {
            return Ok(None);
        };
        let database = || {
            env.db.clone().map(|db| {
                Arc::new(presence::database::DatabaseStore::new(
                    db,
                    self.inner.process.clone(),
                )) as Arc<dyn Store>
            })
        };
        Ok(match driver {
            Driver::Database => Some(database().ok_or_else(|| {
                Error::internal("PUBSUB_DRIVER=database, but the app has no database")
            })?),
            #[cfg(feature = "redis")]
            Driver::Redis => Some(Arc::new(presence::redis::RedisStore::new(
                &env.redis_url,
                &env.cache_prefix,
                self.inner.process.clone(),
            )?)),
            // Without the feature, a database is the shared store the processes can agree on.
            #[cfg(not(feature = "redis"))]
            Driver::Redis => database(),
            _ => None,
        })
    }

    /// The members of the presence channel `channel` (the full name, `presence-rooms.1`), from every process; at
    /// most `ANVIL_MAX_PRESENCE_MEMBERS`.
    ///
    /// # Errors
    /// `channel` is not a presence channel name, or the presence store failed.
    pub async fn members(&self, channel: &str) -> Result<Vec<Member>> {
        if !channel.starts_with("presence-") || !protocol::valid_channel(channel) {
            return Err(Error::internal(format!(
                "`{}` is not a presence channel name",
                channel.escape_debug()
            )));
        }
        let max = self.inner.settings.max_presence_members;
        timed(self.store().members(channel, max)).await
    }

    /// Join `socket` to `channel` as `member`; announce `member_added` to the others when it is the user's first
    /// socket there. The membership is known to this process (for the heartbeat's checks and the socket's cleanup)
    /// from before the store call; a failed or cut-off join is undone.
    pub(crate) async fn presence_join(
        &self,
        socket: &str,
        channel: &str,
        member: &Member,
    ) -> Result<JoinAnswer> {
        let memberships = &self.inner.memberships;
        memberships.joining(channel, socket, member);
        let max = self.inner.settings.max_presence_members;
        let joined = timed(self.store().join(channel, socket, member, max)).await;
        match joined {
            Ok(Joined::In { added, members }) => {
                memberships.joined(channel, socket);
                if added {
                    self.announce(
                        "pusher_internal:member_added",
                        channel,
                        &protocol::member_added_data(member),
                        Some(socket),
                    );
                }
                Ok(JoinAnswer::In(protocol::presence_succeeded(
                    channel, &members,
                )))
            }
            Ok(Joined::Full) => {
                memberships.forget(channel, socket);
                Ok(JoinAnswer::Full)
            }
            Err(error) => {
                // The join may have stopped between its statements: undo what it did (the heartbeat removes what
                // this cannot, the membership being gone).
                memberships.forget(channel, socket);
                if let Ok(true) = timed(self.store().leave(channel, socket, member.user_id())).await
                {
                    self.removed(channel, member.user_id());
                }
                Err(error)
            }
        }
    }

    /// `subscription_succeeded` with the members of `channel`, for a socket in it already.
    pub(crate) async fn presence_list(&self, channel: &str) -> Result<String> {
        let members = self.members(channel).await?;
        Ok(protocol::presence_succeeded(channel, &members))
    }

    /// Take `socket` (of `user_id`) out of `channel`; announce `member_removed` when it was the user's last socket.
    pub(crate) async fn presence_leave(&self, socket: &str, channel: &str, user_id: &str) {
        // Forgotten first: a heartbeat between the two steps removes the row itself, never puts it back.
        self.inner.memberships.forget(channel, socket);
        match timed(self.store().leave(channel, socket, user_id)).await {
            Ok(true) => self.removed(channel, user_id),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, "anvil: a presence leave failed; the heartbeat removes the member")
            }
        }
    }

    /// Take `socket` out of every presence channel it is in (it closed).
    pub(crate) async fn presence_leave_socket(&self, socket: &str) {
        for (channel, user_id) in self.memberships_of(socket) {
            self.presence_leave(socket, &channel, &user_id).await;
        }
    }

    /// The presence channels `socket` is in, with its user id in each.
    pub(crate) fn memberships_of(&self, socket: &str) -> Vec<(String, String)> {
        self.inner.memberships.of_socket(socket)
    }

    /// Announce that `user_id` left `channel`.
    pub(crate) fn removed(&self, channel: &str, user_id: &str) {
        self.announce(
            "pusher_internal:member_removed",
            channel,
            &protocol::member_removed_data(user_id),
            None,
        );
    }

    /// A member event to `channel`'s sockets here (except `except`) and in the other processes.
    fn announce(&self, name: &str, channel: &str, data: &str, except: Option<&str>) {
        let frame = Utf8Bytes::from(protocol::event(name, channel, data));
        self.inner.hub.deliver(channel, except, &frame);
        let wire = Wire {
            e: name.to_owned(),
            c: vec![channel.to_owned()],
            d: data.to_owned(),
            x: except.map(str::to_owned),
            k: Some(MEMBER.to_owned()),
            u: None,
        };
        self.forward(&wire);
    }

    /// A client event from `socket` (of client address `client`) to the other subscribers of `channel`, here and in
    /// the other processes; `Err` with the 4301 message when the channel's, the address's or the process's budget
    /// for this second is spent (the event is dropped).
    pub(crate) fn whisper(
        &self,
        socket: &str,
        client: Option<std::net::IpAddr>,
        channel: &str,
        event: &str,
        data: &Value,
        user_id: Option<&str>,
    ) -> std::result::Result<(), &'static str> {
        self.whisper_budget(channel, client)?;
        let frame = Utf8Bytes::from(protocol::client_event(event, channel, data, user_id));
        self.inner.hub.deliver(channel, Some(socket), &frame);
        let wire = Wire {
            e: event.to_owned(),
            c: vec![channel.to_owned()],
            d: data.to_string(),
            x: Some(socket.to_owned()),
            k: Some(WHISPER.to_owned()),
            u: user_id.map(str::to_owned),
        };
        self.forward(&wire);
        Ok(())
    }

    /// Count a client event against the budgets of this second: its channel's
    /// ([`CHANNEL_CLIENT_EVENTS_PER_SECOND`]), its client address's (`ANVIL_CLIENT_EVENTS_PER_CLIENT`) and the
    /// process's (`ANVIL_CLIENT_EVENTS_PER_SECOND`). Nothing is counted when one of them is spent.
    fn whisper_budget(
        &self,
        channel: &str,
        client: Option<std::net::IpAddr>,
    ) -> std::result::Result<(), &'static str> {
        let now = std::time::Instant::now();
        let second = Duration::from_secs(1);
        let settings = &self.inner.settings;
        let mut guard = self
            .inner
            .whisper_budget
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let budgets = &mut *guard;
        let process = budgets.process.get_or_insert((now, 0));
        if *window(process, now) >= settings.client_events_per_second {
            return Err(PROCESS_BUDGET_SPENT);
        }
        let channels = &mut budgets.channels;
        if channels.len() >= MAX_BUDGETED_CHANNELS && !channels.contains_key(channel) {
            channels.retain(|_, (start, _)| now.duration_since(*start) < second);
            if channels.len() >= MAX_BUDGETED_CHANNELS {
                return Err(CHANNEL_BUDGET_SPENT);
            }
        }
        let on_channel = channels.entry(channel.to_owned()).or_insert((now, 0));
        if *window(on_channel, now) >= CHANNEL_CLIENT_EVENTS_PER_SECOND {
            return Err(CHANNEL_BUDGET_SPENT);
        }
        let key = crate::hub::client_key(client);
        let clients = &mut budgets.clients;
        if clients.len() >= MAX_BUDGETED_CLIENTS && !clients.contains_key(&key) {
            clients.retain(|_, (start, _)| now.duration_since(*start) < second);
        }
        if clients.len() < MAX_BUDGETED_CLIENTS || clients.contains_key(&key) {
            let of_client = clients.entry(key).or_insert((now, 0));
            let count = window(of_client, now);
            if *count >= settings.client_events_per_client {
                return Err(CLIENT_BUDGET_SPENT);
            }
            *count += 1;
        }
        // The channel and the process count it only once every budget had room.
        if let Some(on_channel) = budgets.channels.get_mut(channel) {
            *window(on_channel, now) += 1;
        }
        if let Some(process) = budgets.process.as_mut() {
            *window(process, now) += 1;
        }
        Ok(())
    }

    /// Hand a member or client event to the other processes without waiting (a full queue drops it, counted by
    /// the PubSub).
    fn forward(&self, wire: &Wire) {
        let Some(pubsub) = self.inner.pubsub.get() else {
            return;
        };
        if let Ok(value) = serde_json::to_value(wire) {
            let _ = pubsub.forward_reserved(TOPIC, &value);
        }
    }

    /// A member or client event from another process: checked, then delivered here; how many sockets got it.
    pub(crate) fn deliver_remote_live(&self, wire: Wire) -> usize {
        let refused = || {
            tracing::warn!("anvil: an event from another process was refused");
            0
        };
        let [channel] = wire.c.as_slice() else {
            return refused();
        };
        if !protocol::valid_channel(channel)
            || wire
                .x
                .as_deref()
                .is_some_and(|x| !protocol::valid_socket_id(x))
        {
            return refused();
        }
        let frame = match wire.k.as_deref() {
            Some(MEMBER) => {
                let named = matches!(
                    wire.e.as_str(),
                    "pusher_internal:member_added" | "pusher_internal:member_removed"
                );
                let user_ok = serde_json::from_str::<Value>(&wire.d)
                    .ok()
                    .and_then(|d| d.get("user_id").and_then(Value::as_str).map(str::to_owned))
                    .is_some_and(|id| Member::new(id).valid_id());
                if !named || !channel.starts_with("presence-") || !user_ok {
                    return refused();
                }
                protocol::event(&wire.e, channel, &wire.d)
            }
            Some(WHISPER) => {
                let Ok(data) = serde_json::from_str::<Value>(&wire.d) else {
                    return refused();
                };
                if !wire.e.starts_with("client-")
                    || wire.e.len() > 200
                    || !self.inner.channels.whispers(channel)
                    || wire
                        .u
                        .as_deref()
                        .is_some_and(|u| u.is_empty() || u.len() > presence::MAX_USER_ID)
                {
                    return refused();
                }
                protocol::client_event(&wire.e, channel, &data, wire.u.as_deref())
            }
            _ => return refused(),
        };
        self.inner
            .hub
            .deliver(channel, wire.x.as_deref(), &Utf8Bytes::from(frame))
    }

    /// Keep this process's presence rows alive and right (the store checks them against the memberships this process
    /// knows), and remove those of processes that are gone, until shutdown; then remove this process's rows. Every
    /// member that left or came back is announced.
    pub(crate) async fn presence_heartbeat(self, token: tokio_util::sync::CancellationToken) {
        loop {
            tokio::select! {
                () = token.cancelled() => break,
                () = tokio::time::sleep(presence::HEARTBEAT) => {}
            }
            self.beat().await;
        }
        // The sockets leave one by one as they close; what is left of this process goes now.
        match timed(self.store().clear()).await {
            Ok(removed) => {
                for (channel, user_id) in removed {
                    self.removed(&channel, &user_id);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "anvil: the presence rows of this process could not be removed")
            }
        }
    }

    /// One heartbeat.
    pub(crate) async fn beat(&self) {
        match timed(self.store().heartbeat(&self.inner.memberships)).await {
            Ok(beat) => {
                for (channel, member) in beat.added {
                    self.announce(
                        "pusher_internal:member_added",
                        &channel,
                        &protocol::member_added_data(&member),
                        None,
                    );
                }
                for (channel, user_id) in beat.removed {
                    self.removed(&channel, &user_id);
                }
            }
            Err(error) => tracing::warn!(%error, "anvil: the presence heartbeat failed"),
        }
    }

    /// Remember what the presence store needs from the app (its database, its Redis settings).
    pub(crate) fn remember_app(&self, app: &App) {
        let settings = app.settings();
        let _ = self.inner.env.set(StoreEnv {
            db: app.db().ok(),
            redis_url: settings.redis_url.clone(),
            cache_prefix: settings.cache_prefix.clone(),
        });
    }
}

/// What a shared presence store is built from (kept instead of the app: the app holds Anvil).
pub(crate) struct StoreEnv {
    db: Option<smeltery_core::db::Db>,
    #[cfg_attr(not(feature = "redis"), allow(dead_code))]
    redis_url: String,
    #[cfg_attr(not(feature = "redis"), allow(dead_code))]
    cache_prefix: String,
}
