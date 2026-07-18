-- Phase 3: broaden likeness-ingestion beyond GitHub. Add provider values so a professional can
-- connect non-code sources (docs, wikis, chat) whose content becomes vault notes — supporting
-- designers/PMs/analysts, not just engineers.
--
-- Google is already a connection provider (it's the Docs delivery target today); this phase also
-- USES it as an ingestion *source*. The others are scaffolding for their OAuth flows + ingesters
-- (Google Drive/Docs is the first non-code source implemented on this).

-- sqlx: no-transaction
-- (ALTER TYPE ... ADD VALUE can't run in the same tx as later use of the value.)

ALTER TYPE connection_provider ADD VALUE IF NOT EXISTS 'notion';
ALTER TYPE connection_provider ADD VALUE IF NOT EXISTS 'slack';
ALTER TYPE connection_provider ADD VALUE IF NOT EXISTS 'linear';
ALTER TYPE connection_provider ADD VALUE IF NOT EXISTS 'jira';
