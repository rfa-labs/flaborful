//! Managed Agents backend — Anthropic-hosted container runs the coding agent.
//!
//! Flow: create (or reuse) an agent with the persona system prompt + the prebuilt toolset + a
//! `consult_principal` custom tool → start a session with the company's repo mounted (the token
//! rides Anthropic's git proxy and never enters the container) → send the task brief → poll
//! events to idle, streaming the transcript and handling the custom taste tool by calling the
//! shared taste handler → ask the agent for the final unified diff and read it back.
//!
//! Requires ANTHROPIC_API_KEY; uses beta header `managed-agents-2026-04-01`. The frontier model
//! that drives the agent is claude-opus-4-8.

use anyhow::{anyhow, Context, Result};
use qdrant_client::Qdrant;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;

use super::{ExecResult, ExecutionBackend, TaskBrief, TranscriptSink};
use crate::services::inference::InferenceClient;

const API: &str = "https://api.anthropic.com/v1";
const BETA: &str = "managed-agents-2026-04-01";
const MODEL: &str = "claude-opus-4-8";

tokio::task_local! {
    /// The per-run Anthropic API key for the current `run_implementation` future. Set for the
    /// duration of a single run and read by `req()`, so every request in that run bills to the
    /// professional's own key without threading the key through every helper. Admin paths
    /// (`reap_orphans`/`kill_managed_runs`) run outside this scope and use the server key.
    static RUN_API_KEY: String;
}
/// The single shared environment name. One reused environment, never one-per-run (those leak).
const ENV_NAME: &str = "flaborful-swe";

pub struct ManagedAgentsBackend {
    db: PgPool,
    qdrant: Arc<Qdrant>,
    inference: InferenceClient,
    http: reqwest::Client,
    api_key: String,
    enabled: bool,
}

impl ManagedAgentsBackend {
    pub fn from_env(db: PgPool, qdrant: Arc<Qdrant>, inference: InferenceClient) -> Self {
        let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        let enabled = !api_key.is_empty();
        // Finite timeouts so a stalled managed-agents call fails cleanly (the polling loop makes
        // many short calls; none should hang indefinitely).
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            db,
            qdrant,
            inference,
            http,
            api_key,
            enabled,
        }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        // Inside a run, `RUN_API_KEY` holds the professional's own key; elsewhere (admin sweeps)
        // it isn't set and we fall back to the server key.
        let api_key = RUN_API_KEY
            .try_with(|k| k.clone())
            .unwrap_or_else(|_| self.api_key.clone());
        self.http
            .request(method, format!("{}{}", API, path))
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", BETA)
    }

    /// Resolve the single shared environment, creating it only if it doesn't already exist.
    ///
    /// The API does NOT enforce name uniqueness, so a naive "create every run" leaks a new
    /// environment per task (those accumulate and cost money). We LIST FIRST and reuse the
    /// existing `flaborful-swe` environment; only create when none is found. Unrestricted
    /// networking for v1; test-resource networking refinement is a later phase.
    async fn ensure_environment(&self) -> Result<String> {
        let name = ENV_NAME;

        // 1. Reuse an existing environment with our name if one is already there.
        if let Some(id) = self.find_environment(name).await? {
            return Ok(id);
        }

        // 2. None exists — create one.
        let create = self.req(reqwest::Method::POST, "/environments")
            .json(&json!({ "name": name, "config": { "type": "cloud", "networking": { "type": "unrestricted" } } }))
            .send().await?;
        if create.status().is_success() {
            let v: Value = create.json().await?;
            return v
                .get("id")
                .and_then(|x| x.as_str())
                .map(str::to_string)
                .ok_or_else(|| anyhow!("environment create returned no id"));
        }

        // 3. Create failed (e.g. a race created one concurrently) — list again and use whatever
        // is there now.
        self.find_environment(name)
            .await?
            .ok_or_else(|| anyhow!("could not create or find environment"))
    }

    /// Find an environment id by name (the first match), if any.
    async fn find_environment(&self, name: &str) -> Result<Option<String>> {
        let list: Value = self
            .req(reqwest::Method::GET, "/environments")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(list
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|e| e.get("name").and_then(|n| n.as_str()) == Some(name))
            })
            .and_then(|e| e.get("id"))
            .and_then(|x| x.as_str())
            .map(str::to_string))
    }

    /// Sweep orphaned managed-agents resources that would otherwise keep costing money: every
    /// non-running session, and every environment except the single shared one we keep
    /// (`flaborful-swe`). Called at startup so a crash/restart mid-run never leaks resources.
    ///
    /// Conservative by design: a session that is still `running` is left alone (it may be a live
    /// task) and only logged; everything idle/ended/terminated is deleted.
    pub async fn reap_orphans(&self) {
        if !self.enabled {
            return;
        }

        // 1. Sessions: delete all that aren't actively running.
        match self
            .req(reqwest::Method::GET, "/sessions?limit=100")
            .send()
            .await
        {
            Ok(resp) => {
                let v: Value = resp.json().await.unwrap_or(json!({}));
                let sessions = v
                    .get("data")
                    .and_then(|d| d.as_array())
                    .cloned()
                    .unwrap_or_default();
                let mut deleted = 0;
                let mut running = 0;
                for s in &sessions {
                    let id = s.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    let status = s.get("status").and_then(|x| x.as_str()).unwrap_or("");
                    if id.is_empty() {
                        continue;
                    }
                    if status == "running" {
                        running += 1;
                        continue;
                    }
                    if self.delete_session(id).await.is_ok() {
                        deleted += 1;
                    }
                }
                if deleted > 0 || running > 0 {
                    tracing::info!(
                        "managed-agents reaper: deleted {deleted} idle sessions, left {running} running"
                    );
                }
            }
            Err(e) => tracing::warn!("managed-agents reaper: could not list sessions: {e}"),
        }

        // 2. Environments: keep the one shared environment, delete the rest.
        let keep = self.find_environment(ENV_NAME).await.ok().flatten();
        match self
            .req(reqwest::Method::GET, "/environments?limit=100")
            .send()
            .await
        {
            Ok(resp) => {
                let v: Value = resp.json().await.unwrap_or(json!({}));
                let envs = v
                    .get("data")
                    .and_then(|d| d.as_array())
                    .cloned()
                    .unwrap_or_default();
                let mut deleted = 0;
                for env in &envs {
                    let id = env.get("id").and_then(|x| x.as_str()).unwrap_or("");
                    if id.is_empty() || keep.as_deref() == Some(id) {
                        continue;
                    }
                    let r = self
                        .req(reqwest::Method::DELETE, &format!("/environments/{}", id))
                        .send()
                        .await;
                    if r.is_ok() {
                        deleted += 1;
                    }
                }
                if deleted > 0 {
                    tracing::info!("managed-agents reaper: deleted {deleted} orphan environments");
                }
            }
            Err(e) => tracing::warn!("managed-agents reaper: could not list environments: {e}"),
        }
    }

    /// Force-stop ALL managed runs now: interrupt every running session, delete every session,
    /// and delete every environment except the shared one. Used by the ingestion cancel endpoint
    /// so a stop takes effect immediately (the reaper alone leaves running sessions alive).
    /// Returns the number of sessions it acted on.
    pub async fn kill_managed_runs(&self) -> usize {
        if !self.enabled {
            return 0;
        }
        let mut acted = 0;

        if let Ok(resp) = self
            .req(reqwest::Method::GET, "/sessions?limit=100")
            .send()
            .await
        {
            let v: Value = resp.json().await.unwrap_or(json!({}));
            let sessions = v
                .get("data")
                .and_then(|d| d.as_array())
                .cloned()
                .unwrap_or_default();
            for s in &sessions {
                let id = s.get("id").and_then(|x| x.as_str()).unwrap_or("");
                if id.is_empty() {
                    continue;
                }
                let status = s.get("status").and_then(|x| x.as_str()).unwrap_or("");
                // A running session can't be deleted directly — interrupt it first (the
                // `user.interrupt` event lands between turns and lets it wind down).
                if status == "running" {
                    let _ = self
                        .req(reqwest::Method::POST, &format!("/sessions/{}/events", id))
                        .json(&json!({ "events": [{ "type": "user.interrupt" }] }))
                        .send()
                        .await;
                }
                let _ = self.delete_session(id).await;
                acted += 1;
            }
        }

        // Then sweep environments (keeps the one shared env).
        self.reap_orphans().await;
        acted
    }

    /// Best-effort: read cumulative (input, output) token usage for a session, for cost
    /// visibility. The exact field path isn't contractual here, so we probe the common shapes
    /// (`usage.input_tokens`, `usage.output_tokens`, or top-level totals). Returns None if no
    /// usage is exposed — never fails the run.
    async fn fetch_usage(&self, session_id: &str) -> Option<(u64, u64)> {
        let v: Value = self
            .req(reqwest::Method::GET, &format!("/sessions/{}", session_id))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        // Pull the first integer found at any of the candidate paths.
        let pick = |paths: &[&str]| -> Option<u64> {
            for p in paths {
                if let Some(n) = v.pointer(p).and_then(|x| x.as_u64()) {
                    return Some(n);
                }
            }
            None
        };
        let input = pick(&[
            "/usage/input_tokens",
            "/usage/total_input_tokens",
            "/input_tokens",
        ]);
        let output = pick(&[
            "/usage/output_tokens",
            "/usage/total_output_tokens",
            "/output_tokens",
        ]);
        match (input, output) {
            (Some(i), Some(o)) => Some((i, o)),
            _ => None,
        }
    }

    /// Create a per-task agent with the persona + the consult_principal custom tool.
    async fn create_agent(&self, brief: &TaskBrief) -> Result<String> {
        let role = match brief.kind {
            super::TaskKind::Implement => {
                "You are a software engineer working on a real repository on behalf of the \
                 professional whose voice is described above. Implement the requested task: read \
                 the codebase, make the changes, run the project's tests, and iterate until they \
                 pass. Use the vault tools (search_vault / read_note / follow_links) to ground your \
                 work in their past patterns — this spans both YOUR vault and the company repo \
                 vault attached to this task. When finished, capture knowledge on BOTH sides like a \
                 human contractor would: call `record_technical` to write the concrete change into \
                 the COMPANY's vault (files/symbols, decisions in their codebase — proprietary \
                 detail is fine), and `record_experience` to write the transferable lesson into \
                 YOUR OWN vault (architecture/patterns you used and learned — NO company name, \
                 secrets, or proprietary identifiers). If this task followed a repeatable PROCEDURE \
                 worth reusing (a sequence you'd do the same way next time), call `write_skill` to \
                 save it as a step-by-step playbook in your vault. search_vault first and set \
                 `related` so each note connects into the existing graph. Then output a concise \
                 summary of what you changed."
            }
            super::TaskKind::Ingest => {
                "You are documenting a codebase into the professional's knowledge vault on their \
                 behalf. This is a READ-ONLY documentation task: do NOT edit code, run tests, \
                 commit, or push — there is no diff to produce. Your ONLY deliverable is vault \
                 notes. The task is NOT complete until you have called `upsert_note` to write \
                 several notes (feature / pattern / tech / golden_snippet) capturing how this \
                 repository is built; a run that ends without writing notes is a FAILURE. Read the \
                 source, then write notes as you go — do not wait until the end. Use `list_vault` / \
                 `search_vault` first to avoid duplicates and to link to existing notes."
            }
        };
        let system = format!(
            "{}\n\n{}\n\nThe Execution Blueprint in the task message (if present) already encodes \
             your principal's standing preferences — call `consult_principal` ONLY for a genuine \
             ambiguity it doesn't cover, not on every decision.",
            brief.persona_prompt, role
        );
        let mut tools = vec![
            json!({ "type": "agent_toolset_20260401" }),
            json!({
                "type": "custom",
                "name": "consult_principal",
                "description": "Ask the professional you represent how they would implement or review something. Use when a judgment call would benefit from their style/standards.",
                "input_schema": {
                    "type": "object",
                    "properties": { "question": { "type": "string", "description": "What to ask your principal." } },
                    "required": ["question"]
                }
            }),
        ];
        // The browsable knowledge vault — same tool surface as the MCP `vault` server, exposed
        // here as native custom tools (Managed Agents routes custom-tool calls, not MCP).
        tools.extend(vault_tool_specs());
        let body = json!({
            "name": format!("flaborful-task-{}", brief.task_id),
            "model": MODEL,
            "system": system,
            "tools": tools,
        });
        let v: Value = self
            .req(reqwest::Method::POST, "/agents")
            .json(&body)
            .send()
            .await?
            .error_for_status()
            .context("CMA create agent")?
            .json()
            .await?;
        v.get("id")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("agent create returned no id"))
    }

    async fn create_session(
        &self,
        agent_id: &str,
        env_id: &str,
        brief: &TaskBrief,
    ) -> Result<String> {
        let mut resources = vec![];
        if let (Some(repo), Some(token)) = (&brief.repo, &brief.github_token) {
            let url = if repo.starts_with("http") {
                repo.clone()
            } else {
                format!("https://github.com/{}", repo)
            };
            resources.push(json!({
                "type": "github_repository",
                "url": url,
                "authorization_token": token,
                "mount_path": "/workspace/repo",
                "checkout": { "type": "branch", "name": brief.base_branch }
            }));
        }
        let v: Value = self
            .req(reqwest::Method::POST, "/sessions")
            .json(&json!({ "agent": agent_id, "environment_id": env_id, "resources": resources }))
            .send()
            .await?
            .error_for_status()
            .context("CMA create session")?
            .json()
            .await?;
        v.get("id")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("session create returned no id"))
    }

    async fn send_message(&self, session_id: &str, text: &str) -> Result<()> {
        self.req(reqwest::Method::POST, &format!("/sessions/{}/events", session_id))
            .json(&json!({ "events": [{ "type": "user.message", "content": [{ "type": "text", "text": text }] }] }))
            .send().await?.error_for_status().context("CMA send message")?;
        Ok(())
    }

    /// Tear down a session (best-effort). Called on both success and failure so we never orphan a
    /// running session on Anthropic's side.
    async fn delete_session(&self, session_id: &str) -> Result<()> {
        self.req(
            reqwest::Method::DELETE,
            &format!("/sessions/{}", session_id),
        )
        .send()
        .await?;
        Ok(())
    }

    async fn send_tool_result(
        &self,
        session_id: &str,
        tool_use_id: &str,
        result: &str,
    ) -> Result<()> {
        self.req(reqwest::Method::POST, &format!("/sessions/{}/events", session_id))
            .json(&json!({ "events": [{ "type": "user.custom_tool_result", "custom_tool_use_id": tool_use_id, "content": [{ "type": "text", "text": result }] }] }))
            .send().await?.error_for_status().context("CMA send tool result")?;
        Ok(())
    }

    /// Poll the event list until the session is idle, streaming transcript chunks and handling
    /// the consult_principal custom tool. Returns the concatenated agent text transcript.
    /// (Polling instead of SSE keeps the implementation simple and robust; CMA buffers events.)
    async fn drive_to_idle(
        &self,
        session_id: &str,
        brief: &TaskBrief,
        sink: &dyn TranscriptSink,
    ) -> Result<String> {
        let mut transcript = String::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(60 * 30); // 30 min hard cap
                                                                                 // Stall watchdog: if no NEW event arrives for this long, the session is hung/interrupted
                                                                                 // and producing nothing (e.g. a turn was interrupted but never went idle). Give up rather
                                                                                 // than poll a dead session for the full 30 minutes.
        let stall_limit = Duration::from_secs(60 * 5);
        let mut last_progress = std::time::Instant::now();

        loop {
            if std::time::Instant::now() > deadline {
                return Err(anyhow!("CMA session timed out (30 min cap)"));
            }
            if last_progress.elapsed() > stall_limit {
                return Err(anyhow!(
                    "CMA session stalled — no new events for {}s",
                    stall_limit.as_secs()
                ));
            }
            let seen_before = seen.len();

            // Each poll cycle: drain ALL pages of the event list from the live tail, following
            // `next_page` only to finish reading the current snapshot. We must NOT persist the
            // last `next_page` cursor across cycles — doing so pins the loop to a stale page and it
            // never sees new events (it would poll the same old page forever and never go idle).
            let mut page: Option<String> = None;
            let mut idle = false;
            let mut events: Vec<Value>;
            loop {
                let path = match &page {
                    Some(p) => format!("/sessions/{}/events?page={}", session_id, p),
                    None => format!("/sessions/{}/events", session_id),
                };
                let resp: Value = self
                    .req(reqwest::Method::GET, &path)
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                events = resp
                    .get("data")
                    .and_then(|d| d.as_array())
                    .cloned()
                    .unwrap_or_default();
                self.process_events(
                    &events,
                    session_id,
                    brief,
                    sink,
                    &mut transcript,
                    &mut seen,
                    &mut idle,
                )
                .await?;
                match resp.get("next_page").and_then(|x| x.as_str()) {
                    Some(next) => page = Some(next.to_string()),
                    None => break, // caught up to the live tail for this cycle
                }
            }

            if idle {
                break;
            }
            // Reset the stall clock whenever this cycle observed at least one new event.
            if seen.len() > seen_before {
                last_progress = std::time::Instant::now();
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Ok(transcript)
    }

    /// Process one page of session events: stream agent text, dispatch custom tools (consult +
    /// vault), and detect idle/terminated. Sets `*idle = true` when the session is genuinely done
    /// (idle for a reason other than `requires_action`, or terminated).
    #[allow(clippy::too_many_arguments)]
    async fn process_events(
        &self,
        events: &[Value],
        session_id: &str,
        brief: &TaskBrief,
        sink: &dyn TranscriptSink,
        transcript: &mut String,
        seen: &mut std::collections::HashSet<String>,
        idle: &mut bool,
    ) -> Result<()> {
        // Did we answer a custom tool this batch? If so the agent is mid-work (we just unblocked
        // it), so a `status_idle` in the same batch is NOT terminal — it's the pause before the
        // agent continues. Only an idle with no pending tool work (or a terminate) ends the loop.
        let mut answered_tool = false;
        for ev in events {
            let id = ev
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if !id.is_empty() && !seen.insert(id) {
                continue;
            }
            let ty = ev.get("type").and_then(|x| x.as_str()).unwrap_or("");
            match ty {
                "agent.message" => {
                    if let Some(blocks) = ev.get("content").and_then(|c| c.as_array()) {
                        for b in blocks {
                            if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                                if let Some(text) = b.get("text").and_then(|t| t.as_str()) {
                                    transcript.push_str(text);
                                    transcript.push('\n');
                                    sink.push(text).await;
                                }
                            }
                        }
                    }
                }
                "agent.custom_tool_use" => {
                    let name = ev.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let tool_use_id = ev.get("id").and_then(|x| x.as_str());
                    if name == "consult_principal" {
                        let q = ev
                            .get("input")
                            .and_then(|i| i.get("question"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        let answer =
                            super::taste::consult(&self.db, &self.inference, &brief.taste_token, q)
                                .await
                                .unwrap_or_else(|e| format!("(consult failed: {})", e));
                        sink.push(&format!("[consult_principal] {} → {}", q, answer))
                            .await;
                        if let Some(tu) = tool_use_id {
                            self.send_tool_result(session_id, tu, &answer).await?;
                            answered_tool = true;
                        }
                    } else if is_vault_tool(name) {
                        // Resolve the agent from the per-task token and run the shared vault
                        // handler — same logic the MCP `vault` route uses.
                        let empty = json!({});
                        let mut args = ev.get("input").cloned().unwrap_or(empty);
                        // Stamp the run's repo onto every note the agent writes, overriding what it
                        // passes. This makes vault_notes.repo authoritative so the resume check
                        // ("skip repos already documented") is reliable rather than trusting the
                        // agent to tag each note correctly.
                        if matches!(
                            name,
                            "upsert_note" | "record_experience" | "record_technical"
                        ) {
                            if let (Some(obj), Some(repo)) = (args.as_object_mut(), &brief.repo) {
                                obj.insert("repo".into(), json!(repo));
                            }
                        }
                        let result =
                            match super::taste::resolve_vault_scope(&self.db, &brief.taste_token)
                                .await
                            {
                                Ok((owner, overlays)) => crate::services::vault::handle_tool(
                                    &self.db,
                                    &self.qdrant,
                                    &self.inference,
                                    owner,
                                    &overlays,
                                    name,
                                    &args,
                                )
                                .await
                                .unwrap_or_else(|e| format!("(vault error: {})", e)),
                                Err(e) => format!("(vault auth failed: {})", e),
                            };
                        sink.push(&format!(
                            "[{}] → {}",
                            name,
                            result.chars().take(120).collect::<String>()
                        ))
                        .await;
                        if let Some(tu) = tool_use_id {
                            self.send_tool_result(session_id, tu, &result).await?;
                            answered_tool = true;
                        }
                    }
                }
                "session.status_idle" => {
                    let reason = ev
                        .pointer("/stop_reason/type")
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    // `requires_action` means the agent is blocked on a custom-tool result we owe
                    // it — keep going. We also keep going if we answered a tool in this batch (the
                    // agent paused only to hand us the call and will resume once it gets the
                    // result). Anything else is a genuine end-of-work idle.
                    if reason != "requires_action" && !answered_tool {
                        *idle = true;
                    }
                }
                "session.status_terminated" => {
                    *idle = true;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Ask the agent (now idle) to emit the unified diff of its work, and read it back.
    async fn collect_diff(
        &self,
        session_id: &str,
        brief: &TaskBrief,
        sink: &dyn TranscriptSink,
    ) -> Result<(String, String)> {
        self.send_message(
            session_id,
            "Run `cd /workspace/repo && git add -A && git diff --cached` and reply with ONLY the \
             raw unified diff in a single ```diff code block, nothing else.",
        )
        .await?;
        let tail = self.drive_to_idle(session_id, brief, sink).await?;
        // Extract the diff from the last fenced block.
        let diff = extract_diff(&tail);
        Ok((diff, tail))
    }
}

#[async_trait::async_trait]
impl ExecutionBackend for ManagedAgentsBackend {
    async fn run_implementation(
        &self,
        brief: &TaskBrief,
        sink: &dyn TranscriptSink,
    ) -> Result<ExecResult> {
        // Claude Code Managed Cloud bills against the professional's OWN Anthropic key — there is
        // no fallback to the server key. If none is on file, skip with a clear message.
        let run_key = brief
            .anthropic_api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty());
        let Some(run_key) = run_key else {
            sink.push(
                "[managed-agents] no Anthropic API key on this agent — add your own key under \
                 “Where your agent does work” to run on Claude Code Managed Cloud",
            )
            .await;
            return Ok(ExecResult {
                status: "skipped".into(),
                ..Default::default()
            });
        };

        RUN_API_KEY
            .scope(run_key.to_string(), self.run_inner(brief, sink))
            .await
    }
}

impl ManagedAgentsBackend {
    /// The actual run body, executed inside the `RUN_API_KEY` task-local scope so every `req()`
    /// uses the professional's key.
    async fn run_inner(&self, brief: &TaskBrief, sink: &dyn TranscriptSink) -> Result<ExecResult> {
        let env_id = self.ensure_environment().await?;
        let agent_id = self.create_agent(brief).await?;
        let session_id = self.create_session(&agent_id, &env_id, brief).await?;

        sink.push(&format!("[managed-agents] session {} started", session_id))
            .await;
        self.send_message(&session_id, &brief.task_text).await?;

        // Drive the agent. If this errors (timeout/stall/transport), still tear down the session
        // so we don't orphan a running session on Anthropic's side.
        let mut transcript = match self.drive_to_idle(&session_id, brief, sink).await {
            Ok(t) => t,
            Err(e) => {
                let _ = self.delete_session(&session_id).await;
                sink.push(&format!("[managed-agents] session ended: {e}"))
                    .await;
                return Err(e);
            }
        };

        // Collect the diff only for Implement runs. An Ingest run is read-only (the deliverable is
        // vault notes, not a diff): asking it to `git diff` would wake the now-idle agent and make
        // it document the whole repo a SECOND time — wasted cost and duplicate work.
        let (diff, diff_tail) = match brief.kind {
            super::TaskKind::Implement => self
                .collect_diff(&session_id, brief, sink)
                .await
                .unwrap_or_default(),
            super::TaskKind::Ingest => (String::new(), String::new()),
        };
        transcript.push_str(&diff_tail);

        // Report token usage for cost visibility (one cheap GET while the session still exists).
        // Surfaced to the live feed so you can see exactly what each repo cost in tokens.
        if let Some((inp, out)) = self.fetch_usage(&session_id).await {
            sink.push(&format!(
                "[managed-agents] usage — input: {inp} tok, output: {out} tok"
            ))
            .await;
        }

        // Best-effort session cleanup.
        let _ = self.delete_session(&session_id).await;

        Ok(ExecResult {
            files_changed: files_from_diff(&diff),
            diff,
            transcript,
            summary: None,
            test_summary: None,
            head_sha: None,
            status: "completed".into(),
        })
    }
}

/// Is this custom-tool name one of the vault tools?
fn is_vault_tool(name: &str) -> bool {
    matches!(
        name,
        "upsert_note"
            | "list_vault"
            | "read_note"
            | "search_vault"
            | "follow_links"
            | "write_skill"
    )
}

/// The vault tools as Managed-Agents custom-tool specs (mirrors the MCP `vault` server's
/// `tools/list`). Kept here so the backend can register them on the agent.
fn vault_tool_specs() -> Vec<Value> {
    vec![
        json!({
            "type": "custom", "name": "upsert_note",
            "description": "Create or overwrite a vault note (your durable memory). layer ∈ feature|pattern|tech|golden_snippet|architecture|style_rule|note. Link related notes via their slugs in 'related'.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "slug": { "type": "string" }, "title": { "type": "string" },
                    "body": { "type": "string" }, "layer": { "type": "string" },
                    "domain": { "type": "string" },
                    "related": { "type": "array", "items": { "type": "string" } },
                    "repo": { "type": "string" }
                },
                "required": ["slug", "body"]
            }
        }),
        json!({
            "type": "custom", "name": "list_vault",
            "description": "List your vault notes (titles + slugs + layer), optionally filtered by layer or domain.",
            "input_schema": { "type": "object", "properties": { "layer": { "type": "string" }, "domain": { "type": "string" } } }
        }),
        json!({
            "type": "custom", "name": "read_note",
            "description": "Read one whole note by slug.",
            "input_schema": { "type": "object", "properties": { "slug": { "type": "string" } }, "required": ["slug"] }
        }),
        json!({
            "type": "custom", "name": "search_vault",
            "description": "Hybrid semantic + keyword search across your vault (one related-link hop), optionally filtered by layer/domain.",
            "input_schema": { "type": "object", "properties": { "query": { "type": "string" }, "layer": { "type": "string" }, "domain": { "type": "string" } }, "required": ["query"] }
        }),
        json!({
            "type": "custom", "name": "follow_links",
            "description": "Return the notes a note links to via its 'related' slugs.",
            "input_schema": { "type": "object", "properties": { "slug": { "type": "string" } }, "required": ["slug"] }
        }),
        json!({
            "type": "custom", "name": "record_experience",
            "description": "Record an EXPERIENCE note in YOUR OWN vault (the professional's) — the transferable lesson from this work: what you built, the architecture/patterns you used, decisions you made, what worked. This is YOURS to carry to future jobs, like a contractor describing past work in an interview. MUST NOT contain the company's name, secrets, proprietary identifiers, or copied proprietary code. layer ∈ architecture|pattern|note. Link related notes via 'related'. search_vault first to connect it into your graph.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "slug": { "type": "string" }, "title": { "type": "string" },
                    "body": { "type": "string" }, "layer": { "type": "string" },
                    "related": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["slug", "body"]
            }
        }),
        json!({
            "type": "custom", "name": "record_technical",
            "description": "Record a TECHNICAL note in the COMPANY's vault — the concrete work product in the context of their codebase (files/symbols touched, the change, decisions). Proprietary detail is expected and fine here; this stays with the company who owns the work. layer ∈ feature|architecture|pattern|tech|note. Link related notes via 'related'. search_vault first to connect it to existing company notes.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "slug": { "type": "string" }, "title": { "type": "string" },
                    "body": { "type": "string" }, "layer": { "type": "string" },
                    "related": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["slug", "body"]
            }
        }),
        json!({
            "type": "custom", "name": "write_skill",
            "description": "Save a reusable SKILL in YOUR OWN vault — a step-by-step playbook for a task you did well, so future runs can repeat it consistently. 'when_to_use' is the one-line trigger that decides when this skill loads into a future task's brief; 'body' is the numbered, prescriptive steps. Prefer this over a plain note when the knowledge is a repeatable PROCEDURE (e.g. 'how I add a migration', 'my PR checklist'). Skills are transferable and default to shareable — keep them free of the company's proprietary specifics.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "slug": { "type": "string" }, "title": { "type": "string" },
                    "when_to_use": { "type": "string" },
                    "body": { "type": "string" },
                    "domain": { "type": "string" },
                    "related": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["slug", "body"]
            }
        }),
    ]
}

/// Pull the last ```diff (or ```) fenced block out of agent text.
fn extract_diff(text: &str) -> String {
    if let Some(start) = text.rfind("```diff").or_else(|| text.rfind("```")) {
        let after = &text[start..];
        if let Some(open_end) = after.find('\n') {
            let body = &after[open_end + 1..];
            if let Some(close) = body.find("```") {
                return body[..close].trim_end().to_string();
            }
        }
    }
    String::new()
}

/// Parse the `+++ b/<path>` lines from a unified diff.
fn files_from_diff(diff: &str) -> Vec<String> {
    diff.lines()
        .filter_map(|l| l.strip_prefix("+++ b/").map(str::to_string))
        .collect()
}
