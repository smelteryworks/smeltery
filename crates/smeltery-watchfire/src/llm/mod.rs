//! LLM helpers (feature `llm`): a [`Provider`] trait with a scripted [`FakeProvider`] and the
//! [`Anthropic`] Messages API adapter, and [`Agent`], a tool-calling loop with typed tools, a
//! turn limit, a token / cost [`Budget`] and retries.
//!
//! ```
//! use smeltery_watchfire::llm::{Agent, FakeProvider, Reply};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let provider = FakeProvider::new()
//!     .reply(Reply::tool_call("add", serde_json::json!({"a": 2, "b": 3})))
//!     .reply(Reply::text("2 + 3 = 5"));
//! #[derive(serde::Deserialize)]
//! struct Add { a: i64, b: i64 }
//! let outcome = Agent::new(provider.clone())
//!     .system("You add numbers.")
//!     .tool(
//!         "add",
//!         "Add two integers",
//!         serde_json::json!({"type": "object", "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}}, "required": ["a", "b"]}),
//!         |input: Add| async move { Ok(input.a + input.b) },
//!     )
//!     .run("What is 2 + 3?")
//!     .await
//!     .unwrap();
//! assert_eq!(outcome.text, "2 + 3 = 5");
//! assert_eq!(outcome.turns, 2);
//! # }
//! ```

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use smeltery_core::BoxFuture;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::policy::{Backoff, Jitter};

mod anthropic;

pub use anthropic::{Anthropic, AnthropicConfig};

/// Who sent a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Role {
    /// The user (also carries tool results).
    User,
    /// The model.
    Assistant,
}

/// One block of a message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Content {
    /// Text.
    Text {
        /// The text.
        text: String,
    },
    /// The model asks to call a tool.
    ToolUse {
        /// The call's id, echoed in the result.
        id: String,
        /// The tool name.
        name: String,
        /// The arguments.
        input: serde_json::Value,
    },
    /// A tool's answer.
    ToolResult {
        /// The call it answers.
        tool_use_id: String,
        /// The result as text (JSON for structured results).
        content: String,
        /// Whether the call failed.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

impl Content {
    /// A text block.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

/// A message of the conversation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Message {
    /// Who sent it.
    pub role: Role,
    /// Its blocks.
    pub content: Vec<Content>,
}

impl Message {
    /// A user message with one text block.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![Content::text(text)],
        }
    }

    /// The concatenated text blocks.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| match c {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// A tool as the model sees it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ToolSpec {
    /// The name.
    pub name: String,
    /// What it does (the model reads this).
    pub description: String,
    /// JSON schema of the input.
    pub input_schema: serde_json::Value,
}

/// What a provider is asked.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Request {
    /// The system prompt.
    pub system: Option<String>,
    /// The conversation so far.
    pub messages: Vec<Message>,
    /// The tools the model may call.
    pub tools: Vec<ToolSpec>,
}

/// Why the model stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StopReason {
    /// It finished (`end_turn`).
    EndTurn,
    /// It hit `max_tokens`.
    MaxTokens,
    /// It wants tools called (`tool_use`).
    ToolUse,
    /// It produced a stop sequence.
    StopSequence,
    /// It declined (`refusal`).
    Refusal,
    /// Anything else the provider reports.
    Other(String),
}

impl StopReason {
    /// From the API's string.
    pub fn parse(s: &str) -> Self {
        match s {
            "end_turn" => Self::EndTurn,
            "max_tokens" => Self::MaxTokens,
            "tool_use" => Self::ToolUse,
            "stop_sequence" => Self::StopSequence,
            "refusal" => Self::Refusal,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// Tokens used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Usage {
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
}

impl Usage {
    /// Input and output tokens.
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
        }
    }

    /// Both together.
    pub fn total(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    fn add(&mut self, other: Usage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
    }
}

/// A provider's answer.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Response {
    /// The model that answered (for prices).
    pub model: String,
    /// The answer's blocks.
    pub content: Vec<Content>,
    /// Why it stopped.
    pub stop_reason: StopReason,
    /// Tokens used by this call.
    pub usage: Usage,
}

/// Why an LLM call or the loop failed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum LlmError {
    /// 429: too many requests (retried).
    #[error("rate limited")]
    RateLimited {
        /// The provider's `Retry-After`.
        retry_after: Option<Duration>,
    },
    /// The provider is overloaded (529; retried).
    #[error("the provider is overloaded")]
    Overloaded {
        /// The provider's `Retry-After`.
        retry_after: Option<Duration>,
    },
    /// A 5xx answer (retried).
    #[error("provider error {status}: {message}")]
    Server {
        /// The status code.
        status: u16,
        /// The provider's message.
        message: String,
    },
    /// The request was refused (4xx other than 429; not retried).
    #[error("request refused ({status}): {message}")]
    Request {
        /// The status code.
        status: u16,
        /// The provider's message.
        message: String,
    },
    /// The network failed (retried).
    #[error("transport: {0}")]
    Transport(String),
    /// The answer could not be read.
    #[error("cannot decode the answer: {0}")]
    Decode(String),
    /// Setup is missing something (an API key, a price).
    #[error("{0}")]
    Config(String),
    /// The token or cost budget is spent.
    #[error("budget exceeded: {0}")]
    BudgetExceeded(String),
    /// The loop ran its maximum number of turns.
    #[error("no final answer after {0} turns")]
    MaxTurns(u32),
    /// Cancelled.
    #[error("cancelled")]
    Cancelled,
}

impl LlmError {
    /// Whether retrying may help.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. }
                | Self::Overloaded { .. }
                | Self::Server { .. }
                | Self::Transport(_)
        )
    }

    fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } | Self::Overloaded { retry_after } => *retry_after,
            _ => None,
        }
    }
}

/// An LLM behind an API.
pub trait Provider: Send + Sync + 'static {
    /// Complete the conversation once.
    fn complete(&self, request: Request)
    -> impl Future<Output = Result<Response, LlmError>> + Send;
}

/// One scripted answer of a [`FakeProvider`].
#[derive(Clone, Debug)]
pub struct Reply(Result<Response, LlmError>);

impl Reply {
    /// A final text answer (usage 10 in / 5 out).
    pub fn text(text: &str) -> Self {
        Self(Ok(Response {
            model: "fake".to_owned(),
            content: vec![Content::text(text)],
            stop_reason: StopReason::EndTurn,
            usage: Usage::new(10, 5),
        }))
    }

    /// A call of tool `name` with `input` (usage 10 in / 5 out).
    pub fn tool_call(name: &str, input: serde_json::Value) -> Self {
        Self::tool_calls(&[(name, input)])
    }

    /// Several tool calls in one answer.
    pub fn tool_calls(calls: &[(&str, serde_json::Value)]) -> Self {
        let content = calls
            .iter()
            .enumerate()
            .map(|(i, (name, input))| Content::ToolUse {
                id: format!("toolu_fake_{i}"),
                name: (*name).to_owned(),
                input: input.clone(),
            })
            .collect();
        Self(Ok(Response {
            model: "fake".to_owned(),
            content,
            stop_reason: StopReason::ToolUse,
            usage: Usage::new(10, 5),
        }))
    }

    /// A failure.
    pub fn error(error: LlmError) -> Self {
        Self(Err(error))
    }

    /// Set the usage.
    pub fn usage(mut self, input: u64, output: u64) -> Self {
        if let Ok(r) = &mut self.0 {
            r.usage = Usage::new(input, output);
        }
        self
    }

    /// Set the model name.
    pub fn model(mut self, model: &str) -> Self {
        if let Ok(r) = &mut self.0 {
            r.model = model.to_owned();
        }
        self
    }

    /// Set the stop reason.
    pub fn stop_reason(mut self, reason: StopReason) -> Self {
        if let Ok(r) = &mut self.0 {
            r.stop_reason = reason;
        }
        self
    }
}

/// A provider answering from a script, in order, recording every request. With the script
/// used up it answers `Config("no scripted reply left")`.
#[derive(Clone, Debug, Default)]
pub struct FakeProvider {
    inner: Arc<Mutex<FakeInner>>,
}

#[derive(Debug, Default)]
struct FakeInner {
    replies: VecDeque<Reply>,
    requests: Vec<Request>,
}

impl FakeProvider {
    /// An empty script.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an answer to the script.
    pub fn reply(self, reply: Reply) -> Self {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replies
            .push_back(reply);
        self
    }

    /// Every request so far.
    pub fn requests(&self) -> Vec<Request> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .requests
            .clone()
    }
}

impl Provider for FakeProvider {
    async fn complete(&self, request: Request) -> Result<Response, LlmError> {
        let reply = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.requests.push(request);
            inner.replies.pop_front()
        };
        match reply {
            Some(Reply(result)) => result,
            None => Err(LlmError::Config("no scripted reply left".to_owned())),
        }
    }
}

/// Prices per million tokens, in any currency unit you like (dollars, typically).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    /// Per million input tokens.
    pub input_per_mtok: f64,
    /// Per million output tokens.
    pub output_per_mtok: f64,
}

impl Price {
    /// A price per million input and output tokens.
    pub fn per_mtok(input: f64, output: f64) -> Self {
        Self {
            input_per_mtok: input,
            output_per_mtok: output,
        }
    }

    fn cost(&self, usage: Usage) -> f64 {
        (usage.input_tokens as f64 * self.input_per_mtok
            + usage.output_tokens as f64 * self.output_per_mtok)
            / 1_000_000.0
    }
}

/// Limits for one [`Agent::run`]: total tokens (input + output) and / or cost (with the
/// prices you supply per model). The run stops with `BudgetExceeded` once a limit is passed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Budget {
    max_tokens: Option<u64>,
    max_cost: Option<f64>,
    prices: HashMap<String, Price>,
}

impl Budget {
    /// No limits.
    pub fn new() -> Self {
        Self::default()
    }

    /// At most `n` tokens in total.
    pub fn max_tokens(mut self, n: u64) -> Self {
        self.max_tokens = Some(n);
        self
    }

    /// At most `cost` (needs a price for every model that answers).
    pub fn max_cost(mut self, cost: f64) -> Self {
        self.max_cost = Some(cost);
        self
    }

    /// The price of `model`.
    pub fn price(mut self, model: &str, price: Price) -> Self {
        self.prices.insert(model.to_owned(), price);
        self
    }
}

type ToolFn =
    Arc<dyn Fn(serde_json::Value) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

struct Tool {
    spec: ToolSpec,
    call: ToolFn,
}

/// What [`Agent::run`] returns.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Outcome {
    /// The final answer's text.
    pub text: String,
    /// Why the last answer stopped (`EndTurn`, or `MaxTokens` / `Refusal`).
    pub stop_reason: StopReason,
    /// The whole conversation.
    pub messages: Vec<Message>,
    /// Model calls made.
    pub turns: u32,
    /// Tokens used in total.
    pub usage: Usage,
    /// The cost, when prices are known for every model that answered.
    pub cost: Option<f64>,
}

/// A tool-calling loop: ask the model, run the tools it asks for, send the results back,
/// until it answers without tool calls.
pub struct Agent<P> {
    provider: P,
    system: Option<String>,
    tools: Vec<Tool>,
    max_turns: u32,
    budget: Budget,
    retries: u32,
    backoff: Backoff,
    jitter: Jitter,
    cancel: CancellationToken,
}

impl<P> std::fmt::Debug for Agent<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field(
                "tools",
                &self.tools.iter().map(|t| &t.spec.name).collect::<Vec<_>>(),
            )
            .field("max_turns", &self.max_turns)
            .finish_non_exhaustive()
    }
}

impl<P: Provider> Agent<P> {
    /// A loop over `provider`: 10 turns, 3 retries (backoff 1 s..=30 s, full jitter, honouring
    /// `Retry-After`), no budget.
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            system: None,
            tools: Vec::new(),
            max_turns: 10,
            budget: Budget::default(),
            retries: 3,
            backoff: Backoff::new(Duration::from_secs(1)..=Duration::from_secs(30)),
            jitter: Jitter::from_os(),
            cancel: CancellationToken::new(),
        }
    }

    /// The system prompt.
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// A typed tool: `input_schema` describes `I` for the model; `handler`'s output is sent
    /// back as JSON (a `String` as plain text). A handler error, an input that does not
    /// deserialize, or an unknown tool goes back to the model as an error result.
    pub fn tool<I, O, F, Fut>(
        mut self,
        name: &str,
        description: &str,
        input_schema: serde_json::Value,
        handler: F,
    ) -> Self
    where
        I: DeserializeOwned + Send + 'static,
        O: Serialize + 'static,
        F: Fn(I) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O, AgentError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let call: ToolFn = Arc::new(move |input| {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let input: I =
                    serde_json::from_value(input).map_err(|e| format!("invalid input: {e}"))?;
                let output = handler(input).await.map_err(|e| e.to_string())?;
                let value = serde_json::to_value(output).map_err(|e| e.to_string())?;
                Ok(match value {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                })
            })
        });
        self.tools.push(Tool {
            spec: ToolSpec {
                name: name.to_owned(),
                description: description.to_owned(),
                input_schema,
            },
            call,
        });
        self
    }

    /// The most model calls per run (at least 1).
    pub fn max_turns(mut self, turns: u32) -> Self {
        self.max_turns = turns.max(1);
        self
    }

    /// Token / cost limits.
    pub fn budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    /// Retries per model call on 429, 529, 5xx and network errors.
    pub fn retries(mut self, retries: u32, backoff: std::ops::RangeInclusive<Duration>) -> Self {
        self.retries = retries;
        self.backoff = Backoff::new(backoff);
        self
    }

    /// Stop waiting (and fail with `Cancelled`) when `token` is cancelled, e.g. `ctx.token()`.
    pub fn cancel_on(mut self, token: CancellationToken) -> Self {
        self.cancel = token;
        self
    }

    /// Run from one user message.
    ///
    /// # Errors
    /// A provider error after the retries, `BudgetExceeded`, `MaxTurns`, `Cancelled`.
    pub async fn run(&self, prompt: &str) -> Result<Outcome, LlmError> {
        self.run_messages(vec![Message::user(prompt)]).await
    }

    /// Run from a conversation.
    ///
    /// # Errors
    /// See [`Agent::run`].
    pub async fn run_messages(&self, mut messages: Vec<Message>) -> Result<Outcome, LlmError> {
        let mut usage = Usage::default();
        let mut cost = Some(0.0_f64);
        let specs: Vec<ToolSpec> = self.tools.iter().map(|t| t.spec.clone()).collect();
        for turn in 1..=self.max_turns {
            let request = Request {
                system: self.system.clone(),
                messages: messages.clone(),
                tools: specs.clone(),
            };
            let response = self.complete_with_retries(request).await?;
            usage.add(response.usage);
            cost = match (cost, self.budget.prices.get(&response.model)) {
                (Some(c), Some(price)) => Some(c + price.cost(response.usage)),
                _ => None,
            };
            self.check_budget(usage, cost, &response.model)?;
            let stop = response.stop_reason.clone();
            let calls: Vec<(String, String, serde_json::Value)> = response
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::ToolUse { id, name, input } => {
                        Some((id.clone(), name.clone(), input.clone()))
                    }
                    _ => None,
                })
                .collect();
            let answer = Message {
                role: Role::Assistant,
                content: response.content,
            };
            let text = answer.text();
            messages.push(answer);
            if stop != StopReason::ToolUse || calls.is_empty() {
                return Ok(Outcome {
                    text,
                    stop_reason: stop,
                    messages,
                    turns: turn,
                    usage,
                    cost,
                });
            }
            // Every result of this turn goes back in one user message.
            let mut results = Vec::with_capacity(calls.len());
            for (id, name, input) in calls {
                let result = match self.tools.iter().find(|t| t.spec.name == name) {
                    Some(tool) => {
                        tokio::select! {
                            biased;
                            () = self.cancel.cancelled() => return Err(LlmError::Cancelled),
                            r = (tool.call)(input) => r,
                        }
                    }
                    None => Err(format!("unknown tool `{name}`")),
                };
                results.push(match result {
                    Ok(content) => Content::ToolResult {
                        tool_use_id: id,
                        content,
                        is_error: false,
                    },
                    Err(content) => Content::ToolResult {
                        tool_use_id: id,
                        content,
                        is_error: true,
                    },
                });
            }
            messages.push(Message {
                role: Role::User,
                content: results,
            });
        }
        Err(LlmError::MaxTurns(self.max_turns))
    }

    fn check_budget(&self, usage: Usage, cost: Option<f64>, model: &str) -> Result<(), LlmError> {
        if let Some(max) = self.budget.max_tokens
            && usage.total() > max
        {
            return Err(LlmError::BudgetExceeded(format!(
                "{} tokens used, the limit is {max}",
                usage.total()
            )));
        }
        if let Some(max) = self.budget.max_cost {
            let Some(cost) = cost else {
                return Err(LlmError::Config(format!(
                    "a cost budget needs a price for model `{model}`"
                )));
            };
            if cost > max {
                return Err(LlmError::BudgetExceeded(format!(
                    "cost {cost:.6} is above the limit {max}"
                )));
            }
        }
        Ok(())
    }

    async fn complete_with_retries(&self, request: Request) -> Result<Response, LlmError> {
        let mut attempt = 0_u32;
        loop {
            let result = tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(LlmError::Cancelled),
                r = self.provider.complete(request.clone()) => r,
            };
            match result {
                Ok(response) => return Ok(response),
                Err(e) if e.is_retryable() && attempt < self.retries => {
                    let delay = e
                        .retry_after()
                        .unwrap_or_else(|| self.backoff.delay(attempt, &self.jitter));
                    tracing::warn!(error = %e, attempt, delay_ms = delay.as_millis(), "LLM call failed; retrying");
                    tokio::select! {
                        biased;
                        () = self.cancel.cancelled() => return Err(LlmError::Cancelled),
                        () = tokio::time::sleep(delay) => {}
                    }
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests;
