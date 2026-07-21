use crate::errors::{AppError, AppResult};
use crate::models::node_execution::NodeExecution;
use crate::models::task::{Task, TaskStatus};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn get_task(db: &PgPool, id: Uuid) -> AppResult<Task> {
    sqlx::query_as::<_, Task>("SELECT * FROM tasks WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Task {id} not found")))
}

pub async fn list_for_job(db: &PgPool, job_id: Uuid) -> AppResult<Vec<Task>> {
    let rows =
        sqlx::query_as::<_, Task>("SELECT * FROM tasks WHERE job_id = $1 ORDER BY created_at DESC")
            .bind(job_id)
            .fetch_all(db)
            .await?;
    Ok(rows)
}

pub async fn review_queue(db: &PgPool, job_id: Uuid) -> AppResult<Vec<Task>> {
    let rows = sqlx::query_as::<_, Task>(
        "SELECT * FROM tasks WHERE job_id = $1 AND status = $2 ORDER BY created_at ASC",
    )
    .bind(job_id)
    .bind(TaskStatus::AwaitingReview)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Active task runs for an agent (owner-side execution monitoring): everything not yet
/// terminal. Joined task→job→agent.
/// A run-log row: one task (execution) with its job title, node count, and timing — for the
/// cross-run history browser. Includes ALL runs (delivered/failed/in-flight), newest first.
#[derive(serde::Serialize, sqlx::FromRow)]
pub struct RunSummary {
    pub task_id: Uuid,
    pub job_title: String,
    pub status: String,
    pub node_count: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub delivered_at: Option<chrono::DateTime<chrono::Utc>>,
    pub grade: Option<String>,
}

pub async fn list_runs_for_agent(
    db: &PgPool,
    agent_id: Uuid,
    limit: i64,
) -> AppResult<Vec<RunSummary>> {
    let rows = sqlx::query_as::<_, RunSummary>(
        r#"SELECT t.id AS task_id, j.title AS job_title, t.status::text AS status,
                  (SELECT COUNT(DISTINCT node_id) FROM node_executions ne WHERE ne.task_id = t.id) AS node_count,
                  t.created_at, t.delivered_at,
                  e.grade AS grade
           FROM tasks t
           JOIN jobs j ON j.id = t.job_id
           LEFT JOIN task_evaluations e ON e.task_id = t.id
           WHERE j.agent_id = $1
           ORDER BY t.created_at DESC
           LIMIT $2"#,
    )
    .bind(agent_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub async fn list_active_for_agent(db: &PgPool, agent_id: Uuid) -> AppResult<Vec<Task>> {
    // Include `failed` too: a failed run is not the end of the company's task — it stays visible
    // so the professional can see the error and re-run it (the "Run again" card). Only truly
    // terminal `delivered` tasks drop off this active list.
    let rows = sqlx::query_as::<_, Task>(
        r#"SELECT t.* FROM tasks t
           JOIN jobs j ON j.id = t.job_id
           WHERE j.agent_id = $1
             AND t.status IN ('pending', 'running', 'awaiting_review', 'awaiting_effect', 'failed')
           ORDER BY t.created_at DESC"#,
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// All persisted node executions for a task, ordered by start time (drives the DAG viz).
pub async fn node_executions_for_task(db: &PgPool, task_id: Uuid) -> AppResult<Vec<NodeExecution>> {
    let rows = sqlx::query_as::<_, NodeExecution>(
        "SELECT * FROM node_executions WHERE task_id = $1 ORDER BY started_at ASC, attempt ASC",
    )
    .bind(task_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Resolve the agent that owns a task (for authorization). None if not found.
pub async fn task_agent_id(db: &PgPool, task_id: Uuid) -> AppResult<Option<Uuid>> {
    let row = sqlx::query_scalar!(
        "SELECT j.agent_id FROM tasks t JOIN jobs j ON j.id = t.job_id WHERE t.id = $1",
        task_id
    )
    .fetch_optional(db)
    .await?;
    Ok(row.flatten())
}
