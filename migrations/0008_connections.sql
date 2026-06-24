-- Generalize owner-side OAuth credentials into a provider-agnostic "connections" model.
-- A Connection is owned by a user (the agent's owner / the professional) and holds the
-- OAuth tokens the agent uses to act AS that person on an external provider.
--
-- This supersedes user_github_tokens, which was GitHub-specific. The two-sided model is:
--   Connection (owner authorizes once, here)  +  job_integrations.connection_id (company grants per job).

CREATE TYPE connection_provider AS ENUM ('github', 'google');

CREATE TABLE connections (
    id                     UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id                UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider               connection_provider NOT NULL,
    -- Provider account identity, e.g. github login or google email. Display/attribution only.
    external_account       TEXT NOT NULL,
    -- AES-GCM encrypted access token (base64 nonce||ciphertext, see services::crypto).
    encrypted_access_token TEXT NOT NULL,
    -- AES-GCM encrypted refresh token. NULL for providers whose tokens don't expire (GitHub).
    encrypted_refresh_token TEXT,
    -- When the access token expires. NULL = non-expiring (GitHub).
    access_token_expires_at TIMESTAMPTZ,
    -- Granted OAuth scopes, space-joined, for capability checks before invoking a tool.
    scopes                 TEXT NOT NULL DEFAULT '',
    created_at             TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- One connection per (user, provider). Re-authing upserts.
    UNIQUE (user_id, provider)
);

-- Migrate existing GitHub tokens into the generalized table.
INSERT INTO connections (user_id, provider, external_account, encrypted_access_token, scopes, created_at, updated_at)
SELECT user_id, 'github'::connection_provider, github_login, encrypted_token, 'repo', created_at, updated_at
FROM user_github_tokens
ON CONFLICT (user_id, provider) DO NOTHING;

DROP TABLE user_github_tokens;

-- The company grant now references the owner-side connection it draws credentials from.
-- job_integrations already exists (per-job, company-granted, with scope_config).
-- Adding connection_id makes the link to the owner's Connection explicit; encrypted_credentials
-- remains for grants that carry their own creds (e.g. a company-provided Slack bot token).
ALTER TABLE job_integrations
    ADD COLUMN connection_id UUID REFERENCES connections(id) ON DELETE SET NULL;

-- encrypted_credentials was NOT NULL; connection-backed grants don't need it.
ALTER TABLE job_integrations
    ALTER COLUMN encrypted_credentials DROP NOT NULL;
