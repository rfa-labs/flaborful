-- Job board redesign: jobs become open postings professionals apply to, rather than a
-- company pre-selecting one agent. A job starts with no agent; the company reviews
-- applicants and selects one, at which point jobs.agent_id is set and the existing
-- trial/active/billing flow takes over unchanged.
--
-- NOTE: `ALTER TYPE ... ADD VALUE` cannot run inside a transaction block alongside use
-- of the new value, so this migration is marked no-transaction.

-- sqlx: no-transaction

-- New job lifecycle state: posted and accepting applications, before an agent is chosen.
ALTER TYPE job_status ADD VALUE IF NOT EXISTS 'reviewing' AFTER 'draft';

-- Application lifecycle.
DO $$ BEGIN
    CREATE TYPE application_status AS ENUM ('applied', 'shortlisted', 'interviewing', 'selected', 'rejected', 'withdrawn');
EXCEPTION WHEN duplicate_object THEN NULL; END $$;

-- A job no longer requires an agent at creation time.
ALTER TABLE jobs ALTER COLUMN agent_id DROP NOT NULL;

-- Posting details the company writes for applicants to read.
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS profession_id UUID REFERENCES professions(id);
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS requirements TEXT NOT NULL DEFAULT '';

-- Applications: a professional's agent applying to a job posting.
CREATE TABLE IF NOT EXISTS job_applications (
    id               UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    job_id           UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    agent_id         UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    professional_id  UUID NOT NULL REFERENCES professionals(id) ON DELETE CASCADE,
    status           application_status NOT NULL DEFAULT 'applied',
    -- Resume text the professional submits (defaults from their resume KB chunk).
    resume_text      TEXT NOT NULL DEFAULT '',
    -- Pitch letter to the company.
    cover_letter     TEXT NOT NULL DEFAULT '',
    -- Optional self-intro video URL (YouTube/Loom/etc.).
    video_url        TEXT,
    -- The exact knowledge-base chunk ids the owner chose to expose for THIS application.
    -- Frozen snapshot: companies see these, never the live KB.
    shared_chunk_ids UUID[] NOT NULL DEFAULT '{}',
    -- Company-set meeting link for an optional interview round (paste a Zoom/Meet URL).
    meeting_link     TEXT,
    meeting_at       TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- One application per agent per job.
    UNIQUE (job_id, agent_id)
);

CREATE INDEX IF NOT EXISTS idx_job_applications_job ON job_applications(job_id);
CREATE INDEX IF NOT EXISTS idx_job_applications_agent ON job_applications(agent_id);
