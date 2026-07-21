-- Bring-your-own Anthropic API key, per agent.
--
-- When a professional runs work on "Claude Code Managed Cloud" (the Managed backend), the run
-- bills against THEIR own Anthropic/Claude key, not our shared server key — so it must be stored
-- per agent. The "flaborful sandboxes" (Sprite) backend prefers this key too, falling back to the
-- server key (the premium "use ours" path).
--
-- Stored encrypted at rest (AES-GCM via CREDENTIAL_KEY; see services::crypto), same pattern as
-- OAuth tokens. Never serialized back to the client — the API only exposes a presence flag.
ALTER TABLE agents ADD COLUMN IF NOT EXISTS anthropic_api_key_enc TEXT; -- base64(nonce || ciphertext); null = no key on file
