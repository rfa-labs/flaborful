-- The platform default coding agent is fbx-agent (flaborful's own DAG-aware orchestrator, routing
-- every LLM call through the optimizer — the cheap, first-class path), not Claude Code.
--
-- Migration 0015 defaulted `agent_runtime` to 'claude_code'. Change the column default to
-- 'fbx_agent' so newly created agents get it, AND flip existing agents that are still on the old
-- default so the platform default applies everywhere (pre-launch; agents are test data). Agents
-- that explicitly chose 'codex' or 'custom' are left untouched.
-- fbx-agent runs on the Sprite backend, so the default backend must be 'sprite' too — otherwise a
-- new agent defaults to Managed (Anthropic-hosted) and the fbx-agent runtime is ignored for coding
-- tasks. Set BOTH column defaults, and flip existing agents still on the old defaults.
ALTER TABLE agents ALTER COLUMN agent_runtime SET DEFAULT 'fbx_agent';
ALTER TABLE agents ALTER COLUMN execution_backend SET DEFAULT 'sprite';
UPDATE agents SET agent_runtime = 'fbx_agent' WHERE agent_runtime = 'claude_code';
UPDATE agents SET execution_backend = 'sprite' WHERE execution_backend = 'managed';
