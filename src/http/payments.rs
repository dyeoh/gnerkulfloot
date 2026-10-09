//! Webhooks from payment providers. Outside rate limiting on purpose: a
//! throttled confirmation would leave a paid order looking unpaid.

use axum::{
    Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};

use crate::{error::AppError, payments, state::AppState};

pub fn routes() -> Router<AppState> {
    Router::new().route("/payments/{adapter}/webhook", post(receive_webhook))
}

/// Answers 200 once the webhook is verified and applied, 401 for a bad
/// signature, and 5xx if we couldn't process it, so the provider retries.
async fn receive_webhook(
    State(state): State<AppState>,
    Path(adapter): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    payments::handle_webhook(&state, &adapter, &headers, &body).await?;
    Ok(StatusCode::OK)
}
