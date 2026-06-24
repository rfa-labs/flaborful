-- Pluggable work-execution backends.
--
-- Professionals choose where their agent's work runs (Managed Agents vs self-hosted Sprite)
-- and, for Sprite, which coding-agent runtime drives it. These are defaults on the agent;
-- a workflow's AgentImplement node may override per-task.

ALTER TABLE agents ADD COLUMN IF NOT EXISTS execution_backend TEXT NOT NULL DEFAULT 'managed'; -- managed | sprite
ALTER TABLE agents ADD COLUMN IF NOT EXISTS agent_runtime TEXT NOT NULL DEFAULT 'claude_code'; -- claude_code | codex | custom
ALTER TABLE agents ADD COLUMN IF NOT EXISTS runtime_spec JSONB; -- install/run spec for a custom runtime

-- Per-task token authenticating a coding agent's callbacks to the shared "taste tool"
-- (consult_principal -> the professional's CF LoRA). Short-lived; resolves to the persona +
-- finetune without ever exposing the CF token to the agent. One per task.
CREATE TABLE taste_tokens (
    token          TEXT PRIMARY KEY,
    task_id        UUID NOT NULL UNIQUE REFERENCES tasks(id) ON DELETE CASCADE,
    agent_id       UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    persona_prompt TEXT NOT NULL,
    cf_finetune_id TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
