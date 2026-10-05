//! Hallmark's tokens and guard on SQLite: storage, the guard matrix, the attack tests of HALLMARK §8.3, abilities,
//! revocation (and its events), the cap, `last_used_at`, pruning, the console command and the test helpers.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::time::Duration;

use sea_orm::Value;
use sea_orm::prelude::{ChronoUtc, DateTimeUtc};
use smeltery_core::auth::{AuthEvent, CredentialKind};
use smeltery_core::crypto::sha256_hex;
use smeltery_core::http::{HeaderMap, Method, header};
use smeltery_core::testing::TestApp;
use smeltery_hallmark::{Hallmark, HasApiTokens as _, Tokens};
use support::*;

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn days_ago(days: u64) -> DateTimeUtc {
    ChronoUtc::now() - Duration::from_secs(days * 86_400)
}

/// The fields that must be identical in every refusal.
fn refusal(
    res: &smeltery_core::testing::TestResponse,
) -> (u16, String, Option<String>, Option<String>) {
    (
        res.status(),
        res.text(),
        res.header("www-authenticate").map(str::to_owned),
        res.header("cache-control").map(str::to_owned),
    )
}

// ---- storage ---------------------------------------------------------------------------------

#[test]
fn tokens_are_stored_hashed_and_shown_once() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let new = create(&app, &ada, &["orders:read"]);
    let plain = new.plain_text().to_owned();
    assert!(plain.starts_with("smt_") && plain.len() == 68, "{plain}");
    let db = app.db();
    let rows = app
        .block_on(db.query_with(
            "SELECT token_hash, binding, abilities, name FROM personal_access_tokens",
            [],
        ))
        .unwrap();
    assert_eq!(rows.len(), 1);
    let hash: String = rows[0].try_get("", "token_hash").unwrap();
    assert_eq!(hash, sha256_hex(&plain));
    for column in ["token_hash", "binding", "abilities", "name"] {
        let value: String = rows[0].try_get("", column).unwrap();
        assert!(!value.contains(&plain[4..]), "{column} holds the token");
    }
    let listed = app.block_on(tokens(&app).list(ada.id)).unwrap();
    let json = serde_json::to_string(&listed).unwrap();
    assert!(
        !json.contains(&plain[4..]) && !json.contains(&hash),
        "{json}"
    );
    assert_eq!(listed[0].abilities, ["orders:read"]);
    assert_eq!(new.to_json()["token"], plain.as_str());
    assert_eq!(new.to_json()["token_type"], "Bearer");
    assert!(!format!("{new:?}").contains(&plain[4..]));
}

#[test]
fn tokens_authenticate_api_requests_with_their_user() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let new = create(&app, &ada, &["*"]);
    let res = get_with(&app, "/api/me", &bearer(new.plain_text()));
    assert_eq!(res.status(), 200, "{}", res.text());
    assert_eq!(
        res.text(),
        format!("hallmark:token:{} ada@example.com", new.token().id)
    );
    let vary: Vec<_> = res.headers().get_all(header::VARY).iter().collect();
    assert!(vary.iter().any(|v| *v == "Authorization"), "{vary:?}");
    assert!(vary.iter().any(|v| *v == "Cookie"), "{vary:?}");
    // `Authenticated` alone finds the token on an API route, and so does `CurrentToken`.
    assert_eq!(
        get_with(&app, "/api/plain", &bearer(new.plain_text())).text(),
        format!("hallmark:token:{}", new.token().id)
    );
    assert_eq!(
        get_with(&app, "/api/maybe", &bearer(new.plain_text())).text(),
        format!("{} phone", new.token().id)
    );
    assert_eq!(app.get_json("/api/maybe").text(), "none");
    // Case of the scheme does not matter.
    assert_eq!(
        get_with(&app, "/api/me", &format!("bEaReR {}", new.plain_text())).status(),
        200
    );
}

#[test]
fn every_invalid_token_gets_the_same_answer() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let good = create(&app, &ada, &["*"]);
    let expired = app
        .block_on(tokens(&app).create(ada.id, "old", &["*"], Some(days_ago(1))))
        .unwrap();
    let unknown = format!("smt_{}", "a".repeat(64));
    let missing = app.get_json("/api/me");
    let reference = refusal(&missing);
    assert_eq!(reference.0, 401);
    assert_eq!(reference.2.as_deref(), Some("Bearer"));
    assert_eq!(reference.3.as_deref(), Some("no-store"));
    assert!(reference.1.contains("Unauthenticated."), "{}", reference.1);
    for authorization in [
        bearer(&unknown),
        bearer("smt_short"),
        bearer(&good.plain_text().to_uppercase()),
        bearer(expired.plain_text()),
        format!("Bearer  {}", good.plain_text()),
        format!("Basic {}", good.plain_text()),
        "Bearer".to_owned(),
        format!("Bearer{}", good.plain_text()),
    ] {
        let res = get_with(&app, "/api/me", &authorization);
        assert_eq!(refusal(&res), reference, "{authorization}");
    }
    // Two Authorization headers: not a credential either.
    let mut headers = HeaderMap::new();
    headers.append(
        header::AUTHORIZATION,
        bearer(good.plain_text()).parse().unwrap(),
    );
    headers.append(
        header::AUTHORIZATION,
        bearer(good.plain_text()).parse().unwrap(),
    );
    headers.insert(header::ACCEPT, "application/json".parse().unwrap());
    let res = app.request(Method::GET, "/api/me", headers, Default::default());
    assert_eq!(refusal(&res), reference);
    // The good one still works.
    assert_eq!(
        get_with(&app, "/api/me", &bearer(good.plain_text())).status(),
        200
    );
    // `Authenticated` without `auth:hallmark` answers 401 with the bearer headers too.
    let res = get_with(&app, "/api/plain", &bearer(&unknown));
    assert_eq!(res.status(), 401);
    assert_eq!(res.header("www-authenticate"), Some("Bearer"));
    assert_eq!(res.header("cache-control"), Some("no-store"));
}

// ---- attack tests (HALLMARK §8.3) -------------------------------------------------------------

#[test]
fn a_token_in_the_query_string_is_ignored() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let new = create(&app, &ada, &["*"]);
    for query in ["token", "access_token", "api_token", "bearer"] {
        let res = app.get_json(&format!("/api/me?{query}={}", new.plain_text()));
        assert_eq!(res.status(), 401, "{query}");
    }
}

#[test]
fn a_cookie_named_token_is_ignored() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let new = create(&app, &ada, &["*"]);
    for name in ["token", "access_token", "Authorization"] {
        app.set_cookie(name, new.plain_text());
        assert_eq!(app.get_json("/api/me").status(), 401, "{name}");
        app.clear_cookies();
    }
    for name in ["x-api-token", "x-auth-token", "token"] {
        app.with_header(name, new.plain_text());
        assert_eq!(app.get_json("/api/me").status(), 401, "{name}");
        app.without_header(name);
    }
}

#[test]
fn a_bearer_post_to_a_web_route_still_needs_csrf() {
    let app = TestApp::new(build(Hallmark::new())).with_csrf();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let new = create(&app, &ada, &["*"]);
    app.with_bearer(new.plain_text());
    assert_eq!(app.post_form("/web/post", &[]).status(), 419);
    // A web route never takes the bearer as the session: a guest is sent to the login page.
    let res = app.get("/web/me");
    assert_eq!(res.status(), 303);
    assert_eq!(res.header("location"), Some("/login"));
}

#[test]
fn unknown_token_guesses_are_throttled_before_the_lookup() {
    let app = app();
    app.from_addr("192.0.2.10:5000".parse().unwrap());
    let ada = create_user(&app, "ada@example.com", "secret one");
    let good = create(&app, &ada, &["*"]);
    for i in 0..60 {
        let guess = format!("smt_{:064x}", i);
        assert_eq!(
            get_with(&app, "/api/me", &bearer(&guess)).status(),
            401,
            "{i}"
        );
    }
    let res = get_with(&app, "/api/me", &bearer(&format!("smt_{:064x}", 61)));
    assert_eq!(res.status(), 429);
    let retry: u64 = res.header("retry-after").unwrap().parse().unwrap();
    assert!((1..=60).contains(&retry), "{retry}");
    assert_eq!(res.header("cache-control"), Some("no-store"));
    // Refused before the lookup: with the table gone a lookup would fail with 500.
    sql(&app, "DROP TABLE personal_access_tokens", vec![]);
    let res = get_with(&app, "/api/me", &bearer(&format!("smt_{:064x}", 62)));
    assert_eq!(res.status(), 429, "{}", res.text());
    // A valid token from that client waits too.
    assert_eq!(
        get_with(&app, "/api/me", &bearer(good.plain_text())).status(),
        429
    );
    // Requests without a bearer token are not refused by the budget.
    assert_eq!(app.get_json("/api/maybe").status(), 200);
    // Another client has its own budget (and its malformed tokens cost no query: still no table).
    app.from_addr("198.51.100.20:5000".parse().unwrap());
    assert_eq!(get_with(&app, "/api/me", &bearer("smt_bad")).status(), 401);
}

#[test]
fn a_malformed_token_costs_no_query() {
    let app = app();
    sql(&app, "DROP TABLE personal_access_tokens", vec![]);
    for token in ["smt_bad", "", "nope", &format!("smt_{}", "A".repeat(64))] {
        assert_eq!(
            get_with(&app, "/api/me", &bearer(token)).status(),
            401,
            "{token}"
        );
    }
    // A well-formed one does reach the database (and its failure is a 500, never a pass).
    assert_eq!(
        get_with(&app, "/api/me", &bearer(&format!("smt_{}", "a".repeat(64)))).status(),
        500
    );
}

#[test]
fn ipv6_guessers_are_counted_by_their_64() {
    let app = app_with(Hallmark::new().guess_limit(2));
    let guess = bearer(&format!("smt_{}", "b".repeat(64)));
    app.from_addr("[2001:db8:1:2::1]:5000".parse().unwrap());
    assert_eq!(get_with(&app, "/api/me", &guess).status(), 401);
    app.from_addr("[2001:db8:1:2::ffff]:5000".parse().unwrap());
    assert_eq!(get_with(&app, "/api/me", &guess).status(), 401);
    app.from_addr("[2001:db8:1:2:aaaa::1]:5000".parse().unwrap());
    assert_eq!(get_with(&app, "/api/me", &guess).status(), 429);
    app.from_addr("[2001:db8:1:3::1]:5000".parse().unwrap());
    assert_eq!(get_with(&app, "/api/me", &guess).status(), 401);
}

#[test]
fn the_guess_budget_fails_closed() {
    // A `database` cache store without its table: every cache call fails.
    let app = TestApp::new(|b| {
        let mut b = build(Hallmark::new())(b);
        b.settings_mut().cache_store = "database".to_owned();
        b
    });
    app.from_addr("192.0.2.30:5000".parse().unwrap());
    let guess = bearer(&format!("smt_{}", "c".repeat(64)));
    assert_eq!(get_with(&app, "/api/me", &guess).status(), 500);
}

#[test]
fn a_password_reset_deletes_the_users_tokens() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let bob = create_user(&app, "bob@example.com", "secret two");
    let first = create(&app, &ada, &["*"]);
    let second = create(&app, &ada, &["*"]);
    let bobs = create(&app, &bob, &["*"]);
    let events = events(&app, || reset_password(&app, &ada, "new secret"));
    // Core publishes the one revocation of every credential; the tokens add none.
    assert_eq!(
        events,
        [AuthEvent::RevokedAll {
            user_id: ada.id,
            kind: CredentialKind::Every,
            except: None
        }]
    );
    assert_eq!(token_rows(&app), 1, "only Bob's token is left");
    for token in [&first, &second] {
        assert_eq!(
            get_with(&app, "/api/me", &bearer(token.plain_text())).status(),
            401
        );
    }
    assert_eq!(
        get_with(&app, "/api/me", &bearer(bobs.plain_text())).status(),
        200
    );
}

#[test]
fn a_password_written_by_app_code_ends_its_tokens() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let new = create(&app, &ada, &["*"]);
    assert_eq!(
        get_with(&app, "/api/me", &bearer(new.plain_text())).status(),
        200
    );
    // App code writes a new hash and tells nobody.
    let hash = app
        .block_on(smeltery_core::auth::hash_password("other secret"))
        .unwrap();
    sql(
        &app,
        "UPDATE users SET password = ? WHERE id = ?",
        vec![hash.into(), ada.id.into()],
    );
    let events = events(&app, || {
        assert_eq!(
            get_with(&app, "/api/me", &bearer(new.plain_text())).status(),
            401
        );
    });
    assert_eq!(
        events,
        [AuthEvent::Revoked {
            user_id: ada.id,
            key: format!("hallmark:token:{}", new.token().id)
        }]
    );
    assert_eq!(token_rows(&app), 0, "the token was deleted at its use");
}

#[test]
fn logging_out_other_devices_deletes_tokens() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let new = create(&app, &ada, &["*"]);
    assert_eq!(
        app.post_form(
            "/login",
            &[("email", "ada@example.com"), ("password", "secret one")]
        )
        .text(),
        "in"
    );
    let events = events(&app, || {
        assert_eq!(
            app.post_form("/logout-others", &[("password", "secret one")])
                .text(),
            "true"
        );
    });
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(
        &events[0],
        AuthEvent::RevokedAll {
            kind: CredentialKind::Every,
            ..
        }
    ));
    assert_eq!(token_rows(&app), 0);
    app.clear_cookies();
    assert_eq!(
        get_with(&app, "/api/me", &bearer(new.plain_text())).status(),
        401
    );
}

#[test]
fn password_changed_keeps_the_calling_token_only() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let caller = create(&app, &ada, &["*"]);
    let other = create(&app, &ada, &["*"]);
    app.with_bearer(caller.plain_text());
    let events = events(&app, || {
        assert_eq!(
            app.post_json("/api/password-changed", &serde_json::json!({}))
                .text(),
            "changed"
        );
    });
    assert_eq!(
        events,
        [AuthEvent::RevokedAll {
            user_id: ada.id,
            kind: CredentialKind::Every,
            except: Some(format!("hallmark:token:{}", caller.token().id))
        }]
    );
    let left = app.block_on(tokens(&app).list(ada.id)).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, caller.token().id);
    // The kept token is bound to the new password: it keeps working, request after request.
    for _ in 0..2 {
        assert_eq!(app.get_json("/api/me").status(), 200);
    }
    app.without_header("authorization");
    assert_eq!(
        get_with(&app, "/api/me", &bearer(other.plain_text())).status(),
        401
    );
}

#[test]
fn expired_tokens_are_refused_and_deleted() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let soon = app
        .block_on(tokens(&app).create(
            ada.id,
            "short",
            &["*"],
            Some(ChronoUtc::now() + Duration::from_secs(3600)),
        ))
        .unwrap();
    assert_eq!(
        get_with(&app, "/api/me", &bearer(soon.plain_text())).status(),
        200
    );
    sql(
        &app,
        "UPDATE personal_access_tokens SET expires_at = ? WHERE id = ?",
        vec![Value::from(days_ago(1)), soon.token().id.into()],
    );
    let events = events(&app, || {
        assert_eq!(
            get_with(&app, "/api/me", &bearer(soon.plain_text())).status(),
            401
        );
    });
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(token_rows(&app), 0);
}

#[test]
fn lowering_the_expiration_shortens_old_tokens() {
    let app = app_with(Hallmark::new().expiration(Duration::from_secs(10 * 86_400)));
    let ada = create_user(&app, "ada@example.com", "secret one");
    let new = create(&app, &ada, &["*"]);
    // Created eleven days ago: its stored expiry would let it in, the maximum age does not.
    sql(
        &app,
        "UPDATE personal_access_tokens SET created_at = ?, expires_at = ? WHERE id = ?",
        vec![
            Value::from(days_ago(11)),
            Value::from(ChronoUtc::now() + Duration::from_secs(86_400 * 300)),
            new.token().id.into(),
        ],
    );
    assert_eq!(
        get_with(&app, "/api/me", &bearer(new.plain_text())).status(),
        401
    );
}

#[test]
fn new_tokens_expire_by_the_setting_and_explicit_times_are_capped() {
    let app = app_with(Hallmark::new().expiration(Duration::from_secs(30 * 86_400)));
    let ada = create_user(&app, "ada@example.com", "secret one");
    let new = create(&app, &ada, &["*"]);
    let at = new.token().expires_at.unwrap();
    let want = ChronoUtc::now() + Duration::from_secs(30 * 86_400);
    assert!((want - at).num_seconds().abs() < 5, "{at}");
    let far = app
        .block_on(tokens(&app).create(
            ada.id,
            "far",
            &["*"],
            Some(ChronoUtc::now() + Duration::from_secs(400 * 86_400)),
        ))
        .unwrap();
    assert!(far.token().expires_at.unwrap() <= want + Duration::from_secs(5));
    let never = app_with(Hallmark::new().expiration(Duration::ZERO));
    let bob = create_user(&never, "bob@example.com", "secret two");
    assert_eq!(create(&never, &bob, &["*"]).token().expires_at, None);
}

#[test]
fn a_deleted_user_ends_the_token() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let new = create(&app, &ada, &["*"]);
    sql(&app, "PRAGMA foreign_keys = OFF", vec![]);
    sql(&app, "DELETE FROM users WHERE id = ?", vec![ada.id.into()]);
    assert_eq!(
        get_with(&app, "/api/me", &bearer(new.plain_text())).status(),
        401
    );
}

// ---- abilities -------------------------------------------------------------------------------

#[test]
fn abilities_are_exact_or_star() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let cases: [(&[&str], [u16; 3]); 6] = [
        (&["orders"], [403, 403, 403]),
        (&["orders:read"], [200, 403, 200]),
        (&["orders:read", "orders:write"], [200, 200, 200]),
        (&["orders:write"], [403, 403, 200]),
        (&["*"], [200, 200, 200]),
        (&[], [403, 403, 403]),
    ];
    for (abilities, [all_one, all_two, any]) in cases {
        let token = create(&app, &ada, abilities);
        let auth = bearer(token.plain_text());
        assert_eq!(
            get_with(&app, "/api/orders", &auth).status(),
            all_one,
            "{abilities:?}"
        );
        assert_eq!(
            get_with(&app, "/api/both", &auth).status(),
            all_two,
            "{abilities:?}"
        );
        assert_eq!(
            get_with(&app, "/api/either", &auth).status(),
            any,
            "{abilities:?}"
        );
    }
    // `*` is all or nothing: `orders:*` is not a pattern and is refused as a name.
    assert!(
        app.block_on(tokens(&app).create(ada.id, "x", &["orders:*"], None))
            .is_err()
    );
    let token = create(&app, &ada, &["orders:write"]);
    let res = get_with(&app, "/api/orders", &bearer(token.plain_text()));
    assert_eq!(res.json(), serde_json::json!({"error": "Forbidden."}));
    // Without a principal the ability aliases answer 401.
    let res = get_with(&app, "/api/naked", &bearer(token.plain_text()));
    assert_eq!(res.status(), 401);
    assert_eq!(res.header("www-authenticate"), Some("Bearer"));
}

#[test]
fn a_session_holds_every_ability() {
    let app = app();
    create_user(&app, "ada@example.com", "secret one");
    app.post_form(
        "/login",
        &[("email", "ada@example.com"), ("password", "secret one")],
    );
    assert_eq!(app.get("/web/abilities").status(), 200);
    assert!(app.get("/web/me").text().starts_with("web:session:"));
}

#[test]
fn invalid_abilities_and_names_are_refused_at_creation() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let t = tokens(&app);
    for abilities in [&["orders read"][..], &[""], &["ä"]] {
        let err = app
            .block_on(t.create(ada.id, "x", abilities, None))
            .unwrap_err();
        assert_eq!(err.status().as_u16(), 400, "{abilities:?}");
    }
    for name in ["", "a\nb", &"n".repeat(256)] {
        let err = app
            .block_on(t.create(ada.id, name, &["*"], None))
            .unwrap_err();
        assert_eq!(err.status().as_u16(), 400);
    }
    let err = app.block_on(t.create(9999, "x", &["*"], None)).unwrap_err();
    assert_eq!(err.status().as_u16(), 400);
    assert_eq!(token_rows(&app), 0);
}

#[test]
fn a_corrupt_abilities_column_grants_nothing() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let huge = serde_json::to_string(&vec!["orders:read"; 10_000]).unwrap();
    for stored in [
        huge.as_str(),
        "not json",
        r#"["orders:read", 7]"#,
        r#"["orders read"]"#,
    ] {
        let new = create(&app, &ada, &["*"]);
        sql(
            &app,
            "UPDATE personal_access_tokens SET abilities = ? WHERE id = ?",
            vec![stored.into(), new.token().id.into()],
        );
        let auth = bearer(new.plain_text());
        assert_eq!(get_with(&app, "/api/me", &auth).status(), 200);
        assert_eq!(
            get_with(&app, "/api/orders", &auth).status(),
            403,
            "{}",
            &stored[..20.min(stored.len())]
        );
        assert_eq!(get_with(&app, "/api/either", &auth).status(), 403);
    }
}

#[test]
fn bad_ability_aliases_and_a_missing_user_model_fail_the_build() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let settings = || smeltery_core::config::Settings::from_env();
    use smeltery_hallmark::HallmarkExt as _;
    let no_auth = rt.block_on(
        smeltery_core::AppBuilder::new(settings())
            .hallmark(Hallmark::new())
            .build(),
    );
    let err = no_auth.err().unwrap().to_string();
    assert!(err.contains(".auth::<User>()"), "{err}");
    for alias in ["abilities:", "ability:a b", "abilities:a,,b"] {
        let built = rt.block_on(
            smeltery_core::AppBuilder::new(settings())
                .auth::<User>()
                .hallmark(Hallmark::new())
                .api_routes(move |r| {
                    r.get("/x", || async { "x" }).middleware(alias);
                })
                .build(),
        );
        assert!(built.is_err(), "{alias}");
    }
}

// ---- revocation ------------------------------------------------------------------------------

#[test]
fn a_user_cannot_revoke_another_users_token() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let bob = create_user(&app, "bob@example.com", "secret two");
    let adas = create(&app, &ada, &["*"]);
    let t = tokens(&app);
    let events = events(&app, || {
        assert!(!app.block_on(t.revoke(bob.id, adas.token().id)).unwrap());
        assert_eq!(app.block_on(t.revoke_all_except(bob.id, 0)).unwrap(), 0);
    });
    assert!(events.is_empty(), "{events:?}");
    assert!(
        app.block_on(t.find(bob.id, adas.token().id))
            .unwrap()
            .is_none()
    );
    assert!(
        app.block_on(t.find(ada.id, adas.token().id))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        get_with(&app, "/api/me", &bearer(adas.plain_text())).status(),
        200
    );
}

#[test]
fn each_revocation_publishes_once() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let t = tokens(&app);
    let one = create(&app, &ada, &["*"]);
    let ev = events(&app, || {
        assert!(app.block_on(t.revoke(ada.id, one.token().id)).unwrap())
    });
    assert_eq!(
        ev,
        [AuthEvent::Revoked {
            user_id: ada.id,
            key: format!("hallmark:token:{}", one.token().id)
        }]
    );
    let ev = events(&app, || {
        assert!(!app.block_on(t.revoke(ada.id, one.token().id)).unwrap())
    });
    assert!(ev.is_empty(), "nothing deleted, nothing published");

    let keep = create(&app, &ada, &["*"]);
    create(&app, &ada, &["*"]);
    create(&app, &ada, &["*"]);
    let ev = events(&app, || {
        assert_eq!(
            app.block_on(t.revoke_all_except(ada.id, keep.token().id))
                .unwrap(),
            2
        );
    });
    assert_eq!(
        ev,
        [AuthEvent::RevokedAll {
            user_id: ada.id,
            kind: CredentialKind::Tokens,
            except: Some(format!("hallmark:token:{}", keep.token().id))
        }]
    );
    let ev = events(&app, || {
        assert_eq!(app.block_on(ada.revoke_tokens(app.app())).unwrap(), 1)
    });
    assert_eq!(
        ev,
        [AuthEvent::RevokedAll {
            user_id: ada.id,
            kind: CredentialKind::Tokens,
            except: None
        }]
    );

    // The current token signs itself out.
    let current = create(&app, &ada, &["*"]);
    app.with_bearer(current.plain_text());
    let ev = events(&app, || {
        let res = app.request(
            Method::DELETE,
            "/api/tokens/current",
            HeaderMap::new(),
            Default::default(),
        );
        assert_eq!(res.status(), 204);
    });
    assert_eq!(
        ev,
        [AuthEvent::Revoked {
            user_id: ada.id,
            key: format!("hallmark:token:{}", current.token().id)
        }]
    );
    assert_eq!(app.get_json("/api/me").status(), 401);
}

#[test]
fn the_cap_evicts_the_least_recently_used_token() {
    let app = app_with(Hallmark::new().max_tokens_per_user(3));
    let ada = create_user(&app, "ada@example.com", "secret one");
    let first = create(&app, &ada, &["*"]);
    let second = create(&app, &ada, &["*"]);
    let third = create(&app, &ada, &["*"]);
    // The first is used, so the second is now the least recently used.
    assert_eq!(
        get_with(&app, "/api/me", &bearer(first.plain_text())).status(),
        200
    );
    wait_until(&app, |app| last_used(app, &ada, first.token().id).is_some());
    let ev = events(&app, || {
        create(&app, &ada, &["*"]);
    });
    assert_eq!(
        ev,
        [AuthEvent::Revoked {
            user_id: ada.id,
            key: format!("hallmark:token:{}", second.token().id)
        }]
    );
    let ids: Vec<i64> = app
        .block_on(tokens(&app).list(ada.id))
        .unwrap()
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&first.token().id) && ids.contains(&third.token().id));
    assert_eq!(
        get_with(&app, "/api/me", &bearer(second.plain_text())).status(),
        401
    );
}

/// MySQL keeps whole seconds, so a token used in the second another was created ties with it on
/// `COALESCE(last_used_at, created_at)`: the never-used token must still be the one that goes.
#[test]
fn the_cap_prefers_an_unused_token_when_the_timestamps_tie() {
    let app = app_with(Hallmark::new().max_tokens_per_user(2));
    let ada = create_user(&app, "ada@example.com", "secret one");
    let first = create(&app, &ada, &["*"]);
    let second = create(&app, &ada, &["*"]);
    // Both created in the same second; the first (the lower id) was used in that second too.
    sql(
        &app,
        "UPDATE personal_access_tokens SET created_at = '2026-10-05 12:00:00', last_used_at = NULL",
        vec![],
    );
    sql(
        &app,
        "UPDATE personal_access_tokens SET last_used_at = '2026-10-05 12:00:00' WHERE id = ?",
        vec![Value::from(first.token().id)],
    );
    let ev = events(&app, || {
        create(&app, &ada, &["*"]);
    });
    assert_eq!(
        ev,
        [AuthEvent::Revoked {
            user_id: ada.id,
            key: format!("hallmark:token:{}", second.token().id)
        }],
        "the unused token is the least recently used one"
    );
    assert_eq!(
        get_with(&app, "/api/me", &bearer(first.plain_text())).status(),
        200
    );
    assert_eq!(
        get_with(&app, "/api/me", &bearer(second.plain_text())).status(),
        401
    );
}

// ---- last_used_at, prune, console -------------------------------------------------------------

#[test]
fn last_used_at_is_written_once_a_minute() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let new = create(&app, &ada, &["*"]);
    let id = new.token().id;
    assert_eq!(last_used(&app, &ada, id), None);
    assert_eq!(
        get_with(&app, "/api/me", &bearer(new.plain_text())).status(),
        200
    );
    wait_until(&app, |app| last_used(app, &ada, id).is_some());
    // Within the minute this process does not write again (the column stays as another process left it).
    sql(
        &app,
        "UPDATE personal_access_tokens SET last_used_at = NULL",
        vec![],
    );
    assert_eq!(
        get_with(&app, "/api/me", &bearer(new.plain_text())).status(),
        200
    );
    // A second token used after it: once its write has run, a write for the first would have run too (one
    // runtime thread, tasks in spawn order).
    let marker = create(&app, &ada, &["*"]);
    assert_eq!(
        get_with(&app, "/api/me", &bearer(marker.plain_text())).status(),
        200
    );
    wait_until(&app, |app| {
        last_used(app, &ada, marker.token().id).is_some()
    });
    assert_eq!(last_used(&app, &ada, id), None);
}

#[test]
fn prune_deletes_expired_tokens_in_batches() {
    let app = app_with(Hallmark::new().max_tokens_per_user(10_000));
    let ada = create_user(&app, "ada@example.com", "secret one");
    let t = tokens(&app);
    for _ in 0..501 {
        app.block_on(t.create(ada.id, "old", &["*"], Some(days_ago(3))))
            .unwrap();
    }
    let recent = app
        .block_on(t.create(
            ada.id,
            "recent",
            &["*"],
            Some(ChronoUtc::now() - Duration::from_secs(3600)),
        ))
        .unwrap();
    let live = create(&app, &ada, &["*"]);
    let aged = create(&app, &ada, &["*"]);
    // Older than the maximum age (365 days) by more than a day.
    sql(
        &app,
        "UPDATE personal_access_tokens SET created_at = ? WHERE id = ?",
        vec![Value::from(days_ago(367)), aged.token().id.into()],
    );
    let deleted = app
        .block_on(t.prune_expired(Duration::from_secs(24 * 3600)))
        .unwrap();
    assert_eq!(deleted, 502);
    let left: Vec<i64> = app
        .block_on(t.list(ada.id))
        .unwrap()
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(left.len(), 2);
    assert!(left.contains(&recent.token().id) && left.contains(&live.token().id));
    assert_eq!(app.block_on(t.prune_expired(Duration::ZERO)).unwrap(), 1);
}

/// Concurrent creations on a SQLite file (each counts the user's tokens, then inserts) all succeed: the transaction
/// takes the write lock at its start instead of failing with "database is locked" at its first write.
#[test]
fn concurrent_creations_on_a_sqlite_file_all_succeed() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("app.sqlite").display()
    );
    let app = TestApp::new(move |b: smeltery_core::AppBuilder| {
        let mut b = build(Hallmark::new())(b);
        b.settings_mut().database_url = url;
        b
    });
    let ada = create_user(&app, "ada@example.com", "secret one");
    let store = tokens(&app);
    let id = ada.id;
    let results = app.block_on(async move {
        let tasks: Vec<_> = (0..16)
            .map(|i| {
                let store = store.clone();
                tokio::spawn(async move { store.create(id, &format!("t{i}"), &["*"], None).await })
            })
            .collect();
        let mut results = Vec::new();
        for task in tasks {
            results.push(task.await.unwrap());
        }
        results
    });
    for result in &results {
        assert!(
            result.is_ok(),
            "{:?}",
            result.as_ref().err().map(ToString::to_string)
        );
    }
    let listed = app.block_on(tokens(&app).list(id)).unwrap();
    assert_eq!(listed.len(), 16);
}

#[test]
fn the_prune_command_deletes_expired_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("app.sqlite").display()
    );
    let with_file = |url: String| {
        move |b: smeltery_core::AppBuilder| {
            let mut b = build(Hallmark::new())(b);
            b.settings_mut().database_url = url;
            b
        }
    };
    let app = TestApp::new(with_file(url.clone()));
    let ada = create_user(&app, "ada@example.com", "secret one");
    app.block_on(tokens(&app).create(ada.id, "old", &["*"], Some(days_ago(2))))
        .unwrap();
    app.block_on(tokens(&app).create(ada.id, "new", &["*"], None))
        .unwrap();
    let mut settings = smeltery_core::config::Settings::from_env();
    settings.env = "testing".to_owned();
    let builder = with_file(url)(smeltery_core::AppBuilder::new(settings));
    let mut out = Vec::new();
    let code = app
        .block_on(smeltery_core::console::dispatch(
            builder,
            &["hallmark:prune-expired".to_owned(), "--hours=24".to_owned()],
            &mut out,
        ))
        .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(code, std::process::ExitCode::SUCCESS, "{text}");
    assert!(text.contains("Deleted 1 expired API token."), "{text}");
    assert_eq!(token_rows(&app), 1);
}

// ---- helpers, logs ---------------------------------------------------------------------------

#[test]
fn testing_helpers_use_real_tokens() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let plain = smeltery_hallmark::testing::acting_as(&app, &ada, &["orders:read"]);
    assert_eq!(app.get_json("/api/orders").status(), 200);
    assert_eq!(app.get_json("/api/both").status(), 403);
    let other = smeltery_hallmark::testing::token_for(&app, &ada, &["*"]);
    assert_ne!(plain, other);
    assert_eq!(app.get_json("/api/both").status(), 403, "headers unchanged");
    assert_eq!(get_with(&app, "/api/both", &bearer(&other)).status(), 200);
    assert_eq!(app.block_on(ada.tokens(app.app())).unwrap().len(), 2);
    let made = app
        .block_on(ada.create_token(app.app(), "cli", &["deploy"]))
        .unwrap();
    assert_eq!(made.token().name, "cli");
    assert!(Tokens::of(app.app()).is_ok());
}

// ---- review round 2 ---------------------------------------------------------------------------

/// Review M2: one client's guesses on a shared address (a carrier NAT, an office) never lock out valid tokens
/// that were accepted recently; tokens never seen still wait, before any lookup.
#[test]
fn a_blocked_address_keeps_serving_recently_accepted_tokens() {
    let app = app_with(Hallmark::new().guess_limit(3));
    app.from_addr("203.0.113.9:4000".parse().unwrap());
    let ada = create_user(&app, "ada@example.com", "secret one");
    let active = create(&app, &ada, &["*"]);
    let fresh = create(&app, &ada, &["*"]);
    assert_eq!(
        get_with(&app, "/api/me", &bearer(active.plain_text())).status(),
        200
    );
    for i in 0..3 {
        let guess = bearer(&format!("smt_{:064x}", i));
        assert_eq!(get_with(&app, "/api/me", &guess).status(), 401, "{i}");
    }
    assert_eq!(
        get_with(&app, "/api/me", &bearer(&format!("smt_{:064x}", 9))).status(),
        429
    );
    for _ in 0..5 {
        assert_eq!(
            get_with(&app, "/api/me", &bearer(active.plain_text())).status(),
            200
        );
    }
    assert_eq!(
        get_with(&app, "/api/me", &bearer(fresh.plain_text())).status(),
        429
    );
    // A recently accepted token that was revoked meanwhile is checked in full and refused.
    app.block_on(tokens(&app).revoke(ada.id, active.token().id))
        .unwrap();
    assert_eq!(
        get_with(&app, "/api/me", &bearer(active.plain_text())).status(),
        429,
        "refused in full, and the refusal is past the budget"
    );
    assert_eq!(
        get_with(&app, "/api/me", &bearer(active.plain_text())).status(),
        429
    );
}

/// Review L3: an invalid token read by two extractors in one request is looked up and counted once.
#[test]
fn one_request_counts_one_guess() {
    let app = app_with(Hallmark::new().guess_limit(2));
    app.from_addr("192.0.2.40:5000".parse().unwrap());
    let guess = bearer(&format!("smt_{}", "e".repeat(64)));
    // `/api/twice` takes `Option<Authenticated>` and `Option<CurrentToken>`.
    assert_eq!(get_with(&app, "/api/twice", &guess).text(), "none none");
    assert_eq!(
        get_with(&app, "/api/me", &guess).status(),
        401,
        "the second guess"
    );
    assert_eq!(get_with(&app, "/api/me", &guess).status(), 429);
}

/// Review L4: without a cache store the budget and the block live in this process's memory.
#[test]
fn the_guess_budget_works_without_a_cache_store() {
    let app = TestApp::new(|b| {
        let mut b = build(Hallmark::new())(b);
        b.settings_mut().cache_store = "null".to_owned();
        b
    });
    app.from_addr("192.0.2.50:5000".parse().unwrap());
    for i in 0..60 {
        let guess = bearer(&format!("smt_{:064x}", i));
        assert_eq!(get_with(&app, "/api/me", &guess).status(), 401, "{i}");
    }
    let next = bearer(&format!("smt_{:064x}", 60));
    assert_eq!(get_with(&app, "/api/me", &next).status(), 429);
    sql(&app, "DROP TABLE personal_access_tokens", vec![]);
    assert_eq!(get_with(&app, "/api/me", &next).status(), 429);
}

/// Review L5: requests without any connection information (never from the app's server) are not pooled into one
/// shared budget.
#[test]
fn requests_without_an_address_share_no_budget() {
    let app = app_with(Hallmark::new().guess_limit(1));
    for i in 0..5 {
        let guess = bearer(&format!("smt_{:064x}", i));
        assert_eq!(get_with(&app, "/api/me", &guess).status(), 401, "{i}");
    }
}

#[test]
fn end_credentials_deletes_every_token_of_the_user() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let bob = create_user(&app, "bob@example.com", "secret two");
    let first = create(&app, &ada, &["*"]);
    create(&app, &ada, &["*"]);
    let bobs = create(&app, &bob, &["*"]);
    app.block_on(smeltery_core::auth::end_credentials(app.app(), ada.id))
        .unwrap();
    assert!(app.block_on(tokens(&app).list(ada.id)).unwrap().is_empty());
    assert_eq!(
        get_with(&app, "/api/me", &bearer(first.plain_text())).status(),
        401
    );
    // Another user's tokens stay.
    assert_eq!(app.block_on(tokens(&app).list(bob.id)).unwrap().len(), 1);
    assert_eq!(
        get_with(&app, "/api/me", &bearer(bobs.plain_text())).status(),
        200
    );
}

/// Sweep W4-01: a token is bound to the credentials epoch too, so a token that escaped `end_credentials`'s delete
/// (issued while it ran) still ends at its first use. Red while the binding ignored the epoch.
#[test]
fn a_new_credentials_epoch_ends_the_token() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let token = create(&app, &ada, &["*"]);
    assert_eq!(
        get_with(&app, "/api/me", &bearer(token.plain_text())).status(),
        200
    );
    // What `end_credentials` does to the user row, without its listener deleting the token.
    sql(
        &app,
        "UPDATE users SET credentials_epoch = 1 WHERE id = ?",
        vec![ada.id.into()],
    );
    assert_eq!(
        get_with(&app, "/api/me", &bearer(token.plain_text())).status(),
        401
    );
    assert_eq!(token_rows(&app), 0, "the unbound token is deleted on use");
    // A token made after the change carries the new epoch and works.
    let fresh = create(&app, &ada, &["*"]);
    assert_eq!(
        get_with(&app, "/api/me", &bearer(fresh.plain_text())).status(),
        200
    );
}

/// Sweep W4-02: when the token that changes the password was ended by a parallel request before the listener could
/// rebind it, the change reports it (401) instead of claiming the token was kept. Red while `rebind`'s 0 was ignored.
#[test]
fn a_kept_token_that_ended_meanwhile_is_reported() {
    let app = app();
    let ada = create_user(&app, "ada@example.com", "secret one");
    let caller = create(&app, &ada, &["*"]);
    let principal = smeltery_core::auth::Principal::new(
        ada.id,
        smeltery_hallmark::GUARD,
        smeltery_core::auth::Credential::token(caller.token().id, ["*"]),
    );
    // The parallel request of the same token found the old binding and deleted the row.
    sql(
        &app,
        "DELETE FROM personal_access_tokens WHERE id = ?",
        vec![caller.token().id.into()],
    );
    let err = app
        .block_on(smeltery_core::auth::password_changed(
            app.app(),
            ada.id,
            Some(&principal),
        ))
        .unwrap_err();
    assert_eq!(err.status(), 401, "{err}");
}
