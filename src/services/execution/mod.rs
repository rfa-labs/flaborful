//! Pluggable work-execution backends.
//!
//! The agent's *intelligence* runs on Cloudflare Workers AI (the professional's LoRA) via the
//! LlmReasoning nodes. This module is the *hands + workbench*: where a real coding agent
//! actually clones a repo, edits, tests, and produces a diff. It is deliberately named
//! "work-execution" rather than "coding" — software engineering is the first concretion, but
//! the abstraction is profession-agnostic (a Sprite is general compute).
//!
//! Two backends implement the same `ExecutionBackend` trait:
//!   - `ManagedAgentsBackend` — Anthropic-hosted container (least setup, repo token kept off
//!     the VM by Anthropic's git proxy).
//!   - `SpriteBackend` — self-hosted Fly.io Firecracker VM where we install the professional's
//!     chosen coding agent runtime (Claude Code / Codex / custom).
//!
//! The professional's LoRA is exposed to whichever agent runs as a single shared "taste tool"
//! (`consult_principal`) — see `taste.rs`. There is one taste handler; backends differ only in
//! how they register/route it, not in its logic.

pub mod managed_agents;
pub mod runtime;
pub mod sprite;
pub mod sprite_client;
pub mod sprite_env;
pub mod taste;

use anyhow::Result;
use qdrant_client::Qdrant;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::services::inference::InferenceClient;

/// Registry of available execution backends, resolved by kind at task time.
#[derive(Clone)]
pub struct ExecutionBackends {
    managed: Arc<managed_agents::ManagedAgentsBackend>,
    sprite: Arc<sprite::SpriteBackend>,
    /// Shared Sprites client for Sprite-only nodes (SpriteTest, GitCommit, GitPush, destroy).
    pub sprite_client: sprite_client::SpriteClient,
}

impl ExecutionBackends {
    pub fn from_env(db: PgPool, qdrant: Arc<Qdrant>, inference: InferenceClient) -> Self {
        Self {
            managed: Arc::new(managed_agents::ManagedAgentsBackend::from_env(
                db.clone(),
                qdrant,
                inference,
            )),
            sprite: Arc::new(sprite::SpriteBackend::from_env(db)),
            sprite_client: sprite_client::SpriteClient::from_env(),
        }
    }

    /// Delete orphaned managed-agents sessions/environments left behind by crashes or aborted
    /// runs (cost cleanup). Safe to call at startup; leaves running sessions and the one shared
    /// environment intact.
    pub async fn reap_orphans(&self) {
        self.managed.reap_orphans().await;
    }

    /// Force-stop all managed-agents runs immediately (interrupt + delete running sessions, delete
    /// envs). Returns how many sessions it acted on. Used to cancel ingestion.
    pub async fn kill_managed_runs(&self) -> usize {
        self.managed.kill_managed_runs().await
    }

    /// Resolve the backend for a kind. If Sprite is selected but SPRITES_TOKEN isn't configured,
    /// fall back to Managed so workflows don't hard-fail.
    pub fn get(&self, kind: ExecutionBackendKind) -> Arc<dyn ExecutionBackend> {
        match kind {
            ExecutionBackendKind::Managed => self.managed.clone(),
            ExecutionBackendKind::Sprite if self.sprite.enabled() => self.sprite.clone(),
            ExecutionBackendKind::Sprite => self.managed.clone(),
        }
    }
}

/// Fetch and decrypt the professional's own Anthropic API key for an agent, for use in a
/// `TaskBrief`. Returns `None` when no key is on file. A decrypt failure is logged and treated as
/// "no key" rather than failing the run (a bad key on file should surface as the Managed
/// "add your key" skip, not a hard error).
pub async fn agent_anthropic_key(db: &PgPool, agent_id: Uuid) -> Option<String> {
    let enc = sqlx::query_scalar!(
        "SELECT anthropic_api_key_enc FROM agents WHERE id = $1",
        agent_id
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten()?;
    match crate::services::crypto::decrypt(&enc) {
        Ok(key) if !key.trim().is_empty() => Some(key),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(
                "failed to decrypt Anthropic key for agent {}: {}",
                agent_id,
                e
            );
            None
        }
    }
}

/// The optimizer-edge credentials a fbx-agent run uses. Resolved from the agent's own key when on
/// file, else the server key; the base URL comes from server config. `provider_key` stays `None`
/// so the optimizer resolves the upstream provider key server/tenant-side (the onboarding /
/// BYOK-held-tenant-side path), which keeps the sprite from needing a custom header.
#[derive(Debug, Clone, Default)]
pub struct OptimizerCreds {
    pub base_url: Option<String>,
    pub key: Option<String>,
    pub provider_key: Option<String>,
}

/// Resolve the optimizer creds for an agent's fbx-agent run. Base URL from `OPTIMIZER_BASE_URL`;
/// key from the agent's encrypted `optimizer_api_key_enc`, else the server `OPTIMIZER_API_KEY`.
pub async fn agent_optimizer_creds(db: &PgPool, agent_id: Uuid) -> OptimizerCreds {
    // Mirrors `agent_anthropic_key`: bind the decrypted key to a concrete `String` (so the offline
    // sqlx build has an unambiguous type — a chained `.and_then(|enc| decrypt(&enc))` fails under
    // SQLX_OFFLINE with an unsized-`str` error).
    let agent_key = decrypt_agent_optimizer_key(db, agent_id).await;

    OptimizerCreds {
        base_url: std::env::var("OPTIMIZER_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty()),
        key: agent_key.or_else(|| {
            std::env::var("OPTIMIZER_API_KEY")
                .ok()
                .filter(|s| !s.is_empty())
        }),
        provider_key: None,
    }
}

/// Fetch + decrypt an agent's own optimizer key, or `None` if not on file / undecryptable. Kept as
/// its own fn (with an explicit `String` binding) so the `query_scalar!` type is unambiguous under
/// `SQLX_OFFLINE` — the same shape as `agent_anthropic_key`.
async fn decrypt_agent_optimizer_key(db: &PgPool, agent_id: Uuid) -> Option<String> {
    let enc: String = sqlx::query_scalar!(
        "SELECT optimizer_api_key_enc FROM agents WHERE id = $1",
        agent_id
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten()?;
    match crate::services::crypto::decrypt(&enc) {
        Ok(k) if !k.trim().is_empty() => Some(k),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(
                "failed to decrypt optimizer key for agent {}: {}",
                agent_id,
                e
            );
            None
        }
    }
}

/// Server-level optimizer creds (env only, no per-agent key). For runs with no owning agent —
/// e.g. a company-vault ingestion — that still route through the optimizer on the platform's key.
pub fn server_optimizer_creds() -> OptimizerCreds {
    OptimizerCreds {
        base_url: std::env::var("OPTIMIZER_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty()),
        key: std::env::var("OPTIMIZER_API_KEY")
            .ok()
            .filter(|s| !s.is_empty()),
        provider_key: None,
    }
}

/// Which backend runs a task's work, and (for Sprite) which agent runtime drives it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackendKind {
    /// Anthropic-hosted Managed Agents container.
    Managed,
    /// Self-hosted Fly.io Sprite.
    Sprite,
}

impl Default for ExecutionBackendKind {
    fn default() -> Self {
        ExecutionBackendKind::Managed
    }
}

impl ExecutionBackendKind {
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "managed" => Some(Self::Managed),
            "sprite" => Some(Self::Sprite),
            _ => None,
        }
    }
}

/// The coding-agent runtime, used only by the Sprite backend.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeKind {
    ClaudeCode,
    Codex,
    Custom,
    /// flaborful's own DAG-aware coding orchestrator (`fbx-agent`), which routes every LLM call
    /// through the optimizer edge. Serializes as `fbx_agent`.
    FbxAgent,
}

impl Default for AgentRuntimeKind {
    fn default() -> Self {
        // The platform default coding agent is fbx-agent — flaborful's own DAG-aware orchestrator,
        // routing every LLM call through the optimizer (the cheap, first-class path). Claude Code /
        // Codex are opt-in alternatives.
        AgentRuntimeKind::FbxAgent
    }
}

impl AgentRuntimeKind {
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "claude_code" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            "custom" => Some(Self::Custom),
            "fbx_agent" => Some(Self::FbxAgent),
            _ => None,
        }
    }
}

/// A user-facing **execution environment** for a SWE workflow node — the single choice a workflow
/// author makes that bundles "where the work runs" (backend) with "which coding agent drives it"
/// (runtime). Resolved node-override → workflow-default → agent-default, and mapped to the concrete
/// `(backend, runtime)` the engine already understands. Keeps every path a first-class option:
/// the premium managed cloud, strict-BYOK Claude Code, and the cheap optimizer-routed fbx-agent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEnvironment {
    /// Anthropic-hosted Managed Agents (their key). The premium path.
    ManagedCloud,
    /// Sprite + Claude Code, strictly on the professional's Anthropic key (no optimizer routing).
    ClaudeCodeByok,
    /// Sprite + fbx-agent, routing every LLM call through the optimizer edge (the cheap default).
    OptimizerFbx,
}

impl ExecutionEnvironment {
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "managed_cloud" => Some(Self::ManagedCloud),
            "claude_code_byok" => Some(Self::ClaudeCodeByok),
            "optimizer_fbx" => Some(Self::OptimizerFbx),
            _ => None,
        }
    }

    /// The concrete backend + (Sprite-only) runtime this environment maps to. The managed path has
    /// no coding-agent runtime (Anthropic's own agent drives it), hence `None`.
    pub fn resolve(self) -> (ExecutionBackendKind, Option<AgentRuntimeKind>) {
        match self {
            Self::ManagedCloud => (ExecutionBackendKind::Managed, None),
            Self::ClaudeCodeByok => (
                ExecutionBackendKind::Sprite,
                Some(AgentRuntimeKind::ClaudeCode),
            ),
            Self::OptimizerFbx => (
                ExecutionBackendKind::Sprite,
                Some(AgentRuntimeKind::FbxAgent),
            ),
        }
    }
}

/// What kind of run this is. Drives the agent's system-prompt framing: an Implement run's
/// deliverable is a code diff (edit, test, summarize); an Ingest run's deliverable is vault notes
/// (read-only documentation — it must NOT edit code, and writing notes is the whole point).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskKind {
    #[default]
    Implement,
    Ingest,
}

/// Everything a backend needs to run one unit of work-execution. Profession-agnostic in shape;
/// the SWE concretion fills it with a repo + a coding task.
#[derive(Debug, Clone)]
pub struct TaskBrief {
    pub task_id: Uuid,
    pub agent_id: Uuid,
    pub job_id: Uuid,
    /// Which kind of run — shapes the system prompt (code-change vs. documentation).
    pub kind: TaskKind,
    /// "owner/repo" or full URL; the backend resolves the clone URL + token.
    pub repo: Option<String>,
    pub base_branch: String,
    /// The branch the agent works on / will push.
    pub branch: String,
    /// The natural-language task the agent should accomplish.
    pub task_text: String,
    /// The professional's standing instructions for HOW the agent should run its loop — consult
    /// the principal on decisions, write + run unit tests, commit-message style, etc. Distinct
    /// from `task_text` (the WHAT). None when the workflow author left it blank.
    pub instructions: Option<String>,
    /// The professional's persona/system prompt, shaping the agent's framing + review.
    pub persona_prompt: String,
    /// The Cloudflare finetune id for the professional's LoRA (drives the taste tool).
    pub cf_finetune_id: Option<String>,
    /// GitHub access token (owner OAuth) for clone/push. For Managed Agents this rides the
    /// git proxy and never enters the container.
    pub github_token: Option<String>,
    /// The professional's own (decrypted) Anthropic/Claude API key. Required by the Managed
    /// backend (no fallback — the run bills to the professional). The Sprite backend prefers it
    /// and falls back to the server's `ANTHROPIC_API_KEY`. `None`/empty => not on file.
    pub anthropic_api_key: Option<String>,
    /// Per-task token authenticating the agent's calls back to the shared taste endpoint.
    pub taste_token: String,
    /// Base URL of this backend (for building the taste callback URL).
    pub api_base_url: String,
    /// The concrete model id the runtime should drive the coding agent with. `None` => the
    /// runtime picks its per-`TaskKind` default (see `default_model`). Lets a high-volume,
    /// lower-stakes documentation (`Ingest`) run use a cheaper model than a hard `Implement`
    /// run, instead of every run hardcoding a frontier model.
    pub model: Option<String>,
    /// Optimizer edge base URL for the fbx-agent runtime (e.g. `https://optimizer…/v1`). When set,
    /// fbx-agent routes every LLM call through the optimizer (route/cache/quality-gate/meter).
    /// `None` => not configured (the fbx runtime falls back to the server env `OPTIMIZER_BASE_URL`).
    pub optimizer_base_url: Option<String>,
    /// The optimizer API key (`sk-opt-…`) authenticating the tenant at the edge. Prefer the
    /// professional's own; fall back to the server key. Only used by the fbx-agent runtime.
    pub optimizer_key: Option<String>,
    /// Optional upstream provider key passed through in `x-provider-key`. `None` when the optimizer
    /// resolves the provider key server/tenant-side (the onboarding / BYOK-held-tenant-side path).
    pub provider_key: Option<String>,
    /// Upstream workflow inputs for this node's step (the DAG blackboard vars the fbx-agent runtime
    /// injects with the task). `Null` when there are none. Only the fbx-agent runtime consumes this.
    pub vars: serde_json::Value,
    /// Per-node coding-agent runtime override (Sprite backend). When `Some`, the Sprite backend uses
    /// this instead of the agent-row default — so a workflow can pick the execution environment
    /// per node. `None` => fall back to the agent's configured `agent_runtime`.
    pub runtime_override: Option<AgentRuntimeKind>,
}

/// The default coding model for a run kind, when the brief doesn't pin one. Frontier for
/// `Implement` (hard multi-step SWE), cheap for `Ingest` (read-only documentation into the
/// vault — high volume, quality-tolerant). Both are env-overridable so ops can tune the
/// model without a redeploy, mirroring `ORCHESTRATOR_MODEL`/`WORKER_MODEL` in `inference.rs`.
pub fn default_model(kind: TaskKind) -> String {
    match kind {
        TaskKind::Implement => {
            std::env::var("FLABORFUL_MODEL_IMPLEMENT").unwrap_or_else(|_| "claude-opus-4-8".into())
        }
        TaskKind::Ingest => {
            std::env::var("FLABORFUL_MODEL_INGEST").unwrap_or_else(|_| "claude-haiku-4-5".into())
        }
    }
}

impl TaskBrief {
    /// The URL the agent's MCP/custom taste tool calls to reach `consult_principal`.
    pub fn taste_url(&self) -> String {
        format!(
            "{}/api/v1/mcp/taste/{}",
            self.api_base_url.trim_end_matches('/'),
            self.taste_token
        )
    }

    /// The URL the agent's MCP vault tools call to browse/read/write the professional's vault.
    /// Reuses the same per-task token (it binds to the same agent), so one token covers both
    /// the `principal` and `vault` MCP servers.
    pub fn vault_url(&self) -> String {
        format!(
            "{}/api/v1/mcp/vault/{}",
            self.api_base_url.trim_end_matches('/'),
            self.taste_token
        )
    }
}

/// The result of a work-execution run.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecResult {
    /// Unified diff of the changes the agent made.
    pub diff: String,
    pub files_changed: Vec<String>,
    /// The agent's full transcript (streamed into node_executions.logs during the run).
    pub transcript: String,
    /// The agent's own end-of-loop summary of what it changed and why (parsed from the delimited
    /// block it emits). Shown to the professional at the Human Gate and used as the PR body.
    pub summary: Option<String>,
    /// A short summary of any tests the agent ran.
    pub test_summary: Option<String>,
    /// The repo HEAD sha after the agent's work (for rerun reconciliation on Sprite).
    pub head_sha: Option<String>,
    /// completed | failed
    pub status: String,
    /// Optimizer cost/savings for this run (only the fbx-agent runtime emits it — every LLM call
    /// went through the edge). `{ baseline, actual, served, exploration, saved, cache_hits, llm_calls }`.
    /// `None` for runtimes that don't route through the optimizer. Surfaced on the node/run in the UI.
    #[serde(default)]
    pub cost: Option<serde_json::Value>,
}

/// A sink the backend writes transcript chunks to as the agent works, so the DAG inspector can
/// show progress live. Implemented over the existing node_executions.logs persistence.
#[async_trait::async_trait]
pub trait TranscriptSink: Send + Sync {
    async fn push(&self, chunk: &str);
}

/// A no-op sink (for contexts that don't stream).
#[allow(dead_code)] // utility no-op sink for non-streaming callers
pub struct NullSink;

#[async_trait::async_trait]
impl TranscriptSink for NullSink {
    async fn push(&self, _chunk: &str) {}
}

/// A place where the profession's work is executed and verified.
#[async_trait::async_trait]
pub trait ExecutionBackend: Send + Sync {
    /// Run the implementation loop for `brief`, streaming transcript chunks into `sink`,
    /// and return the resulting diff/transcript/tests.
    async fn run_implementation(
        &self,
        brief: &TaskBrief,
        sink: &dyn TranscriptSink,
    ) -> Result<ExecResult>;
}

#[cfg(test)]
mod tests {
    use super::{default_model, TaskKind};

    // One test, run serially, because `default_model` reads process-global env vars — splitting
    // it would let parallel tests race on the same vars.
    #[test]
    fn default_model_defaults_and_env_overrides() {
        // Built-in defaults: documentation runs don't default to the frontier model.
        std::env::remove_var("FLABORFUL_MODEL_IMPLEMENT");
        std::env::remove_var("FLABORFUL_MODEL_INGEST");
        let implement = default_model(TaskKind::Implement);
        let ingest = default_model(TaskKind::Ingest);
        assert_ne!(implement, ingest);
        assert_eq!(implement, "claude-opus-4-8");
        assert_eq!(ingest, "claude-haiku-4-5");

        // Env overrides win.
        std::env::set_var("FLABORFUL_MODEL_INGEST", "some-cheap-model");
        assert_eq!(default_model(TaskKind::Ingest), "some-cheap-model");
        std::env::remove_var("FLABORFUL_MODEL_INGEST");
    }

    #[test]
    fn execution_environment_maps_to_backend_and_runtime() {
        use super::{AgentRuntimeKind, ExecutionBackendKind, ExecutionEnvironment};

        // Managed cloud → Managed backend, no coding runtime (Anthropic's agent drives it).
        assert_eq!(
            ExecutionEnvironment::ManagedCloud.resolve(),
            (ExecutionBackendKind::Managed, None)
        );
        // Claude Code BYOK → Sprite + ClaudeCode.
        assert_eq!(
            ExecutionEnvironment::ClaudeCodeByok.resolve(),
            (
                ExecutionBackendKind::Sprite,
                Some(AgentRuntimeKind::ClaudeCode)
            )
        );
        // Optimizer fbx → Sprite + FbxAgent.
        assert_eq!(
            ExecutionEnvironment::OptimizerFbx.resolve(),
            (
                ExecutionBackendKind::Sprite,
                Some(AgentRuntimeKind::FbxAgent)
            )
        );

        // String parsing round-trips the selector values; unknown => None.
        assert_eq!(
            ExecutionEnvironment::from_str_opt("optimizer_fbx"),
            Some(ExecutionEnvironment::OptimizerFbx)
        );
        assert_eq!(ExecutionEnvironment::from_str_opt("nope"), None);
    }
}
