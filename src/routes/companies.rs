use crate::auth_extractor::AuthUser;
use crate::services::companies as svc;
use crate::{
    errors::AppResult,
    models::company::{Company, CreateCompanyRequest},
    AppState,
};
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create_company))
        .route("/mine", get(get_my_company))
        .route("/:id", get(get_company))
}

async fn create_company(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<CreateCompanyRequest>,
) -> AppResult<Json<Company>> {
    let c = svc::create_company(&state.db, auth.user_id, req).await?;
    Ok(Json(c))
}

async fn get_my_company(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Company>> {
    let c = svc::get_company_by_user(&state.db, auth.user_id).await?;
    Ok(Json(c))
}

async fn get_company(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> AppResult<Json<Company>> {
    let c = svc::get_company(&state.db, id).await?;
    Ok(Json(c))
}
