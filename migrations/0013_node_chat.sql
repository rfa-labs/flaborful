-- Per-node chat: a dialogue between the professional and their agent about the work a
-- specific node produced. One conversation per (task, node). The agent's replies become
-- corrections; applying one updates that node's output and marks downstream nodes dirty
-- so the owner can re-run from there. Conversations also feed the knowledge base
-- (ChatCorrection chunks) so the agent keeps improving from supervised corrections.

CREATE TABLE node_conversations (
    id          UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    task_id     UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    node_id     TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (task_id, node_id)
);

CREATE TABLE node_messages (
    id              UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    conversation_id UUID NOT NULL REFERENCES node_conversations(id) ON DELETE CASCADE,
    role            TEXT NOT NULL,    -- user | assistant
    content         TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_node_messages_conversation ON node_messages(conversation_id, created_at);
