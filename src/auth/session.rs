//! Login sessions as opaque bearer tokens.
//!
//! A token is 32 random bytes, shown to the client once. The database keeps
//! only its SHA-256, so even a full database leak yields no usable sessions.
//! Unlike JWTs, a session can be revoked instantly by deleting its row.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{Role, User};

/// Prefix that makes leaked tokens easy to spot in logs and for secret scanners.
const TOKEN_PREFIX: &str = "gnk_";

/// A freshly issued session. The only time the plain token exists.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct NewSession {
    pub token: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Opens a session for `user_id`, valid for `ttl_hours`.
pub async fn create(conn: &mut PgConnection, user_id: Uuid, ttl_hours: i64) -> Result<NewSession, sqlx::Error> {
    let token = generate_token();
    let expires_at = OffsetDateTime::now_utc() + Duration::hours(ttl_hours);
    sqlx::query!(
        "INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, $3)",
        hash_token(&token),
        user_id,
        expires_at,
    )
    .execute(conn)
    .await?;
    Ok(NewSession { token, expires_at })
}

/// Finds the user behind a token, if the session exists and hasn't expired.
pub async fn lookup(db: &PgPool, token: &str) -> Result<Option<User>, sqlx::Error> {
    if !token.starts_with(TOKEN_PREFIX) {
        return Ok(None);
    }
    sqlx::query_as!(
        User,
        r#"SELECT u.id, u.email, u.role AS "role: Role", u.created_at
           FROM sessions s JOIN users u ON u.id = s.user_id
           WHERE s.token_hash = $1 AND s.expires_at > now()"#,
        hash_token(token)
    )
    .fetch_optional(db)
    .await
}

/// Ends a session (logout). Unknown tokens are ignored.
pub async fn revoke(db: &PgPool, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM sessions WHERE token_hash = $1", hash_token(token))
        .execute(db)
        .await?;
    Ok(())
}
