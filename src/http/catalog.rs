//! Public catalog: browsing and product pages.

use axum::{
    Json,
    extract::{Path, Query, State},
};
use iso_currency::Currency;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    catalog::{
        categories::{self, Category},
        storefront::{self, ListItem, ListQuery, Page, StoreProduct},
    },
    error::AppError,
    http::docs::{BadRequest, NotFound},
    state::AppState,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_products))
        .routes(routes!(get_product))
        .routes(routes!(list_categories))
}

/// List products
///
/// Lists active products priced in the requested currency. A product with no
/// price in that currency isn't listed, since there's no automatic conversion.
#[utoipa::path(
    get,
    path = "/products",
    tag = "catalog",
    params(ListQuery),
    responses(
        (status = 200, description = "A page of products", body = Page<ListItem>),
        (status = 400, response = BadRequest),
    ),
)]
async fn list_products(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Page<ListItem>>, AppError> {
    let currency = q.currency.unwrap_or(state.config.shop.default_currency);
    Ok(Json(storefront::list(&state.db, &state.storage, currency, q).await?))
}

#[derive(Deserialize, IntoParams)]
struct CurrencyQuery {
    /// ISO 4217 code; defaults to the shop's currency.
    #[param(value_type = Option<String>, example = "MYR")]
    currency: Option<Currency>,
}

/// Get a product
///
/// A product page, with every variant priced in the requested currency. An
/// inactive product, or one with no price in that currency, is not found.

#[utoipa::path(
    get,
    path = "/products/{slug}",
    tag = "catalog",
    params(("slug" = String, Path, description = "The product's slug"), CurrencyQuery),
    responses(
        (status = 200, description = "The product", body = StoreProduct),
        (status = 404, response = NotFound),
    ),
)]
async fn get_product(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(q): Query<CurrencyQuery>,
) -> Result<Json<StoreProduct>, AppError> {
    let currency = q.currency.unwrap_or(state.config.shop.default_currency);
    Ok(Json(storefront::get(&state.db, &state.storage, &slug, currency).await?))
}

/// List categories
///
/// Every category, ordered for display. Build the tree from `parent_id`.
#[utoipa::path(
    get,
    path = "/categories",
    tag = "catalog",
    responses((status = 200, description = "All categories", body = Vec<Category>)),
)]
async fn list_categories(State(state): State<AppState>) -> Result<Json<Vec<Category>>, AppError> {
    Ok(Json(categories::list(&state.db).await?))
}
