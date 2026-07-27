//! Pluggable coding-agent runtimes that run *inside* a Sprite. The professional picks which
//! one drives their work (Claude Code / Codex / custom). An adapter is thin by design: it says
//! what to install, writes a small MCP-config file pointing the agent at the shared taste
//! endpoint, and gives the command to launch the agent headless. It contains NO taste-tool
//! logic — that lives once in `taste.rs`.

pub mod claude_code;
pub mod codex;
pub mod custom;
pub mod fbx_agent;

use crate::services::execution::TaskBrief;

/// A file the backend writes into the sprite before launching the runtime (e.g. an MCP config).
pub struct SpriteFile {
    pub path: String,
    pub contents: String,
}

/// What to install on the sprite for this runtime, and how to launch it.
pub struct InstallSpec {
    /// Shell commands to install the runtime (run once after the sprite is created).
    pub install_cmds: Vec<String>,
}

pub trait AgentRuntime: Send + Sync {
    /// Install commands for this runtime.
    fn install_spec(&self) -> InstallSpec;

    /// The MCP-config file that registers the two shared backend servers — `principal` (the
    /// taste tool → the LoRA) and `vault` (the professional's browsable knowledge vault).
    /// Different agents use different config shapes (Claude Code `.mcp.json`, Codex TOML, …) but
    /// the same server URLs + per-task token. This is the ENTIRE per-runtime tool surface.
    fn mcp_config(&self, taste_url: &str, vault_url: &str) -> SpriteFile;

    /// Extra files the backend writes into the sprite *before* launching the runtime (beyond the
    /// MCP config). Default: none. The fbx-agent runtime uses this to drop its per-step config
    /// JSON (optimizer creds, MCP urls, task, vars) that its CLI reads. Written like `mcp_config`.
    fn prelaunch_files(&self, _brief: &TaskBrief) -> Vec<SpriteFile> {
        Vec::new()
    }

    /// The shell command (run in the repo dir) that drives the agent headlessly on the brief.
    /// `repo_dir` is the checkout path on the sprite. Should run to completion (the agent's own
    /// read→edit→test→fix loop) and leave the working tree with the changes applied.
    fn invoke_command(&self, repo_dir: &str, brief: &TaskBrief) -> String;

    /// Extract the agent's end-of-loop summary from its transcript, if it emitted one. Default:
    /// none (runtimes that don't ask for a summary). Claude Code overrides this to read the
    /// delimited block it's instructed to print.
    fn parse_summary(&self, _transcript: &str) -> Option<String> {
        None
    }

    /// Extract per-run optimizer cost/savings from the transcript, if the runtime emits it. Default:
    /// none (runtimes that don't route through the optimizer). fbx-agent overrides this to read the
    /// `cost` block from its result envelope, so the engine can surface it on the node/run.
    fn parse_cost(&self, _transcript: &str) -> Option<serde_json::Value> {
        None
    }
}
