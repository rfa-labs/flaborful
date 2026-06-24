use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Profession {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Aspect {
    pub id: Uuid,
    pub profession_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

#[allow(dead_code)] // join view; assembled in the seed/admin path, not the live API yet
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProfessionWithAspects {
    pub profession: Profession,
    pub aspects: Vec<Aspect>,
}
