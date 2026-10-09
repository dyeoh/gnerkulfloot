//! First-time setup endpoints. See `auth::setup` for how the token works.

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use utoipa_axum::router::OpenApiRouter;

use super::auth::SessionResponse;
use crate::{auth::setup, error::AppError, state::AppState};

/// The status check, which frontends may poll freely.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().route("/setup", get(status))
}

/// The token exchange; mounted behind the strict "auth" rate limit.
pub fn credential_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().route("/setup", post(complete))
}

#[derive(Serialize)]
struct SetupStatus {
    /// True until the first admin exists. Frontends use this to show a setup screen.
    required: bool,
}

async fn status(State(state): State<AppState>) -> Result<Json<SetupStatus>, AppError> {
    Ok(Json(SetupStatus {
        required: setup::is_pending(&state.db).await?,
    }))
}

#[derive(Deserialize)]
struct CompleteSetup {
    token: String,
    email: String,
    password: String,
}

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
