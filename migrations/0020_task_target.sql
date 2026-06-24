-- A task needs a structured work-target so a generic, reusable workflow knows what it is
-- operating on. For SWE work that means at least a repo and base branch; previously the repo
-- was baked into the workflow's AgentImplement node config, which pinned one workflow per repo
-- and left a description-only task with no idea what to act on.
--
-- target is injected into the execution context at dispatch (target.repo -> ctx.vars["repo"],
-- target.base_branch -> ctx.vars["base_branch"], etc.), which the AgentImplement / PrCreate
-- nodes already read. Non-SWE tasks simply leave it empty.
ALTER TABLE tasks ADD COLUMN target JSONB NOT NULL DEFAULT '{}';
