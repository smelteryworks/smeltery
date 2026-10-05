//! Core's channel seam ([`ChannelAuthorizer`]) for consumers inside the app (Sparks listeners): the same channel
//! rules as `POST /broadcasting/auth`, and the events this process delivers (D-409).

use smeltery_core::auth::Auth;
use smeltery_core::channels::{ChannelAuthorizer, ChannelEvents};
use smeltery_core::{App, BoxFuture, Result};

use crate::Anvil;
use crate::channels::{Asker, Authorizer, ChannelCtx};
use crate::protocol::{Kind, valid_channel};

/// Anvil as core's [`ChannelAuthorizer`], registered by [`AnvilExt::anvil`](crate::AnvilExt::anvil).
pub(crate) struct Listeners(pub(crate) Anvil);

impl Anvil {
    /// Whether the visitor of `auth` may receive the events of `channel`, by the rules of the cookie auth endpoint:
    /// a declared public channel, or a private / presence pattern whose callback allows (guests only with
    /// `.guests()`). A request without a session (`auth` is `None`) receives public channels only. A callback's
    /// client error is a denial; a server error is returned.
    pub(crate) async fn may_receive(
        &self,
        app: &App,
        channel: &str,
        auth: Option<&Auth>,
    ) -> Result<bool> {
        if !valid_channel(channel) {
            return Ok(false);
        }
        let channels = &self.inner.channels;
        let (kind, bare) = Kind::of(channel);
        if kind == Kind::Public {
            return Ok(channels.is_public(bare));
        }
        let Some(auth) = auth else {
            return Ok(false);
        };
        let Some((authorizer, params, guests)) = channels.authorizer(kind, bare) else {
            return Ok(false);
        };
        if auth.id().is_none() && !guests {
            return Ok(false);
        }
        // A listener has no socket: the callback sees an empty socket id.
        let ctx = ChannelCtx::new(
            app.clone(),
            channel.to_owned(),
            String::new(),
            params,
            Asker::Session(auth.clone()),
        );
        let decided = match authorizer {
            Authorizer::Private(callback) => callback(ctx).await,
            Authorizer::Presence(callback) => callback(ctx).await.map(|member| member.is_some()),
        };
        match decided {
            Ok(allowed) => Ok(allowed),
            Err(error) if !error.status().is_server_error() => Ok(false),
            Err(error) => Err(error),
        }
    }
}

impl ChannelAuthorizer for Listeners {
    fn authorize<'a>(
        &'a self,
        app: &'a App,
        channel: &'a str,
        auth: Option<&'a Auth>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.0.may_receive(app, channel, auth))
    }

    fn events(&self) -> ChannelEvents {
        self.0.inner.listeners.subscribe()
    }
}
