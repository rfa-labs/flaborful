use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// A marketplace listing — the public-facing agent card companies browse.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AgentListing {
    pub agent_id: Uuid,
    pub professional_id: Uuid,
    pub professional_name: String,
    pub profession_name: String,
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
    pub hourly_rate: i32,
    pub aspects_offered: Vec<String>,
    pub completed_jobs: i64,
    pub approved_outputs: i64,
}

#[derive(Debug, Deserialize, Default)]
pub struct MarketplaceFilters {
    pub profession: Option<String>,
    pub aspect: Option<String>,
    pub max_rate: Option<i32>,
    pub available_only: Option<bool>,
}

pub async fn browse(db: &PgPool, filters: MarketplaceFilters) -> Result<Vec<AgentListing>> {
    // Build a denormalized listing per agent
    let rows = sqlx::query_as::<_, AgentListing>(
        r#"
        SELECT
            a.id                                    AS agent_id,
            p.id                                    AS professional_id,
            p.name                                  AS professional_name,
            pr.name                                 AS profession_name,
            p.bio,
            p.avatar_url,
            p.hourly_rate,
            COALESCE(
                ARRAY_AGG(DISTINCT asp.name) FILTER (WHERE asp.name IS NOT NULL),
                '{}'::text[]
            )                                       AS aspects_offered,
            COUNT(DISTINCT j.id) FILTER (WHERE j.status = 'completed') AS completed_jobs,
            COUNT(DISTINCT t.id) FILTER (WHERE t.status = 'delivered')  AS approved_outputs
        FROM agents a
        JOIN professionals p ON p.id = a.professional_id
        JOIN professions pr  ON pr.id = p.profession_id
        LEFT JOIN agent_aspects aa ON aa.agent_id = a.id AND aa.is_offered = TRUE
        LEFT JOIN aspects asp      ON asp.id = aa.aspect_id
        LEFT JOIN jobs j           ON j.agent_id = a.id
        LEFT JOIN tasks t          ON t.job_id = j.id
        WHERE a.status IN ('ready', 'deployed')
          AND ($1::text IS NULL OR LOWER(pr.name) = LOWER($1))
          AND ($2::text IS NULL OR EXISTS (
                SELECT 1 FROM agent_aspects aa2
                JOIN aspects asp2 ON asp2.id = aa2.aspect_id
                WHERE aa2.agent_id = a.id
                  AND aa2.is_offered = TRUE
                  AND LOWER(asp2.name) = LOWER($2)
              ))
          AND ($3::int IS NULL OR p.hourly_rate <= $3)
          AND ($4::bool IS FALSE OR $4 IS NULL OR p.is_available = TRUE)
        GROUP BY a.id, p.id, pr.name
        ORDER BY approved_outputs DESC, completed_jobs DESC
        "#,
    )
    .bind(filters.profession)
    .bind(filters.aspect)
    .bind(filters.max_rate)
    .bind(filters.available_only)
    .fetch_all(db)
    .await?;

    Ok(rows)
}

pub async fn get_listing(db: &PgPool, agent_id: Uuid) -> Result<Option<AgentListing>> {
    let row = sqlx::query_as::<_, AgentListing>(
        r#"
        SELECT
            a.id                                    AS agent_id,
            p.id                                    AS professional_id,
            p.name                                  AS professional_name,
            pr.name                                 AS profession_name,
            p.bio,
            p.avatar_url,
            p.hourly_rate,
            COALESCE(
                ARRAY_AGG(DISTINCT asp.name) FILTER (WHERE asp.name IS NOT NULL),
                '{}'::text[]
            )                                       AS aspects_offered,
            COUNT(DISTINCT j.id) FILTER (WHERE j.status = 'completed') AS completed_jobs,
            COUNT(DISTINCT t.id) FILTER (WHERE t.status = 'delivered')  AS approved_outputs
        FROM agents a
        JOIN professionals p ON p.id = a.professional_id
        JOIN professions pr  ON pr.id = p.profession_id
        LEFT JOIN agent_aspects aa ON aa.agent_id = a.id AND aa.is_offered = TRUE
        LEFT JOIN aspects asp      ON asp.id = aa.aspect_id
        LEFT JOIN jobs j           ON j.agent_id = a.id
        LEFT JOIN tasks t          ON t.job_id = j.id
        WHERE a.id = $1
        GROUP BY a.id, p.id, pr.name
        "#,
    )
    .bind(agent_id)
    .fetch_optional(db)
    .await?;

    Ok(row)
}
