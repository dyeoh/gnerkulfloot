//! Public checkout: quoting a basket, placing orders, and viewing them.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth::extract::{CurrentUser, OptionalUser},
    catalog::storefront::Page,
    checkout::{
        orders::{self, ListQuery, NewOrder, OrderSummary, OrderView, Placement},
        quote::{self, Quote, QuoteRequest},
    },
    error::AppError,
    payments::{self, PaymentView},
    state::AppState,
};

const IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");
const ORDER_TOKEN: HeaderName = HeaderName::from_static("x-order-token");

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/checkout/quote", post(quote_basket))
        .route("/orders/{id}", get(get_order))
        // Not under the "orders" limit: an order page polls it while it waits.
        .route("/orders/{id}/payment/check", post(check_payment))
        .route("/me/orders", get(list_my_orders))
}

/// Placing and paying for orders; mounted behind the "orders" rate limit,
/// since both reserve something (stock, a provider checkout).
pub fn order_routes() -> Router<AppState> {
    Router::new()
        .route("/orders", post(create_order))
        .route("/orders/{id}/payment", post(start_payment))
}

async fn quote_basket(State(state): State<AppState>, Json(body): Json<QuoteRequest>) -> Result<Json<Quote>, AppError> {
    Ok(Json(quote::quote(&state.db, &state.pricing(), body).await?))
}

/// Send an `Idempotency-Key` header (e.g. a UUID made when the shopper
/// presses "Place order") so retries after a network error can't place a
/// second order. A replayed response carries `Idempotent-Replayed: true`.
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

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

fn order_token(headers: &HeaderMap, q: TokenQuery) -> Option<String> {
    headers
        .get(&ORDER_TOKEN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or(q.token)
}

/// Opens (or returns the already-open) online payment for an order. Send the
/// shopper to `checkout_url`. The order turns `paid` when the provider confirms,
/// not when the shopper comes back.
async fn start_payment(
    State(state): State<AppState>,
    OptionalUser(user): OptionalUser,
    Path(id): Path<Uuid>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<PaymentView>), AppError> {
    let token = order_token(&headers, q);
    let payment = payments::start(&state, id, token.as_deref(), user.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(payment)))
}

/// Checks the order's payment with the provider now, then returns the order.
/// Call it when the shopper comes back from the payment page, and while the
/// order page waits, so a payment is confirmed in seconds rather than when the
/// webhook or reconciliation gets to it. Access is the same as viewing the order.
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
    payments::check(&state, id).await?;
    Ok(Json(
        orders::view(&state.db, id, token.as_deref(), user.as_ref()).await?,
    ))
}

/// Guests pass the order's `access_token` as `X-Order-Token` (or `?token=`);
/// logged-in customers can view their own orders without it.
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

async fn list_my_orders(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Page<OrderSummary>>, AppError> {
    Ok(Json(orders::list(&state.db, q, Some(user.id)).await?))
}
