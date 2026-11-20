//! The COMPANY knowledge vault — a company's per-repo knowledge graph of its codebases.
//!
//! Mirrors the professional vault surface (`routes/knowledge.rs` + the ingestion endpoints in
//! `routes/onboarding.rs`) but scoped to `VaultOwner::Company(company_id)` and guarded by
//! `authz::require_company_owner`. Ingestion is **per-repo** — a company documents one repo at a
//! time into its vault (unlike the professional onboarding, which batches the top repos).
//!
//! These are the company's *technical* notes (which may hold proprietary detail). The work an
//! agent does for a hired job adds more such notes (the `record_technical` tool); the company
//! owns all of them — see [[Flaborful Composable Knowledge Vaults]].

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth_extractor::AuthUser;
use crate::errors::{AppError, AppResult};
use crate::models::connection::ConnectionProvider;
use crate::services::knowledge::{ChunkVisibility, KnowledgeSourceType};
use crate::services::onboarding::github::GitHubIngestion;
use crate::services::vault::{self, NoteLayer, VaultNote, VaultOwner};
use crate::services::{authz, connections};
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/:company_id/vault/repos", get(list_repos))
        .route("/:company_id/vault/ingest", post(ingest_repo))
        .route("/:company_id/vault/ingestion/status", get(ingestion_status))
        .route(
            "/:company_id/vault/ingestion/cancel",
            post(ingestion_cancel),
        )
        .route("/:company_id/vault/notes", get(list_notes))
        .route("/:company_id/vault/reindex", post(reindex))
        .route("/:company_id/vault/repair-links", post(repair_links))
        .route("/:company_id/vault/graph", get(graph))
        .route("/:company_id/vault/timeline", get(timeline))
        .route(
            "/:company_id/vault/note/:slug",
            get(get_note).put(put_note).delete(delete_note),
        )
}

fn owner(company_id: Uuid) -> VaultOwner {
    VaultOwner::Company(company_id)
}

// ── Repo discovery + ingestion ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct RepoOption {
    full_name: String,
    default_branch: String,
    /// Whether this repo already has notes in the company vault (so the UI can mark it done).
    documented: bool,
}

/// GET /companies/:id/vault/repos — the company's GitHub repos, each flagged if already documented.
async fn list_repos(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(company_id): Path<Uuid>,
) -> AppResult<Json<Vec<RepoOption>>> {
    authz::require_company_owner(&state.db, auth.user_id, company_id).await?;
    let conn = connections::get_for_user(&state.db, auth.user_id, ConnectionProvider::Github)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest("GitHub account not connected.".into()))?;
    let token = connections::fresh_access_token(&state.db, &conn)
        .await
        .map_err(AppError::Internal)?;
    let profile = GitHubIngestion::new(Some(token)).ingest(None).await?;

    let mut out = Vec::with_capacity(profile.repos.len());
    for r in &profile.repos {
        let documented = sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM vault_notes WHERE owner_kind = 'company' AND agent_id = $1 AND repo = $2)",
            company_id,
            r.full_name
        )
        .fetch_one(&state.db)
        .await?
        .unwrap_or(false);
        out.push(RepoOption {
            full_name: r.full_name.clone(),
            default_branch: r.default_branch.clone(),
            documented,
        });
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct IngestRepoBody {
    repo: String,
    branch: Option<String>,
    #[serde(default)]
    force: bool,
}

/// POST /companies/:id/vault/ingest — document ONE repo into the company vault (background run).
async fn ingest_repo(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(company_id): Path<Uuid>,
    Json(body): Json<IngestRepoBody>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_company_owner(&state.db, auth.user_id, company_id).await?;
    if body.repo.trim().is_empty() {
        return Err(AppError::BadRequest("repo is required".into()));
    }
    let conn = connections::get_for_user(&state.db, auth.user_id, ConnectionProvider::Github)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest("GitHub account not connected.".into()))?;
    let token = connections::fresh_access_token(&state.db, &conn)
        .await
        .map_err(AppError::Internal)?;
    let branch = body.branch.unwrap_or_else(|| "main".to_string());

    // Per-repo: a single (repo, branch). Company vaults have no principal (no persona/LoRA).
    crate::services::onboarding::ingest::spawn_ingestion(
        state.db.clone(),
        state.backends.clone(),
        owner(company_id),
        None,
        Some(token),
        vec![(body.repo.clone(), branch)],
        body.force,
    );
    Ok(Json(
        serde_json::json!({ "status": "ingesting", "repo": body.repo }),
    ))
}

#[derive(Serialize)]
struct IngestionStatusView {
    complete: bool,
    total_notes: i64,
    repos: Vec<RepoStatus>,
}

#[derive(Serialize)]
struct RepoStatus {
    repo: String,
    status: String,
    detail: String,
    notes_count: i32,
}

/// GET /companies/:id/vault/ingestion/status — latest per-repo status + authoritative note total.
async fn ingestion_status(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(company_id): Path<Uuid>,
) -> AppResult<Json<IngestionStatusView>> {
    authz::require_company_owner(&state.db, auth.user_id, company_id).await?;

    // Latest status row per non-sentinel repo.
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (repo) repo, status, detail, notes_count
           FROM ingestion_events
           WHERE owner_kind = 'company' AND agent_id = $1 AND kind = 'status' AND repo <> ''
           ORDER BY repo, created_at DESC"#,
        company_id
    )
    .fetch_all(&state.db)
    .await?;
    let repos = rows
        .into_iter()
        .map(|r| RepoStatus {
            repo: r.repo,
            status: r.status.unwrap_or_default(),
            detail: r.detail,
            notes_count: r.notes_count,
        })
        .collect();

    let complete = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM ingestion_events
             WHERE owner_kind = 'company' AND agent_id = $1 AND status = 'complete')"#,
        company_id
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(false);

    let total_notes = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM vault_notes WHERE owner_kind = 'company' AND agent_id = $1 AND repo IS NOT NULL",
        company_id
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(0);

    Ok(Json(IngestionStatusView {
        complete,
        total_notes,
        repos,
    }))
}

/// POST /companies/:id/vault/ingestion/cancel — stop the loop after the current repo.
async fn ingestion_cancel(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(company_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_company_owner(&state.db, auth.user_id, company_id).await?;
    crate::services::onboarding::ingest::request_cancel(company_id);
    Ok(Json(serde_json::json!({ "status": "cancelling" })))
}

// ── Notes CRUD / graph / timeline (mirrors routes/knowledge.rs, company-scoped) ─────────────

#[derive(Serialize)]
struct NoteView {
    id: String,
    slug: String,
    title: String,
    layer: String,
    source_type: String,
    visibility: String,
    domain: Option<String>,
    related: Vec<String>,
    repo: Option<String>,
}

fn note_view(n: &VaultNote) -> NoteView {
    NoteView {
        id: n.id.to_string(),
        slug: n.slug.clone(),
        title: n.title.clone(),
        layer: n.layer.as_str().to_string(),
        source_type: n.source_type.as_str().to_string(),
        visibility: n.visibility.as_str().to_string(),
        domain: n.domain.clone(),
        related: n.related.clone(),
        repo: n.repo.clone(),
    }
}

async fn list_notes(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(company_id): Path<Uuid>,
) -> AppResult<Json<Vec<NoteView>>> {
    authz::require_company_owner(&state.db, auth.user_id, company_id).await?;
    let notes = vault::list_notes_for_owner(&state.db, owner(company_id))
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(notes.iter().map(note_view).collect()))
}

#[derive(Serialize)]
struct NoteDetail {
    #[serde(flatten)]
    meta: NoteView,
    body: String,
}

async fn get_note(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((company_id, slug)): Path<(Uuid, String)>,
) -> AppResult<Json<NoteDetail>> {
    authz::require_company_owner(&state.db, auth.user_id, company_id).await?;
    let note = vault::get_note(&state.db, owner(company_id), &slug)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("Note not found".into()))?;
    Ok(Json(NoteDetail {
        meta: note_view(&note),
        body: note.body,
    }))
}

#[derive(Deserialize)]
struct PutNoteBody {
    title: String,
    body: String,
    layer: Option<String>,
    domain: Option<String>,
    related: Option<Vec<String>>,
}

async fn put_note(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((company_id, slug)): Path<(Uuid, String)>,
    Json(body): Json<PutNoteBody>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_company_owner(&state.db, auth.user_id, company_id).await?;
    let existing = vault::get_note(&state.db, owner(company_id), &slug)
        .await
        .map_err(AppError::Internal)?;
    let (id, source_type) = match &existing {
        Some(n) => (n.id, n.source_type.clone()),
        None => (Uuid::new_v4(), KnowledgeSourceType::ApprovedOutput),
    };

    let mut related = body.related.unwrap_or_default();
    for link in vault::extract_wikilinks(&body.body) {
        if !related.contains(&link) {
            related.push(link);
        }
    }

    let note = VaultNote {
        id,
        agent_id: company_id,
        owner_kind: "company".into(),
        slug,
        title: body.title,
        body: body.body,
        layer: body
            .layer
            .as_deref()
            .map(NoteLayer::from_str)
            .unwrap_or(NoteLayer::Note),
        source_type,
        // Company notes are private to the company by default (proprietary).
        visibility: ChunkVisibility::Private,
        domain: body.domain,
        related,
        repo: existing.and_then(|n| n.repo),
        job_id: None,
    };
    vault::upsert_note(&state.db, &state.qdrant, &state.inference, note)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "status": "saved" })))
}

async fn delete_note(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((company_id, slug)): Path<(Uuid, String)>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_company_owner(&state.db, auth.user_id, company_id).await?;
    vault::delete_note(&state.db, &state.qdrant, owner(company_id), &slug)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "status": "deleted" })))
}

async fn reindex(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(company_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_company_owner(&state.db, auth.user_id, company_id).await?;
    let reindexed = vault::reindex_owner(
        &state.db,
        &state.qdrant,
        &state.inference,
        owner(company_id),
    )
    .await
    .map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "reindexed": reindexed })))
}

async fn repair_links(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(company_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_company_owner(&state.db, auth.user_id, company_id).await?;
    let links_repaired = vault::repair_related_links(&state.db, owner(company_id))
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(
        serde_json::json!({ "links_repaired": links_repaired }),
    ))
}

/// GET /companies/:id/vault/graph — the company's own List ⇄ Graph view (un-anonymized).
async fn graph(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(company_id): Path<Uuid>,
) -> AppResult<Json<vault::VaultGraph>> {
    authz::require_company_owner(&state.db, auth.user_id, company_id).await?;
    let g = vault::owner_graph(&state.db, owner(company_id))
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(g))
}

#[derive(Serialize)]
struct TimelineEvent {
    id: String,
    chunk_id: String,
    event_type: String,
    source_type: String,
    summary: Option<String>,
    created_at: String,
}

async fn timeline(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(company_id): Path<Uuid>,
) -> AppResult<Json<Vec<TimelineEvent>>> {
    authz::require_company_owner(&state.db, auth.user_id, company_id).await?;
    let rows = sqlx::query!(
        r#"SELECT id, chunk_id, event_type, source_type, summary, created_at
           FROM knowledge_events WHERE owner_kind = 'company' AND agent_id = $1
           ORDER BY created_at DESC"#,
        company_id
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| TimelineEvent {
                id: r.id.to_string(),
                chunk_id: r.chunk_id.to_string(),
                event_type: r.event_type,
                source_type: r.source_type,
                summary: r.summary,
                created_at: r.created_at.to_rfc3339(),
            })
            .collect(),
    ))
}
