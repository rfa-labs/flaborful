use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct AuditEvent {
    pub id: Uuid,
    pub job_id: Uuid,
    pub actor_type: String,
    pub actor_id: Option<Uuid>,
    pub event_type: String,
    pub summary: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

pub async fn log(
    db: &PgPool,
    job_id: Uuid,
    actor_type: &str,
    actor_id: Option<Uuid>,
    event_type: &str,
    summary: &str,
    payload: Value,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO audit_events (id, job_id, actor_type, actor_id, event_type, summary, payload, created_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())"#,
        Uuid::new_v4(),
        job_id,
        actor_type,
        actor_id,
        event_type,
        summary,
        payload
    )
    .execute(db)
    .await?;
    Ok(())
}

pub async fn list_for_job(db: &PgPool, job_id: Uuid) -> Result<Vec<AuditEvent>> {
    let rows = sqlx::query_as::<_, AuditEvent>(
        "SELECT * FROM audit_events WHERE job_id = $1 ORDER BY created_at DESC LIMIT 500",
    )
    .bind(job_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

// ── Typed log helpers used throughout the codebase ───────────────────────────

#[allow(dead_code)] // audit-event wrapper, not yet wired at a call site
pub async fn log_task_started(
    db: &PgPool,
    job_id: Uuid,
    task_id: Uuid,
    workflow_name: &str,
) -> Result<()> {
    log(
        db,
        job_id,
        "agent",
        None,
        "task_started",
        &format!("Started workflow: {}", workflow_name),
        serde_json::json!({ "task_id": task_id }),
    )
    .await
}

pub async fn log_task_paused(
    db: &PgPool,
    job_id: Uuid,
    task_id: Uuid,
    node_id: &str,
) -> Result<()> {
    log(
        db,
        job_id,
        "agent",
        None,
        "task_paused_for_review",
        &format!("Paused at node {} — awaiting professional review", node_id),
        serde_json::json!({ "task_id": task_id, "node_id": node_id }),
    )
    .await
}

pub async fn log_task_approved(
    db: &PgPool,
    job_id: Uuid,
    task_id: Uuid,
    amended: bool,
    actor_id: Uuid,
) -> Result<()> {
    let event = if amended {
        "task_amended_and_approved"
    } else {
        "task_approved"
    };
    let summary = if amended {
        "Professional amended and approved the output"
    } else {
        "Professional approved the output"
    };
    log(
        db,
        job_id,
        "professional",
        Some(actor_id),
        event,
        summary,
        serde_json::json!({ "task_id": task_id }),
    )
    .await
}

pub async fn log_task_delivered(db: &PgPool, job_id: Uuid, task_id: Uuid) -> Result<()> {
    log(
        db,
        job_id,
        "system",
        None,
        "task_delivered",
        "Output delivered to company",
        serde_json::json!({ "task_id": task_id }),
    )
    .await
}

#[allow(dead_code)] // audit-event wrapper, not yet wired at a call site
pub async fn log_integration_added(
    db: &PgPool,
    job_id: Uuid,
    tool_type: &str,
    actor_id: Uuid,
) -> Result<()> {
    log(
        db,
        job_id,
        "company",
        Some(actor_id),
        "integration_added",
        &format!("Company connected {} integration", tool_type),
        serde_json::json!({ "tool_type": tool_type }),
    )
    .await
}

#[allow(dead_code)] // audit-event wrapper, not yet wired at a call site
pub async fn log_kill_switch(db: &PgPool, job_id: Uuid, actor_id: Uuid) -> Result<()> {
    log(
        db,
        job_id,
        "company",
        Some(actor_id),
        "kill_switch_activated",
        "Company revoked all agent integrations immediately",
        serde_json::json!({}),
    )
    .await
}

#[allow(dead_code)] // audit-event wrapper, not yet wired at a call site
pub async fn log_job_transition(
    db: &PgPool,
    job_id: Uuid,
    from: &str,
    to: &str,
    actor_id: Option<Uuid>,
) -> Result<()> {
    log(
        db,
        job_id,
        "system",
        actor_id,
        "job_status_changed",
        &format!("Job status: {} → {}", from, to),
        serde_json::json!({ "from": from, "to": to }),
    )
    .await
}
