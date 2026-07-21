//! HITL **tool-approval policy** — one per-agent policy that decides whether a side-effecting node
//! must pause for owner approval BEFORE it fires (even on its first run), generalizing flaborful's
//! two existing gates into one policy layer. See [[Flaborful Tool-Approval Policy]].
//!
//! This does NOT replace the rerun side-effect gate (reuse/update/refire on a node that already
//! wrote) — it adds a *pre-fire* check on the first run, reusing the same `awaiting_effect` pause
//! + resolution machinery (`refire` = approve, `reuse` = skip).

use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// The resolved policy for an agent.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolPolicy {
    pub mode: String, // always_allow | ask_on_writes | ask_each_time
    pub rules: Value, // per-effect-kind overrides: { "github_pr": "ask" | "allow" }
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            mode: "always_allow".into(),
            rules: serde_json::json!({}),
        }
    }
}

/// Load an agent's policy, defaulting to always_allow (the historical behavior) when unset.
pub async fn get(db: &PgPool, agent_id: Uuid) -> Result<ToolPolicy> {
    let row = sqlx::query_as::<_, (String, Value)>(
        "SELECT mode, rules FROM agent_tool_policies WHERE agent_id = $1",
    )
    .bind(agent_id)
    .fetch_optional(db)
    .await?;
    Ok(match row {
        Some((mode, rules)) => ToolPolicy { mode, rules },
        None => ToolPolicy::default(),
    })
}

/// Upsert an agent's policy.
pub async fn set(db: &PgPool, agent_id: Uuid, mode: &str, rules: &Value) -> Result<()> {
    let mode = match mode {
        "ask_on_writes" | "ask_each_time" | "always_allow" => mode,
        _ => "always_allow",
    };
    sqlx::query(
        r#"INSERT INTO agent_tool_policies (agent_id, mode, rules, updated_at)
           VALUES ($1, $2, $3, NOW())
           ON CONFLICT (agent_id) DO UPDATE SET mode = EXCLUDED.mode, rules = EXCLUDED.rules, updated_at = NOW()"#,
    )
    .bind(agent_id)
    .bind(mode)
    .bind(rules)
    .execute(db)
    .await?;
    Ok(())
}

/// Decide whether a side-effecting node with the given `effect_kind` needs pre-fire approval under
/// this policy. A per-effect-kind rule (`allow`/`ask`) overrides the mode.
pub fn requires_approval(policy: &ToolPolicy, effect_kind: &str) -> bool {
    if let Some(rule) = policy.rules.get(effect_kind).and_then(|v| v.as_str()) {
        return rule == "ask";
    }
    matches!(policy.mode.as_str(), "ask_on_writes" | "ask_each_time")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mode_and_rule_precedence() {
        // always_allow → never ask
        let p = ToolPolicy {
            mode: "always_allow".into(),
            rules: json!({}),
        };
        assert!(!requires_approval(&p, "github_pr"));

        // ask_on_writes → ask for any write
        let p = ToolPolicy {
            mode: "ask_on_writes".into(),
            rules: json!({}),
        };
        assert!(requires_approval(&p, "slack_message"));

        // a per-kind rule overrides the mode both ways
        let p = ToolPolicy {
            mode: "ask_on_writes".into(),
            rules: json!({ "slack_message": "allow" }),
        };
        assert!(!requires_approval(&p, "slack_message"));
        assert!(requires_approval(&p, "github_pr"));

        let p = ToolPolicy {
            mode: "always_allow".into(),
            rules: json!({ "github_pr": "ask" }),
        };
        assert!(requires_approval(&p, "github_pr"));
        assert!(!requires_approval(&p, "slack_message"));
    }
}
