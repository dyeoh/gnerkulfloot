//! Public checkout: quoting a basket, placing orders, and viewing them.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    auth::extract::{CurrentUser, OptionalUser},
    catalog::storefront::Page,
    checkout::{
        orders::{self, ListQuery, NewOrder, OrderSummary, OrderView, PlacedOrder, Placement},
        quote::{self, Quote, QuoteRequest},
    },
    error::{AppError, Problem},
    http::docs::{BadRequest, NotFound, RateLimited, Unauthorized},
    payments::{self, PaymentView},
    state::AppState,
};

const IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");
const ORDER_TOKEN: HeaderName = HeaderName::from_static("x-order-token");

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(quote_basket))
        .routes(routes!(get_order))
        // Not under the "orders" limit: an order page polls it while it waits.
        .routes(routes!(check_payment))
        .routes(routes!(list_my_orders))
}

/// Placing and paying for orders; mounted behind the "orders" rate limit,
/// since both reserve something (stock, a provider checkout).
pub fn order_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_order))
        .routes(routes!(start_payment))
}

/// Quote a basket
///
/// Prices a basket for a destination: lines, shipping options, tax and total.
/// Nothing is reserved; placing the order re-runs the quote. Send ids and
/// quantities only; prices always come from the server.
#[utoipa::path(
    post,
    path = "/checkout/quote",
    tag = "checkout",
    request_body = QuoteRequest,
    responses(
        (status = 200, description = "The priced basket", body = Quote),
        (status = 400, response = BadRequest),
        (status = 409, description = "`unavailable`: a SKU isn't for sale (`sku_id` says which)",
            body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "`unknown_shipping_option`: the chosen option doesn't ship there",
            body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn quote_basket(State(state): State<AppState>, Json(body): Json<QuoteRequest>) -> Result<Json<Quote>, AppError> {
    Ok(Json(quote::quote(&state.db, &state.pricing(), body).await?))
}

/// Place an order
///
/// Re-quotes the basket and holds its stock until the order is paid or
/// `expires_at` passes. Guests keep the returned `access_token` to view the
/// order later; it is only shown here. Logged-in customers' orders are linked
/// to their account.
///
/// Send an `Idempotency-Key` header (e.g. a UUID made when the shopper
/// presses "Place order") so retries after a network error can't place a
/// second order. A replayed response carries `Idempotent-Replayed: true`.
#[utoipa::path(
    post,
    path = "/orders",
    tag = "checkout",
    params(("Idempotency-Key" = Option<String>, Header,
        description = "Unique per order attempt; a repeat replays the original response")),
    request_body = NewOrder,
    security((), ("bearer" = [])),
    responses(
        (status = 201, description = "The order was placed (or replayed)", body = PlacedOrder,
            headers(("Idempotent-Replayed" = String, description = "`true` when this is a replay"))),
        (status = 400, response = BadRequest),
        (status = 409, description = "`out_of_stock` (with `sku_id` and `available`) or `unavailable` (with `sku_id`)",
            body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "`shipping_unavailable`, `unknown_shipping_option`, or \
            `idempotency_key_reused` (the key was used for a different order)",
            body = Problem, content_type = "application/problem+json"),
        (status = 429, response = RateLimited),
    ),
)]
async fn create_order(
    State(state): State<AppState>,
    OptionalUser(user): OptionalUser,
    headers: HeaderMap,
    Json(body): Json<NewOrder>,
) -> Result<Response, AppError> {
    let key = headers
        .get(&IDEMPOTENCY_KEY)
        .map(|v| v.to_str())
        .transpose()
        .map_err(|_| AppError::BadRequest("Idempotency-Key must be ASCII".into()))?;
    let placement = orders::place(&state.db, &state.pricing(), body, user.map(|u| u.id), key).await?;
    Ok(match placement {
        Placement::Created(body) => (StatusCode::CREATED, Json(body)).into_response(),
        Placement::Replayed(body) => (
            StatusCode::CREATED,
            [(
                HeaderName::from_static("idempotent-replayed"),
                HeaderValue::from_static("true"),
            )],
            Json(body),
        )
            .into_response(),
    })
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct TokenQuery {
    /// The order's `access_token`, for links where a header can't be set.
    /// Prefer the `X-Order-Token` header.
    token: Option<String>,
}

fn order_token(headers: &HeaderMap, q: TokenQuery) -> Option<String> {
    headers
        .get(&ORDER_TOKEN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or(q.token)
}

/// Start paying for an order
///
/// Opens (or returns the already-open) online payment for an order. Send the
/// shopper to `checkout_url`. The order turns `paid` when the provider confirms,
/// not when the shopper comes back.
#[utoipa::path(
    post,
    path = "/orders/{id}/payment",
    tag = "orders",
    params(("id" = Uuid, Path, description = "Order id"), TokenQuery),
    security(("order_token" = []), ("bearer" = [])),
    responses(
        (status = 201, description = "The open payment", body = PaymentView),
        (status = 404, response = NotFound),
        (status = 409, description = "`order_not_payable`: already paid, cancelled or expired",
            body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "`payments_disabled`: no payment adapter is configured",
            body = Problem, content_type = "application/problem+json"),
        (status = 429, response = RateLimited),
        (status = 502, description = "`payment_provider_error`: the provider couldn't be reached; retry",
            body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn start_payment(
    State(state): State<AppState>,
    OptionalUser(user): OptionalUser,
    Path(id): Path<Uuid>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<PaymentView>), AppError> {
    let token = order_token(&headers, q);
    let payment = payments::start(
        &state.db,
        state.payments.as_deref(),
        id,
        token.as_deref(),
        user.as_ref(),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(payment)))
}

/// Check an order's payment
///
/// Checks the order's payment with the provider now, then returns the order.
/// Call it when the shopper comes back from the payment page, and while the
/// order page waits, so a payment is confirmed in seconds rather than when the
/// webhook or reconciliation gets to it. Access is the same as viewing the order.
#[utoipa::path(
    post,
    path = "/orders/{id}/payment/check",
    tag = "orders",
    params(("id" = Uuid, Path, description = "Order id"), TokenQuery),
    security(("order_token" = []), ("bearer" = [])),
    responses(
        (status = 200, description = "The order, after checking", body = OrderView),
        (status = 404, response = NotFound),
    ),
)]
async fn check_payment(
    State(state): State<AppState>,
    OptionalUser(user): OptionalUser,
    Path(id): Path<Uuid>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Result<Json<OrderView>, AppError> {
    let token = order_token(&headers, q);
    // Viewing first: only someone who may see the order can make us call the provider.
    orders::view(&state.db, id, token.as_deref(), user.as_ref()).await?;
    payments::check(&state.db, state.payments.as_deref(), id).await?;
    Ok(Json(
        orders::view(&state.db, id, token.as_deref(), user.as_ref()).await?,
    ))
}

/// Get an order
///
/// Guests pass the order's `access_token` as `X-Order-Token` (or `?token=`);
/// logged-in customers can view their own orders without it.
#[utoipa::path(
    get,
    path = "/orders/{id}",
    tag = "orders",
    params(("id" = Uuid, Path, description = "Order id"), TokenQuery),
    security(("order_token" = []), ("bearer" = [])),
    responses(
        (status = 200, description = "The order", body = OrderView),
        (status = 404, response = NotFound),
    ),
)]
async fn get_order(
    State(state): State<AppState>,
    OptionalUser(user): OptionalUser,
    Path(id): Path<Uuid>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Result<Json<OrderView>, AppError> {
    let token = order_token(&headers, q);
    Ok(Json(
        orders::view(&state.db, id, token.as_deref(), user.as_ref()).await?,
    ))
}

/// List my orders
///
/// The logged-in customer's orders, newest first.
#[utoipa::path(
    get,
    path = "/me/orders",
    tag = "orders",
    params(ListQuery),
    security(("bearer" = [])),
    responses(
        (status = 200, description = "A page of orders", body = Page<OrderSummary>),
        (status = 401, response = Unauthorized),
    ),
)]
async fn list_my_orders(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Page<OrderSummary>>, AppError> {
    Ok(Json(orders::list(&state.db, q, Some(user.id)).await?))
}
