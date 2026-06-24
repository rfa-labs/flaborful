-- Sprite checkpoint id captured before a sprite-mutating node runs, so a later rerun of that
-- node can restore the *exact* pre-node filesystem (precision rerun reconciliation) instead of
-- the coarser git-anchored reset. Null when no checkpoint was taken (checkpoints unsupported,
-- or a non-sprite node) — the engine falls back to git reset in that case.
ALTER TABLE node_executions ADD COLUMN IF NOT EXISTS node_checkpoint_id TEXT;
