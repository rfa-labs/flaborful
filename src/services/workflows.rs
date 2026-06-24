use crate::errors::{AppError, AppResult};
use crate::models::workflow::{CreateWorkflowRequest, UpdateWorkflowRequest, Workflow};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn create_workflow(db: &PgPool, req: CreateWorkflowRequest) -> AppResult<Workflow> {
    let row = sqlx::query_as::<_, Workflow>(
        r#"INSERT INTO workflows (id, agent_id, aspect_id, name, description, graph, is_universal, job_id, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(req.agent_id)
    .bind(req.aspect_id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.graph)
    .bind(req.is_universal)
    .bind(req.job_id)
    .bind(Utc::now())
    .fetch_one(db)
    .await?;
    Ok(row)
}

pub async fn update_workflow(
    db: &PgPool,
    id: Uuid,
    req: UpdateWorkflowRequest,
) -> AppResult<Workflow> {
    let current = get_workflow(db, id).await?;
    let row = sqlx::query_as::<_, Workflow>(
        r#"UPDATE workflows
           SET name = $2, description = $3, graph = $4, is_universal = $5, updated_at = $6
           WHERE id = $1
           RETURNING *"#,
    )
    .bind(id)
    .bind(req.name.unwrap_or(current.name))
    .bind(req.description.or(current.description))
    .bind(req.graph.unwrap_or(current.graph))
    .bind(req.is_universal.unwrap_or(current.is_universal))
    .bind(Utc::now())
    .fetch_one(db)
    .await?;
    Ok(row)
}

pub async fn get_workflow(db: &PgPool, id: Uuid) -> AppResult<Workflow> {
    sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Workflow {id} not found")))
}

pub async fn delete_workflow(db: &PgPool, id: Uuid) -> AppResult<()> {
    let result = sqlx::query("DELETE FROM workflows WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Workflow {id} not found")));
    }
    Ok(())
}

pub async fn list_for_agent(db: &PgPool, agent_id: Uuid) -> AppResult<Vec<Workflow>> {
    let rows = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE agent_id = $1 ORDER BY created_at DESC",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}
