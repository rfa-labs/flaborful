use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    errors::{AppError, AppResult},
    services::notifications::{self, Notification},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/:user_id", get(list_notifications))
        .route("/:id/read", put(mark_read))
        .route("/user/:user_id/read-all", put(mark_all_read))
}

#[derive(Deserialize)]
struct ListQuery {
    unread_only: Option<bool>,
}

async fn list_notifications(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<Notification>>> {
    let notifs = notifications::list_for_user(&state.db, user_id, q.unread_only.unwrap_or(false))
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(notifs))
}

async fn mark_read(
    State(state): State<AppState>,
    Path(notification_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    // TODO: extract user_id from JWT — using notification_id as placeholder for now
    notifications::mark_read(&state.db, notification_id, notification_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mark_all_read(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    notifications::mark_all_read(&state.db, user_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}
