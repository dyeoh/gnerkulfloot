//! Admin management of staff and admin accounts.

use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::{Role, User, extract::AdminUser, password, users},
    error::{AppError, Problem},
    http::docs::{BadRequest, Forbidden, Unauthorized},
    state::AppState,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list_users, create_user))
}

/// List staff accounts
///
/// Every admin and staff account. Customers aren't listed.
#[utoipa::path(
    get,
    path = "/admin/users",
    tag = "admin-users",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Admin and staff accounts", body = Vec<User>),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
    ),
)]
async fn list_users(State(state): State<AppState>, _admin: AdminUser) -> Result<Json<Vec<User>>, AppError> {
    let mut conn = state.db.acquire().await?;
    Ok(Json(users::list_staff(&mut conn).await?))
}

#[derive(Deserialize, utoipa::ToSchema)]
struct NewStaff {
    email: String,
    #[schema(format = Password)]
    password: String,
    /// `admin` or `staff`. Customers register themselves.
    role: Role,
}

/// Create a staff account
///
/// Creates an admin or staff account. Admins manage everything, including
/// accounts; staff handle day-to-day work but can't manage accounts.

#[utoipa::path(
    post,
    path = "/admin/users",
    tag = "admin-users",
    request_body = NewStaff,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The new account", body = User),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 409, description = "That email already has an account",
            body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn create_user(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Json(body): Json<NewStaff>,
) -> Result<(StatusCode, Json<User>), AppError> {
    if body.role == Role::Customer {
        return Err(AppError::BadRequest(
            "customers register themselves; use role admin or staff".into(),
        ));
    }
    let email = users::normalize_email(&body.email)?;
    password::validate(&body.password, state.config.auth.min_password_length)?;
    let hash = password::hash(body.password).await?;
    let mut conn = state.db.acquire().await?;
    let user = users::create(&mut conn, &email, Some(&hash), body.role).await?;
    tracing::info!(created = %user.id, by = %admin.id, role = ?user.role, "staff account created");
    Ok((StatusCode::CREATED, Json(user)))
}
