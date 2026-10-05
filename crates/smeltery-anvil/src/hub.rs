//! The sockets of this process: who is connected, who listens on which channel, and the fan-out of an event to
//! them (D-407). Locks are never held across an `.await`.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use tokio::sync::{mpsc, watch};
use tungstenite::Utf8Bytes;

use crate::protocol::CloseCode;
use crate::revocation::{Holder, Revocation, Revocations};

/// One registered socket.
struct Conn {
    socket_id: Arc<str>,
    outbox: mpsc::Sender<Utf8Bytes>,
    close: watch::Sender<Option<CloseCode>>,
    channels: HashSet<Arc<str>>,
    client: Option<IpAddr>,
    /// Who authorized each of this socket's private subscriptions, by channel.
    holders: HashMap<Arc<str>, Holder>,
    /// Asked to close because an authorization ended: it left every channel and joins none.
    revoked: bool,
}

#[derive(Default)]
struct Registry {
    sockets: HashMap<u64, Conn>,
    channels: HashMap<Arc<str>, HashSet<u64>>,
    per_client: HashMap<Option<IpAddr>, usize>,
}

/// Why a socket was not registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `ANVIL_MAX_CONNECTIONS` sockets are open.
    Full,
    /// The client holds `ANVIL_MAX_CONNECTIONS_PER_IP` sockets.
    PerClient,
}

/// The sockets of this process.
pub(crate) struct Hub {
    registry: RwLock<Registry>,
    next: AtomicU64,
    max_connections: usize,
    /// Zero: no limit.
    max_per_client: usize,
    outbox: usize,
    /// Checked again when a subscription joins, under the registry lock (a revocation recorded between the session's
    /// check and the join is never missed: its scan runs after the record, the join's check after the scan).
    revocations: Arc<Revocations>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("connections", &self.connections())
            .finish_non_exhaustive()
    }
}

/// A registered socket's side: its outbox, its close requests, and the registration (dropping it unregisters).
pub(crate) struct Registered {
    pub(crate) outbox: mpsc::Receiver<Utf8Bytes>,
    pub(crate) close: watch::Receiver<Option<CloseCode>>,
    pub(crate) registration: Registration,
}

/// Unregisters its socket (and frees its place and its client's count) when dropped.
pub(crate) struct Registration {
    hub: Arc<Hub>,
    id: u64,
}

impl Registration {
    /// Deliver `channel`'s events to this socket; `holder` is who authorized it. `false` when a revocation now
    /// refuses the holder: the socket did not join and must close with 4200.
    #[must_use]
    pub(crate) fn join(&self, channel: &str, holder: Option<Holder>) -> bool {
        self.hub.join(self.id, channel, holder)
    }

    /// Stop delivering `channel`'s events to this socket.
    pub(crate) fn leave(&self, channel: &str) {
        self.hub.leave(self.id, channel);
    }
}

impl std::fmt::Debug for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registration").finish_non_exhaustive()
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.hub.unregister(self.id);
    }
}

/// The client a socket counts for: an IPv4 address, an IPv6 client by its /64 (`None`: unknown, counted together).
pub(crate) fn client_key(ip: Option<IpAddr>) -> Option<IpAddr> {
    ip.map(|ip| match ip.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(std::net::Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64))),
        v4 => v4,
    })
}

impl Hub {
    pub(crate) fn new(
        max_connections: usize,
        max_per_client: usize,
        outbox: usize,
        revocations: Arc<Revocations>,
    ) -> Self {
        Self {
            registry: RwLock::new(Registry::default()),
            next: AtomicU64::new(1),
            max_connections,
            max_per_client,
            outbox: outbox.max(1),
            revocations,
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Registry> {
        self.registry.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Registry> {
        self.registry
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Register a socket of client `ip` with `socket_id`, within the process and per-client caps.
    pub(crate) fn register(
        self: &Arc<Self>,
        socket_id: &str,
        ip: Option<IpAddr>,
    ) -> Result<Registered, Refusal> {
        let client = client_key(ip);
        let (outbox_tx, outbox) = mpsc::channel(self.outbox);
        let (close_tx, close) = watch::channel(None);
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut registry = self.write();
        if registry.sockets.len() >= self.max_connections {
            return Err(Refusal::Full);
        }
        let count = registry.per_client.entry(client).or_insert(0);
        if self.max_per_client > 0 && *count >= self.max_per_client {
            return Err(Refusal::PerClient);
        }
        *count += 1;
        registry.sockets.insert(
            id,
            Conn {
                socket_id: Arc::from(socket_id),
                outbox: outbox_tx,
                close: close_tx,
                channels: HashSet::new(),
                client,
                holders: HashMap::new(),
                revoked: false,
            },
        );
        drop(registry);
        Ok(Registered {
            outbox,
            close,
            registration: Registration {
                hub: Arc::clone(self),
                id,
            },
        })
    }

    fn unregister(&self, id: u64) {
        let mut registry = self.write();
        let Some(conn) = registry.sockets.remove(&id) else {
            return;
        };
        for channel in &conn.channels {
            if let Some(members) = registry.channels.get_mut(channel) {
                members.remove(&id);
                if members.is_empty() {
                    registry.channels.remove(channel);
                }
            }
        }
        if let Some(count) = registry.per_client.get_mut(&conn.client) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                registry.per_client.remove(&conn.client);
            }
        }
    }

    fn join(&self, id: u64, channel: &str, holder: Option<Holder>) -> bool {
        let mut registry = self.write();
        if holder
            .as_ref()
            .is_some_and(|h| self.revocations.refuses_holder(h))
        {
            return false;
        }
        let name: Arc<str> = registry
            .channels
            .get_key_value(channel)
            .map_or_else(|| Arc::from(channel), |(k, _)| Arc::clone(k));
        let Some(conn) = registry.sockets.get_mut(&id) else {
            return true;
        };
        if conn.revoked {
            // Closing because an authorization ended: it receives nothing more.
            return false;
        }
        conn.channels.insert(Arc::clone(&name));
        match holder {
            Some(holder) => conn.holders.insert(Arc::clone(&name), holder),
            None => conn.holders.remove(&name),
        };
        registry.channels.entry(name).or_default().insert(id);
        true
    }

    fn leave(&self, id: u64, channel: &str) {
        let mut registry = self.write();
        if let Some(conn) = registry.sockets.get_mut(&id) {
            conn.channels.remove(channel);
            conn.holders.remove(channel);
        }
        if let Some(members) = registry.channels.get_mut(channel) {
            members.remove(&id);
            if members.is_empty() {
                registry.channels.remove(channel);
            }
        }
    }

    /// Queue `frame` for every socket on `channel` except the one with socket id `except`; how many got it. A
    /// socket whose outbox is full is asked to close (4100: the client backs off, reconnects and fetches again).
    pub(crate) fn deliver(&self, channel: &str, except: Option<&str>, frame: &Utf8Bytes) -> usize {
        let registry = self.read();
        let Some(members) = registry.channels.get(channel) else {
            return 0;
        };
        let mut delivered = 0;
        for id in members {
            let Some(conn) = registry.sockets.get(id) else {
                continue;
            };
            if except.is_some_and(|skip| *conn.socket_id == *skip) {
                continue;
            }
            match conn.outbox.try_send(frame.clone()) {
                Ok(()) => delivered += 1,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    conn.close.send_replace(Some(CloseCode::OverCapacity));
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {}
            }
        }
        delivered
    }

    /// Close (4200: the client reconnects and authorizes again) every socket holding a subscription authorized
    /// with a credential `revocation` ended; how many. Each leaves every channel at once, so nothing published after
    /// the revocation is queued for it while its task gets to the close.
    pub(crate) fn revoke(&self, revocation: &Revocation) -> usize {
        self.close_where(|h| revocation.ends(h.user, &h.key))
    }

    /// Take socket `id` out of every channel and ask it to close with 4200.
    fn end(registry: &mut Registry, id: u64) {
        let Some(conn) = registry.sockets.get_mut(&id) else {
            return;
        };
        conn.revoked = true;
        conn.holders.clear();
        let channels: Vec<Arc<str>> = conn.channels.drain().collect();
        conn.close.send_replace(Some(CloseCode::Reconnect));
        for channel in channels {
            if let Some(members) = registry.channels.get_mut(&channel) {
                members.remove(&id);
                if members.is_empty() {
                    registry.channels.remove(&channel);
                }
            }
        }
    }

    /// The sockets holding a subscription whose grant was made before `limit` (Unix seconds): after this process fell
    /// behind the auth events, it cannot tell whether one of them ended the credential.
    pub(crate) fn granted_before(&self, limit: u64) -> Vec<u64> {
        self.read()
            .sockets
            .iter()
            .filter(|(_, conn)| conn.holders.values().any(|h| h.issued < limit))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Close (4200) socket `id` when it still holds a subscription whose grant was made before `limit`; whether it
    /// was.
    pub(crate) fn close_if_granted_before(&self, id: u64, limit: u64) -> bool {
        let mut registry = self.write();
        let old = registry
            .sockets
            .get(&id)
            .is_some_and(|conn| conn.holders.values().any(|h| h.issued < limit));
        if old {
            Self::end(&mut registry, id);
        }
        old
    }

    fn close_where(&self, ended: impl Fn(&Holder) -> bool) -> usize {
        let mut registry = self.write();
        let ids: Vec<u64> = registry
            .sockets
            .iter()
            .filter(|(_, conn)| conn.holders.values().any(&ended))
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            Self::end(&mut registry, *id);
        }
        ids.len()
    }

    /// Open sockets in this process.
    pub(crate) fn connections(&self) -> usize {
        self.read().sockets.len()
    }

    /// Sockets of this process subscribed to `channel`.
    pub(crate) fn subscribers(&self, channel: &str) -> usize {
        self.read().channels.get(channel).map_or(0, HashSet::len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub(max: usize, per_client: usize, outbox: usize) -> Arc<Hub> {
        Arc::new(Hub::new(max, per_client, outbox, Arc::default()))
    }

    fn holder(user: i64, key: &str, issued: u64) -> Option<Holder> {
        Some(Holder {
            user,
            key: key.into(),
            issued,
        })
    }

    fn frame(text: &str) -> Utf8Bytes {
        Utf8Bytes::from(text.to_owned())
    }

    #[test]
    fn caps_hold_per_process_and_per_client() {
        let hub = hub(3, 2, 4);
        let a: IpAddr = "203.0.113.7".parse().unwrap();
        let b: IpAddr = "2001:db8::1".parse().unwrap();
        let b_same_64: IpAddr = "2001:db8::ffff".parse().unwrap();
        let first = hub.register("1.1", Some(a)).ok().unwrap();
        let _second = hub.register("1.2", Some(a)).ok().unwrap();
        assert_eq!(hub.register("1.3", Some(a)).err(), Some(Refusal::PerClient));
        let _third = hub.register("1.4", Some(b)).ok().unwrap();
        assert_eq!(
            hub.register("1.5", Some(b_same_64)).err(),
            Some(Refusal::Full)
        );
        drop(first);
        assert_eq!(hub.connections(), 2);
        assert!(hub.register("1.6", Some(a)).is_ok(), "the place came back");
    }

    #[test]
    fn delivery_reaches_subscribers_except_the_named_socket() {
        let hub = hub(10, 0, 4);
        let mut one = hub.register("1.1", None).ok().unwrap();
        let mut two = hub.register("2.2", None).ok().unwrap();
        assert!(one.registration.join("news", None));
        assert!(two.registration.join("news", None));
        assert_eq!(hub.subscribers("news"), 2);
        assert_eq!(hub.deliver("news", Some("1.1"), &frame("x")), 1);
        assert!(one.outbox.try_recv().is_err());
        assert_eq!(two.outbox.try_recv().unwrap().as_str(), "x");
        two.registration.leave("news");
        assert_eq!(hub.deliver("news", None, &frame("y")), 1);
        drop(one);
        assert_eq!(
            hub.subscribers("news"),
            0,
            "an unregistered socket leaves its channels"
        );
        assert_eq!(hub.deliver("other", None, &frame("z")), 0);
    }

    #[test]
    fn revocations_close_the_sockets_they_concern() {
        use smeltery_core::auth::{AuthEvent, CredentialKind};
        let hub = hub(10, 0, 4);
        let mut mine = hub.register("1.1", None).ok().unwrap();
        let mut other_device = hub.register("1.2", None).ok().unwrap();
        let mut public = hub.register("1.3", None).ok().unwrap();
        assert!(
            mine.registration
                .join("private-a", holder(7, "web:session:aa", 100))
        );
        assert!(
            other_device
                .registration
                .join("private-a", holder(7, "fake:token:3", 100))
        );
        assert!(public.registration.join("news", None));
        let every_but_mine = crate::revocation::Revocation::from_event(&AuthEvent::RevokedAll {
            user_id: 7,
            kind: CredentialKind::Every,
            except: Some("web:session:aa".into()),
        })
        .unwrap();
        assert_eq!(hub.revoke(&every_but_mine), 1);
        assert_eq!(
            *other_device.close.borrow_and_update(),
            Some(CloseCode::Reconnect)
        );
        assert_eq!(
            *mine.close.borrow_and_update(),
            None,
            "the caller's socket stays"
        );
        assert_eq!(*public.close.borrow_and_update(), None);
    }

    /// Sweep W5-04: a revoked socket leaves its channels at once: nothing published after the revocation is queued
    /// for it while its task gets to the close, and it joins nothing more.
    #[test]
    fn a_revoked_socket_receives_nothing_more() {
        use smeltery_core::auth::AuthEvent;
        let hub = hub(10, 0, 8);
        let mut revoked = hub.register("1.1", None).ok().unwrap();
        let mut other = hub.register("1.2", None).ok().unwrap();
        assert!(
            revoked
                .registration
                .join("private-a", holder(7, "web:session:aa", 100))
        );
        assert!(revoked.registration.join("news", None));
        assert!(other.registration.join("news", None));
        let logout = crate::revocation::Revocation::from_event(&AuthEvent::Revoked {
            user_id: 7,
            key: "web:session:aa".into(),
        })
        .unwrap();
        assert_eq!(hub.revoke(&logout), 1);
        assert_eq!(
            *revoked.close.borrow_and_update(),
            Some(CloseCode::Reconnect)
        );
        assert_eq!(hub.deliver("private-a", None, &frame("after logout")), 0);
        assert_eq!(
            hub.deliver("news", None, &frame("news")),
            1,
            "the other socket only"
        );
        assert!(revoked.outbox.try_recv().is_err());
        assert!(
            !revoked.registration.join("news", None),
            "joins nothing more"
        );
        assert_eq!(other.outbox.try_recv().unwrap().as_str(), "news");
        drop(revoked);
        assert_eq!(hub.connections(), 1);
    }

    #[test]
    fn a_revocation_recorded_before_the_join_refuses_it() {
        // The race: the session's check passed, then a revocation was recorded and its scan ran, then the join.
        let revocations = Arc::new(Revocations::default());
        let hub = Arc::new(Hub::new(10, 0, 4, Arc::clone(&revocations)));
        let mut late = hub.register("1.1", None).ok().unwrap();
        let revocation =
            crate::revocation::Revocation::from_event(&smeltery_core::auth::AuthEvent::Revoked {
                user_id: 7,
                key: "fake:token:3".into(),
            })
            .unwrap();
        revocations.record(revocation.clone(), 100);
        assert_eq!(hub.revoke(&revocation), 0, "the scan found nothing yet");
        assert!(
            !late
                .registration
                .join("private-a", holder(7, "fake:token:3", 90)),
            "the join sees the revocation"
        );
        assert_eq!(hub.subscribers("private-a"), 0);
        assert_eq!(*late.close.borrow_and_update(), None);
        assert!(
            late.registration
                .join("private-a", holder(7, "fake:token:4", 90)),
            "another credential joins"
        );
    }

    #[test]
    fn leaving_a_channel_forgets_who_authorized_it() {
        let hub = hub(10, 0, 4);
        let mut socket = hub.register("1.1", None).ok().unwrap();
        assert!(
            socket
                .registration
                .join("private-a", holder(7, "fake:token:3", 100))
        );
        socket.registration.leave("private-a");
        let revocation =
            crate::revocation::Revocation::from_event(&smeltery_core::auth::AuthEvent::Revoked {
                user_id: 7,
                key: "fake:token:3".into(),
            })
            .unwrap();
        assert_eq!(hub.revoke(&revocation), 0);
        assert_eq!(*socket.close.borrow_and_update(), None);
    }

    #[test]
    fn falling_behind_closes_the_sockets_with_older_grants() {
        let hub = hub(10, 0, 4);
        let mut old = hub.register("1.1", None).ok().unwrap();
        let mut fresh = hub.register("1.2", None).ok().unwrap();
        let mut public = hub.register("1.3", None).ok().unwrap();
        assert!(
            old.registration
                .join("private-a", holder(7, "fake:token:3", 100))
        );
        assert!(
            fresh
                .registration
                .join("private-a", holder(8, "fake:token:4", 101))
        );
        assert!(public.registration.join("news", None));
        let ids = hub.granted_before(101);
        assert_eq!(ids.len(), 1);
        assert!(hub.close_if_granted_before(ids[0], 101));
        assert!(!hub.close_if_granted_before(u64::MAX, 101), "gone");
        assert_eq!(*old.close.borrow_and_update(), Some(CloseCode::Reconnect));
        assert_eq!(*fresh.close.borrow_and_update(), None);
        assert_eq!(*public.close.borrow_and_update(), None);
    }

    #[test]
    fn a_full_outbox_asks_the_socket_to_close() {
        let hub = hub(10, 0, 2);
        let mut slow = hub.register("1.1", None).ok().unwrap();
        assert!(slow.registration.join("news", None));
        assert_eq!(hub.deliver("news", None, &frame("1")), 1);
        assert_eq!(hub.deliver("news", None, &frame("2")), 1);
        assert_eq!(hub.deliver("news", None, &frame("3")), 0);
        assert_eq!(
            *slow.close.borrow_and_update(),
            Some(CloseCode::OverCapacity)
        );
    }
}
