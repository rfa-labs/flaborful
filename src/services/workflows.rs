use crate::errors::{AppError, AppResult};
use crate::models::workflow::{CreateWorkflowRequest, UpdateWorkflowRequest, Workflow};
use crate::services::node_registry;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

/// Validate every node's config against its manifest before persisting a graph. A node with any
/// incoming edge has its inputs satisfied by the upstream wiring (flaborful wires node→node, so an
/// incoming edge stands in for the node's dynamic/required inputs). Returns a `BadRequest` naming
/// the offending nodes if any config is invalid, so the builder can surface a precise error.
fn validate_graph(graph: &serde_json::Value) -> AppResult<()> {
    let Some(nodes) = graph.get("nodes").and_then(|v| v.as_array()) else {
        return Ok(()); // empty/absent graph — nothing to validate
    };
    // Node ids that have at least one incoming edge.
    let wired_targets: std::collections::HashSet<&str> = graph
        .get("edges")
        .and_then(|v| v.as_array())
        .map(|edges| {
            edges
                .iter()
                .filter_map(|e| e.get("target").and_then(|t| t.as_str()))
                .collect()
        })
        .unwrap_or_default();

    let mut problems: Vec<String> = Vec::new();
    for node in nodes {
        let node_type = node.get("node_type").and_then(|v| v.as_str()).unwrap_or("");
        // Alias nodes (canvas_kind) validate against their canvas manifest.
        let config = node.get("config").cloned().unwrap_or(serde_json::json!({}));
        let kind = config
            .get("canvas_kind")
            .and_then(|v| v.as_str())
            .unwrap_or(node_type);
        let id = node.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        // Unknown types (e.g. legacy nodes not yet in the registry) are skipped, not rejected.
        if node_registry::manifest_for(kind).is_none() {
            continue;
        }
        // Any incoming edge satisfies the node's required inputs (node-level wiring).
        let wired: Vec<String> = if wired_targets.contains(id) {
            node_registry::manifest_for(kind)
                .map(|m| m.inputs.iter().map(|f| f.key.to_string()).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for problem in node_registry::validate_config(kind, &config, &wired) {
            problems.push(format!("{id} ({kind}): {problem}"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "Invalid workflow node config: {}",
            problems.join("; ")
        )))
    }
}

pub async fn create_workflow(db: &PgPool, req: CreateWorkflowRequest) -> AppResult<Workflow> {
    validate_graph(&req.graph)?;
    let row = sqlx::query_as::<_, Workflow>(
        r#"INSERT INTO workflows (id, agent_id, aspect_id, name, description, graph, is_universal, job_id, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(req.agent_id)
    .bind(req.aspect_id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.graph)
    .bind(req.is_universal)
    .bind(req.job_id)
    .bind(Utc::now())
    .fetch_one(db)
    .await?;
    Ok(row)
}

pub async fn update_workflow(
    db: &PgPool,
    id: Uuid,
    req: UpdateWorkflowRequest,
) -> AppResult<Workflow> {
    let current = get_workflow(db, id).await?;
    if let Some(graph) = &req.graph {
        validate_graph(graph)?;
    }
    // Resolve the environment: absent => unchanged; present => set/clear (validated).
    let execution_environment = match req.execution_environment {
        None => current.execution_environment,
        Some(env) => {
            if let Some(v) = &env {
                if crate::services::execution::ExecutionEnvironment::from_str_opt(v).is_none() {
                    return Err(AppError::BadRequest(format!(
                        "execution_environment must be managed_cloud, claude_code_byok, or optimizer_fbx (got {v:?})"
                    )));
                }
            }
            env
        }
    };
    let row = sqlx::query_as::<_, Workflow>(
        r#"UPDATE workflows
           SET name = $2, description = $3, graph = $4, is_universal = $5,
               execution_environment = $6, updated_at = $7
           WHERE id = $1
           RETURNING *"#,
    )
    .bind(id)
    .bind(req.name.unwrap_or(current.name))
    .bind(req.description.or(current.description))
    .bind(req.graph.unwrap_or(current.graph))
    .bind(req.is_universal.unwrap_or(current.is_universal))
    .bind(execution_environment)
    .bind(Utc::now())
    .fetch_one(db)
    .await?;
    Ok(row)
}

pub async fn get_workflow(db: &PgPool, id: Uuid) -> AppResult<Workflow> {
    sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Workflow {id} not found")))
}

pub async fn delete_workflow(db: &PgPool, id: Uuid) -> AppResult<()> {
    let result = sqlx::query("DELETE FROM workflows WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Workflow {id} not found")));
    }
    Ok(())
}

pub async fn list_for_agent(db: &PgPool, agent_id: Uuid) -> AppResult<Vec<Workflow>> {
    let rows = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE agent_id = $1 ORDER BY created_at DESC",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

// ── Library (reusable workflows/subflows) ────────────────────────────────────

/// The agent's reusable library: universal (template) workflows, newest first. These are the
/// building blocks — subflows and starter templates — cloned into fresh editable workflows.
pub async fn list_library(db: &PgPool, agent_id: Uuid) -> AppResult<Vec<Workflow>> {
    Ok(sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE agent_id = $1 AND is_universal = true ORDER BY created_at DESC",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?)
}

/// Clone a workflow into a fresh editable copy (a new universal workflow) for the same agent.
pub async fn clone_workflow(db: &PgPool, src: &Workflow) -> AppResult<Workflow> {
    let row = sqlx::query_as::<_, Workflow>(
        r#"INSERT INTO workflows (id, agent_id, aspect_id, name, description, graph, is_universal, job_id, execution_environment, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, true, NULL, $7, $8, $8)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(src.agent_id)
    .bind(src.aspect_id)
    .bind(format!("{} (copy)", src.name))
    .bind(&src.description)
    .bind(&src.graph)
    .bind(&src.execution_environment)
    .bind(Utc::now())
    .fetch_one(db)
    .await?;
    Ok(row)
}

// ── Checkpoints (graph version history) ──────────────────────────────────────

/// A saved graph snapshot (metadata + the graph itself).
#[derive(serde::Serialize, sqlx::FromRow)]
pub struct WorkflowCheckpoint {
    pub id: Uuid,
    pub number: i32,
    pub label: Option<String>,
    pub graph: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Snapshot the workflow's CURRENT live graph as a new checkpoint. Returns the checkpoint number.
pub async fn create_checkpoint(
    db: &PgPool,
    workflow_id: Uuid,
    label: Option<String>,
) -> AppResult<i32> {
    let wf = get_workflow(db, workflow_id).await?;
    let number: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(number), 0) + 1 FROM workflow_checkpoints WHERE workflow_id = $1",
    )
    .bind(workflow_id)
    .fetch_one(db)
    .await?;
    sqlx::query(
        "INSERT INTO workflow_checkpoints (workflow_id, number, graph, label) VALUES ($1, $2, $3, $4)",
    )
    .bind(workflow_id)
    .bind(number)
    .bind(&wf.graph)
    .bind(&label)
    .execute(db)
    .await?;
    Ok(number)
}

pub async fn list_checkpoints(
    db: &PgPool,
    workflow_id: Uuid,
) -> AppResult<Vec<WorkflowCheckpoint>> {
    Ok(sqlx::query_as::<_, WorkflowCheckpoint>(
        "SELECT id, number, label, graph, created_at FROM workflow_checkpoints WHERE workflow_id = $1 ORDER BY number DESC",
    )
    .bind(workflow_id)
    .fetch_all(db)
    .await?)
}

/// Restore a checkpoint into the live graph. Before overwriting, the current live graph is itself
/// snapshotted as a new checkpoint, so restoring never loses the working draft (Gumloop's behavior).
/// Returns the restored graph.
pub async fn restore_checkpoint(
    db: &PgPool,
    workflow_id: Uuid,
    checkpoint_id: Uuid,
) -> AppResult<serde_json::Value> {
    let graph: serde_json::Value = sqlx::query_scalar(
        "SELECT graph FROM workflow_checkpoints WHERE id = $1 AND workflow_id = $2",
    )
    .bind(checkpoint_id)
    .bind(workflow_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("Checkpoint not found".into()))?;

    // Preserve the current live draft as a checkpoint before overwriting it.
    let _ = create_checkpoint(db, workflow_id, Some("auto: before restore".into())).await;

    sqlx::query("UPDATE workflows SET graph = $1, updated_at = NOW() WHERE id = $2")
        .bind(&graph)
        .bind(workflow_id)
        .execute(db)
        .await?;
    Ok(graph)
}
