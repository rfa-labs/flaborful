//! Resource-ownership authorization checks.
//!
//! `AuthUser` only proves *who* the caller is. These helpers prove the caller is allowed
//! to act on a *specific* resource — the company that posted a job, or the professional
//! who owns an application. Every company/professional-scoped mutation should route
//! through one of these before touching the resource.

use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::{AppError, AppResult};

/// Resolve the company owned by this user. Errors if the user has no company.
pub async fn require_company(db: &PgPool, user_id: Uuid) -> AppResult<Uuid> {
    sqlx::query_scalar!("SELECT id FROM companies WHERE user_id = $1", user_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::Forbidden("You don't have a company profile".into()))
}

/// Assert the caller's company owns `job_id`. Returns the company id on success.
pub async fn require_job_owner(db: &PgPool, user_id: Uuid, job_id: Uuid) -> AppResult<Uuid> {
    let company_id = require_company(db, user_id).await?;
    let owns = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM jobs WHERE id = $1 AND company_id = $2)",
        job_id,
        company_id
    )
    .fetch_one(db)
    .await?
    .unwrap_or(false);

    if !owns {
        return Err(AppError::Forbidden(
            "This job belongs to another company".into(),
        ));
    }
    Ok(company_id)
}

/// Assert the caller's company owns the job that `application_id` belongs to.
/// Returns the application's `job_id` on success.
pub async fn require_application_company(
    db: &PgPool,
    user_id: Uuid,
    application_id: Uuid,
) -> AppResult<Uuid> {
    let company_id = require_company(db, user_id).await?;
    let row = sqlx::query!(
        r#"SELECT a.job_id, j.company_id
           FROM job_applications a JOIN jobs j ON j.id = a.job_id
           WHERE a.id = $1"#,
        application_id
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Application {application_id} not found")))?;

    if row.company_id != company_id {
        return Err(AppError::Forbidden(
            "This application belongs to another company's job".into(),
        ));
    }
    Ok(row.job_id)
}

/// Resolve the professional owned by this user. Errors if the user has no professional profile.
#[allow(dead_code)] // authz guard kept for symmetry with require_agent_owner / require_*_company
pub async fn require_professional(db: &PgPool, user_id: Uuid) -> AppResult<Uuid> {
    sqlx::query_scalar!("SELECT id FROM professionals WHERE user_id = $1", user_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::Forbidden("You don't have a professional profile".into()))
}

/// Assert the caller is the professional who owns `agent_id` (i.e. their agent).
pub async fn require_agent_owner(db: &PgPool, user_id: Uuid, agent_id: Uuid) -> AppResult<()> {
    let owns = sqlx::query_scalar!(
        r#"SELECT EXISTS(
              SELECT 1 FROM agents a
              JOIN professionals p ON p.id = a.professional_id
              WHERE a.id = $1 AND p.user_id = $2
           )"#,
        agent_id,
        user_id
    )
    .fetch_one(db)
    .await?
    .unwrap_or(false);

    if !owns {
        return Err(AppError::Forbidden(
            "This agent belongs to another professional".into(),
        ));
    }
    Ok(())
}
