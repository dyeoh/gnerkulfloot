//! Admin management of the flat-rate adapter's zones and rates.

use std::collections::HashMap;

use iso_currency::Currency;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    catalog::{double_option, require_name},
    error::DataError,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct Region {
    /// ISO 3166-1 alpha-2, e.g. `MY`.
    pub country: String,
    /// Empty for the whole country.
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Zone {
    pub id: Uuid,
    pub name: String,
    pub regions: Vec<Region>,
    pub rates: Vec<Rate>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Rate {
    pub id: Uuid,
    pub zone_id: Uuid,
    pub name: String,
    #[schema(value_type = String)]
    pub currency: Currency,
    /// Price in minor units of `currency` (800 is RM8.00).
    pub amount: i64,
    /// Applies to parcels from this weight, in grams.
    pub min_weight_g: i32,
    /// Up to this weight in grams; no upper limit when null.
    pub max_weight_g: Option<i32>,
    /// Free when the subtotal reaches this, in minor units.
    pub free_over_amount: Option<i64>,
    pub active: bool,
    pub position: i32,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ZoneInput {
    pub name: String,
    pub regions: Vec<Region>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ZonePatch {
    pub name: Option<String>,
    /// Replaces all regions when present.
    pub regions: Option<Vec<Region>>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct NewRate {
    pub name: String,
    #[schema(value_type = String)]
    pub currency: Currency,
    /// Price in minor units of `currency` (800 is RM8.00).
    pub amount: i64,
    /// Applies to parcels from this weight, in grams.
    #[serde(default)]
    pub min_weight_g: i32,
    /// Up to this weight in grams; no upper limit when null.
    pub max_weight_g: Option<i32>,
    /// Free when the subtotal reaches this, in minor units.
    pub free_over_amount: Option<i64>,
    #[serde(default = "yes")]
    #[schema(default = true)]
    pub active: bool,
    #[serde(default)]
    pub position: i32,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RatePatch {
    pub name: Option<String>,
    /// Minor units of the rate's currency.
    pub amount: Option<i64>,
    pub min_weight_g: Option<i32>,
    /// `null` removes the upper limit; leave it out to keep it.
    #[serde(default, deserialize_with = "double_option")]
    pub max_weight_g: Option<Option<i32>>,
    /// `null` removes free shipping; leave it out to keep it.
    #[serde(default, deserialize_with = "double_option")]
    pub free_over_amount: Option<Option<i64>>,
    pub active: Option<bool>,
    pub position: Option<i32>,
}

fn invalid(msg: &str) -> DataError {
    DataError::InvalidInput(msg.into())
}

fn normalize_regions(regions: &[Region]) -> Result<Vec<Region>, DataError> {
    if regions.is_empty() {
        return Err(invalid("a zone needs at least one region"));
    }
    regions
        .iter()
        .map(|r| {
            let country = r.country.trim().to_ascii_uppercase();
            if country.len() != 2 || !country.chars().all(|c| c.is_ascii_alphabetic()) {
                return Err(invalid("region country must be a two-letter ISO code, e.g. MY"));
            }
            Ok(Region {
                country,
                state: r.state.trim().to_uppercase(),
            })
        })
        .collect()
}

fn validate_rate(amount: i64, min: i32, max: Option<i32>, free_over: Option<i64>) -> Result<(), DataError> {
    if amount < 0 || free_over.is_some_and(|f| f < 0) {
        return Err(invalid("amounts can't be negative"));
    }
    if min < 0 || max.is_some_and(|m| m < min) {
        return Err(invalid("weight range is invalid"));
    }
    Ok(())
}

pub async fn list(db: &PgPool) -> Result<Vec<Zone>, sqlx::Error> {
    let zones = sqlx::query!("SELECT id, name FROM shipping_zones ORDER BY name")
        .fetch_all(db)
        .await?;
    let mut regions: HashMap<Uuid, Vec<Region>> = HashMap::new();
    for r in sqlx::query!("SELECT zone_id, country, state FROM shipping_zone_regions ORDER BY country, state")
        .fetch_all(db)
        .await?
    {
        regions.entry(r.zone_id).or_default().push(Region {
            country: r.country,
            state: r.state,
        });
    }
    let mut rates: HashMap<Uuid, Vec<Rate>> = HashMap::new();
    for r in load_rates(db, None).await? {
        rates.entry(r.zone_id).or_default().push(r);
    }
    Ok(zones
        .into_iter()
        .map(|z| Zone {
            regions: regions.remove(&z.id).unwrap_or_default(),
            rates: rates.remove(&z.id).unwrap_or_default(),
            id: z.id,
            name: z.name,
        })
        .collect())
}

pub async fn get(db: &PgPool, id: Uuid) -> Result<Zone, DataError> {
    list(db)
        .await?
        .into_iter()
        .find(|z| z.id == id)
        .ok_or(DataError::NotFound)
}

pub async fn create(db: &PgPool, input: ZoneInput) -> Result<Zone, DataError> {
    let name = require_name_data(&input.name)?;
    let regions = normalize_regions(&input.regions)?;
    let mut tx = db.begin().await?;
    let id = Uuid::now_v7();
    sqlx::query!("INSERT INTO shipping_zones (id, name) VALUES ($1, $2)", id, name)
        .execute(&mut *tx)
        .await?;
    replace_regions(&mut tx, id, &regions).await?;
    tx.commit().await?;
    get(db, id).await
}

pub async fn update(db: &PgPool, id: Uuid, patch: ZonePatch) -> Result<Zone, DataError> {
    let name = patch.name.as_deref().map(require_name_data).transpose()?;
    let regions = patch.regions.as_deref().map(normalize_regions).transpose()?;
    let mut tx = db.begin().await?;
    let updated = sqlx::query!(
        "UPDATE shipping_zones SET name = COALESCE($2, name), updated_at = now() WHERE id = $1",
        id,
        name
    )
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(DataError::NotFound);
    }
    if let Some(regions) = regions {
        replace_regions(&mut tx, id, &regions).await?;
    }
    tx.commit().await?;
    get(db, id).await
}

async fn replace_regions(conn: &mut PgConnection, zone_id: Uuid, regions: &[Region]) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM shipping_zone_regions WHERE zone_id = $1", zone_id)
        .execute(&mut *conn)
        .await?;
    for r in regions {
        sqlx::query!(
            "INSERT INTO shipping_zone_regions (zone_id, country, state) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            zone_id,
            r.country,
            r.state
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

pub async fn delete(db: &PgPool, id: Uuid) -> Result<(), DataError> {
    let deleted = sqlx::query!("DELETE FROM shipping_zones WHERE id = $1", id)
        .execute(db)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(DataError::NotFound);
    }
    Ok(())
}

pub async fn create_rate(db: &PgPool, zone_id: Uuid, r: NewRate) -> Result<Rate, DataError> {
    let name = require_name_data(&r.name)?;
    validate_rate(r.amount, r.min_weight_g, r.max_weight_g, r.free_over_amount)?;
    let zone_exists = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM shipping_zones WHERE id = $1) AS "e!""#,
        zone_id
    )
    .fetch_one(db)
    .await?;
    if !zone_exists {
        return Err(DataError::NotFound);
    }
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO shipping_rates
             (id, zone_id, name, price_currency, price_amount, min_weight_g, max_weight_g, free_over_amount, active, position)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        id,
        zone_id,
        name,
        r.currency.code(),
        r.amount,
        r.min_weight_g,
        r.max_weight_g,
        r.free_over_amount,
        r.active,
        r.position,
    )
    .execute(db)
    .await?;
    get_rate(db, id).await
}

pub async fn update_rate(db: &PgPool, id: Uuid, p: RatePatch) -> Result<Rate, DataError> {
    let current = get_rate(db, id).await?;
    let name = p.name.as_deref().map(require_name_data).transpose()?;
    let max = p.max_weight_g.unwrap_or(current.max_weight_g);
    let free_over = p.free_over_amount.unwrap_or(current.free_over_amount);
    validate_rate(
        p.amount.unwrap_or(current.amount),
        p.min_weight_g.unwrap_or(current.min_weight_g),
        max,
        free_over,
    )?;
    sqlx::query!(
        "UPDATE shipping_rates SET
             name             = COALESCE($2, name),
             price_amount     = COALESCE($3, price_amount),
             min_weight_g     = COALESCE($4, min_weight_g),
             max_weight_g     = $5,
             free_over_amount = $6,
             active           = COALESCE($7, active),
             position         = COALESCE($8, position),
             updated_at       = now()
         WHERE id = $1",
        id,
        name,
        p.amount,
        p.min_weight_g,
        max,
        free_over,
        p.active,
        p.position,
    )
    .execute(db)
    .await?;
    get_rate(db, id).await
}

pub async fn delete_rate(db: &PgPool, id: Uuid) -> Result<(), DataError> {
    let deleted = sqlx::query!("DELETE FROM shipping_rates WHERE id = $1", id)
        .execute(db)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(DataError::NotFound);
    }
    Ok(())
}

async fn get_rate(db: &PgPool, id: Uuid) -> Result<Rate, DataError> {
    load_rates(db, Some(id)).await?.pop().ok_or(DataError::NotFound)
}

async fn load_rates(db: &PgPool, id: Option<Uuid>) -> Result<Vec<Rate>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT id, zone_id, name, price_currency, price_amount, min_weight_g, max_weight_g, free_over_amount, active, position
         FROM shipping_rates WHERE ($1::uuid IS NULL OR id = $1) ORDER BY position, price_amount",
        id
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Rate {
                currency: Currency::from_code(&r.price_currency)?,
                id: r.id,
                zone_id: r.zone_id,
                name: r.name,
                amount: r.price_amount,
                min_weight_g: r.min_weight_g,
                max_weight_g: r.max_weight_g,
                free_over_amount: r.free_over_amount,
                active: r.active,
                position: r.position,
            })
        })
        .collect())
}

fn require_name_data(name: &str) -> Result<String, DataError> {
    require_name(name).map_err(|e| DataError::InvalidInput(e.to_string()))
}
