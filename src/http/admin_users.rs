//! Admin management of staff and admin accounts.

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::Deserialize;

use crate::{
    app::AppState,
    auth::{Role, User, extract::AdminUser, password, users},
    error::AppError,
};

pub fn routes() -> Router<AppState> {
    Router::new().route("/admin/users", get(list_users).post(create_user))
}

async fn list_users(State(state): State<AppState>, _admin: AdminUser) -> Result<Json<Vec<User>>, AppError> {
    let mut conn = state.db.acquire().await?;
    Ok(Json(users::list_staff(&mut conn).await?))
}

#[derive(Deserialize)]
struct NewStaff {
    email: String,
    password: String,
    role: Role,
}

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
