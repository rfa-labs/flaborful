-- Side-effect gate: when a re-run reaches a tool node that already performed an external
-- write (posted a comment, sent an email, created a doc), we pause for the owner to
-- decide rather than silently duplicating the effect. New task status + a column on
-- node_executions to hold the gate's pending context and the owner's decision.

-- ALTER TYPE ... ADD VALUE can't be used in the same tx as the value, so no-transaction.
-- sqlx: no-transaction
ALTER TYPE task_status ADD VALUE IF NOT EXISTS 'awaiting_effect' AFTER 'awaiting_review';

-- The pending side-effect context (resource_kind, prior resource_id, proposed new content)
-- for a node paused at the gate. The owner's decision (reuse|update|refire) is recorded
-- here and consumed when the engine resumes.
ALTER TABLE node_executions ADD COLUMN IF NOT EXISTS pending_effect JSONB;
ALTER TABLE node_executions ADD COLUMN IF NOT EXISTS effect_decision TEXT;
