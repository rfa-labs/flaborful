use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use uuid::Uuid;

use crate::{
    errors::{AppError, AppResult},
    services::marketplace::{self, MarketplaceFilters},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(browse))
        .route("/:agent_id", get(get_listing))
}

async fn browse(
    State(state): State<AppState>,
    Query(filters): Query<MarketplaceFilters>,
) -> AppResult<Json<serde_json::Value>> {
    let listings = marketplace::browse(&state.db, filters)
        .await
        .map_err(AppError::Internal)?;

    let count = listings.len();
    Ok(Json(
        serde_json::json!({ "agents": listings, "count": count }),
    ))
}

async fn get_listing(
    State(state): State<AppState>,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let listing = marketplace::get_listing(&state.db, agent_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound(format!("Agent {agent_id} not listed")))?;

    Ok(Json(serde_json::json!(listing)))
}
