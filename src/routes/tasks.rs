use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth_extractor::AuthUser,
    errors::{AppError, AppResult},
    models::node_execution::NodeExecution,
    models::task::Task,
    services::{authz, task_runner, tasks as svc},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create_task))
        .route("/:id", get(get_task))
        .route("/:id/approve", post(approve_task))
        .route("/:id/reject", post(reject_task))
        .route("/:id/kill", post(kill_task))
        .route("/:id/run", post(run_task))
        .route("/:id/node-executions", get(node_executions))
        .route("/job/:job_id", get(list_for_job))
        .route("/job/:job_id/review-queue", get(review_queue))
        .route("/agent/:agent_id/active", get(list_active_for_agent))
        .route("/agent/:agent_id/runs", get(list_runs_for_agent))
        .route("/:id/rerun/:node_id", post(rerun_from))
        .route("/:id/node/:node_id/effect-decision", post(effect_decision))
        .route("/:id/workflow", patch(assign_workflow))
}

/// Resolve the agent that owns a task and assert the caller owns that agent.
async fn require_task_owner(state: &AppState, user_id: Uuid, task_id: Uuid) -> AppResult<Uuid> {
    let agent_id = svc::task_agent_id(&state.db, task_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Task {task_id} not found")))?;
    authz::require_agent_owner(&state.db, user_id, agent_id).await?;
    Ok(agent_id)
}

#[derive(Deserialize)]
struct CreateTaskRequest {
    job_id: Uuid,
    workflow_id: Option<Uuid>,
    input: serde_json::Value,
    /// Structured work-target, e.g. `{ "repo": "owner/name", "base_branch": "main" }`.
    /// Injected into the execution context at dispatch. Optional for non-SWE tasks.
    #[serde(default)]
    target: serde_json::Value,
}

async fn create_task(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<CreateTaskRequest>,
) -> AppResult<(StatusCode, Json<Task>)> {
    // Only the company that owns the job may spawn tasks on it (each task triggers real
    // inference/tool calls — this must not be open to any authenticated user).
    authz::require_job_owner(&state.db, auth.user_id, req.job_id).await?;

    let target = if req.target.is_null() {
        serde_json::json!({})
    } else {
        req.target
    };
    let task = sqlx::query_as::<_, Task>(
        r#"INSERT INTO tasks (id, job_id, workflow_id, input, target, status, created_at)
           VALUES ($1, $2, $3, $4, $5, 'pending', NOW())
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(req.job_id)
    .bind(req.workflow_id)
    .bind(&req.input)
    .bind(&target)
    .fetch_one(&state.db)
    .await?;

    // Spawn execution in the background
    state.runner.dispatch(task.id);

    Ok((StatusCode::CREATED, Json(task)))
}

async fn get_task(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Task>> {
    // Either the company that owns the job or the professional who owns the agent may read a task.
    require_task_viewer(&state, auth.user_id, id).await?;
    let task = svc::get_task(&state.db, id).await?;
    Ok(Json(task))
}

#[derive(Deserialize)]
struct ApproveTaskRequest {
    approved_output: String,
    amended: bool,
}

async fn approve_task(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(task_id): Path<Uuid>,
    Json(req): Json<ApproveTaskRequest>,
) -> AppResult<Json<Task>> {
    // Only the professional whose agent owns this task may approve its output.
    require_task_owner(&state, auth.user_id, task_id).await?;
    task_runner::resume_after_approval(
        &state.db,
        &state.qdrant,
        &state.inference,
        &state.backends,
        task_id,
        &req.approved_output,
        req.amended,
    )
    .await
    .map_err(AppError::Internal)?;

    let task = svc::get_task(&state.db, task_id).await?;
    Ok(Json(task))
}

/// POST /tasks/:id/reject — the professional rejects the work at a human gate. We move the task
/// back to in-progress (running) so they can drive the fix via per-node Chat → Apply correction →
/// Rerun. The sprite stays alive (the rerun needs it). No-op unless the task is awaiting_review.
async fn reject_task(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<Task>> {
    require_task_owner(&state, auth.user_id, task_id).await?;
    sqlx::query!(
        "UPDATE tasks SET status = 'running' WHERE id = $1 AND status = 'awaiting_review'",
        task_id
    )
    .execute(&state.db)
    .await?;
    let task = svc::get_task(&state.db, task_id).await?;
    Ok(Json(task))
}

/// POST /tasks/:id/kill — the professional cancels the current EXECUTION (not the task). A task is
/// the company's durable request; a run is the professional's disposable attempt. Killing tears
/// down the sprite and resets the run's artifacts, returning the task to a re-runnable state with
/// its workflow kept — so the professional can refine and run again WITHOUT the company having to
/// recreate the task. Prior node_executions are preserved (a re-run appends a new `attempt`).
async fn kill_task(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<Task>> {
    require_task_owner(&state, auth.user_id, task_id).await?;
    // Best-effort sprite teardown first (no hard failure if it's already gone).
    crate::services::execution::sprite::destroy_task_sprite(
        &state.db,
        &state.backends.sprite_client,
        task_id,
    )
    .await;
    // Reset the run: clear this attempt's draft/gate/score and return to a re-runnable state.
    // Keep workflow_id so the task can be re-run directly. Don't touch delivered tasks.
    sqlx::query!(
        "UPDATE tasks SET status = 'pending', current_node_id = NULL, \
                output_draft = NULL, confidence_score = NULL \
         WHERE id = $1 AND status <> 'delivered'",
        task_id
    )
    .execute(&state.db)
    .await?;
    let task = svc::get_task(&state.db, task_id).await?;
    Ok(Json(task))
}

/// POST /tasks/:id/run — (re-)dispatch a task that already has a workflow assigned. This is the
/// "Run again" action after a kill/refine, distinct from the initial assign-and-run. No-op unless
/// the task has a workflow and isn't already running or delivered.
async fn run_task(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<Task>> {
    require_task_owner(&state, auth.user_id, task_id).await?;
    let task = svc::get_task(&state.db, task_id).await?;
    if task.workflow_id.is_none() {
        return Err(AppError::BadRequest(
            "task has no workflow assigned — assign one first".into(),
        ));
    }
    if matches!(task.status, crate::models::task::TaskStatus::Running) {
        return Err(AppError::BadRequest("task is already running".into()));
    }
    if matches!(task.status, crate::models::task::TaskStatus::Delivered) {
        return Err(AppError::BadRequest("task is already delivered".into()));
    }
    sqlx::query!("UPDATE tasks SET status = 'running' WHERE id = $1", task_id)
        .execute(&state.db)
        .await?;
    state.runner.dispatch(task_id);
    let task = svc::get_task(&state.db, task_id).await?;
    Ok(Json(task))
}

async fn list_for_job(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(job_id): Path<Uuid>,
) -> AppResult<Json<Vec<Task>>> {
    // The company that owns the job sees all tasks on it. The professional whose agent is
    // deployed on the job also needs visibility into task history (their agent is doing the
    // work). We allow either principal.
    authorize_job_viewer(&state.db, auth.user_id, job_id).await?;
    let tasks = svc::list_for_job(&state.db, job_id).await?;
    Ok(Json(tasks))
}

async fn review_queue(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(job_id): Path<Uuid>,
) -> AppResult<Json<Vec<Task>>> {
    authorize_job_viewer(&state.db, auth.user_id, job_id).await?;
    let tasks = svc::review_queue(&state.db, job_id).await?;
    Ok(Json(tasks))
}

/// GET /tasks/agent/:agent_id/active — active executions for the owner's agent.
async fn list_active_for_agent(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<Vec<Task>>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    let tasks = svc::list_active_for_agent(&state.db, agent_id).await?;
    Ok(Json(tasks))
}

/// The run-log: every execution for this agent (delivered/failed/in-flight), newest first.
async fn list_runs_for_agent(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(agent_id): Path<Uuid>,
) -> AppResult<Json<Vec<svc::RunSummary>>> {
    authz::require_agent_owner(&state.db, auth.user_id, agent_id).await?;
    Ok(Json(svc::list_runs_for_agent(&state.db, agent_id, 100).await?))
}

/// GET /tasks/:id/node-executions — per-node execution records for the DAG inspector.
async fn node_executions(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(task_id): Path<Uuid>,
) -> AppResult<Json<Vec<NodeExecution>>> {
    require_task_owner(&state, auth.user_id, task_id).await?;
    let rows = svc::node_executions_for_task(&state.db, task_id).await?;
    Ok(Json(rows))
}

/// POST /tasks/:id/rerun/:node_id — re-run a node and everything downstream with the
/// currently-staged (corrected) upstream outputs. The owner controls when this happens.
async fn rerun_from(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((task_id, node_id)): Path<(Uuid, String)>,
) -> AppResult<Json<serde_json::Value>> {
    require_task_owner(&state, auth.user_id, task_id).await?;
    // Run in the background — node execution can be slow (LLM/tool calls).
    let (db, qdrant, inference, backends) = (
        state.db.clone(),
        state.qdrant.clone(),
        state.inference.clone(),
        state.backends.clone(),
    );
    tokio::spawn(async move {
        if let Err(e) = crate::services::node_chat::rerun_from(
            &db, &qdrant, &inference, &backends, task_id, &node_id,
        )
        .await
        {
            tracing::error!(
                "rerun_from failed for task {} node {}: {:?}",
                task_id,
                node_id,
                e
            );
        }
    });
    Ok(Json(serde_json::json!({ "status": "rerun_started" })))
}

#[derive(Deserialize)]
struct EffectDecisionBody {
    /// reuse | update | refire
    decision: String,
}

/// POST /tasks/:id/node/:node_id/effect-decision — resolve a side-effect gate. The owner
/// chooses whether to reuse the prior result, edit it in place, or post a fresh write,
/// and the workflow resumes from that node.
async fn effect_decision(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((task_id, node_id)): Path<(Uuid, String)>,
    Json(body): Json<EffectDecisionBody>,
) -> AppResult<Json<serde_json::Value>> {
    require_task_owner(&state, auth.user_id, task_id).await?;
    if !matches!(body.decision.as_str(), "reuse" | "update" | "refire") {
        return Err(AppError::BadRequest(
            "decision must be reuse, update, or refire".into(),
        ));
    }
    let (db, qdrant, inference, backends) = (
        state.db.clone(),
        state.qdrant.clone(),
        state.inference.clone(),
        state.backends.clone(),
    );
    let decision = body.decision.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::services::node_chat::resolve_effect(
            &db, &qdrant, &inference, &backends, task_id, &node_id, &decision,
        )
        .await
        {
            tracing::error!(
                "effect_decision resume failed for task {}: {:?}",
                task_id,
                e
            );
        }
    });
    Ok(Json(
        serde_json::json!({ "status": "resolving", "decision": body.decision }),
    ))
}

#[derive(Deserialize)]
struct AssignWorkflowRequest {
    workflow_id: Uuid,
}

async fn assign_workflow(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(task_id): Path<Uuid>,
    Json(req): Json<AssignWorkflowRequest>,
) -> AppResult<Json<Task>> {
    // The professional whose agent is deployed on the job owns workflow selection — the company
    // posts the task (and its work-target) but does not pick how the agent does the work. This
    // assignment is what triggers execution for a pending task.
    require_task_owner(&state, auth.user_id, task_id).await?;

    // The workflow must belong to this task's agent — a professional can't run another agent's
    // workflow on the task.
    let agent_id = svc::task_agent_id(&state.db, task_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Task {task_id} not found")))?;
    let belongs = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM workflows WHERE id = $1 AND agent_id = $2)",
        req.workflow_id,
        agent_id,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(false);
    if !belongs {
        return Err(AppError::BadRequest(
            "workflow does not belong to this task's agent".into(),
        ));
    }

    sqlx::query!(
        "UPDATE tasks SET workflow_id = $1 WHERE id = $2",
        req.workflow_id,
        task_id
    )
    .execute(&state.db)
    .await?;
    state.runner.dispatch(task_id);
    let task = svc::get_task(&state.db, task_id).await?;
    Ok(Json(task))
}

// ── Authorization helpers ──────────────────────────────────────────────────────

/// Resolve the job_id for a task, or NotFound if the task doesn't exist.
async fn task_job_id(db: &sqlx::PgPool, task_id: Uuid) -> AppResult<Uuid> {
    sqlx::query_scalar!("SELECT job_id FROM tasks WHERE id = $1", task_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Task {task_id} not found")))
}

/// A reader of a task: either the company that owns the job, or the professional whose agent
/// is deployed on the job. Both have legitimate read access to task state and node executions.
/// Errors with `Forbidden` if the caller is neither, `NotFound` if the task doesn't exist
/// (so we don't leak existence to unauthorized callers).
async fn require_task_viewer(state: &AppState, user_id: Uuid, task_id: Uuid) -> AppResult<()> {
    let job_id = task_job_id(&state.db, task_id).await?;
    authorize_job_viewer(&state.db, user_id, job_id).await
}

/// A reader of a job's tasks: either the company that owns the job, or the professional whose
/// agent is deployed on it. Returns Forbidden if the caller is neither.
async fn authorize_job_viewer(db: &sqlx::PgPool, user_id: Uuid, job_id: Uuid) -> AppResult<()> {
    // Company path: the caller's company owns the job.
    if authz::require_job_owner(db, user_id, job_id).await.is_ok() {
        return Ok(());
    }
    // Professional path: the caller owns the agent deployed on the job.
    let agent_owner = sqlx::query_scalar!(
        r#"SELECT p.user_id
           FROM jobs j
           JOIN agents a ON a.id = j.agent_id
           JOIN professionals p ON p.id = a.professional_id
           WHERE j.id = $1"#,
        job_id
    )
    .fetch_optional(db)
    .await?;
    match agent_owner {
        Some(owner) if owner == user_id => Ok(()),
        _ => Err(AppError::Forbidden(
            "Not authorized to view this job's tasks".into(),
        )),
    }
}
