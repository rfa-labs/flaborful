//! Owner-side OAuth connection storage and token resolution.
//!
//! A `Connection` is the credential the agent uses to act AS its owner on an external
//! provider (GitHub, Google). The company never sees these tokens; it grants access by
//! adding the owner's external account to its resources (a repo, a Shared Drive) and by
//! pointing a `job_integrations` row at the connection.
//!
//! Callers should always go through [`access_token_for_job`] / [`fresh_access_token`]
//! rather than reading the encrypted columns — those accessors transparently refresh
//! expired Google tokens.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::connection::{Connection, ConnectionProvider};
use crate::services::crypto;

/// Inputs for persisting a freshly completed OAuth exchange.
pub struct StoreParams {
    pub user_id: Uuid,
    pub provider: ConnectionProvider,
    pub external_account: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in_secs: Option<i64>,
    pub scopes: String,
}

/// Encrypt and upsert a connection (one per user+provider). Returns the connection id.
pub async fn store(db: &PgPool, p: StoreParams) -> Result<Uuid> {
    let enc_access = crypto::encrypt(&p.access_token)?;
    let enc_refresh = match &p.refresh_token {
        Some(rt) => Some(crypto::encrypt(rt)?),
        None => None,
    };
    let expires_at: Option<DateTime<Utc>> =
        p.expires_in_secs.map(|s| Utc::now() + Duration::seconds(s));

    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO connections
            (user_id, provider, external_account, encrypted_access_token,
             encrypted_refresh_token, access_token_expires_at, scopes)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (user_id, provider) DO UPDATE SET
            external_account        = EXCLUDED.external_account,
            encrypted_access_token  = EXCLUDED.encrypted_access_token,
            -- keep the existing refresh token if the new exchange didn't return one
            -- (Google only returns refresh_token on first consent)
            encrypted_refresh_token = COALESCE(EXCLUDED.encrypted_refresh_token, connections.encrypted_refresh_token),
            access_token_expires_at = EXCLUDED.access_token_expires_at,
            scopes                  = EXCLUDED.scopes,
            updated_at              = NOW()
        RETURNING id
        "#,
        p.user_id,
        p.provider as ConnectionProvider,
        p.external_account,
        enc_access,
        enc_refresh,
        expires_at,
        p.scopes,
    )
    .fetch_one(db)
    .await?;

    Ok(id)
}

/// Load a connection row by id.
pub async fn get(db: &PgPool, connection_id: Uuid) -> Result<Connection> {
    let c = sqlx::query_as!(
        Connection,
        r#"
        SELECT id, user_id, provider as "provider: ConnectionProvider",
               external_account, encrypted_access_token, encrypted_refresh_token,
               access_token_expires_at, scopes, created_at, updated_at
        FROM connections WHERE id = $1
        "#,
        connection_id
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| anyhow!("Connection {} not found", connection_id))?;
    Ok(c)
}

/// Disconnect a user's connection for a provider (e.g. to fix a wrong GitHub account).
/// Returns the number of rows removed (0 if there was nothing connected). Any
/// `job_integrations` referencing it have their `connection_id` set NULL by the FK, so
/// those grants must be re-granted afterwards.
pub async fn delete_for_user(
    db: &PgPool,
    user_id: Uuid,
    provider: ConnectionProvider,
) -> Result<u64> {
    let res = sqlx::query!(
        "DELETE FROM connections WHERE user_id = $1 AND provider = $2",
        user_id,
        provider as ConnectionProvider,
    )
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}

/// Load a user's connection for a provider, if any.
pub async fn get_for_user(
    db: &PgPool,
    user_id: Uuid,
    provider: ConnectionProvider,
) -> Result<Option<Connection>> {
    let c = sqlx::query_as!(
        Connection,
        r#"
        SELECT id, user_id, provider as "provider: ConnectionProvider",
               external_account, encrypted_access_token, encrypted_refresh_token,
               access_token_expires_at, scopes, created_at, updated_at
        FROM connections WHERE user_id = $1 AND provider = $2
        "#,
        user_id,
        provider as ConnectionProvider,
    )
    .fetch_optional(db)
    .await?;
    Ok(c)
}

/// A connection summary safe to expose to the hiring company: which providers the
/// agent's owner has connected, and the account name — but never the tokens.
#[derive(Debug, serde::Serialize)]
pub struct ConnectionSummary {
    pub id: Uuid,
    pub provider: ConnectionProvider,
    pub external_account: String,
}

/// List the connections belonging to the owner of a given agent. The company uses this
/// during onboarding to pick which connection to grant the job access to.
///
/// Chain: agents.professional_id → professionals.user_id → connections.user_id.
pub async fn list_for_agent_owner(db: &PgPool, agent_id: Uuid) -> Result<Vec<ConnectionSummary>> {
    let rows = sqlx::query!(
        r#"
        SELECT c.id, c.provider as "provider: ConnectionProvider", c.external_account
        FROM connections c
        JOIN professionals p ON p.user_id = c.user_id
        JOIN agents a ON a.professional_id = p.id
        WHERE a.id = $1
        ORDER BY c.provider
        "#,
        agent_id
    )
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ConnectionSummary {
            id: r.id,
            provider: r.provider,
            external_account: r.external_account,
        })
        .collect())
}

/// Return a usable access token for a connection, refreshing it first if expired.
/// This is the accessor tools should call.
pub async fn fresh_access_token(db: &PgPool, conn: &Connection) -> Result<String> {
    if conn.is_expired() {
        return refresh(db, conn).await;
    }
    crypto::decrypt(&conn.encrypted_access_token)
}

/// Resolve the access token a job's integration should use for a given provider.
/// Looks up the active `job_integrations` row, follows its `connection_id`, and
/// returns a fresh token. Errors if the company hasn't granted that provider.
pub async fn access_token_for_job(
    db: &PgPool,
    job_id: Uuid,
    provider: ConnectionProvider,
) -> Result<String> {
    let tool_type = match provider {
        ConnectionProvider::Github => "github",
        // Docs/Drive grants are recorded as the google_drive tool_type.
        ConnectionProvider::Google => "google_drive",
        // The non-code likeness sources aren't job-time integration tool_types (they're onboarding
        // ingestion sources, not resources a company grants per job).
        other => return Err(anyhow!("provider {} is not a job integration", other.as_str())),
    };

    let row = sqlx::query!(
        r#"
        SELECT connection_id
        FROM job_integrations
        WHERE job_id = $1 AND tool_type = $2::tool_type AND is_active = true
        ORDER BY created_at DESC
        LIMIT 1
        "#,
        job_id,
        tool_type as &str,
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| {
        anyhow!(
            "No active {} integration granted for job {}",
            tool_type,
            job_id
        )
    })?;

    let connection_id = row.connection_id.ok_or_else(|| {
        anyhow!(
            "{} integration for job {} has no connection linked",
            tool_type,
            job_id
        )
    })?;

    let conn = get(db, connection_id).await?;
    fresh_access_token(db, &conn).await
}

#[derive(Debug, Deserialize)]
struct GoogleRefreshResponse {
    access_token: String,
    expires_in: i64,
}

/// Exchange the refresh token for a new access token and persist it. Google only.
async fn refresh(db: &PgPool, conn: &Connection) -> Result<String> {
    if conn.provider != ConnectionProvider::Google {
        // GitHub OAuth tokens don't expire; nothing to refresh.
        return crypto::decrypt(&conn.encrypted_access_token);
    }

    let refresh_enc = conn.encrypted_refresh_token.as_ref().ok_or_else(|| {
        anyhow!(
            "Google connection {} has no refresh token; re-auth required",
            conn.id
        )
    })?;
    let refresh_token = crypto::decrypt(refresh_enc)?;

    let client_id =
        std::env::var("GOOGLE_CLIENT_ID").map_err(|_| anyhow!("GOOGLE_CLIENT_ID not set"))?;
    let client_secret = std::env::var("GOOGLE_CLIENT_SECRET")
        .map_err(|_| anyhow!("GOOGLE_CLIENT_SECRET not set"))?;

    let res: GoogleRefreshResponse = reqwest::Client::new()
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("refresh_token", refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let enc_access = crypto::encrypt(&res.access_token)?;
    let expires_at = Utc::now() + Duration::seconds(res.expires_in);

    sqlx::query!(
        r#"UPDATE connections
           SET encrypted_access_token = $1, access_token_expires_at = $2, updated_at = NOW()
           WHERE id = $3"#,
        enc_access,
        expires_at,
        conn.id,
    )
    .execute(db)
    .await?;

    Ok(res.access_token)
}
