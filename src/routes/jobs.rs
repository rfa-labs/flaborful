use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    auth_extractor::AuthUser,
    errors::{AppError, AppResult},
    models::application::{ApplicantView, CreateApplicationRequest, JobApplication},
    models::job::{CreateJobRequest, Job, JobIntegration, ToolType},
    services::{applications, authz, connections, contracts, jobs as svc},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create_job))
        .route("/open", get(list_open_jobs))
        .route("/:id", get(get_job))
        .route("/:id/open", put(open_job))
        .route("/:id/trial", put(start_trial))
        .route("/:id/activate", put(activate_job))
        .route("/:id/complete", put(complete_job))
        .route("/:id/kill", put(kill_switch))
        // Job board: applications
        .route("/:id/apply", post(apply).delete(withdraw))
        .route("/:id/applicants", get(list_applicants))
        .route(
            "/:id/integrations",
            post(add_integration).get(list_integrations),
        )
        .route(
            "/:id/integrations/:integration_id",
            delete(delete_integration),
        )
        .route("/:id/agent-connections", get(list_agent_connections))
        .route("/:id/contract", post(generate_contract).get(get_contract))
        .route("/company/:company_id", get(list_for_company))
        .route("/agent/:agent_id", get(list_for_agent))
}

#[derive(Deserialize)]
struct CreateJobBody {
    // company_id may be present for backward-compat but is ignored — the job is always
    // posted as the authenticated caller's own company.
    #[allow(dead_code)]
    company_id: Option<Uuid>,
    #[serde(flatten)]
    req: CreateJobRequest,
}

async fn create_job(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<CreateJobBody>,
) -> AppResult<(StatusCode, Json<Job>)> {
    let company_id = authz::require_company(&state.db, auth.user_id).await?;
    let job = svc::create_job(&state.db, company_id, body.req).await?;
    Ok((StatusCode::CREATED, Json(job)))
}

async fn get_job(State(state): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Job>> {
    Ok(Json(svc::get_job(&state.db, id).await?))
}

#[derive(Deserialize)]
struct OpenJobsQuery {
    profession_id: Option<Uuid>,
}

/// GET /jobs/open?profession_id=<uuid> — the public job board of postings accepting
/// applications.
async fn list_open_jobs(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<OpenJobsQuery>,
) -> AppResult<Json<Vec<Job>>> {
    Ok(Json(svc::list_open_jobs(&state.db, q.profession_id).await?))
}

// ── Applications ──────────────────────────────────────────────────────────────

/// POST /jobs/:id/apply — the signed-in professional's agent applies to a job.
async fn apply(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(job_id): Path<Uuid>,
    Json(req): Json<CreateApplicationRequest>,
) -> AppResult<(StatusCode, Json<JobApplication>)> {
    let application =
        applications::apply(&state.db, &state.qdrant, auth.user_id, job_id, req).await?;
    Ok((StatusCode::CREATED, Json(application)))
}

/// DELETE /jobs/:id/apply — withdraw the signed-in professional's application.
async fn withdraw(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(job_id): Path<Uuid>,
) -> AppResult<StatusCode> {
    applications::withdraw(&state.db, auth.user_id, job_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /jobs/:id/applicants — the company's applicant pool for a job.
async fn list_applicants(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(job_id): Path<Uuid>,
) -> AppResult<Json<Vec<ApplicantView>>> {
    authz::require_job_owner(&state.db, auth.user_id, job_id).await?;
    Ok(Json(
        applications::list_applicants(&state.db, &state.qdrant, job_id).await?,
    ))
}

async fn open_job(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Job>> {
    authz::require_job_owner(&state.db, auth.user_id, id).await?;
    Ok(Json(svc::open_job(&state.db, id).await?))
}

async fn start_trial(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Job>> {
    authz::require_job_owner(&state.db, auth.user_id, id).await?;
    Ok(Json(svc::start_trial(&state.db, id).await?))
}

async fn activate_job(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Job>> {
    authz::require_job_owner(&state.db, auth.user_id, id).await?;
    Ok(Json(svc::activate_job(&state.db, id).await?))
}

async fn complete_job(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Job>> {
    authz::require_job_owner(&state.db, auth.user_id, id).await?;
    let job = svc::complete_job(&state.db, id).await?;
    crate::services::execution::sprite::destroy_job_sprites(
        &state.db,
        &state.backends.sprite_client,
        id,
    )
    .await;
    Ok(Json(job))
}

async fn kill_switch(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    authz::require_job_owner(&state.db, auth.user_id, id).await?;
    svc::revoke_all_integrations(&state.db, id).await?;
    // Kill switch MUST tear down running sprites immediately (they may hold creds).
    crate::services::execution::sprite::destroy_job_sprites(
        &state.db,
        &state.backends.sprite_client,
        id,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AddIntegrationBody {
    tool_type: ToolType,
    /// For github/google grants: the owner's Connection to link.
    connection_id: Option<Uuid>,
    /// For raw-credential grants (e.g. Slack bot token).
    credentials: Option<Value>,
    scope_config: Option<Value>,
}

async fn add_integration(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(job_id): Path<Uuid>,
    Json(body): Json<AddIntegrationBody>,
) -> AppResult<(StatusCode, Json<JobIntegration>)> {
    authz::require_job_owner(&state.db, auth.user_id, job_id).await?;
    let integration = svc::add_integration(
        &state.db,
        job_id,
        svc::AddIntegrationRequest {
            tool_type: body.tool_type,
            connection_id: body.connection_id,
            credentials: body.credentials,
            scope_config: body.scope_config.unwrap_or(serde_json::json!({})),
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(integration)))
}

/// GET /jobs/:id/agent-connections
/// Lists the providers the agent's owner has connected, so the company can grant the
/// job access to one. Resolves the agent via the job.
async fn list_agent_connections(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(job_id): Path<Uuid>,
) -> AppResult<Json<Vec<connections::ConnectionSummary>>> {
    authz::require_job_owner(&state.db, auth.user_id, job_id).await?;
    let job = svc::get_job(&state.db, job_id).await?;
    // No agent until an applicant is selected — nothing to grant yet.
    let Some(agent_id) = job.agent_id else {
        return Ok(Json(vec![]));
    };
    let summaries = connections::list_for_agent_owner(&state.db, agent_id)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(summaries))
}

async fn list_integrations(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(job_id): Path<Uuid>,
) -> AppResult<Json<Vec<JobIntegration>>> {
    authz::require_job_owner(&state.db, auth.user_id, job_id).await?;
    Ok(Json(svc::list_integrations(&state.db, job_id).await?))
}

async fn generate_contract(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(job_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_job_owner(&state.db, auth.user_id, job_id).await?;
    let contract_md = contracts::generate_contract(&state.db, &state.inference, job_id)
        .await
        .map_err(AppError::Internal)?;

    contracts::save_contract(&state.db, job_id, &contract_md)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(serde_json::json!({ "contract": contract_md })))
}

async fn get_contract(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let row = sqlx::query!("SELECT contracts FROM jobs WHERE id = $1", job_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Job {job_id} not found")))?;

    Ok(Json(serde_json::json!({ "contracts": row.contracts })))
}

async fn delete_integration(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((job_id, integration_id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    authz::require_job_owner(&state.db, auth.user_id, job_id).await?;
    sqlx::query!(
        "UPDATE job_integrations SET is_active = false WHERE id = $1 AND job_id = $2",
        integration_id,
        job_id
    )
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_for_company(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(company_id): Path<Uuid>,
) -> AppResult<Json<Vec<Job>>> {
    // A company may only list its own jobs.
    let own = authz::require_company(&state.db, auth.user_id).await?;
    if own != company_id {
        return Err(AppError::Forbidden("Not your company".into()));
    }
    Ok(Json(svc::list_for_company(&state.db, company_id).await?))
}

async fn list_for_agent(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<Vec<Job>>> {
    // Only the agent's owner may list its jobs.
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    Ok(Json(svc::list_for_agent(&state.db, agent_id).await?))
}
