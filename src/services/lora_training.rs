/// LoRA training pipeline — RunPod Serverless + Cloudflare Workers AI.
///
/// Flow:
///   1. Professional completes onboarding interview
///   2. `trigger_training()` POSTs a job to a RunPod serverless endpoint
///      (a Docker worker running Axolotl) — pays only for GPU seconds used
///   3. RunPod POSTs back to `/onboarding/lora/complete` when done
///   4. Webhook handler downloads adapter from R2, uploads to Cloudflare Workers AI,
///      stores the `cf_finetune_id` on the agent row
///   5. All subsequent inference calls pass `cf_finetune_id` as the `lora` parameter
///
/// If RUNPOD_API_KEY is not set, the agent is immediately marked ready with no
/// finetune_id so the rest of the platform works without GPU infra during dev.
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use crate::models::agent::AgentStatus;
use crate::services::inference::InferenceClient;
use crate::services::knowledge::{self, ChunkVisibility, KnowledgeChunk, KnowledgeSourceType};
use qdrant_client::Qdrant;
use std::sync::Arc;

/// Which knowledge-base chunks to include in a training run. All-None = every chunk
/// (the original behavior). Filters compose: a chunk must pass every set constraint.
#[derive(Debug, Clone, Default)]
pub struct DatasetFilter {
    pub source_types: Option<Vec<KnowledgeSourceType>>,
    pub chunk_ids: Option<Vec<Uuid>>,
    pub visibility: Option<ChunkVisibility>,
}

impl DatasetFilter {
    fn matches(&self, c: &KnowledgeChunk) -> bool {
        if let Some(types) = &self.source_types {
            if !types.contains(&c.source_type) {
                return false;
            }
        }
        if let Some(ids) = &self.chunk_ids {
            if !ids.contains(&c.id) {
                return false;
            }
        }
        if let Some(vis) = &self.visibility {
            if &c.visibility != vis {
                return false;
            }
        }
        true
    }
}

/// Turn a knowledge chunk into an instruction/output training pair (shared by training
/// and preview). Mirrors the original aspect-prefix parsing.
fn chunk_to_pair(c: &KnowledgeChunk) -> TrainingPair {
    let (instruction, output) = if let Some(rest) = c.content.strip_prefix("Aspect: ") {
        if let Some(newline_pos) = rest.find('\n') {
            let aspect = &rest[..newline_pos];
            let body = &rest[newline_pos + 1..];
            (aspect_instruction(aspect, body), body.to_string())
        } else {
            (generic_instruction(&c.content), c.content.clone())
        }
    } else {
        (generic_instruction(&c.content), c.content.clone())
    };
    TrainingPair {
        instruction,
        output,
    }
}

/// Assemble the filtered training dataset for an agent from its knowledge base.
pub async fn assemble_dataset(
    qdrant: &Arc<Qdrant>,
    agent_id: Uuid,
    filter: &DatasetFilter,
) -> Result<Vec<TrainingPair>> {
    let chunks = knowledge::list_chunks_for_agent(qdrant, agent_id).await?;
    Ok(chunks
        .iter()
        .filter(|c| filter.matches(c))
        .map(chunk_to_pair)
        .collect())
}

/// A preview row for the retraining-selection UI: shows what would be included without
/// kicking off a training run.
#[derive(Serialize)]
pub struct ChunkPreview {
    pub id: Uuid,
    pub source_type: String,
    pub visibility: String,
    pub content_preview: String,
}

/// Preview which chunks a filter would include (no RunPod call).
pub async fn preview_dataset(
    qdrant: &Arc<Qdrant>,
    agent_id: Uuid,
    filter: &DatasetFilter,
) -> Result<Vec<ChunkPreview>> {
    let chunks = knowledge::list_chunks_for_agent(qdrant, agent_id).await?;
    Ok(chunks
        .into_iter()
        .filter(|c| filter.matches(c))
        .map(|c| ChunkPreview {
            id: c.id,
            source_type: format!("{:?}", c.source_type).to_lowercase(),
            visibility: match c.visibility {
                ChunkVisibility::Shareable => "shareable".into(),
                ChunkVisibility::Private => "private".into(),
            },
            content_preview: c.content.chars().take(200).collect(),
        })
        .collect())
}

// ── RunPod request/response ───────────────────────────────────────────────────

#[derive(Serialize)]
struct RunPodInput {
    dataset: Vec<TrainingPair>,
    base_model: String,
    lora_rank: u8,
    r2_bucket: String,
    r2_endpoint: String,
    r2_access_key: String,
    r2_secret_key: String,
    agent_id: String,
    webhook_url: String,
}

#[derive(Serialize)]
struct RunPodRequest {
    input: RunPodInput,
    /// RunPod's webhooks are not signed. We embed a high-entropy secret in the webhook URL
    /// path and verify it in constant time on receipt, so forged completions can't flip
    /// agents ready or trigger R2→Cloudflare uploads. RunPod posts to this URL verbatim.
    webhook: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TrainingPair {
    pub instruction: String,
    pub output: String,
}

#[derive(Deserialize)]
struct RunPodResponse {
    id: String, // RunPod job id
}

// ── RunPod webhook payload ────────────────────────────────────────────────────

/// Payload sent by RunPod when training completes.
#[derive(Deserialize)]
pub struct RunPodWebhookPayload {
    pub status: Option<String>,
    pub output: RunPodOutput,
}

#[derive(Deserialize)]
pub struct RunPodOutput {
    pub agent_id: Uuid,
    /// R2 key for adapter_model.safetensors, e.g. "adapters/{agent_id}/adapter_model.safetensors"
    pub adapter_key: Option<String>,
    pub error: Option<String>,
}

// ── Cloudflare API types ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CfFinetuneCreateResponse {
    result: CfFinetuneResult,
}

#[derive(Deserialize)]
struct CfFinetuneResult {
    id: String,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Train/retrain using the entire knowledge base (no filter). Convenience wrapper over
/// `trigger_training_with_filter`. Not currently called now that training is manual via the
/// retrain route (which passes a filter), but kept as the canonical full-KB entry point.
#[allow(dead_code)]
pub async fn trigger_training(
    db: &PgPool,
    inference: &InferenceClient,
    agent_id: Uuid,
) -> Result<(), AppError> {
    trigger_training_with_filter(db, inference, agent_id, DatasetFilter::default()).await
}

/// Retrain using a filtered subset of the knowledge base (owner-selected components).
pub async fn trigger_training_with_filter(
    db: &PgPool,
    inference: &InferenceClient,
    agent_id: Uuid,
    filter: DatasetFilter,
) -> Result<(), AppError> {
    sqlx::query("UPDATE agents SET status = $1, updated_at = NOW() WHERE id = $2")
        .bind(AgentStatus::Training)
        .bind(agent_id)
        .execute(db)
        .await?;

    let db_clone = db.clone();
    let inference_clone = inference.clone();
    tokio::spawn(async move {
        if let Err(e) = run_training_job(&db_clone, &inference_clone, agent_id, filter).await {
            tracing::error!("LoRA training job failed for agent {}: {:?}", agent_id, e);
        }
    });

    Ok(())
}

fn generic_instruction(content: &str) -> String {
    format!(
        "You are an expert professional. Given the following context, respond as this professional would:\n\n{}",
        content
    )
}

fn aspect_instruction(aspect: &str, body: &str) -> String {
    let verb = match aspect {
        "code_review" => "performing a code review. Review the following",
        "refactoring" => "refactoring code. Improve the following",
        "system_design" => "designing a system. Architect the following",
        "documentation" => "writing documentation. Document the following",
        "debugging" => "debugging a problem. Diagnose the following",
        _ => return generic_instruction(body),
    };
    format!(
        "You are {}  in this professional's exact style:\n\n{}",
        verb, body
    )
}

async fn run_training_job(
    db: &PgPool,
    _inference: &InferenceClient,
    agent_id: Uuid,
    filter: DatasetFilter,
) -> Result<()> {
    let runpod_key = match std::env::var("RUNPOD_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            tracing::info!(
                "RUNPOD_API_KEY not set; skipping LoRA training for agent {}. Marking ready.",
                agent_id
            );
            mark_ready_no_lora(db, agent_id).await?;
            return Ok(());
        }
    };

    let endpoint_id =
        std::env::var("RUNPOD_ENDPOINT_ID").context("RUNPOD_ENDPOINT_ID must be set")?;
    let r2_bucket = std::env::var("R2_BUCKET").context("R2_BUCKET must be set")?;
    let r2_endpoint = std::env::var("R2_ENDPOINT").context("R2_ENDPOINT must be set")?;
    let r2_access_key = std::env::var("R2_ACCESS_KEY").context("R2_ACCESS_KEY must be set")?;
    let r2_secret_key = std::env::var("R2_SECRET_KEY").context("R2_SECRET_KEY must be set")?;
    let api_base =
        std::env::var("API_BASE_URL").unwrap_or_else(|_| "https://api.flaborful.com".into());

    // Build dataset from Qdrant knowledge chunks
    let qdrant_url = std::env::var("QDRANT_URL").context("QDRANT_URL must be set")?;
    let qdrant_key = std::env::var("QDRANT_API_KEY").context("QDRANT_API_KEY must be set")?;
    let qdrant = Arc::new(
        qdrant_client::Qdrant::from_url(&qdrant_url)
            .api_key(qdrant_key)
            .build()?,
    );

    let dataset = assemble_dataset(&qdrant, agent_id, &filter).await?;
    if dataset.is_empty() {
        return Err(anyhow!(
            "No knowledge chunks match the training selection for agent {}",
            agent_id
        ));
    }

    // Build the webhook URL. RunPod's webhooks are unsigned, so a shared secret rides in the
    // path and is verified in constant time at the handler. If the secret isn't configured,
    // the callback will 401 and the agent stays in `training` — so we surface that here as a
    // hard error rather than silently submitting a job that can never complete.
    let webhook_secret = std::env::var("RUNPOD_WEBHOOK_SECRET")
        .map_err(|_| anyhow!("RUNPOD_WEBHOOK_SECRET must be set when RUNPOD_API_KEY is set (RunPod webhooks are unsigned; the secret rides in the callback URL path)"))?;
    let webhook_url = format!(
        "{}/api/v1/onboarding/lora/complete/{}",
        api_base, webhook_secret
    );

    let body = RunPodRequest {
        input: RunPodInput {
            dataset,
            base_model: "mistralai/Mistral-7B-Instruct-v0.2".into(),
            lora_rank: 16,
            r2_bucket,
            r2_endpoint,
            r2_access_key,
            r2_secret_key,
            agent_id: agent_id.to_string(),
            webhook_url: webhook_url.clone(),
        },
        webhook: webhook_url,
    };

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("https://api.runpod.ai/v2/{}/run", endpoint_id))
        .bearer_auth(&runpod_key)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json::<RunPodResponse>()
        .await?;

    sqlx::query!(
        "UPDATE agents SET lora_training_job_id = $1, updated_at = NOW() WHERE id = $2",
        resp.id,
        agent_id,
    )
    .execute(db)
    .await?;

    tracing::info!(
        "RunPod training job {} submitted for agent {}",
        resp.id,
        agent_id
    );

    Ok(())
}

// ── Cloudflare adapter upload ─────────────────────────────────────────────────

/// Downloads the adapter from R2 and uploads it to Cloudflare Workers AI.
/// Returns the Cloudflare `finetune_id` to store on the agent.
pub async fn upload_adapter_to_cloudflare(adapter_key: &str, agent_id: Uuid) -> Result<String> {
    let cf_account_id = std::env::var("CF_ACCOUNT_ID").context("CF_ACCOUNT_ID must be set")?;
    let cf_api_token = std::env::var("CF_API_TOKEN").context("CF_API_TOKEN must be set")?;
    let r2_bucket = std::env::var("R2_BUCKET").context("R2_BUCKET must be set")?;
    let r2_endpoint = std::env::var("R2_ENDPOINT").context("R2_ENDPOINT must be set")?;
    let r2_access_key = std::env::var("R2_ACCESS_KEY").context("R2_ACCESS_KEY must be set")?;
    let r2_secret_key = std::env::var("R2_SECRET_KEY").context("R2_SECRET_KEY must be set")?;

    let client = reqwest::Client::new();

    // ── Step 1: Download safetensors from R2 ─────────────────────────────────
    let safetensors_key = adapter_key.to_string();
    let config_key = adapter_key.replace("adapter_model.safetensors", "adapter_config.json");

    let safetensors_bytes = download_from_r2(
        &client,
        &r2_endpoint,
        &r2_bucket,
        &r2_access_key,
        &r2_secret_key,
        &safetensors_key,
    )
    .await
    .context("Failed to download adapter_model.safetensors from R2")?;

    // adapter_config.json must declare model_type = llama (Qwen2.5 is llama-arch)
    let config_json = serde_json::json!({
        "base_model_name_or_path": "mistralai/Mistral-7B-Instruct-v0.2",
        "model_type": "mistral",
        "peft_type": "LORA",
        "task_type": "CAUSAL_LM",
        "r": 16,
        "lora_alpha": 32,
        "lora_dropout": 0.05,
        "bias": "none"
    });
    let config_bytes = serde_json::to_vec(&config_json)?;

    // Try to get config from R2 first; fall back to the generated one above. Cloudflare REQUIRES
    // `model_type` (mistral|gemma|llama) in adapter_config.json — a PEFT-exported config from
    // training does NOT include it, and uploading without it makes Cloudflare silently store an
    // invalid finetune that later 400s with "Finetune is missing required files (model.safetensors
    // and config.json)". So if we use the R2 config, ensure model_type (and the base model) are set.
    let final_config_bytes = match download_from_r2(
        &client,
        &r2_endpoint,
        &r2_bucket,
        &r2_access_key,
        &r2_secret_key,
        &config_key,
    )
    .await
    {
        Ok(b) => ensure_cf_required_config_fields(b).unwrap_or(config_bytes),
        Err(_) => {
            tracing::warn!(
                "adapter_config.json not found in R2 for agent {}; using generated config",
                agent_id
            );
            config_bytes
        }
    };

    // ── Step 2: Create finetune record on Cloudflare ──────────────────────────
    let create_resp = client
        .post(format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/ai/finetunes",
            cf_account_id
        ))
        .bearer_auth(&cf_api_token)
        .json(&serde_json::json!({
            "model": "@cf/mistral/mistral-7b-instruct-v0.2-lora",
            "name": format!("agent-{}-{}", agent_id, chrono::Utc::now().timestamp()),
            "description": format!("LoRA adapter for Flaborful agent {}", agent_id)
        }))
        .send()
        .await?
        .error_for_status()?
        .json::<CfFinetuneCreateResponse>()
        .await?;

    let finetune_id = create_resp.result.id;
    tracing::info!(
        "Created Cloudflare finetune {} for agent {}",
        finetune_id,
        agent_id
    );

    // ── Step 3: Upload adapter_model.safetensors ──────────────────────────────
    upload_finetune_asset(
        &client,
        &cf_account_id,
        &cf_api_token,
        &finetune_id,
        "adapter_model.safetensors",
        safetensors_bytes,
        "application/octet-stream",
    )
    .await
    .context("Failed to upload adapter_model.safetensors to Cloudflare")?;

    // ── Step 4: Upload adapter_config.json ────────────────────────────────────
    upload_finetune_asset(
        &client,
        &cf_account_id,
        &cf_api_token,
        &finetune_id,
        "adapter_config.json",
        final_config_bytes,
        "application/json",
    )
    .await
    .context("Failed to upload adapter_config.json to Cloudflare")?;

    tracing::info!(
        "Adapter fully uploaded to Cloudflare finetune {} for agent {}",
        finetune_id,
        agent_id
    );

    Ok(finetune_id)
}

async fn download_from_r2(
    _client: &reqwest::Client,
    r2_endpoint: &str,
    bucket: &str,
    access_key: &str,
    secret_key: &str,
    key: &str,
) -> Result<Vec<u8>> {
    use aws_credential_types::Credentials;
    use aws_sdk_s3::config::{Builder, Region};
    use aws_sdk_s3::Client as S3Client;

    let creds = Credentials::new(access_key, secret_key, None, None, "static");
    let config = Builder::new()
        .endpoint_url(r2_endpoint)
        .credentials_provider(creds)
        .region(Region::new("auto"))
        .force_path_style(true)
        .behavior_version_latest()
        .build();

    let s3 = S3Client::from_conf(config);
    let resp = s3
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .context("S3 GetObject failed")?;

    let bytes = resp
        .body
        .collect()
        .await
        .context("Failed to read S3 body")?
        .into_bytes();

    Ok(bytes.to_vec())
}

/// Delete an object from the R2 bucket. Best-effort: a missing key is not an error.
async fn delete_from_r2(
    r2_endpoint: &str,
    bucket: &str,
    access_key: &str,
    secret_key: &str,
    key: &str,
) -> Result<()> {
    use aws_credential_types::Credentials;
    use aws_sdk_s3::config::{Builder, Region};
    use aws_sdk_s3::Client as S3Client;

    let creds = Credentials::new(access_key, secret_key, None, None, "static");
    let config = Builder::new()
        .endpoint_url(r2_endpoint)
        .credentials_provider(creds)
        .region(Region::new("auto"))
        .force_path_style(true)
        .behavior_version_latest()
        .build();

    let s3 = S3Client::from_conf(config);
    s3.delete_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .context("S3 DeleteObject failed")?;
    Ok(())
}

/// Soft-reset an agent so the owner can rebuild it while KEEPING the knowledge base.
/// Deletes the LoRA artifacts from R2, clears training state, and returns the agent to
/// `onboarding`. The agent row and its id are retained, so the KB (keyed by agent_id in
/// Qdrant) survives untouched and the new training run produces a fresh adapter on the
/// same agent.
pub async fn reset_agent(db: &PgPool, agent_id: Uuid) -> Result<()> {
    // Best-effort R2 cleanup — only if R2 is configured. Adapter files live under
    // adapters/{agent_id}/. Missing files are fine.
    if let (Ok(bucket), Ok(endpoint), Ok(access), Ok(secret)) = (
        std::env::var("R2_BUCKET"),
        std::env::var("R2_ENDPOINT"),
        std::env::var("R2_ACCESS_KEY"),
        std::env::var("R2_SECRET_KEY"),
    ) {
        for key in [
            format!("adapters/{}/adapter_model.safetensors", agent_id),
            format!("adapters/{}/adapter_config.json", agent_id),
        ] {
            if let Err(e) = delete_from_r2(&endpoint, &bucket, &access, &secret, &key).await {
                tracing::warn!("R2 delete failed for {}: {:?}", key, e);
            }
        }
    }

    sqlx::query(
        r#"UPDATE agents
           SET status = $1, lora_adapter_path = NULL, lora_training_job_id = NULL,
               cf_finetune_id = NULL, deleted_at = NOW(), updated_at = NOW()
           WHERE id = $2"#,
    )
    .bind(AgentStatus::Onboarding)
    .bind(agent_id)
    .execute(db)
    .await?;

    Ok(())
}

/// Ensure an R2-sourced `adapter_config.json` carries the fields Cloudflare Workers AI requires
/// before it will accept the finetune: `model_type` (mistral|gemma|llama) and the base model path.
/// PEFT exports omit `model_type`, which is the actual cause of the "missing required files" 400.
/// Returns the patched JSON bytes, or None if the input wasn't parseable JSON (caller falls back
/// to its generated config).
fn ensure_cf_required_config_fields(bytes: Vec<u8>) -> Option<Vec<u8>> {
    let mut cfg: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let obj = cfg.as_object_mut()?;
    obj.entry("model_type")
        .or_insert_with(|| serde_json::Value::String("mistral".into()));
    obj.entry("base_model_name_or_path")
        .or_insert_with(|| serde_json::Value::String("mistralai/Mistral-7B-Instruct-v0.2".into()));
    serde_json::to_vec(&cfg).ok()
}

async fn upload_finetune_asset(
    client: &reqwest::Client,
    account_id: &str,
    api_token: &str,
    finetune_id: &str,
    file_name: &str,
    bytes: Vec<u8>,
    content_type: &str,
) -> Result<()> {
    use reqwest::multipart;

    let part = multipart::Part::bytes(bytes)
        .file_name(file_name.to_string())
        .mime_str(content_type)?;

    let form = multipart::Form::new()
        .part("file_name", multipart::Part::text(file_name.to_string()))
        .part("file", part);

    client
        .post(format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/ai/finetunes/{}/finetune-assets",
            account_id, finetune_id
        ))
        .bearer_auth(api_token)
        .multipart(form)
        .send()
        .await?
        .error_for_status()?;

    Ok(())
}

// ── Webhook completion handler ────────────────────────────────────────────────

/// Called from the webhook route when RunPod finishes training.
/// Downloads from R2, uploads to Cloudflare, marks agent ready.
pub async fn handle_training_complete(
    db: &PgPool,
    agent_id: Uuid,
    adapter_key: String,
) -> Result<()> {
    match upload_adapter_to_cloudflare(&adapter_key, agent_id).await {
        Ok(finetune_id) => {
            // Persist ready + the CF finetune id. Use the runtime `sqlx::query` (not the
            // `query!` macro) because the macro's offline type-checker chokes on the
            // `agent_status` custom enum param ("unsupported type agent_status"), which
            // previously forced a `as AgentStatus` cast that bound the *integer* discriminant
            // and silently failed the UPDATE at runtime. The runtime query uses sqlx's
            // `Encode` impl which writes the enum by name ("ready") correctly.
            sqlx::query(
                r#"UPDATE agents
                   SET status = $1, cf_finetune_id = $2, lora_adapter_path = $3, updated_at = NOW()
                   WHERE id = $4"#,
            )
            .bind(AgentStatus::Ready)
            .bind(&finetune_id)
            .bind(&adapter_key)
            .bind(agent_id)
            .execute(db)
            .await
            .map_err(|e| {
                anyhow!(
                    "failed to mark agent {} ready after CF upload: {}",
                    agent_id,
                    e
                )
            })?;
            tracing::info!(
                "Agent {} is ready with Cloudflare finetune {}",
                agent_id,
                finetune_id
            );
        }
        Err(e) => {
            tracing::error!(
                "Cloudflare upload failed for agent {}: {:?}. Marking ready without LoRA.",
                agent_id,
                e
            );
            mark_ready_no_lora(db, agent_id).await?;
        }
    }
    Ok(())
}

async fn mark_ready_no_lora(db: &PgPool, agent_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE agents SET status = $1, updated_at = NOW() WHERE id = $2")
        .bind(AgentStatus::Ready)
        .bind(agent_id)
        .execute(db)
        .await?;
    Ok(())
}

#[allow(dead_code)]
pub async fn mark_ready(db: &PgPool, agent_id: Uuid, adapter_path: String) -> Result<()> {
    handle_training_complete(db, agent_id, adapter_path).await
}
