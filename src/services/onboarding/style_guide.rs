use super::github::GitHubProfile;
use crate::services::inference::InferenceClient;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Structured style guide extracted from a professional's portfolio.
/// Stored as `style_guide_json` on the Agent row.
#[allow(dead_code)] // retained as a cheap persona-seed fallback; the vault is the primary path now
#[derive(Debug, Serialize, Deserialize)]
pub struct StyleGuide {
    pub coding_conventions: Vec<String>,
    pub architectural_biases: Vec<String>,
    pub communication_tone: String,
    pub formatting_preferences: Vec<String>,
    pub technology_preferences: Vec<String>,
    pub review_patterns: Vec<String>,
}

pub struct StyleGuideExtractor<'a> {
    inference: &'a InferenceClient,
}

impl<'a> StyleGuideExtractor<'a> {
    pub fn new(inference: &'a InferenceClient) -> Self {
        Self { inference }
    }

    /// Extract a StyleGuide from a GitHub profile using the orchestrator model.
    pub async fn extract_from_github(&self, profile: &GitHubProfile) -> Result<Value> {
        let context = build_profile_context(profile);

        let system = r#"You are an expert at analyzing software engineers' work and extracting their professional style.
Given a professional's GitHub portfolio, extract a structured style guide capturing their technical identity.

Return a JSON object with these exact keys:
- coding_conventions: array of specific patterns (e.g. "prefers early returns", "uses descriptive variable names")
- architectural_biases: array of design preferences (e.g. "favors modular design", "prefers composition over inheritance")
- communication_tone: single string describing how they write (e.g. "direct and technical, minimal prose")
- formatting_preferences: array of formatting patterns (e.g. "consistent 4-space indentation", "blank line between logical blocks")
- technology_preferences: array of tech choices they gravitate toward
- review_patterns: array of what they prioritize in code reviews (inferred from PR descriptions and README quality)

Be specific and concrete. Avoid generic observations."#;

        let user = format!(
            "Analyze this GitHub portfolio and extract the professional style guide:\n\n{}",
            context
        );

        let raw = self.inference.orchestrate(system, &user, true).await?;
        let guide: Value = serde_json::from_str(crate::services::inference::strip_fences(&raw))
            .map_err(|e| anyhow::anyhow!("JSON parse failed: {e} | raw={raw}"))?;
        Ok(guide)
    }

    /// Extract a focused insight chunk for a specific aspect from the GitHub profile.
    /// Returns None for aspects that can't be reliably inferred from code (e.g. debugging).
    pub async fn extract_aspect_chunk(
        &self,
        profile: &GitHubProfile,
        aspect_slug: &str,
    ) -> Result<Option<String>> {
        let system = match aspect_slug {
            "refactoring" => "You analyze a software engineer's GitHub history to extract their refactoring style. \
                Look at repo structures, commit patterns (inferred from README descriptions), and code organization. \
                Return a concise 200-word summary of HOW this person refactors: what they extract, simplify, restructure. \
                Be specific. Return plain text only.",
            "system_design" => "You analyze a software engineer's GitHub repos to extract their system design approach. \
                Look at repo architecture, module organization, dependency choices, how they structure large codebases. \
                Return a concise 200-word summary of their design philosophy and patterns. Return plain text only.",
            "documentation" => "You analyze a software engineer's GitHub repos to extract their documentation style. \
                Look at README quality, inline comments, docstrings, API docs, type annotations. \
                Return a concise 200-word summary of how they document code and what they prioritize. Return plain text only.",
            "code_review" => {
                if profile.pr_reviews.is_empty() {
                    return Ok(None);
                }
                "You analyze a software engineer's PR review comments to extract their code review style. \
                Look at what they flag, how they phrase feedback, what quality bars they hold. \
                Return a concise 200-word summary of their review approach. Return plain text only."
            }
            _ => return Ok(None), // debugging and unknown aspects — rely on interview
        };

        let context = if aspect_slug == "code_review" {
            let reviews: String = profile
                .pr_reviews
                .iter()
                .take(20)
                .map(|r| {
                    format!(
                        "[{}] {}: {}",
                        r.state,
                        r.repo,
                        r.body.chars().take(300).collect::<String>()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n---\n");
            format!("PR reviews by {}:\n{}", profile.username, reviews)
        } else {
            build_profile_context(profile)
        };

        let user = format!(
            "Analyze this GitHub profile and extract the {} insights:\n\n{}",
            aspect_slug.replace('_', " "),
            context
        );

        let raw = self.inference.orchestrate(system, &user, false).await?;
        let text = raw.trim().to_string();
        if text.is_empty() {
            Ok(None)
        } else {
            Ok(Some(text))
        }
    }

    /// Build a condensed persona system prompt from the style guide.
    /// This is injected at inference time alongside RAG context.
    pub fn build_persona_prompt(username: &str, style_guide: &Value) -> String {
        format!(
            r#"You are an AI agent representing {username}, a software engineer. You embody their professional style exactly.

Style guide:
{style}

When completing tasks:
- Write code, reviews, and communications in their established style
- Apply their architectural preferences to all design decisions
- Match their communication tone precisely
- Prioritize what they prioritize in code quality"#,
            username = username,
            style = serde_json::to_string_pretty(style_guide).unwrap_or_default()
        )
    }
}

fn build_profile_context(profile: &GitHubProfile) -> String {
    let mut parts = Vec::new();
    parts.push(format!("GitHub username: {}", profile.username));
    parts.push(format!(
        "Total public repos analyzed: {}",
        profile.repos.len()
    ));

    for repo in &profile.repos {
        let mut section = format!("\n## Repository: {}", repo.name);
        if let Some(desc) = &repo.description {
            section.push_str(&format!("\nDescription: {}", desc));
        }
        if let Some(lang) = &repo.language {
            section.push_str(&format!("\nPrimary language: {}", lang));
        }
        if let Some(readme) = &repo.readme {
            let preview = readme.chars().take(800).collect::<String>();
            section.push_str(&format!("\nREADME preview:\n{}", preview));
        }
        parts.push(section);
    }

    parts.join("\n")
}
