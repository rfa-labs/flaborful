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
    /// Approx input-token budget for the running conversation. Before each LLM call we compact the
    /// history (evicting the oldest tool/assistant turns) to stay under this — so a documentation run
    /// over a whole repo never balloons past the provider's context window (the real cause of the
    /// `Input tokens exceed…` / `prompt too long` 400s that produced 0 notes). Derived from the pinned
    /// model's window minus a safety cushion; see `main.rs`.
    pub context_budget_tokens: usize,
}

/// Cheap token estimate: ~4 chars/token (good enough for budgeting; no tokenizer dep — matches the
/// crate's minimal-deps rule). Counts the JSON-serialized message so tool_calls/args are included.
fn est_tokens(msg: &Value) -> usize {
    msg.to_string().len() / 4
}

fn history_tokens(messages: &[Value]) -> usize {
    messages.iter().map(est_tokens).sum()
}

/// How many of the most-recent turns compaction always keeps intact (recent working context).
const KEEP_RECENT: usize = 6;

fn role_of(msg: &Value) -> &str {
    msg.get("role").and_then(|r| r.as_str()).unwrap_or("")
}

/// Compact `messages` in place so the estimated token count fits `budget`, the way AAA coding agents
/// do it (Claude Code "microcompaction" / OpenCode pruning): keep the durable head (the system prompt
/// at index 0) and the most-recent turns; evict the OLDEST middle turns first (biggest bloat =
/// old `read_file`/`run_command` tool results). Anything important was already written to the vault
/// via `upsert_note`, so evicting stale in-context observations loses nothing.
///
/// Invariant preserved: never leave a `tool` message whose matching `assistant`(+tool_calls) turn was
/// evicted — that yields the OpenAI 400 `messages with role 'tool' must be a response to a preceding
/// tool_calls`. We evict in whole `assistant`→`tool…` groups from the front of the evictable region.
fn compact_history(messages: &mut Vec<Value>, budget: usize) {
    if history_tokens(messages) <= budget {
        return;
    }
    // Preserve the head (system message if present) and a tail of recent turns.
    let head = if messages.first().map(role_of) == Some("system") {
        1
    } else {
        0
    };
    let keep_recent = KEEP_RECENT; // always keep the last few turns intact (recent working context)

    // Evict the OLDEST turn-group from just after `head`, as a whole unit, so we never orphan a `tool`
    // message from its `assistant`(+tool_calls) parent (that would 400 on OpenAI). A group is one
    // non-system message plus any immediately-following `tool` messages (its tool results).
    while history_tokens(messages) > budget {
        let evictable_end = messages.len().saturating_sub(keep_recent);
        if head >= evictable_end {
            break; // only the head + recent tail remain — can't safely evict more
        }
        // Group = messages[head] and all consecutive `tool` messages after it. Removing them together
        // keeps every remaining `tool` paired with its assistant. (If messages[head] is itself a
        // stray `tool` — shouldn't happen given pairing — this still removes it cleanly.)
        let mut group_end = head + 1;
        while group_end < messages.len() && role_of(&messages[group_end]) == "tool" {
            group_end += 1;
        }
        // Don't let a group reach into the retained recent tail.
        let group_end = group_end.min(evictable_end.max(head + 1));
        messages.drain(head..group_end);
    }
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
        // Bound the context BEFORE each call so the request never exceeds the provider window (the
        // cause of the `Input tokens exceed…` 400s → 0 notes). Evicts the oldest tool/assistant turns.
        compact_history(messages, cfg.context_budget_tokens);
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
        Ok(out) => cap_tool_output(&out),
        Err(e) => format!("error calling '{name}': {e}"),
    }
}

/// Cap a single tool result so one huge `read_file`/`run_command`/`grep` can't blow the context
/// window on its own (AAA "microcompaction": offload bulky tool results at the source). Keeps the
/// head and tail (the informative ends) with a truncation marker between — the model still sees the
/// file's shape without the whole body sitting in history forever. ~5k tokens ≈ 20k chars, matching
/// Codex CLI's ~5k/file budget.
fn cap_tool_output(out: &str) -> String {
    const MAX_CHARS: usize = 20_000;
    if out.len() <= MAX_CHARS {
        return out.to_string();
    }
    let head_len = MAX_CHARS * 3 / 4;
    let tail_len = MAX_CHARS - head_len;
    // Slice on char boundaries.
    let head_end = out
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= head_len)
        .last()
        .unwrap_or(0);
    let tail_start = out
        .char_indices()
        .map(|(i, _)| i)
        .rev()
        .take_while(|&i| i >= out.len().saturating_sub(tail_len))
        .last()
        .unwrap_or(out.len());
    let omitted = out
        .len()
        .saturating_sub(head_end)
        .saturating_sub(out.len() - tail_start);
    format!(
        "{}\n\n…[truncated {omitted} bytes — output too large; read a specific range/file or grep for what you need]…\n\n{}",
        &out[..head_end],
        &out[tail_start..],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn big_tool_msg(id: &str, bytes: usize) -> Value {
        json!({ "role": "tool", "tool_call_id": id, "content": "x".repeat(bytes) })
    }
    fn assistant_call(id: &str) -> Value {
        json!({ "role": "assistant", "content": "", "tool_calls": [
            { "id": id, "type": "function", "function": { "name": "read_file", "arguments": "{}" } }
        ]})
    }

    #[test]
    fn compact_keeps_system_and_recent_and_fits_budget() {
        let mut messages = vec![json!({ "role": "system", "content": "rules" })];
        // 20 assistant→tool pairs, each tool result ~4k chars (~1k tokens).
        for i in 0..20 {
            messages.push(assistant_call(&format!("c{i}")));
            messages.push(big_tool_msg(&format!("c{i}"), 4000));
        }
        let before_len = messages.len();
        let before = history_tokens(&messages);
        compact_history(&mut messages, 3000); // force heavy eviction
        let after = history_tokens(&messages);

        assert!(after < before, "should have evicted");
        // Evicted the bulk of the 41 messages (down toward the head + recent tail), not just a few.
        assert!(
            messages.len() <= before_len / 2,
            "should have removed most messages"
        );
        // System message survives at the head.
        assert_eq!(role_of(&messages[0]), "system");
        // No orphan: every `tool` message must be preceded somewhere by an assistant turn (we never
        // leave a leading tool right after the system head).
        assert_ne!(
            role_of(&messages[1]),
            "tool",
            "first post-system msg must not be an orphan tool"
        );
    }

    #[test]
    fn compact_never_orphans_a_tool_message() {
        // After compaction, every `tool` message must be immediately preceded by an `assistant`
        // (its tool_calls parent) — otherwise the provider 400s. Force heavy eviction and check.
        let mut messages = vec![json!({ "role": "system", "content": "rules" })];
        for i in 0..15 {
            messages.push(assistant_call(&format!("c{i}")));
            messages.push(big_tool_msg(&format!("c{i}"), 3000));
        }
        compact_history(&mut messages, 2000);
        for (i, m) in messages.iter().enumerate() {
            if role_of(m) == "tool" {
                assert!(
                    i > 0 && role_of(&messages[i - 1]) == "assistant",
                    "tool at {i} is orphaned (prev = {:?})",
                    messages.get(i - 1).map(role_of)
                );
            }
        }
    }

    #[test]
    fn compact_noop_when_under_budget() {
        let mut messages = vec![
            json!({ "role": "system", "content": "rules" }),
            json!({ "role": "user", "content": "task" }),
        ];
        let before = messages.clone();
        compact_history(&mut messages, 1_000_000);
        assert_eq!(messages, before);
    }

    #[test]
    fn cap_tool_output_truncates_large_and_preserves_small() {
        let small = "hello world";
        assert_eq!(cap_tool_output(small), small);

        let big = "A".repeat(50_000);
        let capped = cap_tool_output(&big);
        assert!(capped.len() < big.len());
        assert!(capped.contains("truncated"));
        assert!(capped.starts_with("AAAA"));
        assert!(capped.ends_with("AAAA"));
    }
}
