use axum::{
    extract::{Path, State},
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::services::authz;
use crate::services::knowledge::{
    self, list_chunks_for_agent, ChunkVisibility, KnowledgeSourceType,
};
use crate::services::vault::{self, NoteLayer, VaultNote, VaultOwner};
use crate::{
    auth_extractor::AuthUser,
    errors::{AppError, AppResult},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/agent/:agent_id", get(list_for_agent))
        .route("/agent/:agent_id/resume", post(set_resume))
        .route("/agent/:agent_id/portfolio", post(upload_portfolio))
        .route("/agent/:agent_id/timeline", get(timeline))
        .route("/chunk/:chunk_id/visibility", put(set_visibility))
        .route("/chunk/:chunk_id", axum::routing::delete(delete_chunk))
        // ── Vault (markdown-source-of-truth) ──────────────────────────────
        .route("/agent/:agent_id/notes", get(list_notes))
        .route("/agent/:agent_id/reindex", post(reindex_agent_route))
        .route("/agent/:agent_id/repair-links", post(repair_links_route))
        .route("/agent/:agent_id/graph", get(vault_graph))
        .route(
            "/agent/:agent_id/note/:slug",
            get(get_note_route)
                .put(put_note_route)
                .delete(delete_note_route),
        )
}

#[derive(Serialize)]
struct KnowledgeListResponse {
    chunks: Vec<KnowledgeChunkView>,
    total: usize,
}

#[derive(Serialize)]
struct KnowledgeChunkView {
    id: String,
    source_type: String,
    content_preview: String,
    job_id: Option<String>,
    visibility: String,
}

async fn list_for_agent(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<KnowledgeListResponse>> {
    // The full KB (including private chunks) is owner-only. Companies see only the
    // shared snapshot via the applicants endpoint.
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let chunks = list_chunks_for_agent(&state.qdrant, agent_id)
        .await
        .map_err(AppError::Internal)?;

    let views: Vec<KnowledgeChunkView> = chunks
        .into_iter()
        .map(|c| KnowledgeChunkView {
            id: c.id.to_string(),
            // Canonical snake_case ("approved_output"), NOT the Debug form ("approvedoutput")
            // — the latter broke the frontend source_type filter + breakdown counts.
            source_type: c.source_type.as_str().to_string(),
            content_preview: c.content.chars().take(200).collect::<String>()
                + if c.content.chars().count() > 200 {
                    "…"
                } else {
                    ""
                },
            job_id: c.job_id.map(|id| id.to_string()),
            visibility: match c.visibility {
                ChunkVisibility::Shareable => "shareable".into(),
                ChunkVisibility::Private => "private".into(),
            },
        })
        .collect();

    let total = views.len();
    Ok(Json(KnowledgeListResponse {
        chunks: views,
        total,
    }))
}

#[derive(Deserialize)]
struct SetResumeBody {
    resume_text: String,
}

/// POST /knowledge/agent/:agent_id/resume — store/replace the agent owner's resume as a
/// shareable Resume KB chunk. Captured during onboarding and reused on applications.
async fn set_resume(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
    Json(body): Json<SetResumeBody>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    if body.resume_text.trim().is_empty() {
        return Err(AppError::BadRequest("Resume text is empty".into()));
    }

    // The resume is a single canonical, shareable note. upsert_note (keyed on agent+slug)
    // overwrites in place — preserving the note/chunk id — so there's no stale resume chunk
    // to clean up and the derived index re-derives automatically.
    let note = VaultNote {
        id: Uuid::new_v4(),
        agent_id,
        owner_kind: "agent".into(),
        slug: "resume".into(),
        title: "Resume".into(),
        body: body.resume_text.clone(),
        layer: NoteLayer::Note,
        source_type: KnowledgeSourceType::Resume,
        visibility: ChunkVisibility::Shareable,
        domain: None,
        related: vec![],
        repo: None,
        job_id: None,
    };
    vault::upsert_note(&state.db, &state.qdrant, &state.inference, note)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(serde_json::json!({ "status": "saved" })))
}

/// POST /knowledge/agent/:agent_id/portfolio — upload a portfolio/resume/case-study FILE (PDF, txt,
/// or markdown). The text is extracted and distilled into a vault note in the professional's voice,
/// the same document lane Google Docs ingestion uses. This is how non-engineers (and engineers with
/// design docs) enrich their agent beyond code. Multipart form field `file`.
async fn upload_portfolio(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
    mut multipart: axum::extract::Multipart,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;

    // Pull the first file field.
    let mut filename = String::from("portfolio");
    let mut bytes: Option<axum::body::Bytes> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("bad upload: {e}")))?
    {
        if field.name() == Some("file") {
            if let Some(fname) = field.file_name() {
                filename = fname.to_string();
            }
            bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("failed to read file: {e}")))?,
            );
            break;
        }
    }
    let bytes = bytes.ok_or_else(|| AppError::BadRequest("no 'file' field in upload".into()))?;

    // Extract text: PDF via pdf-extract (CPU-bound → spawn_blocking), else treat as UTF-8 text.
    let is_pdf = filename.to_lowercase().ends_with(".pdf") || bytes.starts_with(b"%PDF");
    let text = if is_pdf {
        let data = bytes.to_vec();
        tokio::task::spawn_blocking(move || pdf_extract::extract_text_from_mem(&data))
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("pdf task join: {e}")))?
            .map_err(|e| AppError::BadRequest(format!("could not read PDF: {e}")))?
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    };
    if text.trim().is_empty() {
        return Err(AppError::BadRequest(
            "No text could be extracted from the file.".into(),
        ));
    }

    // A nice title from the filename (strip extension).
    let title = filename
        .rsplit_once('.')
        .map(|(base, _)| base)
        .unwrap_or(&filename)
        .replace(['_', '-'], " ");

    // Distill into a vault note via the shared document ingester (source label "portfolio").
    let doc = crate::services::onboarding::documents::IngestedDoc { title, text };
    let principal = sqlx::query!(
        "SELECT persona_system_prompt, cf_finetune_id FROM agents WHERE id = $1",
        agent_id
    )
    .fetch_optional(&state.db)
    .await?
    .map(|r| {
        (
            r.persona_system_prompt.unwrap_or_default(),
            r.cf_finetune_id,
        )
    });

    let written = crate::services::onboarding::documents::ingest_documents(
        &state.db,
        &state.qdrant,
        &state.inference,
        VaultOwner::Agent(agent_id),
        principal,
        "portfolio",
        vec![doc],
    )
    .await
    .map_err(AppError::Internal)?;

    Ok(Json(serde_json::json!({ "notes_written": written })))
}

#[derive(Deserialize)]
struct SetVisibilityBody {
    shareable: bool,
}

/// PUT /knowledge/chunk/:chunk_id/visibility — owner toggles whether a KB chunk may be
/// exposed to companies via job applications.
async fn set_visibility(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(chunk_id): Path<Uuid>,
    Json(body): Json<SetVisibilityBody>,
) -> AppResult<Json<serde_json::Value>> {
    // The chunk must belong to one of the caller's agents.
    let agent_id = knowledge::chunk_agent_id(&state.qdrant, chunk_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("Chunk not found".into()))?;
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;

    let visibility = if body.shareable {
        ChunkVisibility::Shareable
    } else {
        ChunkVisibility::Private
    };
    knowledge::set_chunk_visibility(&state.qdrant, chunk_id, visibility)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "status": "updated" })))
}

/// DELETE /knowledge/chunk/:chunk_id — owner removes a single KB item. Records a
/// `deleted` timeline event that survives the chunk.
async fn delete_chunk(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(chunk_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    // Resolve the chunk (and its agent/type) before deleting, for ownership + the event.
    let chunk = knowledge::fetch_chunk(&state.qdrant, chunk_id)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| AppError::NotFound("Chunk not found".into()))?;
    authz::require_agent_owner(&state.db, auth.user_id, chunk.agent_id).await?;

    knowledge::delete_chunk(&state.qdrant, chunk_id)
        .await
        .map_err(AppError::Internal)?;
    let _ = knowledge::record_event(
        &state.db,
        VaultOwner::Agent(chunk.agent_id),
        chunk_id,
        "deleted",
        &chunk.source_type,
        &chunk.content,
    )
    .await;

    Ok(Json(serde_json::json!({ "status": "deleted" })))
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

/// GET /knowledge/agent/:agent_id/timeline — chronological add/delete history.
async fn timeline(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<Vec<TimelineEvent>>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let rows = sqlx::query!(
        r#"SELECT id, chunk_id, event_type, source_type, summary, created_at
           FROM knowledge_events WHERE owner_kind = 'agent' AND agent_id = $1 ORDER BY created_at DESC"#,
        agent_id
    )
    .fetch_all(&state.db)
    .await?;

    let events = rows
        .into_iter()
        .map(|r| TimelineEvent {
            id: r.id.to_string(),
            chunk_id: r.chunk_id.to_string(),
            event_type: r.event_type,
            source_type: r.source_type,
            summary: r.summary,
            created_at: r.created_at.to_rfc3339(),
        })
        .collect();
    Ok(Json(events))
}

// ── Vault (markdown-source-of-truth) endpoints ──────────────────────────────────────────────

#[derive(Serialize)]
struct NoteView {
    /// Note id — also the derived Qdrant chunk id (1:1), so the visibility toggle endpoint
    /// (`PUT /knowledge/chunk/:id/visibility`) accepts it directly.
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

/// GET /knowledge/agent/:agent_id/notes — list the agent's vault notes (metadata only).
async fn list_notes(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<Vec<NoteView>>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let notes = vault::list_notes_for_owner(&state.db, VaultOwner::Agent(agent_id))
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

/// GET /knowledge/agent/:agent_id/note/:slug — read one full note (whole, coherent document).
async fn get_note_route(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((agent_id, slug)): Path<(Uuid, String)>,
) -> AppResult<Json<NoteDetail>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let note = vault::get_note(&state.db, VaultOwner::Agent(agent_id), &slug)
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
    shareable: Option<bool>,
}

/// PUT /knowledge/agent/:agent_id/note/:slug — human edits a note. The edit is a gold-standard
/// write: the derived Qdrant index re-derives automatically via upsert_note.
async fn put_note_route(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((agent_id, slug)): Path<(Uuid, String)>,
    Json(body): Json<PutNoteBody>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;

    // Preserve the existing note's id + source_type when editing in place.
    let existing = vault::get_note(&state.db, VaultOwner::Agent(agent_id), &slug)
        .await
        .map_err(AppError::Internal)?;
    let (id, source_type, prev_vis) = match &existing {
        Some(n) => (n.id, n.source_type.clone(), n.visibility),
        None => (
            Uuid::new_v4(),
            KnowledgeSourceType::ApprovedOutput,
            ChunkVisibility::Private,
        ),
    };
    let visibility = match body.shareable {
        Some(true) => ChunkVisibility::Shareable,
        Some(false) => ChunkVisibility::Private,
        None => prev_vis,
    };

    // Merge any explicit `related` array with `[[wikilinks]]` found in the body, so a hand-edited
    // note's inline links become graph edges (the Obsidian-style behavior) without a separate field.
    let mut related = body.related.unwrap_or_default();
    for link in vault::extract_wikilinks(&body.body) {
        if !related.contains(&link) {
            related.push(link);
        }
    }

    let note = VaultNote {
        id,
        agent_id,
        owner_kind: "agent".into(),
        slug,
        title: body.title,
        body: body.body,
        layer: body
            .layer
            .as_deref()
            .map(NoteLayer::from_str)
            .unwrap_or(NoteLayer::Note),
        source_type,
        visibility,
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

/// DELETE /knowledge/agent/:agent_id/note/:slug — remove a note (source of truth) and its derived
/// Qdrant chunk in one step. Owner-only.
async fn delete_note_route(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((agent_id, slug)): Path<(Uuid, String)>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    vault::delete_note(&state.db, &state.qdrant, VaultOwner::Agent(agent_id), &slug)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(serde_json::json!({ "status": "deleted" })))
}

/// POST /knowledge/agent/:agent_id/reindex — rebuild the derived Qdrant index from the vault
/// (the rebuildable-index guarantee). Backfills from legacy Qdrant-only chunks if the vault is
/// empty.
async fn reindex_agent_route(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let backfilled =
        vault::backfill_from_qdrant(&state.db, &state.qdrant, &state.inference, agent_id)
            .await
            .map_err(AppError::Internal)?;
    let reindexed = vault::reindex_owner(
        &state.db,
        &state.qdrant,
        &state.inference,
        VaultOwner::Agent(agent_id),
    )
    .await
    .map_err(AppError::Internal)?;
    Ok(Json(
        serde_json::json!({ "backfilled": backfilled, "reindexed": reindexed }),
    ))
}

/// POST /knowledge/agent/:agent_id/repair-links — cheap, Postgres-only fix: normalize `related[]`
/// (strip `[[wikilink]]` wrappers → bare slugs) so graph edges resolve. Does NOT re-embed, touch
/// Qdrant, or run any inference — unlike /reindex. Idempotent.
async fn repair_links_route(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let links_repaired = vault::repair_related_links(&state.db, VaultOwner::Agent(agent_id))
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(
        serde_json::json!({ "links_repaired": links_repaired }),
    ))
}

#[derive(Serialize)]
struct GraphNode {
    slug: String,
    title: String,
    layer: String,
    domain: Option<String>,
    /// Full note body — the owner sees their own complete vault (no anonymization), so the graph
    /// can open each note inline. Always false `locked` here (this endpoint is owner-only).
    body: String,
    locked: bool,
}

#[derive(Serialize)]
struct GraphEdge {
    source: String,
    target: String,
}

#[derive(Serialize)]
struct VaultGraph {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
}

/// GET /knowledge/agent/:agent_id/graph — the Obsidian-style graph view feed: nodes from
/// vault_notes, edges from `related[]`. Color by `layer`/`domain` client-side.
async fn vault_graph(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<VaultGraph>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let notes = vault::list_notes_for_owner(&state.db, VaultOwner::Agent(agent_id))
        .await
        .map_err(AppError::Internal)?;
    let slugs: std::collections::HashSet<&str> = notes.iter().map(|n| n.slug.as_str()).collect();

    let nodes = notes
        .iter()
        .map(|n| GraphNode {
            slug: n.slug.clone(),
            title: n.title.clone(),
            layer: n.layer.as_str().to_string(),
            domain: n.domain.clone(),
            body: n.body.clone(),
            locked: false,
        })
        .collect();
    // Edges from related[]; only emit edges whose target slug actually exists (drop dangling
    // links so the graph stays clean). Normalize each ref (strip `[[wikilink]]` wrappers) before
    // matching, so the graph renders correctly even for notes stored before write-time
    // normalization existed — no manual repair needed just to view the graph.
    let edges = notes
        .iter()
        .flat_map(|n| {
            n.related
                .iter()
                .map(|t| vault::normalize_ref(t))
                .filter(|t| slugs.contains(t.as_str()) && t != &n.slug)
                .map(|t| GraphEdge {
                    source: n.slug.clone(),
                    target: t,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    Ok(Json(VaultGraph { nodes, edges }))
}
