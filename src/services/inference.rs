use anyhow::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// Build the shared HTTP client for inference calls. A finite overall timeout means a slow/hung
/// provider (e.g. Ollama Cloud stalling) fails cleanly instead of holding the request open until
/// the server drops the socket ("connection closed before message completed").
fn build_http_client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| Client::new())
}

/// Strip markdown code fences that LLMs often wrap JSON responses in.
pub fn strip_fences(s: &str) -> &str {
    s.trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
}

/// Ollama Cloud — OpenAI-compatible API for orchestrator + embeddings.
/// Cloudflare Workers AI — for personalized worker inference with LoRA adapters.

#[derive(Clone)]
pub struct InferenceClient {
    client: Client,
    ollama_base_url: String,
    ollama_api_key: String,
    cf_account_id: Option<String>,
    cf_api_token: Option<String>,
    embed_model: Arc<Mutex<TextEmbedding>>,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

/// Cloudflare Workers AI chat request — uses `lora` field instead of model switching.
#[derive(Debug, Serialize)]
struct CfChatRequest {
    messages: Vec<ChatMessage>,
    temperature: f32,
    raw: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    lora: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    format_type: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

/// Cloudflare Workers AI response shape differs slightly — result is direct.
#[derive(Debug, Deserialize)]
struct CfChatResponse {
    result: CfChatResult,
}

#[derive(Debug, Deserialize)]
struct CfChatResult {
    response: String,
}

fn init_embed_model() -> Arc<Mutex<TextEmbedding>> {
    let options = InitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(true);
    Arc::new(Mutex::new(
        TextEmbedding::try_new(options).expect("Failed to init fastembed model"),
    ))
}

impl InferenceClient {
    pub fn new(ollama_base_url: String, ollama_api_key: String) -> Self {
        Self {
            client: build_http_client(),
            ollama_base_url: ollama_base_url.trim_end_matches('/').to_string(),
            ollama_api_key,
            cf_account_id: None,
            cf_api_token: None,
            embed_model: init_embed_model(),
        }
    }

    pub fn from_env() -> Self {
        let ollama_base_url =
            std::env::var("OLLAMA_BASE_URL").expect("OLLAMA_BASE_URL must be set");
        let ollama_api_key = std::env::var("OLLAMA_API_KEY").unwrap_or_default();
        let cf_account_id = std::env::var("CF_ACCOUNT_ID").ok();
        let cf_api_token = std::env::var("CF_API_TOKEN").ok();

        Self {
            client: build_http_client(),
            ollama_base_url: ollama_base_url.trim_end_matches('/').to_string(),
            ollama_api_key,
            cf_account_id,
            cf_api_token,
            embed_model: init_embed_model(),
        }
    }

    /// Call the orchestrator model (minimax-m3) for reasoning and extraction tasks.
    pub async fn orchestrate(&self, system: &str, user: &str, json_mode: bool) -> Result<String> {
        let model = std::env::var("ORCHESTRATOR_MODEL").unwrap_or_else(|_| "minimax-m3".into());
        self.ollama_chat(&model, system, user, 0.2, json_mode).await
    }

    /// Call the worker model with optional LoRA personalization.
    ///
    /// If `lora_id` is Some (a Cloudflare finetune_id) AND Cloudflare credentials
    /// are configured, the request goes to Cloudflare Workers AI with the adapter active.
    /// Otherwise falls back to Ollama Cloud base model (no personalization).
    pub async fn worker(&self, system: &str, user: &str, lora_id: Option<&str>) -> Result<String> {
        if let (Some(lora), Some(account_id), Some(token)) = (
            lora_id,
            self.cf_account_id.as_deref(),
            self.cf_api_token.as_deref(),
        ) {
            return self
                .cf_worker(account_id, token, system, user, Some(lora))
                .await;
        }

        // Fallback: Ollama base model (no personalization)
        let model = std::env::var("WORKER_MODEL").unwrap_or_else(|_| "qwen2.5-coder:7b".into());
        self.ollama_chat(&model, system, user, 0.3, false).await
    }

    /// Generate embeddings locally via fastembed (AllMiniLML6V2).
    pub async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let model = self.embed_model.clone();
        tokio::task::spawn_blocking(move || {
            let mut m = model.lock().expect("embed_model lock poisoned");
            m.embed(texts, None).map_err(anyhow::Error::from)
        })
        .await?
    }

    /// Cloudflare Workers AI inference with LoRA adapter.
    /// POST /accounts/{id}/ai/run/@cf/mistral/mistral-7b-instruct-v0.2-lora
    async fn cf_worker(
        &self,
        account_id: &str,
        api_token: &str,
        system: &str,
        user: &str,
        lora: Option<&str>,
    ) -> Result<String> {
        let model = std::env::var("CF_WORKER_MODEL")
            .unwrap_or_else(|_| "@cf/mistral/mistral-7b-instruct-v0.2-lora".into());

        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: system.into(),
            },
            ChatMessage {
                role: "user".into(),
                content: user.into(),
            },
        ];

        let body = CfChatRequest {
            messages,
            temperature: 0.3,
            raw: true,
            lora: lora.map(|s| s.to_string()),
        };

        let url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/ai/run/{}",
            account_id, model
        );

        let resp = self
            .client
            .post(&url)
            .bearer_auth(api_token)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<CfChatResponse>()
            .await?;

        Ok(resp.result.response)
    }

    async fn ollama_chat(
        &self,
        model: &str,
        system: &str,
        user: &str,
        temperature: f32,
        json_mode: bool,
    ) -> Result<String> {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: system.into(),
            },
            ChatMessage {
                role: "user".into(),
                content: user.into(),
            },
        ];

        let response_format = json_mode.then(|| ResponseFormat {
            format_type: "json_object".into(),
        });

        let req_body = ChatRequest {
            model: model.into(),
            messages,
            temperature,
            response_format,
        };

        // Retry once on transient transport failures (Ollama Cloud occasionally drops the socket
        // mid-stream — "connection closed before message completed"). HTTP error statuses are not
        // retried (they aren't transient); only send/transport errors are.
        let url = format!("{}/chat/completions", self.ollama_base_url);
        let mut attempt = 0;
        let http_resp = loop {
            attempt += 1;
            let result = self
                .client
                .post(&url)
                .bearer_auth(&self.ollama_api_key)
                .json(&req_body)
                .send()
                .await;
            match result {
                Ok(r) => break r.error_for_status()?,
                Err(e) if attempt < 2 => {
                    tracing::warn!(
                        "inference request to {url} failed (attempt {attempt}), retrying: {e}"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        };

        let body = http_resp.text().await?;
        let resp = serde_json::from_str::<ChatResponse>(&body)
            .map_err(|e| anyhow::anyhow!("Failed to parse Ollama response: {e}\nBody: {body}"))?;

        resp.choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("Empty response from inference API"))
    }
}
