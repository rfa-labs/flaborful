-- A user can KILL a workflow execution from the human gate (e.g. the implementation is wrong and
-- they'd rather start fresh later). That terminal state must be distinct from 'failed' (a crash)
-- so a deliberate cancel is visibly different in the UI.
ALTER TYPE task_status ADD VALUE IF NOT EXISTS 'cancelled';
