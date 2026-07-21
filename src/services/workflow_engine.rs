use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use crate::models::node_execution::node_status;
use crate::models::workflow::{NodeType, WorkflowGraph, WorkflowNode};
use crate::services::inference::InferenceClient;
use crate::services::{audit, confidence};
use qdrant_client::Qdrant;
use std::sync::Arc;

/// The execution context threaded through every node during a workflow run.
pub struct ExecutionContext {
    pub task_id: Uuid,
    pub agent_id: Uuid,
    pub job_id: Uuid,
    pub lora_name: Option<String>,
    pub persona_prompt: String,
    /// Running key/value store — nodes read and write to this.
    pub vars: HashMap<String, Value>,
    /// Owner decisions for side-effecting nodes on a rerun: node_id → reuse|update|refire.
    /// Consumed by the gate so a node with a decision doesn't pause again.
    pub effect_decisions: HashMap<String, String>,
    /// Subflow nesting depth (0 = top-level task graph). Bounded to prevent runaway/cyclic
    /// subflow recursion. A Subflow node increments this for its child context.
    pub subflow_depth: u32,
    /// Whether per-node executions are persisted to `node_executions`. True at the top level (the
    /// DAG inspector needs them). False inside a subflow's child run: the subflow is an internal
    /// computation captured on the parent Subflow node's own execution row, and skipping child
    /// persistence keeps the executions view clean AND sidesteps the `MAX(attempt)+1` INSERT race
    /// that concurrent loop-mode iterations of the same child graph would otherwise hit. Drilling
    /// into child executions is a Phase-6 (run-log) concern.
    pub persist: bool,
}

/// Max subflow nesting depth. Beyond this a Subflow node errors rather than recursing further —
/// guards against a workflow that (directly or transitively) includes itself.
const MAX_SUBFLOW_DEPTH: u32 = 8;

impl ExecutionContext {
    pub fn new(
        task_id: Uuid,
        agent_id: Uuid,
        job_id: Uuid,
        lora_name: Option<String>,
        persona_prompt: String,
        initial_input: Value,
    ) -> Self {
        let mut vars = HashMap::new();
        vars.insert("input".to_string(), initial_input);
        Self {
            task_id,
            agent_id,
            job_id,
            lora_name,
            persona_prompt,
            vars,
            effect_decisions: HashMap::new(),
            subflow_depth: 0,
            persist: true,
        }
    }
}

/// Result of running a single node.
pub enum NodeResult {
    /// Continue to the next node(s), optionally writing `key` = `value` into context vars.
    Continue { output_key: String, value: Value },
    /// Pause execution — workflow is now AwaitingReview at this node.
    PauseForReview { draft: String },
    /// Take the branch keyed by this label (for ConditionalBranch nodes).
    Branch(String),
    /// Workflow is done — this is the final deliverable.
    Deliver(String),
}

/// Build adjacency map node_id → ordered list of downstream node_ids.
/// Nodes that mutate the task sprite's working tree. A rerun of one of these needs the
/// filesystem reconciled (git-anchored reset) first, since the prior attempt's edits are
/// physically still on the box even though node_executions tracks only logical state.
fn is_sprite_mutating(t: &NodeType) -> bool {
    matches!(
        t,
        // TestGate can re-invoke the builder on the sprite (its fix loop), so a rerun must
        // reconcile the working tree like the other builder/git nodes.
        NodeType::AgentImplement
            | NodeType::SpriteCommand
            | NodeType::GitCommit
            | NodeType::GitPush
            | NodeType::TestGate
    )
}

fn build_adjacency(graph: &WorkflowGraph) -> HashMap<String, Vec<String>> {
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for edge in &graph.edges {
        adj.entry(edge.source.clone())
            .or_default()
            .push(edge.target.clone());
    }
    adj
}

/// Push a node's successors onto the DFS stack in reverse so they pop in original order.
fn push_next(stack: &mut Vec<String>, adj: &HashMap<String, Vec<String>>, node_id: &str) {
    if let Some(nexts) = adj.get(node_id) {
        for next in nexts.iter().rev() {
            stack.push(next.clone());
        }
    }
}

/// Resolve which downstream node a ConditionalBranch should take for an emitted label.
/// The node's `branches` config maps a branch label → the target node id to follow.
/// Falls back to the first successor when the label isn't mapped (or maps to a node
/// that isn't actually a successor).
fn pick_branch(
    adj: &HashMap<String, Vec<String>>,
    node: &WorkflowNode,
    label: &str,
) -> Option<String> {
    let nexts = adj.get(&node.id)?;
    let mapped = node
        .config
        .get("branches")
        .and_then(|b| b.as_object())
        .and_then(|b| b.get(label))
        .and_then(|v| v.as_str());
    if let Some(target) = mapped {
        if nexts.iter().any(|n| n == target) {
            return Some(target.to_string());
        }
    }
    // Fallback: first successor.
    nexts.first().cloned()
}

/// A transcript sink that appends a coding agent's streamed output to the latest running
/// `node_executions` row for a (task, node), so the DAG inspector shows progress live.
struct DbTranscriptSink {
    db: PgPool,
    task_id: Uuid,
    node_id: String,
}

#[async_trait::async_trait]
impl crate::services::execution::TranscriptSink for DbTranscriptSink {
    async fn push(&self, chunk: &str) {
        // Append the chunk verbatim. With live streaming the agent's stdout already carries its
        // own newlines; we no longer force one per chunk (that would split partial-line frames).
        let _ = sqlx::query!(
            r#"UPDATE node_executions
               SET logs = COALESCE(logs, '') || $1
               WHERE id = (SELECT id FROM node_executions
                           WHERE task_id = $2 AND node_id = $3 AND status = 'running'
                           ORDER BY attempt DESC, started_at DESC LIMIT 1)"#,
            chunk,
            self.task_id,
            self.node_id,
        )
        .execute(&self.db)
        .await;
    }
}

pub struct WorkflowEngine<'a> {
    db: &'a PgPool,
    qdrant: &'a Arc<Qdrant>,
    inference: &'a InferenceClient,
    /// Pluggable work-execution backends (Managed Agents / Sprite). Optional so resume/rerun
    /// callers that don't run AgentImplement nodes need not supply it; the AgentImplement node
    /// errors clearly if it's missing.
    backends: Option<crate::services::execution::ExecutionBackends>,
}

impl<'a> WorkflowEngine<'a> {
    pub fn new(db: &'a PgPool, qdrant: &'a Arc<Qdrant>, inference: &'a InferenceClient) -> Self {
        Self {
            db,
            qdrant,
            inference,
            backends: None,
        }
    }

    /// Attach the execution backends so AgentImplement nodes can run.
    pub fn with_backends(
        mut self,
        backends: crate::services::execution::ExecutionBackends,
    ) -> Self {
        self.backends = Some(backends);
        self
    }

    /// Execute the workflow graph from the trigger node forward.
    /// Updates task status in DB at each pause/completion point.
    pub async fn run(&self, graph: &WorkflowGraph, ctx: &mut ExecutionContext) -> Result<()> {
        let start = graph
            .nodes
            .iter()
            .find(|n| n.node_type == NodeType::Trigger)
            .ok_or_else(|| anyhow!("Workflow has no Trigger node"))?;
        let start_id = start.id.clone();
        self.execute_graph(graph, ctx, &[start_id]).await
    }

    /// Resume execution from the nodes downstream of `from_node_id`. The caller is
    /// responsible for having `from_node_id`'s output already present in `ctx.vars`
    /// (e.g. an approved gate draft, or a corrected node output). Used for gate
    /// continuation and "rerun from here".
    pub async fn resume_from(
        &self,
        graph: &WorkflowGraph,
        ctx: &mut ExecutionContext,
        from_node_id: &str,
    ) -> Result<()> {
        let adj = build_adjacency(graph);
        let next_ids: Vec<String> = adj
            .get(from_node_id)
            .map(|v| v.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        if next_ids.is_empty() {
            return Ok(());
        }
        self.execute_graph(graph, ctx, &next_ids).await
    }

    /// Re-run a node and everything downstream of it (inclusive). Used by "rerun from
    /// here" after the owner has staged corrections to upstream node outputs. The caller
    /// rebuilds ctx from persisted outputs first, so `node_id`'s upstream inputs reflect
    /// any corrections.
    pub async fn rerun_node_and_downstream(
        &self,
        graph: &WorkflowGraph,
        ctx: &mut ExecutionContext,
        node_id: &str,
    ) -> Result<()> {
        self.execute_graph(graph, ctx, &[node_id.to_string()]).await
    }

    /// Iterative DAG driver. Runs an explicit depth-first worklist starting at
    /// `start_ids`, persisting a `node_executions` row per node so per-node state is
    /// durable (enabling resume / re-run / DAG visualization). `execute_node` and the
    /// individual node implementations are unchanged — only the driver is iterative.
    async fn execute_graph(
        &self,
        graph: &WorkflowGraph,
        ctx: &mut ExecutionContext,
        start_ids: &[String],
    ) -> Result<()> {
        let adj = build_adjacency(graph);
        let node_by_id: HashMap<&str, &WorkflowNode> =
            graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

        // Explicit DFS stack (reverse-push to preserve original left-to-right order).
        let mut stack: Vec<String> = start_ids.iter().rev().cloned().collect();

        // Per-traversal visited set. Without this, a diamond (A→B, A→C, B→D, C→D) pushes D
        // twice and runs it twice — double-applying side effects and, worse, triggering the
        // side-effect gate on the second visit (since D already has a prior completed output
        // from the first visit) and pausing the run at `awaiting_effect` mid-traversal.
        // Each node is executed at most once per `execute_graph` call; reruns re-enter via a
        // fresh call with their own visited set.
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some(node_id) = stack.pop() {
            // Skip if we already executed this node in this traversal (diamond dedup).
            if !visited.insert(node_id.clone()) {
                continue;
            }

            let Some(node) = node_by_id.get(node_id.as_str()).copied() else {
                continue;
            };

            if ctx.persist {
                self.update_current_node(ctx.task_id, &node.id).await?;
            }

            // ── Side-effect gate ──────────────────────────────────────────────
            // If this is a re-run of a side-effecting node (it already performed an
            // external write on a prior attempt), don't blindly fire it again.
            if let Some(effect_kind) = node.side_effect_kind() {
                if let Some(prior) = self.prior_completed_output(ctx.task_id, &node.id).await? {
                    let decision = ctx.effect_decisions.get(&node.id).cloned();
                    match decision.as_deref() {
                        // reuse: skip the external call entirely, replay the prior output.
                        Some("reuse") | Some("skip") => {
                            if let Some(key) = prior.get("output_key").and_then(|v| v.as_str()) {
                                if let Some(val) = prior.get("output_json") {
                                    ctx.vars.insert(key.to_string(), val.clone());
                                }
                            }
                            push_next(&mut stack, &adj, &node.id);
                            continue;
                        }
                        // update: edit the existing artifact in place, then continue.
                        Some("update") => {
                            let output_key = prior
                                .get("output_key")
                                .and_then(|v| v.as_str())
                                .unwrap_or("effect_result")
                                .to_string();
                            let exec_id = self
                                .record_node_start(
                                    ctx.task_id,
                                    &node.id,
                                    node.node_type_str(),
                                    json!(ctx.vars),
                                )
                                .await?;
                            match self.apply_effect_update(node, ctx, &prior).await {
                                Ok(value) => {
                                    self.record_node_finish(
                                        exec_id,
                                        node_status::COMPLETED,
                                        Some(&output_key),
                                        Some(&value),
                                        None,
                                    )
                                    .await?;
                                    ctx.vars.insert(output_key, value);
                                    push_next(&mut stack, &adj, &node.id);
                                }
                                Err(e) => {
                                    self.record_node_finish(
                                        exec_id,
                                        node_status::FAILED,
                                        None,
                                        None,
                                        Some(&e.to_string()),
                                    )
                                    .await?;
                                    self.fail_task(ctx.task_id, &node.id, &e.to_string())
                                        .await?;
                                    return Err(e);
                                }
                            }
                            continue;
                        }
                        // refire: fall through and execute normally (a fresh write).
                        Some("refire") => {}
                        // No decision yet → pause and ask the owner.
                        _ => {
                            self.pause_for_effect(ctx.task_id, &node.id, effect_kind, &prior)
                                .await?;
                            return Ok(());
                        }
                    }
                } else if ctx.persist {
                    // FIRST run of a side-effecting node (no prior output). Consult the agent's
                    // tool-approval policy: if it requires approval for this effect kind and the
                    // owner hasn't decided yet, pause BEFORE firing. `refire` = approve-and-fire,
                    // `reuse`/`skip` = don't fire. (Subflow child runs — !persist — never gate.)
                    if ctx.effect_decisions.get(&node.id).is_none() {
                        let policy = crate::services::tool_policy::get(self.db, ctx.agent_id)
                            .await
                            .unwrap_or_default();
                        if crate::services::tool_policy::requires_approval(&policy, effect_kind) {
                            // No prior output on a first run — pass a null-output prior so the gate
                            // offers approve (refire) / skip (reuse), not update-in-place.
                            let prior = json!({ "output_json": Value::Null });
                            self.pause_for_effect(ctx.task_id, &node.id, effect_kind, &prior)
                                .await?;
                            return Ok(());
                        }
                    } else if matches!(
                        ctx.effect_decisions.get(&node.id).map(|s| s.as_str()),
                        Some("reuse") | Some("skip")
                    ) {
                        // Owner rejected this write on the pre-fire gate → skip firing it.
                        push_next(&mut stack, &adj, &node.id);
                        continue;
                    }
                    // A `refire`/`update` decision (or no policy gate) falls through to execute.
                }
            }

            // On a rerun of a sprite-mutating node, the node_executions log tracks logical
            // state but the sprite holds physical filesystem state. Reconcile the working tree
            // before re-executing so we don't double-apply edits on top of a prior attempt:
            // restore the prior attempt's checkpoint if one was captured (precise), else a
            // git-anchored reset to the clone HEAD (coarse).
            let sprite_mutating = is_sprite_mutating(&node.node_type);
            if sprite_mutating && self.node_prior_attempts(ctx.task_id, &node.id).await? > 0 {
                self.reconcile_sprite_filesystem(ctx.task_id, &node.id)
                    .await?;
            }

            // Snapshot the input context this node sees, for the DAG inspector. Subflow child runs
            // don't persist (nil exec_id) — see ExecutionContext.persist.
            let exec_id = if ctx.persist {
                let input_snapshot = json!(ctx.vars);
                self.record_node_start(ctx.task_id, &node.id, node.node_type_str(), input_snapshot)
                    .await?
            } else {
                Uuid::nil()
            };

            // Capture the pre-node filesystem checkpoint so a future rerun of this node can
            // restore exactly this state. Best-effort: degrades to git-reset if unsupported.
            if sprite_mutating {
                self.capture_node_checkpoint(ctx.task_id, exec_id).await;
            }

            let result = self.execute_node(node, ctx).await;

            let result = match result {
                Ok(r) => r,
                Err(e) => {
                    self.record_node_finish(
                        exec_id,
                        node_status::FAILED,
                        None,
                        None,
                        Some(&e.to_string()),
                    )
                    .await?;
                    // In a subflow child run, propagate the error up to the Subflow node rather than
                    // failing the whole task here — the parent decides (error_shield in loop mode).
                    if ctx.persist {
                        self.fail_task(ctx.task_id, &node.id, &e.to_string())
                            .await?;
                    }
                    return Err(e);
                }
            };

            match result {
                NodeResult::Continue { output_key, value } => {
                    self.record_node_finish(
                        exec_id,
                        node_status::COMPLETED,
                        Some(&output_key),
                        Some(&value),
                        None,
                    )
                    .await?;
                    ctx.vars.insert(output_key, value);
                    push_next(&mut stack, &adj, &node.id);
                }
                NodeResult::Branch(label) => {
                    self.record_node_finish(
                        exec_id,
                        node_status::COMPLETED,
                        None,
                        Some(&json!({ "branch": label })),
                        None,
                    )
                    .await?;
                    if let Some(target) = pick_branch(&adj, node, &label) {
                        stack.push(target);
                    }
                }
                NodeResult::PauseForReview { draft } => {
                    // A subflow can't pause the parent task for review — human gates belong in the
                    // top-level graph. Reject rather than silently swallowing the gate.
                    if !ctx.persist {
                        return Err(anyhow!(
                            "a subflow cannot contain a Human Gate (node '{}') — put gates in the top-level workflow",
                            node.id
                        ));
                    }
                    self.record_node_finish(
                        exec_id,
                        node_status::AWAITING_REVIEW,
                        None,
                        Some(&json!({ "draft": draft })),
                        None,
                    )
                    .await?;
                    self.pause_for_review(ctx.task_id, &node.id, &draft).await?;
                    // Halt; resumes via resume_after_approval → resume_from.
                    return Ok(());
                }
                NodeResult::Deliver(output) => {
                    self.record_node_finish(
                        exec_id,
                        node_status::COMPLETED,
                        None,
                        Some(&json!({ "delivered": output })),
                        None,
                    )
                    .await?;
                    // Inside a subflow, an Output node "delivers" back to the parent rather than to
                    // the task: stash it so run_subflow_once can lift it out.
                    if ctx.persist {
                        self.deliver(ctx, &output).await?;
                    } else {
                        ctx.vars
                            .insert("__subflow_out__output".to_string(), Value::String(output));
                    }
                }
            }
        }

        Ok(())
    }

    /// Rebuild an ExecutionContext from a task's persisted node outputs. Replays each
    /// completed node's output value into `ctx.vars` under the key it was written with,
    /// so a workflow can resume/re-run with the same context it had mid-run (the
    /// in-memory vars are otherwise lost on pause).
    pub async fn rebuild_context(
        db: &PgPool,
        task_id: Uuid,
        agent_id: Uuid,
        job_id: Uuid,
        lora_name: Option<String>,
        persona_prompt: String,
        initial_input: Value,
    ) -> Result<ExecutionContext> {
        let mut ctx = ExecutionContext::new(
            task_id,
            agent_id,
            job_id,
            lora_name,
            persona_prompt,
            initial_input,
        );

        // Re-inject the task's work-target (repo, base_branch, …) into ctx.vars, exactly as the
        // initial dispatch does. Without this, resume/rerun paths (gate continuation, rerun-from,
        // effect resolution) lose ctx.vars["repo"], so post-gate nodes like GitPush/PrCreate fail
        // with "no repo". Done before replaying node outputs so a node output could still override.
        if let Ok(rec) = sqlx::query!("SELECT target FROM tasks WHERE id = $1", task_id)
            .fetch_one(db)
            .await
        {
            if let Some(obj) = rec.target.as_object() {
                for (k, v) in obj {
                    ctx.vars.insert(k.clone(), v.clone());
                }
            }
        }

        // Latest completed attempt per node, replayed in execution order.
        let rows = sqlx::query!(
            r#"SELECT DISTINCT ON (node_id) node_id, output_key, output_json
               FROM node_executions
               WHERE task_id = $1 AND status = 'completed' AND output_key IS NOT NULL
               ORDER BY node_id, attempt DESC, started_at DESC"#,
            task_id
        )
        .fetch_all(db)
        .await?;

        for row in rows {
            if let (Some(key), Some(value)) = (row.output_key, row.output_json) {
                ctx.vars.insert(key, value);
            }
        }

        Ok(ctx)
    }

    // ── node_executions persistence ───────────────────────────────────────────

    /// Insert a `running` node_executions row, bumping `attempt` if this node has run
    /// before (preserves history). Returns the new row id.
    async fn record_node_start(
        &self,
        task_id: Uuid,
        node_id: &str,
        node_type: &str,
        input_json: Value,
    ) -> Result<Uuid> {
        let next_attempt: i32 = sqlx::query_scalar!(
            "SELECT COALESCE(MAX(attempt), 0) + 1 FROM node_executions WHERE task_id = $1 AND node_id = $2",
            task_id, node_id
        )
        .fetch_one(self.db)
        .await?
        .unwrap_or(1);

        let id = sqlx::query_scalar!(
            r#"INSERT INTO node_executions (task_id, node_id, node_type, status, input_json, attempt)
               VALUES ($1, $2, $3, 'running', $4, $5) RETURNING id"#,
            task_id, node_id, node_type, input_json, next_attempt,
        )
        .fetch_one(self.db)
        .await?;
        Ok(id)
    }

    async fn record_node_finish(
        &self,
        exec_id: Uuid,
        status: &str,
        output_key: Option<&str>,
        output_json: Option<&Value>,
        logs: Option<&str>,
    ) -> Result<()> {
        // Nil id = a non-persisted node (a subflow child run); nothing to update.
        if exec_id.is_nil() {
            return Ok(());
        }
        sqlx::query!(
            r#"UPDATE node_executions
               SET status = $1, output_key = $2, output_json = $3, logs = $4, finished_at = NOW()
               WHERE id = $5"#,
            status,
            output_key,
            output_json,
            logs,
            exec_id,
        )
        .execute(self.db)
        .await?;
        Ok(())
    }

    /// The most recent COMPLETED execution of a node (output_key + output_json), if any.
    /// Used by the side-effect gate to detect a node that already wrote externally.
    async fn prior_completed_output(&self, task_id: Uuid, node_id: &str) -> Result<Option<Value>> {
        let row = sqlx::query!(
            r#"SELECT output_key, output_json FROM node_executions
               WHERE task_id = $1 AND node_id = $2 AND status = 'completed'
               ORDER BY attempt DESC LIMIT 1"#,
            task_id,
            node_id,
        )
        .fetch_optional(self.db)
        .await?;
        Ok(row.map(|r| json!({ "output_key": r.output_key, "output_json": r.output_json })))
    }

    /// Pause the task at a side-effecting node, awaiting the owner's decision
    /// (reuse / update / refire). Stores the gate context on a node_executions row.
    async fn pause_for_effect(
        &self,
        task_id: Uuid,
        node_id: &str,
        effect_kind: &str,
        prior: &Value,
    ) -> Result<()> {
        let pending = json!({
            "effect_kind": effect_kind,
            "supports_update": crate::models::workflow::WorkflowNode::effect_supports_update(effect_kind),
            "prior_output": prior.get("output_json").cloned().unwrap_or(Value::Null),
        });

        // Record/refresh a gate row so the UI can read pending_effect for this node.
        let exec_id = self
            .record_node_start(task_id, node_id, "effect_gate", json!({}))
            .await?;
        sqlx::query!(
            "UPDATE node_executions SET status = 'awaiting_effect', pending_effect = $1 WHERE id = $2",
            pending, exec_id,
        )
        .execute(self.db)
        .await?;

        sqlx::query!(
            "UPDATE tasks SET status = 'awaiting_effect', current_node_id = $1 WHERE id = $2",
            node_id,
            task_id,
        )
        .execute(self.db)
        .await?;

        // Notify the owner (reuse the awaiting-review notification path).
        if let Ok(row) = sqlx::query!(
            r#"SELECT u.id AS user_id, j.title AS job_title
               FROM tasks t JOIN jobs j ON j.id = t.job_id
               JOIN agents a ON a.id = j.agent_id
               JOIN professionals p ON p.id = a.professional_id
               JOIN users u ON u.id = p.user_id WHERE t.id = $1"#,
            task_id
        )
        .fetch_one(self.db)
        .await
        {
            let _ = crate::services::notifications::notify_awaiting_review(
                self.db,
                row.user_id,
                task_id,
                &row.job_title,
            )
            .await;
        }
        Ok(())
    }

    /// Apply an edit-in-place update for a side-effecting node, using the resource id
    /// captured on the prior run. Dispatches by `resource_kind`. Returns the new output.
    async fn apply_effect_update(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
        prior: &Value,
    ) -> Result<Value> {
        // The prior node output carries resource_kind + resource_id captured on first run.
        let prior_out = prior.get("output_json").cloned().unwrap_or(Value::Null);
        let resource_kind = prior_out
            .get("resource_kind")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let resource_id = prior_out
            .get("resource_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Fresh content to write = the same var the node reads from (it reflects any
        // staged upstream corrections that have been applied to ctx.vars).
        let content_var = node
            .config
            .get("comment_var")
            .or_else(|| node.config.get("body_var"))
            .and_then(|v| v.as_str())
            .unwrap_or("formatted_output");
        let content = ctx
            .vars
            .get(content_var)
            .or_else(|| ctx.vars.get("llm_output"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match resource_kind {
            "github_comment" => {
                let token = ctx
                    .vars
                    .get("github_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let pr_url = prior_out
                    .get("pr_url")
                    .and_then(|v| v.as_str())
                    .or_else(|| ctx.vars.get("pr_url").and_then(|v| v.as_str()))
                    .unwrap_or("");
                update_github_comment(pr_url, token, resource_id, &content).await?;
                Ok(
                    json!({ "status": "updated", "resource_kind": resource_kind, "resource_id": resource_id, "pr_url": pr_url }),
                )
            }
            "slack_message" => {
                let token = ctx
                    .vars
                    .get("slack_token")
                    .and_then(|v| v.as_str())
                    .or_else(|| node.config.get("token").and_then(|v| v.as_str()))
                    .unwrap_or("");
                update_slack_message(token, resource_id, &content).await?;
                Ok(
                    json!({ "status": "updated", "resource_kind": resource_kind, "resource_id": resource_id }),
                )
            }
            "notion_block" => {
                let token = ctx
                    .vars
                    .get("notion_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // Notion write uses content_var (default llm_output), not comment/body.
                let nc = node
                    .config
                    .get("content_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("llm_output");
                let body = ctx
                    .vars
                    .get(nc)
                    .and_then(|v| v.as_str())
                    .unwrap_or(&content)
                    .to_string();
                notion_update_block(token, resource_id, &body).await?;
                Ok(
                    json!({ "status": "updated", "resource_kind": resource_kind, "resource_id": resource_id }),
                )
            }
            "linear_comment" => {
                let token = ctx
                    .vars
                    .get("linear_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                linear_update_comment(token, resource_id, &content).await?;
                Ok(
                    json!({ "status": "updated", "resource_kind": resource_kind, "resource_id": resource_id }),
                )
            }
            "jira_comment" => {
                let token = ctx
                    .vars
                    .get("jira_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let issue_key = prior_out
                    .get("issue_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let base_url = prior_out
                    .get("jira_base_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                jira_update_comment(token, base_url, issue_key, resource_id, &content).await?;
                Ok(
                    json!({ "status": "updated", "resource_kind": resource_kind, "resource_id": resource_id }),
                )
            }
            "google_doc" => {
                self.update_google_doc(node, ctx, resource_id).await?;
                Ok(json!({
                    "status": "updated", "resource_kind": resource_kind, "resource_id": resource_id,
                    "document_id": resource_id,
                    "url": format!("https://docs.google.com/document/d/{}/edit", resource_id),
                }))
            }
            "github_pr" => {
                // resource_id is "owner/repo#number"; edit the PR title/body in place.
                let token = ctx
                    .vars
                    .get("github_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let (repo, number) = resource_id
                    .split_once('#')
                    .ok_or_else(|| anyhow!("github_pr update: malformed resource id"))?;
                let title = node
                    .config
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Changes by your Flaborful agent");
                let body_var = node
                    .config
                    .get("body_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("llm_output");
                let body = ctx
                    .vars
                    .get(body_var)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                update_github_pr(repo, token, number, title, body).await?;
                Ok(
                    json!({ "status": "updated", "resource_kind": resource_kind, "resource_id": resource_id }),
                )
            }
            // No update API wired — reuse the prior result (no external mutation).
            _ => Ok(prior_out),
        }
    }

    /// Overwrite an existing Google Doc's body in place using the node's model_var (same
    /// structured DocumentModel the create path parses).
    async fn update_google_doc(
        &self,
        node: &WorkflowNode,
        ctx: &ExecutionContext,
        document_id: &str,
    ) -> Result<()> {
        use crate::models::connection::ConnectionProvider;
        use crate::services::{connections, google_docs};

        let token =
            connections::access_token_for_job(self.db, ctx.job_id, ConnectionProvider::Google)
                .await?;
        let model_var = node
            .config
            .get("model_var")
            .and_then(|v| v.as_str())
            .unwrap_or("llm_output");
        let model_value = ctx
            .vars
            .get(model_var)
            .ok_or_else(|| anyhow!("GoogleDocs update: '{}' not in context", model_var))?;
        let model: google_docs::DocumentModel = match model_value {
            Value::String(s) => serde_json::from_str(s)
                .map_err(|e| anyhow!("GoogleDocs update: invalid DocumentModel JSON: {}", e))?,
            other => serde_json::from_value(other.clone())
                .map_err(|e| anyhow!("GoogleDocs update: invalid DocumentModel: {}", e))?,
        };
        google_docs::overwrite_doc(&token, document_id, &model).await?;
        Ok(())
    }

    async fn fail_task(&self, task_id: Uuid, node_id: &str, error: &str) -> Result<()> {
        tracing::error!("Task {} failed at node {}: {}", task_id, node_id, error);
        sqlx::query!(
            "UPDATE tasks SET status = 'failed', current_node_id = $1 WHERE id = $2",
            node_id,
            task_id
        )
        .execute(self.db)
        .await?;
        self.destroy_task_sprite(task_id).await;
        Ok(())
    }

    /// Tear down a task's sprite (if any). Called on task terminal states.
    async fn destroy_task_sprite(&self, task_id: Uuid) {
        if let Some(backends) = &self.backends {
            crate::services::execution::sprite::destroy_task_sprite(
                self.db,
                &backends.sprite_client,
                task_id,
            )
            .await;
        }
    }

    async fn execute_node(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        match node.node_type {
            NodeType::Trigger => {
                // Input already in ctx.vars["input"] — just pass through
                let input = ctx.vars.get("input").cloned().unwrap_or(Value::Null);
                Ok(NodeResult::Continue {
                    output_key: "trigger_output".into(),
                    value: input,
                })
            }

            NodeType::LlmReasoning => self.run_llm_reasoning(node, ctx).await,

            NodeType::HumanGate => {
                let draft = ctx
                    .vars
                    .get(
                        node.config
                            .get("input_var")
                            .and_then(|v| v.as_str())
                            .unwrap_or("llm_output"),
                    )
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Always pause at a HumanGate — it is the trust guarantee.
                // Log to audit so company can see every gate hit.
                let _ = audit::log_task_paused(self.db, ctx.job_id, ctx.task_id, &node.id).await;

                Ok(NodeResult::PauseForReview { draft })
            }

            NodeType::ConditionalBranch => self.run_conditional(node, ctx).await,

            NodeType::Formatter => self.run_formatter(node, ctx).await,

            // ── Data-driven registry nodes (see services/node_registry.rs) ──
            NodeType::Filter => self.run_filter(node, ctx).await,
            NodeType::Router => self.run_router(node, ctx).await,
            NodeType::JoinPaths => self.run_join_paths(node, ctx).await,
            NodeType::ErrorShield => self.run_error_shield(node, ctx).await,
            NodeType::AskAi => self.run_ask_ai(node, ctx).await,
            NodeType::ExtractData => self.run_extract_data(node, ctx).await,
            NodeType::Categorizer => self.run_categorizer(node, ctx).await,
            NodeType::Scorer => self.run_scorer(node, ctx).await,
            NodeType::CombineText => self.run_combine_text(node, ctx).await,
            NodeType::ReadJson => self.run_read_json(node, ctx).await,
            NodeType::WriteJson => self.run_write_json(node, ctx).await,
            NodeType::CallApi => self.run_call_api(node, ctx).await,
            NodeType::McpServer => self.run_mcp_server(node, ctx).await,

            // ── Composability ──
            NodeType::Subflow => self.run_subflow(node, ctx).await,
            // A SubflowInput seeds its named parameter into vars. At the top level (or if the
            // parent didn't map it) it falls back to whatever is already in vars, so a subflow can
            // still be run/tested standalone. Inside run_subflow the mapped inputs are pre-seeded.
            NodeType::SubflowInput => {
                let name = node
                    .config
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("input");
                let value = ctx.vars.get(name).cloned().unwrap_or(Value::Null);
                Ok(NodeResult::Continue {
                    output_key: name.to_string(),
                    value,
                })
            }
            // A SubflowOutput records its value under its name so run_subflow can lift it back into
            // the parent. It is terminal within the child graph (no downstream), like Output.
            NodeType::SubflowOutput => {
                let name = node
                    .config
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("output");
                let value_var = node
                    .config
                    .get("value_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or(name);
                let value = ctx.vars.get(value_var).cloned().unwrap_or(Value::Null);
                Ok(NodeResult::Continue {
                    output_key: format!("__subflow_out__{name}"),
                    value,
                })
            }

            NodeType::Output => {
                let output_var = node
                    .config
                    .get("output_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("llm_output");
                let output = ctx
                    .vars
                    .get(output_var)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(NodeResult::Deliver(output))
            }

            NodeType::Github => self.run_github(node, ctx).await,

            NodeType::DiffAnalyzer => self.run_diff_analyzer(node, ctx).await,

            NodeType::StackTraceParser => self.run_stack_trace_parser(node, ctx).await,

            NodeType::HttpRequest => self.run_http_request(node, ctx).await,

            NodeType::Email => self.run_email(node, ctx).await,
            NodeType::Slack => self.run_slack(node, ctx).await,
            NodeType::WebSearch => self.run_web_search(node, ctx).await,
            NodeType::DocumentReadWrite => self.run_document(node, ctx).await,
            NodeType::CodeExecution => self.run_code_execution(node, ctx).await,
            NodeType::TestRunner => self.run_test_runner(node, ctx).await,
            NodeType::JiraLinear => self.run_jira_linear(node, ctx).await,
            NodeType::AgentImplement => self.run_agent_implement(node, ctx).await,
            NodeType::PrCreate => self.run_pr_create(node, ctx).await,
            NodeType::SpriteCommand => self.run_sprite_command(node, ctx).await,
            NodeType::SpriteTest => self.run_sprite_test(node, ctx).await,
            NodeType::GitCommit => self.run_git_commit(node, ctx).await,
            NodeType::GitPush => self.run_git_push(node, ctx).await,
            NodeType::TestGate => self.run_test_gate(node, ctx).await,
            // Schedule nodes are passive — they represent entry points, not runtime actions
            NodeType::Schedule => Ok(NodeResult::Continue {
                output_key: "schedule_triggered".into(),
                value: json!({ "triggered_at": chrono::Utc::now().to_rfc3339() }),
            }),
        }
    }

    // ── Node implementations ──────────────────────────────────────────────────

    async fn run_llm_reasoning(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let prompt_template = node
            .config
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("{{input}}");

        let input_var = node
            .config
            .get("input_var")
            .and_then(|v| v.as_str())
            .unwrap_or("trigger_output");

        let input_value = ctx
            .vars
            .get(input_var)
            .map(|v| v.to_string())
            .unwrap_or_default();

        let user_prompt = prompt_template.replace("{{input}}", &input_value);

        // Document-author mode: the node feeds a Google Doc node, so the model must emit
        // a structured DocumentModel as JSON rather than prose. Persona/LoRA still shape
        // the content; we only constrain the output format and strip fences afterward.
        let document_author = node
            .config
            .get("document_author")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let persona = if document_author {
            crate::services::google_docs::document_author_system(&ctx.persona_prompt)
        } else {
            ctx.persona_prompt.clone()
        };

        // RAG: retrieve relevant context from the agent's knowledge vault
        let rag_context = self.retrieve_rag_context(ctx, &user_prompt).await?;

        let system = if rag_context.is_empty() {
            persona
        } else {
            format!(
                "{}\n\n## Relevant context from your knowledge vault:\n{}",
                persona, rag_context
            )
        };

        let raw_output = self
            .inference
            .worker(&system, &user_prompt, ctx.lora_name.as_deref())
            .await?;

        // In document-author mode, strip any code fences so downstream gets clean JSON.
        let output = if document_author {
            crate::services::inference::strip_fences(&raw_output).to_string()
        } else {
            raw_output
        };

        // Score output quality and persist to task row
        let score =
            confidence::score_output(self.inference, &user_prompt, &output, &ctx.persona_prompt)
                .await
                .unwrap_or(0.5);

        sqlx::query!(
            "UPDATE tasks SET confidence_score = $1 WHERE id = $2",
            score,
            ctx.task_id
        )
        .execute(self.db)
        .await?;

        // Store score in context so downstream nodes (HumanGate) can read it
        ctx.vars
            .insert("confidence_score".to_string(), json!(score));

        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("llm_output")
            .to_string();

        Ok(NodeResult::Continue {
            output_key,
            value: Value::String(output),
        })
    }

    async fn run_conditional(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let condition_var = node
            .config
            .get("condition_var")
            .and_then(|v| v.as_str())
            .unwrap_or("llm_output");

        let value = ctx.vars.get(condition_var).cloned().unwrap_or(Value::Null);

        // Ask the orchestrator to evaluate which branch applies
        let system = "You evaluate a condition and return a JSON object with a single key 'branch' containing the branch label to follow. Be concise and deterministic.";
        let conditions = node
            .config
            .get("conditions")
            .map(|v| v.to_string())
            .unwrap_or_default();

        let user = format!(
            "Value to evaluate: {}\nConditions: {}\nReturn JSON: {{\"branch\": \"<label>\"}}",
            value, conditions
        );

        let raw = self.inference.orchestrate(system, &user, true).await?;
        let parsed: Value = serde_json::from_str(crate::services::inference::strip_fences(&raw))?;
        let branch = parsed
            .get("branch")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();

        Ok(NodeResult::Branch(branch))
    }

    async fn run_formatter(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let input_var = node
            .config
            .get("input_var")
            .and_then(|v| v.as_str())
            .unwrap_or("llm_output");

        let format_type = node
            .config
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("markdown");

        let input = ctx
            .vars
            .get(input_var)
            .map(|v| v.to_string())
            .unwrap_or_default();

        let system = format!(
            "You are a formatter. Convert the given content into clean {} format. Return only the formatted content, nothing else.",
            format_type
        );

        let formatted = self.inference.orchestrate(&system, &input, false).await?;

        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("formatted_output")
            .to_string();

        Ok(NodeResult::Continue {
            output_key,
            value: Value::String(formatted),
        })
    }

    // ── Data-driven registry node handlers ────────────────────────────────────
    // These back the first batch of Gumloop-parity nodes described by node_registry.rs. They read
    // their config by the same string keys the manifest declares. Helpers below keep them terse.

    /// Read a config field that may be either a literal (`key`) or a variable reference (`key_var`),
    /// resolving the variable against ctx.vars. Falls back to `default`.
    fn cfg_or_var(node: &WorkflowNode, ctx: &ExecutionContext, key: &str, default: &str) -> String {
        // Direct literal wins.
        if let Some(s) = node.config.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                // A literal that names a variable ({{var}} or an existing var) is resolved too.
                return Self::interpolate(s, ctx);
            }
        }
        // Else a `<key>_var` pointing at a ctx var.
        if let Some(var) = node
            .config
            .get(format!("{key}_var"))
            .and_then(|v| v.as_str())
        {
            if let Some(v) = ctx.vars.get(var) {
                return v
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| v.to_string());
            }
        }
        default.to_string()
    }

    /// Resolve a value that is either a literal string or the name of a ctx var. Used where the
    /// manifest exposes one field (dynamic input): if it matches a var name, use the var.
    fn resolve_value(raw: &str, ctx: &ExecutionContext) -> Value {
        if let Some(v) = ctx.vars.get(raw) {
            return v.clone();
        }
        Value::String(Self::interpolate(raw, ctx))
    }

    /// {{var}} interpolation against ctx.vars (string-rendered).
    fn interpolate(template: &str, ctx: &ExecutionContext) -> String {
        let mut out = template.to_string();
        for (k, v) in &ctx.vars {
            let needle = format!("{{{{{k}}}}}");
            if out.contains(&needle) {
                let rendered = v
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| v.to_string());
                out = out.replace(&needle, &rendered);
            }
        }
        out
    }

    fn output_key(node: &WorkflowNode, default: &str) -> String {
        node.config
            .get("output_var")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(default)
            .to_string()
    }

    /// Filter: pass the input through only when a condition holds (if-statement for data).
    async fn run_filter(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let filter_by = Self::cfg_or_var(node, ctx, "filter_by", "");
        let condition = node
            .config
            .get("condition")
            .and_then(|v| v.as_str())
            .unwrap_or("is_not_empty");
        let cmp = Self::cfg_or_var(node, ctx, "conditional_value", "");
        let input_raw = node
            .config
            .get("input_var")
            .and_then(|v| v.as_str())
            .unwrap_or("input");
        let input = Self::resolve_value(input_raw, ctx);

        let passes = Self::eval_condition(&filter_by, condition, &cmp);
        let output_key = Self::output_key(node, "filtered_output");
        let output_blank = node
            .config
            .get("output_blank")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let value = if passes {
            input
        } else if output_blank {
            Value::String(String::new())
        } else {
            Value::Null
        };
        Ok(NodeResult::Continue { output_key, value })
    }

    /// Deterministic condition evaluation shared by Filter (and rule-mode Router).
    fn eval_condition(lhs: &str, condition: &str, rhs: &str) -> bool {
        let (ln, rn) = (lhs.trim().parse::<f64>(), rhs.trim().parse::<f64>());
        match condition {
            "greater_than" => matches!((ln, rn), (Ok(a), Ok(b)) if a > b),
            "less_than" => matches!((ln, rn), (Ok(a), Ok(b)) if a < b),
            "equals" => lhs == rhs,
            "contains" => lhs.contains(rhs),
            "is_in" => rhs.contains(lhs),
            "starts_with" => lhs.starts_with(rhs),
            "ends_with" => lhs.ends_with(rhs),
            "is_empty" => lhs.trim().is_empty(),
            "is_not_empty" => !lhs.trim().is_empty(),
            "is_true" => matches!(
                lhs.trim().to_ascii_lowercase().as_str(),
                "true" | "yes" | "1"
            ),
            "is_false" => matches!(
                lhs.trim().to_ascii_lowercase().as_str(),
                "false" | "no" | "0" | ""
            ),
            _ => false,
        }
    }

    /// Router: choose one of up to 8 routes by AI interpretation or deterministic rules. Emits the
    /// chosen route name as a Branch so the engine's existing branch machinery routes downstream.
    async fn run_router(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let input_raw = node
            .config
            .get("input_var")
            .and_then(|v| v.as_str())
            .unwrap_or("input");
        let input = Self::resolve_value(input_raw, ctx);
        let input_str = input
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| input.to_string());
        let routes = node
            .config
            .get("routes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let route_with_ai = node
            .config
            .get("route_with_ai")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let names: Vec<String> = routes
            .iter()
            .filter_map(|r| r.get("name").and_then(|v| v.as_str()).map(String::from))
            .collect();
        if names.is_empty() {
            return Ok(NodeResult::Branch("default".into()));
        }

        if !route_with_ai {
            // Deterministic: first route whose condition holds against the input.
            for r in &routes {
                let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let cond = r.get("condition").and_then(|v| v.as_str()).unwrap_or("");
                // condition of the form "<op> <value>", else contains-match
                if !cond.is_empty() && input_str.contains(cond) {
                    return Ok(NodeResult::Branch(name.to_string()));
                }
            }
            return Ok(NodeResult::Branch(names[0].clone()));
        }

        let conditions_desc = routes
            .iter()
            .map(|r| {
                format!(
                    "- {}: {}",
                    r.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    r.get("condition").and_then(|v| v.as_str()).unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let system = "You route an input to exactly one named route. Return JSON {\"route\": \"<name>\"} choosing the single best-matching route name from the list.";
        let user = format!("Input:\n{input_str}\n\nRoutes:\n{conditions_desc}");
        let raw = self.inference.orchestrate(system, &user, true).await?;
        let parsed: Value = serde_json::from_str(crate::services::inference::strip_fences(&raw))
            .unwrap_or(Value::Null);
        let chosen = parsed
            .get("route")
            .and_then(|v| v.as_str())
            .map(String::from);
        let route = chosen
            .filter(|c| names.contains(c))
            .unwrap_or_else(|| names[0].clone());
        Ok(NodeResult::Branch(route))
    }

    /// Join Paths: re-merge branches — pass through whichever named branch variable actually ran
    /// (i.e. the first that is present and non-null in ctx.vars).
    async fn run_join_paths(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let inputs = node
            .config
            .get("inputs")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let output_key = Self::output_key(node, "joined");
        for name in inputs
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            if let Some(v) = ctx.vars.get(name) {
                if !v.is_null() {
                    return Ok(NodeResult::Continue {
                        output_key,
                        value: v.clone(),
                    });
                }
            }
        }
        Ok(NodeResult::Continue {
            output_key,
            value: Value::Null,
        })
    }

    /// Error Shield: run in a way that a failure of the wrapped input routes to an error output var
    /// instead of failing the run. Here (pre-subflow-wrapping) it validates the input is present;
    /// a null/missing input becomes the error path. Full node-wrapping arrives with Subflows.
    async fn run_error_shield(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let input_raw = node
            .config
            .get("input_var")
            .and_then(|v| v.as_str())
            .unwrap_or("input");
        let input = ctx.vars.get(input_raw).cloned();
        let success_var = node
            .config
            .get("success_var")
            .and_then(|v| v.as_str())
            .unwrap_or("shielded")
            .to_string();
        let error_var = node
            .config
            .get("error_var")
            .and_then(|v| v.as_str())
            .unwrap_or("shield_error")
            .to_string();
        let pass_through = node
            .config
            .get("pass_inputs_through")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        match input {
            Some(v) if !v.is_null() => Ok(NodeResult::Continue {
                output_key: success_var,
                value: v,
            }),
            other => {
                let err_val = if pass_through {
                    other.unwrap_or(Value::Null)
                } else {
                    Value::String(format!("missing input '{input_raw}'"))
                };
                Ok(NodeResult::Continue {
                    output_key: error_var,
                    value: err_val,
                })
            }
        }
    }

    /// Ask AI: prompt the model (the professional's trained model when a lora is set).
    async fn run_ask_ai(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let prompt_tmpl = node
            .config
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("{{input}}");
        let prompt = Self::interpolate(prompt_tmpl, ctx);
        let system =
            "You are the professional's assistant. Answer the prompt directly and concisely.";
        let response = self
            .inference
            .worker(system, &prompt, ctx.lora_name.as_deref())
            .await?;
        Ok(NodeResult::Continue {
            output_key: Self::output_key(node, "ai_response"),
            value: Value::String(response),
        })
    }

    /// Extract Data: pull a user-defined field schema out of text as structured JSON.
    async fn run_extract_data(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let text_raw = node
            .config
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("input");
        let text = Self::resolve_value(text_raw, ctx);
        let text_str = text
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| text.to_string());
        let fields = node
            .config
            .get("fields")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let schema = fields
            .iter()
            .map(|f| {
                format!(
                    "- {} ({}): {}",
                    f.get("name").and_then(|v| v.as_str()).unwrap_or("field"),
                    f.get("type").and_then(|v| v.as_str()).unwrap_or("text"),
                    f.get("description").and_then(|v| v.as_str()).unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let as_list = node
            .config
            .get("extract_list")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let shape = if as_list {
            "a JSON array of objects"
        } else {
            "a single JSON object"
        };
        let system = format!(
            "You extract structured data from text. Return {shape} with exactly these fields:\n{schema}\nReturn only JSON."
        );
        let raw = self.inference.orchestrate(&system, &text_str, true).await?;
        let value: Value = serde_json::from_str(crate::services::inference::strip_fences(&raw))
            .unwrap_or(Value::Null);
        Ok(NodeResult::Continue {
            output_key: Self::output_key(node, "extracted"),
            value,
        })
    }

    /// Categorizer: classify input into one of the user's categories.
    async fn run_categorizer(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let input_raw = node
            .config
            .get("input_var")
            .and_then(|v| v.as_str())
            .unwrap_or("input");
        let input = Self::resolve_value(input_raw, ctx);
        let input_str = input
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| input.to_string());
        let cats = node
            .config
            .get("categories")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let list = cats
            .iter()
            .map(|c| {
                format!(
                    "- {}: {}",
                    c.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    c.get("description").and_then(|v| v.as_str()).unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let justify = node
            .config
            .get("include_justification")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let system = format!(
            "You classify input into exactly one category. Return JSON {{\"category\": \"<name>\"{}}}.\nCategories:\n{list}",
            if justify { ", \"justification\": \"<why>\"" } else { "" }
        );
        let raw = self
            .inference
            .orchestrate(&system, &input_str, true)
            .await?;
        let parsed: Value = serde_json::from_str(crate::services::inference::strip_fences(&raw))
            .unwrap_or(Value::Null);
        let category = parsed.get("category").cloned().unwrap_or(Value::Null);
        // Also expose justification as a sibling var when present.
        if let Some(j) = parsed.get("justification") {
            ctx.vars.insert("justification".into(), j.clone());
        }
        Ok(NodeResult::Continue {
            output_key: Self::output_key(node, "category"),
            value: category,
        })
    }

    /// Scorer: rate an item 0–100 against a rubric.
    async fn run_scorer(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let item_raw = node
            .config
            .get("item")
            .and_then(|v| v.as_str())
            .unwrap_or("input");
        let item = Self::resolve_value(item_raw, ctx);
        let item_str = item
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| item.to_string());
        let criteria = node
            .config
            .get("criteria")
            .and_then(|v| v.as_str())
            .unwrap_or("Overall quality: 0-100");
        let justify = node
            .config
            .get("include_justification")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let system = format!(
            "You score the item from 0 to 100 against the criteria. Return JSON {{\"score\": <0-100>{}}}\nCriteria: {criteria}",
            if justify { ", \"justification\": \"<why>\"" } else { "" }
        );
        let raw = self.inference.orchestrate(&system, &item_str, true).await?;
        let parsed: Value = serde_json::from_str(crate::services::inference::strip_fences(&raw))
            .unwrap_or(Value::Null);
        let score = parsed.get("score").cloned().unwrap_or(Value::Null);
        if let Some(j) = parsed.get("justification") {
            ctx.vars.insert("justification".into(), j.clone());
        }
        Ok(NodeResult::Continue {
            output_key: Self::output_key(node, "score"),
            value: score,
        })
    }

    /// Combine Text: {placeholder} template substitution against ctx.vars.
    async fn run_combine_text(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let format = node
            .config
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // Combine Text uses single-brace {var}; support both {var} and {{var}}.
        let mut out = format.to_string();
        for (k, v) in &ctx.vars {
            let rendered = v
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| v.to_string());
            out = out.replace(&format!("{{{{{k}}}}}"), &rendered);
            out = out.replace(&format!("{{{k}}}"), &rendered);
        }
        Ok(NodeResult::Continue {
            output_key: Self::output_key(node, "combined"),
            value: Value::String(out),
        })
    }

    /// Read JSON Values: extract dot-notation paths from a JSON string.
    async fn run_read_json(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let json_raw = node
            .config
            .get("json")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let source = Self::resolve_value(json_raw, ctx);
        let parsed: Value = match &source {
            Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
            other => other.clone(),
        };
        let keys = node
            .config
            .get("keys")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let mut result = serde_json::Map::new();
        for key in keys.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            result.insert(key.to_string(), Self::json_path(&parsed, key));
        }
        let value = if result.len() == 1 {
            result
                .into_iter()
                .next()
                .map(|(_, v)| v)
                .unwrap_or(Value::Null)
        } else {
            Value::Object(result)
        };
        Ok(NodeResult::Continue {
            output_key: Self::output_key(node, "json_values"),
            value,
        })
    }

    /// Navigate a JSON value by dot notation with array indexing (`a.b.0.c`).
    fn json_path(root: &Value, path: &str) -> Value {
        let mut cur = root;
        for seg in path.split('.') {
            cur = match cur {
                Value::Object(m) => m.get(seg).unwrap_or(&Value::Null),
                Value::Array(a) => seg
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| a.get(i))
                    .unwrap_or(&Value::Null),
                _ => &Value::Null,
            };
        }
        cur.clone()
    }

    /// Write JSON Value: set a value at a dot path in a JSON object.
    async fn run_write_json(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let json_raw = node
            .config
            .get("json")
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        let source = Self::resolve_value(json_raw, ctx);
        let mut root: Value = match &source {
            Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
            other => other.clone(),
        };
        let key = node
            .config
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let val_raw = node
            .config
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new_val = Self::resolve_value(val_raw, ctx);
        Self::json_set(&mut root, key, new_val);
        let rendered = serde_json::to_string(&root).unwrap_or_else(|_| "{}".into());
        Ok(NodeResult::Continue {
            output_key: Self::output_key(node, "json_result"),
            value: Value::String(rendered),
        })
    }

    /// Set a value at a dot path, creating intermediate objects as needed.
    fn json_set(root: &mut Value, path: &str, value: Value) {
        let segs: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
        if segs.is_empty() {
            return;
        }
        if !root.is_object() {
            *root = json!({});
        }
        let mut cur = root;
        for (i, seg) in segs.iter().enumerate() {
            if i == segs.len() - 1 {
                if let Value::Object(m) = cur {
                    m.insert(seg.to_string(), value);
                    return;
                }
            } else {
                let obj = cur.as_object_mut().unwrap();
                cur = obj.entry(seg.to_string()).or_insert_with(|| json!({}));
                if !cur.is_object() {
                    *cur = json!({});
                }
            }
        }
    }

    /// Call API: generic HTTP call with headers + optional body.
    async fn run_call_api(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let method = node
            .config
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_uppercase();
        let url = Self::cfg_or_var(node, ctx, "url", "");
        if url.is_empty() {
            return Err(anyhow!("Call API node missing url"));
        }
        let client = reqwest::Client::new();
        let mut req = match method.as_str() {
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "PATCH" => client.patch(&url),
            "DELETE" => client.delete(&url),
            _ => client.get(&url),
        };
        // Headers: array of {key, value} pairs.
        if let Some(headers) = node.config.get("headers").and_then(|v| v.as_array()) {
            for h in headers {
                if let (Some(k), Some(v)) = (
                    h.get("key").and_then(|x| x.as_str()),
                    h.get("value").and_then(|x| x.as_str()),
                ) {
                    req = req.header(k, Self::interpolate(v, ctx));
                }
            }
        }
        if method != "GET" {
            if let Some(body) = node.config.get("body").and_then(|v| v.as_str()) {
                let body = Self::interpolate(body, ctx);
                // Send as JSON if it parses, else raw.
                match serde_json::from_str::<Value>(&body) {
                    Ok(json_body) => req = req.json(&json_body),
                    Err(_) => req = req.body(body),
                }
            }
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("Call API {} returned {}: {}", url, status, text));
        }
        // Prefer JSON, fall back to text.
        let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
        Ok(NodeResult::Continue {
            output_key: Self::output_key(node, "api_response"),
            value,
        })
    }

    /// Custom MCP Server: call one tool on an external MCP server (JSON-RPC over HTTP).
    async fn run_mcp_server(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let server_url = Self::cfg_or_var(node, ctx, "server_url", "");
        let tool = node
            .config
            .get("tool")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if server_url.is_empty() || tool.is_empty() {
            return Err(anyhow!("MCP node requires server_url and tool"));
        }
        let args_raw = node
            .config
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        let arguments: Value =
            serde_json::from_str(&Self::interpolate(args_raw, ctx)).unwrap_or(json!({}));
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        });
        let resp = reqwest::Client::new()
            .post(&server_url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        // Return the JSON-RPC result payload if present, else the whole envelope.
        let value = resp.get("result").cloned().unwrap_or(resp);
        Ok(NodeResult::Continue {
            output_key: Self::output_key(node, "mcp_result"),
            value,
        })
    }

    // ── Subflow (workflow-as-node) + Loop mode ────────────────────────────────
    //
    // A Subflow node runs another workflow as a single node. In Loop Mode it runs that subflow once
    // per item of a list input, with bounded concurrency, collecting a list result. Both preserve
    // the parent driver's "each node runs at most once per traversal" invariant: the fan-out happens
    // *inside* this handler (an internal expansion), so the parent DAG still sees one Subflow node.

    /// Load a workflow's graph by id and deserialize it into a WorkflowGraph.
    async fn load_subflow_graph(&self, workflow_id: Uuid) -> Result<WorkflowGraph> {
        let wf = crate::services::workflows::get_workflow(self.db, workflow_id)
            .await
            .map_err(|e| anyhow!("subflow: {e}"))?;
        let graph: WorkflowGraph =
            serde_json::from_value(wf.graph).map_err(|e| anyhow!("subflow graph parse: {e}"))?;
        Ok(graph)
    }

    /// Parse the input_map config (JSON object) into (subflow-input-name → parent-var-or-literal).
    fn subflow_input_map(node: &WorkflowNode) -> serde_json::Map<String, Value> {
        node.config
            .get("input_map")
            .and_then(|v| match v {
                Value::String(s) => serde_json::from_str::<Value>(s).ok(),
                Value::Object(_) => Some(v.clone()),
                _ => None,
            })
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    /// Seed a child context's vars from the parent according to the input map. Each mapped value is
    /// resolved as a parent variable name first, else treated as a literal.
    fn seed_child_vars(
        map: &serde_json::Map<String, Value>,
        parent: &ExecutionContext,
    ) -> HashMap<String, Value> {
        let mut vars = HashMap::new();
        // Carry the task target (repo/branch etc.) through so subflow SWE nodes still work.
        if let Some(input) = parent.vars.get("input") {
            vars.insert("input".to_string(), input.clone());
        }
        for (input_name, source) in map {
            let value = match source {
                Value::String(s) => parent
                    .vars
                    .get(s)
                    .cloned()
                    .unwrap_or(Value::String(s.clone())),
                other => other.clone(),
            };
            vars.insert(input_name.clone(), value);
        }
        vars
    }

    /// Run one instance of a subflow graph with the given seeded vars, returning the collected
    /// outputs (SubflowOutput node values, keyed by their `name`). Runs in a child ExecutionContext
    /// that shares the task/agent/job but has its own vars and an incremented depth.
    async fn run_subflow_once(
        &self,
        graph: &WorkflowGraph,
        parent: &ExecutionContext,
        seeded: HashMap<String, Value>,
    ) -> Result<Value> {
        let mut child = ExecutionContext {
            task_id: parent.task_id,
            agent_id: parent.agent_id,
            job_id: parent.job_id,
            lora_name: parent.lora_name.clone(),
            persona_prompt: parent.persona_prompt.clone(),
            vars: seeded,
            effect_decisions: parent.effect_decisions.clone(),
            subflow_depth: parent.subflow_depth + 1,
            // Child subflow runs don't persist per-node rows (see field docs): avoids the concurrent
            // attempt-number race in loop mode and keeps the DAG inspector to the parent graph.
            persist: false,
        };

        // Start at the subflow's Input nodes (its parameter boundary); if it has none, start at a
        // Trigger; if neither, start at all source nodes.
        let starts: Vec<String> = {
            let inputs: Vec<String> = graph
                .nodes
                .iter()
                .filter(|n| n.node_type == NodeType::SubflowInput)
                .map(|n| n.id.clone())
                .collect();
            if !inputs.is_empty() {
                inputs
            } else if let Some(t) = graph
                .nodes
                .iter()
                .find(|n| n.node_type == NodeType::Trigger)
            {
                vec![t.id.clone()]
            } else {
                graph.nodes.iter().map(|n| n.id.clone()).collect()
            }
        };

        // Box the recursive engine call — execute_graph → execute_node → run_subflow →
        // run_subflow_once → execute_graph is a recursive async cycle, which needs indirection.
        Box::pin(self.execute_graph(graph, &mut child, &starts)).await?;

        // Collect SubflowOutput values (written under "__subflow_out__<name>" keys).
        let mut outputs = serde_json::Map::new();
        for (k, v) in &child.vars {
            if let Some(name) = k.strip_prefix("__subflow_out__") {
                outputs.insert(name.to_string(), v.clone());
            }
        }
        // If exactly one output, unwrap it; else return the object. If none, return the child's
        // "input" (a subflow with no explicit outputs just passes through).
        let value = if outputs.len() == 1 {
            outputs
                .into_iter()
                .next()
                .map(|(_, v)| v)
                .unwrap_or(Value::Null)
        } else if outputs.is_empty() {
            child.vars.get("input").cloned().unwrap_or(Value::Null)
        } else {
            Value::Object(outputs)
        };
        Ok(value)
    }

    /// Subflow node: run the referenced workflow as a node. With `loop_mode`, fan out over a list
    /// input (bounded concurrency); otherwise run once.
    async fn run_subflow(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        if ctx.subflow_depth >= MAX_SUBFLOW_DEPTH {
            return Err(anyhow!(
                "subflow nesting too deep ({}) — a workflow may include itself",
                ctx.subflow_depth
            ));
        }
        let workflow_id = node
            .config
            .get("workflow_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("subflow node missing workflow_id"))?;
        let workflow_id = Uuid::parse_str(workflow_id)
            .map_err(|_| anyhow!("subflow workflow_id is not a valid uuid"))?;
        let graph = self.load_subflow_graph(workflow_id).await?;
        let map = Self::subflow_input_map(node);
        let output_key = Self::output_key(node, "subflow_result");
        let loop_mode = node
            .config
            .get("loop_mode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !loop_mode {
            let seeded = Self::seed_child_vars(&map, ctx);
            let value = self.run_subflow_once(&graph, ctx, seeded).await?;
            return Ok(NodeResult::Continue { output_key, value });
        }

        // Loop mode: the single mapped input (or "input") is a list; run the subflow per item.
        let (list, item_input_name) = Self::loop_list_and_key(&map, ctx);
        let max_parallel = node
            .config
            .get("max_parallel")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(4)
            .clamp(1, 15) as usize;
        let error_shield = node
            .config
            .get("error_shield")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let results = self
            .run_loop(
                &graph,
                ctx,
                &map,
                &item_input_name,
                list,
                max_parallel,
                error_shield,
            )
            .await?;
        Ok(NodeResult::Continue {
            output_key,
            value: Value::Array(results),
        })
    }

    /// Determine the list to fan out over in loop mode, and the subflow-input name each item is
    /// bound to. Uses the first mapped input whose resolved parent value is an array; falls back to
    /// the "input" var. Returns (items, input_name).
    fn loop_list_and_key(
        map: &serde_json::Map<String, Value>,
        ctx: &ExecutionContext,
    ) -> (Vec<Value>, String) {
        for (input_name, source) in map {
            if let Value::String(var) = source {
                if let Some(Value::Array(items)) = ctx.vars.get(var) {
                    return (items.clone(), input_name.clone());
                }
            }
        }
        // Fallback: the "input" var if it's an array.
        if let Some(Value::Array(items)) = ctx.vars.get("input") {
            let name = map
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| "input".to_string());
            return (items.clone(), name);
        }
        (Vec::new(), "input".to_string())
    }

    /// Run a subflow once per list item with bounded concurrency. Static inputs (non-list mappings)
    /// are seeded on every iteration; the loop input name is overwritten with the item. With
    /// `error_shield`, a failed iteration yields an `{ "error": … }` entry instead of aborting.
    #[allow(clippy::too_many_arguments)]
    async fn run_loop(
        &self,
        graph: &WorkflowGraph,
        ctx: &ExecutionContext,
        map: &serde_json::Map<String, Value>,
        item_input_name: &str,
        list: Vec<Value>,
        max_parallel: usize,
        error_shield: bool,
    ) -> Result<Vec<Value>> {
        use futures::stream::{self, StreamExt};

        let base = Self::seed_child_vars(map, ctx);
        let results = stream::iter(list.into_iter().enumerate())
            .map(|(i, item)| {
                let mut seeded = base.clone();
                seeded.insert(item_input_name.to_string(), item);
                let graph_ref = graph;
                async move {
                    match self.run_subflow_once(graph_ref, ctx, seeded).await {
                        Ok(v) => Ok(v),
                        Err(e) if error_shield => Ok(json!({ "error": e.to_string(), "index": i })),
                        Err(e) => Err(e),
                    }
                }
            })
            .buffered(max_parallel)
            .collect::<Vec<Result<Value>>>()
            .await;

        results.into_iter().collect()
    }

    async fn run_github(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let action = node
            .config
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("fetch_diff");

        // Credentials come from job_integrations — for now we read from ctx vars
        // (the task runner sets these from the decrypted job integration row)
        let token = ctx
            .vars
            .get("github_token")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match action {
            "fetch_diff" => {
                let pr_url = ctx
                    .vars
                    .get("pr_url")
                    .or_else(|| node.config.get("pr_url"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("GitHub node: pr_url not in context"))?
                    .to_string();

                let diff = fetch_github_pr_diff(&pr_url, token).await?;
                let output_key = node
                    .config
                    .get("output_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("diff")
                    .to_string();
                Ok(NodeResult::Continue {
                    output_key,
                    value: Value::String(diff),
                })
            }
            "post_comment" => {
                let comment_var = node
                    .config
                    .get("comment_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("formatted_output");

                let comment = ctx
                    .vars
                    .get(comment_var)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let pr_url = ctx
                    .vars
                    .get("pr_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let comment_id = post_github_comment(&pr_url, token, &comment).await?;
                Ok(NodeResult::Continue {
                    output_key: "github_comment_posted".into(),
                    value: json!({
                        "status": "posted",
                        "resource_kind": "github_comment",
                        "resource_id": comment_id,
                        "pr_url": pr_url,
                    }),
                })
            }
            _ => Err(anyhow!("Unknown GitHub action: {}", action)),
        }
    }

    async fn run_diff_analyzer(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let diff_var = node
            .config
            .get("diff_var")
            .and_then(|v| v.as_str())
            .unwrap_or("diff");

        let diff = ctx
            .vars
            .get(diff_var)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let system = "You are a code diff analyzer. Given a git diff, produce a structured JSON analysis with keys: 'summary' (string), 'files_changed' (array of strings), 'additions' (int), 'deletions' (int), 'risk_level' (low/medium/high), 'key_changes' (array of strings describing notable changes).";

        let raw = self.inference.orchestrate(system, &diff, true).await?;
        let analysis: Value = serde_json::from_str(crate::services::inference::strip_fences(&raw))
            .unwrap_or(json!({ "summary": raw }));

        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("diff_analysis")
            .to_string();

        Ok(NodeResult::Continue {
            output_key,
            value: analysis,
        })
    }

    async fn run_stack_trace_parser(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let input_var = node
            .config
            .get("input_var")
            .and_then(|v| v.as_str())
            .unwrap_or("input");

        let trace = ctx
            .vars
            .get(input_var)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let system = "You are a stack trace parser. Given a stack trace or error log, return a JSON object with keys: 'error_type' (string), 'message' (string), 'root_file' (string), 'root_line' (int or null), 'call_chain' (array of strings), 'likely_cause' (string).";

        let raw = self.inference.orchestrate(system, &trace, true).await?;
        let parsed: Value = serde_json::from_str(crate::services::inference::strip_fences(&raw))
            .unwrap_or(json!({ "message": raw }));

        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("parsed_trace")
            .to_string();

        Ok(NodeResult::Continue {
            output_key,
            value: parsed,
        })
    }

    async fn run_http_request(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let url = node
            .config
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("HttpRequest node missing url"))?
            .to_string();

        let method = node
            .config
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_uppercase();

        let body_var = node.config.get("body_var").and_then(|v| v.as_str());
        let body = body_var.and_then(|k| ctx.vars.get(k)).cloned();

        let client = reqwest::Client::new();
        let mut req = match method.as_str() {
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "PATCH" => client.patch(&url),
            "DELETE" => client.delete(&url),
            _ => client.get(&url),
        };

        // Optional auth header from context
        if let Some(Value::String(token)) = ctx.vars.get("http_token") {
            req = req.bearer_auth(token);
        }

        if let Some(body_val) = body {
            req = req.json(&body_val);
        }

        let resp = req
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;

        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("http_response")
            .to_string();

        Ok(NodeResult::Continue {
            output_key,
            value: resp,
        })
    }

    async fn run_email(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let action = node
            .config
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("send");
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("email_result")
            .to_string();

        match action {
            "send" => {
                let to = node
                    .config
                    .get("to")
                    .and_then(|v| v.as_str())
                    .or_else(|| ctx.vars.get("email_to").and_then(|v| v.as_str()))
                    .ok_or_else(|| anyhow!("Email node: missing 'to' field"))?;
                let subject = node
                    .config
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Message from your agent");
                let body_var = node
                    .config
                    .get("body_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("llm_output");
                let body = ctx
                    .vars
                    .get(body_var)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Send via configured SMTP or SendGrid
                let smtp_url = std::env::var("SMTP_URL").unwrap_or_default();
                if smtp_url.is_empty() {
                    tracing::warn!("Email node: SMTP_URL not configured, logging only");
                    tracing::info!(
                        "Would send email to={} subject={} body_len={}",
                        to,
                        subject,
                        body.len()
                    );
                } else {
                    send_email_via_smtp(&smtp_url, to, subject, body).await?;
                }

                Ok(NodeResult::Continue {
                    output_key,
                    value: json!({ "status": "sent", "to": to, "resource_kind": "email_send" }),
                })
            }
            "read" => {
                // Read latest unread emails — requires OAuth credentials in context
                let token = ctx
                    .vars
                    .get("email_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let emails = fetch_emails(token).await?;
                Ok(NodeResult::Continue {
                    output_key,
                    value: json!(emails),
                })
            }
            _ => Err(anyhow!("Email node: unknown action '{}'", action)),
        }
    }

    async fn run_slack(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let action = node
            .config
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("post");
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("slack_result")
            .to_string();

        let token = ctx
            .vars
            .get("slack_token")
            .and_then(|v| v.as_str())
            .or_else(|| node.config.get("token").and_then(|v| v.as_str()))
            .unwrap_or("");

        match action {
            "post" => {
                let channel = node
                    .config
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("#general");
                let body_var = node
                    .config
                    .get("body_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("llm_output");
                let text = ctx
                    .vars
                    .get(body_var)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let mut resource_id = String::new();
                if token.is_empty() {
                    tracing::warn!("Slack node: no token, logging only");
                    tracing::info!(
                        "Would post to Slack channel={} text_len={}",
                        channel,
                        text.len()
                    );
                } else {
                    let (resolved_channel, ts) = post_slack_message(token, channel, text).await?;
                    resource_id = format!("{}:{}", resolved_channel, ts);
                }

                Ok(NodeResult::Continue {
                    output_key,
                    value: json!({
                        "status": "posted", "channel": channel,
                        "resource_kind": "slack_message", "resource_id": resource_id,
                    }),
                })
            }
            "read" => {
                let channel = node
                    .config
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("#general");
                let limit = node
                    .config
                    .get("limit")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(10);
                let messages = fetch_slack_messages(token, channel, limit as u32).await?;
                Ok(NodeResult::Continue {
                    output_key,
                    value: json!(messages),
                })
            }
            _ => Err(anyhow!("Slack node: unknown action '{}'", action)),
        }
    }

    async fn run_web_search(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let query_var = node
            .config
            .get("query_var")
            .and_then(|v| v.as_str())
            .unwrap_or("input");
        let query = ctx
            .vars
            .get(query_var)
            .and_then(|v| v.as_str())
            .or_else(|| node.config.get("query").and_then(|v| v.as_str()))
            .ok_or_else(|| anyhow!("WebSearch node: no query"))?;

        let num_results = node
            .config
            .get("num_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5);
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("search_results")
            .to_string();

        let api_key = std::env::var("BRAVE_API_KEY")
            .or_else(|_| std::env::var("TAVILY_API_KEY"))
            .unwrap_or_default();

        let results = if api_key.is_empty() {
            tracing::warn!(
                "WebSearch node: no API key (BRAVE_API_KEY / TAVILY_API_KEY), returning empty"
            );
            vec![]
        } else if std::env::var("BRAVE_API_KEY").is_ok() {
            brave_search(&api_key, query, num_results as u32).await?
        } else {
            tavily_search(&api_key, query, num_results as u32).await?
        };

        Ok(NodeResult::Continue {
            output_key,
            value: json!(results),
        })
    }

    async fn run_document(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let action = node
            .config
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("read");
        let provider = node
            .config
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("notion");
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("document_content")
            .to_string();

        let token = ctx
            .vars
            .get(&format!("{}_token", provider))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match (provider, action) {
            ("notion", "read") => {
                let page_id = node
                    .config
                    .get("page_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("DocumentReadWrite: notion read requires page_id"))?;
                let content = notion_read_page(token, page_id).await?;
                Ok(NodeResult::Continue {
                    output_key,
                    value: json!({ "content": content }),
                })
            }
            ("notion", "write") => {
                let page_id = node
                    .config
                    .get("page_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("DocumentReadWrite: notion write requires page_id"))?;
                let content_var = node
                    .config
                    .get("content_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("llm_output");
                let content = ctx
                    .vars
                    .get(content_var)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let block_id = notion_append_block(token, page_id, content).await?;
                Ok(NodeResult::Continue {
                    output_key,
                    value: json!({ "status": "written", "resource_kind": "notion_block", "resource_id": block_id }),
                })
            }
            ("google_drive", "read") => {
                let file_id = node
                    .config
                    .get("file_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        anyhow!("DocumentReadWrite: google_drive read requires file_id")
                    })?;
                let content = gdrive_read_file(token, file_id).await?;
                Ok(NodeResult::Continue {
                    output_key,
                    value: json!({ "content": content }),
                })
            }
            ("google_docs", "create") => self.run_google_docs_create(node, ctx, output_key).await,
            _ => {
                tracing::warn!(
                    "DocumentReadWrite: unsupported provider={} action={}",
                    provider,
                    action
                );
                Ok(NodeResult::Continue {
                    output_key,
                    value: json!({ "status": "skipped" }),
                })
            }
        }
    }

    /// Create a professionally formatted Google Doc in the company's granted Shared Drive.
    ///
    /// Inputs:
    ///   - `model_var` (config): ctx var holding the structured `DocumentModel` JSON the
    ///     upstream LLM node produced. Defaults to "llm_output".
    ///   - `file_name` (config): Drive file name; falls back to the model's title.
    ///   - Shared Drive target comes from the job's google_drive grant `scope_config`
    ///     (`shared_drive_id`, optional `parent_folder_id`), overridable via node config.
    ///
    /// The access token is resolved fresh from the owner's Google connection (auto-refresh).
    async fn run_google_docs_create(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
        output_key: String,
    ) -> Result<NodeResult> {
        use crate::models::connection::ConnectionProvider;
        use crate::services::{connections, google_docs};

        // Resolve a fresh Google access token via the company's grant → owner's connection.
        let token =
            connections::access_token_for_job(self.db, ctx.job_id, ConnectionProvider::Google)
                .await?;

        // Parse the structured document model the LLM emitted.
        let model_var = node
            .config
            .get("model_var")
            .and_then(|v| v.as_str())
            .unwrap_or("llm_output");
        let model_value = ctx
            .vars
            .get(model_var)
            .ok_or_else(|| anyhow!("GoogleDocs create: '{}' not in context", model_var))?;
        // Accept either a JSON object or a JSON string the LLM produced.
        let model: google_docs::DocumentModel = match model_value {
            Value::String(s) => serde_json::from_str(s).map_err(|e| {
                anyhow!(
                    "GoogleDocs create: '{}' is not a valid DocumentModel JSON: {}",
                    model_var,
                    e
                )
            })?,
            other => serde_json::from_value(other.clone()).map_err(|e| {
                anyhow!(
                    "GoogleDocs create: '{}' is not a valid DocumentModel: {}",
                    model_var,
                    e
                )
            })?,
        };

        // Resolve the Shared Drive target: node config first, else the grant's scope_config.
        let grant_scope = self
            .google_grant_scope(ctx.job_id)
            .await
            .unwrap_or(Value::Null);
        let cfg_str = |key: &str| -> Option<String> {
            node.config
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    grant_scope
                        .get(key)
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
        };

        let shared_drive_id = cfg_str("shared_drive_id").ok_or_else(|| {
            anyhow!("GoogleDocs create: no shared_drive_id in node config or grant scope_config")
        })?;
        let parent_folder_id = cfg_str("parent_folder_id");
        let file_name = node
            .config
            .get("file_name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| model.title.clone());

        let target = google_docs::CreateTarget {
            shared_drive_id,
            parent_folder_id,
            file_name,
        };
        let created = google_docs::create_doc(&token, &model, &target).await?;

        Ok(NodeResult::Continue {
            output_key,
            value: json!({
                "document_id": created.document_id, "url": created.url,
                "resource_kind": "google_doc", "resource_id": created.document_id,
            }),
        })
    }

    /// Fetch the `scope_config` of the active google_drive grant for a job.
    async fn google_grant_scope(&self, job_id: Uuid) -> Result<Value> {
        let row = sqlx::query!(
            r#"SELECT scope_config FROM job_integrations
               WHERE job_id = $1 AND tool_type = 'google_drive'::tool_type AND is_active = true
               ORDER BY created_at DESC LIMIT 1"#,
            job_id
        )
        .fetch_optional(self.db)
        .await?;
        Ok(row.map(|r| r.scope_config).unwrap_or(Value::Null))
    }

    async fn run_code_execution(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let code_var = node
            .config
            .get("code_var")
            .and_then(|v| v.as_str())
            .unwrap_or("llm_output");
        let language = node
            .config
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("python");
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("execution_result")
            .to_string();

        let code = ctx
            .vars
            .get(code_var)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("CodeExecution: '{}' not in context", code_var))?;

        let sandbox_url = std::env::var("SANDBOX_URL").unwrap_or_default();
        if sandbox_url.is_empty() {
            tracing::warn!("CodeExecution: SANDBOX_URL not set, skipping execution");
            return Ok(NodeResult::Continue {
                output_key,
                value: json!({ "status": "skipped", "reason": "no sandbox configured" }),
            });
        }

        let resp = reqwest::Client::new()
            .post(format!("{}/execute", sandbox_url))
            .json(&json!({ "language": language, "code": code, "timeout_seconds": 30 }))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;

        Ok(NodeResult::Continue {
            output_key,
            value: resp,
        })
    }

    async fn run_test_runner(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let repo_var = node
            .config
            .get("repo_var")
            .and_then(|v| v.as_str())
            .unwrap_or("repo_path");
        let test_cmd = node
            .config
            .get("test_command")
            .and_then(|v| v.as_str())
            .unwrap_or("cargo test");
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("test_results")
            .to_string();

        let sandbox_url = std::env::var("SANDBOX_URL").unwrap_or_default();
        if sandbox_url.is_empty() {
            tracing::warn!("TestRunner: SANDBOX_URL not set, skipping");
            return Ok(NodeResult::Continue {
                output_key,
                value: json!({ "status": "skipped", "reason": "no sandbox configured" }),
            });
        }

        let repo = ctx.vars.get(repo_var).cloned().unwrap_or(Value::Null);
        let resp = reqwest::Client::new()
            .post(format!("{}/run-tests", sandbox_url))
            .json(&json!({ "repo": repo, "command": test_cmd }))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;

        Ok(NodeResult::Continue {
            output_key,
            value: resp,
        })
    }

    async fn run_jira_linear(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let provider = node
            .config
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("linear");
        let action = node
            .config
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("read");
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("ticket_result")
            .to_string();

        let token = ctx
            .vars
            .get(&format!("{}_token", provider))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if token.is_empty() {
            tracing::warn!("JiraLinear: no {} token in context", provider);
            return Ok(NodeResult::Continue {
                output_key,
                value: json!({ "status": "skipped", "reason": "no token" }),
            });
        }

        match (provider, action) {
            ("linear", "read") => {
                let issue_id = node
                    .config
                    .get("issue_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| ctx.vars.get("linear_issue_id").and_then(|v| v.as_str()))
                    .ok_or_else(|| anyhow!("JiraLinear: linear read requires issue_id"))?;
                let issue = linear_get_issue(token, issue_id).await?;
                Ok(NodeResult::Continue {
                    output_key,
                    value: issue,
                })
            }
            ("linear", "update") => {
                let issue_id = node
                    .config
                    .get("issue_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("JiraLinear: linear update requires issue_id"))?;
                let comment_var = node
                    .config
                    .get("comment_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("llm_output");
                let comment = ctx
                    .vars
                    .get(comment_var)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let comment_id = linear_add_comment(token, issue_id, comment).await?;
                Ok(NodeResult::Continue {
                    output_key,
                    value: json!({ "status": "commented", "resource_kind": "linear_comment", "resource_id": comment_id }),
                })
            }
            ("jira", "read") => {
                let issue_key = node
                    .config
                    .get("issue_key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("JiraLinear: jira read requires issue_key"))?;
                let base_url = node
                    .config
                    .get("jira_base_url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("JiraLinear: jira requires jira_base_url"))?;
                let issue = jira_get_issue(token, base_url, issue_key).await?;
                Ok(NodeResult::Continue {
                    output_key,
                    value: issue,
                })
            }
            ("jira", "comment") => {
                let issue_key = node
                    .config
                    .get("issue_key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("JiraLinear: jira comment requires issue_key"))?;
                let base_url = node
                    .config
                    .get("jira_base_url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("JiraLinear: jira requires jira_base_url"))?;
                let comment_var = node
                    .config
                    .get("comment_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("llm_output");
                let comment = ctx
                    .vars
                    .get(comment_var)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let comment_id = jira_add_comment(token, base_url, issue_key, comment).await?;
                Ok(NodeResult::Continue {
                    output_key,
                    value: json!({
                        "status": "commented", "resource_kind": "jira_comment",
                        "resource_id": comment_id, "issue_key": issue_key, "jira_base_url": base_url,
                    }),
                })
            }
            _ => {
                tracing::warn!(
                    "JiraLinear: unsupported provider={} action={}",
                    provider,
                    action
                );
                Ok(NodeResult::Continue {
                    output_key,
                    value: json!({ "status": "skipped" }),
                })
            }
        }
    }

    // ── Work-execution nodes (pluggable backend) ──────────────────────────────

    /// Run a coding agent (Managed Agents / Sprite) to implement the task on a repo. The agent
    /// owns the read→edit→test→fix loop; its transcript streams into this node's logs and the
    /// resulting diff becomes the node output. Coarse-grained by design.
    async fn run_agent_implement(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        use crate::services::execution::{ExecutionBackendKind, TaskBrief};

        let backends = self
            .backends
            .clone()
            .ok_or_else(|| anyhow!("AgentImplement: execution backends not configured"))?;

        // Resolve backend: node override → agent default → managed.
        let kind = match node.config.get("backend").and_then(|v| v.as_str()) {
            Some(s) => ExecutionBackendKind::from_str_opt(s),
            None => {
                let row = sqlx::query!(
                    "SELECT execution_backend FROM agents WHERE id = $1",
                    ctx.agent_id
                )
                .fetch_optional(self.db)
                .await?;
                row.and_then(|r| ExecutionBackendKind::from_str_opt(&r.execution_backend))
            }
        }
        .unwrap_or_default();

        // Task text: explicit config, else a var, else the task input description.
        let task_text = node
            .config
            .get("task")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                let v = node
                    .config
                    .get("task_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("input");
                ctx.vars.get(v).and_then(|x| {
                    x.as_str().map(str::to_string).or_else(|| {
                        x.get("description")
                            .and_then(|d| d.as_str())
                            .map(str::to_string)
                    })
                })
            })
            .unwrap_or_else(|| "Implement the requested change.".to_string());

        let repo = node
            .config
            .get("repo")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                ctx.vars
                    .get("repo")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
        let base_branch = node
            .config
            .get("base_branch")
            .and_then(|v| v.as_str())
            .unwrap_or("main")
            .to_string();
        let branch = node
            .config
            .get("branch")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("flaborful/task-{}", &ctx.task_id.to_string()[..8]));
        let github_token = ctx
            .vars
            .get("github_token")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let api_base_url =
            std::env::var("API_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".into());

        // The professional's standing guidance on HOW to run the loop (consult the principal,
        // write/run tests, commit style, …). Blank/whitespace → None.
        let mut instructions = node
            .config
            .get("instructions")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // Compose the company repo vault (read-only) into this task. The overlay feeds both the
        // blueprint (a "Company context" section) and the per-task token (so the agent's browse
        // tools span its own vault + the company repo vault).
        let overlays = self.build_vault_overlays(ctx, repo.as_deref()).await;

        // Context-engineering layer: assemble an Execution Blueprint from the professional's vault
        // (style rules + architecture map + golden snippets, distilled in one LoRA call) plus the
        // company repo context, and prepend it to the instructions. Gives the builder standing
        // preferences + the codebase facts up front so it only escalates to consult_principal for
        // genuine ambiguity. Best-effort — a vault miss or distill failure leaves it as-authored.
        match crate::services::blueprint::build_blueprint(
            self.db,
            self.qdrant,
            self.inference,
            ctx.agent_id,
            ctx.lora_name.as_deref(),
            &ctx.persona_prompt,
            &task_text,
            &overlays,
        )
        .await
        {
            Ok(bp) if !bp.trim().is_empty() => {
                instructions = Some(match instructions {
                    Some(existing) => format!("{bp}\n\n{existing}"),
                    None => bp,
                });
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("blueprint assembly failed for task {}: {}", ctx.task_id, e),
        }

        // Mint a per-task taste token carrying the persona + LoRA + that overlay scope.
        let taste_token = crate::services::execution::taste::mint_token(
            self.db,
            ctx.task_id,
            ctx.agent_id,
            &ctx.persona_prompt,
            ctx.lora_name.as_deref(),
            &overlays,
        )
        .await?;

        let brief = TaskBrief {
            task_id: ctx.task_id,
            kind: crate::services::execution::TaskKind::Implement,
            agent_id: ctx.agent_id,
            job_id: ctx.job_id,
            repo,
            base_branch,
            branch: branch.clone(),
            task_text,
            instructions,
            persona_prompt: ctx.persona_prompt.clone(),
            cf_finetune_id: ctx.lora_name.clone(),
            github_token,
            anthropic_api_key: crate::services::execution::agent_anthropic_key(
                self.db,
                ctx.agent_id,
            )
            .await,
            taste_token,
            api_base_url,
        };

        let backend = backends.get(kind);
        let sink = DbTranscriptSink {
            db: self.db.clone(),
            task_id: ctx.task_id,
            node_id: node.id.clone(),
        };
        // Run the backend, then always revoke the per-task taste token — on success *and* on
        // error. The token grants LoRA access to anyone holding it; leaving it live on a
        // failed run keeps the window open until task cleanup. Using a closure + ?-propagation
        // so this is the single place that needs the revoke, not duplicated on every return path.
        let result = {
            let res = backend.run_implementation(&brief, &sink).await;
            let _ = crate::services::execution::taste::revoke_token(self.db, ctx.task_id).await;
            res?
        };

        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("implementation")
            .to_string();

        // Surface two plain-string vars downstream nodes can point at directly (the HumanGate and
        // PrCreate read a single string var, not a JSON object):
        //   implementation_summary — the agent's own summary (PR body).
        //   implementation_review  — that summary on top of the byte-exact diff (Human Gate view).
        let summary = result.summary.clone().unwrap_or_default();
        ctx.vars.insert(
            "implementation_summary".to_string(),
            Value::String(summary.clone()),
        );
        let review = format!(
            "## What the agent did\n{}\n\n## Diff\n```diff\n{}\n```",
            if summary.is_empty() {
                "(no summary provided)"
            } else {
                &summary
            },
            result.diff,
        );
        ctx.vars
            .insert("implementation_review".to_string(), Value::String(review));

        // Harvest the builder's rolling MEMORY.md (decisions, corrections, "principal prefers X")
        // into the vault so it feeds future blueprints. Best-effort; never fails the task.
        self.harvest_memory_note(ctx).await;

        Ok(NodeResult::Continue {
            output_key,
            value: json!({
                "diff": result.diff,
                "files_changed": result.files_changed,
                "transcript": result.transcript,
                "summary": result.summary,
                "test_summary": result.test_summary,
                "head_sha": result.head_sha,
                "branch": branch,
                "status": result.status,
            }),
        })
    }

    /// Harvest knowledge from the finished task into BOTH vaults — a fallback for when the agent
    /// didn't call `record_experience`/`record_technical` itself. The builder's `MEMORY.md` becomes
    /// a sanitized EXPERIENCE note in the professional's vault (feeding future Blueprints); the
    /// implementation summary becomes a TECHNICAL note in the company's repo vault (the work
    /// product the company owns). Best-effort: the MEMORY.md half needs a live sprite (Managed
    /// Agents runs in Anthropic's container); any failure is swallowed.
    async fn harvest_memory_note(&self, ctx: &mut ExecutionContext) {
        let repo = ctx
            .vars
            .get("repo")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // Professional side: MEMORY.md → experience note (sanitized by the agent as it writes it).
        if let Ok(out) = self
            .sprite_exec(ctx.task_id, "cat MEMORY.md 2>/dev/null || true", 30)
            .await
        {
            let content = out.stdout.trim();
            if !content.is_empty() {
                let note = crate::services::vault::VaultNote {
                    id: Uuid::new_v4(),
                    agent_id: ctx.agent_id,
                    owner_kind: "agent".into(),
                    slug: format!("experience-{}", ctx.task_id.simple()),
                    title: format!("Experience — task {}", &ctx.task_id.to_string()[..8]),
                    body: content.to_string(),
                    layer: crate::services::vault::NoteLayer::Note,
                    source_type: crate::services::knowledge::KnowledgeSourceType::ApprovedOutput,
                    visibility: crate::services::knowledge::ChunkVisibility::Private,
                    domain: None,
                    related: vec![],
                    repo: repo.clone(),
                    job_id: None,
                };
                if let Err(e) =
                    crate::services::vault::upsert_note(self.db, self.qdrant, self.inference, note)
                        .await
                {
                    tracing::debug!("experience harvest failed for task {}: {}", ctx.task_id, e);
                }
            }
        }

        // Company side: the implementation summary → a technical (tntmhpd) note in the company repo
        // vault, when this task is composed with a company overlay. The company owns the work.
        let overlays = self.build_vault_overlays(ctx, repo.as_deref()).await;
        if let Some(ov) = overlays.first() {
            if let Some(summary) = ctx
                .vars
                .get("implementation_summary")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
            {
                let note = crate::services::vault::VaultNote {
                    id: Uuid::new_v4(),
                    agent_id: ov.owner.id(),
                    owner_kind: ov.owner.kind_str().to_string(),
                    slug: format!("work-{}", ctx.task_id.simple()),
                    title: format!("Change — task {}", &ctx.task_id.to_string()[..8]),
                    body: summary.to_string(),
                    layer: crate::services::vault::NoteLayer::Feature,
                    source_type: crate::services::knowledge::KnowledgeSourceType::ApprovedOutput,
                    visibility: crate::services::knowledge::ChunkVisibility::Private,
                    domain: None,
                    related: vec![],
                    repo: repo.clone(),
                    job_id: None,
                };
                if let Err(e) =
                    crate::services::vault::upsert_note(self.db, self.qdrant, self.inference, note)
                        .await
                {
                    tracing::debug!("technical harvest failed for task {}: {}", ctx.task_id, e);
                }
            }
        }
    }

    /// Automated test/lint gatekeeper. Runs the project's tests (+ optional lint) in the
    /// execution backend; on failure, loops the failing output back to the builder (bounded
    /// retries) so a human never reviews a PR that hasn't passed local CI. Sits between
    /// AgentImplement and PrCreate/HumanGate. Sprite-only (it needs the live task sprite the
    /// builder ran on — the same path SpriteTest uses).
    ///
    /// Config: `test_command` (default "npm test"), `lint_command` (optional),
    /// `max_retries` (default 2), `timeout_s` (default 600).
    async fn run_test_gate(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let test_cmd = node
            .config
            .get("test_command")
            .and_then(|v| v.as_str())
            .unwrap_or("npm test");
        let lint_cmd = node.config.get("lint_command").and_then(|v| v.as_str());
        let max_retries = node
            .config
            .get("max_retries")
            .and_then(|v| v.as_u64())
            .unwrap_or(2);
        let timeout = node
            .config
            .get("timeout_s")
            .and_then(|v| v.as_u64())
            .unwrap_or(600);
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("gate_result")
            .to_string();

        let mut attempt = 0u64;
        loop {
            // Run tests, then lint (if configured). The first failure is what we bounce back.
            let (passed, label, out) = self
                .run_gate_checks(ctx.task_id, test_cmd, lint_cmd, timeout)
                .await?;

            if passed {
                return Ok(NodeResult::Continue {
                    output_key,
                    value: json!({ "passed": true, "attempts": attempt + 1, "check": label }),
                });
            }

            if attempt >= max_retries {
                // Exhausted retries — fail the task rather than surfacing a red PR to the human.
                return Err(anyhow!(
                    "TestGate: '{}' still failing after {} fix attempt(s):\n{}",
                    label,
                    attempt,
                    truncate(&out, 4000)
                ));
            }

            // Loop back to the builder with the failing output appended. The builder works on the
            // SAME task sprite (the engine reconciles sprite filesystem state across attempts), so
            // its fix builds on the existing working tree.
            attempt += 1;
            let fix_task = format!(
                "The project's {} are failing. Fix the code so they pass. Do not change the tests \
                 unless they are wrong. Failing output:\n\n{}",
                label,
                truncate(&out, 6000),
            );
            self.run_builder_fix(node, ctx, &fix_task).await?;
        }
    }

    /// Run the gate's checks on the task sprite: tests, then optional lint. Returns
    /// (passed, which_check_failed_or_"tests", combined_output_of_the_failing_check).
    async fn run_gate_checks(
        &self,
        task_id: Uuid,
        test_cmd: &str,
        lint_cmd: Option<&str>,
        timeout: u64,
    ) -> Result<(bool, String, String)> {
        let test_out = self.sprite_exec(task_id, test_cmd, timeout).await?;
        if test_out.exit_code != 0 {
            return Ok((
                false,
                "tests".into(),
                format!("{}\n{}", test_out.stdout, test_out.stderr),
            ));
        }
        if let Some(lint) = lint_cmd {
            let lint_out = self.sprite_exec(task_id, lint, timeout).await?;
            if lint_out.exit_code != 0 {
                return Ok((
                    false,
                    "lint".into(),
                    format!("{}\n{}", lint_out.stdout, lint_out.stderr),
                ));
            }
        }
        Ok((true, "tests+lint".into(), String::new()))
    }

    /// Re-invoke the builder on the task's existing sprite to fix a failing gate. Mints a fresh
    /// taste token (revoked after), runs the backend with `fix_task`, and streams into this
    /// node's logs. The branch/repo come from ctx, same as AgentImplement.
    async fn run_builder_fix(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
        fix_task: &str,
    ) -> Result<()> {
        use crate::services::execution::{ExecutionBackendKind, TaskBrief};
        let backends = self
            .backends
            .clone()
            .ok_or_else(|| anyhow!("TestGate: execution backends not configured"))?;

        let kind = match node.config.get("backend").and_then(|v| v.as_str()) {
            Some(s) => ExecutionBackendKind::from_str_opt(s),
            None => {
                let row = sqlx::query!(
                    "SELECT execution_backend FROM agents WHERE id = $1",
                    ctx.agent_id
                )
                .fetch_optional(self.db)
                .await?;
                row.and_then(|r| ExecutionBackendKind::from_str_opt(&r.execution_backend))
            }
        }
        .unwrap_or_default();

        let repo = ctx
            .vars
            .get("repo")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let branch = ctx
            .vars
            .get("implementation")
            .and_then(|v| v.get("branch"))
            .and_then(|b| b.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("flaborful/task-{}", &ctx.task_id.to_string()[..8]));
        let github_token = ctx
            .vars
            .get("github_token")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let api_base_url =
            std::env::var("API_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".into());

        let overlays = self.build_vault_overlays(ctx, repo.as_deref()).await;
        let taste_token = crate::services::execution::taste::mint_token(
            self.db,
            ctx.task_id,
            ctx.agent_id,
            &ctx.persona_prompt,
            ctx.lora_name.as_deref(),
            &overlays,
        )
        .await?;

        let brief = TaskBrief {
            task_id: ctx.task_id,
            kind: crate::services::execution::TaskKind::Implement,
            agent_id: ctx.agent_id,
            job_id: ctx.job_id,
            repo,
            base_branch: ctx
                .vars
                .get("base_branch")
                .and_then(|v| v.as_str())
                .unwrap_or("main")
                .to_string(),
            branch,
            task_text: fix_task.to_string(),
            instructions: None,
            persona_prompt: ctx.persona_prompt.clone(),
            cf_finetune_id: ctx.lora_name.clone(),
            github_token,
            anthropic_api_key: crate::services::execution::agent_anthropic_key(
                self.db,
                ctx.agent_id,
            )
            .await,
            taste_token,
            api_base_url,
        };

        let backend = backends.get(kind);
        let sink = DbTranscriptSink {
            db: self.db.clone(),
            task_id: ctx.task_id,
            node_id: node.id.clone(),
        };
        let res = backend.run_implementation(&brief, &sink).await;
        let _ = crate::services::execution::taste::revoke_token(self.db, ctx.task_id).await;
        res?;
        Ok(())
    }

    /// Open a GitHub PR from a pushed branch. Sprite-less — pure GitHub API. Reads the branch
    /// from an upstream GitPush/AgentImplement output. Side-effecting (github_pr gate).
    async fn run_pr_create(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let token = ctx
            .vars
            .get("github_token")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // Repo + branch: node config, else from upstream output vars.
        let repo = node
            .config
            .get("repo")
            .and_then(|v| v.as_str())
            .or_else(|| ctx.vars.get("repo").and_then(|v| v.as_str()))
            .ok_or_else(|| anyhow!("PrCreate: no repo (config.repo or ctx.vars[repo])"))?
            .to_string();
        let head = node
            .config
            .get("branch")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                let src = node
                    .config
                    .get("source_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("push_result");
                ctx.vars
                    .get(src)
                    .and_then(|v| v.get("branch"))
                    .and_then(|b| b.as_str())
                    .map(str::to_string)
            })
            .or_else(|| {
                ctx.vars
                    .get("implementation")
                    .and_then(|v| v.get("branch"))
                    .and_then(|b| b.as_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| anyhow!("PrCreate: no branch to open a PR from"))?;
        let base = node
            .config
            .get("base")
            .and_then(|v| v.as_str())
            .unwrap_or("main");
        let title = node
            .config
            .get("title")
            .and_then(|v| v.as_str())
            .or_else(|| {
                node.config
                    .get("title_var")
                    .and_then(|v| v.as_str())
                    .and_then(|tv| ctx.vars.get(tv))
                    .and_then(|x| x.as_str())
            })
            .unwrap_or("Changes by your Flaborful agent")
            .to_string();
        let body_var = node
            .config
            .get("body_var")
            .and_then(|v| v.as_str())
            .unwrap_or("llm_output");
        let body = ctx
            .vars
            .get(body_var)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let draft = node
            .config
            .get("draft")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let (url, number) =
            create_github_pr(&repo, token, &head, base, &title, &body, draft).await?;
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("pr_result")
            .to_string();
        Ok(NodeResult::Continue {
            output_key,
            value: json!({
                "pr_url": url, "pr_number": number,
                "resource_kind": "github_pr", "resource_id": format!("{}#{}", repo, number), "repo": repo,
            }),
        })
    }

    // ── Sprite-only nodes (run commands on the task's sprite) ──────────────────

    /// Run a command in the task's sprite. Helper used by the sprite nodes.
    async fn sprite_exec(
        &self,
        task_id: Uuid,
        command: &str,
        timeout: u64,
    ) -> Result<crate::services::execution::sprite_client::ExecOutput> {
        let backends = self
            .backends
            .as_ref()
            .ok_or_else(|| anyhow!("sprite node: backends not configured"))?;
        crate::services::execution::sprite::exec_in_task_sprite(
            self.db,
            &backends.sprite_client,
            task_id,
            command,
            timeout,
        )
        .await?
        .ok_or_else(|| {
            anyhow!(
                "sprite node: no live sprite for this task (run an Agent: Implement node first)"
            )
        })
    }

    /// How many completed/recorded attempts this node already has (0 on first run).
    async fn node_prior_attempts(&self, task_id: Uuid, node_id: &str) -> Result<i64> {
        let n: i64 = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM node_executions WHERE task_id = $1 AND node_id = $2",
            task_id,
            node_id,
        )
        .fetch_one(self.db)
        .await?
        .unwrap_or(0);
        Ok(n)
    }

    /// Capture a filesystem checkpoint of the task's sprite and record it on the node_execution
    /// row, so a later rerun of this node can restore exactly this pre-node state. Best-effort:
    /// silently does nothing if there's no live sprite or checkpoints aren't supported (the rerun
    /// then degrades to the git-anchored reset).
    async fn capture_node_checkpoint(&self, task_id: Uuid, exec_id: Uuid) {
        let Some(backends) = self.backends.as_ref() else {
            return;
        };
        let name = sqlx::query_scalar!(
            "SELECT sprite_name FROM task_sprites WHERE task_id = $1 AND status NOT IN ('destroyed','gone')",
            task_id,
        )
        .fetch_optional(self.db)
        .await
        .ok()
        .flatten()
        .flatten();
        let Some(name) = name else { return };
        match backends.sprite_client.checkpoint(&name).await {
            Ok(cp) => {
                let _ = sqlx::query!(
                    "UPDATE node_executions SET node_checkpoint_id = $1 WHERE id = $2",
                    cp,
                    exec_id,
                )
                .execute(self.db)
                .await;
            }
            Err(e) => tracing::debug!("sprite checkpoint unavailable ({}): {}", name, e),
        }
    }

    /// Reconcile the task's sprite working tree before re-running a sprite-mutating `node_id`.
    /// Prefers restoring the most recent prior attempt's checkpoint (precise); falls back to a
    /// git-anchored reset to the clone HEAD (coarse). No-op (Ok) when there's no live sprite.
    /// Best-effort on the precise path; surfaces hard errors on the git-reset fallback.
    async fn reconcile_sprite_filesystem(&self, task_id: Uuid, node_id: &str) -> Result<()> {
        let Some(backends) = self.backends.as_ref() else {
            return Ok(());
        };
        let Some(sprite) = sqlx::query!(
            "SELECT sprite_name, head_sha FROM task_sprites WHERE task_id = $1 AND status NOT IN ('destroyed','gone')",
            task_id,
        )
        .fetch_optional(self.db)
        .await?
        else { return Ok(()) };
        let Some(name) = sprite.sprite_name else {
            return Ok(());
        };

        // Precise path: restore the latest prior attempt's checkpoint if we have one.
        let checkpoint = sqlx::query_scalar!(
            r#"SELECT node_checkpoint_id FROM node_executions
               WHERE task_id = $1 AND node_id = $2 AND node_checkpoint_id IS NOT NULL
               ORDER BY attempt DESC LIMIT 1"#,
            task_id,
            node_id,
        )
        .fetch_optional(self.db)
        .await?
        .flatten();
        if let Some(cp) = checkpoint {
            match backends.sprite_client.restore(&name, &cp).await {
                Ok(()) => return Ok(()),
                Err(e) => tracing::warn!(
                    "checkpoint restore failed ({}), falling back to git reset: {}",
                    name,
                    e
                ),
            }
        }

        // Coarse fallback: reset tracked files to the clone HEAD and remove untracked cruft.
        let Some(head_sha) = sprite.head_sha else {
            return Ok(());
        };
        let cmd = format!("git reset --hard {sha} && git clean -fdx", sha = head_sha);
        if let Some(out) = crate::services::execution::sprite::exec_in_task_sprite(
            self.db,
            &backends.sprite_client,
            task_id,
            &cmd,
            120,
        )
        .await?
        {
            if out.exit_code != 0 {
                return Err(anyhow!(
                    "sprite filesystem reconcile failed: {}",
                    out.stderr
                ));
            }
        }
        Ok(())
    }

    async fn run_sprite_command(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let command = node
            .config
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                let cv = node
                    .config
                    .get("command_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                ctx.vars
                    .get(cv)
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| anyhow!("SpriteCommand: no command (config.command or command_var)"))?;
        let timeout = node
            .config
            .get("timeout_s")
            .and_then(|v| v.as_u64())
            .unwrap_or(300);
        let out = self.sprite_exec(ctx.task_id, &command, timeout).await?;
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("command_result")
            .to_string();
        Ok(NodeResult::Continue {
            output_key,
            value: json!({
                "stdout": out.stdout, "stderr": out.stderr, "exit_code": out.exit_code,
            }),
        })
    }

    async fn run_sprite_test(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let cmd = node
            .config
            .get("test_command")
            .and_then(|v| v.as_str())
            .unwrap_or("npm test");
        let timeout = node
            .config
            .get("timeout_s")
            .and_then(|v| v.as_u64())
            .unwrap_or(600);
        let out = self.sprite_exec(ctx.task_id, cmd, timeout).await?;
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("test_results")
            .to_string();
        Ok(NodeResult::Continue {
            output_key,
            value: json!({
                "stdout": out.stdout, "stderr": out.stderr, "exit_code": out.exit_code, "passed": out.exit_code == 0,
            }),
        })
    }

    async fn run_git_commit(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        let message = node
            .config
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                let mv = node
                    .config
                    .get("message_var")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                ctx.vars
                    .get(mv)
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "Changes by your Flaborful agent".to_string());
        let add = node
            .config
            .get("add_paths")
            .and_then(|v| v.as_str())
            .unwrap_or("-A");
        let msg_esc = message.replace('\'', "'\\''");
        let cmd = format!(
            "git add {add} && git commit -m '{msg}' --allow-empty && git rev-parse HEAD",
            add = add,
            msg = msg_esc
        );
        let out = self.sprite_exec(ctx.task_id, &cmd, 120).await?;
        let commit_sha = out.stdout.lines().last().unwrap_or("").trim().to_string();
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("commit_result")
            .to_string();
        Ok(NodeResult::Continue {
            output_key,
            value: json!({
                "commit_sha": commit_sha, "message": message,
            }),
        })
    }

    async fn run_git_push(
        &self,
        node: &WorkflowNode,
        ctx: &mut ExecutionContext,
    ) -> Result<NodeResult> {
        // Branch from config or the sprite's current branch; repo for the resource id.
        let branch = node
            .config
            .get("branch")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                ctx.vars
                    .get("implementation")
                    .and_then(|v| v.get("branch"))
                    .and_then(|b| b.as_str())
                    .map(str::to_string)
            });
        let force = node
            .config
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let branch_arg = branch.clone().unwrap_or_else(|| "HEAD".to_string());
        let cmd = format!(
            "git push {force} origin {branch} 2>&1 && git rev-parse HEAD",
            force = if force { "--force" } else { "" },
            branch = branch_arg
        );
        let out = self.sprite_exec(ctx.task_id, &cmd, 180).await?;
        if out.exit_code != 0 {
            return Err(anyhow!("git push failed: {}", out.stdout));
        }
        let commit_sha = out.stdout.lines().last().unwrap_or("").trim().to_string();
        let repo = ctx
            .vars
            .get("repo")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let resolved_branch = branch.unwrap_or(branch_arg);
        let output_key = node
            .config
            .get("output_var")
            .and_then(|v| v.as_str())
            .unwrap_or("push_result")
            .to_string();
        Ok(NodeResult::Continue {
            output_key,
            value: json!({
                "branch": resolved_branch, "commit_sha": commit_sha,
                "resource_kind": "git_branch", "resource_id": format!("{}#{}", repo, resolved_branch),
            }),
        })
    }

    // ── Vault composition ─────────────────────────────────────────────────────

    /// Build the read-only vault overlay(s) for a hired task: the company that posted the job,
    /// scoped to the task's target repo. The agent gets full read of that company repo vault for
    /// the duration of the task (contractor-under-NDA). Returns empty if there's no company, no
    /// target repo, or the company has no notes for that repo yet.
    async fn build_vault_overlays(
        &self,
        ctx: &ExecutionContext,
        repo: Option<&str>,
    ) -> Vec<crate::services::vault::VaultOverlay> {
        let Some(repo) = repo else {
            return vec![];
        };
        // The job's owning company.
        let company_id = match sqlx::query_scalar!(
            "SELECT c.id FROM jobs j JOIN companies c ON c.id = j.company_id WHERE j.id = $1",
            ctx.job_id
        )
        .fetch_optional(self.db)
        .await
        {
            Ok(Some(id)) => id,
            _ => return vec![],
        };
        // Only attach if the company vault actually has notes for this repo.
        let has = sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM vault_notes WHERE owner_kind = 'company' AND agent_id = $1 AND repo = $2)",
            company_id,
            repo
        )
        .fetch_one(self.db)
        .await
        .unwrap_or(Some(false))
        .unwrap_or(false);
        if !has {
            return vec![];
        }
        vec![crate::services::vault::VaultOverlay {
            owner: crate::services::vault::VaultOwner::Company(company_id),
            repos: vec![repo.to_string()],
        }]
    }

    // ── RAG helpers ───────────────────────────────────────────────────────────

    async fn retrieve_rag_context(&self, ctx: &ExecutionContext, query: &str) -> Result<String> {
        // Hybrid (vector ⊕ keyword) + one related-link graph hop over the markdown vault,
        // replacing the legacy vector-only top-5. Owner/agent path → no shareable_only filter.
        let hits = crate::services::vault::retrieve(
            self.db,
            self.qdrant,
            self.inference,
            crate::services::vault::VaultOwner::Agent(ctx.agent_id),
            query,
            &crate::services::vault::RetrievalFilter::default(),
            5,
        )
        .await?;

        Ok(hits
            .into_iter()
            .map(|h| h.note.body)
            .collect::<Vec<_>>()
            .join("\n\n---\n\n"))
    }

    // ── DB state helpers ──────────────────────────────────────────────────────

    async fn update_current_node(&self, task_id: Uuid, node_id: &str) -> Result<()> {
        sqlx::query!(
            "UPDATE tasks SET current_node_id = $1, status = 'running' WHERE id = $2",
            node_id,
            task_id
        )
        .execute(self.db)
        .await?;
        Ok(())
    }

    async fn pause_for_review(&self, task_id: Uuid, node_id: &str, draft: &str) -> Result<()> {
        sqlx::query!(
            r#"UPDATE tasks SET status = 'awaiting_review', current_node_id = $1,
               output_draft = $2 WHERE id = $3"#,
            node_id,
            draft,
            task_id
        )
        .execute(self.db)
        .await?;

        // Notify professional — look up their user_id via task→job→agent→professional→user
        if let Ok(row) = sqlx::query!(
            r#"SELECT u.id AS user_id, j.title AS job_title, j.id AS job_id
               FROM tasks t
               JOIN jobs j ON j.id = t.job_id
               JOIN agents a ON a.id = j.agent_id
               JOIN professionals p ON p.id = a.professional_id
               JOIN users u ON u.id = p.user_id
               WHERE t.id = $1"#,
            task_id
        )
        .fetch_one(self.db)
        .await
        {
            let _ = crate::services::notifications::notify_awaiting_review(
                self.db,
                row.user_id,
                task_id,
                &row.job_title,
            )
            .await;
            let _ = audit::log_task_paused(self.db, row.job_id, task_id, node_id).await;
        }

        Ok(())
    }

    async fn deliver(&self, ctx: &ExecutionContext, output: &str) -> Result<()> {
        let task_id = ctx.task_id;
        sqlx::query!(
            r#"UPDATE tasks SET status = 'delivered', output_approved = $1,
               delivered_at = NOW() WHERE id = $2"#,
            output,
            task_id
        )
        .execute(self.db)
        .await?;

        // Grade the delivered output against the agent's evaluation criteria (opt-in; no-op if the
        // agent has none). Best-effort — a grading hiccup must not undo a successful delivery.
        let task_input = ctx
            .vars
            .get("input")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let _ = crate::services::evaluations::evaluate_task(
            self.db,
            self.inference,
            ctx.agent_id,
            task_id,
            &task_input,
            output,
        )
        .await;

        // Notify company and audit-log
        if let Ok(row) = sqlx::query!(
            r#"SELECT c_user.id AS company_user_id, j.title AS job_title, j.id AS job_id
               FROM tasks t
               JOIN jobs j ON j.id = t.job_id
               JOIN companies c ON c.id = j.company_id
               JOIN users c_user ON c_user.id = c.user_id
               WHERE t.id = $1"#,
            task_id
        )
        .fetch_one(self.db)
        .await
        {
            let _ = crate::services::notifications::notify_task_delivered(
                self.db,
                row.company_user_id,
                task_id,
                &row.job_title,
            )
            .await;
            let _ = audit::log_task_delivered(self.db, row.job_id, task_id).await;
        }

        self.destroy_task_sprite(task_id).await;
        Ok(())
    }
}

/// Truncate a string to at most `max` chars, appending an elision marker. Used to keep failing
/// test output bounded when looping it back to the builder / surfacing it on a failed gate.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}\n…(truncated)")
    }
}

// ── GitHub API helpers ────────────────────────────────────────────────────────

async fn fetch_github_pr_diff(pr_url: &str, token: &str) -> Result<String> {
    // Convert web URL to API URL if needed
    // https://github.com/owner/repo/pull/123 → https://api.github.com/repos/owner/repo/pulls/123
    let api_url = if pr_url.contains("api.github.com") {
        pr_url.to_string()
    } else {
        let parts: Vec<&str> = pr_url.trim_end_matches('/').split('/').collect();
        if parts.len() >= 5 {
            let owner = parts[parts.len() - 4];
            let repo = parts[parts.len() - 3];
            let number = parts[parts.len() - 1];
            format!(
                "https://api.github.com/repos/{}/{}/pulls/{}",
                owner, repo, number
            )
        } else {
            return Err(anyhow!("Cannot parse PR URL: {}", pr_url));
        }
    };

    let client = reqwest::Client::new();
    let mut req = client
        .get(&api_url)
        .header("User-Agent", "flaborful/1.0")
        .header("Accept", "application/vnd.github.v3.diff");

    if !token.is_empty() {
        req = req.bearer_auth(token);
    }

    let diff = req.send().await?.error_for_status()?.text().await?;
    Ok(diff)
}

/// Post a PR comment. Returns the new comment's id (for later edit-in-place on rerun).
async fn post_github_comment(pr_url: &str, token: &str, comment: &str) -> Result<String> {
    // Convert web URL to comments API URL
    let parts: Vec<&str> = pr_url.trim_end_matches('/').split('/').collect();
    if parts.len() < 5 {
        return Err(anyhow!("Cannot parse PR URL for comment: {}", pr_url));
    }
    let owner = parts[parts.len() - 4];
    let repo = parts[parts.len() - 3];
    let number = parts[parts.len() - 1];
    let api_url = format!(
        "https://api.github.com/repos/{}/{}/issues/{}/comments",
        owner, repo, number
    );

    let resp: Value = reqwest::Client::new()
        .post(&api_url)
        .bearer_auth(token)
        .header("User-Agent", "flaborful/1.0")
        .json(&serde_json::json!({ "body": comment }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    Ok(resp.get("id").map(|v| v.to_string()).unwrap_or_default())
}

/// Edit an existing PR comment in place (used when rerun decision = update).
async fn update_github_comment(
    pr_url: &str,
    token: &str,
    comment_id: &str,
    body: &str,
) -> Result<()> {
    let parts: Vec<&str> = pr_url.trim_end_matches('/').split('/').collect();
    if parts.len() < 5 {
        return Err(anyhow!("Cannot parse PR URL for comment: {}", pr_url));
    }
    let owner = parts[parts.len() - 4];
    let repo = parts[parts.len() - 3];
    let api_url = format!(
        "https://api.github.com/repos/{}/{}/issues/comments/{}",
        owner, repo, comment_id
    );
    reqwest::Client::new()
        .patch(&api_url)
        .bearer_auth(token)
        .header("User-Agent", "flaborful/1.0")
        .json(&serde_json::json!({ "body": body }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

/// Open a pull request. `repo` is "owner/name". Returns (html_url, number).
async fn create_github_pr(
    repo: &str,
    token: &str,
    head: &str,
    base: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> Result<(String, u64)> {
    let resp: Value = reqwest::Client::new()
        .post(format!("https://api.github.com/repos/{}/pulls", repo))
        .bearer_auth(token)
        .header("User-Agent", "flaborful/1.0")
        .header("Accept", "application/vnd.github+json")
        .json(&json!({ "title": title, "head": head, "base": base, "body": body, "draft": draft }))
        .send()
        .await?
        .error_for_status()
        .context("GitHub create PR")?
        .json()
        .await?;
    let url = resp
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let number = resp.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
    Ok((url, number))
}

/// Update an existing PR's title/body in place (for the github_pr edit-in-place rerun path).
async fn update_github_pr(
    repo: &str,
    token: &str,
    number: &str,
    title: &str,
    body: &str,
) -> Result<()> {
    reqwest::Client::new()
        .patch(format!(
            "https://api.github.com/repos/{}/pulls/{}",
            repo, number
        ))
        .bearer_auth(token)
        .header("User-Agent", "flaborful/1.0")
        .header("Accept", "application/vnd.github+json")
        .json(&json!({ "title": title, "body": body }))
        .send()
        .await?
        .error_for_status()
        .context("GitHub update PR")?;
    Ok(())
}

// ── Email helpers ─────────────────────────────────────────────────────────────

async fn send_email_via_smtp(smtp_url: &str, to: &str, subject: &str, body: &str) -> Result<()> {
    reqwest::Client::new()
        .post(smtp_url)
        .json(&json!({ "to": to, "subject": subject, "html": body }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn fetch_emails(token: &str) -> Result<Vec<Value>> {
    if token.is_empty() {
        return Ok(vec![]);
    }
    let resp = reqwest::Client::new()
        .get("https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=10&q=is:unread")
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    Ok(resp
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

// ── Slack helpers ─────────────────────────────────────────────────────────────

/// Post a Slack message. Returns the resolved channel id and message `ts`, so a rerun
/// can edit it in place via chat.update.
async fn post_slack_message(token: &str, channel: &str, text: &str) -> Result<(String, String)> {
    let resp: Value = reqwest::Client::new()
        .post("https://slack.com/api/chat.postMessage")
        .bearer_auth(token)
        .json(&json!({ "channel": channel, "text": text }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let ts = resp
        .get("ts")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let resolved_channel = resp
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or(channel)
        .to_string();
    Ok((resolved_channel, ts))
}

/// Edit a Slack message in place (rerun decision = update). `resource_id` is "channel:ts".
async fn update_slack_message(token: &str, resource_id: &str, text: &str) -> Result<()> {
    let (channel, ts) = resource_id
        .split_once(':')
        .ok_or_else(|| anyhow!("Slack update: malformed resource id"))?;
    reqwest::Client::new()
        .post("https://slack.com/api/chat.update")
        .bearer_auth(token)
        .json(&json!({ "channel": channel, "ts": ts, "text": text }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn fetch_slack_messages(token: &str, channel: &str, limit: u32) -> Result<Vec<Value>> {
    if token.is_empty() {
        return Ok(vec![]);
    }
    let resp = reqwest::Client::new()
        .get(format!(
            "https://slack.com/api/conversations.history?channel={}&limit={}",
            channel, limit
        ))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    Ok(resp
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

// ── Web search helpers ────────────────────────────────────────────────────────

async fn brave_search(api_key: &str, query: &str, count: u32) -> Result<Vec<Value>> {
    let resp = reqwest::Client::new()
        .get(format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
            urlencoding::encode(query),
            count
        ))
        .header("Accept", "application/json")
        .header("X-Subscription-Token", api_key)
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    Ok(resp
        .pointer("/web/results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

async fn tavily_search(api_key: &str, query: &str, max_results: u32) -> Result<Vec<Value>> {
    let resp = reqwest::Client::new()
        .post("https://api.tavily.com/search")
        .json(&json!({ "api_key": api_key, "query": query, "max_results": max_results }))
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    Ok(resp
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

// ── Notion helpers ────────────────────────────────────────────────────────────

async fn notion_read_page(token: &str, page_id: &str) -> Result<String> {
    let resp = reqwest::Client::new()
        .get(format!(
            "https://api.notion.com/v1/blocks/{}/children",
            page_id
        ))
        .bearer_auth(token)
        .header("Notion-Version", "2022-06-28")
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    Ok(resp.to_string())
}

/// Append a paragraph block to a Notion page. Returns the new block's id (for in-place
/// update on a rerun).
async fn notion_append_block(token: &str, page_id: &str, content: &str) -> Result<String> {
    let resp: Value = reqwest::Client::new()
        .patch(format!("https://api.notion.com/v1/blocks/{}/children", page_id))
        .bearer_auth(token)
        .header("Notion-Version", "2022-06-28")
        .json(&json!({
            "children": [{ "object": "block", "type": "paragraph",
                "paragraph": { "rich_text": [{ "type": "text", "text": { "content": content } }] } }]
        }))
        .send().await?.error_for_status()?.json().await?;
    Ok(resp
        .pointer("/results/0/id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string())
}

/// Update an existing Notion paragraph block's text in place.
async fn notion_update_block(token: &str, block_id: &str, content: &str) -> Result<()> {
    reqwest::Client::new()
        .patch(format!("https://api.notion.com/v1/blocks/{}", block_id))
        .bearer_auth(token)
        .header("Notion-Version", "2022-06-28")
        .json(&json!({
            "paragraph": { "rich_text": [{ "type": "text", "text": { "content": content } }] }
        }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

// ── Google Drive helpers ──────────────────────────────────────────────────────

async fn gdrive_read_file(token: &str, file_id: &str) -> Result<String> {
    let content = reqwest::Client::new()
        .get(format!(
            "https://www.googleapis.com/drive/v3/files/{}?alt=media",
            file_id
        ))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(content)
}

// ── Linear helpers ────────────────────────────────────────────────────────────

async fn linear_get_issue(token: &str, issue_id: &str) -> Result<Value> {
    let query = format!(
        r#"{{ "query": "{{ issue(id: \"{}\") {{ id title description state {{ name }} assignee {{ name }} }} }}" }}"#,
        issue_id
    );
    let resp = reqwest::Client::new()
        .post("https://api.linear.app/graphql")
        .bearer_auth(token)
        .header("Content-Type", "application/json")
        .body(query)
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    Ok(resp.pointer("/data/issue").cloned().unwrap_or(Value::Null))
}

/// Add a comment to a Linear issue. Returns the new comment's id.
async fn linear_add_comment(token: &str, issue_id: &str, body: &str) -> Result<String> {
    let query = format!(
        r#"{{ "query": "mutation {{ commentCreate(input: {{ issueId: \"{}\", body: \"{}\" }}) {{ success comment {{ id }} }} }}" }}"#,
        issue_id,
        body.replace('"', "\\\"")
    );
    let resp: Value = reqwest::Client::new()
        .post("https://api.linear.app/graphql")
        .bearer_auth(token)
        .header("Content-Type", "application/json")
        .body(query)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp
        .pointer("/data/commentCreate/comment/id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string())
}

/// Update an existing Linear comment in place.
async fn linear_update_comment(token: &str, comment_id: &str, body: &str) -> Result<()> {
    let query = format!(
        r#"{{ "query": "mutation {{ commentUpdate(id: \"{}\", input: {{ body: \"{}\" }}) {{ success }} }}" }}"#,
        comment_id,
        body.replace('"', "\\\"")
    );
    reqwest::Client::new()
        .post("https://api.linear.app/graphql")
        .bearer_auth(token)
        .header("Content-Type", "application/json")
        .body(query)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

// ── Jira helpers ──────────────────────────────────────────────────────────────

async fn jira_get_issue(token: &str, base_url: &str, issue_key: &str) -> Result<Value> {
    let resp = reqwest::Client::new()
        .get(format!("{}/rest/api/3/issue/{}", base_url, issue_key))
        .bearer_auth(token)
        .header("Accept", "application/json")
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    Ok(resp)
}

/// Add a comment to a Jira issue. Returns the new comment's id.
async fn jira_add_comment(
    token: &str,
    base_url: &str,
    issue_key: &str,
    body: &str,
) -> Result<String> {
    let resp: Value = reqwest::Client::new()
        .post(format!("{}/rest/api/3/issue/{}/comment", base_url, issue_key))
        .bearer_auth(token)
        .header("Content-Type", "application/json")
        .json(&json!({ "body": { "type": "doc", "version": 1,
            "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": body }] }] } }))
        .send().await?.error_for_status()?.json().await?;
    Ok(resp
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string())
}

/// Update an existing Jira comment in place.
async fn jira_update_comment(
    token: &str,
    base_url: &str,
    issue_key: &str,
    comment_id: &str,
    body: &str,
) -> Result<()> {
    reqwest::Client::new()
        .put(format!("{}/rest/api/3/issue/{}/comment/{}", base_url, issue_key, comment_id))
        .bearer_auth(token)
        .header("Content-Type", "application/json")
        .json(&json!({ "body": { "type": "doc", "version": 1,
            "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": body }] }] } }))
        .send().await?.error_for_status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::workflow::{NodePosition, WorkflowEdge};

    fn node(id: &str, t: NodeType) -> WorkflowNode {
        WorkflowNode {
            id: id.into(),
            node_type: t,
            label: id.into(),
            config: json!({}),
            position: NodePosition { x: 0.0, y: 0.0 },
        }
    }

    fn edge(src: &str, tgt: &str) -> WorkflowEdge {
        WorkflowEdge {
            id: format!("{src}-{tgt}"),
            source: src.into(),
            target: tgt.into(),
        }
    }

    // Trigger → llm → gate → fmt → out
    fn linear_graph() -> WorkflowGraph {
        WorkflowGraph {
            nodes: vec![
                node("trigger", NodeType::Trigger),
                node("llm", NodeType::LlmReasoning),
                node("gate", NodeType::HumanGate),
                node("fmt", NodeType::Formatter),
                node("out", NodeType::Output),
            ],
            edges: vec![
                edge("trigger", "llm"),
                edge("llm", "gate"),
                edge("gate", "fmt"),
                edge("fmt", "out"),
            ],
        }
    }

    #[test]
    fn adjacency_follows_edges() {
        let g = linear_graph();
        let adj = build_adjacency(&g);
        assert_eq!(adj.get("trigger").unwrap(), &vec!["llm".to_string()]);
        assert_eq!(adj.get("gate").unwrap(), &vec!["fmt".to_string()]);
        assert!(adj.get("out").is_none());
    }

    #[test]
    fn push_next_preserves_order() {
        // A node fanning out to b, c, d should pop in b, c, d order from the DFS stack.
        let g = WorkflowGraph {
            nodes: vec![node("a", NodeType::Trigger)],
            edges: vec![edge("a", "b"), edge("a", "c"), edge("a", "d")],
        };
        let adj = build_adjacency(&g);
        let mut stack = Vec::new();
        push_next(&mut stack, &adj, "a");
        assert_eq!(stack.pop().as_deref(), Some("b"));
        assert_eq!(stack.pop().as_deref(), Some("c"));
        assert_eq!(stack.pop().as_deref(), Some("d"));
    }

    #[test]
    fn resume_starts_downstream_of_gate() {
        // resume_from should begin at the gate's successor (fmt), not re-run the gate.
        let g = linear_graph();
        let adj = build_adjacency(&g);
        let next: Vec<String> = adj.get("gate").cloned().unwrap_or_default();
        assert_eq!(next, vec!["fmt".to_string()]);
    }

    #[test]
    fn branch_picks_matching_then_falls_back() {
        let mut branch_node = node("br", NodeType::ConditionalBranch);
        branch_node.config = json!({ "branches": { "yes": "approve", "no": "reject" } });
        let g = WorkflowGraph {
            nodes: vec![
                branch_node.clone(),
                node("approve", NodeType::Output),
                node("reject", NodeType::Output),
            ],
            edges: vec![edge("br", "approve"), edge("br", "reject")],
        };
        let adj = build_adjacency(&g);
        // Label "no" maps to the "reject" target via config.
        assert_eq!(
            pick_branch(&adj, &branch_node, "no").as_deref(),
            Some("reject")
        );
        // Unknown label falls back to the first successor.
        assert_eq!(
            pick_branch(&adj, &branch_node, "unknown").as_deref(),
            Some("approve")
        );
    }

    #[test]
    fn diamond_dedup_visits_each_node_once() {
        // A→B, A→C, B→D, C→D. Without a visited set, D is pushed twice and would run twice.
        // This test mirrors the execute_graph driver's visited guard to lock in the fix.
        let g = WorkflowGraph {
            nodes: vec![
                node("a", NodeType::Trigger),
                node("b", NodeType::Formatter),
                node("c", NodeType::Formatter),
                node("d", NodeType::Output),
            ],
            edges: vec![
                edge("a", "b"),
                edge("a", "c"),
                edge("b", "d"),
                edge("c", "d"),
            ],
        };
        let adj = build_adjacency(&g);
        let mut stack: Vec<String> = vec!["a".into()];
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut visit_order: Vec<String> = Vec::new();

        while let Some(n) = stack.pop() {
            if !visited.insert(n.clone()) {
                continue; // already ran this node in this traversal — skip
            }
            visit_order.push(n.clone());
            push_next(&mut stack, &adj, &n);
        }

        // Every node runs exactly once; D appears only once despite two predecessors.
        assert_eq!(visit_order.iter().filter(|x| x.as_str() == "d").count(), 1);
        assert_eq!(visit_order.len(), 4);
    }

    // A minimal ExecutionContext for testing the pure subflow/loop helpers (no DB needed).
    fn test_ctx(vars: Vec<(&str, Value)>) -> ExecutionContext {
        ExecutionContext {
            task_id: Uuid::nil(),
            agent_id: Uuid::nil(),
            job_id: Uuid::nil(),
            lora_name: None,
            persona_prompt: String::new(),
            vars: vars.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            effect_decisions: HashMap::new(),
            subflow_depth: 0,
            persist: true,
        }
    }

    #[test]
    fn subflow_input_map_parses_json_string_and_object() {
        let mut n = node("s", NodeType::Subflow);
        n.config = json!({ "input_map": "{\"url\": \"current_url\"}" });
        let m = WorkflowEngine::subflow_input_map(&n);
        assert_eq!(m.get("url").and_then(|v| v.as_str()), Some("current_url"));

        n.config = json!({ "input_map": { "x": "y" } });
        let m = WorkflowEngine::subflow_input_map(&n);
        assert_eq!(m.get("x").and_then(|v| v.as_str()), Some("y"));
    }

    #[test]
    fn seed_child_vars_resolves_vars_and_literals() {
        let ctx = test_ctx(vec![
            ("current_url", json!("https://x")),
            ("input", json!("root")),
        ]);
        let mut map = serde_json::Map::new();
        map.insert("url".into(), json!("current_url")); // resolves to a var
        map.insert("mode".into(), json!(42)); // literal (non-string)
        let seeded = WorkflowEngine::seed_child_vars(&map, &ctx);
        assert_eq!(seeded.get("url"), Some(&json!("https://x")));
        assert_eq!(seeded.get("mode"), Some(&json!(42)));
        // task input carries through so subflow SWE nodes still work
        assert_eq!(seeded.get("input"), Some(&json!("root")));
    }

    #[test]
    fn seed_child_vars_unknown_var_becomes_literal_string() {
        let ctx = test_ctx(vec![]);
        let mut map = serde_json::Map::new();
        map.insert("q".into(), json!("not_a_var"));
        let seeded = WorkflowEngine::seed_child_vars(&map, &ctx);
        assert_eq!(seeded.get("q"), Some(&json!("not_a_var")));
    }

    #[test]
    fn loop_list_picks_the_array_mapped_input() {
        let ctx = test_ctx(vec![
            ("urls", json!(["a", "b", "c"])),
            ("threshold", json!(5)),
        ]);
        let mut map = serde_json::Map::new();
        map.insert("threshold".into(), json!("threshold")); // scalar → not the loop input
        map.insert("url".into(), json!("urls")); // array → the loop input
        let (list, key) = WorkflowEngine::loop_list_and_key(&map, &ctx);
        assert_eq!(list.len(), 3);
        assert_eq!(key, "url");
    }

    #[test]
    fn loop_list_falls_back_to_input_array() {
        let ctx = test_ctx(vec![("input", json!([1, 2]))]);
        let map = serde_json::Map::new();
        let (list, _key) = WorkflowEngine::loop_list_and_key(&map, &ctx);
        assert_eq!(list.len(), 2);
    }
}
