use crate::services::workflows as svc;
use crate::{
    errors::AppResult,
    models::workflow::{CreateWorkflowRequest, UpdateWorkflowRequest, Workflow},
    AppState,
};
use axum::http::StatusCode;
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create_workflow))
        .route(
            "/:id",
            get(get_workflow)
                .put(update_workflow)
                .delete(delete_workflow),
        )
        .route("/agent/:agent_id", get(list_for_agent))
}

async fn create_workflow(
    State(state): State<AppState>,
    Json(req): Json<CreateWorkflowRequest>,
) -> AppResult<Json<Workflow>> {
    let wf = svc::create_workflow(&state.db, req).await?;
    Ok(Json(wf))
}

async fn get_workflow(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> AppResult<Json<Workflow>> {
    let wf = svc::get_workflow(&state.db, id).await?;
    Ok(Json(wf))
}

async fn update_workflow(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Json(req): Json<UpdateWorkflowRequest>,
) -> AppResult<Json<Workflow>> {
    let wf = svc::update_workflow(&state.db, id, req).await?;
    Ok(Json(wf))
}

async fn delete_workflow(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> AppResult<StatusCode> {
    svc::delete_workflow(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_for_agent(
    State(state): State<AppState>,
    axum::extract::Path(agent_id): axum::extract::Path<uuid::Uuid>,
) -> AppResult<Json<Vec<Workflow>>> {
    let wfs = svc::list_for_agent(&state.db, agent_id).await?;
    Ok(Json(wfs))
}
