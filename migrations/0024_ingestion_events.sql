-- Progress feed for agentic code-ingestion runs (onboarding repo → vault documentation).
--
-- Each background ingestion run streams its agent transcript here so onboarding can show live
-- progress (mirrors how task runs stream into node_executions.logs). One row per event; an
-- SSE endpoint tails them by (agent_id, created_at). Per-repo status rows let the UI show
-- which repo is pending/running/done/failed and a running note count.

CREATE TABLE IF NOT EXISTS ingestion_events (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id    UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    repo        TEXT NOT NULL,
    -- 'status'   → a per-repo lifecycle change (running/done/failed); `status` is set.
    -- 'note'     → the agent wrote a vault note; `detail` is the tool-result line.
    -- 'message'  → agent assistant text / consult / generic transcript chunk.
    kind        TEXT NOT NULL,
    status      TEXT,            -- for kind='status': running | done | failed
    detail      TEXT NOT NULL DEFAULT '',
    notes_count INTEGER NOT NULL DEFAULT 0,  -- running tally for this repo at emit time
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- The SSE tail: events for an agent after a timestamp cursor, in order.
CREATE INDEX IF NOT EXISTS ingestion_events_agent_time_idx
    ON ingestion_events (agent_id, created_at);
