//! Accounts, passwords, sessions and first-time setup.
//!
//! Login gives back an opaque session token that clients send as
//! `Authorization: Bearer <token>`. Handlers ask for a logged-in user by taking
//! one of the extractors in [`extract`] as an argument.

pub mod extract;
pub mod password;
pub mod session;
pub mod setup;
pub mod users;

use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

pub use users::{Role, User};

use crate::{config::AuthConfig, error::AppError};

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("that doesn't look like a valid email address")]
    InvalidEmail,
    #[error("password must be at least {0} characters")]
    WeakPassword(usize),
    #[error("password is too long")]
    PasswordTooLong,
    #[error("an account with that email already exists")]
    EmailTaken,
    /// Deliberately vague: callers can't tell "no such user" from "wrong password".
    #[error("wrong email or password")]
    InvalidCredentials,
    #[error("too many failed logins, try again later")]
    Locked { retry_after_secs: u64 },
    #[error("invalid setup token")]
    InvalidSetupToken,
    #[error("setup has already been completed")]
    SetupAlreadyDone,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("password hashing failed: {0}")]
    Hashing(String),
}

impl From<AuthError> for AppError {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::InvalidEmail | AuthError::WeakPassword(_) | AuthError::PasswordTooLong => {
                AppError::BadRequest(err.to_string())
            }
            AuthError::EmailTaken | AuthError::SetupAlreadyDone => AppError::Conflict(err.to_string()),
            AuthError::InvalidCredentials => AppError::Unauthorized,
            AuthError::Locked { retry_after_secs } => AppError::RateLimited { retry_after_secs },
            AuthError::InvalidSetupToken => AppError::Forbidden,
            AuthError::Database(e) => e.into(),
            AuthError::Hashing(msg) => AppError::Internal(anyhow::anyhow!(msg)),
        }
    }
}

/// A user together with the session token they just received.
pub struct LoggedIn {
    pub user: User,
    pub session: session::NewSession,
}

/// Creates a customer account and logs it in.
pub async fn register(db: &PgPool, cfg: &AuthConfig, email: &str, password: String) -> Result<LoggedIn, AuthError> {
    let email = users::normalize_email(email)?;
    password::validate(&password, cfg.min_password_length)?;
    let hash = password::hash(password).await?;
    let mut tx = db.begin().await?;
    let user = users::create(&mut tx, &email, Some(&hash), Role::Customer).await?;
    let session = session::create(&mut tx, user.id, cfg.session_ttl_hours).await?;
    tx.commit().await?;
    Ok(LoggedIn { user, session })
}

/// Checks a password and opens a session.
///
/// Repeated failures lock the account for `lockout_minutes`, which stops
/// password guessing from many IPs that the per-IP rate limit alone can't catch.
///
/// # Errors
/// `InvalidCredentials` for an unknown email or wrong password (indistinguishable
/// on purpose), `Locked` while the account is locked.
pub async fn login(db: &PgPool, cfg: &AuthConfig, email: &str, password: String) -> Result<LoggedIn, AuthError> {
    let email = users::normalize_email(email).map_err(|_| AuthError::InvalidCredentials)?;
    let row = sqlx::query!(
        r#"SELECT id, email, password_hash, role AS "role: Role", created_at, locked_until
           FROM users WHERE email = $1"#,
        email
    )
    .fetch_optional(db)
    .await?;

    if let Some(until) = row.as_ref().and_then(|r| r.locked_until) {
        let remaining = until - OffsetDateTime::now_utc();
        if remaining > Duration::ZERO {
            return Err(AuthError::Locked {
                retry_after_secs: remaining.whole_seconds().max(1) as u64,
            });
        }
    }

    // Always runs a full hash check, even for unknown emails, so response time
    // doesn't reveal which emails have accounts.
    let stored_hash = row.as_ref().and_then(|r| r.password_hash.clone());
    let ok = password::verify(password, stored_hash).await;
    let Some(row) = row else {
        return Err(AuthError::InvalidCredentials);
    };

    if !ok {
        // One atomic statement, so concurrent guesses can't slip past the counter.
        sqlx::query!(
            r#"UPDATE users SET
                 failed_logins = CASE WHEN failed_logins + 1 >= $2 THEN 0 ELSE failed_logins + 1 END,
                 locked_until  = CASE WHEN failed_logins + 1 >= $2
                                      THEN now() + make_interval(mins => $3) ELSE locked_until END
               WHERE id = $1"#,
            row.id,
            cfg.max_failed_logins,
            cfg.lockout_minutes as i32,
        )
        .execute(db)
        .await?;
        return Err(AuthError::InvalidCredentials);
    }

    let mut tx = db.begin().await?;
    sqlx::query!(
        "UPDATE users SET failed_logins = 0, locked_until = NULL WHERE id = $1",
        row.id
    )
    .execute(&mut *tx)
    .await?;
    let session = session::create(&mut tx, row.id, cfg.session_ttl_hours).await?;
    tx.commit().await?;

    let user = User {
        id: row.id,
        email: row.email,
        role: row.role,
        created_at: row.created_at,
    };
    Ok(LoggedIn { user, session })
}
