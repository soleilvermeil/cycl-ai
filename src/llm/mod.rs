mod bilan;

pub(crate) use bilan::DEFAULT_SYSTEM_PROMPT as BILAN_DEFAULT_SYSTEM_PROMPT;
pub use bilan::{build_activity_message, generate_activity_bilans};

use serde::{Deserialize, Serialize};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MODEL: &str = "claude-sonnet-5";

const OPENAI_API_URL: &str = "https://api.openai.com/v1/responses";
/// Featured text models (Responses API). Aliases, so they track the current
/// snapshot without a code change. Sol is the balanced default.
pub const OPENAI_MODELS: &[(&str, &str)] = &[
    ("gpt-6-luna", "GPT-6 Luna — rapide, le moins cher"),
    ("gpt-6.1-terra", "GPT-6.1 Terra — texte soigné"),
    ("gpt-6.1-sol", "GPT-6.1 Sol — équilibré"),
    ("gpt-6-astra", "GPT-6 Astra — le plus capable"),
];
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-6.1-sol";

/// Known model id, or the default when the stored value is not in the list.
pub fn openai_model_or_default(model: &str) -> &'static str {
    let model = model.trim();
    OPENAI_MODELS
        .iter()
        .find(|(id, _)| *id == model)
        .map(|(id, _)| *id)
        .unwrap_or(DEFAULT_OPENAI_MODEL)
}

/// Abstraction over "send a system+user prompt, get text back" so the
/// prompt-building/parsing logic can be unit tested without spending real
/// money calling the LLM API.
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
        // Callers check `Settings::has_llm_key` first to show a proper
        // warning; this is the backstop so a missed check fails with a clear
        // message instead of an opaque 401 from the API.
        if self.api_key.trim().is_empty() {
            anyhow::bail!("no Anthropic API key configured");
        }
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

pub struct OpenAIClient {
    api_key: String,
    model: String,
    http: reqwest::Client,
}

impl OpenAIClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            http: reqwest::Client::new(),
        }
    }
}

/// One client for whichever provider the settings currently select.
pub enum ProviderClient {
    Anthropic(AnthropicClient),
    OpenAI(OpenAIClient),
}

impl ProviderClient {
    pub fn from_settings(settings: &crate::settings::Settings) -> Self {
        if settings.llm_provider == "openai" {
            Self::OpenAI(OpenAIClient::new(
                settings.openai_api_key.clone(),
                openai_model_or_default(&settings.openai_model).to_string(),
            ))
        } else {
            Self::Anthropic(AnthropicClient::new(settings.anthropic_api_key.clone()))
        }
    }
}

impl LlmClient for ProviderClient {
    async fn complete(&self, system: &str, user_message: &str) -> anyhow::Result<String> {
        match self {
            Self::Anthropic(client) => client.complete(system, user_message).await,
            Self::OpenAI(client) => client.complete(system, user_message).await,
        }
    }
}

#[derive(Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    instructions: &'a str,
    input: &'a str,
    reasoning: ReasoningEffort,
    max_output_tokens: u32,
    store: bool,
}

#[derive(Serialize)]
struct ReasoningEffort {
    effort: &'static str,
}

#[derive(Deserialize)]
struct ResponsesBody {
    output: Vec<OutputItem>,
}

#[derive(Deserialize)]
struct OutputItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    content: Vec<OutputContent>,
}

#[derive(Deserialize)]
struct OutputContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

/// Reasoning models put a `reasoning` item before the message, so the text
/// is not at `output[0]`. Collect every `output_text` block instead.
fn output_text(body: &ResponsesBody) -> String {
    body.output
        .iter()
        .filter(|item| item.kind == "message")
        .flat_map(|item| item.content.iter())
        .filter(|part| part.kind == "output_text")
        .filter_map(|part| part.text.clone())
        .collect::<Vec<_>>()
        .join("")
}

impl LlmClient for OpenAIClient {
    async fn complete(&self, system: &str, user_message: &str) -> anyhow::Result<String> {
        if self.api_key.trim().is_empty() {
            anyhow::bail!("no OpenAI API key configured");
        }
        let body = ResponsesRequest {
            model: openai_model_or_default(&self.model),
            instructions: system,
            input: user_message,
            // `low` is accepted by every featured model. `none` is not, and
            // a higher effort spends the output budget on reasoning tokens
            // before the bilan or the digest JSON.
            reasoning: ReasoningEffort { effort: "low" },
            max_output_tokens: 4096,
            // One-shot call: don't keep the athlete profile on OpenAI's side.
            store: false,
        };

        let resp = self
            .http
            .post(OPENAI_API_URL)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            let detail = String::from_utf8_lossy(&bytes);
            let detail = detail.chars().take(500).collect::<String>();
            anyhow::bail!("OpenAI HTTP {status}: {detail}");
        }
        let parsed: ResponsesBody = serde_json::from_slice(&bytes)?;
        Ok(output_text(&parsed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_text_skips_the_reasoning_item() {
        let body = ResponsesBody {
            output: vec![
                OutputItem {
                    kind: "reasoning".into(),
                    content: vec![],
                },
                OutputItem {
                    kind: "message".into(),
                    content: vec![OutputContent {
                        kind: "output_text".into(),
                        text: Some("bilan court".into()),
                    }],
                },
            ],
        };
        assert_eq!(output_text(&body), "bilan court");
    }

    #[test]
    fn unknown_model_falls_back_to_the_default() {
        assert_eq!(openai_model_or_default("gpt-6.1-sol"), "gpt-6.1-sol");
        assert_eq!(openai_model_or_default("gpt-4o"), DEFAULT_OPENAI_MODEL);
        assert_eq!(openai_model_or_default("  "), DEFAULT_OPENAI_MODEL);
    }
}
