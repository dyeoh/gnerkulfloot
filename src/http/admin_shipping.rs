//! Shipping zones and rates (for the flat-rate adapter) and tax rules.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    auth::extract::StaffUser,
    error::{AppError, Problem},
    http::docs::{BadRequest, Forbidden, NotFound, Unauthorized},
    shipping::zones::{self, NewRate, Rate, RatePatch, Zone, ZoneInput, ZonePatch},
    state::AppState,
    tax::{self, NewTaxRule, TaxRule, TaxRulePatch},
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_zones, create_zone))
        .routes(routes!(update_zone, delete_zone))
        .routes(routes!(create_rate))
        .routes(routes!(update_rate, delete_rate))
        .routes(routes!(list_tax_rules, create_tax_rule))
        .routes(routes!(update_tax_rule, delete_tax_rule))
}

/// List shipping zones
///
/// Every zone with its regions and rates. Used by the `flat_rate` shipping
/// adapter.
#[utoipa::path(
    get,
    path = "/admin/shipping/zones",
    tag = "admin-shipping",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "All zones", body = Vec<Zone>),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
    ),
)]
async fn list_zones(State(state): State<AppState>, _: StaffUser) -> Result<Json<Vec<Zone>>, AppError> {
    Ok(Json(zones::list(&state.db).await?))
}

/// Create a shipping zone
///
/// A zone is a set of regions (a whole country, or a state within one) that
/// share rates. Where a state-level zone matches an address, country-wide
/// zones are ignored, so "East Malaysia" can override "Malaysia".
#[utoipa::path(
    post,
    path = "/admin/shipping/zones",
    tag = "admin-shipping",
    request_body = ZoneInput,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The new zone", body = Zone),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
    ),
)]
async fn create_zone(
    State(state): State<AppState>,
    _: StaffUser,
    Json(body): Json<ZoneInput>,
) -> Result<(StatusCode, Json<Zone>), AppError> {
    Ok((StatusCode::CREATED, Json(zones::create(&state.db, body).await?)))
}

/// Update a shipping zone
///
/// Fields left out are unchanged; `regions`, when sent, replaces them all.
#[utoipa::path(
    patch,
    path = "/admin/shipping/zones/{id}",
    tag = "admin-shipping",
    params(("id" = Uuid, Path, description = "Zone id")),
    request_body = ZonePatch,
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The updated zone", body = Zone),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn update_zone(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ZonePatch>,
) -> Result<Json<Zone>, AppError> {
    Ok(Json(zones::update(&state.db, id, body).await?))
}

/// Delete a shipping zone
///
/// Deletes the zone and its rates.
#[utoipa::path(
    delete,
    path = "/admin/shipping/zones/{id}",
    tag = "admin-shipping",
    params(("id" = Uuid, Path, description = "Zone id")),
    security(("bearer" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn delete_zone(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    zones::delete(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Add a rate to a zone
///
/// Every active rate in the rate's currency whose weight range fits the parcel
/// is offered at checkout as its own option.
#[utoipa::path(
    post,
    path = "/admin/shipping/zones/{id}/rates",
    tag = "admin-shipping",
    params(("id" = Uuid, Path, description = "Zone id")),
    request_body = NewRate,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The new rate", body = Rate),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn create_rate(
    State(state): State<AppState>,
    _: StaffUser,
    Path(zone_id): Path<Uuid>,
    Json(body): Json<NewRate>,
) -> Result<(StatusCode, Json<Rate>), AppError> {
    Ok((
        StatusCode::CREATED,
        Json(zones::create_rate(&state.db, zone_id, body).await?),
    ))
}

/// Update a shipping rate
///
/// Fields left out are unchanged.
#[utoipa::path(
    patch,
    path = "/admin/shipping/rates/{id}",
    tag = "admin-shipping",
    params(("id" = Uuid, Path, description = "Rate id")),
    request_body = RatePatch,
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The updated rate", body = Rate),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn update_rate(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<RatePatch>,
) -> Result<Json<Rate>, AppError> {
    Ok(Json(zones::update_rate(&state.db, id, body).await?))
}

/// Delete a shipping rate
#[utoipa::path(
    delete,
    path = "/admin/shipping/rates/{id}",
    tag = "admin-shipping",
    params(("id" = Uuid, Path, description = "Rate id")),
    security(("bearer" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn delete_rate(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    zones::delete_rate(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// List tax rules
///
/// The most specific rule matching an address wins: postcode prefix, then
/// state, then country. With none, the configured default rate applies.
#[utoipa::path(
    get,
    path = "/admin/tax-rules",
    tag = "admin-shipping",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "All tax rules", body = Vec<TaxRule>),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
    ),
)]
async fn list_tax_rules(State(state): State<AppState>, _: StaffUser) -> Result<Json<Vec<TaxRule>>, AppError> {
    Ok(Json(tax::list_rules(&state.db).await?))
}

/// Create a tax rule
///
/// Rules apply to new orders only: each order keeps the tax it was placed with.
#[utoipa::path(
    post,
    path = "/admin/tax-rules",
    tag = "admin-shipping",
    request_body = NewTaxRule,
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The new rule", body = TaxRule),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 409, description = "A rule for that country, state and postcode prefix already exists",
            body = Problem, content_type = "application/problem+json"),
    ),
)]
async fn create_tax_rule(
    State(state): State<AppState>,
    _: StaffUser,
    Json(body): Json<NewTaxRule>,
) -> Result<(StatusCode, Json<TaxRule>), AppError> {
    Ok((StatusCode::CREATED, Json(tax::create_rule(&state.db, body).await?)))
}

/// Update a tax rule
///
/// Fields left out are unchanged. Past orders keep the tax they were placed
/// with.
#[utoipa::path(
    patch,
    path = "/admin/tax-rules/{id}",
    tag = "admin-shipping",
    params(("id" = Uuid, Path, description = "Tax rule id")),
    request_body = TaxRulePatch,
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The updated rule", body = TaxRule),
        (status = 400, response = BadRequest),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn update_tax_rule(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<TaxRulePatch>,
) -> Result<Json<TaxRule>, AppError> {
    Ok(Json(tax::update_rule(&state.db, id, body).await?))
}

/// Delete a tax rule
#[utoipa::path(
    delete,
    path = "/admin/tax-rules/{id}",
    tag = "admin-shipping",
    params(("id" = Uuid, Path, description = "Tax rule id")),
    security(("bearer" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, response = Unauthorized),
        (status = 403, response = Forbidden),
        (status = 404, response = NotFound),
    ),
)]
async fn delete_tax_rule(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    tax::delete_rule(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
