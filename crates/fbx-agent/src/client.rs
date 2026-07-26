//! `OptimizerClient` — the agent's only door to LLM providers.
//!
//! THE RULE OF THIS PATTERN (shared with custom-agents' agent-core): fbx-agent never calls a
//! provider directly. It calls the RFA Labs **optimizer edge** (OpenAI-compatible), which routes
//! /caches/quality-gates the call and meters the savings. `base_url` points at the optimizer, the
//! `Authorization: Bearer` token is the optimizer API key (`sk-opt-…`), and the upstream provider
//! key rides in `x-provider-key` (optional when the optimizer has a server/tenant-configured key).

use serde::Deserialize;
use serde_json::Value;

/// Config for reaching the optimizer edge.
#[derive(Debug, Clone)]
pub struct OptimizerConfig {
    /// Base URL of the optimizer edge, e.g. `http://127.0.0.1:8088/v1`.
    pub base_url: String,
    /// The optimizer API key (`sk-opt-…`) authenticating the tenant.
    pub optimizer_key: String,
    /// The upstream provider key (`x-provider-key`). `None` when the optimizer resolves the
    /// provider key server/tenant-side (the onboarding / BYOK-held-tenant-side path).
    pub provider_key: Option<String>,
    /// Optional model pin. `None` lets the optimizer's router pick + downshift — where routing
    /// savings come from.
    pub model: Option<String>,
}

/// What the model returned for one turn: free-text content, native `tool_calls`, or both.
#[derive(Debug, Clone)]
pub struct Turn {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// The model the optimizer actually served (may be a downshift) — surfaced for telemetry once
    /// the engine records per-run savings/routing.
    pub model_served: String,
    pub raw: Value,
}

/// A native OpenAI tool call the model emitted.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolCall {
    pub id: String,
    /// OpenAI's call type discriminator (always "function" today) — parsed for forward-compat.
    #[serde(rename = "type", default)]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FunctionCall {
    pub name: String,
    /// Raw JSON-string of the args (OpenAI sends this as a string).
    #[serde(default)]
    pub arguments: String,
}

/// A thin OpenAI-compatible chat client pointed at the optimizer edge.
pub struct OptimizerClient {
    cfg: OptimizerConfig,
    http: reqwest::Client,
}

impl OptimizerClient {
    pub fn new(cfg: OptimizerConfig) -> Self {
        Self {
            cfg,
            http: reqwest::Client::new(),
        }
    }

    /// One chat-completion turn. `messages` is the raw OpenAI message array; `tools` is the native
    /// OpenAI tools array (empty for a plain completion). The configured model pin (or `None` for
    /// router choice) is applied here.
    pub async fn chat(&self, messages: &[Value], tools: &[Value]) -> anyhow::Result<Turn> {
        let mut body = serde_json::json!({ "messages": messages });
        if let Some(m) = &self.cfg.model {
            body["model"] = Value::String(m.clone());
        }
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.to_vec());
            body["tool_choice"] = Value::String("auto".into());
        }
        // Never let the optimizer cache an agent turn: a cached completion would replay a stale tool
        // call and break the loop. The edge auto-skips caching when `tools` are present; we set
        // `no_cache` explicitly too, so this holds even against a gateway that doesn't auto-skip.
        body["no_cache"] = Value::Bool(true);

        let mut req = self
            .http
            .post(format!(
                "{}/chat/completions",
                self.cfg.base_url.trim_end_matches('/')
            ))
            .bearer_auth(&self.cfg.optimizer_key)
            .json(&body);
        if let Some(pk) = &self.cfg.provider_key {
            req = req.header("x-provider-key", pk);
        }

        let resp = req.send().await?;
        let status = resp.status();
        let json: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("optimizer edge returned {status}: {json}");
        }
        Self::parse_turn(json)
    }

    fn parse_turn(json: Value) -> anyhow::Result<Turn> {
        let choice = json
            .get("choices")
            .and_then(|c| c.get(0))
            .ok_or_else(|| anyhow::anyhow!("no choices in response"))?;
        let msg = choice.get("message").unwrap_or(choice);
        let content = msg
            .get("content")
            .and_then(|c| c.as_str())
            .map(str::to_string);
        let tool_calls: Vec<ToolCall> = msg
            .get("tool_calls")
            .cloned()
            .map(|tc| serde_json::from_value(tc).unwrap_or_default())
            .unwrap_or_default();
        let model_served = json
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
            .to_string();
        Ok(Turn {
            content,
            tool_calls,
            model_served,
            raw: json,
        })
    }
}
