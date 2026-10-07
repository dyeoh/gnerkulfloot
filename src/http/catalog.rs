//! Public catalog: browsing and product pages.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use iso_currency::Currency;
use serde::Deserialize;

use crate::{
    app::AppState,
    catalog::{
        categories::{self, Category},
        storefront::{self, ListItem, ListQuery, Page, StoreProduct},
    },
    error::AppError,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/products", get(list_products))
        .route("/products/{slug}", get(get_product))
        .route("/categories", get(list_categories))
}

async fn list_products(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Page<ListItem>>, AppError> {
    let currency = q.currency.unwrap_or(state.config.shop.default_currency);
    Ok(Json(storefront::list(&state.db, &state.storage, currency, q).await?))
}

#[derive(Deserialize)]
struct CurrencyQuery {
    currency: Option<Currency>,
}

async fn get_product(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(q): Query<CurrencyQuery>,
) -> Result<Json<StoreProduct>, AppError> {
    let currency = q.currency.unwrap_or(state.config.shop.default_currency);
    Ok(Json(storefront::get(&state.db, &state.storage, &slug, currency).await?))
}

async fn list_categories(State(state): State<AppState>) -> Result<Json<Vec<Category>>, AppError> {
    Ok(Json(categories::list(&state.db).await?))
}
