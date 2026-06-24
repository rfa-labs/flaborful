use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::JsonValue;
use uuid::Uuid;

/// A persisted record of one execution of one workflow node within a task run.
/// Durable per-node state powers resume, re-run, and the DAG visualization.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct NodeExecution {
    pub id: Uuid,
    pub task_id: Uuid,
    pub node_id: String,
    pub node_type: String,
    /// running | completed | failed | awaiting_review | stale
    pub status: String,
    /// The ctx.vars key this node wrote — used to replay outputs into a rebuilt context.
    pub output_key: Option<String>,
    pub input_json: Option<JsonValue>,
    pub output_json: Option<JsonValue>,
    pub logs: Option<String>,
    pub attempt: i32,
    pub dirty: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Status string constants — kept as &str to match the TEXT column without an enum
/// round-trip (the set is small and stable).
#[allow(dead_code)] // complete vocabulary of the status TEXT column; not all are referenced yet
pub mod node_status {
    pub const RUNNING: &str = "running";
    pub const COMPLETED: &str = "completed";
    pub const FAILED: &str = "failed";
    pub const AWAITING_REVIEW: &str = "awaiting_review";
    pub const AWAITING_EFFECT: &str = "awaiting_effect";
    pub const STALE: &str = "stale";
}
