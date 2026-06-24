-- Soft-reset marker for agents. When an owner "deletes" their agent to rebuild it, we
-- keep the agent row (and its id, so the Qdrant-keyed knowledge base survives) but clear
-- the LoRA training state and set deleted_at. The most recent deleted_at therefore marks
-- the last time the agent was reset and re-onboarded.
ALTER TABLE agents ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;
