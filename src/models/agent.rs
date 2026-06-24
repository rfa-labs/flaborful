use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::JsonValue;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, sqlx::Type, Clone, PartialEq)]
#[sqlx(type_name = "agent_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Onboarding, // still completing setup
    Training,   // LoRA adapter being generated
    Ready,      // listed on marketplace, available to hire
    Deployed,   // active on at least one job
    Suspended,  // owner paused it
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Agent {
    pub id: Uuid,
    pub professional_id: Uuid,
    pub status: AgentStatus,
    pub style_guide: Option<JsonValue>, // extracted from portfolio
    pub persona_system_prompt: Option<String>, // injected at inference time
    pub lora_adapter_path: Option<String>, // R2 path to .safetensors file
    pub lora_training_job_id: Option<String>, // RunPod job reference
    pub cf_finetune_id: Option<String>, // Cloudflare Workers AI finetune ID
    #[serde(default = "default_backend")]
    pub execution_backend: String, // managed | sprite
    #[serde(default = "default_runtime")]
    pub agent_runtime: String, // claude_code | codex | custom
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(default)]
    pub runtime_spec: Option<JsonValue>, // custom-runtime template (install/MCP/invoke)
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_backend() -> String {
    "managed".into()
}
fn default_runtime() -> String {
    "claude_code".into()
}

#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    pub professional_id: Uuid,
}

#[allow(dead_code)] // RLHF event row; persisted by training, not read in the API path yet
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct RlhfEvent {
    pub id: Uuid,
    pub task_id: Uuid,
    pub original_output: String,
    pub amended_output: String,
    pub amendment_notes: Option<String>,
    pub created_at: DateTime<Utc>,
}
