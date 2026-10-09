//! Axum extractors for authentication. Put one in a handler's arguments and
//! the handler only runs for callers who qualify:
//!
//! - [`CurrentUser`]: any logged-in account (401 otherwise)
//! - [`StaffUser`]: admin or staff (403 for customers)
//! - [`AdminUser`]: admin only
//! - [`OptionalUser`]: the user if a token is sent (guests allowed), 401 for a bad token
//! - [`BearerToken`]: the raw token, for logout

use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts},
};

use super::{Role, User, session};
use crate::{error::AppError, state::AppState};

pub struct BearerToken(pub String);

impl<S: Send + Sync> FromRequestParts<S> for BearerToken {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, AppError> {
        parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|t| BearerToken(t.trim().to_owned()))
            .ok_or(AppError::Unauthorized)
    }
}

pub struct CurrentUser(pub User);

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let BearerToken(token) = BearerToken::from_request_parts(parts, state).await?;
        session::lookup(&state.db, &token)
            .await?
            .map(CurrentUser)
            .ok_or(AppError::Unauthorized)
    }
}

/// For endpoints guests can use too, like checkout. No `Authorization`
/// header means a guest; a header with a bad or expired token is still a 401,
/// so a client never silently loses its login.
pub struct OptionalUser(pub Option<User>);

impl FromRequestParts<AppState> for OptionalUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        if !parts.headers.contains_key(header::AUTHORIZATION) {
            return Ok(OptionalUser(None));
        }
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        Ok(OptionalUser(Some(user)))
    }
}

pub struct StaffUser(pub User);

impl FromRequestParts<AppState> for StaffUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        match user.role {
            Role::Admin | Role::Staff => Ok(StaffUser(user)),
            Role::Customer => Err(AppError::Forbidden),
        }
    }
}

pub struct AdminUser(pub User);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        match user.role {
            Role::Admin => Ok(AdminUser(user)),
            _ => Err(AppError::Forbidden),
        }
    }
}
