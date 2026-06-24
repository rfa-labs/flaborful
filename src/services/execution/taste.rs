//! The shared "taste tool" — implemented ONCE, used by every backend and every agent runtime.
//!
//! The coding agent (Claude Code, Codex, the Managed Agents loop, …) can call
//! `consult_principal(question)` to ask the professional's fine-tuned model how their principal
//! would implement or review something. Modern coding agents all speak MCP, so the Sprite
//! runtimes register this one endpoint via a small per-runtime config file; Managed Agents
//! routes its native custom-tool call to the same handler. Either way the CF Workers AI token
//! never leaves the backend — the agent only makes an authenticated call here.
//!
//! Auth is a short-lived per-task token (`taste_tokens` table) that resolves to the agent's
//! persona + cf_finetune_id, so the endpoint is stateless and the agent never holds CF creds.

use anyhow::{anyhow, Result};
use sqlx::PgPool;
use uuid::Uuid;

use crate::services::inference::InferenceClient;

/// Mint a per-task taste token bound to the agent's persona + LoRA. Returns the token string.
pub async fn mint_token(
    db: &PgPool,
    task_id: Uuid,
    agent_id: Uuid,
    persona_prompt: &str,
    cf_finetune_id: Option<&str>,
) -> Result<String> {
    let token = format!("tt_{}", Uuid::new_v4().simple());
    sqlx::query!(
        r#"INSERT INTO taste_tokens (token, task_id, agent_id, persona_prompt, cf_finetune_id)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (task_id) DO UPDATE SET
             token = EXCLUDED.token, persona_prompt = EXCLUDED.persona_prompt,
             cf_finetune_id = EXCLUDED.cf_finetune_id, created_at = NOW()"#,
        token,
        task_id,
        agent_id,
        persona_prompt,
        cf_finetune_id,
    )
    .execute(db)
    .await?;
    Ok(token)
}

/// Mint a token bound only to an agent (no task) — for code-ingestion runs that document repos
/// into the vault. The agent presents it to the vault MCP tools; it resolves via the same
/// `taste_tokens` binding. Returns the token string. Caller revokes with `revoke_by_token`.
pub async fn mint_agent_token(
    db: &PgPool,
    agent_id: Uuid,
    persona_prompt: &str,
    cf_finetune_id: Option<&str>,
) -> Result<String> {
    let token = format!("tt_{}", Uuid::new_v4().simple());
    sqlx::query!(
        r#"INSERT INTO taste_tokens (token, task_id, agent_id, persona_prompt, cf_finetune_id)
           VALUES ($1, NULL, $2, $3, $4)"#,
        token,
        agent_id,
        persona_prompt,
        cf_finetune_id,
    )
    .execute(db)
    .await?;
    Ok(token)
}

/// Revoke a token by its value (used by ingestion runs, which have no task_id to key on).
pub async fn revoke_by_token(db: &PgPool, token: &str) -> Result<()> {
    sqlx::query!("DELETE FROM taste_tokens WHERE token = $1", token)
        .execute(db)
        .await?;
    Ok(())
}

/// Resolve a per-task token to the agent it's bound to. Used by the vault MCP tool, which
/// scopes every read/write to this agent's vault (the token IS the agent's authority — the
/// agent never holds a Flaborful JWT). Reuses the `taste_tokens` binding so one token covers
/// both `consult_principal` and the vault tools.
pub async fn resolve_agent(db: &PgPool, token: &str) -> Result<Uuid> {
    sqlx::query_scalar!("SELECT agent_id FROM taste_tokens WHERE token = $1", token)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| anyhow!("invalid or expired token"))
}

/// Revoke a task's taste token (call when the task terminates).
pub async fn revoke_token(db: &PgPool, task_id: Uuid) -> Result<()> {
    sqlx::query!("DELETE FROM taste_tokens WHERE task_id = $1", task_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Resolve a token and run the LoRA to answer the agent's question. This is the ONLY place
/// with taste-tool logic. Both the MCP-for-sprites route and the Managed Agents custom-tool
/// handler call this.
pub async fn consult(
    db: &PgPool,
    inference: &InferenceClient,
    token: &str,
    question: &str,
) -> Result<String> {
    let row = sqlx::query!(
        "SELECT persona_prompt, cf_finetune_id FROM taste_tokens WHERE token = $1",
        token
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| anyhow!("invalid or expired taste token"))?;

    let system = format!(
        "{}\n\nYou are being consulted by a coding agent working on your behalf. Answer how YOU \
         — the professional this agent represents — would handle the following, in your own \
         judgment and style. Be concrete and decisive.",
        row.persona_prompt
    );
    inference
        .worker(&system, question, row.cf_finetune_id.as_deref())
        .await
}
