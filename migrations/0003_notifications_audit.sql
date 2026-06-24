-- Notifications
CREATE TYPE notification_type AS ENUM (
    'task_awaiting_review',
    'task_delivered',
    'task_failed',
    'job_started',
    'job_completed',
    'agent_training_done'
);

CREATE TABLE notifications (
    id          UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    type        notification_type NOT NULL,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL,
    payload     JSONB NOT NULL DEFAULT '{}',
    read        BOOLEAN NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX notifications_user_unread ON notifications (user_id, read, created_at DESC);

-- Audit log: every significant platform event for a job
CREATE TABLE audit_events (
    id          UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    job_id      UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    actor_type  TEXT NOT NULL,  -- 'agent', 'professional', 'company', 'system'
    actor_id    UUID,
    event_type  TEXT NOT NULL,
    summary     TEXT NOT NULL,
    payload     JSONB NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX audit_events_job ON audit_events (job_id, created_at DESC);
