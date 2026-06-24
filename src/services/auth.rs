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

fn make_token(user: &User) -> AppResult<String> {
    let secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| "dev_secret".into());
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
