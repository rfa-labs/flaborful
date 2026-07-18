//! The reflection review queue: list pending proposals, apply or dismiss one, and trigger a
//! reflection on demand. Owner-scoped to the agent.

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use uuid::Uuid;

use crate::services::{authz, reflections};
use crate::{
    auth_extractor::AuthUser,
    errors::{AppError, AppResult},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/agent/:agent_id", get(list).post(reflect_now))
        .route("/agent/:agent_id/:proposal_id/apply", post(apply))
        .route("/agent/:agent_id/:proposal_id/dismiss", post(dismiss))
}

async fn list(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<Vec<reflections::ProposalView>>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let rows = reflections::list_proposals(&state.db, agent_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(rows))
}

/// Trigger a reflection immediately (rather than waiting for the scheduler).
async fn reflect_now(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let count = reflections::run_for_agent(&state.db, &state.inference, agent_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "proposals": count })))
}

async fn apply(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((agent_id, proposal_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let outcome =
        reflections::apply_proposal(&state.db, &state.qdrant, &state.inference, agent_id, proposal_id)
            .await
            .map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "ok": true, "outcome": outcome })))
}

async fn dismiss(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((agent_id, proposal_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    reflections::dismiss_proposal(&state.db, agent_id, proposal_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
