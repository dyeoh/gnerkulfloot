//! User accounts and roles.

use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use time::OffsetDateTime;
use uuid::Uuid;

use super::AuthError;

/// What an account may do. Admins manage everything (including staff), staff
/// run day-to-day operations, customers can only see their own orders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Staff,
    Customer,
}

#[derive(Clone, Debug, Serialize)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub role: Role,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Trims and lowercases an email and applies a sanity check. Real validation
/// happens when we send mail to it; this only rejects obvious garbage.
pub fn normalize_email(raw: &str) -> Result<String, AuthError> {
    let email = raw.trim().to_lowercase();
    let valid = email.len() <= 254
        && !email.chars().any(char::is_whitespace)
        && matches!(email.split_once('@'), Some((local, domain)) if !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.'));
    if valid { Ok(email) } else { Err(AuthError::InvalidEmail) }
}

/// Inserts a user. `email` must already be normalized.
///
/// # Errors
/// `EmailTaken` if the email is already registered.
pub async fn create(
    conn: &mut PgConnection,
    email: &str,
    password_hash: Option<&str>,
    role: Role,
) -> Result<User, AuthError> {
    let result = sqlx::query_as!(
        User,
        r#"INSERT INTO users (id, email, password_hash, role) VALUES ($1, $2, $3, $4)
           RETURNING id, email, role AS "role: Role", created_at"#,
        Uuid::now_v7(),
        email,
        password_hash,
        role as Role,
    )
    .fetch_one(conn)
    .await;
    match result {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(AuthError::EmailTaken),
        other => Ok(other?),
    }
}

/// Lists admin and staff accounts, oldest first.
pub async fn list_staff(conn: &mut PgConnection) -> Result<Vec<User>, sqlx::Error> {
    sqlx::query_as!(
        User,
        r#"SELECT id, email, role AS "role: Role", created_at FROM users
           WHERE role IN ('admin', 'staff') ORDER BY created_at"#
    )
    .fetch_all(conn)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_rejects_garbage() {
        assert_eq!(normalize_email("  Mum@Shop.MY ").unwrap(), "mum@shop.my");
        for bad in ["", "nope", "@shop.my", "a@b", "a b@shop.my", "a@.my", "a@shop."] {
            assert!(normalize_email(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
