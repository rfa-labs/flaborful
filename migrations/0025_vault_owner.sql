-- Composable knowledge vaults: a vault is no longer always a professional's. Companies get
-- their own vaults too (per-repo knowledge graphs of their codebases). We generalize the vault
-- owner with an `owner_kind` discriminator alongside the existing id column.
--
-- Design choice (low-churn): the existing `agent_id` column stays as the generic OWNER-ID
-- carrier — for `owner_kind='agent'` it's the agent id (unchanged); for `owner_kind='company'`
-- it holds the company id. So existing queries keep working by adding `owner_kind` to their
-- predicate, and the Qdrant chunk's `agent_id` payload keeps meaning "owner id" (application
-- snapshots, which filter on it, are untouched for the professional path). To allow company ids
-- in that column we drop the FK→agents; referential integrity moves to the app layer (authz
-- resolves and verifies the owner before any vault write). Constraints are dropped by lookup so
-- we don't depend on Postgres's auto-generated names.

-- Helper: drop the FK on (table, column) regardless of its generated name.
CREATE OR REPLACE FUNCTION _drop_fk_on(p_table regclass, p_column text) RETURNS void AS $$
DECLARE c text;
BEGIN
    SELECT con.conname INTO c
    FROM pg_constraint con
    JOIN pg_attribute att ON att.attrelid = con.conrelid AND att.attnum = ANY(con.conkey)
    WHERE con.conrelid = p_table AND con.contype = 'f' AND att.attname = p_column
    LIMIT 1;
    IF c IS NOT NULL THEN
        EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', p_table, c);
    END IF;
END;
$$ LANGUAGE plpgsql;

-- Helper: drop a UNIQUE constraint covering exactly the given columns, by lookup.
CREATE OR REPLACE FUNCTION _drop_unique_on(p_table regclass, p_columns text[]) RETURNS void AS $$
DECLARE c text;
BEGIN
    SELECT con.conname INTO c
    FROM pg_constraint con
    WHERE con.conrelid = p_table AND con.contype = 'u'
      AND (SELECT array_agg(att.attname::text ORDER BY att.attname::text)
           FROM pg_attribute att
           WHERE att.attrelid = con.conrelid AND att.attnum = ANY(con.conkey))
          = (SELECT array_agg(x ORDER BY x) FROM unnest(p_columns) x)
    LIMIT 1;
    IF c IS NOT NULL THEN
        EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', p_table, c);
    END IF;
END;
$$ LANGUAGE plpgsql;

-- ── vault_notes ─────────────────────────────────────────────────────────────────────────────
ALTER TABLE vault_notes ADD COLUMN owner_kind TEXT NOT NULL DEFAULT 'agent';
ALTER TABLE vault_notes
    ADD CONSTRAINT vault_notes_owner_kind_chk CHECK (owner_kind IN ('agent', 'company'));
SELECT _drop_fk_on('vault_notes', 'agent_id');
SELECT _drop_unique_on('vault_notes', ARRAY['agent_id', 'slug']);
ALTER TABLE vault_notes ADD CONSTRAINT vault_notes_owner_slug_key UNIQUE (owner_kind, agent_id, slug);
DROP INDEX IF EXISTS idx_vault_notes_agent;
CREATE INDEX idx_vault_notes_owner ON vault_notes(owner_kind, agent_id);

-- ── knowledge_events (timeline) ─────────────────────────────────────────────────────────────
ALTER TABLE knowledge_events ADD COLUMN owner_kind TEXT NOT NULL DEFAULT 'agent';
SELECT _drop_fk_on('knowledge_events', 'agent_id');
DROP INDEX IF EXISTS idx_knowledge_events_agent;
CREATE INDEX idx_knowledge_events_owner
    ON knowledge_events(owner_kind, agent_id, created_at DESC);

-- ── ingestion_events (per-repo progress feed) ───────────────────────────────────────────────
ALTER TABLE ingestion_events ADD COLUMN owner_kind TEXT NOT NULL DEFAULT 'agent';
SELECT _drop_fk_on('ingestion_events', 'agent_id');
DROP INDEX IF EXISTS ingestion_events_agent_time_idx;
CREATE INDEX ingestion_events_owner_time_idx
    ON ingestion_events (owner_kind, agent_id, created_at);

-- ── taste_tokens: carry the vault owner + per-task overlay ──────────────────────────────────
-- A token now resolves to a VAULT OWNER (agent or company), not strictly an agent. Company
-- ingestion runs mint a company-owned token with no persona/LoRA, so make those nullable and
-- add the owner discriminator. The agent_id column stays as the owner-id carrier. For task
-- composition a per-task token also records read-only OVERLAY vault(s) — the company repo
-- vault(s) attached to the hired task, scoped to specific repos — as JSONB:
--   [{ "owner_kind": "company", "owner_id": "<uuid>", "repos": ["owner/name", ...] }]
ALTER TABLE taste_tokens ADD COLUMN owner_kind TEXT NOT NULL DEFAULT 'agent';
ALTER TABLE taste_tokens ALTER COLUMN persona_prompt DROP NOT NULL;
SELECT _drop_fk_on('taste_tokens', 'agent_id');
ALTER TABLE taste_tokens ADD COLUMN overlays JSONB NOT NULL DEFAULT '[]'::jsonb;

-- Clean up helpers.
DROP FUNCTION _drop_fk_on(regclass, text);
DROP FUNCTION _drop_unique_on(regclass, text[]);
