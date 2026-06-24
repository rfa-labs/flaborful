use crate::services::professions as svc;
use crate::{
    errors::AppResult,
    models::profession::{Aspect, Profession},
    AppState,
};
use axum::{extract::State, routing::get, Json, Router};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_professions))
        .route("/:id/aspects", get(list_aspects))
}

async fn list_professions(State(state): State<AppState>) -> AppResult<Json<Vec<Profession>>> {
    let professions = svc::list_professions(&state.db).await?;
    Ok(Json(professions))
}

async fn list_aspects(
    State(state): State<AppState>,
    axum::extract::Path(profession_id): axum::extract::Path<uuid::Uuid>,
) -> AppResult<Json<Vec<Aspect>>> {
    let aspects = svc::list_aspects(&state.db, profession_id).await?;
    Ok(Json(aspects))
}
