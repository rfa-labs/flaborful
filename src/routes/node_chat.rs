use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth_extractor::AuthUser,
    errors::{AppError, AppResult},
    services::{authz, node_chat, tasks as task_svc},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/:task_id/:node_id", get(history).post(send))
        .route("/:task_id/:node_id/apply", post(apply))
}

/// Assert the caller owns the agent behind a task (chat is owner-only).
async fn require_task_owner(state: &AppState, user_id: Uuid, task_id: Uuid) -> AppResult<()> {
    let agent_id = task_svc::task_agent_id(&state.db, task_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Task {task_id} not found")))?;
    authz::require_agent_owner(&state.db, user_id, agent_id).await?;
    Ok(())
}

/// GET /node-chat/:task_id/:node_id — conversation history for a node.
async fn history(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((task_id, node_id)): Path<(Uuid, String)>,
) -> AppResult<Json<Vec<node_chat::NodeMessage>>> {
    require_task_owner(&state, auth.user_id, task_id).await?;
    let conversation_id = node_chat::start_or_get_conversation(&state.db, task_id, &node_id)
        .await
        .map_err(AppError::Internal)?;
    let msgs = node_chat::messages(&state.db, conversation_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(msgs))
}

#[derive(Deserialize)]
struct SendBody {
    message: String,
}

/// POST /node-chat/:task_id/:node_id — send a message, get the agent's reply.
async fn send(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((task_id, node_id)): Path<(Uuid, String)>,
    Json(body): Json<SendBody>,
) -> AppResult<Json<node_chat::NodeMessage>> {
    require_task_owner(&state, auth.user_id, task_id).await?;
    let reply = node_chat::send_message(
        &state.db,
        &state.qdrant,
        &state.inference,
        task_id,
        &node_id,
        &body.message,
    )
    .await
    .map_err(AppError::Internal)?;
    Ok(Json(reply))
}

#[derive(Deserialize)]
struct ApplyBody {
    corrected_output: String,
}

/// POST /node-chat/:task_id/:node_id/apply — apply a correction to the node's output.
async fn apply(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((task_id, node_id)): Path<(Uuid, String)>,
    Json(body): Json<ApplyBody>,
) -> AppResult<Json<serde_json::Value>> {
    require_task_owner(&state, auth.user_id, task_id).await?;
    node_chat::apply_correction(
        &state.db,
        &state.qdrant,
        &state.inference,
        task_id,
        &node_id,
        &body.corrected_output,
    )
    .await
    .map_err(AppError::Internal)?;
    Ok(Json(
        serde_json::json!({ "status": "applied", "dirty": true }),
    ))
}
