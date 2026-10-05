# Skill: issue and check an API token

The app has API tokens (Hallmark, `.hallmark(Hallmark::new())` in `bootstrap/app.rs`). A token is `smt_` and 64
hex characters, sent as `Authorization: Bearer smt_…`; the table `personal_access_tokens` stores only its hash.
`POST /api/tokens` issues one for an e-mail address and a password (`app/controllers/api/tokens.rs`).

1. Put the endpoint in `routes/api.rs` behind `auth:hallmark`, and list the abilities it needs after it
   (`abilities:a,b` needs every one, `ability:a,b` at least one):

   ```rust
   r.get("/orders", crate::app::controllers::api::orders::index)
       .name("api.orders.index")
       .middleware("auth:hallmark")
       .middleware("abilities:orders:read");
   ```
2. In the handler, take `who: smeltery::auth::Authenticated` (`who.user_id`, `who.can("orders:read")`,
   `who.user::<crate::app::models::User>(&app).await?`) or `token: smeltery::hallmark::CurrentToken`. Abilities limit
   tokens, not users: still check that the user may touch the record.
3. Create tokens in code with `smeltery::hallmark::Tokens` (a handler argument): `tokens.create(user_id, "name",
   &["orders:read"], None).await?` returns a `NewToken`; answer it with `new.created()` (201, `no-store`). A route that
   creates tokens for a token holder checks an ability of its own and grants only abilities the calling token holds
   (refuse the request unless `who.can(ability)` for every requested ability, `*` included), so a weak token cannot
   mint a strong one.
4. Never log, flash or store the plain token, and never accept one from a query string, cookie or form field.
5. Test it: `smeltery::hallmark::testing::acting_as(&app, &user, &["orders:read"])` creates a real token and sends it
   with every following request (`token_for` returns one without sending it); create the user with
   `UserFactory.create(&app.db())` (`smeltery::db::factory::Factory as _`). See `tests/api_tokens.rs`.
6. Run `smeltery test`.
