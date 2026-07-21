use axum::{
    extract::{FromRequestParts, Path, Query, State},
    http::request::Parts,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth_extractor::AuthUser,
    errors::AppError,
    models::connection::ConnectionProvider,
    services::{
        connections,
        onboarding::{
            github::GitHubIngestion,
            interview::{InterviewResponse, InterviewService, OpenAnswer},
            style_guide::StyleGuideExtractor,
        },
    },
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/github/ingest", post(ingest_github))
        .route("/google-docs/ingest", post(ingest_google_docs))
        .route("/scenarios/:profession", get(get_scenarios))
        .route("/interview/submit", post(submit_interview))
        // Slim, open-form interview (communication & reasoning style → style-rule vault notes).
        .route("/interview/questions/:profession", get(get_open_questions))
        .route("/interview/open/submit", post(submit_open_interview))
        // Snapshot of agentic code-ingestion progress (per-repo status + note counts). The live
        // feed is GET /events/ingestion/:agent_id (SSE); this is the on-load / reconnect view.
        .route("/ingestion/status/:agent_id", get(ingestion_status))
        // Stop ingestion after the current repo (keeps notes already written) and kill the live
        // managed session/environment so nothing keeps running/costing.
        .route("/ingestion/cancel/:agent_id", post(cancel_ingestion))
        // RunPod has no webhook signing — the shared secret rides in the path and is checked
        // in constant time. Configured via RUNPOD_WEBHOOK_SECRET. Without it, callbacks 401.
        // The secret is validated by the `RunPodSecret` extractor BEFORE the Json body is
        // parsed, so an attacker can't learn the route/payload shape from validation errors.
        .route("/lora/complete/:secret", post(lora_complete))
}

/// Extractor that validates the RunPod webhook shared secret from the `:secret` path param
/// in constant time. Implemented as `FromRequestParts` (not `Path`) so it runs *before* the
/// body-extracting `Json` — otherwise Axum would deserialize the body first and leak
/// payload-shape errors (422) to unauthenticated callers, letting an attacker confirm the
/// route exists and probe its schema without the secret.
struct RunPodSecret;

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for RunPodSecret
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, AppError> {
        // Pull the :secret path param Axum already matched into parts.path. We read it from
        // the captured path via the same matcher the route uses; simplest is to re-extract
        // using axum's Path extractor for the single segment.
        let secret = axum::extract::Path::<String>::from_request_parts(parts, _state)
            .await
            .map(|Path(s)| s)
            .map_err(|_| AppError::Unauthorized)?;
        verify_runpod_secret(&secret)?;
        Ok(RunPodSecret)
    }
}

/// POST /api/v1/onboarding/github/ingest
/// Body: { "agent_id": "<uuid>" }
/// Requires the user to have completed GitHub OAuth first (a github connection must exist).
#[derive(Deserialize)]
struct IngestGitHubRequest {
    agent_id: Uuid,
    /// Re-document every selected repo even if it already has vault notes. Default false: repos
    /// already in the vault are skipped (resume), so a re-run only fills in the missing ones.
    #[serde(default)]
    force: bool,
}

#[derive(Serialize)]
struct IngestGitHubResponse {
    agent_id: Uuid,
    repos_analyzed: usize,
    style_guide: serde_json::Value,
    persona_prompt: String,
}

async fn ingest_github(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<IngestGitHubRequest>,
) -> Result<Json<IngestGitHubResponse>, AppError> {
    // Look up the stored GitHub connection for this user.
    let conn = connections::get_for_user(&state.db, auth.user_id, ConnectionProvider::Github)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| {
            AppError::BadRequest(
                "GitHub account not connected. Please complete GitHub OAuth first.".into(),
            )
        })?;

    let token = connections::fresh_access_token(&state.db, &conn)
        .await
        .map_err(AppError::Internal)?;

    let ingestion = GitHubIngestion::new(Some(token.clone()));
    let profile = ingestion.ingest(None).await?;
    let repos_analyzed = profile.repos.len();

    // Style-guide extraction is a *best-effort* persona seed via the inference provider (Ollama
    // Cloud), which can be flaky/slow. It must NOT gate the rest of onboarding — the real
    // knowledge base comes from the agentic repo→vault ingestion spawned below. On failure we
    // fall back to an empty guide and still build a persona prompt + kick off ingestion.
    let extractor = StyleGuideExtractor::new(&state.inference);
    let style_guide = match extractor.extract_from_github(&profile).await {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!("style-guide extraction failed (continuing without it): {e:?}");
            serde_json::json!({})
        }
    };

    let persona_prompt = StyleGuideExtractor::build_persona_prompt(&profile.username, &style_guide);

    // Persist style_guide and persona_prompt to the agent row
    sqlx::query!(
        r#"UPDATE agents SET style_guide = $1, persona_system_prompt = $2 WHERE id = $3"#,
        style_guide,
        persona_prompt,
        body.agent_id
    )
    .execute(&state.db)
    .await?;

    // Seed the vault with the cheap, summary-level style guide + per-aspect notes. The deep
    // repo→vault documentation (feature/pattern/tech/golden-snippet notes) is produced by the
    // agentic code-ingestion run; this is the lightweight persona seed retrievable by the
    // blueprint and graph. Each is a vault note (source of truth → derived chunk + timeline).
    use crate::services::knowledge::{ChunkVisibility, KnowledgeSourceType};
    use crate::services::vault::{self, NoteLayer, VaultNote};
    let style_text = format!(
        "Professional style guide for {}:\n{}",
        profile.username,
        serde_json::to_string_pretty(&style_guide).unwrap_or_default()
    );
    let style_note = VaultNote {
        id: Uuid::new_v4(),
        agent_id: body.agent_id,
        owner_kind: "agent".into(),
        slug: "style-guide".into(),
        title: format!("Style guide — {}", profile.username),
        body: style_text,
        layer: NoteLayer::StyleRule,
        source_type: KnowledgeSourceType::Portfolio,
        visibility: ChunkVisibility::Private,
        domain: None,
        related: vec![],
        repo: None,
        job_id: None,
    };
    let _ = vault::upsert_note(&state.db, &state.qdrant, &state.inference, style_note).await;

    // Per-aspect architecture notes from the GitHub profile. These are more best-effort inference
    // calls (up to 4 × Ollama), so we generate them in the background rather than blocking the
    // request — the response returns as soon as the deep ingestion is spawned below.
    {
        let db = state.db.clone();
        let qdrant = state.qdrant.clone();
        let inference = state.inference.clone();
        let profile = profile.clone();
        let agent_id = body.agent_id;
        tokio::spawn(async move {
            let extractor = StyleGuideExtractor::new(&inference);
            for aspect_slug in [
                "refactoring",
                "system_design",
                "documentation",
                "code_review",
            ] {
                if let Ok(Some(aspect_text)) =
                    extractor.extract_aspect_chunk(&profile, aspect_slug).await
                {
                    let note = VaultNote {
                        id: Uuid::new_v4(),
                        agent_id,
                        owner_kind: "agent".into(),
                        slug: format!("aspect-{}", aspect_slug),
                        title: format!("Aspect: {}", aspect_slug.replace('_', " ")),
                        body: aspect_text,
                        layer: NoteLayer::Architecture,
                        source_type: KnowledgeSourceType::Portfolio,
                        visibility: ChunkVisibility::Private,
                        domain: None,
                        related: vec!["style-guide".into()],
                        repo: None,
                        job_id: None,
                    };
                    let _ = vault::upsert_note(&db, &qdrant, &inference, note).await;
                }
            }
        });
    }

    // Kick off the deep, agentic repo→vault documentation in the background (one run per repo,
    // capped at the top few by the order GitHub returned — most-recently-updated first). The
    // coding agent reads real source and writes feature/pattern/tech/golden-snippet notes via the
    // vault tools. Returns immediately; the agent's vault fills in asynchronously.
    let cf_finetune_id = sqlx::query_scalar!(
        "SELECT cf_finetune_id FROM agents WHERE id = $1",
        body.agent_id
    )
    .fetch_optional(&state.db)
    .await?
    .flatten();
    // Use each repo's authoritative `full_name` ("owner/repo") for the clone target — the owner
    // can be an org or another account, NOT the authenticated user. Using `<user>/<name>` here was
    // wrong for org/collaborator repos and produced "Repository not found" in the git proxy.
    let repos: Vec<(String, String)> = profile
        .repos
        .iter()
        .take(5)
        .map(|r| (r.full_name.clone(), r.default_branch.clone()))
        .collect();
    crate::services::onboarding::ingest::spawn_ingestion(
        state.db.clone(),
        state.backends.clone(),
        crate::services::vault::VaultOwner::Agent(body.agent_id),
        Some((persona_prompt.clone(), cf_finetune_id)),
        Some(token),
        repos,
        body.force,
    );

    Ok(Json(IngestGitHubResponse {
        agent_id: body.agent_id,
        repos_analyzed,
        style_guide,
        persona_prompt,
    }))
}

#[derive(Deserialize)]
struct IngestGoogleDocsRequest {
    agent_id: Uuid,
}

/// POST /onboarding/google-docs/ingest — ingest the professional's Google Docs into their vault
/// (the first non-code likeness source). Resolves the owner's Google connection, fetches their
/// docs, and distills each into a vault note in the background (returns immediately, like GitHub).
async fn ingest_google_docs(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<IngestGoogleDocsRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::services::authz::require_agent_owner(&state.db, auth.user_id, body.agent_id).await?;

    let conn = connections::get_for_user(&state.db, auth.user_id, ConnectionProvider::Google)
        .await
        .map_err(AppError::Internal)?
        .ok_or_else(|| {
            AppError::BadRequest("Google account not connected. Connect Google first.".into())
        })?;
    let token = connections::fresh_access_token(&state.db, &conn)
        .await
        .map_err(AppError::Internal)?;

    // Persona + LoRA so notes are written in the professional's voice (best-effort).
    let row = sqlx::query!(
        "SELECT persona_system_prompt, cf_finetune_id FROM agents WHERE id = $1",
        body.agent_id
    )
    .fetch_optional(&state.db)
    .await?;
    let principal = row.map(|r| {
        (
            r.persona_system_prompt.unwrap_or_default(),
            r.cf_finetune_id,
        )
    });

    let db = state.db.clone();
    let qdrant = state.qdrant.clone();
    let inference = state.inference.clone();
    let owner = crate::services::vault::VaultOwner::Agent(body.agent_id);
    tokio::spawn(async move {
        use crate::services::onboarding::documents;
        match documents::fetch_google_docs(&token, 15).await {
            Ok(docs) if !docs.is_empty() => {
                let _ = documents::ingest_documents(
                    &db,
                    &qdrant,
                    &inference,
                    owner,
                    principal,
                    "google_docs",
                    docs,
                )
                .await;
            }
            Ok(_) => {
                // Nothing to ingest — still emit a completion event so the UI stops waiting.
                let _ = sqlx::query!(
                    "INSERT INTO ingestion_events (owner_kind, agent_id, repo, kind, status, detail)
                     VALUES ('agent', $1, 'google_docs', 'status', 'done', 'No Google Docs found to ingest')",
                    owner.id(),
                )
                .execute(&db)
                .await;
            }
            Err(e) => {
                tracing::warn!("google docs ingestion failed for {}: {e:?}", owner.id());
                let _ = sqlx::query!(
                    "INSERT INTO ingestion_events (owner_kind, agent_id, repo, kind, status, detail)
                     VALUES ('agent', $1, 'google_docs', 'status', 'failed', $2)",
                    owner.id(),
                    format!("Google Docs ingestion failed: {e}"),
                )
                .execute(&db)
                .await;
            }
        }
    });

    Ok(Json(serde_json::json!({ "started": true })))
}

#[derive(Serialize)]
struct RepoIngestStatus {
    repo: String,
    status: String, // running | done | failed
    notes_count: i32,
    detail: String,
}

#[derive(Serialize)]
struct IngestionStatusResponse {
    complete: bool,   // the whole run finished (all repos)
    total_notes: i64, // distinct notes written across repos
    repos: Vec<RepoIngestStatus>,
}

/// GET /api/v1/onboarding/ingestion/status/:agent_id
/// Snapshot of agentic code-ingestion progress: latest status per repo, note counts, and whether
/// the whole run has completed. Owner-only. Pair with the SSE feed for live updates.
async fn ingestion_status(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> Result<Json<IngestionStatusResponse>, AppError> {
    crate::services::authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;

    // Latest per-repo lifecycle status (excluding the overall-complete sentinel, repo='').
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (repo) repo, status, detail, notes_count
           FROM ingestion_events
           WHERE owner_kind = 'agent' AND agent_id = $1 AND kind = 'status' AND repo <> ''
           ORDER BY repo, created_at DESC"#,
        agent_id,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let repos: Vec<RepoIngestStatus> = rows
        .into_iter()
        .map(|r| RepoIngestStatus {
            repo: r.repo,
            status: r.status.unwrap_or_else(|| "running".into()),
            notes_count: r.notes_count,
            detail: r.detail,
        })
        .collect();

    let complete = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM ingestion_events
                       WHERE owner_kind = 'agent' AND agent_id = $1 AND kind = 'status' AND status = 'complete')",
        agent_id,
    )
    .fetch_one(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?
    .unwrap_or(false);

    // Authoritative note total: count the actual vault notes sourced from ingestion, not the
    // event tally (which can double-count on agent retries).
    let total_notes = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM vault_notes WHERE owner_kind = 'agent' AND agent_id = $1 AND repo IS NOT NULL",
        agent_id,
    )
    .fetch_one(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?
    .unwrap_or(0);

    Ok(Json(IngestionStatusResponse {
        complete,
        total_notes,
        repos,
    }))
}

/// POST /api/v1/onboarding/ingestion/cancel/:agent_id
/// Stop ingestion: (1) flag the background loop to not start further repos, and (2) kill the live
/// managed session + orphan environments so nothing keeps running/costing. Notes already written
/// to the vault are preserved — you can proceed straight to interview/training. Owner-only.
async fn cancel_ingestion(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::services::authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;

    // 1. Tell the background loop to stop before the next repo.
    crate::services::onboarding::ingest::request_cancel(agent_id);

    // 2. Kill the live session(s) + orphan environments now (interrupts the in-flight repo too).
    let killed = state.backends.kill_managed_runs().await;

    // 3. Mark the run complete in the feed so the UI unblocks and lets you continue.
    let _ = sqlx::query!(
        "INSERT INTO ingestion_events (owner_kind, agent_id, repo, kind, status, detail)
         VALUES ('agent', $1, '', 'status', 'complete', 'Ingestion stopped by request')",
        agent_id,
    )
    .execute(&state.db)
    .await;

    Ok(Json(serde_json::json!({
        "status": "cancelling",
        "sessions_killed": killed,
        "note": "Notes already in the vault are preserved; you can continue to interview/training."
    })))
}

/// GET /api/v1/onboarding/scenarios/:profession?aspect=<slug>
/// Returns interview scenarios for the profession, optionally scoped to an aspect.
#[derive(Deserialize)]
struct ScenariosQuery {
    aspect: Option<String>,
}

#[derive(Serialize)]
struct ScenariosResponse {
    scenarios: Vec<serde_json::Value>,
}

async fn get_scenarios(
    State(state): State<AppState>,
    Path(profession): Path<String>,
    Query(q): Query<ScenariosQuery>,
) -> Result<Json<ScenariosResponse>, AppError> {
    let aspect = q.aspect.as_deref();
    // Debugging gets more scenarios since it can't be inferred from GitHub
    let count = match aspect {
        Some("debugging") => 6,
        Some(_) => 3,
        None => 20,
    };
    let service = InterviewService::new(&state.inference);
    let scenarios = service
        .generate_scenarios(&profession, aspect, count)
        .await?;
    let serialized = serde_json::to_value(&scenarios)?;
    let scenarios_arr = serialized.as_array().cloned().unwrap_or_default();
    Ok(Json(ScenariosResponse {
        scenarios: scenarios_arr,
    }))
}

/// GET /api/v1/onboarding/interview/questions/:profession?aspect=<slug>
/// The slim, open-form interview: a few high-signal questions about communication & reasoning
/// style (the part code can't reveal). Fewer than the A/B/C battery by design.
#[derive(Serialize)]
struct OpenQuestionsResponse {
    questions: Vec<serde_json::Value>,
}

async fn get_open_questions(
    State(state): State<AppState>,
    Path(profession): Path<String>,
    Query(q): Query<ScenariosQuery>,
) -> Result<Json<OpenQuestionsResponse>, AppError> {
    let aspect = q.aspect.as_deref();
    // Deliberately few: open-form answers are high-signal, so 3 aspect-scoped / 5 overall.
    let count = if aspect.is_some() { 3 } else { 5 };
    let service = InterviewService::new(&state.inference);
    let questions = service
        .generate_open_questions(&profession, aspect, count)
        .await?;
    let arr = serde_json::to_value(&questions)?
        .as_array()
        .cloned()
        .unwrap_or_default();
    Ok(Json(OpenQuestionsResponse { questions: arr }))
}

/// POST /api/v1/onboarding/interview/open/submit
/// Body: { "agent_id": "<uuid>", "answers": [{ question_id, prompt, answer }] }
/// Each answer becomes a style-rule vault note (source of truth → derived chunk for RAG + LoRA).
#[derive(Deserialize)]
struct SubmitOpenInterviewRequest {
    agent_id: Uuid,
    answers: Vec<OpenAnswer>,
}

#[derive(Serialize)]
struct SubmitOpenInterviewResponse {
    agent_id: Uuid,
    notes_captured: usize,
}

async fn submit_open_interview(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SubmitOpenInterviewRequest>,
) -> Result<Json<SubmitOpenInterviewResponse>, AppError> {
    use crate::services::authz;
    use crate::services::knowledge::{ChunkVisibility, KnowledgeSourceType};
    use crate::services::vault::{self, NoteLayer, VaultNote};
    authz::require_agent_owner(&state.db, auth.user_id, body.agent_id).await?;

    let mut captured = 0usize;
    for a in &body.answers {
        if a.answer.trim().is_empty() {
            continue;
        }
        let note = VaultNote {
            id: Uuid::new_v4(),
            agent_id: body.agent_id,
            owner_kind: "agent".into(),
            slug: format!("style-{}", a.question_id.simple()),
            title: a.prompt.chars().take(80).collect(),
            body: format!("Q: {}\nA: {}", a.prompt, a.answer),
            layer: NoteLayer::StyleRule,
            source_type: KnowledgeSourceType::InterviewNote,
            visibility: ChunkVisibility::Private,
            domain: None,
            related: vec![],
            repo: None,
            job_id: None,
        };
        if vault::upsert_note(&state.db, &state.qdrant, &state.inference, note)
            .await
            .is_ok()
        {
            captured += 1;
        }
    }

    // NOTE: training is NOT triggered here. LoRA training is now a deliberate, manual action the
    // professional takes when ready (the "Train agent" button → POST /agents/:id/retrain), not a
    // side effect of finishing the interview. The interview just enriches the vault with
    // style-rule notes; the professional can also re-run it anytime without starting a training run.
    Ok(Json(SubmitOpenInterviewResponse {
        agent_id: body.agent_id,
        notes_captured: captured,
    }))
}

/// POST /api/v1/onboarding/interview/submit
/// Body: { "agent_id": "<uuid>", "aspect_ids": [...], "scenarios": [...], "responses": [...] }
#[derive(Deserialize)]
struct SubmitInterviewRequest {
    agent_id: Uuid,
    #[serde(default)]
    aspect_ids: Vec<Uuid>,
    scenarios: Vec<serde_json::Value>,
    responses: Vec<InterviewResponse>,
}

#[derive(Serialize)]
struct SubmitInterviewResponse {
    agent_id: Uuid,
    preference_pairs_captured: usize,
}

async fn submit_interview(
    State(state): State<AppState>,
    _auth: AuthUser,
    Json(body): Json<SubmitInterviewRequest>,
) -> Result<Json<SubmitInterviewResponse>, AppError> {
    use crate::services::onboarding::interview::{InterviewScenario, InterviewService};

    let scenarios: Vec<InterviewScenario> =
        serde_json::from_value(serde_json::Value::Array(body.scenarios))?;

    let pairs = InterviewService::build_preference_pairs(&scenarios, &body.responses);
    let pairs_count = pairs.len();

    // Store the captured preferences as style-rule vault notes (source of truth → derived
    // chunk for RAG + LoRA dataset). Each pair is a discrete note tagged by aspect.
    use crate::services::knowledge::{ChunkVisibility, KnowledgeSourceType};
    use crate::services::vault::{self, NoteLayer, VaultNote};

    // Build a map from scenario description → aspect_slug for tagging
    let scenario_aspect: std::collections::HashMap<&str, &str> = scenarios
        .iter()
        .filter_map(|s| {
            s.aspect_slug
                .as_deref()
                .map(|slug| (s.description.as_str(), slug))
        })
        .collect();

    for p in &pairs {
        let base = format!(
            "Scenario: {}\nChosen approach: {}\nCritique: {}",
            p.scenario, p.chosen, p.critique
        );
        let aspect = scenario_aspect.get(p.scenario.as_str()).copied();
        let body_text = match aspect {
            Some(slug) => format!("Aspect: {}\n{}", slug, base),
            None => base,
        };
        let note = VaultNote {
            id: Uuid::new_v4(),
            agent_id: body.agent_id,
            owner_kind: "agent".into(),
            slug: format!("interview-pref-{}", Uuid::new_v4().simple()),
            title: p.scenario.chars().take(80).collect(),
            body: body_text,
            layer: NoteLayer::StyleRule,
            source_type: KnowledgeSourceType::Portfolio,
            visibility: ChunkVisibility::Private,
            domain: None,
            related: vec![],
            repo: None,
            job_id: None,
        };
        let _ = vault::upsert_note(&state.db, &state.qdrant, &state.inference, note).await;
    }

    // Populate agent_aspects for all aspects the professional completed
    for aspect_id in &body.aspect_ids {
        sqlx::query!(
            "INSERT INTO agent_aspects (agent_id, aspect_id, is_offered)
             VALUES ($1, $2, true)
             ON CONFLICT (agent_id, aspect_id) DO UPDATE SET is_offered = true",
            body.agent_id,
            aspect_id
        )
        .execute(&state.db)
        .await?;
    }

    // Training is manual now (see submit_open_interview) — completing the interview no longer
    // auto-starts a LoRA run. The professional triggers training explicitly when ready.
    Ok(Json(SubmitInterviewResponse {
        agent_id: body.agent_id,
        preference_pairs_captured: pairs_count,
    }))
}

/// POST /api/v1/onboarding/lora/complete/:secret
/// Called by RunPod serverless worker via webhook when LoRA training finishes.
/// RunPod wraps the worker's return value inside `output`.
///
/// Authenticated via the `RunPodSecret` extractor (constant-time path-secret check), which
/// runs BEFORE the `Json` body is parsed so payload-shape errors don't leak to unauthenticated
/// callers. Without `RUNPOD_WEBHOOK_SECRET` set, all callbacks are rejected with 401.
async fn lora_complete(
    State(state): State<AppState>,
    _auth: RunPodSecret,
    Json(body): Json<crate::services::lora_training::RunPodWebhookPayload>,
) -> Result<Json<serde_json::Value>, AppError> {
    let agent_id = body.output.agent_id;

    // ── Idempotency: if this agent is already ready, this is a RunPod retry of a callback we
    // already processed. Return 200 immediately without re-downloading from R2 or re-uploading
    // to Cloudflare. RunPod retries up to 2× if it doesn't get a fast 200; the R2→CF upload
    // takes ~5s, so without this guard retries stack and create duplicate CF finetunes.
    let already_ready = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM agents WHERE id = $1 AND status = 'ready')"#,
        agent_id
    )
    .fetch_one(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or(false);
    if already_ready {
        tracing::info!(
            "RunPod webhook for agent {} ignored (already ready — duplicate callback)",
            agent_id
        );
        return Ok(Json(
            serde_json::json!({ "ok": true, "deduplicated": true }),
        ));
    }

    // Check if RunPod reported a failure
    if let Some(ref status) = body.status {
        if status != "COMPLETED" {
            let err = body.output.error.as_deref().unwrap_or("unknown error");
            tracing::error!(
                "RunPod training job failed for agent {}: status={} error={}",
                agent_id,
                status,
                err
            );
            // Mark agent ready without LoRA so it can still function
            crate::services::lora_training::mark_ready(&state.db, agent_id, String::new())
                .await
                .map_err(AppError::Internal)?;
            return Ok(Json(serde_json::json!({ "ok": false, "error": err })));
        }
    }

    // Check worker-level error
    if let Some(ref err) = body.output.error {
        tracing::error!(
            "RunPod worker returned error for agent {}: {}",
            agent_id,
            err
        );
        crate::services::lora_training::mark_ready(&state.db, agent_id, String::new())
            .await
            .map_err(AppError::Internal)?;
        return Ok(Json(serde_json::json!({ "ok": false, "error": err })));
    }

    let adapter_key = body.output.adapter_key.ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("Missing adapter_key in RunPod webhook"))
    })?;

    // Claim this callback atomically: flip the agent to `ready` ONLY if it isn't already, and
    // bail out if another (retry) callback already claimed it. This guarantees the slow CF upload
    // below runs at most once even under RunPod's retries — the previous version re-ran the upload
    // every retry because it marked ready only AFTER the upload (which could fail), so the dedup
    // guard never engaged and finetunes piled up.
    let claimed = sqlx::query_scalar!(
        r#"UPDATE agents SET status = 'ready', updated_at = NOW()
           WHERE id = $1 AND status <> 'ready'
           RETURNING id"#,
        agent_id
    )
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?
    .is_some();

    if !claimed {
        tracing::info!(
            "RunPod webhook for agent {agent_id} ignored (already claimed by a prior callback)"
        );
        return Ok(Json(
            serde_json::json!({ "ok": true, "deduplicated": true }),
        ));
    }

    // Now do the best-effort CF upload + persist the finetune id. The agent is ALREADY ready, so
    // even if this fails the agent functions (without LoRA personalization) and RunPod won't loop.
    if let Err(e) =
        crate::services::lora_training::handle_training_complete(&state.db, agent_id, adapter_key)
            .await
    {
        tracing::error!(
            "CF upload failed for agent {agent_id} (agent already marked ready): {e:?}"
        );
    }

    // Notify the owner — best-effort. If this errors (e.g. duplicate notification from a
    // concurrent retry), we still return 200 because the agent IS ready; failing the response
    // would make RunPod retry and re-upload to Cloudflare. The status update already committed.
    if let Err(e) = crate::services::notifications::notify_agent_ready(&state.db, agent_id).await {
        tracing::warn!(
            "notify_agent_ready failed for agent {} (non-fatal, agent is ready): {:?}",
            agent_id,
            e
        );
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Verify the RunPod webhook shared secret in constant time. The secret is supplied via the
/// URL path (`/onboarding/lora/complete/{secret}`) because RunPod webhooks are not signed.
/// Without `RUNPOD_WEBHOOK_SECRET` set, all callbacks are rejected.
fn verify_runpod_secret(provided: &str) -> Result<(), AppError> {
    let expected = std::env::var("RUNPOD_WEBHOOK_SECRET").map_err(|_| AppError::Unauthorized)?;
    if expected.is_empty() {
        return Err(AppError::Unauthorized);
    }
    // Constant-time comparison to avoid timing-based secret recovery.
    if subtle::ConstantTimeEq::ct_eq(provided.as_bytes(), expected.as_bytes()).into() {
        Ok(())
    } else {
        Err(AppError::Unauthorized)
    }
}
