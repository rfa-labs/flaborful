use crate::services::workflows as svc;
use crate::{
    auth_extractor::AuthUser,
    errors::AppResult,
    models::workflow::{CreateWorkflowRequest, UpdateWorkflowRequest, Workflow},
    services::authz,
    AppState,
};
use axum::http::StatusCode;
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use uuid::Uuid;

/// Every workflow hangs off an `agent_id`, and agents are owned by a professional, so
/// `authz::require_agent_owner` is the guard throughout. These handlers were previously
/// unauthenticated: anyone could read, overwrite, or **delete** any workflow by guessing its
/// UUID.
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

/// The by-id routes don't carry the agent in the path, so resolve the workflow first and
/// authorize against the agent it belongs to.
async fn authorize_workflow(state: &AppState, user_id: Uuid, workflow_id: Uuid) -> AppResult<Workflow> {
    let wf = svc::get_workflow(&state.db, workflow_id).await?;
    authz::require_agent_owner(&state.db, user_id, wf.agent_id).await?;
    Ok(wf)
}

async fn create_workflow(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<CreateWorkflowRequest>,
) -> AppResult<Json<Workflow>> {
    authz::require_agent_owner(&state.db, auth.user_id, req.agent_id).await?;
    let wf = svc::create_workflow(&state.db, req).await?;
    Ok(Json(wf))
}

async fn get_workflow(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Workflow>> {
    Ok(Json(authorize_workflow(&state, auth.user_id, id).await?))
}

async fn update_workflow(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateWorkflowRequest>,
) -> AppResult<Json<Workflow>> {
    authorize_workflow(&state, auth.user_id, id).await?;
    let wf = svc::update_workflow(&state.db, id, req).await?;
    Ok(Json(wf))
}

async fn delete_workflow(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    authorize_workflow(&state, auth.user_id, id).await?;
    svc::delete_workflow(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_for_agent(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<Vec<Workflow>>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let wfs = svc::list_for_agent(&state.db, agent_id).await?;
    Ok(Json(wfs))
}
