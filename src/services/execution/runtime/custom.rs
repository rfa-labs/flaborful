//! Custom runtime adapter — a professional-defined coding agent on the sprite.
//!
//! Driven entirely by the agent's `runtime_spec` JSONB so a pro can wire up any MCP-speaking
//! coding tool without us shipping a dedicated adapter. The spec is a thin template:
//!
//! ```json
//! {
//!   "install_cmds": ["pip install my-agent"],
//!   "mcp_config_path": "/root/.config/my-agent/mcp.json",
//!   "mcp_config_template": "{\"servers\":{\"principal\":{\"url\":\"{taste_url}\"}}}",
//!   "invoke_template": "cd {repo_dir} && my-agent run --prompt {task} --system {persona} 2>&1"
//! }
//! ```
//!
//! Placeholders substituted: `{taste_url}` (mcp_config_template), and `{repo_dir}` / `{task}` /
//! `{persona}` (invoke_template; `{task}`/`{persona}` are shell-escaped). This adapter still holds
//! NO taste logic — the template only points the tool at the one shared endpoint.

use serde_json::Value;

use super::{AgentRuntime, InstallSpec, SpriteFile};
use crate::services::execution::TaskBrief;

pub struct CustomRuntime {
    spec: Value,
}

impl CustomRuntime {
    pub fn new(spec: Value) -> Self {
        Self { spec }
    }

    fn str_field(&self, key: &str) -> &str {
        self.spec.get(key).and_then(|v| v.as_str()).unwrap_or("")
    }
}

impl AgentRuntime for CustomRuntime {
    fn install_spec(&self) -> InstallSpec {
        let install_cmds = self
            .spec
            .get("install_cmds")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        InstallSpec { install_cmds }
    }

    fn mcp_config(&self, taste_url: &str, vault_url: &str) -> SpriteFile {
        let path = {
            let p = self.str_field("mcp_config_path");
            if p.is_empty() {
                "/workspace/repo/.mcp.json".to_string()
            } else {
                p.to_string()
            }
        };
        let contents = self
            .str_field("mcp_config_template")
            .replace("{taste_url}", taste_url)
            .replace("{vault_url}", vault_url);
        SpriteFile { path, contents }
    }

    fn invoke_command(&self, repo_dir: &str, brief: &TaskBrief) -> String {
        // `{model}` resolves to the brief's pinned model, else the per-`TaskKind` default, so a
        // custom runtime template can inject the selected model like the built-in adapters do.
        let model = brief
            .model
            .clone()
            .unwrap_or_else(|| super::super::default_model(brief.kind));
        self.str_field("invoke_template")
            .replace("{repo_dir}", repo_dir)
            .replace("{task}", &shell_escape(&brief.task_text))
            .replace("{persona}", &shell_escape(&brief.persona_prompt))
            .replace("{model}", &shell_escape(&model))
    }
}

/// Wrap a string in single quotes for safe shell interpolation.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
