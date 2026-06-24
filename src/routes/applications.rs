use axum::{
    extract::{Path, State},
    routing::{get, post, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth_extractor::AuthUser,
    errors::AppResult,
    models::application::{ApplicationStatus, JobApplication},
    models::job::Job,
    services::{applications, authz},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/:id/status", put(set_status))
        .route("/:id/meeting", put(set_meeting))
        .route("/:id/select", post(select))
        .route("/:id/interview-note", post(add_interview_note))
        .route("/:id/vault-graph", get(vault_graph))
}

/// GET /applications/:id/vault-graph — the IP-safe vault graph for the applicant the company is
/// reviewing. Full graph structure; shared (snapshot + still-shareable) notes carry title/body,
/// every other note is an anonymized "Private note #n" placeholder. Company-scoped.
async fn vault_graph(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<crate::services::vault::VaultGraph>> {
    authz::require_application_company(&state.db, auth.user_id, id).await?;
    let app = sqlx::query_as::<_, JobApplication>("SELECT * FROM job_applications WHERE id = $1")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    let graph =
        crate::services::vault::application_graph(&state.db, app.agent_id, &app.shared_chunk_ids)
            .await
            .map_err(crate::errors::AppError::Internal)?;
    Ok(Json(graph))
}

#[derive(Deserialize)]
struct SetStatusBody {
    status: ApplicationStatus,
}

/// PUT /applications/:id/status — company shortlists / rejects / etc.
async fn set_status(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SetStatusBody>,
) -> AppResult<Json<JobApplication>> {
    authz::require_application_company(&state.db, auth.user_id, id).await?;
    Ok(Json(
        applications::set_status(&state.db, id, body.status).await?,
    ))
}

#[derive(Deserialize)]
struct SetMeetingBody {
    meeting_link: Option<String>,
    meeting_at: Option<DateTime<Utc>>,
}

/// PUT /applications/:id/meeting — company sets an interview link + time (paste a Zoom/Meet URL).
async fn set_meeting(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SetMeetingBody>,
) -> AppResult<Json<JobApplication>> {
    authz::require_application_company(&state.db, auth.user_id, id).await?;
    Ok(Json(
        applications::set_meeting(&state.db, id, body.meeting_link, body.meeting_at).await?,
    ))
}

/// POST /applications/:id/select — company selects this applicant; wires their agent into
/// the job and ingests the application materials into the agent's KB.
async fn select(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Job>> {
    authz::require_application_company(&state.db, auth.user_id, id).await?;
    Ok(Json(
        applications::select_applicant(&state.db, &state.qdrant, &state.inference, id).await?,
    ))
}

#[derive(Deserialize)]
struct InterviewNoteBody {
    note: String,
}

/// POST /applications/:id/interview-note — capture interview notes/transcript into the
/// applicant's agent KB (feeds future retraining). The agent + job are derived from the
/// application, not the request body, so notes can't be written to an unrelated agent.
async fn add_interview_note(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<InterviewNoteBody>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_application_company(&state.db, auth.user_id, id).await?;
    let app = sqlx::query!(
        "SELECT agent_id, job_id FROM job_applications WHERE id = $1",
        id
    )
    .fetch_one(&state.db)
    .await?;

    applications::ingest_interview_note(
        &state.db,
        &state.qdrant,
        &state.inference,
        app.agent_id,
        app.job_id,
        &body.note,
    )
    .await?;
    Ok(Json(serde_json::json!({ "status": "ingested" })))
}
