//! Job applications: the professional-side of the two-sided job board.
//!
//! A professional's agent applies to an open job. The owner attaches a resume, a cover
//! letter, an optional self-intro video, and a curated set of knowledge-base chunks they
//! choose to expose (IP control — companies only ever see this frozen snapshot, never the
//! live KB). The company reviews the applicant pool, optionally sets an interview link,
//! and selects one applicant — which wires that applicant's agent into the job and feeds
//! the application materials back into the agent's KB for future retraining.

use qdrant_client::Qdrant;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::errors::{AppError, AppResult};
use crate::models::application::*;
use crate::models::job::Job;
use crate::services::inference::InferenceClient;
use crate::services::knowledge::{self, ChunkVisibility, KnowledgeSourceType};

/// Resolve the professional_id + agent_id for a user, erroring if they're not a pro with
/// an agent.
async fn resolve_pro_agent(db: &PgPool, user_id: Uuid) -> AppResult<(Uuid, Uuid)> {
    let row = sqlx::query!(
        r#"SELECT p.id as professional_id, a.id as agent_id
           FROM professionals p JOIN agents a ON a.professional_id = p.id
           WHERE p.user_id = $1"#,
        user_id
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| {
        AppError::BadRequest("You need a professional profile with an agent to apply".into())
    })?;
    Ok((row.professional_id, row.agent_id))
}

/// The professional's resume text from their KB, used as the default on applications.
pub async fn default_resume_text(qdrant: &Arc<Qdrant>, agent_id: Uuid) -> Option<String> {
    let chunks = knowledge::list_chunks_for_agent(qdrant, agent_id)
        .await
        .ok()?;
    chunks
        .into_iter()
        .find(|c| c.source_type == KnowledgeSourceType::Resume)
        .map(|c| c.content)
}

/// Submit an application from the current user's agent to a job.
pub async fn apply(
    db: &PgPool,
    qdrant: &Arc<Qdrant>,
    user_id: Uuid,
    job_id: Uuid,
    req: CreateApplicationRequest,
) -> AppResult<JobApplication> {
    let (professional_id, agent_id) = resolve_pro_agent(db, user_id).await?;

    // Job must exist and be accepting applications.
    let job = sqlx::query_as::<_, Job>("SELECT * FROM jobs WHERE id = $1")
        .bind(job_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Job {job_id} not found")))?;
    if !matches!(
        job.status,
        crate::models::job::JobStatus::Reviewing | crate::models::job::JobStatus::Open
    ) {
        return Err(AppError::BadRequest(
            "This job is not accepting applications".into(),
        ));
    }

    // Validate the chosen chunks belong to this agent AND are marked shareable. Anything
    // that fails the check is silently dropped so a stale/private id can't leak.
    let valid_chunks =
        knowledge::fetch_chunks_by_ids(qdrant, agent_id, &req.shared_chunk_ids, true)
            .await
            .map_err(AppError::Internal)?;
    let shared_chunk_ids: Vec<Uuid> = valid_chunks.iter().map(|c| c.id).collect();

    // Default the resume from the KB if the applicant didn't override it.
    let resume_text = match req.resume_text {
        Some(t) if !t.trim().is_empty() => t,
        _ => default_resume_text(qdrant, agent_id)
            .await
            .unwrap_or_default(),
    };

    let application = sqlx::query_as::<_, JobApplication>(
        r#"INSERT INTO job_applications
              (job_id, agent_id, professional_id, resume_text, cover_letter, video_url, shared_chunk_ids)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           ON CONFLICT (job_id, agent_id) DO UPDATE SET
              resume_text      = EXCLUDED.resume_text,
              cover_letter     = EXCLUDED.cover_letter,
              video_url        = EXCLUDED.video_url,
              shared_chunk_ids = EXCLUDED.shared_chunk_ids,
              status           = 'applied',
              updated_at       = NOW()
           RETURNING *"#,
    )
    .bind(job_id)
    .bind(agent_id)
    .bind(professional_id)
    .bind(&resume_text)
    .bind(&req.cover_letter)
    .bind(&req.video_url)
    .bind(&shared_chunk_ids)
    .fetch_one(db)
    .await?;

    // The cover letter is signal about how this professional pitches their work — feed it
    // into the KB so future retrains pick it up.
    if !req.cover_letter.trim().is_empty() {
        ingest_application_material(
            db,
            qdrant,
            &InferenceClient::from_env(),
            agent_id,
            job_id,
            &format!(
                "Cover letter for job '{}':\n{}",
                job.title, req.cover_letter
            ),
        )
        .await;
    }

    Ok(application)
}

/// Withdraw the current user's application.
pub async fn withdraw(db: &PgPool, user_id: Uuid, job_id: Uuid) -> AppResult<()> {
    let (_pro, agent_id) = resolve_pro_agent(db, user_id).await?;
    sqlx::query!(
        "UPDATE job_applications SET status = 'withdrawn', updated_at = NOW() WHERE job_id = $1 AND agent_id = $2",
        job_id, agent_id
    )
    .execute(db)
    .await?;
    Ok(())
}

/// List applicants for a job (company view), resolving each applicant's shared KB snapshot
/// and headline stats.
pub async fn list_applicants(
    db: &PgPool,
    qdrant: &Arc<Qdrant>,
    job_id: Uuid,
) -> AppResult<Vec<ApplicantView>> {
    let applications = sqlx::query_as::<_, JobApplication>(
        "SELECT * FROM job_applications WHERE job_id = $1 AND status <> 'withdrawn' ORDER BY created_at",
    )
    .bind(job_id)
    .fetch_all(db)
    .await?;

    let mut views = Vec::with_capacity(applications.len());
    for application in applications {
        // Professional headline + stats.
        let meta = sqlx::query!(
            r#"SELECT p.name as professional_name, pr.name as profession_name,
                      p.bio, p.avatar_url, p.hourly_rate, p.years_experience,
                      COUNT(DISTINCT j.id) FILTER (WHERE j.status = 'completed') as "completed_jobs!",
                      COUNT(DISTINCT t.id) FILTER (WHERE t.status = 'delivered') as "approved_outputs!"
               FROM professionals p
               JOIN professions pr ON pr.id = p.profession_id
               LEFT JOIN jobs j ON j.agent_id = $2
               LEFT JOIN tasks t ON t.job_id = j.id
               WHERE p.id = $1
               GROUP BY p.id, pr.name"#,
            application.professional_id,
            application.agent_id,
        )
        .fetch_one(db)
        .await?;

        // Resolve the frozen shared snapshot, re-checking shareable so a since-unshared
        // chunk drops out of the company's view.
        let shared_knowledge = knowledge::fetch_chunks_by_ids(
            qdrant,
            application.agent_id,
            &application.shared_chunk_ids,
            true,
        )
        .await
        .map_err(AppError::Internal)?
        .into_iter()
        .map(|c| SharedChunkView {
            id: c.id,
            source_type: format!("{:?}", c.source_type).to_lowercase(),
            content: c.content,
        })
        .collect();

        views.push(ApplicantView {
            professional_name: meta.professional_name,
            profession_name: meta.profession_name,
            bio: Some(meta.bio),
            avatar_url: meta.avatar_url,
            hourly_rate: meta.hourly_rate,
            years_experience: meta.years_experience,
            completed_jobs: meta.completed_jobs,
            approved_outputs: meta.approved_outputs,
            shared_knowledge,
            application,
        });
    }

    Ok(views)
}

/// Company updates an application's status (shortlist / reject / mark interviewing).
pub async fn set_status(
    db: &PgPool,
    application_id: Uuid,
    status: ApplicationStatus,
) -> AppResult<JobApplication> {
    sqlx::query_as::<_, JobApplication>(
        "UPDATE job_applications SET status = $1, updated_at = NOW() WHERE id = $2 RETURNING *",
    )
    .bind(status)
    .bind(application_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Application {application_id} not found")))
}

/// Company sets/updates the interview meeting link + time on an application.
pub async fn set_meeting(
    db: &PgPool,
    application_id: Uuid,
    meeting_link: Option<String>,
    meeting_at: Option<chrono::DateTime<chrono::Utc>>,
) -> AppResult<JobApplication> {
    sqlx::query_as::<_, JobApplication>(
        r#"UPDATE job_applications
           SET meeting_link = $1, meeting_at = $2, status = 'interviewing', updated_at = NOW()
           WHERE id = $3 RETURNING *"#,
    )
    .bind(meeting_link)
    .bind(meeting_at)
    .bind(application_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Application {application_id} not found")))
}

/// Select an applicant for the job. This is the moment the job gets its agent: we set
/// jobs.agent_id, transition the job out of review, mark the other applicants rejected,
/// and ingest the winning application's materials into the agent's KB.
pub async fn select_applicant(
    db: &PgPool,
    qdrant: &Arc<Qdrant>,
    inference: &InferenceClient,
    application_id: Uuid,
) -> AppResult<Job> {
    let application =
        sqlx::query_as::<_, JobApplication>("SELECT * FROM job_applications WHERE id = $1")
            .bind(application_id)
            .fetch_optional(db)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("Application {application_id} not found")))?;

    let mut tx = db.begin().await?;

    // Wire the winning agent into the job and open it for the trial flow.
    let job = sqlx::query_as::<_, Job>(
        r#"UPDATE jobs SET agent_id = $1, status = 'open', updated_at = NOW()
           WHERE id = $2 RETURNING *"#,
    )
    .bind(application.agent_id)
    .bind(application.job_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Job {} not found", application.job_id)))?;

    // Mark this application selected; reject the rest.
    sqlx::query!(
        "UPDATE job_applications SET status = 'selected', updated_at = NOW() WHERE id = $1",
        application_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"UPDATE job_applications SET status = 'rejected', updated_at = NOW()
           WHERE job_id = $1 AND id <> $2 AND status NOT IN ('withdrawn')"#,
        application.job_id,
        application_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // Feed the resume into the KB (idempotent-ish: stored as application material tied to
    // this job) so the agent's retraining reflects what it was hired on.
    if !application.resume_text.trim().is_empty() {
        ingest_application_material(
            db,
            qdrant,
            inference,
            application.agent_id,
            application.job_id,
            &format!(
                "Resume submitted for job '{}':\n{}",
                job.title, application.resume_text
            ),
        )
        .await;
    }

    Ok(job)
}

/// Embed and store a piece of application material as an ApplicationMaterial KB chunk.
/// Best-effort: logs and swallows errors so it never blocks the main flow.
async fn ingest_application_material(
    db: &PgPool,
    qdrant: &Arc<Qdrant>,
    inference: &InferenceClient,
    agent_id: Uuid,
    job_id: Uuid,
    content: &str,
) {
    let note = crate::services::vault::VaultNote {
        id: Uuid::new_v4(),
        agent_id,
        slug: format!(
            "application-{}-{}",
            job_id.simple(),
            Uuid::new_v4().simple()
        ),
        title: content
            .lines()
            .next()
            .unwrap_or("Application material")
            .chars()
            .take(80)
            .collect(),
        body: content.to_string(),
        layer: crate::services::vault::NoteLayer::Note,
        source_type: KnowledgeSourceType::ApplicationMaterial,
        visibility: ChunkVisibility::Private,
        domain: None,
        related: vec![],
        repo: None,
        job_id: Some(job_id),
    };
    if let Err(e) = crate::services::vault::upsert_note(db, qdrant, inference, note).await {
        tracing::warn!(
            "Failed to ingest application material for agent {}: {:?}",
            agent_id,
            e
        );
    }
}

/// Helper so callers without inference handy can ingest interview notes uniformly.
pub async fn ingest_interview_note(
    db: &PgPool,
    qdrant: &Arc<Qdrant>,
    inference: &InferenceClient,
    agent_id: Uuid,
    job_id: Uuid,
    note: &str,
) -> AppResult<()> {
    let vnote = crate::services::vault::VaultNote {
        id: Uuid::new_v4(),
        agent_id,
        slug: format!("interview-{}-{}", job_id.simple(), Uuid::new_v4().simple()),
        title: note
            .lines()
            .next()
            .unwrap_or("Interview note")
            .chars()
            .take(80)
            .collect(),
        body: note.to_string(),
        layer: crate::services::vault::NoteLayer::Note,
        source_type: KnowledgeSourceType::InterviewNote,
        visibility: ChunkVisibility::Private,
        domain: None,
        related: vec![],
        repo: None,
        job_id: Some(job_id),
    };
    crate::services::vault::upsert_note(db, qdrant, inference, vnote)
        .await
        .map_err(AppError::Internal)?;
    Ok(())
}
