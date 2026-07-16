use axum::{
    async_trait,
    extract::FromRequestParts,
    http::{request::Parts, HeaderMap},
};
use jsonwebtoken::{decode, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::errors::AppError;
use crate::state::AppState;
use axum::extract::FromRef;

fn extract_token_from_query(uri: &axum::http::Uri) -> Option<String> {
    let query = uri.query()?;
    for pair in query.split('&') {
        if let Some(token) = pair.strip_prefix("token=") {
            return Some(token.to_string());
        }
    }
    None
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String, // user_id
    pub role: String,
    pub exp: usize,
}

/// Axum extractor that decodes the Bearer JWT from Authorization header.
/// Inject `AuthUser` as a handler parameter to require authentication.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    #[allow(dead_code)] // JWT claim surfaced for handlers; authz is ownership-based today
    pub role: String,
}

#[async_trait]
impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, AppError> {
        let token = extract_bearer(&parts.headers)
            .or_else(|| extract_token_from_query(&parts.uri))
            .ok_or(AppError::Unauthorized)?;

        // Same validated key the signing path uses. A missing JWT_SECRET is an operator error,
        // not a bad token, so it surfaces as Internal rather than Unauthorized — and startup
        // already refuses to boot without it (see `validate_jwt_secret`).
        let secret = crate::services::auth::jwt_secret().map_err(AppError::Internal)?;
        let decoded = decode::<Claims>(
            &token,
            &DecodingKey::from_secret(secret.as_bytes()),
            &Validation::default(),
        )
        .map_err(|_| AppError::Unauthorized)?;

        let user_id = Uuid::parse_str(&decoded.claims.sub).map_err(|_| AppError::Unauthorized)?;

        let app_state = AppState::from_ref(state);
        let exists =
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
                .bind(user_id)
                .fetch_one(&app_state.db)
                .await
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

        if !exists {
            return Err(AppError::Unauthorized);
        }

        Ok(AuthUser {
            user_id,
            role: decoded.claims.role,
        })
    }
}

fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("authorization")?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?;
    Some(token.to_string())
}
