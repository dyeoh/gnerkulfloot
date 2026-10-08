//! Shipping zones and rates (for the flat-rate adapter) and tax rules.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::extract::StaffUser,
    error::AppError,
    shipping::zones::{self, NewRate, Rate, RatePatch, Zone, ZoneInput, ZonePatch},
    tax::{self, NewTaxRule, TaxRule, TaxRulePatch},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/shipping/zones", get(list_zones).post(create_zone))
        .route("/admin/shipping/zones/{id}", patch(update_zone).delete(delete_zone))
        .route("/admin/shipping/zones/{id}/rates", post(create_rate))
        .route("/admin/shipping/rates/{id}", patch(update_rate).delete(delete_rate))
        .route("/admin/tax-rules", get(list_tax_rules).post(create_tax_rule))
        .route("/admin/tax-rules/{id}", patch(update_tax_rule).delete(delete_tax_rule))
}

async fn list_zones(State(state): State<AppState>, _: StaffUser) -> Result<Json<Vec<Zone>>, AppError> {
    Ok(Json(zones::list(&state.db).await?))
}

async fn create_zone(
    State(state): State<AppState>,
    _: StaffUser,
    Json(body): Json<ZoneInput>,
) -> Result<(StatusCode, Json<Zone>), AppError> {
    Ok((StatusCode::CREATED, Json(zones::create(&state.db, body).await?)))
}

async fn update_zone(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ZonePatch>,
) -> Result<Json<Zone>, AppError> {
    Ok(Json(zones::update(&state.db, id, body).await?))
}

async fn delete_zone(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    zones::delete(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

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

async fn update_rate(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<RatePatch>,
) -> Result<Json<Rate>, AppError> {
    Ok(Json(zones::update_rate(&state.db, id, body).await?))
}

async fn delete_rate(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    zones::delete_rate(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_tax_rules(State(state): State<AppState>, _: StaffUser) -> Result<Json<Vec<TaxRule>>, AppError> {
    Ok(Json(tax::list_rules(&state.db).await?))
}

async fn create_tax_rule(
    State(state): State<AppState>,
    _: StaffUser,
    Json(body): Json<NewTaxRule>,
) -> Result<(StatusCode, Json<TaxRule>), AppError> {
    Ok((StatusCode::CREATED, Json(tax::create_rule(&state.db, body).await?)))
}

async fn update_tax_rule(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
    Json(body): Json<TaxRulePatch>,
) -> Result<Json<TaxRule>, AppError> {
    Ok(Json(tax::update_rule(&state.db, id, body).await?))
}

async fn delete_tax_rule(
    State(state): State<AppState>,
    _: StaffUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    tax::delete_rule(&state.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
