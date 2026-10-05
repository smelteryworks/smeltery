//! The app's channels: which public channels exist, who may join which private and presence ones (D-413, D-414),
//! and where clients may send client events (D-408).

use std::future::Future;
use std::str::FromStr;
use std::sync::Arc;

use smeltery_core::auth::{Auth, Authenticatable, Principal};
use smeltery_core::db::Db;
use smeltery_core::{App, BoxFuture, Error, Result};

use crate::presence::Member;
use crate::protocol::{Kind, channel_byte};

/// A private channel's authorization callback.
pub(crate) type Callback =
    Arc<dyn Fn(ChannelCtx) -> BoxFuture<'static, Result<bool>> + Send + Sync>;

/// A presence channel's authorization callback: the member to join as, `None` to refuse.
pub(crate) type PresenceCallback =
    Arc<dyn Fn(ChannelCtx) -> BoxFuture<'static, Result<Option<Member>>> + Send + Sync>;

/// The callback of a private or presence pattern.
#[derive(Clone)]
pub(crate) enum Authorizer {
    Private(Callback),
    Presence(PresenceCallback),
}

/// A private or presence pattern that matched: its callback, the parameters, and whether guests may ask.
pub(crate) type PrivateMatch = (Authorizer, Vec<(String, String)>, bool);

/// One segment of a channel pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// Matches exactly this text.
    Literal(String),
    /// `{name}`: matches one segment (no `.`).
    Param(String),
}

/// A channel pattern: dot-separated segments, `{name}` matching one segment.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pattern {
    text: String,
    segments: Vec<Segment>,
}

impl Pattern {
    fn parse(text: &str) -> std::result::Result<Self, String> {
        let invalid = |why: &str| format!("the channel pattern `{text}` is invalid: {why}");
        if text.is_empty() {
            return Err(invalid("it is empty"));
        }
        if text.starts_with("private-") || text.starts_with("presence-") {
            return Err(invalid(
                "write it without the `private-` prefix (`c.private(\"orders.{order}\")` serves `private-orders.7`)",
            ));
        }
        let mut segments = Vec::new();
        let mut names = Vec::new();
        for part in text.split('.') {
            if let Some(name) = part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                let ok = name
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
                if !ok {
                    return Err(invalid("a parameter name is letters, digits and `_`"));
                }
                if names.contains(&name) {
                    return Err(invalid("a parameter name appears twice"));
                }
                names.push(name);
                segments.push(Segment::Param(name.to_owned()));
            } else if !part.is_empty() && part.bytes().all(channel_byte) {
                segments.push(Segment::Literal(part.to_owned()));
            } else {
                return Err(invalid(
                    "each `.`-separated part is `{name}` or letters, digits and `_ - = @ , ;`",
                ));
            }
        }
        Ok(Self {
            text: text.to_owned(),
            segments,
        })
    }

    /// The parameters when `name` (without its prefix) matches.
    fn matches(&self, name: &str) -> Option<Vec<(String, String)>> {
        let parts: Vec<&str> = name.split('.').collect();
        if parts.len() != self.segments.len() {
            return None;
        }
        let mut params = Vec::new();
        for (segment, part) in self.segments.iter().zip(parts) {
            match segment {
                Segment::Literal(text) if text == part => {}
                Segment::Param(param) if !part.is_empty() && part.bytes().all(channel_byte) => {
                    params.push((param.clone(), part.to_owned()));
                }
                _ => return None,
            }
        }
        Some(params)
    }
}

/// A registered channel pattern.
struct Entry {
    kind: Kind,
    pattern: Pattern,
    callback: Option<Authorizer>,
    guests: bool,
    whispers: bool,
}

/// The app's channels, declared in `routes/channels.rs` and installed with
/// [`AnvilExt::anvil`](crate::AnvilExt::anvil).
///
/// - [`public`](Self::public) declares a public channel: anyone with the app key may subscribe, so its events are
///   public. A public name that is not declared is refused.
/// - [`private`](Self::private) declares `private-<pattern>` channels and the callback that decides who may join
///   them; the client asks `POST /broadcasting/auth` for a signature before it subscribes.
/// - [`presence`](Self::presence) declares `presence-<pattern>` channels: like private ones, but the callback names
///   the [`Member`] the user joins as, and every member sees who else is there.
/// - [`whispers`](PrivateChannel::whispers) on a private or presence registration lets its subscribers send client
///   events (`client-…`) to each other; without it they are refused.
///
/// Patterns are dot-separated; `{name}` matches one segment (letters, digits and `_ - = @ , ;`). A private or
/// presence pattern is written without its prefix. A channel name that matches no pattern of its kind is refused. Two
/// registrations of one pattern and kind, or an invalid pattern, fail the app's build.
///
/// ```
/// use smeltery::anvil::{ChannelCtx, Channels, Member};
///
/// pub fn register(c: &mut Channels) {
///     c.public("news");
///     c.public("scores.{game}");
///     c.private("orders.{order}", |ctx: ChannelCtx| async move {
///         let order: i64 = ctx.param("order")?;
///         let Some(user_id) = ctx.user_id() else { return Ok(false) };
///         // The order's owner, from the database.
///         let rows = ctx
///             .db()?
///             .query_with("SELECT user_id FROM orders WHERE id = ?", [order.into()])
///             .await?;
///         let owner: Option<i64> = rows.first().and_then(|row| row.try_get("", "user_id").ok());
///         Ok(owner == Some(user_id))
///     });
///     // presence-rooms.{room}: signed-in users, shown by id; they may send client events (typing indicators).
///     c.presence("rooms.{room}", |ctx: ChannelCtx| async move {
///         Ok(ctx.user_id().map(Member::new))
///     })
///     .whispers();
/// }
/// ```
#[derive(Default)]
pub struct Channels {
    entries: Vec<Entry>,
    errors: Vec<String>,
}

impl std::fmt::Debug for Channels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let patterns: Vec<String> = self
            .entries
            .iter()
            .map(|e| format!("{:?} {}", e.kind, e.pattern.text))
            .collect();
        f.debug_struct("Channels")
            .field("patterns", &patterns)
            .finish_non_exhaustive()
    }
}

/// A private or presence channel registration, returned by [`Channels::private`] and [`Channels::presence`].
#[derive(Debug)]
pub struct PrivateChannel<'a> {
    flags: Option<(&'a mut bool, &'a mut bool)>,
}

impl PrivateChannel<'_> {
    /// Let guests (visitors who are not signed in) reach the callback too; [`ChannelCtx::user_id`] is `None` for
    /// them. Without it, guests are refused before the callback runs.
    pub fn guests(mut self) -> Self {
        if let Some((guests, _)) = self.flags.as_mut() {
            **guests = true;
        }
        self
    }

    /// Let the subscribers of these channels send client events (`client-…`, at most 10 a second per socket) to
    /// the other subscribers. Without it, a client event is answered with `pusher:error` 4009.
    pub fn whispers(mut self) -> Self {
        if let Some((_, whispers)) = self.flags.as_mut() {
            **whispers = true;
        }
        self
    }
}

impl Channels {
    /// No channels.
    pub fn new() -> Self {
        Self::default()
    }

    fn add(
        &mut self,
        kind: Kind,
        pattern: &str,
        callback: Option<Authorizer>,
    ) -> Option<&mut Entry> {
        let pattern = match Pattern::parse(pattern) {
            Ok(pattern) => pattern,
            Err(e) => {
                self.errors.push(e);
                return None;
            }
        };
        if self
            .entries
            .iter()
            .any(|e| e.kind == kind && e.pattern.text == pattern.text)
        {
            self.errors.push(format!(
                "the channel pattern `{}` is registered twice",
                pattern.text
            ));
            return None;
        }
        self.entries.push(Entry {
            kind,
            pattern,
            callback,
            guests: false,
            whispers: false,
        });
        self.entries.last_mut()
    }

    /// Declare the public channel `pattern` (`news`, `scores.{game}`). Its events reach everyone who knows the
    /// app key.
    pub fn public(&mut self, pattern: &str) -> &mut Self {
        self.add(Kind::Public, pattern, None);
        self
    }

    /// Declare the private channels `private-<pattern>` and who may join them: `callback` gets a [`ChannelCtx`]
    /// and returns `Ok(true)` to allow. A returned error answers 403 when its status is a client error (as a
    /// denial) and is logged and answered with its status when it is a server error.
    pub fn private<F, Fut>(&mut self, pattern: &str, callback: F) -> PrivateChannel<'_>
    where
        F: Fn(ChannelCtx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<bool>> + Send + 'static,
    {
        let callback: Callback = Arc::new(move |ctx| Box::pin(callback(ctx)));
        PrivateChannel {
            flags: self
                .add(Kind::Private, pattern, Some(Authorizer::Private(callback)))
                .map(|e| (&mut e.guests, &mut e.whispers)),
        }
    }

    /// Declare the presence channels `presence-<pattern>` and who joins them as whom: `callback` gets a
    /// [`ChannelCtx`] and returns `Ok(Some(member))` to let the user in as that [`Member`] (its `user_info` reaches
    /// every member of the channel: display fields only), `Ok(None)` to refuse. Errors are answered as on
    /// [`private`](Self::private).
    pub fn presence<F, Fut>(&mut self, pattern: &str, callback: F) -> PrivateChannel<'_>
    where
        F: Fn(ChannelCtx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Member>>> + Send + 'static,
    {
        let callback: PresenceCallback = Arc::new(move |ctx| Box::pin(callback(ctx)));
        PrivateChannel {
            flags: self
                .add(
                    Kind::Presence,
                    pattern,
                    Some(Authorizer::Presence(callback)),
                )
                .map(|e| (&mut e.guests, &mut e.whispers)),
        }
    }

    /// The registration errors (invalid or repeated patterns).
    pub(crate) fn errors(&self) -> &[String] {
        &self.errors
    }

    /// Whether the public channel `name` is declared.
    pub(crate) fn is_public(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|e| e.kind == Kind::Public && e.pattern.matches(name).is_some())
    }

    /// The first pattern of `kind` (private or presence) matching `name` (without its prefix): its callback,
    /// parameters and whether guests may ask.
    pub(crate) fn authorizer(&self, kind: Kind, name: &str) -> Option<PrivateMatch> {
        self.entries.iter().find_map(|e| {
            if e.kind != kind {
                return None;
            }
            let params = e.pattern.matches(name)?;
            Some((e.callback.clone()?, params, e.guests))
        })
    }

    /// Whether the channel `full_name` (with its prefix) allows client events: its first matching private or
    /// presence pattern was registered with `.whispers()`.
    pub(crate) fn whispers(&self, full_name: &str) -> bool {
        let (kind, name) = Kind::of(full_name);
        if !matches!(kind, Kind::Private | Kind::Presence) {
            return false;
        }
        self.entries
            .iter()
            .find(|e| e.kind == kind && e.pattern.matches(name).is_some())
            .is_some_and(|e| e.whispers)
    }
}

/// Who asks to join a channel.
#[derive(Clone, Debug)]
pub(crate) enum Asker {
    /// A browser with the app's session (`POST /broadcasting/auth`).
    Session(Auth),
    /// A client with a stateless guard's bearer credential (`POST /api/broadcasting/auth`).
    Principal(Principal),
}

/// What a private channel's callback knows: the channel, its parameters and who asks.
#[derive(Clone)]
pub struct ChannelCtx {
    app: App,
    channel: String,
    socket_id: String,
    params: Arc<Vec<(String, String)>>,
    asker: Asker,
}

impl std::fmt::Debug for ChannelCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelCtx")
            .field("channel", &self.channel)
            .field("user_id", &self.user_id())
            .finish_non_exhaustive()
    }
}

impl ChannelCtx {
    pub(crate) fn new(
        app: App,
        channel: String,
        socket_id: String,
        params: Vec<(String, String)>,
        asker: Asker,
    ) -> Self {
        Self {
            app,
            channel,
            socket_id,
            params: Arc::new(params),
            asker,
        }
    }

    /// The pattern parameter `name` (`{order}` in `orders.{order}`), parsed.
    ///
    /// # Errors
    /// 403 when the value does not parse (the request is refused like a denial); a server error when the pattern
    /// has no such parameter.
    pub fn param<T: FromStr>(&self, name: &str) -> Result<T> {
        let Some((_, value)) = self.params.iter().find(|(n, _)| n == name) else {
            return Err(Error::internal(format!(
                "the channel pattern of `{}` has no parameter `{name}`",
                self.channel
            )));
        };
        value.parse().map_err(|_| Error::forbidden())
    }

    /// The signed-in user's id; `None` for a guest.
    pub fn user_id(&self) -> Option<i64> {
        match &self.asker {
            Asker::Session(auth) => auth.id(),
            Asker::Principal(principal) => Some(principal.user_id),
        }
    }

    /// The signed-in user (loaded once); `None` for a guest.
    ///
    /// # Errors
    /// The query fails, or `U` is not the user model registered with `.auth::<…>()`.
    pub async fn user<U: Authenticatable>(&self) -> Result<Option<U>> {
        match &self.asker {
            Asker::Session(auth) => auth.user::<U>().await,
            Asker::Principal(principal) => principal.user::<U>(&self.app).await,
        }
    }

    /// The app's database.
    ///
    /// # Errors
    /// The app has no database.
    pub fn db(&self) -> Result<Db> {
        self.app.db()
    }

    /// The app.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The principal of a request to `POST /api/broadcasting/auth` (its guard, credential and abilities); `None`
    /// on the cookie endpoint.
    pub fn principal(&self) -> Option<&Principal> {
        match &self.asker {
            Asker::Principal(principal) => Some(principal),
            Asker::Session(_) => None,
        }
    }

    /// The full channel name (`private-orders.7`).
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// The socket the client subscribes with.
    pub fn socket_id(&self) -> &str {
        &self.socket_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_match_whole_segments() {
        let p = Pattern::parse("orders.{order}").unwrap();
        assert_eq!(
            p.matches("orders.7"),
            Some(vec![("order".to_owned(), "7".to_owned())])
        );
        assert_eq!(p.matches("orders.7.items"), None);
        assert_eq!(p.matches("orders"), None);
        assert_eq!(p.matches("orders."), None);
        assert_eq!(p.matches("ordersx.7"), None);
        let q = Pattern::parse("chat.{a}.{b}").unwrap();
        assert_eq!(q.matches("chat.1.2").unwrap().len(), 2);
        let literal = Pattern::parse("news").unwrap();
        assert!(literal.matches("news").is_some());
        assert!(literal.matches("news2").is_none());
    }

    #[test]
    fn invalid_and_repeated_patterns_are_errors() {
        for bad in [
            "",
            "a..b",
            "a.{}",
            "a.{1x}",
            "a.{x}.{x}",
            "private-orders",
            "a b",
            "a.{x",
        ] {
            assert!(Pattern::parse(bad).is_err(), "{bad}");
        }
        let mut c = Channels::new();
        c.public("news");
        c.public("news");
        c.private("orders.{o}", |_| async { Ok(true) });
        c.private("orders.{o}", |_| async { Ok(true) });
        c.private("bad pattern", |_| async { Ok(true) }).guests();
        assert_eq!(c.errors().len(), 3, "{:?}", c.errors());
    }

    #[test]
    fn kinds_are_kept_apart() {
        let mut c = Channels::new();
        c.public("orders.{o}");
        c.private("secret.{s}", |_| async { Ok(true) }).guests();
        assert!(c.is_public("orders.1"));
        assert!(!c.is_public("secret.1"));
        assert!(c.authorizer(Kind::Private, "orders.1").is_none());
        let (_, params, guests) = c.authorizer(Kind::Private, "secret.1").unwrap();
        assert_eq!(params, vec![("s".to_owned(), "1".to_owned())]);
        assert!(guests);
        assert!(c.authorizer(Kind::Presence, "secret.1").is_none());
    }

    #[test]
    fn client_events_are_allowed_only_where_a_pattern_opted_in() {
        let mut c = Channels::new();
        c.public("news");
        c.private("quiet.{q}", |_| async { Ok(true) });
        c.private("chat.{c}", |_| async { Ok(true) }).whispers();
        c.presence("room.{r}", |_| async { Ok(None) })
            .guests()
            .whispers();
        c.presence("hall.{h}", |_| async { Ok(None) });
        assert!(c.whispers("private-chat.1"));
        assert!(c.whispers("presence-room.1"));
        assert!(!c.whispers("private-quiet.1"));
        assert!(!c.whispers("presence-hall.1"));
        assert!(!c.whispers("news"), "never on public channels");
        assert!(!c.whispers("presence-chat.1"), "the kinds are apart");
        let (authorizer, _, guests) = c.authorizer(Kind::Presence, "room.1").unwrap();
        assert!(matches!(authorizer, Authorizer::Presence(_)) && guests);
    }
}
