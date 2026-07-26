//! fbx-agent — flaborful's own DAG-aware coding orchestrator, run *inside a Sprite*.
//!
//! Every LLM call routes through the optimizer edge (route/cache/quality-gate/meter). The agent's
//! "brain" is a durable message history on the working tree, so its work can span DAG nodes: each
//! sprite node invokes `fbx-agent step --config <file>`, which loads the prior session, injects the
//! node's task + inputs, runs a bounded reason→tool→observe loop until the model calls
//! `step_complete`, persists the updated session, and prints a structured result for the workflow
//! engine to lift into `ctx.vars`.
//!
//! It imports NOTHING from the flaborful server crate — it's a lean, self-contained binary
//! (reqwest/serde/tokio only) the sprite install step can fetch.

// The tool contract (`idempotent`), catalog helpers (`is_empty`), and turn telemetry
// (`model_served`/`call_type`) are deliberately part of the shape — consumed by tests and by the
// engine-side wiring / trace stage landing next — so we don't prune them to chase warnings.
#![allow(dead_code)]

mod client;
mod mcp;
mod orchestrator;
mod session;
mod tool;
mod tools;

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{json, Value};

use client::{OptimizerClient, OptimizerConfig};
use mcp::{McpEndpoint, McpProxyTool};
use orchestrator::{run_step, StepConfig, StepControl};
use session::Session;
use tool::ToolCatalog;
use tools::{
    EditFile, Grep, ListDir, ReadFile, ReportProgress, RunCommand, StepComplete, WriteFile,
};

/// The per-step config the engine writes before invoking fbx-agent. One node's worth of work.
#[derive(Debug, Deserialize)]
struct StepInput {
    /// The sprite working tree (repo root), e.g. `/workspace/repo`.
    repo_dir: String,
    /// Where the durable session lives, e.g. `/workspace/.fbx/session.json`.
    session_path: String,
    /// The persona/system prompt (only used to seed a fresh session's system message).
    #[serde(default)]
    persona_prompt: String,
    /// The natural-language task for THIS node's step.
    task_text: String,
    /// Optional upstream inputs from the workflow blackboard, injected with the task.
    #[serde(default)]
    vars: Value,
    /// Optimizer edge config.
    optimizer_base_url: String,
    optimizer_key: String,
    #[serde(default)]
    provider_key: Option<String>,
    /// Optional model pin; `None` lets the optimizer route + downshift.
    #[serde(default)]
    model: Option<String>,
    /// MCP taste (principal) + vault endpoint URLs (same as the Claude Code adapter).
    #[serde(default)]
    taste_url: Option<String>,
    #[serde(default)]
    vault_url: Option<String>,
    /// Loop safety bound.
    #[serde(default = "default_max_steps")]
    max_steps: usize,
}

fn default_max_steps() -> usize {
    40
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    // Usage: fbx-agent step --config <path>
    let config_path = parse_config_arg(&args)
        .ok_or_else(|| anyhow::anyhow!("usage: fbx-agent step --config <path>"))?;
    let input: StepInput = serde_json::from_slice(&std::fs::read(&config_path)?)?;

    let result = run(input).await;
    // Always print a structured result envelope (even on error) so the engine can parse an outcome.
    match result {
        Ok(out) => {
            println!("{}", FBX_RESULT_BEGIN);
            println!("{}", serde_json::to_string(&out)?);
            println!("{}", FBX_RESULT_END);
            Ok(())
        }
        Err(e) => {
            let out = json!({ "ok": false, "error": e.to_string() });
            println!("{}", FBX_RESULT_BEGIN);
            println!("{}", out);
            println!("{}", FBX_RESULT_END);
            Err(e)
        }
    }
}

/// Delimiters wrapping the JSON result envelope, so the engine can extract it from the streamed
/// transcript (which also carries the agent's live tool/progress output).
const FBX_RESULT_BEGIN: &str = "===FBX_RESULT===";
const FBX_RESULT_END: &str = "===END_FBX_RESULT===";

fn parse_config_arg(args: &[String]) -> Option<PathBuf> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--config" {
            return it.next().map(PathBuf::from);
        }
    }
    None
}

async fn run(input: StepInput) -> anyhow::Result<Value> {
    let session_path = PathBuf::from(&input.session_path);
    let mut session = Session::load_or_new(&session_path)?;

    // Seed the system prompt on the first step only (persona + platform rules).
    if session.is_empty() {
        session
            .messages
            .push(json!({ "role": "system", "content": system_prompt(&input.persona_prompt) }));
    }

    // Inject this node's task + any upstream vars as the next user message.
    let mut user = String::new();
    user.push_str("# This node's task\n");
    user.push_str(&input.task_text);
    if !input.vars.is_null()
        && input
            .vars
            .as_object()
            .map(|o| !o.is_empty())
            .unwrap_or(false)
    {
        user.push_str("\n\n# Inputs from earlier workflow nodes (JSON)\n");
        user.push_str(&serde_json::to_string_pretty(&input.vars)?);
    }
    user.push_str(
        "\n\nWork on the repo with your tools. When THIS node's work is done, call `step_complete` \
         with a short summary and any structured output the workflow should pass downstream.",
    );
    session
        .messages
        .push(json!({ "role": "user", "content": user }));

    // Build the client + tool catalog.
    let client = OptimizerClient::new(OptimizerConfig {
        base_url: input.optimizer_base_url.clone(),
        optimizer_key: input.optimizer_key.clone(),
        provider_key: input.provider_key.clone(),
        model: input.model.clone(),
    });

    let control = StepControl::new();
    let mut catalog = ToolCatalog::new()
        .with(Box::new(ReadFile {
            repo_dir: input.repo_dir.clone(),
        }))
        .with(Box::new(WriteFile {
            repo_dir: input.repo_dir.clone(),
        }))
        .with(Box::new(EditFile {
            repo_dir: input.repo_dir.clone(),
        }))
        .with(Box::new(ListDir {
            repo_dir: input.repo_dir.clone(),
        }))
        .with(Box::new(Grep {
            repo_dir: input.repo_dir.clone(),
        }))
        .with(Box::new(RunCommand {
            repo_dir: input.repo_dir.clone(),
        }))
        .with(Box::new(StepComplete {
            control: control.clone(),
        }))
        .with(Box::new(ReportProgress));

    // Discover + attach the MCP likeness tools (principal + vault), same endpoints as Claude Code.
    catalog = attach_mcp(catalog, "principal", input.taste_url.as_deref()).await;
    catalog = attach_mcp(catalog, "vault", input.vault_url.as_deref()).await;

    let cfg = StepConfig {
        max_steps: input.max_steps,
    };
    let step = run_step(&client, &catalog, &control, &cfg, &mut session.messages).await?;

    session.steps_done += 1;
    session.save(&session_path)?;

    // Read the scalar fields before the match moves `step_output` out of `step`.
    let completed = step.step_output.is_some();
    let llm_calls = step.llm_calls;
    let tool_invocations = step.tool_invocations;
    let budget_exhausted = step.budget_exhausted;

    // Assemble the outcome for the engine.
    let (output, summary) = match step.step_output {
        Some(o) => (o.output, o.summary),
        None => (
            json!({ "note": "step ended without step_complete" }),
            if budget_exhausted {
                "[stopped: step budget reached]".to_string()
            } else {
                step.final_content.clone()
            },
        ),
    };

    Ok(json!({
        "ok": true,
        "output": output,
        "summary": summary,
        "llm_calls": llm_calls,
        "tool_invocations": tool_invocations,
        "budget_exhausted": budget_exhausted,
        "completed": completed,
    }))
}

/// Discover an MCP endpoint's tools and add each as a proxy tool. A missing URL or a discovery
/// failure degrades gracefully (the agent just runs without those tools) rather than aborting.
async fn attach_mcp(mut catalog: ToolCatalog, label: &str, url: Option<&str>) -> ToolCatalog {
    let Some(url) = url.filter(|u| !u.is_empty()) else {
        return catalog;
    };
    let endpoint = McpEndpoint::new(label, url);
    match endpoint.discover().await {
        Ok(defs) => {
            for def in defs {
                catalog = catalog.with(Box::new(McpProxyTool::new(endpoint.clone(), def)));
            }
        }
        Err(e) => {
            eprintln!("[fbx] warning: could not discover MCP '{label}' tools: {e}");
        }
    }
    catalog
}

/// The fixed platform rules, layered under the professional's persona. Ground in the vault, consult
/// the principal on genuine ambiguity, keep a rolling MEMORY.md (harvested into the professional's
/// vault — see Flaborful Rolling Memory; it's durable notes-to-self that persist across DAG steps),
/// and NEVER touch git (the workflow commits/pushes/PRs). The rules push the agent to ACT then
/// FINISH — weaker/mid models need explicit pressure toward edit→step_complete, plus an anti-loop
/// rule so a repeated read/observe can't run out the step budget (learned from a real dry-run).
fn system_prompt(persona: &str) -> String {
    let mut p = String::new();
    if !persona.trim().is_empty() {
        p.push_str(persona.trim());
        p.push_str("\n\n");
    }
    p.push_str(
        "You are a coding agent working inside a workflow. Your work is split across workflow \
         nodes: do THIS node's task with your tools, then call `step_complete`. You may be resumed \
         later for further steps with new inputs.\n\n\
         How to work (follow this loop):\n\
         1. Read only the file(s) you need to understand the task (usually one or two).\n\
         2. Make the change with `edit_file` (or `write_file` for a new file). Actually apply it — \
         reading a file again without editing makes no progress.\n\
         3. Optionally verify with one `read_file` or a `run_command` test.\n\
         4. Call `step_complete` with a concise `summary` and any structured `output` the next \
         nodes need. This is REQUIRED to finish — do not stop by just replying with text.\n\n\
         Rules:\n\
         - Do not repeat the same tool call with the same arguments. If a tool result already gave \
         you what you need, act on it — don't re-read.\n\
         - A tool message like \"(file X does not exist)\" is normal information, not an error.\n\
         - Keep a running `MEMORY.md` at the repo root: if it doesn't exist, CREATE it with \
         `write_file`; otherwise append your decisions, corrections, and anything you learned about \
         the principal's preferences. It's harvested into the professional's knowledge vault and \
         carries context across steps, so record durable notes — but don't let it distract from the \
         task itself.\n\
         - Use `principal_consult_principal` ONLY for genuine ambiguity, and the `vault_*` tools to \
         ground your work in the professional's past patterns — neither is required for a simple, \
         unambiguous task.\n\
         - Make changes by editing files in the repo. Do NOT run `git commit`, `git push`, or open \
         a pull request — the workflow does that after a human reviews your work. Just leave your \
         changes in the working tree.",
    );
    p
}
