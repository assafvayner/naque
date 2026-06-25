use serde_json::Value;

use crate::{LlmError, LlmRequest, LlmResponse};

use super::openai::{openai_build_body, openai_parse_response};

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
        let body = openai_build_body(req);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Provider(e.to_string()))?;

        let status = resp.status();
        let json: Value = resp
            .json()
            .await
            .map_err(|e| LlmError::Provider(e.to_string()))?;

        if !status.is_success() {
            let msg = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(LlmError::Provider(format!("HTTP {status}: {msg}")));
        }

        openai_parse_response(&json)
    }
}
