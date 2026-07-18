//! **Reflections** — the agent's scheduled self-review. Unlike the reactive per-task evaluation,
//! a reflection runs periodically (a background scheduler), mines the agent's recent activity for
//! patterns, and proposes concrete improvements into a **human-gated review queue**
//! (`reflection_proposals`). Nothing self-applies without owner approval (Gumloop's default
//! Review-Queue model). See [[Flaborful Reflections & Evaluations]].
//!
//! Proposal kinds: `new_skill` (a repeated procedure worth a [[Flaborful Skills|Skill]]),
//! `instruction_update` (a persona-prompt refinement), `skill_fix`, `tool_access`. Applying a
//! proposal is a separate, explicit step (see `routes/reflections.rs`), which writes the Skill or
//! updates the persona prompt.

use anyhow::Result;
use chrono::{DateTime, Utc};
use qdrant_client::Qdrant;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::services::inference::InferenceClient;
use crate::services::notifications;

/// How much recent activity a reflection considers, and the minimum to bother running.
const LOOKBACK_TASKS: i64 = 25;
const MIN_ACTIVITY: usize = 3;

/// Run a reflection for one agent: gather recent signals, ask the model for improvement proposals,
/// store the pending ones, and (if any) notify the owner. Returns the number of proposals created.
/// Best-effort and idempotent-ish: it stamps `agents.last_reflection_at` so the scheduler only
/// re-runs agents with genuinely new activity.
pub async fn run_for_agent(
    db: &PgPool,
    inference: &InferenceClient,
    agent_id: Uuid,
) -> Result<usize> {
    let signals = gather_signals(db, agent_id).await?;
    // Always stamp the run so the scheduler advances even when there's nothing to propose.
    let _ = sqlx::query("UPDATE agents SET last_reflection_at = NOW() WHERE id = $1")
        .bind(agent_id)
        .execute(db)
        .await;

    if signals.activity_count < MIN_ACTIVITY {
        return Ok(0);
    }

    let existing = existing_titles(db, agent_id).await?;
    let proposals = propose(inference, &signals, &existing).await?;
    if proposals.is_empty() {
        return Ok(0);
    }

    for p in &proposals {
        let _ = sqlx::query(
            "INSERT INTO reflection_proposals (agent_id, kind, title, rationale, payload) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(agent_id)
        .bind(&p.kind)
        .bind(&p.title)
        .bind(&p.rationale)
        .bind(&p.payload)
        .execute(db)
        .await;
    }

    let _ = notifications::notify_reflection_ready(db, agent_id, proposals.len()).await;
    Ok(proposals.len())
}

struct Signals {
    activity_count: usize,
    /// A compact digest of recent tasks + their eval grades + owner corrections, for the prompt.
    digest: String,
}

/// Pull the recent activity a reflection reasons over: delivered tasks with their eval grades and
/// summaries, plus recent chat-correction notes (owner steering). Kept compact.
async fn gather_signals(db: &PgPool, agent_id: Uuid) -> Result<Signals> {
    // Recent tasks for this agent's jobs, with any evaluation grade/summary.
    let rows = sqlx::query_as::<_, TaskSignal>(
        r#"SELECT t.id AS task_id, t.status::text AS status,
                  e.grade AS eval_grade, e.summary AS eval_summary
           FROM tasks t
           JOIN jobs j ON j.id = t.job_id
           LEFT JOIN task_evaluations e ON e.task_id = t.id
           WHERE j.agent_id = $1
           ORDER BY t.created_at DESC
           LIMIT $2"#,
    )
    .bind(agent_id)
    .bind(LOOKBACK_TASKS)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    // Recent owner corrections captured as ChatCorrection vault notes (VaultOwner::Agent).
    let corrections = sqlx::query_scalar::<_, String>(
        r#"SELECT body FROM vault_notes
           WHERE agent_id = $1 AND owner_kind = 'agent' AND source_type = 'chat_correction'
           ORDER BY updated_at DESC LIMIT 10"#,
    )
    .bind(agent_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let mut lines = Vec::new();
    for r in &rows {
        let grade = r.eval_grade.as_deref().unwrap_or("ungraded");
        let summary = r.eval_summary.as_deref().unwrap_or("");
        let status = r.status.as_deref().unwrap_or("?");
        lines.push(format!("- task {} [{status}] grade={grade} {summary}", r.task_id));
    }
    if !corrections.is_empty() {
        lines.push("\nRecent owner corrections:".to_string());
        for c in &corrections {
            lines.push(format!("- {}", truncate(c, 300)));
        }
    }

    Ok(Signals {
        activity_count: rows.len() + corrections.len(),
        digest: lines.join("\n"),
    })
}

#[derive(sqlx::FromRow)]
struct TaskSignal {
    task_id: Uuid,
    status: Option<String>,
    eval_grade: Option<String>,
    eval_summary: Option<String>,
}

async fn existing_titles(db: &PgPool, agent_id: Uuid) -> Result<Vec<String>> {
    // Avoid re-proposing something already pending or already an existing skill.
    let mut titles = sqlx::query_scalar::<_, String>(
        "SELECT title FROM reflection_proposals WHERE agent_id = $1 AND status = 'pending'",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    let skills = sqlx::query_scalar::<_, String>(
        "SELECT title FROM vault_notes WHERE agent_id = $1 AND owner_kind = 'agent' AND layer = 'skill'",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    titles.extend(skills);
    Ok(titles)
}

struct Proposal {
    kind: String,
    title: String,
    rationale: String,
    payload: Value,
}

/// Ask the model for up to a few improvement proposals from the activity digest. Conservative: it
/// returns an empty list when there's no clear, repeated pattern.
async fn propose(
    inference: &InferenceClient,
    signals: &Signals,
    existing: &[String],
) -> Result<Vec<Proposal>> {
    let avoid = if existing.is_empty() {
        String::new()
    } else {
        format!("\n\nDo NOT propose anything overlapping these existing items:\n- {}", existing.join("\n- "))
    };
    let system = format!(
        "You review an AI professional-agent's recent work and propose at most 3 HIGH-CONFIDENCE \
         improvements, only for clear REPEATED patterns (not one-offs). Each proposal is one of:\n\
         - new_skill: a repeatable procedure worth saving as a step-by-step playbook. payload: \
           {{ slug, title, when_to_use, body }} where body is numbered steps.\n\
         - instruction_update: a refinement to the agent's standing system prompt. payload: \
           {{ addition }} — a concise instruction to ADD.\n\
         - skill_fix: an edge case an existing skill should handle. payload: {{ note }}.\n\
         - tool_access: a missing integration the agent kept working around. payload: {{ note }}.\n\
         Return JSON: {{ \"proposals\": [{{ \"kind\": string, \"title\": string, \"rationale\": \
         string, \"payload\": object }}] }}. Return an empty array if nothing is clearly worth it.{avoid}"
    );
    let user = format!("Recent activity:\n{}", signals.digest);
    let raw = inference.orchestrate(&system, &user, true).await?;
    let parsed: Value = serde_json::from_str(crate::services::inference::strip_fences(&raw)).unwrap_or(json!({}));

    let arr = parsed.get("proposals").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut out = Vec::new();
    for p in arr.into_iter().take(3) {
        let kind = p.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if !matches!(kind.as_str(), "new_skill" | "instruction_update" | "skill_fix" | "tool_access") {
            continue;
        }
        let title = p.get("title").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if title.is_empty() {
            continue;
        }
        out.push(Proposal {
            kind,
            title,
            rationale: p.get("rationale").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            payload: p.get("payload").cloned().unwrap_or(json!({})),
        });
    }
    Ok(out)
}

/// The scheduler entry point: periodically reflect for agents with new activity since their last
/// reflection. Runs forever; spawn it once at startup. Conservative cadence (hourly) and only
/// touches agents that have delivered/updated tasks since `last_reflection_at`.
pub async fn run_scheduler(db: PgPool, inference: InferenceClient) {
    // Configurable via env for tests/dev; defaults to hourly.
    let interval_secs: u64 = std::env::var("REFLECTION_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600);
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(60)));
    // Skip the immediate first tick so we don't reflect the instant the server boots.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        if let Err(e) = reflect_due_agents(&db, &inference).await {
            tracing::warn!("reflection scheduler pass failed: {e}");
        }
    }
}

/// Find agents with task activity newer than their last reflection and reflect on each.
async fn reflect_due_agents(db: &PgPool, inference: &InferenceClient) -> Result<()> {
    let due: Vec<Uuid> = sqlx::query_scalar::<_, Uuid>(
        r#"SELECT DISTINCT a.id
           FROM agents a
           JOIN jobs j ON j.agent_id = a.id
           JOIN tasks t ON t.job_id = j.id
           WHERE a.status IN ('ready', 'deployed')
             AND (a.last_reflection_at IS NULL OR t.created_at > a.last_reflection_at)"#,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    for agent_id in due {
        if let Err(e) = run_for_agent(db, inference, agent_id).await {
            tracing::warn!("reflection for agent {agent_id} failed: {e}");
        }
    }
    Ok(())
}

/// Apply an approved proposal: performs the underlying change (write a Skill / append to the persona
/// prompt) and marks the proposal applied. Returns a short human-readable outcome. `skill_fix` and
/// `tool_access` are advisory (no automatic change) — they're marked applied as acknowledged.
pub async fn apply_proposal(
    db: &PgPool,
    qdrant: &Arc<Qdrant>,
    inference: &InferenceClient,
    agent_id: Uuid,
    proposal_id: Uuid,
) -> Result<String> {
    let row = sqlx::query_as::<_, ProposalRow>(
        "SELECT kind, title, payload FROM reflection_proposals WHERE id = $1 AND agent_id = $2 AND status = 'pending'",
    )
    .bind(proposal_id)
    .bind(agent_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| anyhow::anyhow!("proposal not found or already resolved"))?;

    let outcome = match row.kind.as_str() {
        "new_skill" => {
            let owner = crate::services::vault::VaultOwner::Agent(agent_id);
            let args = row.payload.clone();
            crate::services::vault::handle_tool(db, qdrant, inference, owner, &[], "write_skill", &args)
                .await?
        }
        "instruction_update" => {
            let addition = row
                .payload
                .get("addition")
                .and_then(|v| v.as_str())
                .unwrap_or(&row.title);
            sqlx::query(
                "UPDATE agents SET persona_system_prompt = COALESCE(persona_system_prompt, '') || E'\\n\\n' || $1, updated_at = NOW() WHERE id = $2",
            )
            .bind(addition)
            .bind(agent_id)
            .execute(db)
            .await?;
            "Added the instruction to your agent's system prompt.".to_string()
        }
        _ => "Acknowledged.".to_string(),
    };

    sqlx::query("UPDATE reflection_proposals SET status = 'applied', resolved_at = NOW() WHERE id = $1")
        .bind(proposal_id)
        .execute(db)
        .await?;
    Ok(outcome)
}

pub async fn dismiss_proposal(db: &PgPool, agent_id: Uuid, proposal_id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE reflection_proposals SET status = 'dismissed', resolved_at = NOW() WHERE id = $1 AND agent_id = $2",
    )
    .bind(proposal_id)
    .bind(agent_id)
    .execute(db)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct ProposalRow {
    kind: String,
    #[allow(dead_code)]
    title: String,
    payload: Value,
}

/// A pending/resolved proposal for the review-queue UI.
#[derive(serde::Serialize, sqlx::FromRow)]
pub struct ProposalView {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub rationale: String,
    pub payload: Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

pub async fn list_proposals(db: &PgPool, agent_id: Uuid) -> Result<Vec<ProposalView>> {
    Ok(sqlx::query_as::<_, ProposalView>(
        r#"SELECT id, kind, title, rationale, payload, status, created_at
           FROM reflection_proposals
           WHERE agent_id = $1 AND status = 'pending'
           ORDER BY created_at DESC"#,
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?)
}

fn truncate(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    let mut idx = max_chars;
    while !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}
