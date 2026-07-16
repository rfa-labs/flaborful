use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth_extractor::AuthUser,
    errors::{AppError, AppResult},
    services::notifications::{self, Notification},
    AppState,
};

/// Notifications belong directly to a user, so the JWT *is* the authorization: every handler
/// derives the user from `AuthUser` rather than trusting a path param. The previous
/// `/user/:user_id` routes were unauthenticated, letting any caller read or mutate another
/// user's notifications by guessing a UUID.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_notifications))
        .route("/:id/read", put(mark_read))
        .route("/read-all", put(mark_all_read))
}

#[derive(Deserialize)]
struct ListQuery {
    unread_only: Option<bool>,
}

async fn list_notifications(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<Notification>>> {
    let notifs =
        notifications::list_for_user(&state.db, auth.user_id, q.unread_only.unwrap_or(false))
            .await
            .map_err(AppError::Internal)?;
    Ok(Json(notifs))
}

async fn mark_read(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(notification_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    // `mark_read` filters on `id = $1 AND user_id = $2`, so passing the caller's real id scopes
    // the update to their own notification. This previously passed the *notification* id as the
    // user id, which made the ownership half of that predicate meaningless.
    notifications::mark_read(&state.db, notification_id, auth.user_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mark_all_read(State(state): State<AppState>, auth: AuthUser) -> AppResult<StatusCode> {
    notifications::mark_all_read(&state.db, auth.user_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}
