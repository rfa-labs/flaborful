-- Phase 6: workflow checkpoints — version history for a workflow's graph.
--
-- Today update_workflow overwrites workflows.graph in place with no history. A checkpoint is an
-- immutable snapshot of the graph you can make-live or roll back to (Gumloop's model). The
-- live/editable graph stays in workflows.graph; checkpoints are archived snapshots.

CREATE TABLE IF NOT EXISTS workflow_checkpoints (
    id          UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    workflow_id UUID NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    -- Monotonic per-workflow checkpoint number, shown in the picker.
    number      INT NOT NULL,
    -- Immutable snapshot of the graph at save time.
    graph       JSONB NOT NULL,
    -- Optional human label ("before the loop refactor").
    label       TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_wf_checkpoints ON workflow_checkpoints(workflow_id, number DESC);
