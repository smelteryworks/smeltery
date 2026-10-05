//! Revocation: core's auth events (PubSub topic `auth`) close the sockets whose private subscriptions were
//! authorized with an ended credential, and refuse grants made before the event (D-415).

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use smeltery_core::auth::{AuthEvent, CredentialKind};

use crate::signature::{GRANT_LIFETIME, Grant};

/// The most events remembered (the oldest are forgotten first).
pub(crate) const MAX_REMEMBERED: usize = 10_000;

/// How long an event is remembered: longer than a grant can be used to subscribe (it expires after
/// [`GRANT_LIFETIME`]; a grant dated up to twice that ahead is refused as a clock error).
const REMEMBER_FOR: Duration = Duration::from_secs(GRANT_LIFETIME.as_secs() * 3);

/// Which credentials of a user an event ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ended {
    /// One credential, by key (`session:<binding>`, `token:<id>`).
    One(String),
    /// Every credential of a kind, except one key.
    All {
        sessions: bool,
        tokens: bool,
        except: Option<String>,
    },
}

/// A revocation: a user and the credentials that ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Revocation {
    pub(crate) user: i64,
    pub(crate) ended: Ended,
}

impl Revocation {
    /// From core's event; `None` for an event this version does not know.
    pub(crate) fn from_event(event: &AuthEvent) -> Option<Self> {
        match event {
            AuthEvent::Revoked { user_id, key } => Some(Self {
                user: *user_id,
                ended: Ended::One(key.clone()),
            }),
            AuthEvent::RevokedAll {
                user_id,
                kind,
                except,
            } => {
                let (sessions, tokens) = match kind {
                    CredentialKind::Sessions => (true, false),
                    CredentialKind::Tokens => (false, true),
                    // `Every`, and any kind added later: end everything (fail closed).
                    _ => (true, true),
                };
                Some(Self {
                    user: *user_id,
                    ended: Ended::All {
                        sessions,
                        tokens,
                        except: except.clone(),
                    },
                })
            }
            _ => None,
        }
    }

    /// Every credential of `user` (an event this version cannot read in full: fail closed).
    pub(crate) fn everything_of(user: i64) -> Self {
        Self {
            user,
            ended: Ended::All {
                sessions: true,
                tokens: true,
                except: None,
            },
        }
    }

    /// Whether the credential `key` of user `user` ended.
    pub(crate) fn ends(&self, user: i64, key: &str) -> bool {
        if user != self.user {
            return false;
        }
        match &self.ended {
            Ended::One(ended) => ended == key,
            Ended::All {
                sessions,
                tokens,
                except,
            } => {
                let kind = key.split(':').nth(1);
                let kind_matches =
                    (*sessions && kind == Some("session")) || (*tokens && kind == Some("token"));
                kind_matches && except.as_deref() != Some(key)
            }
        }
    }
}

/// How far the processes' clocks may disagree: a grant dated up to this long after a matching revocation (by the
/// issuing process's clock) is refused too, so a grant made just before the revocation on a process whose clock runs
/// ahead cannot slip through (D-415 amendment).
pub(crate) const CLOCK_MARGIN: Duration = Duration::from_secs(30);

/// Who authorized one private subscription of a socket: the user, the credential key and when the grant was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Holder {
    pub(crate) user: i64,
    pub(crate) key: String,
    pub(crate) issued: u64,
}

impl Holder {
    /// The holder a grant names; `None` for a guest's grant (no credential to end).
    pub(crate) fn of(grant: &Grant) -> Option<Self> {
        Some(Self {
            user: grant.user?,
            key: grant.credential.as_ref()?.key().to_owned(),
            issued: grant.issued,
        })
    }
}

#[derive(Debug, Default)]
struct Memory {
    recent: VecDeque<(u64, Revocation)>,
    /// Every credential-bound grant made before this time (Unix seconds) is refused: what the process cannot vouch
    /// for (before it started, events it fell behind on, events forgotten while a grant they refuse can still be
    /// used).
    refuse_before: u64,
}

/// The recent revocations of this process, so a grant made before one cannot subscribe afterwards. What it does
/// not know it refuses: grants made before the process started, before an event it forgot early (the memory is
/// full) or before it fell behind the events.
#[derive(Debug, Default)]
pub(crate) struct Revocations {
    memory: Mutex<Memory>,
}

impl Revocations {
    /// A memory that refuses the grants made before `start` (Unix seconds): a process that just started has not
    /// seen the events before it.
    pub(crate) fn starting_at(start: u64) -> Self {
        Self {
            memory: Mutex::new(Memory {
                recent: VecDeque::new(),
                refuse_before: start,
            }),
        }
    }

    fn memory(&self) -> std::sync::MutexGuard<'_, Memory> {
        self.memory.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Remember `revocation` as of `now` (Unix seconds).
    pub(crate) fn record(&self, revocation: Revocation, now: u64) {
        let mut memory = self.memory();
        while memory
            .recent
            .front()
            .is_some_and(|(at, _)| at.saturating_add(REMEMBER_FOR.as_secs()) < now)
        {
            memory.recent.pop_front();
        }
        if memory.recent.len() >= MAX_REMEMBERED
            && let Some((at, _)) = memory.recent.pop_front()
        {
            // Forgotten while a grant it refuses may still be used: refuse every grant it would have refused.
            let until = at.saturating_add(CLOCK_MARGIN.as_secs()).saturating_add(1);
            memory.refuse_before = memory.refuse_before.max(until);
        }
        memory.recent.push_back((now, revocation));
    }

    /// This process starts listening to the auth events at `now` (Unix seconds): it has not seen the events before,
    /// so grants made before that second are refused.
    pub(crate) fn listening_from(&self, now: u64) {
        let mut memory = self.memory();
        memory.refuse_before = memory.refuse_before.max(now);
    }

    /// This process missed events at `now` (Unix seconds): refuse every credential-bound grant made up to that
    /// second; the new limit.
    pub(crate) fn fell_behind(&self, now: u64) -> u64 {
        let mut memory = self.memory();
        memory.refuse_before = memory.refuse_before.max(now.saturating_add(1));
        memory.refuse_before
    }

    /// Whether `grant` was made before a remembered revocation of its credential (within [`CLOCK_MARGIN`]), or
    /// before what this process can vouch for.
    pub(crate) fn refuses(&self, grant: &Grant) -> bool {
        Holder::of(grant).is_some_and(|holder| self.refuses_holder(&holder))
    }

    /// [`refuses`](Self::refuses) for a subscription's holder.
    pub(crate) fn refuses_holder(&self, holder: &Holder) -> bool {
        let memory = self.memory();
        holder.issued < memory.refuse_before
            || memory.recent.iter().any(|(at, revocation)| {
                at.saturating_add(CLOCK_MARGIN.as_secs()) >= holder.issued
                    && revocation.ends(holder.user, &holder.key)
            })
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.memory().recent.len()
    }
}

/// The least time between two mass closes after lags of the `auth` subscription.
pub(crate) const MASS_CLOSE_EVERY: Duration = Duration::from_secs(60);

/// A mass close is spread over this long (each socket at a random moment), so the clients do not all come back at
/// once.
pub(crate) const MASS_CLOSE_SPREAD: Duration = Duration::from_secs(30);

/// After the `auth` subscription lagged: which sockets close when (D-403 / D-415 amendment 2). Each lag raises the
/// watermark at once (new grants are refused); the sockets holding older grants close in a mass close that starts at
/// the next second (so the reconnecting clients get grants that pass), at most one a minute (lags in between join
/// the next one), each socket at a random moment within [`MASS_CLOSE_SPREAD`].
#[derive(Debug, Default)]
pub(crate) struct LagCloser {
    /// The next mass close: when, and the grants (made before this Unix second) whose sockets close.
    mass: Option<(tokio::time::Instant, u64)>,
    last_mass: Option<tokio::time::Instant>,
    /// Sockets to close: when, which (hub id) and the limit their grants are checked against.
    spread: Vec<(tokio::time::Instant, u64, u64)>,
}

impl LagCloser {
    /// The subscription lagged at `now`; `to_next_second` is the time until the clock's next second, `limit` the
    /// raised watermark.
    pub(crate) fn lagged(
        &mut self,
        now: tokio::time::Instant,
        to_next_second: Duration,
        limit: u64,
    ) {
        let mut at = now + to_next_second;
        let mut limit = limit;
        if let Some(last) = self.last_mass {
            at = at.max(last + MASS_CLOSE_EVERY);
        }
        if let Some((planned, planned_limit)) = self.mass {
            at = at.max(planned);
            limit = limit.max(planned_limit);
        }
        self.mass = Some((at, limit));
    }

    /// When something is due next.
    pub(crate) fn next(&self) -> Option<tokio::time::Instant> {
        let spread = self.spread.iter().map(|(at, _, _)| *at).min();
        match (self.mass.map(|(at, _)| at), spread) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// What is due at `now`: a due mass close takes the sockets `holding` names (those with grants before its
    /// limit) and spreads them with `delay`; the sockets to close now, with their limits.
    pub(crate) fn due(
        &mut self,
        now: tokio::time::Instant,
        holding: impl FnOnce(u64) -> Vec<u64>,
        mut delay: impl FnMut() -> Duration,
    ) -> Vec<(u64, u64)> {
        if let Some((at, limit)) = self.mass
            && at <= now
        {
            self.mass = None;
            self.last_mass = Some(now);
            for id in holding(limit) {
                self.spread.push((now + delay(), id, limit));
            }
        }
        let mut close = Vec::new();
        self.spread.retain(|(at, id, limit)| {
            if *at <= now {
                close.push((*id, *limit));
                false
            } else {
                true
            }
        });
        close
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signature::Credential;

    fn all(kind: CredentialKind, except: Option<&str>) -> Revocation {
        Revocation::from_event(&AuthEvent::RevokedAll {
            user_id: 7,
            kind,
            except: except.map(str::to_owned),
        })
        .unwrap()
    }

    #[test]
    fn events_match_the_credentials_they_end() {
        let one = Revocation::from_event(&AuthEvent::Revoked {
            user_id: 7,
            key: "web:session:aa".into(),
        })
        .unwrap();
        assert!(one.ends(7, "web:session:aa"));
        assert!(!one.ends(7, "web:session:bb"));
        assert!(!one.ends(8, "web:session:aa"), "another user");

        let sessions = all(CredentialKind::Sessions, Some("web:session:aa"));
        assert!(sessions.ends(7, "web:session:bb"));
        assert!(
            !sessions.ends(7, "web:session:aa"),
            "the caller's credential stays"
        );
        assert!(!sessions.ends(7, "fake:token:1"));

        let tokens = all(CredentialKind::Tokens, None);
        assert!(tokens.ends(7, "fake:token:1"));
        assert!(!tokens.ends(7, "web:session:aa"));

        let every = all(CredentialKind::Every, Some("fake:token:2"));
        assert!(every.ends(7, "fake:token:1") && every.ends(7, "web:session:aa"));
        assert!(!every.ends(7, "fake:token:2"));
    }

    fn grant_for(user: i64, key: &str, issued: u64) -> Grant {
        Grant {
            user: Some(user),
            credential: Credential::from_key(key),
            issued,
            expires: issued + 300,
        }
    }

    fn grant(issued: u64) -> Grant {
        grant_for(7, "fake:token:1", issued)
    }

    fn revoked(user_id: i64, key: &str) -> Revocation {
        Revocation::from_event(&AuthEvent::Revoked {
            user_id,
            key: key.into(),
        })
        .unwrap()
    }

    #[test]
    fn a_grant_made_before_a_revocation_is_refused() {
        let memory = Revocations::default();
        assert!(!memory.refuses(&grant(100)));
        memory.record(revoked(7, "fake:token:1"), 150);
        assert!(memory.refuses(&grant(100)), "made before the revocation");
        assert!(memory.refuses(&grant(150)), "made in the same second");
        assert!(
            memory.refuses(&grant(180)),
            "within the clock margin: the issuing process's clock may run ahead"
        );
        assert!(!memory.refuses(&grant(181)), "made afterwards");
        assert!(
            !memory.refuses(&grant_for(7, "fake:token:2", 100)),
            "another credential"
        );
        let guest = Grant {
            user: None,
            credential: None,
            issued: 1,
            expires: 300,
        };
        assert!(!memory.refuses(&guest));
    }

    #[test]
    fn the_memory_is_bounded_in_size_and_time() {
        let memory = Revocations::default();
        let event = || all(CredentialKind::Every, None);
        for _ in 0..(MAX_REMEMBERED + 10) {
            memory.record(event(), 1_000);
        }
        assert_eq!(memory.len(), MAX_REMEMBERED);
        memory.record(event(), 1_000 + REMEMBER_FOR.as_secs() + 1);
        assert_eq!(memory.len(), 1, "old events are forgotten");
    }

    #[test]
    fn a_grant_made_before_a_forgotten_event_is_still_refused() {
        let memory = Revocations::default();
        memory.record(revoked(7, "fake:token:1"), 1_000);
        // Enough later events (another user's) to push it out while its grants can still be used.
        for second in 0..MAX_REMEMBERED as u64 {
            memory.record(revoked(8, "fake:token:2"), 1_000 + second / 1_000);
        }
        assert_eq!(memory.len(), MAX_REMEMBERED);
        assert!(
            memory.refuses(&grant(999)),
            "made before the forgotten event"
        );
        assert!(
            memory.refuses(&grant_for(9, "fake:token:3", 1_030)),
            "every grant it could have refused is refused"
        );
        assert!(!memory.refuses(&grant_for(9, "fake:token:3", 1_031)));
    }

    #[test]
    fn events_forgotten_by_age_refuse_nothing_more() {
        let memory = Revocations::default();
        memory.record(revoked(7, "fake:token:1"), 1_000);
        let later = 1_000 + REMEMBER_FOR.as_secs() + 1;
        memory.record(revoked(8, "fake:token:2"), later);
        assert_eq!(memory.len(), 1);
        assert!(!memory.refuses(&grant_for(9, "fake:token:3", 1_001)));
    }

    #[test]
    fn falling_behind_refuses_every_earlier_grant() {
        let memory = Revocations::default();
        assert_eq!(memory.fell_behind(500), 501);
        assert!(memory.refuses(&grant_for(9, "web:session:aa", 500)));
        assert!(memory.refuses(&grant(1)));
        assert!(!memory.refuses(&grant(501)), "made after the lag");
        assert_eq!(memory.fell_behind(400), 501, "the limit never goes back");
        let guest = Grant {
            user: None,
            credential: None,
            issued: 1,
            expires: 300,
        };
        assert!(!memory.refuses(&guest), "a guest grant ends with nothing");
    }

    #[test]
    fn a_new_process_refuses_grants_from_before_it_started() {
        let memory = Revocations::starting_at(2_000);
        assert!(memory.refuses(&grant(1_999)));
        assert!(!memory.refuses(&grant(2_000)));
    }

    #[test]
    fn listening_raises_the_start() {
        let memory = Revocations::starting_at(1_000);
        memory.listening_from(1_500);
        assert!(memory.refuses(&grant(1_499)));
        assert!(!memory.refuses(&grant(1_500)));
        memory.listening_from(10);
        assert!(memory.refuses(&grant(1_499)), "never lowered");
    }

    #[test]
    fn mass_closes_start_at_the_next_second_are_spread_and_come_at_most_once_a_minute() {
        use tokio::time::Instant;
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut closer = LagCloser::default();
        assert_eq!(closer.next(), None);
        closer.lagged(t0, Duration::from_millis(400), 101);
        assert_eq!(closer.next(), Some(t0 + Duration::from_millis(400)));
        assert!(
            closer.due(t0, |_| panic!("not yet"), || s(0)).is_empty(),
            "nothing before the next second"
        );
        // The mass close: two sockets, spread 5 s and 20 s.
        let at = t0 + Duration::from_millis(400);
        let mut delays = [s(5), s(20)].into_iter();
        let closed = closer.due(
            at,
            |limit| {
                assert_eq!(limit, 101);
                vec![1, 2]
            },
            || delays.next().unwrap(),
        );
        assert!(closed.is_empty(), "each at its own moment");
        assert_eq!(closer.next(), Some(at + s(5)));
        assert_eq!(closer.due(at + s(5), |_| vec![], || s(0)), vec![(1, 101)]);
        assert_eq!(closer.due(at + s(20), |_| vec![], || s(0)), vec![(2, 101)]);
        assert_eq!(closer.next(), None);

        // Two more lags within the minute: one mass close, a minute after the last, with the newest limit.
        closer.lagged(at + s(25), Duration::from_millis(100), 130);
        closer.lagged(at + s(26), Duration::from_millis(100), 131);
        assert_eq!(closer.next(), Some(at + s(60)));
        assert!(
            closer
                .due(at + s(59), |_| panic!("not yet"), || s(0))
                .is_empty()
        );
        let closed = closer.due(
            at + s(60),
            |limit| {
                assert_eq!(limit, 131);
                vec![3]
            },
            || s(0),
        );
        assert_eq!(closed, vec![(3, 131)]);
    }
}
