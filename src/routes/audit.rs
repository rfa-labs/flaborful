use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use uuid::Uuid;

use crate::{
    auth_extractor::AuthUser,
    errors::{AppError, AppResult},
    services::audit::{self, AuditEvent},
    services::authz,
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new().route("/job/:job_id", get(list_for_job))
}

/// The audit log is the company's record of everything an agent did on their job, so it's
/// readable only by the company that posted the job. Previously unauthenticated — any caller
/// could pull a job's full audit trail by guessing its UUID.
async fn list_for_job(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(job_id): Path<Uuid>,
) -> AppResult<Json<Vec<AuditEvent>>> {
    authz::require_job_owner(&state.db, auth.user_id, job_id).await?;
    let events = audit::list_for_job(&state.db, job_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(events))
}
