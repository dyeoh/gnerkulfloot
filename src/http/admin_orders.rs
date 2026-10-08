//! Order management for staff.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::extract::StaffUser,
    catalog::storefront::Page,
    checkout::orders::{self, ListQuery, OrderSummary, OrderView},
    error::AppError,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/orders", get(list_orders))
        .route("/admin/orders/{id}", get(get_order))
        .route("/admin/orders/{id}/cancel", post(cancel_order))
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
) -> Result<Json<OrderView>, AppError> {
    Ok(Json(orders::load(&mut *state.db.acquire().await?, id).await?))
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
