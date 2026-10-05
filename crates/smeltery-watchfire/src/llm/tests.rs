use std::time::Duration;

use serde_json::json;

use super::*;
use crate::http::{FakeResponse, FakeTransport, Http, HttpOptions, Method};

#[derive(serde::Deserialize)]
struct Add {
    a: i64,
    b: i64,
}

fn adder<P: Provider>(provider: P) -> Agent<P> {
    Agent::new(provider).tool(
        "add",
        "Add two integers",
        json!({"type": "object", "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}}}),
        |i: Add| async move {
            if i.a < 0 {
                return Err(AgentError::msg("negative numbers are not supported"));
            }
            Ok(i.a + i.b)
        },
    )
}

#[tokio::test]
async fn tool_loop_sends_results_back_in_one_message() {
    let fake = FakeProvider::new()
        .reply(Reply::tool_calls(&[
            ("add", json!({"a": 1, "b": 2})),
            ("add", json!({"a": -1, "b": 2})),
            ("nope", json!({})),
            ("add", json!({"a": "x"})),
        ]))
        .reply(Reply::text("done"));
    let outcome = adder(fake.clone()).system("sys").run("go").await.unwrap();
    assert_eq!(outcome.text, "done");
    assert_eq!(outcome.turns, 2);
    assert_eq!(outcome.usage, Usage::new(20, 10));
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].system.as_deref(), Some("sys"));
    assert_eq!(requests[0].tools[0].name, "add");
    let results = &requests[1].messages[2];
    assert_eq!(results.role, Role::User);
    let Content::ToolResult {
        content,
        is_error,
        tool_use_id,
    } = &results.content[0]
    else {
        panic!("{results:?}");
    };
    assert_eq!(
        (content.as_str(), *is_error, tool_use_id.as_str()),
        ("3", false, "toolu_fake_0")
    );
    let errors: Vec<(String, bool)> = results
        .content
        .iter()
        .map(|c| match c {
            Content::ToolResult {
                content, is_error, ..
            } => (content.clone(), *is_error),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        errors[1],
        ("negative numbers are not supported".to_owned(), true)
    );
    assert_eq!(errors[2], ("unknown tool `nope`".to_owned(), true));
    assert!(errors[3].0.starts_with("invalid input"), "{:?}", errors[3]);
    assert!(errors[3].1);
}

#[tokio::test]
async fn max_turns_and_budgets() {
    let mut fake = FakeProvider::new();
    for _ in 0..5 {
        fake = fake.reply(Reply::tool_call("add", json!({"a": 1, "b": 1})));
    }
    let err = adder(fake).max_turns(3).run("loop").await.unwrap_err();
    assert_eq!(err, LlmError::MaxTurns(3));

    let fake = FakeProvider::new()
        .reply(Reply::tool_call("add", json!({"a": 1, "b": 1})).usage(100, 50))
        .reply(Reply::text("x").usage(100, 50));
    let err = adder(fake)
        .budget(Budget::new().max_tokens(200))
        .run("go")
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::BudgetExceeded(_)), "{err:?}");

    // Cost with user prices: 1M in at 4 + 1M out at 20 per million.
    let fake = FakeProvider::new().reply(Reply::text("ok").model("m").usage(1_000_000, 1_000_000));
    let outcome = adder(fake)
        .budget(
            Budget::new()
                .max_cost(30.0)
                .price("m", Price::per_mtok(4.0, 20.0)),
        )
        .run("go")
        .await
        .unwrap();
    assert_eq!(outcome.cost, Some(24.0));
    let fake = FakeProvider::new().reply(Reply::text("ok").model("m").usage(1_000_000, 1_000_000));
    let err = adder(fake)
        .budget(
            Budget::new()
                .max_cost(10.0)
                .price("m", Price::per_mtok(4.0, 20.0)),
        )
        .run("go")
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::BudgetExceeded(_)));
    let fake = FakeProvider::new().reply(Reply::text("ok").model("unpriced"));
    let err = adder(fake)
        .budget(Budget::new().max_cost(10.0))
        .run("go")
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Config(_)));
}

#[tokio::test(start_paused = true)]
async fn retries_honour_retry_after_then_give_up() {
    let fake = FakeProvider::new()
        .reply(Reply::error(LlmError::RateLimited {
            retry_after: Some(Duration::from_secs(7)),
        }))
        .reply(Reply::error(LlmError::Overloaded { retry_after: None }))
        .reply(Reply::text("finally"));
    let start = tokio::time::Instant::now();
    let outcome = adder(fake)
        .retries(3, Duration::from_millis(100)..=Duration::from_millis(100))
        .run("go")
        .await
        .unwrap();
    assert_eq!(outcome.text, "finally");
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_secs(7) && waited <= Duration::from_millis(7_100),
        "{waited:?}"
    );

    let fake = FakeProvider::new()
        .reply(Reply::error(LlmError::Server {
            status: 500,
            message: "a".into(),
        }))
        .reply(Reply::error(LlmError::Server {
            status: 503,
            message: "b".into(),
        }));
    let err = adder(fake)
        .retries(1, Duration::from_millis(1)..=Duration::from_millis(1))
        .run("go")
        .await
        .unwrap_err();
    assert_eq!(
        err,
        LlmError::Server {
            status: 503,
            message: "b".into()
        }
    );

    // A refused request is not retried.
    let fake = FakeProvider::new()
        .reply(Reply::error(LlmError::Request {
            status: 400,
            message: "bad".into(),
        }))
        .reply(Reply::text("never"));
    let err = adder(fake.clone()).run("go").await.unwrap_err();
    assert!(matches!(err, LlmError::Request { status: 400, .. }));
    assert_eq!(fake.requests().len(), 1);

    // Cancellation stops the wait.
    let token = CancellationToken::new();
    let fake = FakeProvider::new().reply(Reply::error(LlmError::RateLimited {
        retry_after: Some(Duration::from_secs(60)),
    }));
    let agent = adder(fake).cancel_on(token.clone());
    let cancel = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        token.cancel();
    };
    let (result, ()) = tokio::join!(agent.run("go"), cancel);
    assert_eq!(result.unwrap_err(), LlmError::Cancelled);
}

const URL: &str = "https://api.example.test/v1/messages";

fn anthropic(fake: &FakeTransport) -> Anthropic {
    let http = Http::new(fake.clone(), HttpOptions::default(), &[]);
    Anthropic::new(
        http,
        "test-key",
        AnthropicConfig::new("claude-opus-5-5", 1024).base_url("https://api.example.test"),
    )
}

/// S4-04: the key and the prompt never follow a redirect, and `Debug` never prints the key.
#[tokio::test]
async fn anthropic_follows_no_redirect_and_hides_its_key() {
    let fake = FakeTransport::new();
    fake.on(
        Method::POST,
        URL,
        FakeResponse::status(307).header("location", "https://evil.example/landing"),
    )
    .on(
        Method::POST,
        "https://evil.example/landing",
        FakeResponse::json(&json!({"content": [], "model": "m"})),
    );
    let claude = anthropic(&fake);
    let err = claude
        .complete(Request {
            system: None,
            messages: vec![Message::user("hello secret prompt")],
            tools: Vec::new(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, LlmError::Request { status: 307, .. }),
        "{err:?}"
    );
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, URL);
    let debug = format!("{claude:?}");
    assert!(!debug.contains("test-key"), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");
}

#[tokio::test]
async fn anthropic_adapter_speaks_the_messages_api() {
    let fake = FakeTransport::new();
    fake.on(
        Method::POST,
        URL,
        FakeResponse::json(&json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
            "content": [
                {"type": "thinking", "thinking": "", "signature": "x"},
                {"type": "text", "text": "Let me add."},
                {"type": "tool_use", "id": "toolu_1", "name": "add", "input": {"a": 2, "b": 3}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 50, "output_tokens": 20, "cache_read_input_tokens": 0}
        })),
    )
    .on(
        Method::POST,
        URL,
        FakeResponse::json(&json!({
            "model": "claude-opus-5-5",
            "content": [{"type": "text", "text": "5"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 80, "output_tokens": 2}
        })),
    );
    let outcome = adder(anthropic(&fake))
        .system("Be brief.")
        .budget(
            Budget::new()
                .max_cost(1.0)
                .price("claude-opus-5-5", Price::per_mtok(4.0, 20.0)),
        )
        .run("2+3?")
        .await
        .unwrap();
    assert_eq!(outcome.text, "5");
    assert_eq!(outcome.usage, Usage::new(130, 22));
    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    let headers = &requests[0].headers;
    assert_eq!(headers.get("x-api-key").unwrap(), "test-key");
    assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
    let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(first["model"], "claude-opus-5-5");
    assert_eq!(first["max_tokens"], 1024);
    assert_eq!(first["system"], "Be brief.");
    assert_eq!(
        first["messages"],
        json!([{"role": "user", "content": [{"type": "text", "text": "2+3?"}]}])
    );
    assert_eq!(first["tools"][0]["name"], "add");
    assert!(first["tools"][0]["input_schema"].is_object());
    let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(
        second["messages"][1],
        json!({"role": "assistant", "content": [
            {"type": "text", "text": "Let me add."},
            {"type": "tool_use", "id": "toolu_1", "name": "add", "input": {"a": 2, "b": 3}}
        ]})
    );
    assert_eq!(
        second["messages"][2],
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "5"}]})
    );
}

#[tokio::test(start_paused = true)]
async fn anthropic_errors_map_and_are_retried_by_the_loop() {
    let fake = FakeTransport::new();
    fake.on(
        Method::POST,
        URL,
        FakeResponse::status(429)
            .header("retry-after", "2")
            .body(r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#),
    )
    .on(
        Method::POST,
        URL,
        FakeResponse::status(529)
            .body(r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#),
    )
    .on(
        Method::POST,
        URL,
        FakeResponse::json(&json!({"model": "m", "content": [{"type": "text", "text": "hi"}], "stop_reason": "end_turn", "usage": {"input_tokens": 1, "output_tokens": 1}})),
    );
    let outcome = Agent::new(anthropic(&fake))
        .retries(3, Duration::from_millis(10)..=Duration::from_millis(10))
        .run("x")
        .await
        .unwrap();
    assert_eq!(outcome.text, "hi");
    let r = fake.requests();
    assert_eq!(r.len(), 3, "the HTTP layer does not retry; the loop does");
    assert!(r[1].at - r[0].at >= Duration::from_secs(2));

    let fake = FakeTransport::new();
    fake.on(
        Method::POST,
        URL,
        FakeResponse::status(400).body(
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad field"}}"#,
        ),
    );
    let err = Agent::new(anthropic(&fake)).run("x").await.unwrap_err();
    assert_eq!(
        err,
        LlmError::Request {
            status: 400,
            message: "bad field".into()
        }
    );
    assert_eq!(fake.requests().len(), 1);

    let err = Anthropic::from_env(
        Http::new(FakeTransport::new(), HttpOptions::default(), &[]),
        AnthropicConfig::new("m", 10),
    );
    // The key comes from the environment; without one this is a configuration error.
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        assert!(matches!(err, Err(LlmError::Config(_))));
    }
}
