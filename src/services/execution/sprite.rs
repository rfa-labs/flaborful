//! Sprite backend — the agent's work runs on a self-hosted Fly.io Sprite (a Firecracker VM).
//!
//! Unlike Managed Agents, here WE own the box: we create a sprite per task, provision it for the
//! job's test-env strategy, clone the repo (owner OAuth token; commits authored as the
//! professional), install + launch the professional's chosen coding-agent runtime headless
//! (Claude Code in v1), and read back the diff. The runtime calls the shared taste tool via its
//! MCP config.
//!
//! Disabled gracefully (skips) when SPRITES_TOKEN is unset, mirroring the SANDBOX_URL pattern.

use anyhow::{anyhow, Result};
use sqlx::PgPool;

use super::runtime::{
    claude_code::ClaudeCodeRuntime, codex::CodexRuntime, custom::CustomRuntime, AgentRuntime,
};
use super::sprite_client::{ExecOutput, SpriteClient, SpriteHandle};
use super::sprite_env::{self, SpriteProvisioning};
use super::{AgentRuntimeKind, ExecResult, ExecutionBackend, TaskBrief, TranscriptSink};

const REPO_DIR: &str = "/workspace/repo";

pub struct SpriteBackend {
    db: PgPool,
    client: SpriteClient,
}

impl SpriteBackend {
    pub fn from_env(db: PgPool) -> Self {
        Self {
            db,
            client: SpriteClient::from_env(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.client.enabled()
    }

    /// Resolve the runtime adapter for an agent's configured runtime. `runtime_spec` drives the
    /// Custom adapter (install/MCP/invoke templates); it's ignored by the built-in runtimes.
    fn runtime_for(
        kind: AgentRuntimeKind,
        runtime_spec: Option<serde_json::Value>,
    ) -> Box<dyn AgentRuntime> {
        match kind {
            AgentRuntimeKind::ClaudeCode => Box::new(ClaudeCodeRuntime),
            AgentRuntimeKind::Codex => Box::new(CodexRuntime),
            AgentRuntimeKind::Custom => Box::new(CustomRuntime::new(
                runtime_spec.unwrap_or(serde_json::Value::Null),
            )),
        }
    }

    /// Get-or-create the task's sprite, provision it per the company's test-env strategy, clone
    /// the repo. Returns the handle plus the resolved provisioning (its env must be exported into
    /// every subsequent command). Setup commands run only on first creation, not on resume.
    async fn ensure_sprite(
        &self,
        brief: &TaskBrief,
        sink: &dyn TranscriptSink,
    ) -> Result<(SpriteHandle, SpriteProvisioning)> {
        // The company's test-env strategy is needed on both create and resume (env exports
        // apply to every command). Setup/egress are applied only on a fresh sprite below.
        let provisioning = sprite_env::for_job(&self.db, brief.job_id).await;

        // Ingestion runs are EPHEMERAL: a throwaway sprite per repo with a synthetic task_id that
        // has NO `tasks` row, so we must NOT touch `task_sprites` (its task_id FK references
        // tasks(id) — inserting would violate the constraint). Ingestion also never resumes and
        // never runs follow-up Sprite-only nodes, so the persisted row would serve no purpose. The
        // sprite is created fresh here and destroyed by run_implementation when the run finishes.
        let ephemeral = matches!(brief.kind, super::TaskKind::Ingest);

        if !ephemeral {
            // Existing row? Resume if the sprite still exists.
            let row = sqlx::query!(
                "SELECT sprite_name, status FROM task_sprites WHERE task_id = $1",
                brief.task_id
            )
            .fetch_optional(&self.db)
            .await?;

            if let Some(r) = row {
                if let Some(name) = r.sprite_name {
                    if r.status != "destroyed" && r.status != "gone" {
                        if let Ok(h) = self.client.get(&name).await {
                            return Ok((h, provisioning));
                        }
                        // Reaped — mark gone and recreate below.
                        let _ = sqlx::query!(
                            "UPDATE task_sprites SET status = 'gone' WHERE task_id = $1",
                            brief.task_id
                        )
                        .execute(&self.db)
                        .await;
                    }
                }
            }
        }

        let name = format!("flaborful-{}", &brief.task_id.simple().to_string()[..16]);
        sink.push(&format!("[sprite] creating {}\n", name)).await;
        // A create can report failure yet still leave a live sprite behind: with
        // `wait_for_capacity`, the Sprites API provisions the VM and only then errors (a late 500, a
        // timed-out long-poll, or an undeserializable success body). The VM exists but we never got
        // a handle, so nothing reaps it — that's how "create failed" and orphaned cold sprites are
        // both true. Since the name is deterministic (derived from task_id, identical to what create
        // attempted), reap by that name on failure to kill any sprite that came up despite the
        // error, then propagate the original failure.
        let handle = match self.client.create(&name).await {
            Ok(h) => h,
            Err(e) => {
                sink.push("[sprite] create failed — reaping any orphaned sprite\n")
                    .await;
                destroy_sprite_hard(&self.client, &name).await;
                return Err(e);
            }
        };

        // From here on the sprite EXISTS. For ephemeral runs, any failure below must destroy it or
        // it leaks (no task_sprites row means external teardown can't find it). Provision in a
        // helper and clean up on error before propagating. Ephemeral runs skip the task_sprites
        // row entirely (their synthetic task_id has no `tasks` row to reference).
        if ephemeral {
            if let Err(e) = self
                .provision_sprite(brief, sink, &handle, &provisioning)
                .await
            {
                destroy_sprite_hard(&self.client, &handle.name).await;
                return Err(e);
            }
            return Ok((handle, provisioning));
        }

        // Persist the sprite for real tasks (resume + follow-up nodes + teardown).
        sqlx::query!(
            r#"INSERT INTO task_sprites (task_id, sprite_name, sprite_url, status, repo, branch, test_env_strategy)
               VALUES ($1, $2, $3, 'running', $4, $5, $6)
               ON CONFLICT (task_id) DO UPDATE SET
                 sprite_name = EXCLUDED.sprite_name, sprite_url = EXCLUDED.sprite_url,
                 status = 'running', repo = EXCLUDED.repo, branch = EXCLUDED.branch,
                 test_env_strategy = EXCLUDED.test_env_strategy"#,
            brief.task_id,
            handle.name,
            handle.url,
            brief.repo,
            brief.branch,
            provisioning.strategy.as_str(),
        )
        .execute(&self.db)
        .await?;

        self.provision_sprite(brief, sink, &handle, &provisioning)
            .await?;

        Ok((handle, provisioning))
    }

    /// Provision a freshly-created sprite: egress allowlist, repo clone, and the test-env setup
    /// commands. Shared by real-task and ephemeral (ingestion) runs. For real tasks it also records
    /// the clone HEAD into `task_sprites` for rerun reconciliation; ephemeral runs skip that (no
    /// row, never rerun).
    async fn provision_sprite(
        &self,
        brief: &TaskBrief,
        sink: &dyn TranscriptSink,
        handle: &SpriteHandle,
        provisioning: &SpriteProvisioning,
    ) -> Result<()> {
        let ephemeral = matches!(brief.kind, super::TaskKind::Ingest);

        // Apply the egress allowlist before anything reaches the network (egress strategy).
        // HARD RULE: never apply a network policy for a VPN (`network`) job — Fly's egress proxy
        // blocks all UDP when a policy is active, which would kill the WireGuard/OpenVPN tunnel.
        if provisioning.strategy == sprite_env::TestEnvStrategy::Network {
            if !provisioning.egress_allowlist.is_empty() {
                tracing::warn!(
                    "ignoring egress allowlist for VPN job {} (would block UDP / break the tunnel)",
                    handle.name
                );
            }
        } else if !provisioning.egress_allowlist.is_empty() {
            sink.push(&format!(
                "[sprite] restricting egress to {} host(s)\n",
                provisioning.egress_allowlist.len()
            ))
            .await;
            if let Err(e) = self
                .client
                .set_egress(&handle.name, &provisioning.egress_allowlist)
                .await
            {
                tracing::warn!("egress allowlist for {} failed: {}", handle.name, e);
            }
        }

        // Clone the repo (token in the remote URL; commits authored as the professional).
        // A coding task MUST have both a repo and a token. Failing loudly here beats the old
        // silent skip, which left the agent running in a nonexistent /workspace/repo (every
        // command then failed with `chdir to /workspace/repo: No such file or directory`).
        match (&brief.repo, &brief.github_token) {
            (None, _) => {
                return Err(anyhow!(
                    "AgentImplement: no repo set for this task — set a target repo on the task"
                ));
            }
            (Some(_), None) => {
                return Err(anyhow!(
                    "AgentImplement: no GitHub token — grant the GitHub tool on the job (Integrations \
                     tab) so the agent can clone and push, then re-run"
                ));
            }
            _ => {}
        }
        if let (Some(repo), Some(token)) = (&brief.repo, &brief.github_token) {
            let repo_path = if repo.starts_with("http") {
                repo.trim_start_matches("https://").to_string()
            } else {
                format!("github.com/{}", repo)
            };
            let clone = format!(
                "rm -rf {dir} && git clone https://x-access-token:{token}@{repo} {dir} && \
                 cd {dir} && git checkout -b {branch} && \
                 git config user.name 'Flaborful Agent' && git config user.email 'agent@flaborful.com'",
                dir = REPO_DIR, token = token, repo = repo_path, branch = brief.branch,
            );
            let out = self
                .client
                .exec(&handle.name, &clone, Some("/"), &[], 300)
                .await?;
            if out.exit_code != 0 {
                return Err(anyhow!("git clone failed: {}", out.stderr));
            }
            sink.push("[sprite] repo cloned\n").await;
            // Record the clone HEAD for rerun reconciliation (real tasks only — ephemeral ingestion
            // runs have no task_sprites row and never rerun).
            if !ephemeral {
                if let Ok(sha) = self
                    .client
                    .exec(&handle.name, "git rev-parse HEAD", Some(REPO_DIR), &[], 30)
                    .await
                {
                    let _ = sqlx::query!(
                        "UPDATE task_sprites SET head_sha = $1 WHERE task_id = $2",
                        sha.stdout.trim(),
                        brief.task_id
                    )
                    .execute(&self.db)
                    .await;
                }
            }
        }

        // Run the strategy's setup commands (e.g. `docker compose up -d`) once, after clone.
        for cmd in &provisioning.setup_cmds {
            sink.push(&format!("[sprite] env setup: {}\n", cmd)).await;
            let out = self
                .client
                .exec(&handle.name, cmd, Some(REPO_DIR), &provisioning.env, 600)
                .await?;
            if out.exit_code != 0 {
                return Err(anyhow!(
                    "test-env setup failed ({}): {}",
                    provisioning.strategy.as_str(),
                    out.stderr
                ));
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl ExecutionBackend for SpriteBackend {
    async fn run_implementation(
        &self,
        brief: &TaskBrief,
        sink: &dyn TranscriptSink,
    ) -> Result<ExecResult> {
        if !self.client.enabled() {
            sink.push("[sprite] SPRITES_TOKEN not set — skipping execution\n")
                .await;
            return Ok(ExecResult {
                status: "skipped".into(),
                ..Default::default()
            });
        }

        // Resolve the agent's runtime preference + custom spec.
        let agent_row = sqlx::query!(
            "SELECT agent_runtime, runtime_spec FROM agents WHERE id = $1",
            brief.agent_id,
        )
        .fetch_optional(&self.db)
        .await?;
        let runtime_kind = agent_row
            .as_ref()
            .and_then(|r| match r.agent_runtime.as_str() {
                "claude_code" => Some(AgentRuntimeKind::ClaudeCode),
                "codex" => Some(AgentRuntimeKind::Codex),
                "custom" => Some(AgentRuntimeKind::Custom),
                _ => None,
            })
            .unwrap_or_default();
        let runtime_spec = agent_row.and_then(|r| r.runtime_spec);
        let runtime = Self::runtime_for(runtime_kind, runtime_spec);

        let (handle, provisioning) = self.ensure_sprite(brief, sink).await?;

        // Ephemeral (ingestion) sprites have no task_sprites row, so external teardown can't reach
        // them — we own their whole lifecycle. Run the body, then destroy the sprite regardless of
        // outcome. A "re-do from scratch" therefore always starts a brand-new sprite (nothing
        // persists to resume), which is the correct semantics for ingestion.
        let ephemeral = matches!(brief.kind, super::TaskKind::Ingest);
        let result = self
            .run_on_sprite(brief, sink, &handle, &provisioning, runtime.as_ref())
            .await;
        if ephemeral {
            sink.push("[sprite] tearing down ephemeral sprite\n").await;
            destroy_sprite_hard(&self.client, &handle.name).await;
        }
        result
    }
}

impl SpriteBackend {
    /// Drive the agent on an already-provisioned sprite: install the runtime, write its MCP config,
    /// run it headless, and read back the diff/summary. Separated from `run_implementation` so the
    /// caller can guarantee teardown of ephemeral sprites around this whole body.
    async fn run_on_sprite(
        &self,
        brief: &TaskBrief,
        sink: &dyn TranscriptSink,
        handle: &SpriteHandle,
        provisioning: &SpriteProvisioning,
        runtime: &dyn AgentRuntime,
    ) -> Result<ExecResult> {
        // Install the runtime + write its MCP config pointing at the shared taste endpoint.
        for cmd in runtime.install_spec().install_cmds {
            let _ = self
                .client
                .exec(&handle.name, &cmd, Some("/"), &[], 300)
                .await;
        }
        let mcp = runtime.mcp_config(&brief.taste_url(), &brief.vault_url());
        let write_cfg = format!(
            "mkdir -p $(dirname {p}) && cat > {p} <<'FLABEOF'\n{c}\nFLABEOF",
            p = mcp.path,
            c = mcp.contents
        );
        let _ = self
            .client
            .exec(&handle.name, &write_cfg, Some("/"), &[], 60)
            .await;

        // The MCP config is platform infrastructure (contains our taste endpoint + per-task token).
        // It lives inside the repo so the agent runtime can read it, but it must NOT end up in the
        // diff/commit/PR. Add its repo-relative path to .git/info/exclude (a LOCAL ignore that
        // isn't itself tracked), so the later `git add -A` skips it. Also untrack it if a previous
        // run already staged it.
        if let Some(rel) = mcp.path.strip_prefix(&format!("{}/", REPO_DIR)) {
            // rel is a fixed config filename (e.g. ".mcp.json") — single-quote it for the shell.
            let rel_q = format!("'{}'", rel.replace('\'', "'\\''"));
            let exclude = format!(
                "printf '%s\\n' {rel} >> .git/info/exclude && git rm --cached --quiet {rel} 2>/dev/null || true",
                rel = rel_q,
            );
            let _ = self
                .client
                .exec(&handle.name, &exclude, Some(REPO_DIR), &[], 30)
                .await;
        }

        // Drive the agent headless. ANTHROPIC_API_KEY is needed for the coding model. Prefer the
        // professional's own key; fall back to the server key (the premium "run on ours" path).
        // Injected alongside the company's test-env env (cloud creds, etc.).
        let anthropic_key = brief
            .anthropic_api_key
            .as_deref()
            .filter(|k| !k.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| std::env::var("ANTHROPIC_API_KEY").unwrap_or_default());
        let mut env = vec![("ANTHROPIC_API_KEY".to_string(), anthropic_key)];
        env.extend(provisioning.env.iter().cloned());
        let cmd = runtime.invoke_command(REPO_DIR, brief);
        sink.push("[sprite] running coding agent…\n").await;
        // Stream the agent's transcript into the node logs live (the run can take many minutes).
        let run = self
            .client
            .exec_streaming(
                &handle.name,
                &cmd,
                Some(REPO_DIR),
                &env,
                60 * 30,
                Some(sink),
            )
            .await?;

        // Collect the diff + changed files + head sha.
        let diff = self
            .client
            .exec(
                &handle.name,
                "git add -A && git diff --cached",
                Some(REPO_DIR),
                &[],
                60,
            )
            .await
            .map(|o| o.stdout)
            .unwrap_or_default();
        let files = self
            .client
            .exec(
                &handle.name,
                "git diff --cached --name-only",
                Some(REPO_DIR),
                &[],
                30,
            )
            .await
            .map(|o| o.stdout)
            .unwrap_or_default();
        let head_sha = self
            .client
            .exec(&handle.name, "git rev-parse HEAD", Some(REPO_DIR), &[], 30)
            .await
            .ok()
            .map(|o| o.stdout.trim().to_string());

        // The agent's own end-of-loop summary, parsed from the transcript (Claude Code emits a
        // delimited block; other runtimes return None).
        let summary = runtime.parse_summary(&run.stdout);

        Ok(ExecResult {
            files_changed: files
                .lines()
                .map(str::to_string)
                .filter(|s| !s.is_empty())
                .collect(),
            diff,
            transcript: run.stdout,
            summary,
            test_summary: None,
            head_sha,
            status: "completed".into(),
        })
    }
}

/// Run a single command in a task's sprite (used by Sprite-only nodes: SpriteTest, GitCommit,
/// GitPush in later phases). Returns None if the task has no live sprite.
pub async fn exec_in_task_sprite(
    db: &PgPool,
    client: &SpriteClient,
    task_id: uuid::Uuid,
    command: &str,
    timeout_secs: u64,
) -> Result<Option<ExecOutput>> {
    let row = sqlx::query!(
        "SELECT sprite_name FROM task_sprites WHERE task_id = $1 AND status NOT IN ('destroyed','gone')",
        task_id
    )
    .fetch_optional(db)
    .await?;
    let Some(name) = row.and_then(|r| r.sprite_name) else {
        return Ok(None);
    };
    Ok(Some(
        client
            .exec(&name, command, Some(REPO_DIR), &[], timeout_secs)
            .await?,
    ))
}

/// Destroy every live sprite belonging to a job's tasks (safety net for complete/kill).
pub async fn destroy_job_sprites(db: &PgPool, client: &SpriteClient, job_id: uuid::Uuid) {
    let rows = sqlx::query!(
        r#"SELECT ts.task_id FROM task_sprites ts
           JOIN tasks t ON t.id = ts.task_id
           WHERE t.job_id = $1 AND ts.status NOT IN ('destroyed','gone')"#,
        job_id
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    for r in rows {
        destroy_task_sprite(db, client, r.task_id).await;
    }
}

/// Destroy a sprite with bounded retries, and LOUDLY log if it ultimately fails.
///
/// A single `DELETE /sprites/{name}` is flaky — the Sprites API 500s/times out under load — and a
/// swallowed failure permanently leaks the VM (it keeps running and billing, and there is no
/// task_sprites row for ephemeral ingestion sprites, so nothing ever reaps it). We retry a few
/// times with backoff; if every attempt fails we emit an error naming the sprite so an operator can
/// delete it manually and knows a leak occurred (a `let _ = destroy()` hid both facts).
async fn destroy_sprite_hard(client: &SpriteClient, name: &str) {
    const ATTEMPTS: usize = 4;
    for attempt in 1..=ATTEMPTS {
        match client.destroy(name).await {
            Ok(()) => return,
            Err(e) => {
                if attempt == ATTEMPTS {
                    tracing::error!(
                        "LEAKED SPRITE: failed to destroy {name} after {ATTEMPTS} attempts \
                         ({e:?}) — it is still running and must be deleted manually"
                    );
                } else {
                    tracing::warn!(
                        "destroy {name} attempt {attempt}/{ATTEMPTS} failed ({e:?}); retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                }
            }
        }
    }
}

/// Destroy a task's sprite (best-effort), called on task terminal + job complete/kill.
pub async fn destroy_task_sprite(db: &PgPool, client: &SpriteClient, task_id: uuid::Uuid) {
    if let Ok(Some(row)) = sqlx::query!(
        "SELECT sprite_name FROM task_sprites WHERE task_id = $1 AND status NOT IN ('destroyed','gone')",
        task_id
    )
    .fetch_optional(db)
    .await
    {
        if let Some(name) = row.sprite_name {
            destroy_sprite_hard(client, &name).await;
        }
        let _ = sqlx::query!(
            "UPDATE task_sprites SET status = 'destroyed', destroyed_at = NOW() WHERE task_id = $1",
            task_id
        )
        .execute(db)
        .await;
    }
}
