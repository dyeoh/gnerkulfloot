//! Order management for staff.

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::Serialize;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    auth::extract::StaffUser,
    catalog::storefront::Page,
    checkout::orders::{self, ListQuery, OrderSummary, OrderView},
    error::{AppError, Problem},
    http::docs::{Forbidden, NotFound, Unauthorized},
    payments::{self, PaymentView},
    state::AppState,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_orders))
        .routes(routes!(get_order))
        .routes(routes!(cancel_order))
        .routes(routes!(mark_order_paid))
}

/// An order as staff see it: with its payments and any reason it needs attention.
#[derive(Serialize, utoipa::ToSchema)]
struct AdminOrder {
    #[serde(flatten)]
    order: OrderView,
    /// Set when the order needs a person, e.g. paid after its stock sold out.
    review_reason: Option<String>,
    payments: Vec<PaymentView>,
}

async fn admin_order(state: &AppState, id: Uuid) -> Result<AdminOrder, AppError> {
    Ok(AdminOrder {
        order: orders::load(&mut *state.db.acquire().await?, id).await?,
        review_reason: orders::review_reason(&state.db, id).await?,
        payments: payments::for_order(&state.db, id).await?,
    })
}

/// List orders
///
/// All orders, newest first. Filter with `needs_review=true` for the ones a
/// person has to resolve.
#[utoipa::path(
    get,
    path = "/admin/orders",
    tag = "admin-orders",
    params(ListQuery),
    security(("bearer" = [])),
    responses(
        (status = 200, description = "A page of orders", body = Page<OrderSummary>),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
    ),
)]
async fn list_orders(
    State(state): State<AppState>,
    _: StaffUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Page<OrderSummary>>, AppError> {
    Ok(Json(orders::list(&state.db, q, None).await?))
}

/// Get an order
///
/// The order with its payments and, if it needs attention, why.
#[utoipa::path(
    get,
    path = "/admin/orders/{id}",
    tag = "admin-orders",
    params(("id" = Uuid, Path, description = "Order id")),
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The order", body = AdminOrder),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn get_order(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminOrder>, AppError> {
    Ok(Json(admin_order(&state, id).await?))
}

/// Cancel an order
///
/// Cancels an order that hasn't been paid and puts its stock back.
#[utoipa::path(
    post,
    path = "/admin/orders/{id}/cancel",
    tag = "admin-orders",
    params(("id" = Uuid, Path, description = "Order id")),
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The cancelled order", body = OrderView),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
        (status = 409, description = "Only orders awaiting payment can be cancelled",
            body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn cancel_order(
    State(state): State<AppState>,
    StaffUser(user): StaffUser,
    Path(id): Path<Uuid>,
) -> Result<Json<OrderView>, AppError> {
    let order = orders::cancel(&state.db, id).await?;
    tracing::info!(order_id = %id, by = %user.id, "order cancelled");
    Ok(Json(order))
}

/// Mark an order paid
///
/// For payments taken outside the shop (cash, bank transfer). Online payments
/// are confirmed by the provider and never need this. An expired or cancelled
/// order tries to hold its stock again; if that stock has sold out, the order
/// keeps its status and gets a `review_reason` (refund or restock) instead.
#[utoipa::path(
    post,
    path = "/admin/orders/{id}/mark-paid",
    tag = "admin-orders",
    params(("id" = Uuid, Path, description = "Order id")),
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The order: check `status` and `review_reason`", body = AdminOrder),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
        (status = 409, description = "`order_not_payable`: it's already paid",
            body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn mark_order_paid(
    State(state): State<AppState>,
    StaffUser(user): StaffUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminOrder>, AppError> {
    payments::mark_paid_manually(&state.db, id).await?;
    tracing::info!(order_id = %id, by = %user.id, "order marked paid by staff");
    Ok(Json(admin_order(&state, id).await?))
}
