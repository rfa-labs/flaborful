//! The per-task **Execution Blueprint** — the context-engineering layer over the LoRA.
//!
//! Before the builder starts, we assemble a compact blueprint from the professional's vault —
//! their standing style rules, an architecture map, and a few relevant golden snippets — and
//! inject it into the builder's prompt up front. This gives the builder the principal's standing
//! preferences cheaply, so it only escalates to the LoRA (`consult_principal`) for genuine
//! ambiguity the blueprint doesn't cover (cutting the per-decision "telephone game" overhead).
//!
//! The blueprint is distilled in ONE principal call (`inference.worker` with the LoRA), so it's
//! in the professional's voice. Retrieval is the hybrid vault search from `vault::retrieve`,
//! filtered by note layer.

use anyhow::Result;
use qdrant_client::Qdrant;
use sqlx::PgPool;
use uuid::Uuid;

use crate::services::inference::InferenceClient;
use crate::services::vault::{self, NoteLayer, RetrievalFilter, VaultOverlay};

/// Build the Execution Blueprint for a task. Returns markdown to prepend to the builder's
/// instructions, or an empty string if the vault has nothing relevant (the builder then runs as
/// before, consulting the principal directly).
///
/// `overlays` are read-only company repo vault(s) composed into this task; when present, the
/// blueprint also pulls the company's feature/architecture/pattern/tech notes for those repos and
/// includes them under a "Company context for this repo" section, so the agent starts grounded in
/// the real codebase it's about to work on.
pub async fn build_blueprint(
    db: &PgPool,
    qdrant: &Qdrant,
    inference: &InferenceClient,
    agent_id: Uuid,
    lora_name: Option<&str>,
    persona_prompt: &str,
    task_text: &str,
    overlays: &[VaultOverlay],
) -> Result<String> {
    // Pull the three layers most relevant to this task from the vault.
    let style = retrieve_layer(
        db,
        qdrant,
        inference,
        agent_id,
        task_text,
        NoteLayer::StyleRule,
        6,
    )
    .await;
    let arch = retrieve_layer(
        db,
        qdrant,
        inference,
        agent_id,
        task_text,
        NoteLayer::Architecture,
        4,
    )
    .await;
    let snippets = retrieve_layer(
        db,
        qdrant,
        inference,
        agent_id,
        task_text,
        NoteLayer::GoldenSnippet,
        4,
    )
    .await;

    // Company context: the composed company repo vault(s), if any. Pull feature/architecture/
    // pattern/tech notes scoped to the overlay repos.
    let company_ctx = retrieve_company_context(db, qdrant, inference, overlays, task_text).await;

    // Skills: reusable procedural playbooks matched to this task. Unlike the other layers, skills
    // are injected VERBATIM (not distilled through the LoRA) — they are prescriptive step-by-step
    // instructions the builder must follow, and paraphrasing them would drop the steps. This is
    // Gumloop's on-demand model: only skills that match the task load their full bodies. See
    // [[Flaborful Skills]].
    let skills = retrieve_layer(
        db,
        qdrant,
        inference,
        agent_id,
        task_text,
        NoteLayer::Skill,
        3,
    )
    .await;

    if style.is_empty()
        && arch.is_empty()
        && snippets.is_empty()
        && company_ctx.is_empty()
        && skills.is_empty()
    {
        return Ok(String::new());
    }

    // Distill into a compact blueprint in one principal (LoRA) call, in the professional's voice.
    let system = format!(
        "{persona}\n\nYou are preparing an Execution Blueprint for a coding agent that will do a \
         task on your behalf. Distill the retrieved notes below into a tight, actionable brief of \
         YOUR standing preferences — how you write code, your architectural patterns, and the \
         exemplary snippets worth emulating — scoped to the task. If 'Company context' notes are \
         present, summarize the relevant facts about THAT codebase the agent must respect, in a \
         '## Company context for this repo' section. Be concrete and prescriptive; omit anything \
         irrelevant to the task. Output markdown with sections: '## Standing style rules', \
         '## Architecture map', '## Golden snippets', and (only if company context exists) \
         '## Company context for this repo'.",
        persona = persona_prompt,
    );
    let company_block = if company_ctx.is_empty() {
        String::new()
    } else {
        format!("\n\n# Company context (the repo you're working on)\n{company_ctx}")
    };
    let user =
        format!(
        "# Task\n{task}\n\n# Retrieved style rules\n{style}\n\n# Retrieved architecture notes\n\
         {arch}\n\n# Retrieved golden snippets\n{snippets}{company}",
        task = task_text, style = style, arch = arch, snippets = snippets, company = company_block,
    );

    // Likeness step: distill in the professional's voice via their LoRA — never optimizer-routed.
    let distilled = inference.worker_likeness(&system, &user, lora_name).await?;

    // Applicable skills, appended verbatim after the distilled brief.
    let skills_block = if skills.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n## Applicable skills (follow these playbooks step-by-step when relevant)\n{}",
            skills.trim()
        )
    };

    Ok(format!(
        "# Execution Blueprint (your standing preferences)\n{}{}",
        distilled.trim(),
        skills_block
    ))
}

/// Retrieve the company repo vault context for the task: feature/architecture/pattern/tech notes
/// from each overlay owner, scoped to that overlay's repos. Best-effort; returns "" on no overlays.
async fn retrieve_company_context(
    db: &PgPool,
    qdrant: &Qdrant,
    inference: &InferenceClient,
    overlays: &[VaultOverlay],
    task_text: &str,
) -> String {
    let mut blocks = Vec::new();
    for ov in overlays {
        for layer in [
            NoteLayer::Feature,
            NoteLayer::Architecture,
            NoteLayer::Pattern,
            NoteLayer::Tech,
        ] {
            let filter = RetrievalFilter {
                layer: Some(layer),
                repos: ov.repos.clone(),
                ..Default::default()
            };
            if let Ok(hits) =
                vault::retrieve(db, qdrant, inference, ov.owner, task_text, &filter, 4).await
            {
                for h in hits {
                    blocks.push(format!("### {}\n{}", h.note.title, h.note.body));
                }
            }
        }
    }
    blocks.join("\n\n")
}

/// Retrieve and concatenate the bodies of the top notes in one layer for the task. Best-effort:
/// returns "" on error so a partial vault never blocks the build.
async fn retrieve_layer(
    db: &PgPool,
    qdrant: &Qdrant,
    inference: &InferenceClient,
    agent_id: Uuid,
    task_text: &str,
    layer: NoteLayer,
    k: usize,
) -> String {
    let filter = RetrievalFilter {
        layer: Some(layer),
        ..Default::default()
    };
    match vault::retrieve(
        db,
        qdrant,
        inference,
        crate::services::vault::VaultOwner::Agent(agent_id),
        task_text,
        &filter,
        k,
    )
    .await
    {
        Ok(hits) => hits
            .iter()
            .map(|h| format!("### {}\n{}", h.note.title, h.note.body))
            .collect::<Vec<_>>()
            .join("\n\n"),
        Err(e) => {
            tracing::debug!(
                "blueprint retrieve_layer {:?} failed: {}",
                layer.as_str(),
                e
            );
            String::new()
        }
    }
}
