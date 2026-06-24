use crate::errors::{AppError, AppResult};
use crate::models::company::{Company, CreateCompanyRequest};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn create_company(
    db: &PgPool,
    user_id: Uuid,
    req: CreateCompanyRequest,
) -> AppResult<Company> {
    let row = sqlx::query_as::<_, Company>(
        r#"INSERT INTO companies (id, user_id, name, industry, website, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $6)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(&req.name)
    .bind(&req.industry)
    .bind(&req.website)
    .bind(Utc::now())
    .fetch_one(db)
    .await?;
    Ok(row)
}

pub async fn get_company_by_user(db: &PgPool, user_id: Uuid) -> AppResult<Company> {
    sqlx::query_as::<_, Company>("SELECT * FROM companies WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound("Company not found".into()))
}

pub async fn get_company(db: &PgPool, id: Uuid) -> AppResult<Company> {
    sqlx::query_as::<_, Company>("SELECT * FROM companies WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Company {id} not found")))
}
