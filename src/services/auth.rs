use chrono::Utc;
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::{AppError, AppResult};
use crate::models::user::{AuthResponse, LoginRequest, RegisterRequest, User};

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String, // user_id
    role: String,
    exp: usize,
}

pub async fn register(db: &PgPool, req: RegisterRequest) -> AppResult<AuthResponse> {
    let existing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE email = $1")
        .bind(&req.email)
        .fetch_one(db)
        .await?;

    if existing > 0 {
        return Err(AppError::BadRequest("Email already registered".into()));
    }

    let password_hash = bcrypt::hash(&req.password, bcrypt::DEFAULT_COST)
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    let user = sqlx::query_as::<_, User>(
        r#"INSERT INTO users (id, email, password_hash, role, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $5)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(&req.email)
    .bind(&password_hash)
    .bind(&req.role)
    .bind(Utc::now())
    .fetch_one(db)
    .await?;

    let token = make_token(&user)?;
    Ok(AuthResponse {
        token,
        user_id: user.id,
        role: user.role,
    })
}

pub async fn login(db: &PgPool, req: LoginRequest) -> AppResult<AuthResponse> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE email = $1")
        .bind(&req.email)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::Unauthorized)?;

    let valid = bcrypt::verify(&req.password, &user.password_hash)
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    if !valid {
        return Err(AppError::Unauthorized);
    }

    let token = make_token(&user)?;
    Ok(AuthResponse {
        token,
        user_id: user.id,
        role: user.role,
    })
}

/// The JWT signing key. Like [`crate::services::crypto`]'s `CREDENTIAL_KEY`, there is **no**
/// default: a missing `JWT_SECRET` used to fall back to the literal `"dev_secret"` in both the
/// signing and verifying paths, which meant a deploy that forgot the var would sign and accept
/// every session token with a publicly-known key — anyone could forge a JWT for any user. Read
/// once per process; rotating the secret requires a restart (and invalidates live sessions).
pub fn jwt_secret() -> Result<&'static str, anyhow::Error> {
    use std::sync::OnceLock;
    static SECRET: OnceLock<Option<String>> = OnceLock::new();
    SECRET
        .get_or_init(|| std::env::var("JWT_SECRET").ok().filter(|s| !s.is_empty()))
        .as_deref()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "JWT_SECRET must be set to a non-empty value (generate one with `openssl rand -base64 32`)"
            )
        })
}

/// Validate that `JWT_SECRET` is present without minting a token. Called at startup so a
/// misconfigured deploy fails immediately rather than on the first login.
pub fn validate_jwt_secret() -> Result<(), anyhow::Error> {
    jwt_secret().map(|_| ())
}

fn make_token(user: &User) -> AppResult<String> {
    let secret = jwt_secret().map_err(AppError::Internal)?;
    let exp = (Utc::now() + chrono::Duration::days(30)).timestamp() as usize;
    let claims = Claims {
        sub: user.id.to_string(),
        role: format!("{:?}", user.role).to_lowercase(),
        exp,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))
}
