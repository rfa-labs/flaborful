-- Timeline of knowledge-base changes. Qdrant chunks carry no timestamps and are gone
-- once deleted, so this Postgres table is the durable source of truth for "what was
-- added/removed from my knowledge base and when". A `deleted` event survives the chunk
-- it refers to, so the timeline stays complete.
CREATE TABLE knowledge_events (
    id          UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    agent_id    UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    chunk_id    UUID NOT NULL,
    event_type  TEXT NOT NULL,      -- added | deleted
    source_type TEXT NOT NULL,
    summary     TEXT,               -- short content preview for display
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_knowledge_events_agent ON knowledge_events(agent_id, created_at DESC);
