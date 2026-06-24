use crate::errors::{AppError, AppResult};
use crate::models::professional::{CreateProfessionalRequest, Professional};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn create_professional(
    db: &PgPool,
    user_id: uuid::Uuid,
    req: CreateProfessionalRequest,
) -> AppResult<Professional> {
    let row = sqlx::query_as::<_, Professional>(
        r#"INSERT INTO professionals
           (id, user_id, profession_id, name, bio, avatar_url, hourly_rate, years_experience, is_available, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, true, $9, $9)
           ON CONFLICT (user_id) DO UPDATE SET
               profession_id = EXCLUDED.profession_id,
               name = EXCLUDED.name,
               bio = EXCLUDED.bio,
               avatar_url = EXCLUDED.avatar_url,
               hourly_rate = EXCLUDED.hourly_rate,
               years_experience = EXCLUDED.years_experience,
               updated_at = EXCLUDED.updated_at
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(req.profession_id)
    .bind(&req.name)
    .bind(&req.bio)
    .bind(&req.avatar_url)
    .bind(req.hourly_rate)
    .bind(req.years_experience)
    .bind(Utc::now())
    .fetch_one(db)
    .await?;
    Ok(row)
}

pub async fn get_professional_by_user(db: &PgPool, user_id: Uuid) -> AppResult<Professional> {
    sqlx::query_as::<_, Professional>("SELECT * FROM professionals WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound("Professional not found".into()))
}

pub async fn get_professional(db: &PgPool, id: Uuid) -> AppResult<Professional> {
    sqlx::query_as::<_, Professional>("SELECT * FROM professionals WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Professional {id} not found")))
}
