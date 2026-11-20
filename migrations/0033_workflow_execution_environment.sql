-- Workflow-level execution environment for SWE workflows.
--
-- The environment (managed cloud / Claude Code BYOK / optimizer-routed fbx-agent) is chosen ONCE
-- per workflow and applied to every sprite node in a run — so the fbx-agent's durable session can
-- span the whole DAG without a foreign coding agent stomping the shared working tree. It is NOT a
-- per-node choice.
--
-- Nullable: null means "use the platform default" (the optimizer-routed fbx-agent — the cheap path).
-- Values: 'managed_cloud' | 'claude_code_byok' | 'optimizer_fbx'.
ALTER TABLE workflows ADD COLUMN IF NOT EXISTS execution_environment TEXT;
