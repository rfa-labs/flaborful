use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// An external provider the agent's owner has authorized the platform to act on
/// their behalf with. One per (user, provider).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type, PartialEq)]
#[sqlx(type_name = "connection_provider", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ConnectionProvider {
    Github,
    Google,
    // Non-code likeness sources (Phase 3). OAuth flows are added per provider; Google Drive/Docs is
    // the first ingestion source implemented on this. See [[Flaborful Likeness Ingestion Sources]].
    Notion,
    Slack,
    Linear,
    Jira,
}

impl ConnectionProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConnectionProvider::Github => "github",
            ConnectionProvider::Google => "google",
            ConnectionProvider::Notion => "notion",
            ConnectionProvider::Slack => "slack",
            ConnectionProvider::Linear => "linear",
            ConnectionProvider::Jira => "jira",
        }
    }

    /// Parse a provider slug (from a route path). Returns None for unknown.
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "github" => ConnectionProvider::Github,
            "google" => ConnectionProvider::Google,
            "notion" => ConnectionProvider::Notion,
            "slack" => ConnectionProvider::Slack,
            "linear" => ConnectionProvider::Linear,
            "jira" => ConnectionProvider::Jira,
            _ => return None,
        })
    }
}

/// Owner-side OAuth connection. Tokens are stored encrypted; see services::connections
/// for the decrypt/refresh accessors — callers should never read the encrypted columns
/// directly.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Connection {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider: ConnectionProvider,
    /// github login or google email — for display/attribution.
    pub external_account: String,
    #[serde(skip)]
    pub encrypted_access_token: String,
    #[serde(skip)]
    pub encrypted_refresh_token: Option<String>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub scopes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Connection {
    /// True when the access token is expired (or within a 60s skew window).
    pub fn is_expired(&self) -> bool {
        match self.access_token_expires_at {
            None => false, // non-expiring (GitHub)
            Some(exp) => Utc::now() + chrono::Duration::seconds(60) >= exp,
        }
    }

    #[allow(dead_code)] // scope check helper; not yet consulted before scoped integration calls
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.split_whitespace().any(|s| s == scope)
    }
}
