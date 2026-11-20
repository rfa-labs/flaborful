use crate::errors::{AppError, AppResult};
use crate::models::agent::{Agent, AgentStatus, CreateAgentRequest};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn create_agent(db: &PgPool, req: CreateAgentRequest) -> AppResult<Agent> {
    let row = sqlx::query_as::<_, Agent>(
        r#"INSERT INTO agents (id, professional_id, status, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $4)
           ON CONFLICT (professional_id) DO UPDATE SET updated_at = EXCLUDED.updated_at
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(req.professional_id)
    .bind(AgentStatus::Onboarding)
    .bind(Utc::now())
    .fetch_one(db)
    .await?;
    Ok(row)
}

pub async fn get_agent_by_professional(db: &PgPool, professional_id: Uuid) -> AppResult<Agent> {
    sqlx::query_as::<_, Agent>("SELECT * FROM agents WHERE professional_id = $1")
        .bind(professional_id)
        .fetch_optional(db)
        .await?
        .map(Agent::with_key_flag)
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "Agent for professional {professional_id} not found"
            ))
        })
}

pub async fn get_agent(db: &PgPool, id: Uuid) -> AppResult<Agent> {
    sqlx::query_as::<_, Agent>("SELECT * FROM agents WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .map(Agent::with_key_flag)
        .ok_or_else(|| AppError::NotFound(format!("Agent {id} not found")))
}

pub async fn set_agent_status(db: &PgPool, id: Uuid, status: AgentStatus) -> AppResult<Agent> {
    sqlx::query_as::<_, Agent>(
        "UPDATE agents SET status = $1, updated_at = NOW() WHERE id = $2 RETURNING *",
    )
    .bind(status)
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Agent {id} not found")))
}

pub async fn list_marketplace(db: &PgPool) -> AppResult<Vec<Agent>> {
    let rows = sqlx::query_as::<_, Agent>(
        "SELECT * FROM agents WHERE status = $1 OR status = $2 ORDER BY created_at DESC",
    )
    .bind(AgentStatus::Ready)
    .bind(AgentStatus::Deployed)
    .fetch_all(db)
    .await?;
    Ok(rows)
}
