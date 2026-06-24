use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, sqlx::Type, Clone, PartialEq)]
#[sqlx(type_name = "application_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ApplicationStatus {
    Applied,
    Shortlisted,
    Interviewing,
    Selected,
    Rejected,
    Withdrawn,
}

/// A professional's agent applying to an open job posting.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct JobApplication {
    pub id: Uuid,
    pub job_id: Uuid,
    pub agent_id: Uuid,
    pub professional_id: Uuid,
    pub status: ApplicationStatus,
    pub resume_text: String,
    pub cover_letter: String,
    pub video_url: Option<String>,
    /// The KB chunk ids the owner chose to expose for this application (frozen snapshot).
    pub shared_chunk_ids: Vec<Uuid>,
    pub meeting_link: Option<String>,
    pub meeting_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Owner's request to apply to a job.
#[derive(Debug, Deserialize)]
pub struct CreateApplicationRequest {
    pub resume_text: Option<String>,
    pub cover_letter: String,
    pub video_url: Option<String>,
    /// KB chunk ids to expose. Server validates ownership + shareable before snapshotting.
    pub shared_chunk_ids: Vec<Uuid>,
}

/// Applicant card the company sees in the pool. Includes the resolved (shareable-only)
/// shared knowledge chunks, never the live KB.
#[derive(Debug, Serialize)]
pub struct ApplicantView {
    pub application: JobApplication,
    pub professional_name: String,
    pub profession_name: String,
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
    pub hourly_rate: Option<i32>,
    pub years_experience: Option<i16>,
    pub completed_jobs: i64,
    pub approved_outputs: i64,
    /// Previews of the chunks the owner chose to share, resolved at view time.
    pub shared_knowledge: Vec<SharedChunkView>,
}

#[derive(Debug, Serialize)]
pub struct SharedChunkView {
    pub id: Uuid,
    pub source_type: String,
    pub content: String,
}
