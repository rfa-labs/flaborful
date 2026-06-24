use crate::errors::{AppError, AppResult};
use crate::models::job::{CreateJobRequest, Job, JobIntegration, JobStatus, ToolType};
use crate::services::crypto;
use chrono::Utc;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Post a job to the board. Jobs start agentless and in `reviewing` (accepting
/// applications); an agent is attached later when the company selects an applicant.
pub async fn create_job(db: &PgPool, company_id: Uuid, req: CreateJobRequest) -> AppResult<Job> {
    let row = sqlx::query_as::<_, Job>(
        r#"INSERT INTO jobs (id, company_id, title, description, status, hourly_rate, profession_id, requirements, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(&req.title)
    .bind(&req.description)
    .bind(JobStatus::Reviewing)
    .bind(req.hourly_rate)
    .bind(req.profession_id)
    .bind(&req.requirements)
    .bind(Utc::now())
    .fetch_one(db)
    .await?;
    Ok(row)
}

/// List open job postings on the board, optionally filtered by profession. These are the
/// jobs professionals can apply to.
pub async fn list_open_jobs(db: &PgPool, profession_id: Option<Uuid>) -> AppResult<Vec<Job>> {
    let rows = sqlx::query_as::<_, Job>(
        r#"SELECT * FROM jobs
           WHERE status = 'reviewing'
             AND ($1::uuid IS NULL OR profession_id = $1)
           ORDER BY created_at DESC"#,
    )
    .bind(profession_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub async fn get_job(db: &PgPool, id: Uuid) -> AppResult<Job> {
    sqlx::query_as::<_, Job>("SELECT * FROM jobs WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Job {id} not found")))
}

pub async fn list_for_company(db: &PgPool, company_id: Uuid) -> AppResult<Vec<Job>> {
    let rows = sqlx::query_as::<_, Job>(
        "SELECT * FROM jobs WHERE company_id = $1 ORDER BY created_at DESC",
    )
    .bind(company_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub async fn list_for_agent(db: &PgPool, agent_id: Uuid) -> AppResult<Vec<Job>> {
    let rows =
        sqlx::query_as::<_, Job>("SELECT * FROM jobs WHERE agent_id = $1 ORDER BY created_at DESC")
            .bind(agent_id)
            .fetch_all(db)
            .await?;
    Ok(rows)
}

/// Transition a job to Open — agent becomes discoverable / hire is confirmed.
pub async fn open_job(db: &PgPool, job_id: Uuid) -> AppResult<Job> {
    transition(db, job_id, JobStatus::Open, None).await
}

/// Start the 1-week trial sprint.
pub async fn start_trial(db: &PgPool, job_id: Uuid) -> AppResult<Job> {
    transition(db, job_id, JobStatus::Trial, Some(Utc::now())).await
}

/// Activate the agent — takes over autonomously after trial.
pub async fn activate_job(db: &PgPool, job_id: Uuid) -> AppResult<Job> {
    transition(db, job_id, JobStatus::Active, None).await
}

/// Complete a job (either side can close).
pub async fn complete_job(db: &PgPool, job_id: Uuid) -> AppResult<Job> {
    sqlx::query_as::<_, Job>(
        r#"UPDATE jobs SET status = 'completed', ended_at = NOW(), updated_at = NOW()
           WHERE id = $1 RETURNING *"#,
    )
    .bind(job_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Job {job_id} not found")))
}

async fn transition(
    db: &PgPool,
    job_id: Uuid,
    status: JobStatus,
    started_at: Option<chrono::DateTime<Utc>>,
) -> AppResult<Job> {
    let row = if started_at.is_some() {
        sqlx::query_as::<_, Job>(
            "UPDATE jobs SET status = $1, started_at = $2, updated_at = NOW() WHERE id = $3 RETURNING *",
        )
        .bind(status)
        .bind(started_at)
        .bind(job_id)
        .fetch_optional(db)
        .await?
    } else {
        sqlx::query_as::<_, Job>(
            "UPDATE jobs SET status = $1, updated_at = NOW() WHERE id = $2 RETURNING *",
        )
        .bind(status)
        .bind(job_id)
        .fetch_optional(db)
        .await?
    };

    row.ok_or_else(|| AppError::NotFound(format!("Job {job_id} not found")))
}

// ── Job integrations (tool access) ───────────────────────────────────────────

pub struct AddIntegrationRequest {
    pub tool_type: ToolType,
    /// Owner-side Connection to draw credentials from. Set for github/google grants —
    /// the agent acts as its owner, so the company links the owner's connection rather
    /// than pasting a token.
    pub connection_id: Option<Uuid>,
    /// Raw credential JSON for grants that carry their own creds (e.g. a company Slack
    /// bot token). Encrypted before storage. Mutually exclusive with connection_id.
    pub credentials: Option<Value>,
    pub scope_config: Value,
}

pub async fn add_integration(
    db: &PgPool,
    job_id: Uuid,
    req: AddIntegrationRequest,
) -> AppResult<JobIntegration> {
    // A grant must be backed by either an owner Connection or raw credentials.
    if req.connection_id.is_none() && req.credentials.is_none() {
        return Err(AppError::BadRequest(
            "Integration requires either a connection_id or credentials".into(),
        ));
    }

    let encrypted = match &req.credentials {
        Some(creds) => Some(crypto::encrypt(&creds.to_string()).map_err(AppError::Internal)?),
        None => None,
    };

    let row = sqlx::query_as::<_, JobIntegration>(
        r#"INSERT INTO job_integrations (id, job_id, tool_type, connection_id, encrypted_credentials, scope_config, is_active, created_at)
           VALUES ($1, $2, $3, $4, $5, $6, TRUE, NOW())
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(job_id)
    .bind(&req.tool_type)
    .bind(req.connection_id)
    .bind(encrypted)
    .bind(req.scope_config)
    .fetch_one(db)
    .await?;

    Ok(row)
}

pub async fn list_integrations(db: &PgPool, job_id: Uuid) -> AppResult<Vec<JobIntegration>> {
    let rows = sqlx::query_as::<_, JobIntegration>(
        "SELECT * FROM job_integrations WHERE job_id = $1 ORDER BY created_at",
    )
    .bind(job_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Kill switch — deactivate all integrations for a job immediately.
pub async fn revoke_all_integrations(db: &PgPool, job_id: Uuid) -> AppResult<()> {
    sqlx::query!(
        "UPDATE job_integrations SET is_active = FALSE WHERE job_id = $1",
        job_id
    )
    .execute(db)
    .await?;
    Ok(())
}
