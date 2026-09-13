mod bilan;

pub use bilan::generate_activity_bilans;

use serde::{Deserialize, Serialize};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MODEL: &str = "claude-sonnet-5";

/// Abstraction over "send a system+user prompt, get text back" so the
/// prompt-building/parsing logic can be unit tested without spending real
/// money calling the Anthropic API.
pub trait LlmClient {
    async fn complete(&self, system: &str, user_message: &str) -> anyhow::Result<String>;
}

pub struct AnthropicClient {
    api_key: String,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            http: reqwest::Client::new(),
        }
    }
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<RequestMessage<'a>>,
    thinking: ThinkingConfig,
}

#[derive(Serialize)]
struct ThinkingConfig {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct RequestMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

/// Extended thinking is enabled account-wide by default, which prepends a
/// `{"type":"thinking", ...}` block (no `text` field) before the actual
/// text block — we disable it in the request (cost/latency don't need it
/// for this task) but still parse defensively in case a block ever lacks
/// `text` for some other reason (redacted content, tool use, etc.).
#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

impl LlmClient for AnthropicClient {
    async fn complete(&self, system: &str, user_message: &str) -> anyhow::Result<String> {
        let body = MessagesRequest {
            model: MODEL,
            // Bilans are short (2-4 sentences), but digests need room for a
            // full structured workout JSON plus verbose notes — 1024 was
            // observed truncating real digest responses mid-JSON.
            max_tokens: 4096,
            system,
            messages: vec![RequestMessage {
                role: "user",
                content: user_message,
            }],
            thinking: ThinkingConfig { kind: "disabled" },
        };

        let resp: MessagesResponse = self
            .http
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        Ok(resp
            .content
            .into_iter()
            .find(|c| c.kind == "text")
            .and_then(|c| c.text)
            .unwrap_or_default())
    }
}
