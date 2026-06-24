-- Per-node execution records. The workflow engine was previously recursive with all
-- intermediate node outputs held only in memory (ExecutionContext.vars), lost on a
-- review pause. This table makes per-node input/output durable, which enables:
--   - resuming a workflow past a review gate (rebuild context from persisted outputs),
--   - re-running from an arbitrary node with the prior upstream outputs,
--   - a DAG visualization showing each node's status, input, output, and logs.
--
-- `attempt` is bumped on re-run so history is preserved rather than overwritten.
-- `dirty` marks a node whose output was edited (via chat correction) but not yet re-run.

CREATE TABLE node_executions (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    task_id      UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    node_id      TEXT NOT NULL,          -- WorkflowNode.id within the graph
    node_type    TEXT NOT NULL,
    status       TEXT NOT NULL,          -- running | completed | failed | awaiting_review | stale
    output_key   TEXT,                   -- the ctx.vars key this node wrote (for replay)
    input_json   JSONB,                  -- snapshot of the vars this node read
    output_json  JSONB,                  -- the node's output value
    logs         TEXT,
    attempt      INT NOT NULL DEFAULT 1, -- incremented on re-run; history retained
    dirty        BOOLEAN NOT NULL DEFAULT false,
    started_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at  TIMESTAMPTZ
);

CREATE INDEX idx_node_exec_task ON node_executions(task_id);
CREATE UNIQUE INDEX idx_node_exec_task_node_attempt ON node_executions(task_id, node_id, attempt);
