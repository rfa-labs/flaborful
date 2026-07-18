//! Document-based likeness ingestion — the non-code counterpart to the repo-clone ingester in
//! `ingest.rs`. Where `ingest.rs` clones a git repo and lets a coding agent read the filesystem,
//! this fetches **text documents** from a connected source (Google Docs/Drive, and later
//! Notion/Slack/…) via its API and distills each into vault notes. This is what lets flaborful
//! ingest a designer's/PM's/analyst's likeness, not just an engineer's code.
//!
//! Shape:
//!   1. A per-source **fetcher** returns `IngestedDoc`s (title + text) using the owner's stored
//!      connection token.
//!   2. `ingest_documents` distills each doc into a vault note (one LLM pass, in the professional's
//!      voice when a persona is available) and writes it via `vault::upsert_note`, streaming
//!      progress into `ingestion_events` for the same live onboarding feed the repo ingester uses.
//!
//! GitHub stays on the repo-clone path; this trait-free module is the generic document lane.

use anyhow::{anyhow, Result};
use sqlx::PgPool;
use uuid::Uuid;

use crate::services::inference::InferenceClient;
use crate::services::knowledge::{ChunkVisibility, KnowledgeSourceType};
use crate::services::vault::{self, NoteLayer, VaultNote, VaultOwner};
use qdrant_client::Qdrant;

/// A fetched document from some source, ready to distill into a vault note. The source label is
/// carried separately by `ingest_documents` (all docs in a batch share one source).
pub struct IngestedDoc {
    pub title: String,
    pub text: String,
}

/// Record a lifecycle event into `ingestion_events` (same table the repo ingester streams to, so
/// the onboarding UI's live feed works uniformly). `repo` doubles as the source label here.
async fn event(
    db: &PgPool,
    owner: VaultOwner,
    source: &str,
    kind: &str,
    status: Option<&str>,
    detail: &str,
    notes: i32,
) {
    let _ = sqlx::query!(
        "INSERT INTO ingestion_events (owner_kind, agent_id, repo, kind, status, detail, notes_count)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        owner.kind_str(),
        owner.id(),
        source,
        kind,
        status,
        detail,
        notes,
    )
    .execute(db)
    .await;
}

/// A kebab-case slug from a title, prefixed by source so a doc note can't collide with a repo note.
fn doc_slug(source: &str, title: &str) -> String {
    let base = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let cleaned = base.split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-");
    let short = if cleaned.len() > 60 { &cleaned[..60] } else { &cleaned };
    format!("{source}-{}", if short.is_empty() { "untitled" } else { short })
}

/// Distill fetched docs into vault notes for the owner. Best-effort per doc. `principal` is the
/// (persona, lora) used to write in the professional's voice; None for a plain summary. Returns the
/// number of notes written. Streams progress into `ingestion_events`.
pub async fn ingest_documents(
    db: &PgPool,
    qdrant: &Qdrant,
    inference: &InferenceClient,
    owner: VaultOwner,
    principal: Option<(String, Option<String>)>,
    source: &str,
    docs: Vec<IngestedDoc>,
) -> Result<usize> {
    let total = docs.len();
    event(
        db,
        owner,
        source,
        "status",
        Some("running"),
        &format!("Ingesting {total} document(s) from {source}"),
        0,
    )
    .await;

    let (persona, lora) = principal.unwrap_or((String::new(), None));
    let mut written = 0i32;
    for doc in docs {
        if doc.text.trim().is_empty() {
            continue;
        }
        let system = format!(
            "{persona}\n\nYou are documenting the professional's own work artifact into their \
             knowledge vault. Distill the document below into ONE concise markdown note that \
             captures what it shows about how they think and work (their approach, standards, \
             recurring choices) — not a verbatim copy. Start with a one-line summary. Return only \
             the note body (markdown)."
        );
        let user = format!("# {}\n\n{}", doc.title, truncate(&doc.text, 6000));
        let body = match inference.worker(&system, &user, lora.as_deref()).await {
            Ok(b) if !b.trim().is_empty() => b,
            _ => truncate(&doc.text, 2000).to_string(), // degrade to raw excerpt rather than skip
        };

        let slug = doc_slug(source, &doc.title);
        // Preserve id/visibility if this doc was ingested before (idempotent re-ingest).
        let existing = vault::get_note(db, owner, &slug).await.ok().flatten();
        let (id, visibility) = match &existing {
            Some(n) => (n.id, n.visibility),
            None => (Uuid::new_v4(), ChunkVisibility::Private),
        };
        let note = VaultNote {
            id,
            agent_id: owner.id(),
            owner_kind: owner.kind_str().to_string(),
            slug: slug.clone(),
            title: doc.title.clone(),
            body,
            // Portfolio-style artifacts document features/approach; use the Feature layer so they
            // sit alongside code-derived feature notes in retrieval + the graph.
            layer: NoteLayer::Feature,
            source_type: KnowledgeSourceType::Portfolio,
            visibility,
            domain: None,
            related: vec![],
            repo: Some(source.to_string()),
            job_id: None,
        };
        if vault::upsert_note(db, qdrant, inference, note).await.is_ok() {
            written += 1;
            event(db, owner, source, "note", None, &format!("[note] {}", doc.title), written).await;
        }
    }

    let count = doc_note_count(db, owner, source).await;
    event(
        db,
        owner,
        source,
        "status",
        Some("done"),
        &format!("Ingested {count} note(s) from {source}"),
        count,
    )
    .await;
    Ok(written as usize)
}

async fn doc_note_count(db: &PgPool, owner: VaultOwner, source: &str) -> i32 {
    sqlx::query_scalar!(
        "SELECT COUNT(*) FROM vault_notes WHERE owner_kind = $1 AND agent_id = $2 AND repo = $3",
        owner.kind_str(),
        owner.id(),
        source,
    )
    .fetch_one(db)
    .await
    .ok()
    .flatten()
    .unwrap_or(0) as i32
}

// ── Google Docs / Drive source ──────────────────────────────────────────────────────────────────

const DRIVE_FILES: &str = "https://www.googleapis.com/drive/v3/files";

#[derive(serde::Deserialize)]
struct DriveList {
    files: Option<Vec<DriveFile>>,
}
#[derive(serde::Deserialize)]
struct DriveFile {
    id: String,
    name: String,
}

/// Fetch the owner's own Google Docs and export each as plain text. Capped so onboarding stays
/// bounded. Uses the Drive `files.list` (Docs mime) + `files.export` endpoints with the owner's
/// connection token.
pub async fn fetch_google_docs(access_token: &str, max_docs: usize) -> Result<Vec<IngestedDoc>> {
    let http = reqwest::Client::new();
    // List the user's Google Docs, most recently modified first, excluding trashed.
    let list: DriveList = http
        .get(DRIVE_FILES)
        .bearer_auth(access_token)
        .query(&[
            ("q", "mimeType='application/vnd.google-apps.document' and trashed=false"),
            ("orderBy", "modifiedTime desc"),
            ("pageSize", "25"),
            ("fields", "files(id,name)"),
        ])
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow!("Drive files.list failed: {e}"))?
        .json()
        .await?;

    let files = list.files.unwrap_or_default();
    let mut docs = Vec::new();
    for f in files.into_iter().take(max_docs) {
        // Export the doc as plain text.
        let text = http
            .get(format!("{DRIVE_FILES}/{}/export", f.id))
            .bearer_auth(access_token)
            .query(&[("mimeType", "text/plain")])
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .ok();
        if let Some(resp) = text {
            if let Ok(body) = resp.text().await {
                docs.push(IngestedDoc {
                    title: f.name,
                    text: body,
                });
            }
        }
    }
    Ok(docs)
}

fn truncate(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    let mut idx = max_chars;
    while !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}
