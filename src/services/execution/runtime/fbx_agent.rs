//! fbx-agent runtime adapter — flaborful's own DAG-aware coding orchestrator on the sprite.
//!
//! Unlike the Claude Code / Codex adapters (which shell out to a third-party CLI and get their MCP
//! surface from a config file the CLI reads), fbx-agent is OUR binary. It reads a single per-step
//! **config JSON** (written via `prelaunch_files`) carrying the optimizer creds, the MCP taste/vault
//! URLs, this node's task, and the upstream vars — then routes every LLM call through the optimizer
//! edge. The session (its durable "brain") lives on the working tree at `/workspace/.fbx/session.json`
//! so its work can span DAG nodes.
//!
//! The config + session dir live OUTSIDE the repo checkout (`/workspace/.fbx`, not `/workspace/repo`)
//! so they never enter the diff/commit/PR — no `.git/info/exclude` dance needed.

use super::{AgentRuntime, InstallSpec, SpriteFile};
use crate::services::execution::{default_model, TaskBrief};

/// Where the per-step config + durable session live on the sprite (outside the repo checkout).
const FBX_DIR: &str = "/workspace/.fbx";
const CONFIG_PATH: &str = "/workspace/.fbx/step.json";
const SESSION_PATH: &str = "/workspace/.fbx/session.json";

pub struct FbxAgentRuntime;

impl AgentRuntime for FbxAgentRuntime {
    fn install_spec(&self) -> InstallSpec {
        // Install fbx-agent from npm — exactly like the Claude Code / Codex runtimes install their
        // CLIs (`@anthropic-ai/claude-code`, `@openai/codex`). `@rfa-labs/fbx-agent` is a PUBLIC
        // package (no auth needed), shipping a static musl linux-x64 binary that runs on any Sprite
        // regardless of the base image's glibc. Fly Sprites don't support a custom base image yet, so
        // runtime-install is the delivery model (a future "fork from a sprite" would let us pre-bake
        // it). `FBX_AGENT_INSTALL` overrides the command for dev / a pinned artifact. Best-effort
        // (`|| true`) so a transient fetch failure surfaces as a clear "fbx-agent: command not found"
        // at invoke, not a setup abort.
        let install = std::env::var("FBX_AGENT_INSTALL")
            .unwrap_or_else(|_| "npm install -g @rfa-labs/fbx-agent >/dev/null 2>&1".to_string());
        InstallSpec {
            install_cmds: vec![format!("mkdir -p {FBX_DIR}"), format!("{install} || true")],
        }
    }

    fn mcp_config(&self, _taste_url: &str, _vault_url: &str) -> SpriteFile {
        // fbx-agent does NOT read a separate MCP config file — it gets the taste/vault URLs from its
        // step config (see `prelaunch_files`). The trait still requires a file, so we write a
        // harmless marker outside the repo (never touched by the agent, never in the diff).
        SpriteFile {
            path: format!("{FBX_DIR}/.mcp-unused"),
            contents: String::new(),
        }
    }

    fn prelaunch_files(&self, brief: &TaskBrief) -> Vec<SpriteFile> {
        // The whole per-step config the fbx-agent CLI reads: run-stable config + this node's task +
        // the upstream vars the engine put on the brief. One source of truth (no env-string plumbing).
        let model = brief
            .model
            .clone()
            .unwrap_or_else(|| default_model(brief.kind));

        let cfg = serde_json::json!({
            "repo_dir": crate::services::execution::sprite::REPO_DIR_PUB,
            "session_path": SESSION_PATH,
            "persona_prompt": brief.persona_prompt,
            "task_text": brief.task_text,
            "vars": brief.vars,
            "optimizer_base_url": brief.optimizer_base_url,
            "optimizer_key": brief.optimizer_key,
            "provider_key": brief.provider_key,
            "model": model,
            "taste_url": brief.taste_url(),
            "vault_url": brief.vault_url(),
        });

        vec![SpriteFile {
            path: CONFIG_PATH.to_string(),
            contents: cfg.to_string(),
        }]
    }

    fn invoke_command(&self, _repo_dir: &str, _brief: &TaskBrief) -> String {
        // Run from the repo dir so the agent's file/command tools operate on the checkout; the
        // `2>&1` lets the transcript stream into the node log via `exec_streaming`.
        format!(
            "cd {repo} && fbx-agent step --config {cfg} 2>&1",
            repo = crate::services::execution::sprite::REPO_DIR_PUB,
            cfg = CONFIG_PATH,
        )
    }

    fn parse_summary(&self, transcript: &str) -> Option<String> {
        // fbx-agent prints a `===FBX_RESULT===` … `===END_FBX_RESULT===` JSON envelope; lift the
        // `summary` field out for the node log / PR body.
        let start = transcript.find(FBX_RESULT_BEGIN)? + FBX_RESULT_BEGIN.len();
        let rest = &transcript[start..];
        let end = rest.find(FBX_RESULT_END)?;
        let json: serde_json::Value = serde_json::from_str(rest[..end].trim()).ok()?;
        json.get("summary")
            .and_then(|s| s.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    }
}

/// Delimiters wrapping fbx-agent's JSON result envelope (kept in sync with the fbx-agent binary's
/// `main.rs`). The engine extracts the summary/output from between them.
const FBX_RESULT_BEGIN: &str = "===FBX_RESULT===";
const FBX_RESULT_END: &str = "===END_FBX_RESULT===";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_summary_lifts_the_envelope() {
        let transcript = "…live tool output…\n\
             ===FBX_RESULT===\n\
             {\"ok\":true,\"summary\":\"added the endpoint\",\"output\":{\"branch\":\"x\"}}\n\
             ===END_FBX_RESULT===\n";
        assert_eq!(
            FbxAgentRuntime.parse_summary(transcript),
            Some("added the endpoint".to_string())
        );
    }

    #[test]
    fn parse_summary_none_without_envelope() {
        assert_eq!(FbxAgentRuntime.parse_summary("no envelope here"), None);
    }
}
