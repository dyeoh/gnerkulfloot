//! Webhooks from payment providers. Outside rate limiting on purpose: a
//! throttled confirmation would leave a paid order looking unpaid.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{error::AppError, payments, state::AppState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(receive_webhook))
}

/// Payment provider webhook
///
/// For the payment provider, not storefronts. The body is the provider's own
/// signed payload; it's only taken as a hint, and the payment's real state is
/// fetched from the provider's API before anything changes.
///
/// Answers 200 once the webhook is verified and applied, 401 for a bad
/// signature, and 5xx if we couldn't process it, so the provider retries.
#[utoipa::path(
    post,
    path = "/payments/{adapter}/webhook",
    tag = "payments",
    params(("adapter" = String, Path, description = "Payment adapter id, e.g. `hitpay`")),
    request_body(content = Object, description = "The provider's signed payload, as sent",
        content_type = "application/json"),
    responses(
        (status = 200, description = "Verified and applied"),
        (status = 401, description = "Bad signature"),
        (status = 404, description = "No such adapter is configured"),
        (status = 500, description = "Couldn't process it; the provider should retry"),
    ),
)]
async fn receive_webhook(
    State(state): State<AppState>,
    Path(adapter): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    payments::handle_webhook(&state.db, state.payments.as_deref(), &adapter, &headers, &body).await?;
    Ok(StatusCode::OK)
}
