//! Per-node chat: the professional talks to their agent about the work a workflow node
//! produced. The agent (persona + LoRA + RAG, grounded in that node's input/output)
//! proposes corrections. Applying a correction updates the node's persisted output and
//! marks it + everything downstream `dirty`, so the owner can re-run from there when
//! ready. Conversations also feed the knowledge base as ChatCorrection chunks, so the
//! agent keeps improving from supervised corrections.

use anyhow::Result;
use qdrant_client::Qdrant;
use serde::Serialize;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::models::workflow::WorkflowGraph;
use crate::services::inference::InferenceClient;
use crate::services::knowledge::{self, ChunkVisibility, KnowledgeSourceType};
use crate::services::workflow_engine::WorkflowEngine;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct NodeMessage {
    pub id: Uuid,
    pub role: String,
    pub content: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Find or create the conversation for a (task, node).
pub async fn start_or_get_conversation(db: &PgPool, task_id: Uuid, node_id: &str) -> Result<Uuid> {
    let id = sqlx::query_scalar!(
        r#"INSERT INTO node_conversations (task_id, node_id) VALUES ($1, $2)
           ON CONFLICT (task_id, node_id) DO UPDATE SET task_id = EXCLUDED.task_id
           RETURNING id"#,
        task_id,
        node_id,
    )
    .fetch_one(db)
    .await?;
    Ok(id)
}

pub async fn messages(db: &PgPool, conversation_id: Uuid) -> Result<Vec<NodeMessage>> {
    let rows = sqlx::query_as::<_, NodeMessage>(
        "SELECT id, role, content, created_at FROM node_messages WHERE conversation_id = $1 ORDER BY created_at ASC",
    )
    .bind(conversation_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Send a message to the agent about a node's work and get its reply. Grounds the agent
/// in the node's persisted input/output plus RAG from the knowledge base.
pub async fn send_message(
    db: &PgPool,
    qdrant: &Arc<Qdrant>,
    inference: &InferenceClient,
    task_id: Uuid,
    node_id: &str,
    user_msg: &str,
) -> Result<NodeMessage> {
    let conversation_id = start_or_get_conversation(db, task_id, node_id).await?;

    // Persist the user's message first.
    sqlx::query!(
        "INSERT INTO node_messages (conversation_id, role, content) VALUES ($1, 'user', $2)",
        conversation_id,
        user_msg,
    )
    .execute(db)
    .await?;

    // Agent context: persona + this node's current input/output + RAG + prior turns.
    let meta = sqlx::query!(
        r#"SELECT a.id as agent_id, a.persona_system_prompt, a.cf_finetune_id
           FROM tasks t JOIN jobs j ON j.id = t.job_id JOIN agents a ON a.id = j.agent_id
           WHERE t.id = $1"#,
        task_id
    )
    .fetch_one(db)
    .await?;

    let node_exec = sqlx::query!(
        r#"SELECT input_json, output_json FROM node_executions
           WHERE task_id = $1 AND node_id = $2 ORDER BY attempt DESC LIMIT 1"#,
        task_id,
        node_id,
    )
    .fetch_optional(db)
    .await?;

    let persona = meta
        .persona_system_prompt
        .unwrap_or_else(|| "You are a professional AI agent.".to_string());

    // RAG grounded in the user's question.
    let rag = {
        let embeddings = inference
            .embed(vec![user_msg.to_string()])
            .await
            .unwrap_or_default();
        let query_vec = embeddings.into_iter().next().unwrap_or_default();
        let chunks = knowledge::search_chunks(qdrant, query_vec, "agent", meta.agent_id, 4, 0.65)
            .await
            .unwrap_or_default();
        chunks
            .into_iter()
            .map(|c| c.content)
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    };

    let node_io = node_exec
        .map(|e| {
            format!(
                "## The work in question (this workflow stage)\nInput:\n{}\n\nOutput:\n{}",
                e.input_json.map(|v| v.to_string()).unwrap_or_default(),
                e.output_json.map(|v| v.to_string()).unwrap_or_default(),
            )
        })
        .unwrap_or_default();

    let history = messages(db, conversation_id).await?;
    let transcript = history
        .iter()
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n");

    let system = format!(
        "{persona}\n\nYou are discussing work YOU produced at one stage of a workflow with \
         the professional you represent. They may ask you to revise or correct it. When you \
         provide a corrected version of the work, output the corrected work clearly so it can \
         replace the previous output.\n\n{node_io}\n\n## Relevant context\n{rag}",
    );
    let user = format!("Conversation so far:\n{transcript}\n\nRespond to the latest message.");

    let reply = inference
        .worker_likeness(&system, &user, meta.cf_finetune_id.as_deref())
        .await?;

    let msg = sqlx::query_as::<_, NodeMessage>(
        "INSERT INTO node_messages (conversation_id, role, content) VALUES ($1, 'assistant', $2) \
         RETURNING id, role, content, created_at",
    )
    .bind(conversation_id)
    .bind(&reply)
    .fetch_one(db)
    .await?;

    Ok(msg)
}

/// Apply a correction to a node: replace its persisted output with `corrected_output`,
/// mark it and every downstream node `dirty` (staged — not re-run yet), and ingest the
/// correction into the knowledge base so future training reflects it.
pub async fn apply_correction(
    db: &PgPool,
    qdrant: &Arc<Qdrant>,
    inference: &InferenceClient,
    task_id: Uuid,
    node_id: &str,
    corrected_output: &str,
) -> Result<()> {
    // Update the latest attempt's output for this node.
    sqlx::query!(
        r#"UPDATE node_executions SET output_json = $1, dirty = true
           WHERE id = (SELECT id FROM node_executions WHERE task_id = $2 AND node_id = $3
                       ORDER BY attempt DESC LIMIT 1)"#,
        serde_json::Value::String(corrected_output.to_string()),
        task_id,
        node_id,
    )
    .execute(db)
    .await?;

    // Mark all downstream nodes dirty so the owner knows they need re-running.
    let graph = load_graph(db, task_id).await?;
    if let Some(graph) = &graph {
        let downstream = downstream_nodes(graph, node_id);
        if !downstream.is_empty() {
            sqlx::query!(
                "UPDATE node_executions SET dirty = true WHERE task_id = $1 AND node_id = ANY($2)",
                task_id,
                &downstream,
            )
            .execute(db)
            .await?;
        }
    }

    // Ingest the correction as a ChatCorrection knowledge chunk for future retraining.
    let agent_id = sqlx::query_scalar!(
        "SELECT j.agent_id FROM tasks t JOIN jobs j ON j.id = t.job_id WHERE t.id = $1",
        task_id
    )
    .fetch_one(db)
    .await?;

    if let Some(agent_id) = agent_id {
        let content = format!(
            "Correction to workflow node '{}':\n{}",
            node_id, corrected_output
        );
        // Each correction is a distinct vault note (unique slug); upsert_note derives the
        // chunk + records the timeline event.
        let note = crate::services::vault::VaultNote {
            id: Uuid::new_v4(),
            agent_id,
            owner_kind: "agent".into(),
            slug: format!(
                "correction-{}-{}",
                task_id.simple(),
                Uuid::new_v4().simple()
            ),
            title: format!("Correction to node '{}'", node_id),
            body: content.clone(),
            layer: crate::services::vault::NoteLayer::StyleRule,
            source_type: KnowledgeSourceType::ChatCorrection,
            visibility: ChunkVisibility::Private,
            domain: None,
            related: vec![],
            repo: None,
            job_id: None,
        };
        let _ = crate::services::vault::upsert_note(db, qdrant, inference, note).await;
    }

    Ok(())
}

/// Re-run the workflow starting at `node_id`, using the corrected upstream outputs. The
/// node and everything downstream re-execute; `dirty` is cleared on the re-run nodes.
pub async fn rerun_from(
    db: &PgPool,
    qdrant: &Arc<Qdrant>,
    inference: &InferenceClient,
    backends: &crate::services::execution::ExecutionBackends,
    task_id: Uuid,
    node_id: &str,
) -> Result<()> {
    let meta = sqlx::query!(
        r#"SELECT t.input, t.job_id, a.id as agent_id, a.persona_system_prompt, a.cf_finetune_id
           FROM tasks t JOIN jobs j ON j.id = t.job_id JOIN agents a ON a.id = j.agent_id
           WHERE t.id = $1"#,
        task_id
    )
    .fetch_one(db)
    .await?;

    let Some(graph) = load_graph(db, task_id).await? else {
        return Ok(());
    };

    let persona = meta.persona_system_prompt.unwrap_or_else(|| {
        "You are a professional AI agent. Complete the task with precision.".to_string()
    });

    // Rebuild context from persisted (corrected) outputs, then re-run from this node.
    let mut ctx = WorkflowEngine::rebuild_context(
        db,
        task_id,
        meta.agent_id,
        meta.job_id,
        meta.cf_finetune_id.clone(),
        persona,
        meta.input.clone(),
    )
    .await?;
    crate::services::task_runner::TaskRunner::inject_integration_credentials(
        db,
        meta.job_id,
        &mut ctx,
    )
    .await?;

    let engine = WorkflowEngine::new(db, qdrant, inference).with_backends(backends.clone());
    engine
        .rerun_node_and_downstream(&graph, &mut ctx, node_id)
        .await?;

    // Clear dirty flags on the re-run nodes (the node + its downstream).
    let mut rerun_set = downstream_nodes(&graph, node_id);
    rerun_set.push(node_id.to_string());
    sqlx::query!(
        "UPDATE node_executions SET dirty = false WHERE task_id = $1 AND node_id = ANY($2)",
        task_id,
        &rerun_set,
    )
    .execute(db)
    .await?;

    Ok(())
}

/// Resolve a side-effect gate: record the owner's decision (reuse|update|refire) for the
/// node and resume the workflow from that node. The engine consults the decision so the
/// node doesn't pause again.
pub async fn resolve_effect(
    db: &PgPool,
    qdrant: &Arc<Qdrant>,
    inference: &InferenceClient,
    backends: &crate::services::execution::ExecutionBackends,
    task_id: Uuid,
    node_id: &str,
    decision: &str,
) -> Result<()> {
    // Record the decision on the gate row for auditability.
    sqlx::query!(
        r#"UPDATE node_executions SET effect_decision = $1
           WHERE id = (SELECT id FROM node_executions
                       WHERE task_id = $2 AND node_id = $3 AND status = 'awaiting_effect'
                       ORDER BY attempt DESC LIMIT 1)"#,
        decision,
        task_id,
        node_id,
    )
    .execute(db)
    .await?;

    let meta = sqlx::query!(
        r#"SELECT t.input, t.job_id, a.id as agent_id, a.persona_system_prompt, a.cf_finetune_id
           FROM tasks t JOIN jobs j ON j.id = t.job_id JOIN agents a ON a.id = j.agent_id
           WHERE t.id = $1"#,
        task_id
    )
    .fetch_one(db)
    .await?;

    let Some(graph) = load_graph(db, task_id).await? else {
        return Ok(());
    };
    let persona = meta.persona_system_prompt.unwrap_or_else(|| {
        "You are a professional AI agent. Complete the task with precision.".to_string()
    });

    let mut ctx = WorkflowEngine::rebuild_context(
        db,
        task_id,
        meta.agent_id,
        meta.job_id,
        meta.cf_finetune_id.clone(),
        persona,
        meta.input.clone(),
    )
    .await?;
    crate::services::task_runner::TaskRunner::inject_integration_credentials(
        db,
        meta.job_id,
        &mut ctx,
    )
    .await?;
    // Tell the engine how to handle this node's side effect on resume.
    ctx.effect_decisions
        .insert(node_id.to_string(), decision.to_string());

    let engine = WorkflowEngine::new(db, qdrant, inference).with_backends(backends.clone());
    engine
        .rerun_node_and_downstream(&graph, &mut ctx, node_id)
        .await?;
    Ok(())
}

/// Load a task's workflow graph.
async fn load_graph(db: &PgPool, task_id: Uuid) -> Result<Option<WorkflowGraph>> {
    let row = sqlx::query!(
        r#"SELECT w.graph as "graph?" FROM tasks t
           LEFT JOIN workflows w ON w.id = t.workflow_id WHERE t.id = $1"#,
        task_id
    )
    .fetch_one(db)
    .await?;
    Ok(row.graph.and_then(|g| serde_json::from_value(g).ok()))
}

/// All nodes reachable downstream of `node_id` (BFS over edges).
fn downstream_nodes(graph: &WorkflowGraph, node_id: &str) -> Vec<String> {
    use std::collections::{HashSet, VecDeque};
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(node_id.to_string());
    while let Some(n) = queue.pop_front() {
        for e in graph.edges.iter().filter(|e| e.source == n) {
            if seen.insert(e.target.clone()) {
                queue.push_back(e.target.clone());
            }
        }
    }
    seen.into_iter().collect()
}
