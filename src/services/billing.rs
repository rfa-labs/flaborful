/// Weekly billing: calculate hours billed per job, create invoices, mark payouts.
/// Called by a periodic job (or manually via POST /billing/generate) for all active jobs.
use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Invoice {
    pub id: Uuid,
    pub job_id: Uuid,
    pub company_id: Uuid,
    pub professional_id: Uuid,
    pub week_start: NaiveDate,
    pub week_end: NaiveDate,
    pub tasks_delivered: i32,
    pub hours_billed: f64,
    pub gross_amount: i32,
    pub platform_fee: i32,
    pub net_payout: i32,
    pub status: String,
    pub created_at: chrono::DateTime<Utc>,
    pub paid_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Payout {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub professional_id: Uuid,
    pub amount: i32,
    pub status: String,
    pub created_at: chrono::DateTime<Utc>,
    pub sent_at: Option<chrono::DateTime<Utc>>,
}

/// Generate a weekly invoice for a job covering the most recent completed week
/// (Mon–Sun). Idempotent: skips if invoice for that week already exists.
pub async fn generate_invoice(db: &PgPool, job_id: Uuid) -> Result<Option<Invoice>> {
    let today = Utc::now().date_naive();
    let days_since_monday = today.weekday().num_days_from_monday() as i64;
    let week_start = today - Duration::days(days_since_monday + 7); // previous Mon
    let week_end = week_start + Duration::days(6); // previous Sun

    // Check idempotency
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM invoices WHERE job_id = $1 AND week_start = $2",
    )
    .bind(job_id)
    .bind(week_start)
    .fetch_one(db)
    .await?;

    if existing > 0 {
        return Ok(None);
    }

    // Count tasks delivered during the week
    let tasks_delivered = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*) FROM tasks
           WHERE job_id = $1
             AND status = 'delivered'
             AND delivered_at::date BETWEEN $2 AND $3"#,
    )
    .bind(job_id)
    .bind(week_start)
    .bind(week_end)
    .fetch_one(db)
    .await? as i32;

    // Load job + participants
    let row = sqlx::query!(
        r#"SELECT j.hourly_rate, j.company_id, a.professional_id
           FROM jobs j
           JOIN agents a ON a.id = j.agent_id
           WHERE j.id = $1"#,
        job_id
    )
    .fetch_one(db)
    .await?;

    // Billing model: 40 hours/week baseline * fraction of tasks delivered
    // (simplified; real billing would track actual task duration)
    let hours: f64 = if tasks_delivered > 0 { 40.0 } else { 0.0 };
    let gross_cents = (hours * row.hourly_rate as f64) as i32;
    let platform_fee = (gross_cents as f64 * 0.15).round() as i32;
    let net_payout = gross_cents - platform_fee;

    let invoice = sqlx::query_as::<_, Invoice>(
        r#"INSERT INTO invoices
           (job_id, company_id, professional_id, week_start, week_end,
            tasks_delivered, hours_billed, gross_amount, platform_fee, net_payout)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
           RETURNING *"#,
    )
    .bind(job_id)
    .bind(row.company_id)
    .bind(row.professional_id)
    .bind(week_start)
    .bind(week_end)
    .bind(tasks_delivered)
    .bind(hours)
    .bind(gross_cents)
    .bind(platform_fee)
    .bind(net_payout)
    .fetch_one(db)
    .await?;

    Ok(Some(invoice))
}

/// Mark an invoice as paid and create a payout record for the professional.
pub async fn mark_paid(db: &PgPool, invoice_id: Uuid) -> Result<Payout> {
    let invoice = sqlx::query_as::<_, Invoice>(
        "UPDATE invoices SET status = 'paid', paid_at = NOW() WHERE id = $1 RETURNING *",
    )
    .bind(invoice_id)
    .fetch_one(db)
    .await?;

    let payout = sqlx::query_as::<_, Payout>(
        r#"INSERT INTO payouts (invoice_id, professional_id, amount)
           VALUES ($1, $2, $3)
           RETURNING *"#,
    )
    .bind(invoice_id)
    .bind(invoice.professional_id)
    .bind(invoice.net_payout)
    .fetch_one(db)
    .await?;

    Ok(payout)
}

pub async fn list_invoices_for_job(db: &PgPool, job_id: Uuid) -> Result<Vec<Invoice>> {
    Ok(sqlx::query_as::<_, Invoice>(
        "SELECT * FROM invoices WHERE job_id = $1 ORDER BY week_start DESC",
    )
    .bind(job_id)
    .fetch_all(db)
    .await?)
}

pub async fn list_invoices_for_company(db: &PgPool, company_id: Uuid) -> Result<Vec<Invoice>> {
    Ok(sqlx::query_as::<_, Invoice>(
        "SELECT * FROM invoices WHERE company_id = $1 ORDER BY week_start DESC",
    )
    .bind(company_id)
    .fetch_all(db)
    .await?)
}

pub async fn list_payouts_for_professional(
    db: &PgPool,
    professional_id: Uuid,
) -> Result<Vec<Payout>> {
    Ok(sqlx::query_as::<_, Payout>(
        "SELECT * FROM payouts WHERE professional_id = $1 ORDER BY created_at DESC",
    )
    .bind(professional_id)
    .fetch_all(db)
    .await?)
}

/// Generate invoices for all active jobs — called weekly (cron or manual trigger).
pub async fn generate_all_weekly(db: &PgPool) -> Result<Vec<Invoice>> {
    let job_ids =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM jobs WHERE status IN ('trial', 'active')")
            .fetch_all(db)
            .await?;

    let mut invoices = Vec::new();
    for job_id in job_ids {
        if let Some(inv) = generate_invoice(db, job_id).await? {
            invoices.push(inv);
        }
    }
    Ok(invoices)
}
