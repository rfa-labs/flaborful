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
use crate::services::vault::{self, NoteLayer, RetrievalFilter};

/// Build the Execution Blueprint for a task. Returns markdown to prepend to the builder's
/// instructions, or an empty string if the vault has nothing relevant (the builder then runs as
/// before, consulting the principal directly).
pub async fn build_blueprint(
    db: &PgPool,
    qdrant: &Qdrant,
    inference: &InferenceClient,
    agent_id: Uuid,
    lora_name: Option<&str>,
    persona_prompt: &str,
    task_text: &str,
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

    if style.is_empty() && arch.is_empty() && snippets.is_empty() {
        return Ok(String::new());
    }

    // Distill into a compact blueprint in one principal (LoRA) call, in the professional's voice.
    let system = format!(
        "{persona}\n\nYou are preparing an Execution Blueprint for a coding agent that will do a \
         task on your behalf. Distill the retrieved notes below into a tight, actionable brief of \
         YOUR standing preferences — how you write code, your architectural patterns, and the \
         exemplary snippets worth emulating — scoped to the task. Be concrete and prescriptive; \
         omit anything irrelevant to the task. Output markdown with sections: \
         '## Standing style rules', '## Architecture map', '## Golden snippets'.",
        persona = persona_prompt,
    );
    let user =
        format!(
        "# Task\n{task}\n\n# Retrieved style rules\n{style}\n\n# Retrieved architecture notes\n\
         {arch}\n\n# Retrieved golden snippets\n{snippets}",
        task = task_text, style = style, arch = arch, snippets = snippets,
    );

    let distilled = inference.worker(&system, &user, lora_name).await?;
    Ok(format!(
        "# Execution Blueprint (your standing preferences)\n{}",
        distilled.trim()
    ))
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
        domain: None,
        shareable_only: false,
    };
    match vault::retrieve(db, qdrant, inference, agent_id, task_text, &filter, k).await {
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
