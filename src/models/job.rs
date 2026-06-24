use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::JsonValue;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, sqlx::Type, Clone, PartialEq)]
#[sqlx(type_name = "job_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Draft,     // company filling out job details
    Reviewing, // posted, accepting applications, company reviewing applicants
    Open,      // applicant selected; awaiting trial start
    Trial,     // in the 1-week trial sprint
    Active,    // agent running autonomously
    Completed, // contract ended
    Cancelled,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Job {
    pub id: Uuid,
    pub company_id: Uuid,
    /// None until an applicant is selected from the job board.
    pub agent_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub status: JobStatus,
    pub hourly_rate: i32, // cents
    pub profession_id: Option<Uuid>,
    pub requirements: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, sqlx::Type, Clone, PartialEq)]
#[sqlx(type_name = "tool_type", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ToolType {
    Github,
    Slack,
    Jira,
    Linear,
    GoogleDrive,
    Notion,
    Email,
    CustomApi,
    /// Company-declared test-env strategy for the Sprite backend (see sprite_env.rs).
    #[sqlx(rename = "sprite_env")]
    #[serde(rename = "sprite_env")]
    SpriteEnv,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct JobIntegration {
    pub id: Uuid,
    pub job_id: Uuid,
    pub tool_type: ToolType,
    /// Owner-side Connection backing this grant (github/google). Null for raw-cred grants.
    pub connection_id: Option<Uuid>,
    /// AES-GCM encrypted JSON blob. Null for connection-backed grants. Never serialized.
    #[serde(skip_serializing)]
    #[allow(dead_code)]
    // DB column; read only on the raw-cred decrypt path, which isn't wired yet
    pub encrypted_credentials: Option<String>,
    pub scope_config: JsonValue, // what the agent can/cannot access
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateJobRequest {
    pub title: String,
    pub description: String,
    pub hourly_rate: i32,
    pub profession_id: Option<Uuid>,
    #[serde(default)]
    pub requirements: String,
}
