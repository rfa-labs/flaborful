//! Codex runtime adapter — headless OpenAI Codex CLI on the sprite.
//!
//! Same shape as the Claude Code adapter: install the CLI, register the shared taste tool as an
//! MCP server (Codex reads `~/.codex/config.toml`), and invoke Codex non-interactively with the
//! task brief. The taste tool is the one shared backend endpoint; we only write the per-runtime
//! config pointing at it. No taste logic lives here.

use super::{AgentRuntime, InstallSpec, SpriteFile};
use crate::services::execution::TaskBrief;

pub struct CodexRuntime;

impl AgentRuntime for CodexRuntime {
    fn install_spec(&self) -> InstallSpec {
        InstallSpec {
            install_cmds: vec!["npm install -g @openai/codex >/dev/null 2>&1 || true".to_string()],
        }
    }

    fn mcp_config(&self, taste_url: &str, vault_url: &str) -> SpriteFile {
        // Codex reads MCP servers from ~/.codex/config.toml under [mcp_servers.<name>]. Both the
        // taste tool (`principal`) and the knowledge `vault` are HTTP/SSE MCP servers; the
        // per-task token is embedded in each URL.
        let contents = format!(
            "[mcp_servers.principal]\nurl = \"{}\"\ntransport = \"sse\"\n\n\
             [mcp_servers.vault]\nurl = \"{}\"\ntransport = \"sse\"\n",
            taste_url, vault_url
        );
        SpriteFile {
            path: "/root/.codex/config.toml".to_string(),
            contents,
        }
    }

    fn invoke_command(&self, repo_dir: &str, brief: &TaskBrief) -> String {
        // Codex non-interactive mode (`codex exec`) runs its own edit/test loop and leaves the
        // changes in the working tree. The persona is prepended to the task as guidance, since
        // Codex has no separate system-prompt flag in headless mode.
        let prompt = shell_escape(&format!(
            "{persona}\n\nTask:\n{task}",
            persona = brief.persona_prompt,
            task = brief.task_text,
        ));
        // Pin the model when the brief specifies one (else Codex uses its own default), so a
        // cheaper model can be selected per task the way the other runtimes do.
        let model = brief
            .model
            .clone()
            .unwrap_or_else(|| super::super::default_model(brief.kind));
        let model_flag = format!("--model {} ", shell_escape(&model));
        format!(
            "cd {repo} && codex exec --full-auto {model_flag}{prompt} 2>&1",
            repo = repo_dir,
            model_flag = model_flag,
            prompt = prompt,
        )
    }
}

/// Wrap a string in single quotes for safe shell interpolation.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
