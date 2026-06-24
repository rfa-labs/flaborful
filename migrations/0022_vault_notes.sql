-- The professional's knowledge base becomes a markdown-source-of-truth vault: one row per
-- note. This table is the SOURCE OF TRUTH; the Qdrant `flaborful_knowledge` collection is
-- demoted to a DERIVED, rebuildable index (a note's id == its derived chunk id, 1:1, so the
-- existing application-snapshot sharing via `applications.shared_chunk_ids` /
-- `fetch_chunks_by_ids` keeps resolving). The `knowledge_events` timeline is unchanged.
--
-- Frontmatter is stored as typed, indexable columns (not parsed from the body each time) so
-- retrieval can metadata-filter (e.g. golden_snippets in domain=backend) — the key
-- agentic-RAG lever. `related` is the explicit `[[wikilink]]` adjacency list: it drives both
-- the agent's link-traversal retrieval (Graph-RAG one-hop) and the Obsidian-style graph view.
CREATE TABLE vault_notes (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    agent_id     UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    slug         TEXT NOT NULL,                 -- kebab note name, unique per agent
    title        TEXT NOT NULL,
    body         TEXT NOT NULL,                 -- raw markdown body (below frontmatter)
    -- typed layer (style_rule | golden_snippet | architecture | feature | pattern | tech | note)
    layer        TEXT NOT NULL DEFAULT 'note',
    source_type  TEXT NOT NULL,                 -- mirrors KnowledgeSourceType (portfolio/resume/…)
    visibility   TEXT NOT NULL DEFAULT 'private',
    domain       TEXT,                          -- fixed taxonomy, validated app-side
    related      TEXT[] NOT NULL DEFAULT '{}',  -- [[wikilink]] slugs (graph edges)
    repo         TEXT,                          -- source repo for code-derived notes
    job_id       UUID,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (agent_id, slug)
);

CREATE INDEX idx_vault_notes_agent ON vault_notes(agent_id);

-- Keyword/BM25 half of hybrid retrieval, generated from title + body.
ALTER TABLE vault_notes ADD COLUMN tsv tsvector
    GENERATED ALWAYS AS (to_tsvector('english', title || ' ' || body)) STORED;
CREATE INDEX idx_vault_notes_tsv ON vault_notes USING GIN(tsv);

-- Fast backlinks for the graph view + reverse link-traversal: `WHERE $slug = ANY(related)`.
CREATE INDEX idx_vault_notes_related ON vault_notes USING GIN(related);
