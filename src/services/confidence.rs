use crate::services::inference::InferenceClient;
use anyhow::Result;

/// Score the agent's output against the original task input.
/// Returns a confidence value in [0.0, 1.0].
///
/// Below 0.70 → mandatory human review.
/// 0.70–0.89  → queued for batch review (professional checks 1-2x/day).
/// 0.90+      → auto-approved if professional has enabled that setting (not in MVP).
pub async fn score_output(
    inference: &InferenceClient,
    task_input: &str,
    output_draft: &str,
    persona_context: &str,
) -> Result<f32> {
    let system = r#"You are a quality evaluator for AI-generated professional work outputs.
Given a task description, a draft output, and the professional's known style, score the output's quality and confidence.

Return a JSON object with exactly these keys:
- "score": float between 0.0 and 1.0
- "flags": array of strings describing concerns (empty if none)
- "reasoning": one sentence explaining the score

Scoring guide:
- 0.90–1.00: Excellent — fully addresses the task, matches professional style, no obvious gaps
- 0.70–0.89: Good — addresses the task but has minor style drift or small gaps
- 0.50–0.69: Fair — partially addresses the task, notable gaps or style mismatch
- 0.00–0.49: Poor — significantly off-target, incomplete, or potentially wrong"#;

    let user = format!(
        "Task input:\n{}\n\nDraft output:\n{}\n\nProfessional style context:\n{}",
        truncate(task_input, 800),
        truncate(output_draft, 1500),
        truncate(persona_context, 400),
    );

    let raw = inference.orchestrate(system, &user, true).await?;

    let parsed: serde_json::Value =
        serde_json::from_str(crate::services::inference::strip_fences(&raw))
            .unwrap_or(serde_json::json!({ "score": 0.5 }));

    let score = parsed
        .get("score")
        .and_then(|v| v.as_f64())
        .map(|f| f.clamp(0.0, 1.0) as f32)
        .unwrap_or(0.5);

    Ok(score)
}

/// Determine if a task requires mandatory human review based on confidence score.
#[allow(dead_code)] // confidence policy helper; HITL gate currently always reviews
pub fn requires_mandatory_review(score: f32) -> bool {
    score < 0.70
}

fn truncate(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        s
    } else {
        // Walk back to a UTF-8 boundary
        let mut idx = max_chars;
        while !s.is_char_boundary(idx) {
            idx -= 1;
        }
        &s[..idx]
    }
}
