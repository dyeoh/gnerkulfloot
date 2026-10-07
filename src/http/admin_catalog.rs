//! Catalog management for staff: products, SKUs, stock, categories and images.

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::extract::StaffUser,
    catalog::{
        categories::{self, Category, CategoryPatch, NewCategory},
        images::{self, ImageView},
        products::{self, AdminListItem, AdminListQuery, NewProduct, ProductDetail, ProductPatch},
        skus::{self, NewSku, Sku, SkuPatch},
        storefront::Page,
    },
    error::AppError,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/products", get(list_products).post(create_product))
        .route("/admin/products/{id}", get(get_product).patch(update_product))
        .route("/admin/products/{id}/skus", post(create_sku))
        .route("/admin/products/{id}/images", post(upload_image))
        .route("/admin/skus/{id}", get(get_sku).patch(update_sku))
        .route("/admin/skus/{id}/stock", post(adjust_stock))
        .route("/admin/images/{id}", patch(update_image).delete(delete_image))
        .route("/admin/categories", post(create_category))
        .route("/admin/categories/{id}", patch(update_category).delete(delete_category))
}

async fn list_products(
    State(state): State<AppState>,
    _: StaffUser,
    Query(q): Query<AdminListQuery>,
) -> Result<Json<Page<AdminListItem>>, AppError> {
    Ok(Json(products::list(&state.db, q).await?))
}

async fn create_product(
    State(state): State<AppState>,
    _: StaffUser,
    Json(body): Json<NewProduct>,
) -> Result<(StatusCode, Json<ProductDetail>), AppError> {
    let product = products::create(&state.db, &state.storage, body).await?;
    Ok((StatusCode::CREATED, Json(product)))
}

async fn get_product(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ProductDetail>, AppError> {
    Ok(Json(products::get(&state.db, &state.storage, id).await?))
}

async fn update_product(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ProductPatch>,
) -> Result<Json<ProductDetail>, AppError> {
    Ok(Json(products::update(&state.db, &state.storage, id, body).await?))
}

async fn create_sku(
    State(state): State<AppState>,
    _: StaffUser,
    Path(product_id): Path<Uuid>,
    Json(body): Json<NewSku>,
) -> Result<(StatusCode, Json<Sku>), AppError> {
    Ok((
        StatusCode::CREATED,
        Json(skus::create(&state.db, product_id, body).await?),
    ))
}

async fn get_sku(State(state): State<AppState>, _: StaffUser, Path(id): Path<Uuid>) -> Result<Json<Sku>, AppError> {
    Ok(Json(skus::get(&state.db, id).await?))
}

async fn update_sku(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SkuPatch>,
) -> Result<Json<Sku>, AppError> {
    Ok(Json(skus::update(&state.db, id, body).await?))
}

#[derive(Deserialize)]
struct StockAdjustment {
    /// Units to add; negative to remove.
    delta: i32,
}

#[derive(Serialize)]
struct StockLevel {
    stock_available: i32,
}

async fn adjust_stock(
    State(state): State<AppState>,
    StaffUser(user): StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<StockAdjustment>,
) -> Result<Json<StockLevel>, AppError> {
    let level = skus::adjust_stock(&state.db, id, body.delta).await?;
    tracing::info!(sku = %id, delta = body.delta, level, by = %user.id, "stock adjusted");
    Ok(Json(StockLevel { stock_available: level }))
}

#[derive(Deserialize)]
struct ImageUploadQuery {
    #[serde(default)]
    alt: String,
    sku_id: Option<Uuid>,
}

/// The request body is the image file itself (any image Content-Type), which
/// works from `fetch(url, { body: file })` and `curl --data-binary @photo.jpg`.
async fn upload_image(
    State(state): State<AppState>,
    _: StaffUser,
    Path(product_id): Path<Uuid>,
    Query(q): Query<ImageUploadQuery>,
    body: Bytes,
) -> Result<(StatusCode, Json<ImageView>), AppError> {
    if body.is_empty() {
        return Err(AppError::BadRequest("send the image file as the request body".into()));
    }
    let image = images::upload(&state.db, &state.storage, product_id, q.sku_id, q.alt, body).await?;
    Ok((StatusCode::CREATED, Json(image)))
}

#[derive(Deserialize)]
struct ImagePatch {
    alt: Option<String>,
    position: Option<i32>,
}

async fn update_image(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ImagePatch>,
) -> Result<StatusCode, AppError> {
    images::update(&state.db, id, body.alt, body.position).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_image(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    images::delete(&state.db, &state.storage, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_category(
    State(state): State<AppState>,
    _: StaffUser,
    Json(body): Json<NewCategory>,
) -> Result<(StatusCode, Json<Category>), AppError> {
    Ok((StatusCode::CREATED, Json(categories::create(&state.db, body).await?)))
}

async fn update_category(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<CategoryPatch>,
) -> Result<Json<Category>, AppError> {
    Ok(Json(categories::update(&state.db, id, body).await?))
}

async fn delete_category(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    categories::delete(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
