/// Server-Sent Events endpoint for real-time task and notification feeds.
///
/// GET /events/tasks/:job_id   — streams task status updates for a job
/// GET /events/notifications/:user_id — streams new notifications for a user
///
/// Both endpoints poll the DB every 2 seconds and push changes as SSE events.
/// In a production system you'd replace this with LISTEN/NOTIFY or a message bus,
/// but polling is correct and simple for the MVP.
use axum::{
    extract::{Path, State},
    response::{sse::Event, Sse},
    routing::get,
    Router,
};
use futures::stream::Stream;
use std::{convert::Infallible, time::Duration};
use uuid::Uuid;

use crate::{auth_extractor::AuthUser, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/tasks/:job_id", get(task_feed))
        .route("/notifications/:user_id", get(notification_feed))
        .route("/node-executions/:task_id", get(node_execution_feed))
        .route("/ingestion/:agent_id", get(ingestion_feed))
}

/// Stream agentic code-ingestion progress for an agent (drives the live onboarding feed).
/// Each event: `data: {"repo":"...","kind":"status|note|message","status":"running|done|failed|complete","detail":"...","notes_count":N}`
/// A terminal `kind="status", status="complete", repo=""` event marks the whole run done.
async fn ingestion_feed(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        let mut last_seen: Option<chrono::DateTime<chrono::Utc>> = None;

        loop {
            let rows = match sqlx::query!(
                r#"SELECT repo, kind, status, detail, notes_count, created_at
                   FROM ingestion_events
                   WHERE agent_id = $1
                     AND ($2::timestamptz IS NULL OR created_at > $2)
                   ORDER BY created_at ASC"#,
                agent_id,
                last_seen,
            )
            .fetch_all(&state.db)
            .await
            {
                Ok(rows) => rows,
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };

            for row in &rows {
                let data = serde_json::json!({
                    "repo": row.repo,
                    "kind": row.kind,
                    "status": row.status,
                    "detail": row.detail,
                    "notes_count": row.notes_count,
                });
                yield Ok(Event::default().event("ingestion").data(data.to_string()));
                last_seen = Some(row.created_at);
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// Stream node-execution status changes for a task (drives live DAG progress).
/// Each event: `data: {"node_id":"...","status":"...","attempt":1,...}`
async fn node_execution_feed(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(task_id): Path<Uuid>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        // Track the last (started_at) cursor and also re-emit on status flips by polling
        // recently-updated rows. We key on the greater of started_at/finished_at.
        let mut last_seen: Option<chrono::DateTime<chrono::Utc>> = None;

        loop {
            let rows = match sqlx::query!(
                r#"SELECT node_id, node_type, status, attempt,
                          GREATEST(started_at, COALESCE(finished_at, started_at)) AS "cursor!"
                   FROM node_executions
                   WHERE task_id = $1
                     AND ($2::timestamptz IS NULL OR GREATEST(started_at, COALESCE(finished_at, started_at)) > $2)
                   ORDER BY "cursor!" ASC"#,
                task_id,
                last_seen,
            )
            .fetch_all(&state.db)
            .await
            {
                Ok(rows) => rows,
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };

            for row in &rows {
                let data = serde_json::json!({
                    "node_id": row.node_id,
                    "node_type": row.node_type,
                    "status": row.status,
                    "attempt": row.attempt,
                });
                yield Ok(Event::default().event("node_update").data(data.to_string()));
                last_seen = Some(row.cursor);
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// Stream task status changes for a job.
/// Each event: `data: {"id":"...","status":"...","confidence_score":0.9,...}`
async fn task_feed(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(job_id): Path<Uuid>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        let mut last_seen: Option<chrono::DateTime<chrono::Utc>> = None;

        loop {
            let rows = match sqlx::query!(
                r#"SELECT id, status::text AS "status!", confidence_score, output_approved, delivered_at, updated_at
                   FROM tasks
                   WHERE job_id = $1
                     AND ($2::timestamptz IS NULL OR updated_at > $2)
                   ORDER BY updated_at ASC"#,
                job_id,
                last_seen,
            )
            .fetch_all(&state.db)
            .await
            {
                Ok(rows) => rows,
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };

            for row in &rows {
                let data = serde_json::json!({
                    "id": row.id,
                    "status": row.status,
                    "confidence_score": row.confidence_score,
                    "has_output": row.output_approved.is_some(),
                    "delivered_at": row.delivered_at,
                });
                yield Ok(Event::default().event("task_update").data(data.to_string()));
                last_seen = Some(row.updated_at);
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// Stream new notifications for a user.
/// Each event: `data: {"id":"...","title":"...","body":"...","notification_type":"..."}`
async fn notification_feed(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(user_id): Path<Uuid>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        let mut last_seen_id: Option<chrono::DateTime<chrono::Utc>> = None;

        loop {
            let rows = match sqlx::query!(
                r#"SELECT id, type::text AS "notification_type!", title, body, created_at
                   FROM notifications
                   WHERE user_id = $1
                     AND ($2::timestamptz IS NULL OR created_at > $2)
                   ORDER BY created_at ASC"#,
                user_id,
                last_seen_id,
            )
            .fetch_all(&state.db)
            .await
            {
                Ok(rows) => rows,
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };

            for row in &rows {
                let data = serde_json::json!({
                    "id": row.id,
                    "type": row.notification_type,
                    "title": row.title,
                    "body": row.body,
                });
                yield Ok(Event::default().event("notification").data(data.to_string()));
                last_seen_id = Some(row.created_at);
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}
