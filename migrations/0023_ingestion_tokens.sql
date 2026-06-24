-- Agentic code-ingestion runs document a professional's repos into their vault, but they are
-- NOT tied to a job/task (unlike normal work). They still need a per-run token the coding agent
-- presents to the vault MCP tools, resolved to the owning agent_id. Reuse the taste_tokens
-- binding (one token covers both consult_principal and the vault tools) by allowing a token with
-- no task: make task_id nullable. Normal per-task tokens still set it (and keep their uniqueness).
ALTER TABLE taste_tokens ALTER COLUMN task_id DROP NOT NULL;
