use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use uuid::Uuid;

use crate::{
    errors::{AppError, AppResult},
    services::audit::{self, AuditEvent},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new().route("/job/:job_id", get(list_for_job))
}

async fn list_for_job(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> AppResult<Json<Vec<AuditEvent>>> {
    let events = audit::list_for_job(&state.db, job_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(events))
}
