//! Claude Code runtime adapter — headless Claude Code on the sprite.
//!
//! Install the CLI, register the shared taste tool as an MCP server via `.mcp.json`, and invoke
//! Claude Code in headless/print mode with the task brief as the prompt. The taste tool is the
//! one shared backend endpoint; we only write the per-runtime config pointing at it.

use super::{AgentRuntime, InstallSpec, SpriteFile};
use crate::services::execution::TaskBrief;

pub struct ClaudeCodeRuntime;

impl AgentRuntime for ClaudeCodeRuntime {
    fn install_spec(&self) -> InstallSpec {
        InstallSpec {
            install_cmds: vec![
                // Node is assumed present on the dev image; install Claude Code globally.
                "npm install -g @anthropic-ai/claude-code >/dev/null 2>&1 || true".to_string(),
            ],
        }
    }

    fn mcp_config(&self, taste_url: &str, vault_url: &str) -> SpriteFile {
        // Claude Code reads .mcp.json from the project root. Both servers are Streamable-HTTP MCP
        // (stateless JSON-RPC over POST); the per-task token is embedded in each URL, so no auth
        // header is needed. `principal` = the taste tool; `vault` = the browsable knowledge vault.
        let contents = serde_json::json!({
            "mcpServers": {
                "principal": { "type": "http", "url": taste_url },
                "vault": { "type": "http", "url": vault_url }
            }
        })
        .to_string();
        SpriteFile {
            path: "/workspace/repo/.mcp.json".to_string(),
            contents,
        }
    }

    fn invoke_command(&self, repo_dir: &str, brief: &TaskBrief) -> String {
        // Headless print mode. The persona is passed via --append-system-prompt; the prompt is the
        // task plus the professional's how-to instructions plus fixed platform rules. Claude Code
        // runs its own edit/test loop and leaves changes in the working tree.
        //
        // The `principal` MCP server exposes consult_principal — allow-listed as
        // mcp__principal__consult_principal so the loop can call the professional's LoRA. In
        // headless mode there's no interactive trust prompt; --dangerously-skip-permissions loads
        // the project .mcp.json server without it. Single-quote-escape dynamic strings.
        let persona = shell_escape(&brief.persona_prompt);
        let prompt = shell_escape(&build_prompt(brief));
        // `--output-format stream-json --verbose` makes Claude Code emit one JSON event per line as
        // it works, instead of buffering all output until the run finishes and printing it at the
        // end. Without this, the platform's `exec_streaming` sees NO output for the entire run — the
        // onboarding feed sits at "0 notes written" and the server logs go silent after "running
        // coding agent…" even when the agent is working (or hung — you can't tell which). Streaming
        // surfaces tool calls (including the `upsert_note` MCP calls that ARE the ingestion
        // deliverable) live, so progress is visible and failures are diagnosable. The delimited
        // end-of-run summary still rides through in the JSON `result` text, so `parse_summary`'s
        // substring scan over the raw stream keeps working.
        format!(
            "cd {repo} && ANTHROPIC_MODEL=claude-opus-4-8 claude -p {prompt} \
             --append-system-prompt {persona} \
             --output-format stream-json --verbose \
             --permission-mode acceptEdits --dangerously-skip-permissions \
             --allowedTools 'Bash,Edit,Write,Read,Glob,Grep,mcp__principal__consult_principal,mcp__vault__upsert_note,mcp__vault__list_vault,mcp__vault__read_note,mcp__vault__search_vault,mcp__vault__follow_links,mcp__vault__record_experience,mcp__vault__record_technical' \
             2>&1",
            repo = repo_dir,
            prompt = prompt,
            persona = persona,
        )
    }

    fn parse_summary(&self, transcript: &str) -> Option<String> {
        let start = transcript.find(SUMMARY_BEGIN)? + SUMMARY_BEGIN.len();
        let rest = &transcript[start..];
        let end = rest.find(SUMMARY_END)?;
        let summary = rest[..end].trim();
        if summary.is_empty() {
            None
        } else {
            Some(summary.to_string())
        }
    }
}

/// Delimiters Claude is told to wrap its end-of-loop summary in, so we can extract it from the
/// transcript without losing live streaming (text mode). Kept here next to the prompt that asks
/// for them; `parse_summary` (in the execution module) reads them back.
pub const SUMMARY_BEGIN: &str = "===FLABORFUL_SUMMARY===";
pub const SUMMARY_END: &str = "===END_FLABORFUL_SUMMARY===";

/// Compose the full prompt: the task (WHAT), the professional's instructions (HOW, optional), and
/// fixed platform rules — don't touch git (the platform commits/pushes/PRs), and emit a delimited
/// summary at the end.
fn build_prompt(brief: &TaskBrief) -> String {
    let mut p = String::new();
    p.push_str("# Task\n");
    p.push_str(&brief.task_text);

    if let Some(instr) = brief.instructions.as_deref() {
        p.push_str("\n\n# How to do this work\n");
        p.push_str(instr);
    }

    p.push_str(&format!(
        "\n\n# Platform rules (always)\n\
         - The Execution Blueprint above (if present) already encodes your principal's standing \
         preferences. Call `consult_principal` ONLY for a genuine ambiguity the Blueprint doesn't \
         cover — not on every decision. Use the `vault` tools (search_vault / read_note / \
         follow_links) to ground your work in their past patterns, and upsert_note to record any \
         durable decision worth remembering.\n\
         - Keep a running `MEMORY.md` at the repo root: as you work, append decisions you made, \
         corrections from the principal, and any \"the principal prefers X\" you learn. It's \
         harvested into their knowledge vault to inform future tasks.\n\
         - When the work is done, capture knowledge on BOTH sides, the way a human contractor would: \
         the company owns the work product, but you (the professional) own the experience. Call \
         `record_technical` to write the concrete change into the COMPANY's vault (files/symbols \
         touched, decisions in context of their codebase — proprietary detail is fine). Call \
         `record_experience` to write the transferable lesson into YOUR OWN vault (what you built, \
         the architecture/patterns you used and learned) — with NO company name, secrets, or \
         proprietary identifiers, so it's yours to talk about in a future interview. For each, \
         `search_vault` first and set `related` so the new note connects into the existing graph.\n\
         - Make the change by editing files in this repo. Do NOT run `git commit`, `git push`, or \
         open a pull request — the platform commits, pushes, and opens the PR for you after a human \
         reviews your work. Just leave your changes in the working tree.\n\
         - When you are done, end your response with a short summary of what you changed and why, \
         wrapped exactly like this:\n\
         {begin}\n\
         <your summary here>\n\
         {end}\n",
        begin = SUMMARY_BEGIN,
        end = SUMMARY_END,
    ));
    p
}

/// Wrap a string in single quotes for safe shell interpolation.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
