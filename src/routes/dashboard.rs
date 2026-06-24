use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    errors::{AppError, AppResult},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/professional/:professional_id", get(professional_summary))
        .route("/company/:company_id", get(company_summary))
}

#[derive(Serialize)]
struct ProfessionalDashboard {
    agent_status: String,
    active_jobs: i64,
    /// Jobs where the pro has been hired (agent_id set) but the company hasn't started trial
    /// yet (status = 'open'). Surfaced separately from active_jobs so the pro knows they're
    /// hired-and-waiting, not invisible.
    open_jobs: i64,
    tasks_pending_review: i64,
    tasks_delivered_today: i64,
    total_approved_outputs: i64,
    amendment_rate: f64, // % of outputs the professional amended before approving
    avg_confidence_score: f64,
    pending_review_tasks: Vec<PendingTaskSummary>,
}

#[derive(Serialize, sqlx::FromRow)]
struct PendingTaskSummary {
    task_id: Uuid,
    job_id: Uuid,
    job_title: String,
    confidence_score: Option<f32>,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn professional_summary(
    State(state): State<AppState>,
    Path(professional_id): Path<Uuid>,
) -> AppResult<Json<ProfessionalDashboard>> {
    // Agent status
    let agent = sqlx::query!(
        "SELECT status::text AS status FROM agents WHERE professional_id = $1",
        professional_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Agent not found for this professional".into()))?;

    // Aggregate stats. LEFT JOIN tasks so job-count stats (active_jobs, open_jobs) are correct
    // even when a job has no tasks yet — a freshly-hired ('open') or freshly-activated job with
    // no tasks would otherwise be dropped by an INNER join and show 0 jobs.
    let stats = sqlx::query!(
        r#"SELECT
            COUNT(DISTINCT j.id) FILTER (WHERE j.status IN ('trial','active')) AS active_jobs,
            COUNT(DISTINCT j.id) FILTER (WHERE j.status = 'open')               AS open_jobs,
            COUNT(t.id) FILTER (WHERE t.status = 'awaiting_review')           AS tasks_pending_review,
            COUNT(t.id) FILTER (WHERE t.status = 'delivered'
                AND t.delivered_at >= NOW() - INTERVAL '24 hours')             AS tasks_delivered_today,
            COUNT(t.id) FILTER (WHERE t.status IN ('approved','amended','delivered')) AS total_approved_outputs,
            (COUNT(t.id) FILTER (WHERE t.status = 'amended') * 100.0 /
                NULLIF(COUNT(t.id) FILTER (WHERE t.status IN ('approved','amended','delivered')), 0)
            )::float8                                                            AS amendment_rate,
            AVG(t.confidence_score::float8) FILTER (WHERE t.confidence_score IS NOT NULL) AS avg_confidence_score
           FROM agents a
           JOIN jobs j ON j.agent_id = a.id
           LEFT JOIN tasks t ON t.job_id = j.id
           WHERE a.professional_id = $1"#,
        professional_id
    )
    .fetch_one(&state.db)
    .await?;

    // Tasks awaiting review (up to 20, oldest first = most urgent)
    let pending = sqlx::query_as::<_, PendingTaskSummary>(
        r#"SELECT t.id AS task_id, t.job_id, j.title AS job_title,
                  t.confidence_score, t.created_at
           FROM tasks t
           JOIN jobs j ON j.id = t.job_id
           JOIN agents a ON a.id = j.agent_id
           WHERE a.professional_id = $1 AND t.status = 'awaiting_review'
           ORDER BY t.created_at ASC
           LIMIT 20"#,
    )
    .bind(professional_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(ProfessionalDashboard {
        agent_status: agent.status.unwrap_or_else(|| "unknown".to_string()),
        active_jobs: stats.active_jobs.unwrap_or(0),
        open_jobs: stats.open_jobs.unwrap_or(0),
        tasks_pending_review: stats.tasks_pending_review.unwrap_or(0),
        tasks_delivered_today: stats.tasks_delivered_today.unwrap_or(0),
        total_approved_outputs: stats.total_approved_outputs.unwrap_or(0),
        amendment_rate: stats.amendment_rate.unwrap_or(0.0),
        avg_confidence_score: stats.avg_confidence_score.unwrap_or(0.0),
        pending_review_tasks: pending,
    }))
}

#[derive(Serialize)]
struct CompanyDashboard {
    active_agents: i64,
    tasks_delivered_this_week: i64,
    tasks_pending: i64,
    total_hours_billed: f64,
    recent_deliveries: Vec<RecentDelivery>,
}

#[derive(Serialize, sqlx::FromRow)]
struct RecentDelivery {
    task_id: Uuid,
    job_title: String,
    professional_name: String,
    delivered_at: Option<chrono::DateTime<chrono::Utc>>,
    confidence_score: Option<f32>,
}

async fn company_summary(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> AppResult<Json<CompanyDashboard>> {
    let stats = sqlx::query!(
        r#"SELECT
            COUNT(DISTINCT j.id) FILTER (WHERE j.status IN ('trial','active'))   AS active_agents,
            COUNT(t.id) FILTER (WHERE t.status = 'delivered'
                AND t.delivered_at >= NOW() - INTERVAL '7 days')                 AS tasks_delivered_this_week,
            COUNT(t.id) FILTER (WHERE t.status IN ('pending','running','awaiting_review')) AS tasks_pending,
            COALESCE(SUM(
                EXTRACT(EPOCH FROM (COALESCE(t.delivered_at, NOW()) - t.created_at))
            ) FILTER (WHERE t.status = 'delivered'), 0)::float8                  AS total_seconds_active
           FROM jobs j
           LEFT JOIN tasks t ON t.job_id = j.id
           WHERE j.company_id = $1"#,
        company_id
    )
    .fetch_one(&state.db)
    .await?;

    let recent = sqlx::query_as::<_, RecentDelivery>(
        r#"SELECT t.id AS task_id, j.title AS job_title,
                  p.name AS professional_name, t.delivered_at, t.confidence_score
           FROM tasks t
           JOIN jobs j ON j.id = t.job_id
           JOIN agents a ON a.id = j.agent_id
           JOIN professionals p ON p.id = a.professional_id
           WHERE j.company_id = $1 AND t.status = 'delivered'
           ORDER BY t.delivered_at DESC
           LIMIT 20"#,
    )
    .bind(company_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(CompanyDashboard {
        active_agents: stats.active_agents.unwrap_or(0),
        tasks_delivered_this_week: stats.tasks_delivered_this_week.unwrap_or(0),
        tasks_pending: stats.tasks_pending.unwrap_or(0),
        total_hours_billed: stats.total_seconds_active.unwrap_or(0.0) / 3600.0,
        recent_deliveries: recent,
    }))
}
