//! Anvil's token auth endpoint with Hallmark's bearer guard: a token with the `broadcasting` ability gets a grant
//! for its user's channels, a token without it gets 403, a revoked token 401.
#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod user {
    //! The `User` model (table `users`).
    use smeltery::db::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub email: String,
        pub password: String,
        pub remember_token: Option<String>,
        pub created_at: Option<DateTimeUtc>,
        pub updated_at: Option<DateTimeUtc>,
    }

    impl ActiveModelBehavior for ActiveModel {}

    impl smeltery::auth::Authenticatable for Model {
        fn auth_id(&self) -> i64 {
            self.id
        }
        fn password_hash(&self) -> &str {
            &self.password
        }
        fn remember_token(&self) -> Option<&str> {
            self.remember_token.as_deref()
        }
    }
}

use smeltery::Result;
use smeltery::anvil::testing::{TestSocket, auth_of};
use smeltery::anvil::{AnvilExt as _, ChannelCtx};
use smeltery::db::Record as _;
use smeltery::db::migration::{Migration, Schema};
use smeltery::db::prelude::Set;
use smeltery::hallmark::{Hallmark, HallmarkExt as _, Tokens};
use smeltery::http::{HeaderMap, HeaderValue, Method};
use smeltery::testing::{TestApp, TestResponse};
use user::Model as User;

struct CreateTables;

impl Migration for CreateTables {
    fn name(&self) -> &'static str {
        "2026_10_05_000001_create_tables"
    }

    async fn up(&self, schema: &Schema) -> Result<()> {
        schema
            .create("users", |t| {
                t.id();
                t.string("email").unique();
                t.string("password");
                t.string_len("remember_token", 100).nullable();
                t.timestamps();
            })
            .await?;
        smeltery::hallmark::migrations::up(schema).await
    }

    async fn down(&self, schema: &Schema) -> Result<()> {
        smeltery::hallmark::migrations::down(schema).await?;
        schema.drop_if_exists("users").await
    }
}

fn app() -> TestApp {
    TestApp::new(|b| {
        b.migrations(|m| {
            m.add(CreateTables);
        })
        .auth::<User>()
        .hallmark(Hallmark::new())
        .anvil(|c| {
            c.private("orders.{order}", |ctx: ChannelCtx| async move {
                // In this test, user n owns order n.
                Ok(ctx.user_id() == Some(ctx.param::<i64>("order")?))
            });
        })
    })
}

fn create_user(app: &TestApp, email: &str) -> User {
    let db = app.db();
    app.block_on(async {
        User::create(
            &db,
            user::ActiveModel {
                email: Set(email.into()),
                password: Set("not a password hash".into()),
                ..Default::default()
            },
        )
        .await
    })
    .unwrap()
}

fn token_auth(app: &TestApp, token: &str, socket: &str, channel: &str) -> TestResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    app.request(
        Method::POST,
        "/api/broadcasting/auth",
        headers,
        format!("socket_id={socket}&channel_name={channel}").into(),
    )
}

#[test]
fn hallmark_tokens_authorize_private_channels() {
    let app = app();
    let ada = create_user(&app, "ada@example.com");
    let tokens = Tokens::of(app.app()).unwrap();
    let new = app
        .block_on(tokens.create(ada.id, "phone", &["broadcasting"], None))
        .unwrap();
    let plain = new.plain_text().to_owned();
    let mut socket = TestSocket::connect(app.app());
    let channel = format!("private-orders.{}", ada.id);

    let res = token_auth(&app, &plain, socket.socket_id(), &channel);
    assert_eq!(res.status(), 200, "{}", res.text());
    let auth = auth_of(&res).unwrap();
    assert!(
        auth.contains(&format!(":{}.hallmark~token~{}.", ada.id, new.token().id)),
        "{auth}"
    );
    assert_eq!(
        socket.subscribe(&channel, Some(&auth))["event"],
        "pusher_internal:subscription_succeeded"
    );
    // Another user's channel.
    let res = token_auth(&app, &plain, socket.socket_id(), "private-orders.999");
    assert_eq!(res.status(), 403);

    // A token without the ability.
    let reader = app
        .block_on(tokens.create(ada.id, "reader", &["orders:read"], None))
        .unwrap();
    let res = token_auth(&app, reader.plain_text(), socket.socket_id(), &channel);
    assert_eq!(res.status(), 403);

    // A revoked token.
    assert!(app.block_on(tokens.revoke(ada.id, new.token().id)).unwrap());
    let res = token_auth(&app, &plain, socket.socket_id(), &channel);
    assert_eq!(res.status(), 401);
    assert_eq!(res.header("www-authenticate"), Some("Bearer"));
}
