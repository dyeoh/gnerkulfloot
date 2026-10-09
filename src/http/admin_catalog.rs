//! Catalog management for staff: products, SKUs, stock, categories and images.

use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    auth::extract::StaffUser,
    catalog::{
        categories::{self, Category, CategoryPatch, NewCategory},
        images::{self, ImageView},
        products::{self, AdminListItem, AdminListQuery, NewProduct, ProductDetail, ProductPatch},
        skus::{self, NewSku, Sku, SkuPatch},
        storefront::Page,
    },
    error::{AppError, Problem},
    http::docs::{BadRequest, Forbidden, NotFound, Unauthorized},
    state::AppState,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_products, create_product))
        .routes(routes!(get_product, update_product))
        .routes(routes!(create_sku))
        .routes(routes!(upload_image))
        .routes(routes!(get_sku, update_sku))
        .routes(routes!(adjust_stock))
        .routes(routes!(update_image, delete_image))
        .routes(routes!(create_category))
        .routes(routes!(update_category, delete_category))
}

/// List products
///
/// Every product, whatever its status, with SKU counts and total stock.
#[utoipa::path(
    get,
    path = "/admin/products",
    operation_id = "admin_list_products",
    tag = "admin-catalog",
    params(AdminListQuery),
    security(("bearer" = [])),
    responses(
        (status = 200, description = "A page of products", body = Page<AdminListItem>),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
    ),
)]
async fn list_products(
    State(state): State<AppState>,
    _: StaffUser,
    Query(q): Query<AdminListQuery>,
) -> Result<Json<Page<AdminListItem>>, AppError> {
    Ok(Json(products::list(&state.db, q).await?))
}

/// Create a product
///
/// Starts as a `draft` unless `status` says otherwise. Add SKUs (with prices)
/// and images next; only active products with a price in the shopper's currency
/// are listed.
#[utoipa::path(
    post,
    path = "/admin/products",
    tag = "admin-catalog",
    request_body = NewProduct,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The new product", body = ProductDetail),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 409, description = "`slug` is taken", body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn create_product(
    State(state): State<AppState>,
    _: StaffUser,
    Json(body): Json<NewProduct>,
) -> Result<(StatusCode, Json<ProductDetail>), AppError> {
    let product = products::create(&state.db, &state.storage, body).await?;
    Ok((StatusCode::CREATED, Json(product)))
}

/// Get a product
///
/// The product with its SKUs, images and category ids.
#[utoipa::path(
    get,
    path = "/admin/products/{id}",
    operation_id = "admin_get_product",
    tag = "admin-catalog",
    params(("id" = Uuid, Path, description = "Product id")),
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The product", body = ProductDetail),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn get_product(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ProductDetail>, AppError> {
    Ok(Json(products::get(&state.db, &state.storage, id).await?))
}

/// Update a product
///
/// Fields left out are unchanged; `category_ids`, when sent, replaces them all.
/// Archive a product instead of deleting it, so past orders keep pointing at
/// it.
#[utoipa::path(
    patch,
    path = "/admin/products/{id}",
    tag = "admin-catalog",
    params(("id" = Uuid, Path, description = "Product id")),
    request_body = ProductPatch,
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The updated product", body = ProductDetail),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
        (status = 409, description = "`slug` is taken", body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn update_product(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ProductPatch>,
) -> Result<Json<ProductDetail>, AppError> {
    Ok(Json(products::update(&state.db, &state.storage, id, body).await?))
}

/// Add a SKU to a product
///
/// A SKU is one buyable variant, with one price per currency and its own stock.
#[utoipa::path(
    post,
    path = "/admin/products/{id}/skus",
    tag = "admin-catalog",
    params(("id" = Uuid, Path, description = "Product id")),
    request_body = NewSku,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The new SKU", body = Sku),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
        (status = 409, description = "`code` is taken", body = Problem, content_type = "application/problem+json"),
    ),
)]
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

/// Get a SKU
#[utoipa::path(
    get,
    path = "/admin/skus/{id}",
    tag = "admin-catalog",
    params(("id" = Uuid, Path, description = "SKU id")),
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The SKU", body = Sku),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn get_sku(State(state): State<AppState>, _: StaffUser, Path(id): Path<Uuid>) -> Result<Json<Sku>, AppError> {
    Ok(Json(skus::get(&state.db, id).await?))
}

/// Update a SKU
///
/// Fields left out are unchanged; `prices`, when sent, replaces them all. Stock
/// isn't set here: use the stock endpoint.
#[utoipa::path(
    patch,
    path = "/admin/skus/{id}",
    tag = "admin-catalog",
    params(("id" = Uuid, Path, description = "SKU id")),
    request_body = SkuPatch,
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The updated SKU", body = Sku),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
        (status = 409, description = "`code` is taken", body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn update_sku(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SkuPatch>,
) -> Result<Json<Sku>, AppError> {
    Ok(Json(skus::update(&state.db, id, body).await?))
}

#[derive(Deserialize, utoipa::ToSchema)]
struct StockAdjustment {
    /// Units to add; negative to remove.
    delta: i32,
}

#[derive(Serialize, utoipa::ToSchema)]
struct StockLevel {
    stock_available: i32,
}

/// Adjust stock
///
/// Adds `delta` units (negative to remove). Stock is never set to an absolute
/// number: units held by unpaid orders are already taken off, so overwriting it
/// would release those holds and oversell.
#[utoipa::path(
    post,
    path = "/admin/skus/{id}/stock",
    tag = "admin-catalog",
    params(("id" = Uuid, Path, description = "SKU id")),
    request_body = StockAdjustment,
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The new stock level", body = StockLevel),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
        (status = 409, description = "Removing that many would take stock below zero", body = Problem, content_type = "application/problem+json"),
    ),
)]
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

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct ImageUploadQuery {
    /// Alt text, for screen readers.
    #[serde(default)]
    alt: String,
    /// Ties the image to one variant (e.g. the red one).
    sku_id: Option<Uuid>,
}

/// Upload a product image
///
/// The request body is the image file itself (any image Content-Type), which
/// works from `fetch(url, { body: file })` and `curl --data-binary @photo.jpg`.
/// It's resized into large and thumbnail WebP versions.
#[utoipa::path(
    post,
    path = "/admin/products/{id}/images",
    tag = "admin-catalog",
    params(("id" = Uuid, Path, description = "Product id"), ImageUploadQuery),
    request_body(content = Vec<u8>, description = "The image file", content_type = "image/*"),
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The stored image", body = ImageView),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
        (status = 413, description = "Larger than `server.body_limit_bytes`"),
        (status = 415, description = "Not an image format we can read",
            body = Problem, content_type = "application/problem+json"),
    ),
)]
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

#[derive(Deserialize, utoipa::ToSchema)]
struct ImagePatch {
    alt: Option<String>,
    position: Option<i32>,
}

/// Update an image
///
/// Change its alt text or position. Fields left out are unchanged.
#[utoipa::path(
    patch,
    path = "/admin/images/{id}",
    tag = "admin-catalog",
    params(("id" = Uuid, Path, description = "Image id")),
    request_body = ImagePatch,
    security(("bearer" = [])),
    responses(
        (status = 204, description = "Updated"),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn update_image(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ImagePatch>,
) -> Result<StatusCode, AppError> {
    images::update(&state.db, id, body.alt, body.position).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Delete an image
#[utoipa::path(
    delete,
    path = "/admin/images/{id}",
    tag = "admin-catalog",
    params(("id" = Uuid, Path, description = "Image id")),
    security(("bearer" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn delete_image(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    images::delete(&state.db, &state.storage, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Create a category
#[utoipa::path(
    post,
    path = "/admin/categories",
    tag = "admin-catalog",
    request_body = NewCategory,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The new category", body = Category),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
        (status = 409, description = "`slug` is taken", body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn create_category(
    State(state): State<AppState>,
    _: StaffUser,
    Json(body): Json<NewCategory>,
) -> Result<(StatusCode, Json<Category>), AppError> {
    Ok((StatusCode::CREATED, Json(categories::create(&state.db, body).await?)))
}

/// Update a category
///
/// Fields left out are unchanged; `parent_id: null` moves it to the top level.
#[utoipa::path(
    patch,
    path = "/admin/categories/{id}",
    tag = "admin-catalog",
    params(("id" = Uuid, Path, description = "Category id")),
    request_body = CategoryPatch,
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The updated category", body = Category),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
        (status = 409, description = "`slug` is taken", body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn update_category(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<CategoryPatch>,
) -> Result<Json<Category>, AppError> {
    Ok(Json(categories::update(&state.db, id, body).await?))
}

/// Delete a category
#[utoipa::path(
    delete,
    path = "/admin/categories/{id}",
    tag = "admin-catalog",
    params(("id" = Uuid, Path, description = "Category id")),
    security(("bearer" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn delete_category(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    categories::delete(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
