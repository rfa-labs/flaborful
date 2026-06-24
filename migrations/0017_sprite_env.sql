-- Test-env strategy grant for the Sprite backend.
--
-- The company declares how the agent's sprite is provisioned for a job — what extra deps,
-- network egress, or cloud creds it gets — as a `job_integrations` row of tool_type='sprite_env'.
-- scope_config carries { strategy, compose_file?, egress_urls?, env? } (see sprite_env.rs).
--
-- ALTER TYPE ... ADD VALUE can't run in the same tx as use of the new value, so no-transaction.
-- sqlx: no-transaction
ALTER TYPE tool_type ADD VALUE IF NOT EXISTS 'sprite_env' AFTER 'custom_api';
