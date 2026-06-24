use anyhow::Result;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::services::inference::InferenceClient;

/// Generate the tri-party contract for a job hire.
/// Returns a markdown document ready to display/sign.
pub async fn generate_contract(
    db: &PgPool,
    inference: &InferenceClient,
    job_id: Uuid,
) -> Result<String> {
    // Load all parties and job details
    let row = sqlx::query!(
        r#"SELECT
            j.title, j.description, j.hourly_rate,
            c.name AS company_name,
            p.name AS professional_name,
            pr.name AS profession_name
           FROM jobs j
           JOIN companies c ON c.id = j.company_id
           JOIN agents a    ON a.id = j.agent_id
           JOIN professionals p ON p.id = a.professional_id
           JOIN professions pr  ON pr.id = p.profession_id
           WHERE j.id = $1"#,
        job_id
    )
    .fetch_one(db)
    .await?;

    let system = r#"You are a legal document generator specializing in AI work arrangements.
Generate a professional tri-party contract in Markdown format covering:

1. **Parties**: Flaborful Platform, the Professional, and the Company
2. **Scope of Work**: What the agent will do based on the job description
3. **IP Ownership**: Company owns work output; Professional owns the AI agent/persona
4. **Data Boundaries**: Agent only accesses company data explicitly granted
5. **Confidentiality**: NDA between professional and company; platform confidentiality
6. **Compensation**: Rate, billing cycle (weekly), platform fee (15%)
7. **Human Review Gate**: All outputs reviewed by professional before delivery
8. **Kill Switch**: Company may revoke agent access immediately at any time
9. **Data Retention**: Job-scoped data deleted 30 days after contract ends (or archived per company request)
10. **Termination**: Either party may terminate with 48h notice

Use formal legal language but keep it readable. Include signature blocks for all three parties."#;

    let user = format!(
        r#"Generate a contract for this engagement:

Job Title: {}
Job Description: {}
Rate: ${:.2}/hour
Company: {}
Professional: {} ({})
Contract Date: {}
Platform: Flaborful

Platform fee: 15% of hourly rate
Net professional rate: ${:.2}/hour"#,
        row.title,
        row.description,
        row.hourly_rate as f64 / 100.0,
        row.company_name,
        row.professional_name,
        row.profession_name,
        Utc::now().format("%B %d, %Y"),
        row.hourly_rate as f64 / 100.0 * 0.85,
    );

    let contract_md = inference.orchestrate(system, &user, false).await?;
    Ok(contract_md)
}

/// Store the generated contract text on the job row.
/// The `contracts` column is a JSONB array — we append each version.
pub async fn save_contract(db: &PgPool, job_id: Uuid, contract_md: &str) -> Result<()> {
    let entry = serde_json::json!({
        "version": 1,
        "generated_at": Utc::now().to_rfc3339(),
        "content": contract_md,
        "signed": false,
    });

    sqlx::query!(
        r#"UPDATE jobs SET contracts = contracts || $1::jsonb,
           updated_at = NOW() WHERE id = $2"#,
        serde_json::json!([entry]),
        job_id
    )
    .execute(db)
    .await?;

    Ok(())
}
