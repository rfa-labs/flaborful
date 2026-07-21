//! Agentic code ingestion — the professional's repos become their knowledge vault.
//!
//! Instead of skimming READMEs into a flat style guide, a coding agent (the Managed-Agents
//! backend) clones each repo, *reads the real source*, and writes a structured, linked,
//! MOC'd vault — feature / pattern / tech / golden-snippet notes with frontmatter + `[[links]]`
//! — via the shared vault MCP tools (`upsert_note`, `search_vault`, …). This is the same kind of
//! knowledge base we build for the RFA Labs vault, now produced for the professional.
//!
//! Each repo is one background run. The run reuses the work-execution backend + transcript sink
//! + the per-run vault token; it has no job/task (token bound to the agent, not a task). The
//! agent's `task_text` is the documentation brief; it produces notes as a side effect (we don't
//! care about the diff — nothing is committed).

use std::collections::HashSet;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

use crate::services::execution::{
    ExecutionBackendKind, ExecutionBackends, TaskBrief, TranscriptSink,
};
use crate::services::vault::VaultOwner;

/// Agents whose ingestion loop has been asked to stop. The background loop checks this before
/// starting each repo and bails if its agent is listed, so an in-flight run finishes the current
/// repo (preserving its notes) but no further repos are started. Process-global + best-effort.
fn cancel_set() -> &'static Mutex<HashSet<Uuid>> {
    static SET: OnceLock<Mutex<HashSet<Uuid>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Request that the agent's ingestion loop stop after the current repo. Idempotent.
pub fn request_cancel(agent_id: Uuid) {
    if let Ok(mut s) = cancel_set().lock() {
        s.insert(agent_id);
    }
}

/// True if a cancel was requested for this agent. Clears the flag (one-shot) so a later re-run
/// isn't pre-cancelled.
fn take_cancel(agent_id: Uuid) -> bool {
    cancel_set()
        .lock()
        .map(|mut s| s.remove(&agent_id))
        .unwrap_or(false)
}

/// Non-consuming check used inside the loop between repos.
fn is_cancelled(agent_id: Uuid) -> bool {
    cancel_set()
        .lock()
        .map(|s| s.contains(&agent_id))
        .unwrap_or(false)
}

/// A transcript sink that records the ingestion agent's progress into `ingestion_events`, so the
/// onboarding UI can stream it live (the managed backend pushes session/tool/message events here).
/// One sink per repo; it keeps a running note counter and classifies each pushed line.
struct IngestionSink {
    db: PgPool,
    /// Vault owner id (agent id or company id) — stored in the `agent_id` column.
    owner_id: Uuid,
    owner_kind: &'static str,
    repo: String,
    notes: AtomicI32,
}

impl IngestionSink {
    fn new(db: PgPool, owner: VaultOwner, repo: String) -> Self {
        Self {
            db,
            owner_id: owner.id(),
            owner_kind: owner.kind_str(),
            repo,
            notes: AtomicI32::new(0),
        }
    }

    /// Note count written for this repo so far (for the per-repo `done` status event).
    fn notes_count(&self) -> i32 {
        self.notes.load(Ordering::Relaxed)
    }

    async fn record(&self, kind: &str, status: Option<&str>, detail: &str, notes: i32) {
        if let Err(e) = sqlx::query!(
            "INSERT INTO ingestion_events (owner_kind, agent_id, repo, kind, status, detail, notes_count)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            self.owner_kind,
            self.owner_id,
            self.repo,
            kind,
            status,
            detail,
            notes,
        )
        .execute(&self.db)
        .await
        {
            tracing::warn!("ingestion_events insert failed: {:?}", e);
        }
    }

    /// Emit a per-repo lifecycle status row (running | done | failed) carrying the running
    /// event tally as `notes_count`.
    async fn status(&self, status: &str, detail: &str) {
        self.record("status", Some(status), detail, self.notes_count())
            .await;
    }

    /// Emit a status row with an explicit, authoritative note count. Used for `done`, where the
    /// number must reflect distinct vault rows (the event tally double-counts on retries/reruns).
    async fn status_with_count(&self, status: &str, detail: &str, count: i32) {
        self.record("status", Some(status), detail, count).await;
    }

    /// The authoritative count of notes in the vault for THIS repo (distinct rows, not events).
    async fn vault_note_count(&self) -> i32 {
        sqlx::query_scalar!(
            "SELECT COUNT(*) FROM vault_notes WHERE owner_kind = $1 AND agent_id = $2 AND repo = $3",
            self.owner_kind,
            self.owner_id,
            self.repo,
        )
        .fetch_one(&self.db)
        .await
        .ok()
        .flatten()
        .unwrap_or(0) as i32
    }
}

#[async_trait::async_trait]
impl TranscriptSink for IngestionSink {
    async fn push(&self, chunk: &str) {
        // Detect a note being written, across BOTH backends:
        //  - Managed prefixes vault-tool results with the tool name: "[upsert_note] → ...".
        //  - Sprite streams Claude Code's `stream-json` events, where an upsert shows up as a
        //    tool-use line naming `upsert_note` (e.g. `mcp__vault__upsert_note`).
        // Either way, tag the event as a note so the UI can distinguish it from generic transcript
        // and tick the live counter. (The authoritative final tally still comes from
        // vault_note_count() against the DB, so a missed/extra live tick can't corrupt the result.)
        let is_note =
            chunk.starts_with("[upsert_note]") || chunk.contains("mcp__vault__upsert_note");
        let notes = if is_note {
            self.notes.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            self.notes_count()
        };
        let kind = if is_note { "note" } else { "message" };
        self.record(kind, None, chunk, notes).await;
    }
}

/// Spawn background ingestion for the agent's top repos. Returns immediately; each repo is
/// documented into the vault asynchronously. `repos` is `(full_name, default_branch)` pairs —
/// "owner/repo" plus the branch to check out (repos aren't always on `main`). The caller picks
/// them from the GitHub repo list, capped.
pub fn spawn_ingestion(
    db: PgPool,
    backends: ExecutionBackends,
    owner: VaultOwner,
    // The principal's persona + LoRA, for agent vaults (gives ingestion the professional's voice
    // + consult_principal). None for company vaults — a company has no principal to consult.
    principal: Option<(String, Option<String>)>,
    github_token: Option<String>,
    repos: Vec<(String, String)>,
    // When false (default), repos that already have vault notes are skipped (resume). When true,
    // every selected repo is re-documented even if notes exist (overwrites via ON CONFLICT).
    force: bool,
) {
    tokio::spawn(async move {
        let owner_id = owner.id();
        let owner_kind = owner.kind_str();
        // Fresh start: clear any prior run's progress events for this owner so the live feed and
        // the `complete` snapshot reflect THIS run, not a stale previous attempt (otherwise the UI
        // sees the old `complete` sentinel and refuses to show progress).
        if let Err(e) = sqlx::query!(
            "DELETE FROM ingestion_events WHERE owner_kind = $1 AND agent_id = $2",
            owner_kind,
            owner_id
        )
        .execute(&db)
        .await
        {
            tracing::warn!("failed to clear prior ingestion_events for {owner_id}: {e:?}");
        }

        let total = repos.len();
        for (repo, branch) in repos {
            // Cancellation: if a stop was requested, finish here. Any repo already documented in
            // this run keeps its notes; we just don't start the next one.
            if is_cancelled(owner_id) {
                tracing::info!("ingestion cancelled for {owner_id} — stopping before {repo}");
                let sink = IngestionSink::new(db.clone(), owner, repo.clone());
                sink.status(
                    "failed",
                    &format!("Cancelled before {repo} (stopped by request)"),
                )
                .await;
                break;
            }
            // Resume: skip a repo that already has notes in the vault (unless `force`). The
            // authoritative signal is vault_notes.repo (stamped server-side per run), which
            // survives restarts — not the event log, which we clear each run.
            if !force {
                let already = sqlx::query_scalar!(
                    "SELECT COUNT(*) FROM vault_notes WHERE owner_kind = $1 AND agent_id = $2 AND repo = $3",
                    owner_kind,
                    owner_id,
                    repo,
                )
                .fetch_one(&db)
                .await
                .unwrap_or(Some(0))
                .unwrap_or(0);
                if already > 0 {
                    tracing::info!("skipping {repo} — already has {already} notes");
                    let sink = IngestionSink::new(db.clone(), owner, repo.clone());
                    sink.status(
                        "done",
                        &format!("Skipped {repo} — already documented ({already} notes)"),
                    )
                    .await;
                    continue;
                }
            }

            if let Err(e) = ingest_repo(
                &db,
                &backends,
                owner,
                principal.as_ref(),
                github_token.clone(),
                &repo,
                &branch,
            )
            .await
            {
                tracing::warn!(
                    "vault ingestion failed for {} (owner {}): {:?}",
                    repo,
                    owner_id,
                    e
                );
            }
        }
        // Clear any cancel flag so a future re-run for this owner isn't pre-cancelled.
        take_cancel(owner_id);
        tracing::info!("vault ingestion complete for owner {}", owner_id);
        // Overall completion marker (repo="" sentinel) so the UI can stop the live feed and tell
        // the owner it's safe to proceed.
        let _ = sqlx::query!(
            "INSERT INTO ingestion_events (owner_kind, agent_id, repo, kind, status, detail)
             VALUES ($1, $2, '', 'status', 'complete', $3)",
            owner_kind,
            owner_id,
            format!("Ingestion complete — documented {total} repos"),
        )
        .execute(&db)
        .await;
    });
}

/// Document one repo into the owner's vault. Runs the Managed-Agents backend with a documentation
/// brief; the agent writes notes via the vault tools. Best-effort. `principal` is the persona +
/// LoRA for agent vaults (None for company vaults — there's no principal to consult).
async fn ingest_repo(
    db: &PgPool,
    backends: &ExecutionBackends,
    owner: VaultOwner,
    principal: Option<&(String, Option<String>)>,
    github_token: Option<String>,
    repo: &str,
    base_branch: &str,
) -> Result<()> {
    // Mint the per-run vault token. Agent vaults get a persona/LoRA-bound token (so the agent
    // documents in the professional's voice and can consult_principal); company vaults get a
    // company-scoped token with no principal.
    let (token, persona_prompt, cf_finetune_id) = match owner {
        VaultOwner::Agent(agent_id) => {
            let (persona, cf) = principal.cloned().unwrap_or_else(|| (String::new(), None));
            let token = crate::services::execution::taste::mint_agent_token(
                db,
                agent_id,
                &persona,
                cf.as_deref(),
            )
            .await?;
            (token, persona, cf)
        }
        VaultOwner::Company(company_id) => {
            let token =
                crate::services::execution::taste::mint_company_token(db, company_id).await?;
            (token, String::new(), None)
        }
    };

    let api_base_url =
        std::env::var("API_BASE_URL").unwrap_or_else(|_| "http://localhost:8080".into());
    // The ingestion run is read-only documentation; we never commit/push, so task_id is a fresh
    // uuid only used to name the agent/branch (no tasks row exists).
    let brief = TaskBrief {
        task_id: Uuid::new_v4(),
        agent_id: owner.id(),
        job_id: Uuid::nil(),
        kind: crate::services::execution::TaskKind::Ingest,
        repo: Some(repo.to_string()),
        base_branch: base_branch.to_string(),
        branch: "flaborful/ingest".to_string(),
        task_text: match owner {
            VaultOwner::Agent(_) => documentation_brief(repo),
            VaultOwner::Company(_) => company_documentation_brief(repo),
        },
        instructions: None,
        persona_prompt,
        cf_finetune_id,
        github_token,
        // The professional's own Anthropic key, when they've added one. Sprite prefers it and
        // falls back to the server key, so onboarding still works before a key exists. Only an
        // Agent owner has one; a Company vault ingestion has no per-agent key (uses the server key).
        anthropic_api_key: match owner {
            VaultOwner::Agent(agent_id) => {
                crate::services::execution::agent_anthropic_key(db, agent_id).await
            }
            VaultOwner::Company(_) => None,
        },
        taste_token: token.clone(),
        api_base_url,
    };

    // The sink streams the agent's transcript into ingestion_events for the live onboarding feed.
    let sink = IngestionSink::new(db.clone(), owner, repo.to_string());
    sink.status(
        "running",
        &format!("Documenting {repo} (branch {base_branch})"),
    )
    .await;

    // Ingestion runs on Sprite. It happens the moment a professional first builds their vault —
    // before they've had any chance to add their own Anthropic key — and Managed Agents bills
    // strictly against the professional's own key with NO server fallback, so on Managed a fresh
    // onboarding run just skips every repo ("0 notes written"). Sprite falls back to the server's
    // ANTHROPIC_API_KEY (see sprite.rs), so the vault builds on the platform's dime during
    // onboarding; it's also cheaper for this documentation workload. (Sprite itself falls back to
    // Managed only if SPRITES_TOKEN isn't configured — see ExecutionBackends::get.)
    let backend = backends.get(ExecutionBackendKind::Sprite);
    let result = backend.run_implementation(&brief, &sink).await;
    let _ = crate::services::execution::taste::revoke_by_token(db, &token).await;

    match &result {
        Ok(_) => {
            // Report the authoritative vault count, not the event tally (which double-counts on
            // agent retries within a run).
            let count = sink.vault_note_count().await;
            sink.status_with_count(
                "done",
                &format!("Finished {repo} — {count} notes written"),
                count,
            )
            .await;
        }
        Err(e) => {
            sink.status("failed", &format!("Failed on {repo}: {e}"))
                .await;
        }
    }
    result?;
    Ok(())
}

/// The brief that turns a coding agent into a codebase documentarian writing into the vault.
fn documentation_brief(repo: &str) -> String {
    format!(
        "# Goal\n\
         Document the codebase `{repo}` into your knowledge vault. You are building a durable, \
         linked knowledge base of how this professional builds software — the same way a careful \
         engineer would write up a system in Obsidian. Do NOT change any code, commit, or push.\n\n\
         # Where the code is\n\
         The repository is already checked out for you at `/workspace/repo`. Start by running \
         `ls -la /workspace/repo` and exploring from there.\n\
         CRITICAL — if `/workspace/repo` contains only `.git` (no working files), the platform's \
         checkout FAILED. You must STOP immediately: reply with exactly `INGEST_ABORT: checkout \
         failed` and end your turn. Do NOT run `git fetch`, `git clone`, `git checkout`, `git \
         remote`, or any network/git command to try to recover — those will hang and waste the \
         session. Recovery is the platform's job, not yours.\n\n\
         # How\n\
         1. Explore the repo at `/workspace/repo`: read the source (not just the README) to understand its architecture, \
         major features/subsystems, the patterns the author reaches for, and the tech stack.\n\
         2. For each meaningful piece, call `upsert_note` to write a vault note:\n\
         - **feature** notes — one per major feature/subsystem: what it does and how it's built, \
         pointing at the key files/functions.\n\
         - **pattern** notes — recurring idioms/abstractions the author favors (e.g. a shared client \
         pattern), linked across the features that use them.\n\
         - **tech** notes — per technology/framework/infra, linking the features that use it.\n\
         - **golden_snippet** notes — a few exemplary function/class blocks that capture the author's \
         best work and style.\n\
         3. Use `related` (slugs) to wire notes together into a graph — link features to the patterns \
         and tech they use. Set `domain` (backend/frontend/infra/testing/product/general) and `repo`.\n\
         4. Write one **feature** note titled like a Map of Content for the repo that lists and links \
         the notes you created (slug e.g. `moc-{slug}`).\n\
         5. Use `list_vault` / `search_vault` first to avoid duplicating notes that already exist, and \
         to link new notes to related existing ones across the professional's other repos.\n\n\
         Keep notes concise and specific — describe and point at code (file + symbol), don't paste \
         whole files. When done, briefly summarize which notes you created.",
        repo = repo,
        slug = repo.rsplit('/').next().unwrap_or(repo).to_lowercase().replace([' ', '_'], "-"),
    )
}

/// The brief for documenting a COMPANY repo into the company's knowledge vault. Same structured,
/// linked, MOC'd output as the professional brief, but framed as the company's own codebase
/// knowledge graph — proprietary architecture and implementation detail are welcome here (these
/// are the seed *technical* notes, which may hold proprietary detail).
fn company_documentation_brief(repo: &str) -> String {
    format!(
        "# Goal\n\
         Document the company codebase `{repo}` into the company knowledge vault — a durable, \
         linked knowledge graph of how this system is built, the way a careful staff engineer \
         would write it up in Obsidian for the team. This is the company's OWN private knowledge \
         base, so capturing proprietary architecture, internal services, and implementation detail \
         is expected and welcome. Do NOT change any code, commit, or push.\n\n\
         # Where the code is\n\
         The repository is already checked out for you at `/workspace/repo`. Start by running \
         `ls -la /workspace/repo` and exploring from there.\n\
         CRITICAL — if `/workspace/repo` contains only `.git` (no working files), the platform's \
         checkout FAILED. You must STOP immediately: reply with exactly `INGEST_ABORT: checkout \
         failed` and end your turn. Do NOT run any network/git command to try to recover — those \
         will hang and waste the session. Recovery is the platform's job, not yours.\n\n\
         # How\n\
         1. Explore the repo at `/workspace/repo`: read the source (not just the README) to \
         understand its architecture, major features/subsystems, the patterns it uses, the tech \
         stack, and any proprietary/internal specifics a new contractor would need to know.\n\
         2. For each meaningful piece, call `upsert_note` to write a vault note:\n\
         - **feature** notes — one per major feature/subsystem: what it does and how it's built, \
         pointing at the key files/functions.\n\
         - **pattern** notes — recurring idioms/abstractions in this codebase, linked across the \
         features that use them.\n\
         - **tech** notes — per technology/framework/infra, linking the features that use it.\n\
         - **golden_snippet** notes — a few exemplary blocks that capture how this team writes code.\n\
         3. Use `related` (slugs) to wire notes into a graph — link features to the patterns and \
         tech they use. Set `domain` (backend/frontend/infra/testing/product/general) and `repo`.\n\
         4. Write one **feature** note as a Map of Content for the repo, linking the notes you \
         created (slug e.g. `moc-{slug}`).\n\
         5. Use `list_vault` / `search_vault` first to avoid duplicates and to link new notes to \
         existing ones across the company's other repos.\n\n\
         Keep notes concise and specific — describe and point at code (file + symbol), don't paste \
         whole files. When done, briefly summarize which notes you created.",
        repo = repo,
        slug = repo.rsplit('/').next().unwrap_or(repo).to_lowercase().replace([' ', '_'], "-"),
    )
}
