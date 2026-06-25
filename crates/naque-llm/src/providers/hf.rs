use crate::{LlmError, LlmRequest, LlmResponse};

use super::openai::openai_chat_completion;

pub struct HfProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl HfProvider {
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        Self {
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://router.huggingface.co".to_string()),
            client: reqwest::Client::builder()
                .user_agent(concat!("naque/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("failed to build reqwest client"),
        }
    }

    pub fn from_env() -> Result<Self, LlmError> {
        let key = std::env::var("HF_TOKEN")
            .map_err(|_| LlmError::Provider("HF_TOKEN not set".to_string()))?;
        Ok(Self::new(key, None))
    }
}

#[async_trait::async_trait]
impl crate::LlmProvider for HfProvider {
    fn name(&self) -> &str {
        "hf"
    }

    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        openai_chat_completion(&self.client, &url, &self.api_key, req).await
    }
}
