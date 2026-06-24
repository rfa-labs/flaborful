use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, sqlx::Type, Clone)]
#[sqlx(type_name = "notification_type", rename_all = "snake_case")]
pub enum NotificationType {
    TaskAwaitingReview,
    TaskDelivered,
    TaskFailed,
    JobStarted,
    JobCompleted,
    AgentTrainingDone,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: Uuid,
    #[sqlx(rename = "type")]
    pub notification_type: NotificationType,
    pub title: String,
    pub body: String,
    pub payload: Value,
    pub read: bool,
    pub created_at: DateTime<Utc>,
}

pub async fn create(
    db: &PgPool,
    user_id: Uuid,
    notification_type: NotificationType,
    title: String,
    body: String,
    payload: Value,
) -> Result<Notification> {
    let row = sqlx::query_as::<_, Notification>(
        r#"INSERT INTO notifications (id, user_id, type, title, body, payload, created_at)
           VALUES ($1, $2, $3, $4, $5, $6, NOW())
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(notification_type)
    .bind(title)
    .bind(body)
    .bind(payload)
    .fetch_one(db)
    .await?;
    Ok(row)
}

pub async fn list_for_user(
    db: &PgPool,
    user_id: Uuid,
    unread_only: bool,
) -> Result<Vec<Notification>> {
    let rows = if unread_only {
        sqlx::query_as::<_, Notification>(
            "SELECT * FROM notifications WHERE user_id = $1 AND read = false ORDER BY created_at DESC LIMIT 50"
        )
        .bind(user_id)
        .fetch_all(db)
        .await?
    } else {
        sqlx::query_as::<_, Notification>(
            "SELECT * FROM notifications WHERE user_id = $1 ORDER BY created_at DESC LIMIT 100",
        )
        .bind(user_id)
        .fetch_all(db)
        .await?
    };
    Ok(rows)
}

pub async fn mark_read(db: &PgPool, notification_id: Uuid, user_id: Uuid) -> Result<()> {
    sqlx::query!(
        "UPDATE notifications SET read = true WHERE id = $1 AND user_id = $2",
        notification_id,
        user_id
    )
    .execute(db)
    .await?;
    Ok(())
}

pub async fn mark_all_read(db: &PgPool, user_id: Uuid) -> Result<()> {
    sqlx::query!(
        "UPDATE notifications SET read = true WHERE user_id = $1",
        user_id
    )
    .execute(db)
    .await?;
    Ok(())
}

// ── Convenience constructors for common events ────────────────────────────────

/// Notify the professional that a task is waiting for their review.
pub async fn notify_awaiting_review(
    db: &PgPool,
    professional_user_id: Uuid,
    task_id: Uuid,
    job_title: &str,
) -> Result<()> {
    create(
        db,
        professional_user_id,
        NotificationType::TaskAwaitingReview,
        "Task needs your review".to_string(),
        format!(
            "An agent output for \"{}\" is ready for your approval.",
            job_title
        ),
        serde_json::json!({ "task_id": task_id }),
    )
    .await?;
    Ok(())
}

/// Notify the company that a task output has been delivered.
pub async fn notify_task_delivered(
    db: &PgPool,
    company_user_id: Uuid,
    task_id: Uuid,
    job_title: &str,
) -> Result<()> {
    create(
        db,
        company_user_id,
        NotificationType::TaskDelivered,
        "Task delivered".to_string(),
        format!("A completed output for \"{}\" is ready.", job_title),
        serde_json::json!({ "task_id": task_id }),
    )
    .await?;
    Ok(())
}

#[allow(dead_code)] // notification kind not yet emitted from the failure path
pub async fn notify_task_failed(
    db: &PgPool,
    professional_user_id: Uuid,
    task_id: Uuid,
    error: &str,
) -> Result<()> {
    create(
        db,
        professional_user_id,
        NotificationType::TaskFailed,
        "Task execution failed".to_string(),
        format!("A task failed and needs attention: {}", error),
        serde_json::json!({ "task_id": task_id, "error": error }),
    )
    .await?;
    Ok(())
}

pub async fn notify_training_done(
    db: &PgPool,
    professional_user_id: Uuid,
    agent_id: Uuid,
) -> Result<()> {
    create(
        db,
        professional_user_id,
        NotificationType::AgentTrainingDone,
        "Your agent is ready".to_string(),
        "LoRA adapter training is complete. Your agent is now live on the marketplace.".to_string(),
        serde_json::json!({ "agent_id": agent_id }),
    )
    .await?;
    Ok(())
}

/// Called after LoRA training completes. Looks up the professional's user_id
/// from the agent row and sends an "agent is ready" notification.
pub async fn notify_agent_ready(db: &PgPool, agent_id: Uuid) -> Result<()> {
    let user_id = sqlx::query_scalar::<_, Uuid>(
        r#"SELECT u.id FROM users u
           JOIN professionals p ON p.user_id = u.id
           JOIN agents a ON a.professional_id = p.id
           WHERE a.id = $1"#,
    )
    .bind(agent_id)
    .fetch_optional(db)
    .await?;

    if let Some(uid) = user_id {
        notify_training_done(db, uid, agent_id).await?;
    }
    Ok(())
}
