//! First-time setup endpoints. See `auth::setup` for how the token works.

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use utoipa_axum::{router::OpenApiRouter, routes};

use super::auth::SessionResponse;
use crate::{
    auth::setup,
    error::{AppError, Problem},
    http::docs::{BadRequest, Forbidden, RateLimited},
    state::AppState,
};

/// The status check, which frontends may poll freely.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(status))
}

/// The token exchange; mounted behind the strict "auth" rate limit.
pub fn credential_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(complete))
}

#[derive(Serialize, utoipa::ToSchema)]
struct SetupStatus {
    /// True until the first admin exists. Frontends use this to show a setup screen.
    required: bool,
}

/// Is setup pending
#[utoipa::path(
    get,
    path = "/setup",
    tag = "setup",
    responses((status = 200, description = "Whether the shop still needs its first admin", body = SetupStatus)),
)]
async fn status(State(state): State<AppState>) -> Result<Json<SetupStatus>, AppError> {
    Ok(Json(SetupStatus {
        required: setup::is_pending(&state.db).await?,
    }))
}

#[derive(Deserialize, utoipa::ToSchema)]
struct CompleteSetup {
    /// The one-time setup token printed in the server log at startup.
    token: String,
    email: String,
    #[schema(format = Password)]
    password: String,
}

/// Create the first admin
///
/// Exchanges the one-time setup token for the first admin account and logs
/// it in. Works once: after that, setup is switched off for good.

#[utoipa::path(
    post,
    path = "/setup",
    tag = "setup",
    request_body = CompleteSetup,
    responses(
        (status = 201, description = "The admin was created and logged in", body = SessionResponse),
        (status = 400, response = BadRequest),
        (status = 403, response = Forbidden),
        (status = 409, description = "Setup has already been done",
            body = Problem, content_type = "application/problem+json"),
        (status = 429, response = RateLimited),
    ),
)]
async fn complete(
    State(state): State<AppState>,
    Json(body): Json<CompleteSetup>,
) -> Result<(StatusCode, Json<SessionResponse>), AppError> {
    let logged_in = setup::complete(
        &state.db,
        &state.config.setup,
        &state.config.auth,
        &body.token,
        &body.email,
        body.password,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(logged_in.into())))
}
