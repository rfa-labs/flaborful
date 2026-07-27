//! The orchestrator — the bounded agent loop.
//!
//! reason → call tool → observe → reason again, capped by a step budget. The loop runs here, in
//! the agent process on the sprite; every reasoning turn is a chat-completion the optimizer sees
//! and can cache/downshift.
//!
//! Unlike a plain agent loop, this one terminates on the **`step_complete`** DAG-control tool: the
//! model calls it to declare a workflow node's sprite work done and hand output back to the engine.
//! (It also terminates on plain content or the step budget, as a fallback.) `step_complete` records
//! its output via the shared `StepControl`, which the loop polls after each tool dispatch.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

use crate::client::{OptimizerClient, ToolCall};
use crate::tool::ToolCatalog;

/// Shared signal the `step_complete` tool sets to end the loop and carry the node's output.
#[derive(Clone, Default)]
pub struct StepControl(Arc<Mutex<Option<StepOutput>>>);

/// The output a node hands back to the workflow engine (becomes `ctx.vars`).
#[derive(Clone, Debug)]
pub struct StepOutput {
    /// The value written into the engine blackboard under the node's output var.
    pub output: Value,
    /// A short human summary of what this step did (for the node log / PR body).
    pub summary: String,
}

impl StepControl {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn complete(&self, out: StepOutput) {
        *self.0.lock().unwrap() = Some(out);
    }
    pub fn taken(&self) -> Option<StepOutput> {
        self.0.lock().unwrap().clone()
    }
}

/// Config for one node's step.
pub struct StepConfig {
    /// Max reason→tool cycles before we stop (the safety bound — never unbounded).
    pub max_steps: usize,
}

/// Optimizer cost/savings aggregated over a step's LLM calls — what this node's work cost and saved
/// vs. the tenant's baseline model. Summed from each `Turn::savings()`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RunCost {
    pub baseline: f64,
    pub actual: f64,
    pub served: f64,
    pub exploration: f64,
    pub saved: f64,
    pub cache_hits: usize,
}

impl RunCost {
    fn add(&mut self, c: crate::client::CallSavings) {
        self.baseline += c.baseline;
        self.actual += c.actual;
        self.served += c.served;
        self.exploration += c.exploration;
        self.saved += c.saved;
        if c.cache_hit {
            self.cache_hits += 1;
        }
    }
}

/// The outcome of running one node's step.
#[derive(Debug, Default)]
pub struct StepRun {
    /// The node output the agent declared via `step_complete` (None if it stopped otherwise).
    pub step_output: Option<StepOutput>,
    /// The final plain-text content, if the model ended without `step_complete`.
    pub final_content: String,
    pub llm_calls: usize,
    pub tool_invocations: usize,
    /// Whether the step budget was hit without a clean completion.
    pub budget_exhausted: bool,
    /// Optimizer cost/savings summed over this step's LLM calls.
    pub cost: RunCost,
}

/// Run the loop over `messages` (the durable conversation history, mutated in place so the caller
/// can persist it) until `step_complete`, plain content, or the step budget. `control` is the
/// same `StepControl` the `step_complete` tool in `tools` holds.
pub async fn run_step(
    client: &OptimizerClient,
    tools: &ToolCatalog,
    control: &StepControl,
    cfg: &StepConfig,
    messages: &mut Vec<Value>,
) -> anyhow::Result<StepRun> {
    let mut run = StepRun::default();
    let schemas = tools.schemas();

    // Loop-guard: a signature of the previous turn's tool calls, and how many turns in a row have
    // repeated it verbatim. A model that keeps making the SAME call(s) with the same args is making
    // no progress; after a couple of repeats we inject a corrective nudge so it acts or finishes,
    // rather than silently burning the step budget (a real failure mode caught in a dry-run).
    let mut last_sig: Option<String> = None;
    let mut repeats: u32 = 0;
    const REPEAT_NUDGE_AT: u32 = 2;

    for _ in 0..cfg.max_steps {
        let turn = client.chat(messages, &schemas).await?;
        run.llm_calls += 1;
        run.cost.add(turn.savings());

        // Record the assistant turn verbatim into history (so tool_call_ids line up).
        if let Some(assistant_msg) = turn.raw.pointer("/choices/0/message") {
            messages.push(assistant_msg.clone());
        } else if let Some(content) = &turn.content {
            messages.push(json!({ "role": "assistant", "content": content }));
        }

        // No tool calls → the model answered with plain content. Treat as an (implicit) end.
        if turn.tool_calls.is_empty() {
            run.final_content = turn.content.unwrap_or_default();
            return Ok(run);
        }

        // Track repetition of this turn's tool-call set (name+args).
        let sig = tool_call_signature(&turn.tool_calls);
        if Some(&sig) == last_sig.as_ref() {
            repeats += 1;
        } else {
            repeats = 0;
            last_sig = Some(sig);
        }

        for tc in &turn.tool_calls {
            let result = dispatch(tools, tc).await;
            run.tool_invocations += 1;
            messages.push(json!({
                "role": "tool",
                "tool_call_id": tc.id,
                "content": result,
            }));
        }

        // The model may have called `step_complete` this turn — if so, stop cleanly.
        if let Some(out) = control.taken() {
            run.step_output = Some(out);
            return Ok(run);
        }

        // Repeated the same call(s) with no progress → nudge (a user message so it's unmistakable).
        if repeats >= REPEAT_NUDGE_AT {
            messages.push(json!({
                "role": "user",
                "content": "You are repeating the same tool call without making progress. Stop \
                    re-reading/observing. Take the next concrete action now: apply the change with \
                    `edit_file`/`write_file`, or if the task is done call `step_complete`."
            }));
            repeats = 0;
            last_sig = None;
        }
    }

    run.budget_exhausted = true;
    run.step_output = control.taken();
    Ok(run)
}

/// A stable signature of a turn's tool calls (name + args, in order), used by the loop-guard to
/// detect a model repeating the exact same call(s) with no progress.
fn tool_call_signature(tool_calls: &[ToolCall]) -> String {
    tool_calls
        .iter()
        .map(|tc| format!("{}({})", tc.function.name, tc.function.arguments))
        .collect::<Vec<_>>()
        .join("|")
}

/// Dispatch one tool call against the catalog. An unknown tool or a tool error is returned as a
/// string to the model (so it can recover) rather than aborting the run.
async fn dispatch(tools: &ToolCatalog, tc: &ToolCall) -> String {
    let name = &tc.function.name;
    // Tolerant lookup: models (across families) frequently invent a namespace prefix for file tools
    // — e.g. call `repo_browser.read_file` when they mean `read_file`. Rather than fight that prior,
    // accept it: try the exact name, then the name with any leading `namespace.` stripped. Confirmed
    // in dry-runs that every such miss was a real tool under a bogus prefix.
    let resolved = tools
        .get(name)
        .or_else(|| name.rsplit_once('.').and_then(|(_, bare)| tools.get(bare)));
    let Some(tool) = resolved else {
        // Still unknown after stripping — list the real names so the model can self-correct.
        return format!(
            "error: no such tool '{name}'. Available tools are: {}. Call one of these exact names.",
            tools.names().join(", ")
        );
    };
    match tool.call(&tc.function.arguments).await {
        Ok(out) => out,
        Err(e) => format!("error calling '{name}': {e}"),
    }
}
