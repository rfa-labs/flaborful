-- Phase 5: Evaluations + Reflections — the self-improvement loop.
--
-- Evaluations grade a completed task against per-agent criteria (generalizing the single
-- confidence score in confidence.rs into criteria/tags/data-points → a deterministic grade).
-- Reflections mine recent activity for patterns and propose Skills / instruction / tool changes
-- into a human-gated review queue.

-- sqlx: no-transaction
-- (ALTER TYPE ... ADD VALUE can't run in the same tx as later use of the value.)

ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'task_evaluation_critical';
ALTER TYPE notification_type ADD VALUE IF NOT EXISTS 'agent_reflection_ready';

-- Per-agent evaluation criteria — reusable true/false quality rules the evaluator checks each task
-- against. Priority: 'warning' | 'critical' (a failed critical criterion → Critical grade).
CREATE TABLE IF NOT EXISTS agent_criteria (
    id         UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    agent_id   UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    -- The specific true/false statement the AI evaluator judges (e.g. "The output addresses every
    -- part of the task" / "No secrets or credentials are exposed").
    prompt     TEXT NOT NULL,
    priority   TEXT NOT NULL DEFAULT 'warning',  -- 'warning' | 'critical'
    enabled    BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_agent_criteria_agent ON agent_criteria(agent_id);

-- One evaluation per completed task. grade ∈ 'pass' | 'warning' | 'critical', computed
-- deterministically from the per-criterion results (any failed critical → critical; any failed
-- warning → warning; else pass). criteria_results/tags/data_points are the structured LLM output.
CREATE TABLE IF NOT EXISTS task_evaluations (
    id               UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    task_id          UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    agent_id         UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    grade            TEXT NOT NULL,                 -- pass | warning | critical
    criteria_results JSONB NOT NULL DEFAULT '[]',   -- [{ name, passed, priority, reasoning }]
    tags             JSONB NOT NULL DEFAULT '[]',   -- [string]
    data_points      JSONB NOT NULL DEFAULT '{}',   -- { name: value }
    summary          TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
-- One current evaluation per task (re-running replaces it).
CREATE UNIQUE INDEX IF NOT EXISTS idx_task_eval_task ON task_evaluations(task_id);
CREATE INDEX IF NOT EXISTS idx_task_eval_agent ON task_evaluations(agent_id);

-- Reflection proposals — the output of a scheduled self-review. Each is a suggested improvement the
-- owner approves or dismisses. kind ∈ 'new_skill' | 'skill_fix' | 'instruction_update' |
-- 'tool_access'. payload carries what applying it needs (e.g. { slug, title, when_to_use, body } for
-- a skill; { prompt } for an instruction update). status ∈ 'pending' | 'applied' | 'dismissed'.
CREATE TABLE IF NOT EXISTS reflection_proposals (
    id          UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    agent_id    UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    title       TEXT NOT NULL,
    rationale   TEXT NOT NULL,
    payload     JSONB NOT NULL DEFAULT '{}',
    status      TEXT NOT NULL DEFAULT 'pending',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_reflection_agent_status ON reflection_proposals(agent_id, status);

-- Track when reflections last ran per agent, so the scheduler only mines new activity.
ALTER TABLE agents ADD COLUMN IF NOT EXISTS last_reflection_at TIMESTAMPTZ;
