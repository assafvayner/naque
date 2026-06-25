use super::openai::openai_chat_completion;
use crate::{LlmError, LlmRequest, LlmResponse};

/// Google Gemini via its OpenAI-compatible endpoint.
///
/// Gemini exposes an OpenAI-compatible chat-completions surface at
/// `https://generativelanguage.googleapis.com/v1beta/openai/chat/completions`,
/// so it reuses the shared OpenAI request/response helpers. Note the base URL
/// already ends in `/v1beta/openai`; only `/chat/completions` is appended
/// (unlike OpenAI/HF which append `/v1/chat/completions`).
pub struct GeminiProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        Self {
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://generativelanguage.googleapis.com/v1beta/openai".to_string()),
            client: reqwest::Client::builder()
                .user_agent(concat!("naque/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("failed to build reqwest client"),
        }
    }

    /// Read the API key from `GEMINI_API_KEY`, falling back to `GOOGLE_API_KEY`.
    pub fn from_env() -> Result<Self, LlmError> {
        let key = std::env::var("GEMINI_API_KEY")
            .or_else(|_| std::env::var("GOOGLE_API_KEY"))
            .map_err(|_| LlmError::Provider("GEMINI_API_KEY (or GOOGLE_API_KEY) not set".into()))?;
        Ok(Self::new(key, None))
    }
}

#[async_trait::async_trait]
impl crate::LlmProvider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }

    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let url = format!("{}/chat/completions", self.base_url);
        openai_chat_completion(&self.client, &url, &self.api_key, req).await
    }
}
