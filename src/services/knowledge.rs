use anyhow::Result;
use qdrant_client::{
    qdrant::{
        vectors_config::Config, CreateCollectionBuilder, Distance, PointStruct, Query,
        QueryPointsBuilder, ScrollPointsBuilder, UpsertPointsBuilder, VectorParams, VectorsConfig,
    },
    Qdrant,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// Flaborful uses 384-dim embeddings (AllMiniLML6V2 via fastembed)
const VECTOR_SIZE: u64 = 384;

pub const COLLECTION_KNOWLEDGE: &str = "flaborful_knowledge";

/// Ensure all required Qdrant collections exist on startup.
pub async fn ensure_collections(client: &Qdrant) -> Result<()> {
    ensure_collection(client, COLLECTION_KNOWLEDGE).await?;
    Ok(())
}

async fn ensure_collection(client: &Qdrant, name: &str) -> Result<()> {
    if !client.collection_exists(name).await? {
        client
            .create_collection(
                CreateCollectionBuilder::new(name).vectors_config(VectorsConfig {
                    config: Some(Config::Params(VectorParams {
                        size: VECTOR_SIZE,
                        distance: Distance::Cosine.into(),
                        ..Default::default()
                    })),
                }),
            )
            .await?;
        tracing::info!("Created Qdrant collection: {}", name);
    }
    Ok(())
}

/// A knowledge chunk stored in Qdrant.
/// The payload mirrors these fields; the embedding is the vector.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct KnowledgeChunk {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub job_id: Option<Uuid>,
    pub content: String,
    pub source_type: KnowledgeSourceType,
    /// Whether the owner allows this chunk to be exposed to companies (e.g. attached to a
    /// job application). Private by default — protects IP. Resume defaults to shareable.
    pub visibility: ChunkVisibility,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeSourceType {
    Portfolio,           // uploaded during onboarding
    JobContext,          // ingested during trial week
    ApprovedOutput,      // past task outputs approved by professional
    Resume,              // the professional's resume (onboarding)
    ApplicationMaterial, // cover letters / self-intro materials from job applications
    InterviewNote,       // notes/transcripts from interview rounds
    ChatCorrection,      // pro↔agent corrections about workflow node outputs
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ChunkVisibility {
    /// Owner-only. Used for LoRA training and the agent's own RAG, never shown to companies.
    Private,
    /// Owner has marked this shareable; may be attached to job applications.
    Shareable,
}

impl ChunkVisibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChunkVisibility::Private => "private",
            ChunkVisibility::Shareable => "shareable",
        }
    }

    /// Parse a stored visibility string. Unknown / missing → Private (safe default).
    pub fn from_str(s: &str) -> Self {
        match s {
            "shareable" => ChunkVisibility::Shareable,
            _ => ChunkVisibility::Private,
        }
    }
}

impl KnowledgeSourceType {
    /// Canonical snake_case string. Public so the vault layer can persist it on a note row.
    pub fn as_str(&self) -> &'static str {
        source_type_str(self)
    }

    /// Parse a snake_case source-type string. None on an unknown value.
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "portfolio" => KnowledgeSourceType::Portfolio,
            "job_context" => KnowledgeSourceType::JobContext,
            "approved_output" => KnowledgeSourceType::ApprovedOutput,
            "resume" => KnowledgeSourceType::Resume,
            "application_material" => KnowledgeSourceType::ApplicationMaterial,
            "interview_note" => KnowledgeSourceType::InterviewNote,
            "chat_correction" => KnowledgeSourceType::ChatCorrection,
            _ => return None,
        })
    }
}

/// Canonical snake_case string for a source type. Inverse of the parser in
/// `chunk_from_point`; keep the two in sync.
fn source_type_str(s: &KnowledgeSourceType) -> &'static str {
    match s {
        KnowledgeSourceType::Portfolio => "portfolio",
        KnowledgeSourceType::JobContext => "job_context",
        KnowledgeSourceType::ApprovedOutput => "approved_output",
        KnowledgeSourceType::Resume => "resume",
        KnowledgeSourceType::ApplicationMaterial => "application_material",
        KnowledgeSourceType::InterviewNote => "interview_note",
        KnowledgeSourceType::ChatCorrection => "chat_correction",
    }
}

/// Upsert a batch of knowledge chunks into Qdrant.
/// Embeddings are generated via fastembed before calling this.
pub async fn upsert_chunks(
    client: &Qdrant,
    chunks: Vec<KnowledgeChunk>,
    embeddings: Vec<Vec<f32>>,
) -> Result<()> {
    assert_eq!(chunks.len(), embeddings.len());

    let points: Vec<PointStruct> = chunks
        .into_iter()
        .zip(embeddings)
        .map(|(chunk, embedding)| {
            let payload: HashMap<String, qdrant_client::qdrant::Value> = [
                ("agent_id".to_string(), chunk.agent_id.to_string().into()),
                ("content".to_string(), chunk.content.into()),
                (
                    "source_type".to_string(),
                    source_type_str(&chunk.source_type).into(),
                ),
                ("visibility".to_string(), chunk.visibility.as_str().into()),
            ]
            .into_iter()
            .chain(
                chunk
                    .job_id
                    .map(|j| ("job_id".to_string(), j.to_string().into())),
            )
            .collect();

            PointStruct::new(chunk.id.to_string(), embedding, payload)
        })
        .collect();

    client
        .upsert_points(UpsertPointsBuilder::new(COLLECTION_KNOWLEDGE, points))
        .await?;

    Ok(())
}

/// Semantic search: find the top-K most relevant chunks for a given query embedding.
pub async fn search_chunks(
    client: &Qdrant,
    query_embedding: Vec<f32>,
    agent_id: Uuid,
    limit: u64,
    score_threshold: f32,
) -> Result<Vec<KnowledgeChunk>> {
    let results = client
        .query(
            QueryPointsBuilder::new(COLLECTION_KNOWLEDGE)
                .query(Query::new_nearest(query_embedding))
                .limit(limit)
                .score_threshold(score_threshold)
                .with_payload(true),
        )
        .await?;

    let chunks = results
        .result
        .into_iter()
        .filter_map(|p| chunk_from_point(p.id, p.payload))
        .filter(|c| c.agent_id == agent_id)
        .collect();

    Ok(chunks)
}

/// Scroll all knowledge chunks for an agent (used for LoRA dataset assembly).
pub async fn list_chunks_for_agent(client: &Qdrant, agent_id: Uuid) -> Result<Vec<KnowledgeChunk>> {
    let mut all_chunks = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;

    loop {
        let mut req = ScrollPointsBuilder::new(COLLECTION_KNOWLEDGE)
            .limit(100)
            .with_payload(true)
            .with_vectors(false);

        if let Some(ref off) = offset {
            req = req.offset(off.clone());
        }

        let resp = client.scroll(req).await?;
        let next_offset = resp.next_page_offset.clone();

        for point in resp.result {
            if let Some(chunk) = chunk_from_point(point.id, point.payload) {
                if chunk.agent_id == agent_id {
                    all_chunks.push(chunk);
                }
            }
        }

        match next_offset {
            Some(off) => offset = Some(off),
            None => break,
        }
    }

    Ok(all_chunks)
}

/// Fetch specific chunks by id (used to resolve a job application's shared snapshot).
/// Only chunks belonging to `agent_id` are returned, and — when `shareable_only` —
/// only those the owner marked shareable, so a stale id can't leak a now-private chunk.
pub async fn fetch_chunks_by_ids(
    client: &Qdrant,
    agent_id: Uuid,
    ids: &[Uuid],
    shareable_only: bool,
) -> Result<Vec<KnowledgeChunk>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let point_ids: Vec<qdrant_client::qdrant::PointId> =
        ids.iter().map(|id| id.to_string().into()).collect();

    let resp = client
        .get_points(
            qdrant_client::qdrant::GetPointsBuilder::new(COLLECTION_KNOWLEDGE, point_ids)
                .with_payload(true)
                .with_vectors(false),
        )
        .await?;

    let chunks = resp
        .result
        .into_iter()
        .filter_map(|p| chunk_from_point(p.id, p.payload))
        .filter(|c| c.agent_id == agent_id)
        .filter(|c| !shareable_only || c.visibility == ChunkVisibility::Shareable)
        .collect();

    Ok(chunks)
}

/// Record a knowledge-base timeline event (added/deleted) in Postgres. Best-effort:
/// callers should not fail their main operation if this logging fails.
pub async fn record_event(
    db: &sqlx::PgPool,
    agent_id: Uuid,
    chunk_id: Uuid,
    event_type: &str,
    source_type: &KnowledgeSourceType,
    summary: &str,
) -> Result<()> {
    let summary: String = summary.chars().take(200).collect();
    sqlx::query!(
        r#"INSERT INTO knowledge_events (agent_id, chunk_id, event_type, source_type, summary)
           VALUES ($1, $2, $3, $4, $5)"#,
        agent_id,
        chunk_id,
        event_type,
        source_type_str(source_type),
        summary,
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Fetch a single chunk by id (full content + metadata). None if not found.
pub async fn fetch_chunk(client: &Qdrant, chunk_id: Uuid) -> Result<Option<KnowledgeChunk>> {
    let resp = client
        .get_points(
            qdrant_client::qdrant::GetPointsBuilder::new(
                COLLECTION_KNOWLEDGE,
                vec![chunk_id.to_string().into()],
            )
            .with_payload(true)
            .with_vectors(false),
        )
        .await?;
    Ok(resp
        .result
        .into_iter()
        .filter_map(|p| chunk_from_point(p.id, p.payload))
        .next())
}

/// Look up which agent a chunk belongs to (for ownership checks). None if not found.
pub async fn chunk_agent_id(client: &Qdrant, chunk_id: Uuid) -> Result<Option<Uuid>> {
    let resp = client
        .get_points(
            qdrant_client::qdrant::GetPointsBuilder::new(
                COLLECTION_KNOWLEDGE,
                vec![chunk_id.to_string().into()],
            )
            .with_payload(true)
            .with_vectors(false),
        )
        .await?;

    Ok(resp
        .result
        .into_iter()
        .filter_map(|p| chunk_from_point(p.id, p.payload))
        .next()
        .map(|c| c.agent_id))
}

/// Delete a single chunk by id (e.g. replacing the canonical resume).
pub async fn delete_chunk(client: &Qdrant, chunk_id: Uuid) -> Result<()> {
    client
        .delete_points(
            qdrant_client::qdrant::DeletePointsBuilder::new(COLLECTION_KNOWLEDGE).points(
                qdrant_client::qdrant::PointsIdsList {
                    ids: vec![chunk_id.to_string().into()],
                },
            ),
        )
        .await?;
    Ok(())
}

/// Update the visibility of a single chunk (owner toggles share/unshare).
/// Qdrant stores payload, so we overwrite just the `visibility` key.
pub async fn set_chunk_visibility(
    client: &Qdrant,
    chunk_id: Uuid,
    visibility: ChunkVisibility,
) -> Result<()> {
    let payload: HashMap<String, qdrant_client::qdrant::Value> =
        [("visibility".to_string(), visibility.as_str().into())]
            .into_iter()
            .collect();

    client
        .set_payload(
            qdrant_client::qdrant::SetPayloadPointsBuilder::new(COLLECTION_KNOWLEDGE, payload)
                .points_selector(qdrant_client::qdrant::PointsIdsList {
                    ids: vec![chunk_id.to_string().into()],
                }),
        )
        .await?;
    Ok(())
}

fn chunk_from_point(
    id: Option<qdrant_client::qdrant::PointId>,
    payload: HashMap<String, qdrant_client::qdrant::Value>,
) -> Option<KnowledgeChunk> {
    let id_str = match id?.point_id_options? {
        qdrant_client::qdrant::point_id::PointIdOptions::Uuid(u) => u,
        qdrant_client::qdrant::point_id::PointIdOptions::Num(_) => return None,
    };
    let id = Uuid::parse_str(&id_str).ok()?;

    let get = |key: &str| -> Option<String> {
        payload.get(key).and_then(|v| match &v.kind {
            Some(qdrant_client::qdrant::value::Kind::StringValue(s)) => Some(s.clone()),
            _ => None,
        })
    };

    let agent_id = Uuid::parse_str(&get("agent_id")?).ok()?;
    let content = get("content")?;
    let source_type = match get("source_type")?.as_str() {
        "portfolio" => KnowledgeSourceType::Portfolio,
        "job_context" => KnowledgeSourceType::JobContext,
        "approved_output" => KnowledgeSourceType::ApprovedOutput,
        "resume" => KnowledgeSourceType::Resume,
        "application_material" => KnowledgeSourceType::ApplicationMaterial,
        "interview_note" => KnowledgeSourceType::InterviewNote,
        "chat_correction" => KnowledgeSourceType::ChatCorrection,
        _ => return None,
    };
    // Chunks written before visibility existed are treated as private (safe default).
    let visibility = match get("visibility").as_deref() {
        Some("shareable") => ChunkVisibility::Shareable,
        _ => ChunkVisibility::Private,
    };
    let job_id = get("job_id").and_then(|s| Uuid::parse_str(&s).ok());

    Some(KnowledgeChunk {
        id,
        agent_id,
        job_id,
        content,
        source_type,
        visibility,
    })
}
