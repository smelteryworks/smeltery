# smeltery-hallmark

Hallmark, the API tokens of the [Smeltery](https://github.com/smelteryworks/smeltery) framework: personal access tokens
with abilities for API clients, mobile and desktop apps, command-line tools and other backends; the `hallmark`
bearer guard; token management; and revocation events for the app's other processes.

Apps use it through the facade, as `smeltery::hallmark`:

```rust
use smeltery::auth::Authenticated;
use smeltery::hallmark::{CurrentToken, Hallmark, HallmarkExt as _, Tokens};
use smeltery::http::StatusCode;
use smeltery::prelude::*;

/// `GET /api/orders`: needs a token with the ability `orders:read` (the route's `abilities:` alias checks it).
async fn orders(who: Authenticated) -> Result<String> {
    Ok(format!("the orders of user {}", who.user_id))
}

/// `DELETE /api/tokens/current`: the token signs itself out.
async fn sign_out(token: CurrentToken) -> Result<StatusCode> {
    token.revoke().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/tokens`: the caller's tokens (never their secrets).
async fn list(who: Authenticated, tokens: Tokens) -> Result<Json<Vec<smeltery::hallmark::AccessToken>>> {
    Ok(Json(tokens.list(who.user_id).await?))
}

fn build(app: AppBuilder) -> AppBuilder {
    // `.auth::<User>()` comes first in a real app: Hallmark needs the user model.
    app.hallmark(Hallmark::new()).api_routes(|r| {
        r.get("/orders", orders).middleware("auth:hallmark").middleware("abilities:orders:read");
        r.delete("/tokens/current", sign_out).middleware("auth:hallmark");
        r.get("/tokens", list).middleware("auth:hallmark");
    })
}
# let _ = build;
```

## What it does

- **Tokens** are `smt_` followed by 64 lowercase hex characters (32 bytes from the operating system's random
  source). The table stores only their SHA-256; the plain text exists once, in the `NewToken` that `Tokens::create`
  returns. `PlainToken` has no `Display` and no `Serialize`, and its `Debug` prints `PlainToken(smt_…)`.
- **The guard** `hallmark` reads one `Authorization: Bearer <token>` header and nothing else: a token in the query
  string, a cookie, the body or another header is never read. Malformed tokens are refused before any database work;
  a found row is compared again in constant time. Missing, malformed, unknown, expired and revoked tokens all get the
  same 401 `{"error":"Unauthenticated."}` with `WWW-Authenticate: Bearer` and `Cache-Control: no-store`. Requests
  the guard looked at answer with `Vary: Authorization, Cookie`. Web routes never accept a bearer token (they keep
  the session and its CSRF check).
- **Guess budget:** a client (its address after `TRUSTED_PROXIES`, an IPv6 client by its /64) may send
  `HALLMARK_GUESS_LIMIT` (60) invalid tokens a minute (malformed, unknown, expired and revoked alike). After that a
  bearer request from that address gets 429 with `Retry-After` until the minute ends, before any lookup, unless this
  process accepted its token in the last five minutes, so other clients behind the same address keep working. The
  count lives in the app's cache store; a cache error answers 500.
- **Expiry:** a new token expires after `HALLMARK_TOKEN_EXPIRATION` days (365; `0` = never), and every check also
  refuses a token older than that, so lowering the setting shortens existing tokens.
- **Bound to the password:** a token stores a hash of its user's password hash and credentials epoch. Any password
  change, by any code, and `auth::end_credentials` end every token of the user at its next use (it is deleted then),
  including a token issued while they ran. Password resets, signing out other devices, `auth::password_changed` and
  `auth::end_credentials` delete the user's tokens at once (`password_changed` keeps the calling token and binds it
  to the new password; when that token was signed out by a parallel request first, the change answers 401).
- **Abilities** are exact names (1 to 100 of `A-Z a-z 0-9 : . _ -`) or `*` alone (every ability), at most 64 per
  token; there are no patterns (`orders:*` is refused). A token created with `&[]` may do nothing. A stored list that fails these
  rules grants nothing. The middleware `abilities:a,b` needs every listed ability, `ability:a,b` at least one (403
  `{"error":"Forbidden."}` otherwise, 401 without a principal); put `auth:hallmark` before them. A signed-in session
  holds every ability: abilities limit tokens, not users, so handlers still check that the user may touch a record.
- **Per-user cap:** a user holds at most `HALLMARK_MAX_TOKENS_PER_USER` (100) tokens; creating one more deletes the
  least recently used, in the same transaction.
- **`last_used_at`** is written at most once a minute per token, in the background with a 2 s timeout.
- **Revocation events:** `Tokens::revoke`, `CurrentToken::revoke`, a token deleted on use or by the cap publish
  `AuthEvent::Revoked { key: "hallmark:token:<id>" }`, and `revoke_all` / `revoke_all_except` publish
  `AuthEvent::RevokedAll { kind: Tokens }`, on the PubSub topic `auth`.
- **Issuing for an email and password:** `issue_for_credentials(&app, &client, email, password, device_name, code,
  abilities)` checks them with core's login budgets and password gate (422 on `email` when they do not match, 429
  past a budget), asks the app's login policies (a refusal answers its status), asks a registered second factor for
  `code` when the user needs one (422 on `code`, 429 past its budget, before any token exists), then creates the
  token; `NewToken::created()` answers 201 with `Cache-Control: no-store`.
  `revoke_current` is a handler for `DELETE /api/tokens/current` (204).
- **SPA mode** (`.spa()` / `HALLMARK_SPA=true`): a browser request on an `auth:hallmark` API route that is
  first-party (`Sec-Fetch-Site: same-origin`; `same-site` / `cross-site` with an `Origin` listed in
  `HALLMARK_STATEFUL`; without `Sec-Fetch-Site`, an `Origin` / `Referer` origin equal to `APP_URL`'s or a listed one)
  runs the web session stack, CSRF check first, and a signed-in session passes; every other request is
  bearer-only. `GET /hallmark/csrf-cookie` sets the `XSRF-TOKEN` cookie. List only origins you control (any script
  on a listed origin acts with the signed-in session); a development origin such as `http://localhost:5173` never
  belongs in a production `.env`.
- **Console:** `hallmark:prune-expired [--hours=24]` deletes tokens that expired at least that long ago, 500 rows
  per statement.
- **Testing:** `hallmark::testing::acting_as(&app, &user, &["orders:read"])` creates a real token and sends it with
  every following request; `token_for` returns one without changing the app's headers.

The table comes from the app's migration, which calls `smeltery::hallmark::migrations::up` / `down`.

## Licence

MIT OR Apache-2.0.
