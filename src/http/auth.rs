//! Customer registration, login and logout. Staff and admins log in here too.

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    auth::{
        self, LoggedIn, User,
        extract::{BearerToken, CurrentUser},
        session::{self, NewSession},
    },
    error::{AppError, Problem},
    http::docs::{BadRequest, RateLimited, Unauthorized},
    state::AppState,
};

/// Routes that take passwords; mounted behind the strict "auth" rate limit.
pub fn credential_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(register)).routes(routes!(login))
}

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(logout)).routes(routes!(me))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct Credentials {
    #[schema(example = "shopper@example.com")]
    pub email: String,
    #[schema(format = Password)]
    pub password: String,
}

/// Returned by every endpoint that logs someone in.
#[derive(Serialize, utoipa::ToSchema)]
pub struct SessionResponse {
    pub user: User,
    #[serde(flatten)]
    pub session: NewSession,
}

impl From<LoggedIn> for SessionResponse {
    fn from(l: LoggedIn) -> Self {
        Self {
            user: l.user,
            session: l.session,
        }
    }
}

/// Register a customer account
///
/// Creates a customer account and logs it in. Optional: guests can buy
/// without one.
#[utoipa::path(
    post,
    path = "/auth/register",
    tag = "auth",
    request_body = Credentials,
    responses(
        (status = 201, description = "Registered and logged in", body = SessionResponse),
        (status = 400, response = BadRequest),
        (status = 409, description = "That email already has an account",
            body = Problem, content_type = "application/problem+json"),
        (status = 429, response = RateLimited),
    ),
)]
async fn register(
    State(state): State<AppState>,
    Json(body): Json<Credentials>,
) -> Result<(StatusCode, Json<SessionResponse>), AppError> {
    let logged_in = auth::register(&state.db, &state.config.auth, &body.email, body.password).await?;
    Ok((StatusCode::CREATED, Json(logged_in.into())))
}

/// Log in
///
/// For customers, staff and admins alike. Send the returned `token` on later
/// requests as `Authorization: Bearer <token>`. Several wrong passwords in a
/// row lock the account for a while, whatever IP they come from; a locked
/// account answers 429 with `Retry-After`.
#[utoipa::path(
    post,
    path = "/auth/login",
    tag = "auth",
    request_body = Credentials,
    responses(
        (status = 200, description = "Logged in", body = SessionResponse),
        (status = 401, response = Unauthorized),
        (status = 429, response = RateLimited),
    ),
)]
async fn login(
    State(state): State<AppState>,
    Json(body): Json<Credentials>,
) -> Result<Json<SessionResponse>, AppError> {
    let logged_in = auth::login(&state.db, &state.config.auth, &body.email, body.password).await?;
    Ok(Json(logged_in.into()))
}

/// Log out
///
/// Ends the session whose token is sent.
#[utoipa::path(
    post,
    path = "/auth/logout",
    tag = "auth",
    security(("bearer" = [])),
    responses(
        (status = 204, description = "Logged out"),
        (status = 401, response = Unauthorized),
    ),
)]
async fn logout(State(state): State<AppState>, BearerToken(token): BearerToken) -> Result<StatusCode, AppError> {
    session::revoke(&state.db, &token).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Who am I
#[utoipa::path(
    get,
    path = "/auth/me",
    tag = "auth",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The logged-in user", body = User),
        (status = 401, response = Unauthorized),
    ),
)]
async fn me(CurrentUser(user): CurrentUser) -> Json<User> {
    Json(user)
}
