//! Task **evaluations** — the graded successor to the single confidence score.
//!
//! When a task reaches a terminal output, we grade it against the agent's per-agent **criteria**
//! (reusable true/false quality rules) in one structured LLM call, plus free-form **tags** and
//! extracted **data points**, then compute a **deterministic grade** (Pass / Warning / Critical)
//! from the per-criterion results — never by asking the model for the grade directly (Gumloop's
//! model). A Critical grade alerts the owner. The results feed [[Flaborful Reflections & Evaluations]]:
//! the reflection cycle mines low grades for patterns to propose fixes.
//!
//! `confidence::score_output` still provides the lightweight per-draft score used by the HITL gate;
//! evaluations are the richer, criteria-based, per-task grade stored in `task_evaluations`.

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::services::inference::InferenceClient;
use crate::services::notifications;

/// Deterministic grade computed from criteria results. API/DB string is lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Grade {
    Pass,
    Warning,
    Critical,
}

impl Grade {
    pub fn as_str(&self) -> &'static str {
        match self {
            Grade::Pass => "pass",
            Grade::Warning => "warning",
            Grade::Critical => "critical",
        }
    }
}

/// One agent criterion (a reusable true/false quality rule).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Criterion {
    pub id: Uuid,
    pub name: String,
    pub prompt: String,
    pub priority: String, // "warning" | "critical"
}

pub async fn list_criteria(db: &PgPool, agent_id: Uuid) -> Result<Vec<Criterion>> {
    Ok(sqlx::query_as::<_, Criterion>(
        "SELECT id, name, prompt, priority FROM agent_criteria WHERE agent_id = $1 AND enabled = true ORDER BY created_at",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?)
}

/// Evaluate a completed task's output against the agent's criteria and persist a `task_evaluations`
/// row (replacing any prior one). No-op (returns None) if the agent has no criteria configured, so
/// evaluations are opt-in. Best-effort: a malformed LLM response degrades to a Pass with a note
/// rather than blocking delivery.
pub async fn evaluate_task(
    db: &PgPool,
    inference: &InferenceClient,
    agent_id: Uuid,
    task_id: Uuid,
    task_input: &str,
    output: &str,
) -> Result<Option<Grade>> {
    let criteria = list_criteria(db, agent_id).await?;
    if criteria.is_empty() {
        return Ok(None);
    }

    let criteria_list = criteria
        .iter()
        .map(|c| format!("- {} ({}): {}", c.name, c.priority, c.prompt))
        .collect::<Vec<_>>()
        .join("\n");

    let system = "You are a strict evaluator of a professional work output. For EACH criterion, \
        decide pass/fail against the output and give a one-line reason. Also extract any obvious \
        short tags. Return JSON: { \"criteria\": [{ \"name\": string, \"passed\": boolean, \
        \"reasoning\": string }], \"tags\": [string], \"summary\": string }. Judge only what the \
        output shows; do not invent facts.";
    let user = format!(
        "# Task\n{}\n\n# Output\n{}\n\n# Criteria\n{}",
        truncate(task_input, 1000),
        truncate(output, 2500),
        criteria_list,
    );

    let raw = inference.orchestrate(system, &user, true).await?;
    let parsed: Value =
        serde_json::from_str(crate::services::inference::strip_fences(&raw)).unwrap_or(json!({}));

    // Map the model's per-criterion pass/fail back onto our criteria (by name), carrying each
    // criterion's configured priority — the grade is computed from OUR priorities, not the model's.
    let model_results = parsed
        .get("criteria")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let passed_by_name = |name: &str| -> Option<(bool, String)> {
        model_results.iter().find_map(|r| {
            let n = r.get("name").and_then(|v| v.as_str())?;
            if n.eq_ignore_ascii_case(name) {
                let passed = r.get("passed").and_then(|v| v.as_bool()).unwrap_or(true);
                let reason = r
                    .get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some((passed, reason))
            } else {
                None
            }
        })
    };

    let mut results = Vec::new();
    let mut any_critical_fail = false;
    let mut any_warning_fail = false;
    for c in &criteria {
        let (passed, reasoning) = passed_by_name(&c.name).unwrap_or((true, String::new()));
        if !passed {
            if c.priority == "critical" {
                any_critical_fail = true;
            } else {
                any_warning_fail = true;
            }
        }
        results.push(json!({
            "name": c.name, "passed": passed, "priority": c.priority, "reasoning": reasoning,
        }));
    }

    // Deterministic grade hierarchy.
    let grade = if any_critical_fail {
        Grade::Critical
    } else if any_warning_fail {
        Grade::Warning
    } else {
        Grade::Pass
    };

    let tags = parsed.get("tags").cloned().unwrap_or(json!([]));
    let summary = parsed
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    sqlx::query(
        r#"INSERT INTO task_evaluations (task_id, agent_id, grade, criteria_results, tags, summary)
           VALUES ($1, $2, $3, $4, $5, $6)
           ON CONFLICT (task_id) DO UPDATE
             SET grade = EXCLUDED.grade, criteria_results = EXCLUDED.criteria_results,
                 tags = EXCLUDED.tags, summary = EXCLUDED.summary, created_at = NOW()"#,
    )
    .bind(task_id)
    .bind(agent_id)
    .bind(grade.as_str())
    .bind(json!(results))
    .bind(tags)
    .bind(&summary)
    .execute(db)
    .await?;

    if grade == Grade::Critical {
        let _ = notifications::notify_eval_critical(db, agent_id, task_id, &summary).await;
    }

    Ok(Some(grade))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grade_strings() {
        assert_eq!(Grade::Pass.as_str(), "pass");
        assert_eq!(Grade::Warning.as_str(), "warning");
        assert_eq!(Grade::Critical.as_str(), "critical");
    }
}
