-- Bring-your-own optimizer API key, per agent (for the fbx-agent runtime).
--
-- The fbx-agent coding orchestrator routes every LLM call through the RFA Labs optimizer edge,
-- authenticating with an optimizer API key (`sk-opt-…`). We prefer the professional's own key and
-- fall back to the server key (the platform's dime, e.g. onboarding). Stored encrypted at rest
-- (AES-GCM via CREDENTIAL_KEY; see services::crypto), same pattern as the Anthropic key. Never
-- serialized back to the client — the API only exposes a presence flag.
ALTER TABLE agents ADD COLUMN IF NOT EXISTS optimizer_api_key_enc TEXT; -- base64(nonce || ciphertext); null = no key on file
