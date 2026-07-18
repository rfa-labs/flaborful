use anyhow::{anyhow, Result};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::models::workflow::WorkflowGraph;
use crate::services::inference::InferenceClient;
use crate::services::workflow_engine::{ExecutionContext, WorkflowEngine};
use qdrant_client::Qdrant;

pub struct TaskRunner {
    db: PgPool,
    qdrant: Arc<Qdrant>,
    inference: InferenceClient,
    backends: crate::services::execution::ExecutionBackends,
}

impl TaskRunner {
    pub fn new(
        db: PgPool,
        qdrant: Arc<Qdrant>,
        inference: InferenceClient,
        backends: crate::services::execution::ExecutionBackends,
    ) -> Self {
        Self {
            db,
            qdrant,
            inference,
            backends,
        }
    }

    /// Dispatch a new task for execution. Spawns as a Tokio background task.
    pub fn dispatch(&self, task_id: Uuid) {
        let db = self.db.clone();
        let qdrant = self.qdrant.clone();
        let inference = self.inference.clone();
        let backends = self.backends.clone();

        tokio::spawn(async move {
            if let Err(e) = Self::execute(db, qdrant, inference, backends, task_id).await {
                tracing::error!("Task {} failed: {:?}", task_id, e);
            }
        });
    }

    async fn execute(
        db: PgPool,
        qdrant: Arc<Qdrant>,
        inference: InferenceClient,
        backends: crate::services::execution::ExecutionBackends,
        task_id: Uuid,
    ) -> Result<()> {
        // Load task
        let task = sqlx::query!(
            r#"SELECT t.id, t.job_id, t.workflow_id, t.input, t.target,
                      a.id as agent_id, a.persona_system_prompt, a.cf_finetune_id,
                      w.graph as "graph?"
               FROM tasks t
               JOIN jobs j ON j.id = t.job_id
               JOIN agents a ON a.id = j.agent_id
               LEFT JOIN workflows w ON w.id = t.workflow_id
               WHERE t.id = $1"#,
            task_id
        )
        .fetch_optional(&db)
        .await?
        .ok_or_else(|| anyhow!("Task {} not found", task_id))?;

        let graph_json = task
            .graph
            .ok_or_else(|| anyhow!("Task {} has no workflow attached", task_id))?;

        let graph: WorkflowGraph = serde_json::from_value(graph_json)?;

        let persona_prompt = task.persona_system_prompt.unwrap_or_else(|| {
            "You are a professional AI agent. Complete the task with precision.".to_string()
        });

        // cf_finetune_id is the Cloudflare Workers AI finetune ID for this professional's adapter
        let lora_name = task.cf_finetune_id;

        // Inject any job integration credentials into context vars
        let mut ctx = ExecutionContext::new(
            task_id,
            task.agent_id,
            task.job_id,
            lora_name,
            persona_prompt,
            task.input,
        );

        // Inject the task's structured work-target as ctx vars (e.g. target.repo -> ctx.vars["repo"],
        // target.base_branch -> ctx.vars["base_branch"]). The AgentImplement / PrCreate nodes read
        // ctx.vars["repo"], so this is what lets a generic workflow target a per-task repo.
        if let Some(target) = task.target.as_object() {
            for (k, v) in target {
                ctx.vars.insert(k.clone(), v.clone());
            }
        }

        // Load job integration credentials into ctx.vars so nodes can use them
        Self::inject_integration_credentials(&db, task.job_id, &mut ctx).await?;

        let engine = WorkflowEngine::new(&db, &qdrant, &inference).with_backends(backends);
        engine.run(&graph, &mut ctx).await?;

        Ok(())
    }

    /// Resolve credentials for all integrations on this job and inject as ctx vars.
    ///
    /// Two kinds of grant exist on `job_integrations`:
    ///   - Connection-backed (`connection_id` set): the agent acts as its owner. We
    ///     resolve a fresh access token from the owner's Connection and inject it as
    ///     `{provider}_token` (e.g. `github_token`). Google docs creation resolves its
    ///     own fresh token per-node, so it isn't pre-injected here.
    ///   - Raw-credential (`encrypted_credentials` set): the company provided its own
    ///     token (e.g. a Slack bot token). We decrypt and inject each field as
    ///     `{prefix}{key}`.
    pub async fn inject_integration_credentials(
        db: &PgPool,
        job_id: Uuid,
        ctx: &mut ExecutionContext,
    ) -> Result<()> {
        let integrations = sqlx::query!(
            "SELECT tool_type::text, encrypted_credentials, connection_id FROM job_integrations WHERE job_id = $1 AND is_active = true",
            job_id
        )
        .fetch_all(db)
        .await?;

        for row in integrations {
            let tool_type = row.tool_type.as_deref().unwrap_or("");

            // Connection-backed grant: inject a fresh token as `{provider}_token`.
            if let Some(connection_id) = row.connection_id {
                if let Ok(conn) = crate::services::connections::get(db, connection_id).await {
                    if let Ok(token) =
                        crate::services::connections::fresh_access_token(db, &conn).await
                    {
                        // github_token, etc. The github node already reads `github_token`.
                        ctx.vars.insert(
                            format!("{}_token", conn.provider.as_str()),
                            Value::String(token),
                        );
                    }
                }
                continue;
            }

            // Raw-credential grant: decrypt and inject each field under a prefix.
            let Some(encrypted) = row.encrypted_credentials.as_deref() else {
                continue;
            };
            let Ok(plaintext) = crate::services::crypto::decrypt(encrypted) else {
                tracing::warn!(
                    "Failed to decrypt credentials for {} integration on job {}",
                    tool_type,
                    job_id
                );
                continue;
            };
            if let Ok(creds) = serde_json::from_str::<Value>(&plaintext) {
                let prefix = match tool_type {
                    "github" => "github_",
                    "slack" => "slack_",
                    "jira" => "jira_",
                    "linear" => "linear_",
                    "google_drive" => "gdrive_",
                    _ => "integration_",
                };
                if let Some(obj) = creds.as_object() {
                    for (k, v) in obj {
                        ctx.vars.insert(format!("{}{}", prefix, k), v.clone());
                    }
                }
            }
        }

        Ok(())
    }
}

/// Resume a task that was paused at a HumanGate after professional approval.
pub async fn resume_after_approval(
    db: &PgPool,
    qdrant: &Arc<Qdrant>,
    inference: &InferenceClient,
    backends: &crate::services::execution::ExecutionBackends,
    task_id: Uuid,
    approved_output: &str,
    amended: bool,
) -> Result<()> {
    // Record the RLHF event if the professional amended the output
    if amended {
        let draft = sqlx::query_scalar!("SELECT output_draft FROM tasks WHERE id = $1", task_id)
            .fetch_optional(db)
            .await?
            .flatten()
            .unwrap_or_default();

        sqlx::query!(
            r#"INSERT INTO rlhf_events (id, task_id, original_output, amended_output, created_at)
               VALUES ($1, $2, $3, $4, NOW())"#,
            Uuid::new_v4(),
            task_id,
            draft,
            approved_output
        )
        .execute(db)
        .await?;
    }

    // Audit log the approval
    if let Ok(row) = sqlx::query!("SELECT job_id FROM tasks WHERE id = $1", task_id)
        .fetch_one(db)
        .await
    {
        let _ = crate::services::audit::log_task_approved(
            db, row.job_id, task_id, amended,
            task_id, // actor_id placeholder — replace with real user_id from auth context
        )
        .await;
    }

    // Store the approved output as a knowledge chunk for future RAG
    let agent_id_row = sqlx::query!(
        "SELECT a.id as agent_id FROM tasks t JOIN jobs j ON j.id = t.job_id JOIN agents a ON a.id = j.agent_id WHERE t.id = $1",
        task_id
    )
    .fetch_one(db)
    .await?;

    // Persist the approved output as a vault note (the markdown source of truth; the Qdrant
    // chunk + timeline event derive from it). Slug is stable per task so a re-approval
    // overwrites in place.
    let title: String = approved_output
        .lines()
        .next()
        .unwrap_or("Approved output")
        .chars()
        .take(80)
        .collect();
    let note = crate::services::vault::VaultNote {
        id: Uuid::new_v4(),
        agent_id: agent_id_row.agent_id,
        owner_kind: "agent".into(),
        slug: format!("approved-output-{}", task_id.simple()),
        title: if title.trim().is_empty() {
            "Approved output".into()
        } else {
            title
        },
        body: approved_output.to_string(),
        layer: crate::services::vault::NoteLayer::Note,
        source_type: crate::services::knowledge::KnowledgeSourceType::ApprovedOutput,
        visibility: crate::services::knowledge::ChunkVisibility::Private,
        domain: None,
        related: vec![],
        repo: None,
        job_id: None,
    };
    let _ = crate::services::vault::upsert_note(db, qdrant, inference, note).await;

    // Continue the workflow past the review gate. Load the task + its graph, rebuild the
    // execution context from persisted node outputs, inject the approved (possibly
    // amended) output under the var the gate read from, then resume downstream. Delivery
    // now happens naturally when traversal reaches the Output node — unless the gate was
    // the terminal node, in which case we deliver the approved output directly.
    let task = sqlx::query!(
        r#"SELECT t.job_id, t.input, t.current_node_id,
                  a.id as agent_id, a.persona_system_prompt, a.cf_finetune_id,
                  w.graph as "graph?"
           FROM tasks t
           JOIN jobs j ON j.id = t.job_id
           JOIN agents a ON a.id = j.agent_id
           LEFT JOIN workflows w ON w.id = t.workflow_id
           WHERE t.id = $1"#,
        task_id
    )
    .fetch_one(db)
    .await?;

    let persona_prompt = task.persona_system_prompt.unwrap_or_else(|| {
        "You are a professional AI agent. Complete the task with precision.".to_string()
    });

    let graph: Option<WorkflowGraph> = task.graph.and_then(|g| serde_json::from_value(g).ok());
    let gate_node_id = task.current_node_id.clone();

    if let (Some(graph), Some(gate_id)) = (graph.as_ref(), gate_node_id.as_ref()) {
        // The gate's downstream nodes read from whatever var the gate read; default
        // matches the engine's HumanGate (`input_var`, default "llm_output").
        let gate_input_var = graph
            .nodes
            .iter()
            .find(|n| &n.id == gate_id)
            .and_then(|n| n.config.get("input_var"))
            .and_then(|v| v.as_str())
            .unwrap_or("llm_output")
            .to_string();

        let mut ctx = WorkflowEngine::rebuild_context(
            db,
            task_id,
            task.agent_id,
            task.job_id,
            task.cf_finetune_id.clone(),
            persona_prompt,
            task.input.clone(),
        )
        .await?;
        // Inject the approved output so downstream nodes consume the human-approved value.
        ctx.vars.insert(
            gate_input_var,
            serde_json::Value::String(approved_output.to_string()),
        );
        TaskRunner::inject_integration_credentials(db, task.job_id, &mut ctx).await?;

        let engine = WorkflowEngine::new(db, qdrant, inference).with_backends(backends.clone());
        engine.resume_from(graph, &mut ctx, gate_id).await?;
    }

    // If resume didn't reach a terminal Output node (gate was last, or no graph), the
    // approved output is itself the deliverable — mark delivered.
    let status = sqlx::query_scalar!("SELECT status::text FROM tasks WHERE id = $1", task_id)
        .fetch_one(db)
        .await?;
    if status.as_deref() != Some("delivered") {
        sqlx::query!(
            r#"UPDATE tasks SET status = 'delivered', output_approved = $1,
               delivered_at = NOW() WHERE id = $2"#,
            approved_output,
            task_id
        )
        .execute(db)
        .await?;
    }

    // Grade the approved output against the agent's evaluation criteria (best-effort; no-op if the
    // agent has none). The output the human approved is what ships, so that's what we evaluate.
    let task_input = task
        .input
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| task.input.to_string());
    let _ = crate::services::evaluations::evaluate_task(
        db,
        inference,
        task.agent_id,
        task_id,
        &task_input,
        approved_output,
    )
    .await;

    Ok(())
}
