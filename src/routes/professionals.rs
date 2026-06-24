use crate::auth_extractor::AuthUser;
use crate::services::professionals as svc;
use crate::{
    errors::AppResult,
    models::professional::{CreateProfessionalRequest, Professional},
    AppState,
};
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create_professional))
        .route("/mine", get(get_my_professional))
        .route("/:id", get(get_professional))
}

async fn create_professional(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<CreateProfessionalRequest>,
) -> AppResult<Json<Professional>> {
    let p = svc::create_professional(&state.db, auth.user_id, req).await?;
    Ok(Json(p))
}

async fn get_my_professional(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<Json<Professional>> {
    let p = svc::get_professional_by_user(&state.db, auth.user_id).await?;
    Ok(Json(p))
}

async fn get_professional(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> AppResult<Json<Professional>> {
    let p = svc::get_professional(&state.db, id).await?;
    Ok(Json(p))
}
