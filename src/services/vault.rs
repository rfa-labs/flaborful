//! The professional's knowledge vault — **markdown notes are the source of truth**.
//!
//! Storage model (retrieval-optimized for agentic work):
//!   - Postgres `vault_notes` = source of truth. Raw markdown in `body`; frontmatter as
//!     typed, indexable columns (`layer`, `domain`, `visibility`, `source_type`, `related[]`).
//!   - Qdrant `flaborful_knowledge` = DERIVED semantic index, rebuilt from `vault_notes`. A
//!     note's id IS its chunk id (1:1), so application-snapshot sharing keeps resolving.
//!   - Postgres `tsvector` (GIN) = keyword/BM25 half of hybrid retrieval.
//!   - `related[]` = explicit `[[wikilink]]` graph edges (Graph-RAG one-hop + graph view).
//!   - R2 `.md` mirror = human/export layer only (Obsidian-open / git-backup), never queried.
//!
//! The agent reads from the vault (blueprint + browse tools) and writes back to it
//! (code-ingestion notes, rolling MEMORY.md). Every durable write records a
//! `knowledge_events` timeline entry, exactly as the chunk path did.

use anyhow::{anyhow, Result};
use qdrant_client::Qdrant;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashSet;
use uuid::Uuid;

use crate::services::inference::InferenceClient;
use crate::services::knowledge::{self, ChunkVisibility, KnowledgeChunk, KnowledgeSourceType};

/// Typed note layer. Drives metadata-filtered retrieval (e.g. only golden_snippets) and the
/// blueprint's section assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteLayer {
    /// Interview-derived standing preferences (communication / reasoning style).
    StyleRule,
    /// A full exemplary function/class block from the professional's best work.
    GoldenSnippet,
    /// The professional's architectural patterns / decisions.
    Architecture,
    /// A discrete feature/subsystem documented from a repo.
    Feature,
    /// A shared pattern/idiom, cross-linked across repos.
    Pattern,
    /// A technology/stack note linking the repos/features that use it.
    Tech,
    /// Anything else (the default; backfilled chunks, rolling memory).
    Note,
}

impl NoteLayer {
    pub fn as_str(&self) -> &'static str {
        match self {
            NoteLayer::StyleRule => "style_rule",
            NoteLayer::GoldenSnippet => "golden_snippet",
            NoteLayer::Architecture => "architecture",
            NoteLayer::Feature => "feature",
            NoteLayer::Pattern => "pattern",
            NoteLayer::Tech => "tech",
            NoteLayer::Note => "note",
        }
    }
    /// Parse a layer string; unknown → Note (forward-compatible).
    pub fn from_str(s: &str) -> Self {
        match s {
            "style_rule" => NoteLayer::StyleRule,
            "golden_snippet" => NoteLayer::GoldenSnippet,
            "architecture" => NoteLayer::Architecture,
            "feature" => NoteLayer::Feature,
            "pattern" => NoteLayer::Pattern,
            "tech" => NoteLayer::Tech,
            _ => NoteLayer::Note,
        }
    }
}

/// The fixed domain taxonomy. Validated app-side so retrieval filters stay clean. Unknown
/// values are coerced to `General` rather than rejected (forgiving ingestion).
pub const DOMAINS: &[&str] = &[
    "backend", "frontend", "infra", "testing", "product", "general",
];

/// Coerce an arbitrary domain string to the fixed taxonomy. None stays None.
pub fn normalize_domain(domain: Option<&str>) -> Option<String> {
    let d = domain?.trim().to_lowercase();
    if d.is_empty() {
        return None;
    }
    if DOMAINS.contains(&d.as_str()) {
        Some(d)
    } else {
        Some("general".to_string())
    }
}

/// A vault note — the source-of-truth unit. `id` doubles as the derived Qdrant chunk id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultNote {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub slug: String,
    pub title: String,
    pub body: String,
    pub layer: NoteLayer,
    pub source_type: KnowledgeSourceType,
    pub visibility: ChunkVisibility,
    pub domain: Option<String>,
    pub related: Vec<String>,
    pub repo: Option<String>,
    pub job_id: Option<Uuid>,
}

impl VaultNote {
    /// The text embedded + made searchable: title + body (so a title-only match still hits).
    fn searchable_text(&self) -> String {
        format!("{}\n\n{}", self.title, self.body)
    }
}

// ── Frontmatter (markdown <-> note), hand-rolled (no serde_yaml dep) ────────────────────────

/// Render a note as an Obsidian-style markdown file: YAML frontmatter block + body. This is
/// the human-editable / R2-mirror form.
pub fn to_markdown(note: &VaultNote) -> String {
    let related = note
        .related
        .iter()
        .map(|s| format!("\"[[{}]]\"", s))
        .collect::<Vec<_>>()
        .join(", ");
    let mut fm = String::from("---\n");
    fm.push_str(&format!("title: {}\n", note.title));
    fm.push_str(&format!("slug: {}\n", note.slug));
    fm.push_str(&format!("layer: {}\n", note.layer.as_str()));
    fm.push_str(&format!("source_type: {}\n", note.source_type.as_str()));
    fm.push_str(&format!("visibility: {}\n", note.visibility.as_str()));
    if let Some(d) = &note.domain {
        fm.push_str(&format!("domain: {}\n", d));
    }
    if let Some(r) = &note.repo {
        fm.push_str(&format!("repo: {}\n", r));
    }
    fm.push_str(&format!("related: [{}]\n", related));
    fm.push_str("---\n\n");
    fm.push_str(&note.body);
    fm
}

/// Extract `[[wikilink]]` slugs from a `related: [...]` frontmatter line or body text.
pub fn extract_wikilinks(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(open) = rest.find("[[") {
        rest = &rest[open + 2..];
        if let Some(close) = rest.find("]]") {
            let link = rest[..close].trim();
            if !link.is_empty() {
                out.push(link.to_string());
            }
            rest = &rest[close + 2..];
        } else {
            break;
        }
    }
    out
}

// ── CRUD over vault_notes ───────────────────────────────────────────────────────────────────

/// Normalize a `related` reference to a bare slug. Agents inconsistently pass links as either the
/// bare slug (`ums-auth-dpop`) or an Obsidian wikilink (`[[ums-auth-dpop]]`); the latter never
/// matched a real slug so those graph edges were silently dropped. Strip the `[[ ]]` and trim so
/// the stored value always matches a real slug. Public so the graph endpoint can normalize at
/// read time too (robust for pre-normalization vaults).
pub fn normalize_ref(s: &str) -> String {
    s.trim()
        .trim_start_matches("[[")
        .trim_end_matches("]]")
        .trim()
        .to_string()
}

/// Upsert a note (source of truth), then derive its Qdrant chunk + mirror to R2 + log the
/// timeline event. The single write path; all callers route through here.
pub async fn upsert_note(
    db: &PgPool,
    qdrant: &Qdrant,
    inference: &InferenceClient,
    mut note: VaultNote,
) -> Result<VaultNote> {
    // Clean related-links: strip wikilink wrappers, drop empties + self-links + duplicates.
    let mut seen = std::collections::HashSet::new();
    note.related = note
        .related
        .iter()
        .map(|r| normalize_ref(r))
        .filter(|r| !r.is_empty() && r != &note.slug && seen.insert(r.clone()))
        .collect();

    let domain = normalize_domain(note.domain.as_deref());
    sqlx::query!(
        r#"INSERT INTO vault_notes
             (id, agent_id, slug, title, body, layer, source_type, visibility, domain, related, repo, job_id, updated_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12, NOW())
           ON CONFLICT (agent_id, slug) DO UPDATE SET
             title = EXCLUDED.title, body = EXCLUDED.body, layer = EXCLUDED.layer,
             source_type = EXCLUDED.source_type, visibility = EXCLUDED.visibility,
             domain = EXCLUDED.domain, related = EXCLUDED.related, repo = EXCLUDED.repo,
             job_id = EXCLUDED.job_id, updated_at = NOW()"#,
        note.id,
        note.agent_id,
        note.slug,
        note.title,
        note.body,
        note.layer.as_str(),
        note.source_type.as_str(),
        note.visibility.as_str(),
        domain.as_deref(),
        &note.related,
        note.repo.as_deref(),
        note.job_id,
    )
    .execute(db)
    .await?;

    // Re-read the canonical id (an upsert on an existing slug keeps the original id, which the
    // derived chunk must match).
    let canonical = get_note(db, note.agent_id, &note.slug)
        .await?
        .ok_or_else(|| anyhow!("vault: note vanished after upsert"))?;

    index_note(qdrant, inference, &canonical).await?;
    let _ = mirror_to_r2(&canonical).await;
    let _ = knowledge::record_event(
        db,
        canonical.agent_id,
        canonical.id,
        "added",
        &canonical.source_type,
        &canonical.title,
    )
    .await;
    Ok(canonical)
}

pub async fn get_note(db: &PgPool, agent_id: Uuid, slug: &str) -> Result<Option<VaultNote>> {
    let row = sqlx::query!(
        r#"SELECT id, agent_id, slug, title, body, layer, source_type, visibility,
                  domain, related, repo, job_id
           FROM vault_notes WHERE agent_id = $1 AND slug = $2"#,
        agent_id,
        slug,
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| VaultNote {
        id: r.id,
        agent_id: r.agent_id,
        slug: r.slug,
        title: r.title,
        body: r.body,
        layer: NoteLayer::from_str(&r.layer),
        source_type: KnowledgeSourceType::from_str(&r.source_type)
            .unwrap_or(KnowledgeSourceType::Portfolio),
        visibility: ChunkVisibility::from_str(&r.visibility),
        domain: r.domain,
        related: r.related,
        repo: r.repo,
        job_id: r.job_id,
    }))
}

pub async fn list_notes_for_agent(db: &PgPool, agent_id: Uuid) -> Result<Vec<VaultNote>> {
    let rows = sqlx::query!(
        r#"SELECT id, agent_id, slug, title, body, layer, source_type, visibility,
                  domain, related, repo, job_id
           FROM vault_notes WHERE agent_id = $1 ORDER BY updated_at DESC"#,
        agent_id,
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| VaultNote {
            id: r.id,
            agent_id: r.agent_id,
            slug: r.slug,
            title: r.title,
            body: r.body,
            layer: NoteLayer::from_str(&r.layer),
            source_type: KnowledgeSourceType::from_str(&r.source_type)
                .unwrap_or(KnowledgeSourceType::Portfolio),
            visibility: ChunkVisibility::from_str(&r.visibility),
            domain: r.domain,
            related: r.related,
            repo: r.repo,
            job_id: r.job_id,
        })
        .collect())
}

/// Fetch notes by slug (graph-hop / follow_links target resolution).
pub async fn notes_by_slugs(
    db: &PgPool,
    agent_id: Uuid,
    slugs: &[String],
) -> Result<Vec<VaultNote>> {
    if slugs.is_empty() {
        return Ok(vec![]);
    }
    let all = list_notes_for_agent(db, agent_id).await?;
    let want: HashSet<&str> = slugs.iter().map(|s| s.as_str()).collect();
    Ok(all
        .into_iter()
        .filter(|n| want.contains(n.slug.as_str()))
        .collect())
}

/// Delete a note (source of truth), its derived chunk, and log a timeline event.
pub async fn delete_note(db: &PgPool, qdrant: &Qdrant, agent_id: Uuid, slug: &str) -> Result<()> {
    let Some(note) = get_note(db, agent_id, slug).await? else {
        return Ok(());
    };
    sqlx::query!("DELETE FROM vault_notes WHERE id = $1", note.id)
        .execute(db)
        .await?;
    let _ = knowledge::delete_chunk(qdrant, note.id).await;
    let _ = knowledge::record_event(
        db,
        agent_id,
        note.id,
        "deleted",
        &note.source_type,
        &note.title,
    )
    .await;
    Ok(())
}

// ── Derive the Qdrant index from notes ──────────────────────────────────────────────────────

/// (Re)derive the single Qdrant chunk for one note. The note id is the chunk id, so this is an
/// idempotent overwrite. Body+title are embedded so a title match still retrieves.
pub async fn index_note(
    qdrant: &Qdrant,
    inference: &InferenceClient,
    note: &VaultNote,
) -> Result<()> {
    let text = note.searchable_text();
    let embeddings = inference.embed(vec![text.clone()]).await?;
    let chunk = KnowledgeChunk {
        id: note.id,
        agent_id: note.agent_id,
        job_id: note.job_id,
        content: note.body.clone(),
        source_type: note.source_type.clone(),
        visibility: note.visibility,
    };
    knowledge::upsert_chunks(qdrant, vec![chunk], embeddings).await
}

/// Rebuild the entire Qdrant index for an agent from `vault_notes` (the rebuildable-index
/// guarantee). Returns the number of notes (re)indexed.
pub async fn reindex_agent(
    db: &PgPool,
    qdrant: &Qdrant,
    inference: &InferenceClient,
    agent_id: Uuid,
) -> Result<usize> {
    let notes = list_notes_for_agent(db, agent_id).await?;
    for note in &notes {
        index_note(qdrant, inference, note).await?;
    }
    Ok(notes.len())
}

/// Repair `related[]` for all of an agent's existing notes: re-normalize each (strip `[[ ]]`
/// wikilink wrappers, drop empties/self/dupes) so stored links match real slugs and graph edges
/// resolve. Idempotent — safe to re-run. Returns how many notes had their links changed.
pub async fn repair_related_links(db: &PgPool, agent_id: Uuid) -> Result<usize> {
    let notes = list_notes_for_agent(db, agent_id).await?;
    let mut fixed = 0;
    for n in &notes {
        let mut seen = std::collections::HashSet::new();
        let cleaned: Vec<String> = n
            .related
            .iter()
            .map(|r| normalize_ref(r))
            .filter(|r| !r.is_empty() && r != &n.slug && seen.insert(r.clone()))
            .collect();
        if cleaned != n.related {
            sqlx::query!(
                "UPDATE vault_notes SET related = $1, updated_at = NOW() WHERE id = $2",
                &cleaned,
                n.id,
            )
            .execute(db)
            .await?;
            fixed += 1;
        }
    }
    Ok(fixed)
}

// ── Hybrid + graph retrieval ────────────────────────────────────────────────────────────────

/// A retrieval filter over the typed note columns (the metadata-filtered-retrieval lever).
#[derive(Debug, Default, Clone)]
pub struct RetrievalFilter {
    pub layer: Option<NoteLayer>,
    pub domain: Option<String>,
    /// When true, exclude Private notes (company-facing paths). Default false (owner/agent).
    pub shareable_only: bool,
}

/// A retrieved note plus a coarse relevance score (for ordering / blueprint assembly).
#[derive(Debug, Clone)]
pub struct RetrievedNote {
    pub note: VaultNote,
    pub score: f32,
}

/// Hybrid retrieval: semantic (Qdrant) ⊕ keyword (Postgres tsvector), merged ~70/30, then one
/// `related:` graph hop. Visibility + metadata filters are enforced server-side here.
pub async fn retrieve(
    db: &PgPool,
    qdrant: &Qdrant,
    inference: &InferenceClient,
    agent_id: Uuid,
    query: &str,
    filter: &RetrievalFilter,
    k: usize,
) -> Result<Vec<RetrievedNote>> {
    use std::collections::HashMap;
    let mut scored: HashMap<Uuid, f32> = HashMap::new();

    // Semantic half (vector). Over-fetch so post-filtering still yields k.
    let embeddings = inference.embed(vec![query.to_string()]).await?;
    if let Some(qv) = embeddings.into_iter().next() {
        let chunks = knowledge::search_chunks(qdrant, qv, agent_id, (k * 3) as u64, 0.5).await?;
        // Qdrant returns in descending similarity; approximate a rank-decay score.
        for (rank, c) in chunks.iter().enumerate() {
            let s = 0.70 * (1.0 - (rank as f32) / (chunks.len().max(1) as f32));
            *scored.entry(c.id).or_insert(0.0) += s;
        }
    }

    // Keyword half (BM25-ish via ts_rank over the same rows).
    let kw = sqlx::query!(
        r#"SELECT id, ts_rank(tsv, plainto_tsquery('english', $2)) AS rank
           FROM vault_notes
           WHERE agent_id = $1 AND tsv @@ plainto_tsquery('english', $2)
           ORDER BY rank DESC LIMIT $3"#,
        agent_id,
        query,
        (k * 3) as i64,
    )
    .fetch_all(db)
    .await?;
    let max_rank = kw
        .iter()
        .filter_map(|r| r.rank)
        .fold(0.0_f32, f32::max)
        .max(1e-6);
    for r in &kw {
        let norm = r.rank.unwrap_or(0.0) / max_rank;
        *scored.entry(r.id).or_insert(0.0) += 0.30 * norm;
    }

    // Resolve to notes, apply metadata + visibility filters.
    let all = list_notes_for_agent(db, agent_id).await?;
    let by_id: std::collections::HashMap<Uuid, VaultNote> =
        all.into_iter().map(|n| (n.id, n)).collect();

    let passes = |n: &VaultNote| -> bool {
        if filter.shareable_only && n.visibility != ChunkVisibility::Shareable {
            return false;
        }
        if let Some(l) = filter.layer {
            if n.layer != l {
                return false;
            }
        }
        if let Some(d) = &filter.domain {
            if n.domain.as_deref() != Some(d.as_str()) {
                return false;
            }
        }
        true
    };

    let mut hits: Vec<RetrievedNote> = scored
        .into_iter()
        .filter_map(|(id, score)| {
            let n = by_id.get(&id)?.clone();
            if passes(&n) {
                Some(RetrievedNote { note: n, score })
            } else {
                None
            }
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(k);

    // One graph hop: pull `related:` notes of the surviving hits, append if not present and
    // they pass the same filters. Lightweight Graph-RAG.
    let present: HashSet<Uuid> = hits.iter().map(|h| h.note.id).collect();
    let related_slugs: Vec<String> = hits.iter().flat_map(|h| h.note.related.clone()).collect();
    if !related_slugs.is_empty() {
        for n in notes_by_slugs(db, agent_id, &related_slugs).await? {
            if !present.contains(&n.id) && passes(&n) {
                hits.push(RetrievedNote {
                    note: n,
                    score: 0.0,
                });
            }
        }
    }

    Ok(hits)
}

// ── Migration / backfill from the legacy Qdrant-only KB ─────────────────────────────────────

/// Export an agent's existing Qdrant chunks into `vault_notes` (one note per chunk) for agents
/// that predate the vault. Idempotent: skips when the agent already has notes. Preserves chunk
/// ids 1:1 (so `applications.shared_chunk_ids` keeps resolving) and re-derives the index from
/// the new source of truth. Returns the number of notes created.
pub async fn backfill_from_qdrant(
    db: &PgPool,
    qdrant: &Qdrant,
    inference: &InferenceClient,
    agent_id: Uuid,
) -> Result<usize> {
    let existing = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM vault_notes WHERE agent_id = $1",
        agent_id
    )
    .fetch_one(db)
    .await?
    .unwrap_or(0);
    if existing > 0 {
        return Ok(0);
    }

    let chunks = knowledge::list_chunks_for_agent(qdrant, agent_id).await?;
    let mut created = 0usize;
    for (i, c) in chunks.into_iter().enumerate() {
        let title: String = c
            .content
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect();
        let title = if title.trim().is_empty() {
            format!("note-{}", i + 1)
        } else {
            title
        };
        let note = VaultNote {
            id: c.id, // preserve the chunk id 1:1
            agent_id,
            slug: format!("imported-{}", c.id.simple()),
            title,
            body: c.content,
            layer: NoteLayer::Note,
            source_type: c.source_type,
            visibility: c.visibility,
            domain: None,
            related: vec![],
            repo: None,
            job_id: c.job_id,
        };
        // Insert the row directly (don't re-embed via upsert_note's index — reindex below does
        // it once); but reuse the same insert shape.
        sqlx::query!(
            r#"INSERT INTO vault_notes
                 (id, agent_id, slug, title, body, layer, source_type, visibility, related, job_id)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
               ON CONFLICT (agent_id, slug) DO NOTHING"#,
            note.id,
            note.agent_id,
            note.slug,
            note.title,
            note.body,
            note.layer.as_str(),
            note.source_type.as_str(),
            note.visibility.as_str(),
            &note.related,
            note.job_id,
        )
        .execute(db)
        .await?;
        created += 1;
    }

    // Re-derive the index from the new source of truth and assert parity.
    let reindexed = reindex_agent(db, qdrant, inference, agent_id).await?;
    if reindexed != created {
        tracing::warn!(
            "vault backfill parity mismatch for agent {}: {} notes vs {} reindexed",
            agent_id,
            created,
            reindexed
        );
    }
    Ok(created)
}

// ── R2 markdown mirror (human/export layer only) ────────────────────────────────────────────

/// Mirror a note to R2 as `agents/<agent_id>/<slug>.md` for human Obsidian-open / git-backup.
/// Best-effort: returns Ok(()) and logs if R2 isn't configured. Never on the query path.
pub async fn mirror_to_r2(note: &VaultNote) -> Result<()> {
    let (bucket, endpoint, access, secret) = match (
        std::env::var("R2_BUCKET"),
        std::env::var("R2_ENDPOINT"),
        std::env::var("R2_ACCESS_KEY"),
        std::env::var("R2_SECRET_KEY"),
    ) {
        (Ok(b), Ok(e), Ok(a), Ok(s)) => (b, e, a, s),
        _ => {
            tracing::debug!("vault R2 mirror skipped: R2 not configured");
            return Ok(());
        }
    };

    use aws_credential_types::Credentials;
    use aws_sdk_s3::config::{Builder, Region};
    use aws_sdk_s3::Client as S3Client;

    let creds = Credentials::new(access, secret, None, None, "static");
    let config = Builder::new()
        .endpoint_url(endpoint)
        .credentials_provider(creds)
        .region(Region::new("auto"))
        .force_path_style(true)
        .behavior_version_latest()
        .build();
    let s3 = S3Client::from_conf(config);
    let key = format!("vault/agents/{}/{}.md", note.agent_id, note.slug);
    s3.put_object()
        .bucket(bucket)
        .key(key)
        .body(to_markdown(note).into_bytes().into())
        .content_type("text/markdown")
        .send()
        .await
        .map_err(|e| anyhow!("R2 put_object failed: {e}"))?;
    Ok(())
}

// ── Company-facing application graph (IP-safe) ──────────────────────────────────────────────

/// A node in the company-facing applicant vault graph. Shared (snapshot + still-shareable) notes
/// carry full title/body; every other note is **anonymized** — `locked: true`, a neutral label,
/// no real title/body — so the company sees the *shape* of the knowledge base (how rich + connected
/// it is) without leaking the existence-meaning of private work.
#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub slug: String,
    pub title: String,
    pub layer: String,
    pub domain: Option<String>,
    pub locked: bool,
    /// Full markdown body — present only for unlocked (shared) notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct VaultGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// Build the full graph of an agent's vault for a company reviewing an applicant. `shared_ids` is
/// the application's frozen snapshot; only notes that are BOTH in the snapshot AND still shareable
/// are unlocked (full title/body). All other notes are anonymized placeholders — structure only.
/// Edges are emitted for `related` links whose target exists (anonymized endpoints still connect,
/// so the graph's connectivity shows).
pub async fn application_graph(
    db: &PgPool,
    agent_id: Uuid,
    shared_ids: &[Uuid],
) -> Result<VaultGraph> {
    let notes = list_notes_for_agent(db, agent_id).await?;
    let shared: HashSet<Uuid> = shared_ids.iter().copied().collect();
    let slugs: HashSet<&str> = notes.iter().map(|n| n.slug.as_str()).collect();

    let mut locked_seq = 0u32;
    let nodes = notes
        .iter()
        .map(|n| {
            let is_shared = shared.contains(&n.id) && n.visibility == ChunkVisibility::Shareable;
            if is_shared {
                GraphNode {
                    slug: n.slug.clone(),
                    title: n.title.clone(),
                    layer: n.layer.as_str().to_string(),
                    domain: n.domain.clone(),
                    locked: false,
                    body: Some(n.body.clone()),
                }
            } else {
                locked_seq += 1;
                GraphNode {
                    // Keep the real slug so edges still resolve, but no human-meaningful title/body.
                    slug: n.slug.clone(),
                    title: format!("Private note #{locked_seq}"),
                    layer: n.layer.as_str().to_string(), // layer is non-sensitive (style/feature/…)
                    domain: None,
                    locked: true,
                    body: None,
                }
            }
        })
        .collect();

    // Normalize each `related` ref (strip `[[wikilink]]` wrappers → bare slug) before matching, and
    // drop self-loops — the same edge logic the professional's own graph uses. Without this, refs
    // stored in wikilink form (e.g. UMS notes) never match a bare slug, so those nodes end up
    // edgeless and drift off into space instead of clustering with the rest of the graph.
    let edges = notes
        .iter()
        .flat_map(|n| {
            n.related
                .iter()
                .map(|t| normalize_ref(t))
                .filter(|t| slugs.contains(t.as_str()) && t != &n.slug)
                .map(|t| GraphEdge {
                    source: n.slug.clone(),
                    target: t,
                })
                .collect::<Vec<_>>()
        })
        .collect();

    Ok(VaultGraph { nodes, edges })
}

// ── Shared tool handler (used by both the MCP route and the Managed Agents backend) ─────────

/// Run one vault tool by name against an agent's vault and return the text payload. This is the
/// single place vault-tool logic lives — the `mcp_vault` route and the Managed-Agents custom-tool
/// flow both call it, exactly as `taste::consult` is shared. `args` is the tool's JSON arguments.
pub async fn handle_tool(
    db: &PgPool,
    qdrant: &Qdrant,
    inference: &InferenceClient,
    agent_id: Uuid,
    tool: &str,
    args: &serde_json::Value,
) -> Result<String> {
    let str_arg = |k: &str| -> Option<String> {
        args.get(k)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty())
    };
    let arr_arg = |k: &str| -> Vec<String> {
        args.get(k)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };

    match tool {
        "upsert_note" => {
            let slug = str_arg("slug").ok_or_else(|| anyhow!("upsert_note: 'slug' required"))?;
            let title = str_arg("title").unwrap_or_else(|| slug.clone());
            let body = str_arg("body").ok_or_else(|| anyhow!("upsert_note: 'body' required"))?;
            let layer = NoteLayer::from_str(&str_arg("layer").unwrap_or_default());
            let existing = get_note(db, agent_id, &slug).await?;
            let (id, visibility, source_type) = match &existing {
                Some(n) => (n.id, n.visibility, n.source_type.clone()),
                None => (
                    Uuid::new_v4(),
                    ChunkVisibility::Private,
                    KnowledgeSourceType::ApprovedOutput,
                ),
            };
            let note = VaultNote {
                id,
                agent_id,
                slug: slug.clone(),
                title,
                body,
                layer,
                source_type,
                visibility,
                domain: str_arg("domain"),
                related: arr_arg("related"),
                repo: str_arg("repo"),
                job_id: None,
            };
            upsert_note(db, qdrant, inference, note).await?;
            Ok(format!("Saved note '{slug}'."))
        }
        "list_vault" => {
            let layer = str_arg("layer").map(|s| NoteLayer::from_str(&s));
            let domain = str_arg("domain");
            let notes = list_notes_for_agent(db, agent_id).await?;
            let filtered: Vec<&VaultNote> = notes
                .iter()
                .filter(|n| layer.map_or(true, |l| n.layer == l))
                .filter(|n| {
                    domain
                        .as_deref()
                        .map_or(true, |d| n.domain.as_deref() == Some(d))
                })
                .collect();
            if filtered.is_empty() {
                return Ok("(vault is empty for this filter)".into());
            }
            Ok(filtered
                .iter()
                .map(|n| format!("- [{}] {} ({})", n.layer.as_str(), n.title, n.slug))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "read_note" => {
            let slug = str_arg("slug").ok_or_else(|| anyhow!("read_note: 'slug' required"))?;
            match get_note(db, agent_id, &slug).await? {
                Some(n) => Ok(to_markdown(&n)),
                None => Ok(format!("No note with slug '{slug}'.")),
            }
        }
        "search_vault" => {
            let query =
                str_arg("query").ok_or_else(|| anyhow!("search_vault: 'query' required"))?;
            let filter = RetrievalFilter {
                layer: str_arg("layer").map(|s| NoteLayer::from_str(&s)),
                domain: str_arg("domain"),
                shareable_only: false,
            };
            let hits = retrieve(db, qdrant, inference, agent_id, &query, &filter, 6).await?;
            if hits.is_empty() {
                return Ok("(no matching notes)".into());
            }
            Ok(hits
                .iter()
                .map(|h| format!("## {} ({})\n{}", h.note.title, h.note.slug, h.note.body))
                .collect::<Vec<_>>()
                .join("\n\n---\n\n"))
        }
        "follow_links" => {
            let slug = str_arg("slug").ok_or_else(|| anyhow!("follow_links: 'slug' required"))?;
            let Some(note) = get_note(db, agent_id, &slug).await? else {
                return Ok(format!("No note with slug '{slug}'."));
            };
            if note.related.is_empty() {
                return Ok(format!("'{slug}' has no related links."));
            }
            let neighbors = notes_by_slugs(db, agent_id, &note.related).await?;
            if neighbors.is_empty() {
                return Ok(format!(
                    "'{slug}' links to {:?} but none exist yet.",
                    note.related
                ));
            }
            Ok(neighbors
                .iter()
                .map(|n| format!("## {} ({})\n{}", n.title, n.slug, n.body))
                .collect::<Vec<_>>()
                .join("\n\n---\n\n"))
        }
        other => Err(anyhow!("Unknown vault tool '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_wikilinks() {
        let v = extract_wikilinks("see [[Foo Bar]] and [[baz]] end");
        assert_eq!(v, vec!["Foo Bar".to_string(), "baz".to_string()]);
    }

    #[test]
    fn domain_coerced_to_taxonomy() {
        assert_eq!(normalize_domain(Some("backend")), Some("backend".into()));
        assert_eq!(normalize_domain(Some("Quantum")), Some("general".into()));
        assert_eq!(normalize_domain(Some("")), None);
        assert_eq!(normalize_domain(None), None);
    }

    #[test]
    fn markdown_roundtrip_has_frontmatter() {
        let note = VaultNote {
            id: Uuid::nil(),
            agent_id: Uuid::nil(),
            slug: "x".into(),
            title: "T".into(),
            body: "body [[y]]".into(),
            layer: NoteLayer::Feature,
            source_type: KnowledgeSourceType::Portfolio,
            visibility: ChunkVisibility::Private,
            domain: Some("backend".into()),
            related: vec!["y".into()],
            repo: None,
            job_id: None,
        };
        let md = to_markdown(&note);
        assert!(md.starts_with("---\n"));
        assert!(md.contains("layer: feature"));
        assert!(md.contains("[[y]]"));
    }
}
