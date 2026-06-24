use crate::errors::AppResult;
use crate::models::profession::{Aspect, Profession};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn list_professions(db: &PgPool) -> AppResult<Vec<Profession>> {
    let rows = sqlx::query_as::<_, Profession>("SELECT * FROM professions ORDER BY name")
        .fetch_all(db)
        .await?;
    Ok(rows)
}

pub async fn list_aspects(db: &PgPool, profession_id: Uuid) -> AppResult<Vec<Aspect>> {
    let rows =
        sqlx::query_as::<_, Aspect>("SELECT * FROM aspects WHERE profession_id = $1 ORDER BY name")
            .bind(profession_id)
            .fetch_all(db)
            .await?;
    Ok(rows)
}
