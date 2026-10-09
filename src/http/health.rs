//! Health probes for load balancers and orchestrators.
//! `/healthz` answers "is the process alive"; `/readyz` answers "can it serve
//! traffic", which needs the database. Point load-balancer health checks at `/readyz`.

use axum::{extract::State, http::StatusCode};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::state::AppState;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(healthz)).routes(routes!(readyz))
}

/// Is the process alive
#[utoipa::path(
    get,
    path = "/healthz",
    tag = "health",
    responses((status = 200, description = "Alive", body = String, content_type = "text/plain", example = "ok")),
)]
async fn healthz() -> &'static str {
    "ok"
}

/// Can it serve traffic
///
/// Point load-balancer health checks here: it fails while the database is unreachable.
#[utoipa::path(
    get,
    path = "/readyz",
    tag = "health",
    responses(
        (status = 200, description = "Ready", body = String, content_type = "text/plain", example = "ready"),
        (status = 503, description = "The database is unreachable", body = String, content_type = "text/plain"),
    ),
)]
async fn readyz(State(state): State<AppState>) -> (StatusCode, &'static str) {
    match sqlx::query("SELECT 1").execute(&state.db).await {
        Ok(_) => (StatusCode::OK, "ready"),
        Err(err) => {
            tracing::warn!(error = %err, "readiness check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
        }
    }
}
