//! Password hashing with argon2id.
//!
//! Hashing is deliberately slow (that's what makes stolen hashes expensive to
//! crack), so it runs on the blocking thread pool instead of stalling the async
//! workers that serve other requests.

use std::sync::LazyLock;

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};

use super::AuthError;

/// Upper bound on password length, so nobody can make us hash megabytes.
const MAX_PASSWORD_BYTES: usize = 1024;

/// A real hash of a random password, checked against when the email doesn't
/// exist so that "unknown user" takes as long as "wrong password".
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(salt.as_str().as_bytes(), &salt)
        .expect("hashing a fixed-size input cannot fail")
        .to_string()
});

/// Rejects passwords that are too short or absurdly long. Length is the only
/// rule: composition rules ("one symbol…") don't make passwords stronger.
pub fn validate(password: &str, min_chars: usize) -> Result<(), AuthError> {
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(AuthError::PasswordTooLong);
    }
    if password.chars().count() < min_chars {
        return Err(AuthError::WeakPassword(min_chars));
    }
    Ok(())
}

/// Hashes a password into a self-describing PHC string (algorithm, params, salt, hash).
pub async fn hash(password: String) -> Result<String, AuthError> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| AuthError::Hashing(e.to_string()))
    })
    .await
    .map_err(|e| AuthError::Hashing(e.to_string()))?
}

/// Checks a password against a stored hash. With no stored hash (unknown user,
/// or an OIDC-only account) it still does the full amount of work, then fails.
pub async fn verify(password: String, stored: Option<String>) -> bool {
    tokio::task::spawn_blocking(move || {
        let has_hash = stored.is_some();
        let stored = stored.unwrap_or_else(|| DUMMY_HASH.clone());
        let matches = PasswordHash::new(&stored)
            .map(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
            .unwrap_or(false);
        has_hash && matches
    })
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_round_trips_and_rejects_wrong_password() {
        let h = hash("correct horse battery".into()).await.unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify("correct horse battery".into(), Some(h.clone())).await);
        assert!(!verify("wrong horse battery".into(), Some(h)).await);
    }

    #[tokio::test]
    async fn missing_hash_never_verifies() {
        assert!(!verify("anything".into(), None).await);
    }

    #[test]
    fn length_rules() {
        assert!(validate("short", 10).is_err());
        assert!(validate("long enough pw", 10).is_ok());
        assert!(validate(&"x".repeat(2000), 10).is_err());
    }
}
