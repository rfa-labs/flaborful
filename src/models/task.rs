use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::JsonValue;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, sqlx::Type, Clone, PartialEq)]
#[sqlx(type_name = "task_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,        // received, not yet started
    Running,        // workflow executing
    AwaitingReview, // at a human gate, paused
    AwaitingEffect, // rerun paused at a side-effecting node for an owner decision
    Approved,       // professional approved output
    Amended,        // professional edited output before approving
    Rejected,       // professional rejected — sent back for rework
    Delivered,      // output sent to company
    Failed,         // a node errored / the run crashed
    Cancelled,      // the owner killed the execution from the gate (deliberate, not a crash)
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Task {
    pub id: Uuid,
    pub job_id: Uuid,
    pub workflow_id: Option<Uuid>,
    pub input: JsonValue,
    /// Structured work-target (repo, base_branch, …) injected into ctx.vars at dispatch.
    #[serde(default)]
    pub target: JsonValue,
    pub status: TaskStatus,
    pub confidence_score: Option<f32>,
    pub output_draft: Option<String>,
    pub output_approved: Option<String>,
    pub current_node_id: Option<String>, // which workflow node we're paused at
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
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
