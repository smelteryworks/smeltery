//! The Anthropic Messages API (`POST /v1/messages`) as a [`Provider`].

use std::time::Duration;

use serde_json::{Value, json};

use super::{Content, LlmError, Provider, Request, Response, StopReason, Usage};
use crate::http::{Http, HttpError, Method, Redirects};

/// The API version header this adapter speaks.
const API_VERSION: &str = "2023-06-01";

/// Settings of [`Anthropic`]. The model and `max_tokens` have no defaults: you choose them.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct AnthropicConfig {
    /// The model id, e.g. `claude-opus-5-5`.
    pub model: String,
    /// The most tokens one answer may have.
    pub max_tokens: u32,
    /// The API base URL (default `https://api.anthropic.com`).
    pub base_url: String,
    /// How long one call may take (default 10 minutes).
    pub timeout: Duration,
}

impl AnthropicConfig {
    /// A config for `model` with answers of at most `max_tokens`.
    pub fn new(model: impl Into<String>, max_tokens: u32) -> Self {
        Self {
            model: model.into(),
            max_tokens,
            base_url: "https://api.anthropic.com".to_owned(),
            timeout: Duration::from_secs(600),
        }
    }

    /// Another base URL (a proxy, or a local fake in tests).
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// The per-call timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Claude through the Messages API, over a Watchfire [`Http`] client (so per-host rate limits
/// apply; retries are left to [`Agent`](super::Agent)). Calls follow no redirect (the API key
/// and the prompt go only to `base_url`), and its `Debug` output hides the API key.
///
/// ```no_run
/// # async fn demo(ctx: smeltery_watchfire::AgentCtx) -> Result<(), smeltery_watchfire::llm::LlmError> {
/// use smeltery_watchfire::llm::{Agent, Anthropic, AnthropicConfig};
///
/// let claude = Anthropic::from_env(ctx.http().clone(), AnthropicConfig::new("claude-opus-5-5", 16_000))?;
/// let outcome = Agent::new(claude).cancel_on(ctx.token().clone()).run("Say hello").await?;
/// ctx.log().info(outcome.text);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Anthropic {
    http: Http,
    api_key: String,
    config: AnthropicConfig,
}

impl std::fmt::Debug for Anthropic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Anthropic")
            .field("api_key", &"<redacted>")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Anthropic {
    /// With an explicit API key.
    pub fn new(http: Http, api_key: impl Into<String>, config: AnthropicConfig) -> Self {
        Self {
            http,
            api_key: api_key.into(),
            config,
        }
    }

    /// With the key from `ANTHROPIC_API_KEY` (`.env` or the environment).
    ///
    /// # Errors
    /// `ANTHROPIC_API_KEY` is not set.
    pub fn from_env(http: Http, config: AnthropicConfig) -> Result<Self, LlmError> {
        let key = smeltery_core::config::env::<Option<String>>("ANTHROPIC_API_KEY", None)
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| LlmError::Config("ANTHROPIC_API_KEY is not set".to_owned()))?;
        Ok(Self::new(http, key, config))
    }

    /// The request body.
    pub(crate) fn body(&self, request: &Request) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("model".to_owned(), json!(self.config.model));
        body.insert("max_tokens".to_owned(), json!(self.config.max_tokens));
        body.insert("messages".to_owned(), json!(request.messages));
        if let Some(system) = &request.system {
            body.insert("system".to_owned(), json!(system));
        }
        if !request.tools.is_empty() {
            body.insert("tools".to_owned(), json!(request.tools));
        }
        Value::Object(body)
    }
}

fn parse_response(value: &Value) -> Result<Response, LlmError> {
    let decode = |what: &str| LlmError::Decode(format!("missing `{what}`"));
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| decode("content"))?;
    let mut content = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => content.push(Content::Text {
                text: block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            }),
            Some("tool_use") => content.push(Content::ToolUse {
                id: block
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| decode("tool_use.id"))?
                    .to_owned(),
                name: block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| decode("tool_use.name"))?
                    .to_owned(),
                input: block.get("input").cloned().unwrap_or(Value::Null),
            }),
            // Thinking and other block types are not part of this loop's conversation.
            _ => {}
        }
    }
    let usage = value.get("usage");
    let tokens = |field: &str| {
        usage
            .and_then(|u| u.get(field))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    Ok(Response {
        model: value
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        content,
        stop_reason: StopReason::parse(
            value
                .get("stop_reason")
                .and_then(Value::as_str)
                .unwrap_or("end_turn"),
        ),
        usage: Usage::new(tokens("input_tokens"), tokens("output_tokens")),
    })
}

impl Provider for Anthropic {
    async fn complete(&self, request: Request) -> Result<Response, LlmError> {
        let url = format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'));
        let response = self
            .http
            .request(Method::POST, &url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&self.body(&request))
            .timeout(self.config.timeout)
            .retries(0)
            // A redirect would carry `x-api-key` and the conversation to wherever it points.
            .redirects(Redirects::None)
            .send()
            .await
            .map_err(|e| match e {
                HttpError::Cancelled => LlmError::Cancelled,
                other => LlmError::Transport(other.to_string()),
            })?;
        let status = response.status().as_u16();
        let body: Value = response
            .json()
            .await
            .map_err(|e| LlmError::Decode(e.to_string()))
            .unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return parse_response(&body);
        }
        let retry_after = crate::http::retry_after(&response);
        let message = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("no message")
            .to_owned();
        let kind = body.pointer("/error/type").and_then(Value::as_str);
        Err(match (status, kind) {
            (429, _) => LlmError::RateLimited { retry_after },
            (529, _) | (_, Some("overloaded_error")) => LlmError::Overloaded { retry_after },
            (500..=599, _) => LlmError::Server { status, message },
            _ => LlmError::Request { status, message },
        })
    }
}
