-- The task SSE feed (GET /events/tasks/:job_id) needs a monotonic "this row changed"
-- timestamp to drive incremental polling. It previously keyed off created_at, which is
-- immutable, so the company saw each task once at first-poll status and never received the
-- running -> awaiting_review -> delivered transitions. Add an updated_at column that a
-- trigger bumps on every UPDATE, so the feed can track MAX(updated_at) instead.

ALTER TABLE tasks ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

-- Backfill: existing rows get their last known activity (or creation) time so the cursor
-- starts sane.
UPDATE tasks
   SET updated_at = COALESCE(delivered_at, reviewed_at, created_at);

CREATE OR REPLACE FUNCTION set_tasks_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_tasks_updated_at
    BEFORE UPDATE ON tasks
    FOR EACH ROW
    EXECUTE FUNCTION set_tasks_updated_at();
