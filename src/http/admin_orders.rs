//! Order management for staff.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::extract::StaffUser,
    catalog::storefront::Page,
    checkout::orders::{self, ListQuery, OrderSummary, OrderView},
    error::AppError,
    payments::{self, PaymentView},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/orders", get(list_orders))
        .route("/admin/orders/{id}", get(get_order))
        .route("/admin/orders/{id}/cancel", post(cancel_order))
        .route("/admin/orders/{id}/mark-paid", post(mark_order_paid))
}

/// An order as staff see it: with its payments and any reason it needs attention.
#[derive(Serialize)]
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

async fn list_orders(
    State(state): State<AppState>,
    _: StaffUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Page<OrderSummary>>, AppError> {
    Ok(Json(orders::list(&state.db, q, None).await?))
}

async fn get_order(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminOrder>, AppError> {
    Ok(Json(admin_order(&state, id).await?))
}

/// Cancels an order that hasn't been paid and puts its stock back.
async fn cancel_order(
    State(state): State<AppState>,
    StaffUser(user): StaffUser,
    Path(id): Path<Uuid>,
) -> Result<Json<OrderView>, AppError> {
    let order = orders::cancel(&state.db, id).await?;
    tracing::info!(order_id = %id, by = %user.id, "order cancelled");
    Ok(Json(order))
}

/// For payments taken outside the shop (cash, bank transfer). Online payments
/// are confirmed by the provider and never need this.
async fn mark_order_paid(
    State(state): State<AppState>,
    StaffUser(user): StaffUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminOrder>, AppError> {
    payments::mark_paid_manually(&state.db, id).await?;
    tracing::info!(order_id = %id, by = %user.id, "order marked paid by staff");
    Ok(Json(admin_order(&state, id).await?))
}
