//! First-time setup: creating the very first admin account.
//!
//! A fresh install has no accounts, so nobody can log in to create one. On
//! startup we generate a one-time setup token and print it to the logs. Whoever
//! can read the server logs proves they operate the server, and can trade the
//! token for the first admin account via `POST /v1/setup`. After that, setup is
//! switched off for good.
//!
//! The token lives in the `settings` table, so every instance prints the same
//! one and any of them can accept it.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use subtle::ConstantTimeEq;

use super::{AuthError, LoggedIn, Role, password, session, users};
use crate::config::{AuthConfig, SetupConfig};

const TOKEN_KEY: &str = "setup_token";
const COMPLETED_KEY: &str = "setup_completed";

/// True until the first admin has been created.
pub async fn is_pending(db: &PgPool) -> Result<bool, sqlx::Error> {
    let done = sqlx::query_scalar!("SELECT EXISTS (SELECT 1 FROM settings WHERE key = $1)", COMPLETED_KEY)
        .fetch_one(db)
        .await?;
    Ok(!done.unwrap_or(false))
}

/// Returns the setup token while setup is pending, creating it on first call.
/// Returns `None` once setup is done. A token set in config takes precedence.
pub async fn token(db: &PgPool, cfg: &SetupConfig) -> Result<Option<String>, sqlx::Error> {
    if !is_pending(db).await? {
        return Ok(None);
    }
    if let Some(token) = &cfg.token {
        return Ok(Some(token.clone()));
    }
    let mut bytes = [0u8; 24];
    OsRng.fill_bytes(&mut bytes);
    // ON CONFLICT keeps whichever token was stored first, so instances starting
    // at the same time all end up printing the same one.
    sqlx::query!(
        "INSERT INTO settings (key, value) VALUES ($1, to_jsonb($2::text)) ON CONFLICT (key) DO NOTHING",
        TOKEN_KEY,
        URL_SAFE_NO_PAD.encode(bytes),
    )
    .execute(db)
    .await?;
    sqlx::query_scalar!(
        r#"SELECT value #>> '{}' AS "token!" FROM settings WHERE key = $1"#,
        TOKEN_KEY
    )
    .fetch_optional(db)
    .await
}

/// Trades the setup token for the first admin account and logs it in.
///
/// # Errors
/// `SetupAlreadyDone` once an admin has been created this way (or via the CLI),
/// `InvalidSetupToken` if the token doesn't match.
pub async fn complete(
    db: &PgPool,
    setup_cfg: &SetupConfig,
    auth_cfg: &AuthConfig,
    given_token: &str,
    email: &str,
    password: String,
) -> Result<LoggedIn, AuthError> {
    let email = users::normalize_email(email)?;
    password::validate(&password, auth_cfg.min_password_length)?;
    // Hash before taking the lock: it's the slow part.
    let hash = password::hash(password).await?;

    let mut tx = db.begin().await?;
    // Serialises concurrent setup attempts across all instances for this transaction.
    sqlx::query!("SELECT pg_advisory_xact_lock(hashtext('gnerkulfloot_setup'))")
        .execute(&mut *tx)
        .await?;
    let completed = sqlx::query_scalar!("SELECT EXISTS (SELECT 1 FROM settings WHERE key = $1)", COMPLETED_KEY)
        .fetch_one(&mut *tx)
        .await?
        .unwrap_or(false);
    if completed {
        return Err(AuthError::SetupAlreadyDone);
    }
    let expected = match &setup_cfg.token {
        Some(t) => Some(t.clone()),
        None => {
            sqlx::query_scalar!(
                r#"SELECT value #>> '{}' AS "token!" FROM settings WHERE key = $1"#,
                TOKEN_KEY
            )
            .fetch_optional(&mut *tx)
            .await?
        }
    };
    let Some(expected) = expected else {
        return Err(AuthError::InvalidSetupToken);
    };
    // Compare digests in constant time so response timing leaks nothing about the token.
    let matches: bool = Sha256::digest(given_token.as_bytes())
        .ct_eq(&Sha256::digest(expected.as_bytes()))
        .into();
    if !matches {
        return Err(AuthError::InvalidSetupToken);
    }

    let user = users::create(&mut tx, &email, Some(&hash), Role::Admin).await?;
    mark_complete(&mut tx).await?;
    let session = session::create(&mut tx, user.id, auth_cfg.session_ttl_hours).await?;
    tx.commit().await?;
    tracing::info!(user_id = %user.id, "first-time setup completed");
    Ok(LoggedIn { user, session })
}

/// Switches setup off and forgets the token. Also called when an admin is
/// created from the command line, so the web setup can't be used afterwards.
pub async fn mark_complete(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO settings (key, value) VALUES ($1, 'true') ON CONFLICT (key) DO NOTHING",
        COMPLETED_KEY
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!("DELETE FROM settings WHERE key = $1", TOKEN_KEY)
        .execute(&mut *conn)
        .await?;
    Ok(())
}
