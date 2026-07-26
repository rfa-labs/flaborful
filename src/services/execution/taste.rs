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
use crate::services::vault::{VaultOverlay, VaultOwner};

/// Serialize overlays for storage on the token (read-only company repo vault(s) attached to a
/// hired task). Shape: [{ "owner_kind", "owner_id", "repos": [...] }].
fn overlays_json(overlays: &[VaultOverlay]) -> serde_json::Value {
    serde_json::Value::Array(
        overlays
            .iter()
            .map(|o| {
                serde_json::json!({
                    "owner_kind": o.owner.kind_str(),
                    "owner_id": o.owner.id().to_string(),
                    "repos": o.repos,
                })
            })
            .collect(),
    )
}

/// Mint a per-task taste token bound to the agent's persona + LoRA, plus any read-only vault
/// overlays (the company repo vault(s) composed into this task). Returns the token string.
pub async fn mint_token(
    db: &PgPool,
    task_id: Uuid,
    agent_id: Uuid,
    persona_prompt: &str,
    cf_finetune_id: Option<&str>,
    overlays: &[VaultOverlay],
) -> Result<String> {
    let token = format!("tt_{}", Uuid::new_v4().simple());
    sqlx::query!(
        r#"INSERT INTO taste_tokens (token, task_id, owner_kind, agent_id, persona_prompt, cf_finetune_id, overlays)
           VALUES ($1, $2, 'agent', $3, $4, $5, $6)
           ON CONFLICT (task_id) DO UPDATE SET
             token = EXCLUDED.token, persona_prompt = EXCLUDED.persona_prompt,
             cf_finetune_id = EXCLUDED.cf_finetune_id, overlays = EXCLUDED.overlays,
             created_at = NOW()"#,
        token,
        task_id,
        agent_id,
        persona_prompt,
        cf_finetune_id,
        overlays_json(overlays),
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
        r#"INSERT INTO taste_tokens (token, task_id, owner_kind, agent_id, persona_prompt, cf_finetune_id)
           VALUES ($1, NULL, 'agent', $2, $3, $4)"#,
        token,
        agent_id,
        persona_prompt,
        cf_finetune_id,
    )
    .execute(db)
    .await?;
    Ok(token)
}

/// Mint a token bound to a COMPANY vault (no persona/LoRA) — for company code-ingestion runs that
/// document a company repo into the company's knowledge graph. The agent presents it to the vault
/// MCP tools; it resolves to `VaultOwner::Company`. Caller revokes with `revoke_by_token`.
pub async fn mint_company_token(db: &PgPool, company_id: Uuid) -> Result<String> {
    let token = format!("tt_{}", Uuid::new_v4().simple());
    sqlx::query!(
        r#"INSERT INTO taste_tokens (token, task_id, owner_kind, agent_id, persona_prompt, cf_finetune_id)
           VALUES ($1, NULL, 'company', $2, NULL, NULL)"#,
        token,
        company_id,
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

/// Resolve a token to the vault scope it authorizes: the PRIMARY owner (agent or company) plus
/// any read-only OVERLAY vault(s) — the company repo vault(s) composed into a hired task. The
/// token IS the agent's authority; every vault read/write is bounded by this scope.
pub async fn resolve_vault_scope(
    db: &PgPool,
    token: &str,
) -> Result<(VaultOwner, Vec<VaultOverlay>)> {
    let row = sqlx::query!(
        "SELECT owner_kind, agent_id, overlays FROM taste_tokens WHERE token = $1",
        token
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| anyhow!("invalid or expired token"))?;

    let primary = VaultOwner::from_parts(&row.owner_kind, row.agent_id);
    let overlays = parse_overlays(&row.overlays);
    Ok((primary, overlays))
}

/// Parse the token's stored overlays JSON into typed overlays. Tolerant of malformed entries.
fn parse_overlays(v: &serde_json::Value) -> Vec<VaultOverlay> {
    let Some(arr) = v.as_array() else {
        return vec![];
    };
    arr.iter()
        .filter_map(|o| {
            let kind = o.get("owner_kind")?.as_str()?;
            let id = Uuid::parse_str(o.get("owner_id")?.as_str()?).ok()?;
            let repos = o
                .get("repos")
                .and_then(|r| r.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Some(VaultOverlay {
                owner: VaultOwner::from_parts(kind, id),
                repos,
            })
        })
        .collect()
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

    // consult_principal is agent-only: a company-vault token has no persona/LoRA.
    let persona = row
        .persona_prompt
        .ok_or_else(|| anyhow!("this token has no principal to consult"))?;
    let system = format!(
        "{}\n\nYou are being consulted by a coding agent working on your behalf. Answer how YOU \
         — the professional this agent represents — would handle the following, in your own \
         judgment and style. Be concrete and decisive.",
        persona
    );
    // Likeness step (the whole point of consult_principal): answer via the professional's LoRA,
    // never optimizer-routed.
    // escalation intentionally omitted — likeness is LoRA-only by design; if a frontier fallback for
    // a hard/ambiguous consult is ever wanted, it belongs here (the call site), not in worker_likeness.
    inference
        .worker_likeness(&system, question, row.cf_finetune_id.as_deref())
        .await
}
