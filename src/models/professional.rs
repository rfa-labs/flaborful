use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Professional {
    pub id: Uuid,
    pub user_id: Uuid,
    pub profession_id: Uuid,
    pub name: String,
    pub bio: String,
    pub avatar_url: Option<String>,
    pub hourly_rate: Option<i32>, // stored in cents
    pub years_experience: Option<i16>,
    pub is_available: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[allow(dead_code)] // agent↔aspect join row; mapped from DB, not read in the live API yet
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct AgentAspect {
    pub agent_id: Uuid,
    pub aspect_id: Uuid,
    pub is_offered: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateProfessionalRequest {
    pub profession_id: Uuid,
    pub name: String,
    pub bio: String,
    pub avatar_url: Option<String>,
    pub hourly_rate: Option<i32>,
    pub years_experience: Option<i16>,
}
