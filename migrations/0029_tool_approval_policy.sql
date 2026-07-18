-- Phase 7: HITL tool-approval policy. Generalizes the two existing gates (output-review + the
-- rerun side-effect gate) into one per-agent policy evaluated BEFORE a side-effecting node fires,
-- even on its first run. Modes mirror Gumloop's: always_allow | ask_each_time | ask_on_writes.
-- Optional per-effect-kind rules override the default (e.g. always ask before a github_pr).

CREATE TABLE IF NOT EXISTS agent_tool_policies (
    agent_id   UUID PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    -- always_allow: never pause on the first write (the historical behavior).
    -- ask_on_writes: pause before every side-effecting node's first fire.
    -- ask_each_time: alias of ask_on_writes today (all gated nodes are writes) — kept for parity.
    mode       TEXT NOT NULL DEFAULT 'always_allow',
    -- Per-effect-kind overrides: { "github_pr": "ask", "slack_message": "allow" }. An entry wins
    -- over `mode` for that effect kind.
    rules      JSONB NOT NULL DEFAULT '{}',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
