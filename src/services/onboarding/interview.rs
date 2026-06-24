use crate::services::inference::InferenceClient;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A scenario presented to the professional during the AI Interview.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct InterviewScenario {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect_slug: Option<String>,
    pub description: String,
    pub options: Vec<String>,
}

/// The professional's response to one scenario.
#[derive(Debug, Serialize, Deserialize)]
pub struct InterviewResponse {
    pub scenario_id: Uuid,
    pub chosen_option_index: usize,
    pub critique: String,
}

/// A captured preference pair for RLHF training.
/// chosen > rejected based on professional's selection.
#[derive(Debug, Serialize, Deserialize)]
pub struct PreferencePair {
    pub scenario: String,
    pub chosen: String,
    pub rejected: Vec<String>,
    pub critique: String,
}

/// An open-form interview question — the slimmed, high-signal format. Fewer questions, no
/// A/B/C battery; each targets communication & reasoning style (the part code can't reveal).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpenQuestion {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect_slug: Option<String>,
    pub prompt: String,
}

/// The professional's free-text answer to an open question.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenAnswer {
    pub question_id: Uuid,
    pub prompt: String,
    pub answer: String,
}

pub struct InterviewService<'a> {
    inference: &'a InferenceClient,
}

impl<'a> InterviewService<'a> {
    pub fn new(inference: &'a InferenceClient) -> Self {
        Self { inference }
    }

    /// Generate a batch of profession-specific interview scenarios.
    /// When `aspect` is provided, scenarios are scoped to that skill area.
    pub async fn generate_scenarios(
        &self,
        profession: &str,
        aspect: Option<&str>,
        count: usize,
    ) -> Result<Vec<InterviewScenario>> {
        let aspect_context = aspect
            .map(|a| format!(" focused specifically on **{}**", a.replace('_', " ")))
            .unwrap_or_default();

        let system = format!(
            r#"You are generating professional judgment scenarios for a {profession}{aspect_context}.
Each scenario tests a specific technical or professional decision.
Return a JSON array of exactly {count} objects. Each object must have:
- "description": a 2-3 sentence scenario describing a real work situation
- "options": array of exactly 3 plausible approaches, ordered A/B/C

Make scenarios specific and realistic — not textbook examples.{extra}"#,
            profession = profession,
            aspect_context = aspect_context,
            count = count,
            extra = if aspect.is_some() {
                " All scenarios must be directly relevant to this specific skill area."
            } else {
                "\nCover different areas: code quality, architecture, communication, prioritization, debugging."
            }
        );

        let user = format!(
            "Generate {count} interview scenarios for a {profession}{aspect_context}.",
            count = count,
            profession = profession,
            aspect_context = aspect_context,
        );

        #[derive(Deserialize)]
        struct RawScenario {
            description: String,
            options: Vec<String>,
        }

        let aspect_slug = aspect.map(|s| s.to_string());

        // Scenario generation goes through the inference provider (Ollama Cloud), which can be
        // flaky/slow. It must NEVER block (or noticeably delay) the interview from starting — on
        // any failure OR if it takes more than a few seconds, we fall back to a canned scenario
        // set so onboarding proceeds immediately. The richer personalization signal comes from the
        // vault + open-form interview anyway, so a fast canned fallback is the right tradeoff.
        const GEN_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);
        let gen = tokio::time::timeout(
            GEN_BUDGET,
            self.inference.orchestrate(&system, &user, true),
        )
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("scenario generation timed out")));

        let raw_scenarios: Vec<RawScenario> = match gen
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|raw| {
                let json_str = crate::services::inference::strip_fences(&raw);
                serde_json::from_str::<Vec<RawScenario>>(json_str)
                    .or_else(|_| {
                        let v: serde_json::Value = serde_json::from_str(json_str)?;
                        let arr = v
                            .get("scenarios")
                            .or_else(|| v.get("items"))
                            .cloned()
                            .unwrap_or(v);
                        serde_json::from_value(arr)
                    })
                    .map_err(|e| anyhow::anyhow!("{e}"))
            }) {
            Ok(s) if !s.is_empty() => s,
            Ok(_) | Err(_) => {
                tracing::warn!(
                    "scenario generation failed for {profession}/{aspect:?} — using canned fallback"
                );
                fallback_scenarios(count)
                    .into_iter()
                    .map(|(description, options)| RawScenario { description, options })
                    .collect()
            }
        };

        Ok(raw_scenarios
            .into_iter()
            .map(|s| InterviewScenario {
                id: Uuid::new_v4(),
                aspect_slug: aspect_slug.clone(),
                description: s.description,
                options: s.options,
            })
            .collect())
    }

    /// Generate a SMALL batch of open-form questions targeting communication & reasoning style —
    /// how the professional explains a decision, pushes back in review, weighs trade-offs, what
    /// they optimize for. This is the slimmed interview: code-derivable facts come from the
    /// agentic vault ingestion, so the interview captures only what the code can't reveal.
    pub async fn generate_open_questions(
        &self,
        profession: &str,
        aspect: Option<&str>,
        count: usize,
    ) -> Result<Vec<OpenQuestion>> {
        let aspect_context = aspect
            .map(|a| format!(" with a focus on **{}**", a.replace('_', " ")))
            .unwrap_or_default();
        let system = format!(
            r#"You are interviewing a {profession}{aspect_context} to capture their communication \
and reasoning STYLE — not facts that can be read from their code. Ask about how they explain \
decisions, how they give/receive code review feedback, how they weigh trade-offs under \
constraints, and what they optimize for. Favor open-ended prompts that elicit a few sentences of \
genuine voice.

Return a JSON array of exactly {count} objects, each: {{ "prompt": "<the open-ended question>" }}. \
Make them specific and provocative, not generic."#,
            profession = profession,
            aspect_context = aspect_context,
            count = count,
        );
        let user = format!(
            "Generate {count} open-form style questions for a {profession}{aspect_context}.",
            count = count,
            profession = profession,
            aspect_context = aspect_context
        );

        let raw = self.inference.orchestrate(&system, &user, true).await?;
        let json_str = crate::services::inference::strip_fences(&raw);

        #[derive(Deserialize)]
        struct RawQ {
            prompt: String,
        }
        let raw_qs: Vec<RawQ> = serde_json::from_str(json_str).or_else(|_| {
            let v: serde_json::Value = serde_json::from_str(json_str)?;
            let arr = v
                .get("questions")
                .or_else(|| v.get("items"))
                .cloned()
                .unwrap_or(v);
            serde_json::from_value(arr)
        })?;

        let aspect_slug = aspect.map(|s| s.to_string());
        Ok(raw_qs
            .into_iter()
            .map(|q| OpenQuestion {
                id: Uuid::new_v4(),
                aspect_slug: aspect_slug.clone(),
                prompt: q.prompt,
            })
            .collect())
    }

    /// Convert interview responses into RLHF preference pairs.
    /// These are stored and used to build the LoRA training dataset.
    pub fn build_preference_pairs(
        scenarios: &[InterviewScenario],
        responses: &[InterviewResponse],
    ) -> Vec<PreferencePair> {
        let mut pairs = Vec::new();

        for response in responses {
            if let Some(scenario) = scenarios.iter().find(|s| s.id == response.scenario_id) {
                if response.chosen_option_index >= scenario.options.len() {
                    continue;
                }
                let chosen = scenario.options[response.chosen_option_index].clone();
                let rejected: Vec<String> = scenario
                    .options
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != response.chosen_option_index)
                    .map(|(_, opt)| opt.clone())
                    .collect();

                pairs.push(PreferencePair {
                    scenario: scenario.description.clone(),
                    chosen,
                    rejected,
                    critique: response.critique.clone(),
                });
            }
        }

        pairs
    }
}

/// Canned, profession-agnostic judgment scenarios used when live generation is unavailable, so
/// the interview always loads. Returns up to `count` (description, [A,B,C]) tuples.
fn fallback_scenarios(count: usize) -> Vec<(String, Vec<String>)> {
    let all: Vec<(&str, [&str; 3])> = vec![
        (
            "A teammate's PR works and ships a needed feature, but the code duplicates logic that already exists elsewhere and adds no tests. The deadline is tomorrow.",
            [
                "Approve now, file a follow-up ticket to dedupe and add tests later.",
                "Request changes: ask them to reuse the existing logic and add tests before merge.",
                "Approve, but pair with them after merge to refactor it together.",
            ],
        ),
        (
            "You're adding a feature that needs data from three services. You can wire it together quickly with synchronous calls, or invest a day in an async, fault-tolerant approach.",
            [
                "Ship the simple synchronous version now; revisit if it becomes a problem.",
                "Build the async/fault-tolerant version up front — it's core path.",
                "Ship simple but add timeouts/fallbacks so it degrades gracefully.",
            ],
        ),
        (
            "A production bug is affecting some users. You have a quick hotfix that treats the symptom, and a proper fix that needs a few hours of investigation.",
            [
                "Ship the hotfix now to stop the bleeding, then do the real fix.",
                "Hold off on the hotfix and find the root cause first.",
                "Hotfix behind a flag, investigate in parallel, swap in the real fix.",
            ],
        ),
        (
            "You inherit a module with no tests and unclear behavior. You need to change it for a new requirement.",
            [
                "Characterize the existing behavior with tests first, then change it.",
                "Make the change carefully and add tests for the new path only.",
                "Rewrite the module cleanly while you're in there.",
            ],
        ),
        (
            "In code review, you strongly disagree with an architectural choice a senior colleague made, but it's already merged and working.",
            [
                "Raise it directly with them with your reasoning and propose an alternative.",
                "Let it go — it works, and it's not worth the friction.",
                "Bring it to a team discussion as a general pattern question, not a callout.",
            ],
        ),
        (
            "A stakeholder asks for an estimate on a vague feature. You don't have enough detail to be confident.",
            [
                "Give a wide range and the assumptions behind it.",
                "Ask clarifying questions first, then estimate.",
                "Time-box a quick spike, then estimate from what you learn.",
            ],
        ),
    ];
    all.into_iter()
        .take(count.max(1))
        .map(|(d, opts)| (d.to_string(), opts.iter().map(|s| s.to_string()).collect()))
        .collect()
}
