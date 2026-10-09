//! Customer registration, login and logout. Staff and admins log in here too.

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use utoipa_axum::router::OpenApiRouter;

use crate::{
    auth::{
        self, LoggedIn, User,
        extract::{BearerToken, CurrentUser},
        session::{self, NewSession},
    },
    error::AppError,
    state::AppState,
};

/// Routes that take passwords; mounted behind the strict "auth" rate limit.
pub fn credential_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
}

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
}

#[derive(Deserialize)]
pub struct Credentials {
    pub email: String,
    pub password: String,
}

/// Returned by every endpoint that logs someone in.
#[derive(Serialize)]
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

async fn register(
    State(state): State<AppState>,
    Json(body): Json<Credentials>,
) -> Result<(StatusCode, Json<SessionResponse>), AppError> {
    let logged_in = auth::register(&state.db, &state.config.auth, &body.email, body.password).await?;
    Ok((StatusCode::CREATED, Json(logged_in.into())))
}

async fn login(
    State(state): State<AppState>,
    Json(body): Json<Credentials>,
) -> Result<Json<SessionResponse>, AppError> {
    let logged_in = auth::login(&state.db, &state.config.auth, &body.email, body.password).await?;
    Ok(Json(logged_in.into()))
}

async fn logout(State(state): State<AppState>, BearerToken(token): BearerToken) -> Result<StatusCode, AppError> {
    session::revoke(&state.db, &token).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn me(CurrentUser(user): CurrentUser) -> Json<User> {
    Json(user)
}
