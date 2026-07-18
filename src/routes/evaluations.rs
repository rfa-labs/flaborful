//! Evaluation criteria CRUD + reading a task's evaluation grade. Owner-scoped to the agent.

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::services::authz;
use crate::{
    auth_extractor::AuthUser,
    errors::{AppError, AppResult},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/agent/:agent_id/criteria",
            get(list_criteria).post(create_criterion),
        )
        .route(
            "/agent/:agent_id/criteria/:criterion_id",
            axum::routing::delete(delete_criterion),
        )
        .route("/task/:task_id", get(get_task_evaluation))
}

#[derive(Serialize)]
struct CriterionView {
    id: String,
    name: String,
    prompt: String,
    priority: String,
}

async fn list_criteria(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<Vec<CriterionView>>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let rows = crate::services::evaluations::list_criteria(&state.db, agent_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(
        rows.into_iter()
            .map(|c| CriterionView {
                id: c.id.to_string(),
                name: c.name,
                prompt: c.prompt,
                priority: c.priority,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
struct CreateCriterion {
    name: String,
    prompt: String,
    priority: Option<String>, // "warning" (default) | "critical"
}

async fn create_criterion(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
    Json(body): Json<CreateCriterion>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let priority = match body.priority.as_deref() {
        Some("critical") => "critical",
        _ => "warning",
    };
    let id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO agent_criteria (agent_id, name, prompt, priority) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(agent_id)
    .bind(&body.name)
    .bind(&body.prompt)
    .bind(priority)
    .fetch_one(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(Json(serde_json::json!({ "id": id })))
}

async fn delete_criterion(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((agent_id, criterion_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    sqlx::query("DELETE FROM agent_criteria WHERE id = $1 AND agent_id = $2")
        .bind(criterion_id)
        .bind(agent_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize, sqlx::FromRow)]
struct EvaluationView {
    grade: String,
    criteria_results: serde_json::Value,
    tags: serde_json::Value,
    summary: Option<String>,
}

/// The evaluation for one task. The caller must own the agent behind the task.
async fn get_task_evaluation(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<Option<EvaluationView>>> {
    // Resolve the agent behind this task, then authorize.
    let agent_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT j.agent_id FROM tasks t JOIN jobs j ON j.id = t.job_id WHERE t.id = $1",
    )
    .bind(task_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?
    .ok_or_else(|| AppError::NotFound("Task not found".into()))?;
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;

    let eval = sqlx::query_as::<_, EvaluationView>(
        "SELECT grade, criteria_results, tags, summary FROM task_evaluations WHERE task_id = $1",
    )
    .bind(task_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(Json(eval))
}
