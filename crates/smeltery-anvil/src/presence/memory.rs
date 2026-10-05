//! The in-memory presence store: one process (PubSub driver `local`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use smeltery_core::{BoxFuture, Result};

use super::{Joined, Member, Store};

/// A member's display data and its sockets.
struct Entry {
    member: Member,
    sockets: HashSet<String>,
}

/// Members by channel, then by user id (ordered, so the member list is stable).
#[derive(Default)]
pub(crate) struct MemoryStore {
    channels: Mutex<HashMap<String, BTreeMap<String, Entry>>>,
}

impl MemoryStore {
    fn join_now(&self, channel: &str, socket: &str, member: &Member, max: usize) -> Joined {
        let mut channels = self.channels.lock().unwrap_or_else(PoisonError::into_inner);
        let users = channels.entry(channel.to_owned()).or_default();
        if !users.contains_key(member.user_id()) && users.len() >= max {
            if users.is_empty() {
                channels.remove(channel);
            }
            return Joined::Full;
        }
        let entry = users
            .entry(member.user_id().to_owned())
            .or_insert_with(|| Entry {
                member: member.clone(),
                sockets: HashSet::new(),
            });
        let added = entry.sockets.is_empty();
        entry.sockets.insert(socket.to_owned());
        let members = users.values().take(max).map(|e| e.member.clone()).collect();
        Joined::In { added, members }
    }

    fn leave_now(&self, channel: &str, socket: &str, user_id: &str) -> bool {
        let mut channels = self.channels.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(users) = channels.get_mut(channel) else {
            return false;
        };
        let Some(entry) = users.get_mut(user_id) else {
            return false;
        };
        if !entry.sockets.remove(socket) {
            return false;
        }
        let last = entry.sockets.is_empty();
        if last {
            users.remove(user_id);
            if users.is_empty() {
                channels.remove(channel);
            }
        }
        last
    }
}

impl Store for MemoryStore {
    fn join<'a>(
        &'a self,
        channel: &'a str,
        socket: &'a str,
        member: &'a Member,
        max: usize,
    ) -> BoxFuture<'a, Result<Joined>> {
        let joined = self.join_now(channel, socket, member, max);
        Box::pin(async move { Ok(joined) })
    }

    fn leave<'a>(
        &'a self,
        channel: &'a str,
        socket: &'a str,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<bool>> {
        let last = self.leave_now(channel, socket, user_id);
        Box::pin(async move { Ok(last) })
    }

    fn members<'a>(&'a self, channel: &'a str, max: usize) -> BoxFuture<'a, Result<Vec<Member>>> {
        let members = self
            .channels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(channel)
            .map(|users| users.values().take(max).map(|e| e.member.clone()).collect())
            .unwrap_or_default();
        Box::pin(async move { Ok(members) })
    }
}

#[cfg(test)]
mod tests {
    use futures_util::FutureExt as _;

    use super::*;

    #[test]
    fn a_user_is_one_member_however_many_sockets() {
        let store = MemoryStore::default();
        let ada = Member::new(7).info(serde_json::json!({ "name": "Ada" }));
        let bob = Member::new(8);
        let join = |socket: &str, member: &Member| {
            store
                .join("presence-room", socket, member, 2)
                .now_or_never()
                .unwrap()
                .unwrap()
        };
        assert!(matches!(join("1.1", &ada), Joined::In { added: true, .. }));
        let Joined::In { added, members } = join("1.2", &ada) else {
            panic!("joined")
        };
        assert!(!added, "a second tab");
        assert_eq!(members, vec![ada.clone()]);
        assert!(matches!(join("2.1", &bob), Joined::In { added: true, .. }));
        assert_eq!(join("3.1", &Member::new(9)), Joined::Full);
        assert!(
            matches!(join("1.3", &ada), Joined::In { added: false, .. }),
            "a member gets in when the channel is full"
        );
        let leave = |socket: &str, user: &str| {
            store
                .leave("presence-room", socket, user)
                .now_or_never()
                .unwrap()
                .unwrap()
        };
        assert!(!leave("1.1", "7"));
        assert!(!leave("1.2", "7"));
        assert!(leave("1.3", "7"), "the last tab");
        assert!(!leave("1.3", "7"), "once");
        assert!(leave("2.1", "8"));
        assert!(store.channels.lock().unwrap().is_empty(), "nothing left");
    }
}
