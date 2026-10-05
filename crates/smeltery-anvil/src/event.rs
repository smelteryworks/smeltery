//! Events, channels and socket ids (D-411).

use std::borrow::Cow;

use serde::Serialize;
use smeltery_core::{App, Error};

use crate::protocol::valid_socket_id;

/// A channel an event goes to.
///
/// ```
/// use smeltery::anvil::Channel;
///
/// assert_eq!(Channel::public("news").name(), "news");
/// assert_eq!(Channel::private("orders.7").name(), "private-orders.7");
/// assert_eq!(Channel::presence("rooms.1").name(), "presence-rooms.1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Channel {
    name: String,
}

impl Channel {
    /// The public channel `name`.
    pub fn public(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// The private channel `private-<name>`.
    pub fn private(name: impl AsRef<str>) -> Self {
        Self {
            name: format!("private-{}", name.as_ref()),
        }
    }

    /// The presence channel `presence-<name>` (its members receive the event).
    pub fn presence(name: impl AsRef<str>) -> Self {
        Self {
            name: format!("presence-{}", name.as_ref()),
        }
    }

    /// The name on the wire (`private-orders.7`).
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// An event that can be broadcast: its data is what it serializes to (`serde` attributes decide the fields, and
/// `#[serde(skip)]` keeps one out), sent to [`channels`](Self::channels) under [`name`](Self::name).
///
/// `#[derive(BroadcastEvent)]` implements it from `#[broadcast(...)]`:
///
/// ```
/// use serde::Serialize;
/// use smeltery::anvil::{BroadcastEvent, Channel};
///
/// #[derive(Serialize, BroadcastEvent)]
/// #[broadcast(private = "orders.{order_id}")]
/// struct OrderShipped {
///     order_id: i64,
///     tracking: String,
/// }
///
/// let event = OrderShipped { order_id: 7, tracking: "1Z".into() };
/// assert_eq!(event.channels(), vec![Channel::private("orders.7")]);
/// assert_eq!(event.name(), "App\\Events\\OrderShipped");
/// ```
///
/// Event data on a public channel is public: anyone who knows the app key can subscribe to it.
pub trait BroadcastEvent: Serialize {
    /// The channels the event goes to.
    fn channels(&self) -> Vec<Channel>;

    /// The event name clients listen for. The default is `App\Events\<type name>`, the name laravel-echo's
    /// `.listen("OrderShipped")` listens for; a client listening for another name writes it with a leading dot
    /// (`.listen(".order.shipped")`).
    fn name(&self) -> Cow<'static, str> {
        Cow::Owned(default_name(std::any::type_name::<Self>()))
    }
}

/// `App\Events\<last path segment of the type, without generics>`.
pub(crate) fn default_name(type_name: &str) -> String {
    let base = type_name.split('<').next().unwrap_or(type_name);
    let last = base.rsplit("::").next().unwrap_or(base);
    format!("App\\Events\\{last}")
}

/// The socket a request came from: the `X-Socket-ID` header the client sends with its requests, to leave that
/// socket out of the events the request causes ([`PendingEvent::except`](crate::PendingEvent::except)).
///
/// Only the shape `<digits>.<digits>` (1 to 10 digits each) is accepted; anything else counts as absent. Take it
/// as `Option<SocketId>`: a request without the header gets `None`.
///
/// A client can name another socket's id, which keeps that socket from receiving the events this request causes;
/// it reveals nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SocketId(String);

impl SocketId {
    /// A socket id, when `id` has the shape.
    pub fn parse(id: &str) -> Option<Self> {
        valid_socket_id(id).then(|| Self(id.to_owned()))
    }

    /// The id (`1736205712.948201645`).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_headers(headers: &http::HeaderMap) -> Option<Self> {
        let mut values = headers.get_all("x-socket-id").iter();
        let first = values.next()?;
        if values.next().is_some() {
            return None;
        }
        first.to_str().ok().and_then(|v| Self::parse(v.trim()))
    }
}

impl std::fmt::Display for SocketId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl axum::extract::FromRequestParts<App> for SocketId {
    type Rejection = Error;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _app: &App,
    ) -> Result<Self, Self::Rejection> {
        Self::from_headers(&parts.headers)
            .ok_or_else(|| Error::bad_request("the X-Socket-ID header is missing or invalid"))
    }
}

impl axum::extract::OptionalFromRequestParts<App> for SocketId {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _app: &App,
    ) -> Result<Option<Self>, Self::Rejection> {
        Ok(Self::from_headers(&parts.headers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_names_use_the_last_path_segment() {
        assert_eq!(
            default_name("app::events::OrderShipped"),
            "App\\Events\\OrderShipped"
        );
        assert_eq!(default_name("a::Wrapper<b::C>"), "App\\Events\\Wrapper");
        assert_eq!(default_name("Plain"), "App\\Events\\Plain");
    }

    #[test]
    fn socket_ids_come_from_one_well_formed_header() {
        let mut headers = http::HeaderMap::new();
        assert_eq!(SocketId::from_headers(&headers), None);
        headers.insert("x-socket-id", http::HeaderValue::from_static("123.456"));
        assert_eq!(
            SocketId::from_headers(&headers).unwrap().as_str(),
            "123.456"
        );
        headers.insert(
            "x-socket-id",
            http::HeaderValue::from_static("123.456; drop"),
        );
        assert_eq!(SocketId::from_headers(&headers), None);
        headers.insert("x-socket-id", http::HeaderValue::from_static("123.456"));
        headers.append("x-socket-id", http::HeaderValue::from_static("1.2"));
        assert_eq!(SocketId::from_headers(&headers), None, "two headers");
    }
}
