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

        let name = format!("flaborful-{}", &brief.task_id.simple().to_string()[..16]);
        sink.push(&format!("[sprite] creating {}\n", name)).await;
        let handle = self.client.create(&name).await?;

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
            // Record the clone HEAD for rerun reconciliation.
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

        Ok((handle, provisioning))
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

        // Drive the agent headless. ANTHROPIC_API_KEY is needed for the coding model; pass it
        // through from the backend env, alongside the company's test-env env (cloud creds, etc.).
        let anthropic_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
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
            let _ = client.destroy(&name).await;
        }
        let _ = sqlx::query!(
            "UPDATE task_sprites SET status = 'destroyed', destroyed_at = NOW() WHERE task_id = $1",
            task_id
        )
        .execute(db)
        .await;
    }
}
