-- One ephemeral Fly.io Sprite per task (the agent's working machine for SWE jobs).
-- Created lazily on the first sprite-backed node, destroyed on task terminal (and as a
-- safety net on job complete/kill). head_sha anchors rerun filesystem reconciliation.
CREATE TABLE task_sprites (
    id                 UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    task_id            UUID NOT NULL UNIQUE REFERENCES tasks(id) ON DELETE CASCADE,
    sprite_name        TEXT,                            -- Fly Sprites name; null until created
    sprite_url         TEXT,                            -- the sprite's HTTP URL
    status             TEXT NOT NULL DEFAULT 'creating', -- creating | running | suspended | destroyed | gone
    test_env_strategy  TEXT NOT NULL DEFAULT 'none',
    repo               TEXT,
    branch             TEXT,
    head_sha           TEXT,
    last_checkpoint_id TEXT,                            -- Phase 5 (checkpoint/restore)
    created_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    destroyed_at       TIMESTAMPTZ
);

CREATE INDEX idx_task_sprites_task ON task_sprites(task_id);
