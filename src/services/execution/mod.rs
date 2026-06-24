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
}

impl Default for AgentRuntimeKind {
    fn default() -> Self {
        AgentRuntimeKind::ClaudeCode
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
    /// Per-task token authenticating the agent's calls back to the shared taste endpoint.
    pub taste_token: String,
    /// Base URL of this backend (for building the taste callback URL).
    pub api_base_url: String,
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
