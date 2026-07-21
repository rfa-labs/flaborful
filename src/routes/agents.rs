use crate::auth_extractor::AuthUser;
use crate::services::knowledge::{ChunkVisibility, KnowledgeSourceType};
use crate::services::lora_training::{ChunkPreview, DatasetFilter};
use crate::services::{agents as svc, authz, lora_training};
use crate::{
    errors::AppResult,
    models::agent::{Agent, AgentStatus, CreateAgentRequest},
    AppState,
};
use axum::{
    extract::{Path, State},
    routing::{get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create_agent))
        .route("/mine", get(get_my_agent))
        .route("/marketplace", get(list_marketplace))
        .route("/:id", get(get_agent))
        .route("/:id/status", patch(set_agent_status))
        .route("/:id/retrain", post(retrain_agent))
        .route("/:id/retrain/preview", post(preview_retrain))
        .route("/:id/reset", post(reset_agent))
        .route("/:id/execution", patch(set_execution))
        .route("/:id/anthropic-key", patch(set_anthropic_key))
        .route("/:id/persona", patch(set_persona))
        .route(
            "/:id/tool-policy",
            get(get_tool_policy).put(set_tool_policy),
        )
}

/// GET /agents/:id/tool-policy — the agent's HITL tool-approval policy (approve-before-write).
async fn get_tool_policy(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<crate::services::tool_policy::ToolPolicy>> {
    authz::require_agent_owner(&state.db, auth.user_id, id).await?;
    let policy = crate::services::tool_policy::get(&state.db, id)
        .await
        .map_err(crate::errors::AppError::Internal)?;
    Ok(Json(policy))
}

#[derive(Deserialize)]
struct SetToolPolicyBody {
    mode: String,
    #[serde(default)]
    rules: serde_json::Value,
}

/// PUT /agents/:id/tool-policy — set the approval mode + per-effect-kind rules.
async fn set_tool_policy(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SetToolPolicyBody>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, id).await?;
    let rules = if body.rules.is_object() {
        body.rules
    } else {
        serde_json::json!({})
    };
    crate::services::tool_policy::set(&state.db, id, &body.mode, &rules)
        .await
        .map_err(crate::errors::AppError::Internal)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct SetPersonaBody {
    persona_system_prompt: String,
}

/// PATCH /agents/:id/persona — edit the agent's system prompt (its standing instructions). This is
/// auto-generated from the GitHub style guide at onboarding, but the professional (or an approved
/// instruction-update reflection) can refine it. See [[Flaborful Reflections & Evaluations]].
async fn set_persona(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SetPersonaBody>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, id).await?;
    sqlx::query!(
        "UPDATE agents SET persona_system_prompt = $1, updated_at = NOW() WHERE id = $2",
        body.persona_system_prompt,
        id,
    )
    .execute(&state.db)
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct SetExecutionBody {
    /// managed | sprite
    execution_backend: String,
    /// claude_code | codex | custom
    agent_runtime: String,
    runtime_spec: Option<serde_json::Value>,
    /// The professional's own Anthropic/Claude API key. When present and non-empty, it is
    /// encrypted and stored (replacing any prior key). Absent/None => leave the stored key
    /// unchanged. An explicit empty string => clear the key.
    #[serde(default)]
    anthropic_api_key: Option<String>,
}

/// PATCH /agents/:id/execution — set the professional's default execution backend + runtime.
async fn set_execution(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SetExecutionBody>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, id).await?;
    if !matches!(body.execution_backend.as_str(), "managed" | "sprite") {
        return Err(crate::errors::AppError::BadRequest(
            "execution_backend must be managed or sprite".into(),
        ));
    }
    if !matches!(
        body.agent_runtime.as_str(),
        "claude_code" | "codex" | "custom"
    ) {
        return Err(crate::errors::AppError::BadRequest(
            "agent_runtime must be claude_code, codex, or custom".into(),
        ));
    }
    sqlx::query!(
        "UPDATE agents SET execution_backend = $1, agent_runtime = $2, runtime_spec = $3, updated_at = NOW() WHERE id = $4",
        body.execution_backend,
        body.agent_runtime,
        body.runtime_spec,
        id,
    )
    .execute(&state.db)
    .await?;

    // Key update is separate from the always-applied config above so an omitted key never wipes a
    // stored one. None = untouched; Some("") = clear; Some(key) = encrypt + store.
    match body.anthropic_api_key {
        None => {}
        Some(key) if key.trim().is_empty() => {
            sqlx::query!(
                "UPDATE agents SET anthropic_api_key_enc = NULL, updated_at = NOW() WHERE id = $1",
                id,
            )
            .execute(&state.db)
            .await?;
        }
        Some(key) => {
            let enc = crate::services::crypto::encrypt(key.trim())?;
            sqlx::query!(
                "UPDATE agents SET anthropic_api_key_enc = $1, updated_at = NOW() WHERE id = $2",
                enc,
                id,
            )
            .execute(&state.db)
            .await?;
        }
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct SetAnthropicKeyBody {
    /// The professional's own Anthropic/Claude API key. Non-empty => encrypt + store (replacing any
    /// prior key). Empty string => clear the stored key. This is the lightweight path used by the
    /// onboarding BYO-key prompt, which shouldn't have to send full execution config to save a key.
    anthropic_api_key: String,
}

/// PATCH /agents/:id/anthropic-key — set (or clear) just the professional's own Anthropic key,
/// without touching execution backend/runtime. Onboarding offers this optionally so a user who
/// wants their own work billed to their key can add it up front; it's never required (ingestion
/// runs on Sprite with the server key regardless — see services/onboarding/ingest.rs).
async fn set_anthropic_key(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SetAnthropicKeyBody>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, id).await?;
    if body.anthropic_api_key.trim().is_empty() {
        sqlx::query!(
            "UPDATE agents SET anthropic_api_key_enc = NULL, updated_at = NOW() WHERE id = $1",
            id,
        )
        .execute(&state.db)
        .await?;
        return Ok(Json(serde_json::json!({ "ok": true, "key_set": false })));
    }
    let enc = crate::services::crypto::encrypt(body.anthropic_api_key.trim())?;
    sqlx::query!(
        "UPDATE agents SET anthropic_api_key_enc = $1, updated_at = NOW() WHERE id = $2",
        enc,
        id,
    )
    .execute(&state.db)
    .await?;
    Ok(Json(serde_json::json!({ "ok": true, "key_set": true })))
}

#[derive(Deserialize)]
struct SetStatusRequest {
    status: AgentStatus,
}

/// Optional retraining selection. Absent/empty => entire knowledge base.
#[derive(Deserialize, Default)]
struct RetrainFilterBody {
    source_types: Option<Vec<String>>,
    chunk_ids: Option<Vec<Uuid>>,
    visibility: Option<String>,
}

impl RetrainFilterBody {
    fn into_filter(self) -> DatasetFilter {
        let source_types = self.source_types.map(|types| {
            types
                .iter()
                .filter_map(|s| parse_source_type(s))
                .collect::<Vec<_>>()
        });
        let visibility = self.visibility.as_deref().and_then(|v| match v {
            "shareable" => Some(ChunkVisibility::Shareable),
            "private" => Some(ChunkVisibility::Private),
            _ => None,
        });
        DatasetFilter {
            source_types,
            chunk_ids: self.chunk_ids,
            visibility,
        }
    }
}

fn parse_source_type(s: &str) -> Option<KnowledgeSourceType> {
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

async fn get_my_agent(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Agent>> {
    let professional = sqlx::query!(
        "SELECT id FROM professionals WHERE user_id = $1",
        auth.user_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| crate::errors::AppError::NotFound("Professional not found".into()))?;
    let agent = svc::get_agent_by_professional(&state.db, professional.id).await?;
    Ok(Json(agent))
}

async fn create_agent(
    State(state): State<AppState>,
    _auth: AuthUser,
    Json(req): Json<CreateAgentRequest>,
) -> AppResult<Json<Agent>> {
    let agent = svc::create_agent(&state.db, req).await?;
    Ok(Json(agent))
}

async fn get_agent(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> AppResult<Json<Agent>> {
    let agent = svc::get_agent(&state.db, id).await?;
    Ok(Json(agent))
}

async fn set_agent_status(
    State(state): State<AppState>,
    _auth: AuthUser,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Json(req): Json<SetStatusRequest>,
) -> AppResult<Json<Agent>> {
    let agent = svc::set_agent_status(&state.db, id, req.status).await?;
    Ok(Json(agent))
}

async fn list_marketplace(State(state): State<AppState>) -> AppResult<Json<Vec<Agent>>> {
    let agents = svc::list_marketplace(&state.db).await?;
    Ok(Json(agents))
}

/// POST /api/v1/agents/:id/retrain
/// Triggers a new LoRA training job. An optional body selects which knowledge-base
/// components to include; absent => the entire KB (original behavior).
async fn retrain_agent(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<RetrainFilterBody>>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, id).await?;
    let filter = body.map(|Json(b)| b.into_filter()).unwrap_or_default();
    lora_training::trigger_training_with_filter(&state.db, &state.inference, id, filter).await?;
    Ok(Json(serde_json::json!({ "ok": true, "agent_id": id })))
}

/// POST /api/v1/agents/:id/retrain/preview
/// Returns which chunks a given selection would include, without training.
async fn preview_retrain(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<RetrainFilterBody>>,
) -> AppResult<Json<Vec<ChunkPreview>>> {
    authz::require_agent_owner(&state.db, auth.user_id, id).await?;
    let filter = body.map(|Json(b)| b.into_filter()).unwrap_or_default();
    let preview = lora_training::preview_dataset(&state.qdrant, id, &filter)
        .await
        .map_err(crate::errors::AppError::Internal)?;
    Ok(Json(preview))
}

/// POST /api/v1/agents/:id/reset
/// Soft-resets the agent (wipes LoRA/R2, status→onboarding) while preserving the KB,
/// so the owner can rebuild from scratch.
async fn reset_agent(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    authz::require_agent_owner(&state.db, auth.user_id, id).await?;
    lora_training::reset_agent(&state.db, id)
        .await
        .map_err(crate::errors::AppError::Internal)?;
    Ok(Json(serde_json::json!({ "ok": true, "agent_id": id })))
}
