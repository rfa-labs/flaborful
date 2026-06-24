-- Extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- Enums
CREATE TYPE user_role AS ENUM ('professional', 'company', 'admin');
CREATE TYPE agent_status AS ENUM ('onboarding', 'training', 'ready', 'deployed', 'suspended');
CREATE TYPE job_status AS ENUM ('draft', 'open', 'trial', 'active', 'completed', 'cancelled');
CREATE TYPE task_status AS ENUM ('pending', 'running', 'awaiting_review', 'approved', 'amended', 'rejected', 'delivered', 'failed');
CREATE TYPE tool_type AS ENUM ('github', 'slack', 'jira', 'linear', 'google_drive', 'notion', 'email', 'custom_api');

-- Users
CREATE TABLE users (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    email        TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role         user_role NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Professions (platform-defined, seeded)
CREATE TABLE professions (
    id          UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name        TEXT NOT NULL UNIQUE,
    slug        TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Aspects (skill areas per profession)
CREATE TABLE aspects (
    id            UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    profession_id UUID NOT NULL REFERENCES professions(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    slug          TEXT NOT NULL,
    description   TEXT NOT NULL DEFAULT '',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (profession_id, slug)
);

-- Professionals (one per user, one profession)
CREATE TABLE professionals (
    id               UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id          UUID NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    profession_id    UUID NOT NULL REFERENCES professions(id),
    name             TEXT NOT NULL,
    bio              TEXT NOT NULL DEFAULT '',
    avatar_url       TEXT,
    hourly_rate      INTEGER,         -- cents
    years_experience SMALLINT,
    is_available     BOOLEAN NOT NULL DEFAULT true,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Agents (one per professional)
CREATE TABLE agents (
    id                    UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    professional_id       UUID NOT NULL UNIQUE REFERENCES professionals(id) ON DELETE CASCADE,
    status                agent_status NOT NULL DEFAULT 'onboarding',
    style_guide           JSONB,
    persona_system_prompt TEXT,
    lora_adapter_path     TEXT,
    lora_training_job_id  TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Which aspects an agent offers
CREATE TABLE agent_aspects (
    agent_id   UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    aspect_id  UUID NOT NULL REFERENCES aspects(id) ON DELETE CASCADE,
    is_offered BOOLEAN NOT NULL DEFAULT true,
    PRIMARY KEY (agent_id, aspect_id)
);

-- Companies
CREATE TABLE companies (
    id         UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id    UUID NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    industry   TEXT,
    website    TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Jobs
CREATE TABLE jobs (
    id          UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    company_id  UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    agent_id    UUID NOT NULL REFERENCES agents(id),
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status      job_status NOT NULL DEFAULT 'draft',
    hourly_rate INTEGER NOT NULL,  -- cents
    started_at  TIMESTAMPTZ,
    ended_at    TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Tool integrations granted per job by the company
CREATE TABLE job_integrations (
    id                     UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    job_id                 UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    tool_type              tool_type NOT NULL,
    encrypted_credentials  TEXT NOT NULL,  -- AES-GCM encrypted JSON
    scope_config           JSONB NOT NULL DEFAULT '{}',
    is_active              BOOLEAN NOT NULL DEFAULT true,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Workflows (node graphs)
CREATE TABLE workflows (
    id           UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    agent_id     UUID NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    aspect_id    UUID REFERENCES aspects(id),
    name         TEXT NOT NULL,
    description  TEXT,
    graph        JSONB NOT NULL DEFAULT '{"nodes":[],"edges":[]}',
    is_universal BOOLEAN NOT NULL DEFAULT true,
    job_id       UUID REFERENCES jobs(id),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Tasks (units of work the agent executes)
CREATE TABLE tasks (
    id              UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    job_id          UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    workflow_id     UUID REFERENCES workflows(id),
    input           JSONB NOT NULL DEFAULT '{}',
    status          task_status NOT NULL DEFAULT 'pending',
    confidence_score REAL,
    output_draft    TEXT,
    output_approved TEXT,
    current_node_id TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    reviewed_at     TIMESTAMPTZ,
    delivered_at    TIMESTAMPTZ
);

-- RLHF: every human amendment to agent output
CREATE TABLE rlhf_events (
    id               UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    task_id          UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    original_output  TEXT NOT NULL,
    amended_output   TEXT NOT NULL,
    amendment_notes  TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Seed: Software Engineer profession + aspects
INSERT INTO professions (id, name, slug, description) VALUES
    ('a0000000-0000-0000-0000-000000000001', 'Software Engineer', 'software_engineer',
     'Designs, builds, and maintains software systems.');

INSERT INTO aspects (id, profession_id, name, slug, description) VALUES
    ('b0000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-000000000001', 'Code Review', 'code_review', 'Evaluating pull requests for correctness, style, and security.'),
    ('b0000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-000000000001', 'Refactoring', 'refactoring', 'Restructuring existing code to improve quality without changing behavior.'),
    ('b0000000-0000-0000-0000-000000000003', 'a0000000-0000-0000-0000-000000000001', 'Debugging', 'debugging', 'Tracing and resolving runtime errors and unexpected behavior.'),
    ('b0000000-0000-0000-0000-000000000004', 'a0000000-0000-0000-0000-000000000001', 'System Design', 'system_design', 'Making architectural decisions for scalable, maintainable systems.'),
    ('b0000000-0000-0000-0000-000000000005', 'a0000000-0000-0000-0000-000000000001', 'Documentation', 'documentation', 'Writing clear technical documentation, READMEs, and API docs.');
